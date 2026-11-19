//! The app's end of `jdesk` and of `jiradesk://` links: a socket in the data folder
//! (`control.sock`, only you can open it) whose requests the window answers, one at a time,
//! on its own thread.

use futures::channel::{mpsc, oneshot};
use jdesk_core::control::{Request, Response, socket_path};
use std::path::Path;

/// A request and where its answer goes (`None`: nobody waits, as for a link).
pub type Job = (Request, Option<oneshot::Sender<Response>>);

/// Whether a Jira Desk is already open on this data folder (it answers on the socket).
pub fn running(desk_dir: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::net::UnixStream::connect(socket_path(desk_dir)).is_ok()
    }
    #[cfg(not(unix))]
    {
        let _ = desk_dir;
        false
    }
}

/// Listens on the data folder's socket, handing each request to `jobs`.
#[cfg(unix)]
pub fn listen(desk_dir: &Path, jobs: mpsc::UnboundedSender<Job>) -> anyhow::Result<()> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    let path = socket_path(desk_dir);
    // The folder (0700) is what keeps the socket yours: `bind` makes it with the umask's mode
    // before the chmod below, and the folder stops anyone else reaching it in between.
    jdesk_core::make_private(desk_dir)?;
    if running(desk_dir) {
        anyhow::bail!("another Jira Desk is already open on this folder");
    }
    // Left over from a copy that didn't close cleanly.
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    std::thread::Builder::new()
        .name("jdesk-control".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let jobs = jobs.clone();
                std::thread::spawn(move || {
                    let Ok(reader) = stream.try_clone() else {
                        return;
                    };
                    let mut out = stream;
                    for line in BufReader::new(reader).lines() {
                        let Ok(line) = line else { return };
                        let answer = match serde_json::from_str::<Request>(&line) {
                            Ok(req) => {
                                let (tx, rx) = oneshot::channel();
                                if jobs.unbounded_send((req, Some(tx))).is_err() {
                                    Response::err("Jira Desk is closing")
                                } else {
                                    futures::executor::block_on(rx)
                                        .unwrap_or_else(|_| Response::err("Jira Desk is closing"))
                                }
                            }
                            Err(e) => Response::err(format!("not a request: {e}")),
                        };
                        let Ok(mut text) = serde_json::to_string(&answer) else {
                            return;
                        };
                        text.push('\n');
                        if out.write_all(text.as_bytes()).is_err() {
                            return;
                        }
                    }
                });
            }
        })?;
    Ok(())
}

#[cfg(not(unix))]
pub fn listen(_desk_dir: &Path, _jobs: mpsc::UnboundedSender<Job>) -> anyhow::Result<()> {
    anyhow::bail!("jdesk can only reach the app on macOS and Linux")
}

/// Removes the socket when the app closes.
pub fn stop(desk_dir: &Path) {
    let _ = std::fs::remove_file(socket_path(desk_dir));
}
