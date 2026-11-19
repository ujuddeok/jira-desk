//! Jira Desk: a fast, keyboard-first desktop viewer for a local copy of your Jira issues.
//!
//! Issues sync one way, Jira → this computer, in the background. Changes you make are queued
//! and sent only when you apply them. Notes, stars and tags stay local. The window layout
//! (docks, splits, tabs, Layout mode) is Chorong's.

mod actions;
mod assets;
mod chord;
mod control;
mod double_tap;
mod input;
mod keymap;
mod omni;
mod panes;
mod picker;
mod rich;
mod state;
mod view;

use anyhow::Result;
use cho_layout_ui::shell::{Shell, ShellOptions};
use cho_theme::{Appearance, Theme, ThemeFamily};
use gpui::{
    App, AppContext, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size,
};
use jdesk_core::{Desk, config};
use std::path::PathBuf;
use std::rc::Rc;

struct Args {
    data_dir: Option<PathBuf>,
    demo: bool,
    light: bool,
    theme: Option<PathBuf>,
    theme_name: Option<String>,
    demo_issues: usize,
    /// `jiradesk://` links to show (the platform passes them on Linux; macOS uses
    /// `on_open_urls`).
    links: Vec<String>,
}

fn usage() -> ! {
    eprintln!(
        "usage: jira-desk [--data-dir DIR] [--demo [N]] [--light] [--theme ZED_THEME.json [--theme-name NAME]] [jiradesk://LINK…]\n\n\
         Settings live in config.toml in the data folder (jira-desk --where prints it)."
    );
    std::process::exit(2)
}

fn args() -> Args {
    let mut a = Args {
        data_dir: None,
        demo: false,
        light: false,
        theme: None,
        theme_name: None,
        demo_issues: 300,
        links: Vec::new(),
    };
    let mut it = std::env::args().skip(1).peekable();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--data-dir" => a.data_dir = Some(it.next().unwrap_or_else(|| usage()).into()),
            "--demo" => {
                a.demo = true;
                if let Some(n) = it.peek().and_then(|n| n.parse().ok()) {
                    a.demo_issues = n;
                    it.next();
                }
            }
            "--light" => a.light = true,
            "--theme" => a.theme = Some(it.next().unwrap_or_else(|| usage()).into()),
            "--theme-name" => a.theme_name = Some(it.next().unwrap_or_else(|| usage())),
            "--where" => {
                println!("{}", config::data_dir(a.data_dir.clone()).display());
                std::process::exit(0)
            }
            link if link.starts_with("jiradesk://") => a.links.push(link.to_string()),
            _ => usage(),
        }
    }
    a
}

fn theme(a: &Args) -> Result<Theme> {
    match &a.theme {
        Some(file) => {
            let json = std::fs::read_to_string(file)?;
            let family = ThemeFamily::parse(&json)?;
            Ok(family.theme(a.theme_name.as_deref())?.clone())
        }
        None => Ok(Theme::bundled(if a.light {
            Appearance::Light
        } else {
            Appearance::Dark
        })),
    }
}

fn main() -> Result<()> {
    let a = args();
    let dir = config::data_dir(a.data_dir.clone());
    let desk_dir = if a.demo {
        dir.join("demo")
    } else {
        dir.clone()
    };
    // One app per data folder: a second one hands its links to the first and stops (opening
    // the folder again would race it, and a demo would wipe the first one's files).
    if control::running(&desk_dir) {
        if a.links.is_empty() {
            eprintln!("Jira Desk is already open on {}", desk_dir.display());
        }
        for link in &a.links {
            match jdesk_core::control::from_url(link)
                .and_then(|r| jdesk_core::control::call(&desk_dir, &r))
            {
                Ok(Some(r)) if r.ok => println!("{}", r.message),
                Ok(Some(r)) => eprintln!("{link}: {}", r.message),
                Ok(None) => eprintln!("{link}: Jira Desk stopped answering"),
                Err(e) => eprintln!("{link}: {e:#}"),
            }
        }
        return Ok(());
    }
    let desk = if a.demo {
        Desk::open_demo(&dir, a.demo_issues)?
    } else {
        match Desk::open(&dir) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("jira-desk: {e:#}\n\nTo look around first: jira-desk --demo");
                std::process::exit(1);
            }
        }
    };
    let theme = theme(&a)?;
    let user_keymap = std::fs::read_to_string(dir.join("keymap.json")).ok();
    let layout_dir = desk.dir.join("layout");
    let title = if desk.is_demo() {
        "Jira Desk — demo".to_string()
    } else {
        format!("Jira Desk — {}", desk.site().trim_start_matches("https://"))
    };

    // Requests from jdesk (the socket) and from links all reach the window through `jobs`.
    let (jobs_tx, jobs_rx) = futures::channel::mpsc::unbounded::<control::Job>();
    let control_problem = control::listen(&desk.dir, jobs_tx.clone())
        .err()
        .map(|e| format!("jdesk can't reach this window: {e}"));
    let queue_link = {
        let jobs = jobs_tx.clone();
        move |link: &str| match jdesk_core::control::from_url(link) {
            Ok(req) => {
                let _ = jobs.unbounded_send((req, None));
            }
            Err(e) => eprintln!("jira-desk: {e:#}"),
        }
    };
    for link in &a.links {
        queue_link(link);
    }
    let app = Application::new().with_assets(assets::Assets);
    app.on_open_urls(move |urls| {
        for u in urls {
            queue_link(&u);
        }
    });
    let socket_dir = desk.dir.clone();
    app.run(move |cx: &mut App| {
        cho_layout_ui::fonts::install(cx, None, None);
        let keymap_problem = keymap::bind(cx, user_keymap.as_deref());
        let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
        let ws = view::load_workspace(&layout_dir);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(title.into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |window, cx| {
                let focus = cx.focus_handle();
                let colors =
                    cho_layout_ui::colors::Colors::new(&theme, cho_layout_ui::fonts::pick(cx));
                let model = cx.new(|cx| {
                    let mut m = state::Model::new(desk, colors);
                    if let Some(p) = keymap_problem.or(control_problem) {
                        m.error(p);
                    }
                    if m.refresh_due() {
                        m.start_sync(jdesk_core::sync::Mode::Auto, cx);
                    }
                    m
                });
                let bodies = Rc::new(panes::Bodies {
                    model: model.clone(),
                    scrolls: Default::default(),
                });
                let options = ShellOptions {
                    frame_log: None,
                    state_dir: Some(layout_dir.clone()),
                    state_key: view::LAYOUT_KEY.into(),
                    show_stats: false,
                    autosave: true,
                    metrics: None,
                };
                let shell =
                    cx.new(|cx| Shell::new(ws, &theme, options, bodies.clone(), focus.clone(), cx));
                window.focus(&focus);
                let view = cx.new(|cx| view::DeskView::new(model, shell, bodies, focus, cx));
                view.update(cx, |v, cx| {
                    v.start_control(jobs_rx, window, cx);
                    v.watch_chords(window, cx);
                });
                view
            },
        )
        .expect("open window");
        cx.on_window_closed(move |cx| {
            if cx.windows().is_empty() {
                control::stop(&socket_dir);
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::state::PaneFocus;

    #[test]
    fn default_layout_has_every_pane_in_place() {
        let ws = crate::view::default_workspace();
        let kinds: Vec<PaneFocus> = ws
            .panes()
            .values()
            .map(|c| PaneFocus::of(Some(c)))
            .collect();
        for want in [
            PaneFocus::Filters,
            PaneFocus::Issues,
            PaneFocus::Preview,
            PaneFocus::Notes,
            PaneFocus::Changes,
        ] {
            assert!(kinds.contains(&want), "{want:?} missing: {kinds:?}");
        }
        assert_eq!(
            PaneFocus::of(ws.focused().and_then(|p| ws.content(p))),
            PaneFocus::Issues
        );
        assert!(ws.check_invariants().is_ok());
    }
}
