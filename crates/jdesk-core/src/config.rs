//! `config.toml` in the data folder: which Jira site, how to sign in, which issues to keep.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Api {
    /// Jira Cloud (REST v3, ADF text, `search/jql` paging).
    #[default]
    Cloud,
    /// Jira Server / Data Center (REST v2, personal access token).
    Server,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldOverrides {
    pub sprint: Option<String>,
    pub story_points: Option<String>,
    pub epic_link: Option<String>,
    pub rank: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// e.g. `https://acme.atlassian.net`.
    pub site: String,
    #[serde(default)]
    pub api: Api,
    /// Cloud: the account email that goes with the API token.
    #[serde(default)]
    pub email: Option<String>,
    /// A command that prints the token (`security find-generic-password -w -s jira-desk`,
    /// `op read op://…`). Used when `JIRA_API_TOKEN` is not set.
    #[serde(default)]
    pub token_command: Option<String>,
    /// Which issues to keep locally.
    pub jql: String,
    /// Minutes between background refreshes; 0 turns them off.
    #[serde(default = "default_refresh")]
    pub refresh_minutes: u64,
    /// The most downloaded images may take (MB); past it, the least recently opened ones are
    /// deleted (they download again when you open their issue). 0: no limit.
    #[serde(default = "default_image_mb")]
    pub max_image_mb: u64,
    #[serde(default)]
    pub fields: FieldOverrides,
    /// Lets `site` be `http://`, which sends the token unencrypted: only for a Jira on your own
    /// computer or a network you trust.
    #[serde(default)]
    pub allow_insecure_http: bool,
}

fn default_refresh() -> u64 {
    10
}

fn default_image_mb() -> u64 {
    500
}

pub const EXAMPLE: &str = r#"# Jira Desk settings. The token is never stored here: set JIRA_API_TOKEN, or give a
# token_command that prints it.

site = "https://your-site.atlassian.net"
api = "cloud"                 # "server" for Jira Server / Data Center (personal access token)
email = "you@example.com"     # Cloud only: the account that owns the API token

# macOS Keychain:  security add-generic-password -s jira-desk -a you@example.com -w
token_command = "security find-generic-password -w -s jira-desk"

# Which issues to keep locally. Keep it to what you work with: every refresh fetches what
# changed in it since the last one.
jql = "project = ABC AND (sprint in openSprints() OR sprint in futureSprints() OR updated >= -30d)"

refresh_minutes = 10          # 0: refresh only when asked (Cmd-R)
max_image_mb = 500            # images kept; past this the least recently opened go (0: no limit)

# Only if a field isn't found by itself:
# [fields]
# story_points = "customfield_10016"
"#;

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let c: Config =
            toml::from_str(&text).with_context(|| format!("{} is not valid", path.display()))?;
        // The token goes with every request: never in the clear unless asked for.
        let http = c.site.starts_with("http://");
        if !c.site.starts_with("https://") && !(http && c.allow_insecure_http) {
            if http {
                bail!(
                    "site is http://, which would send your token unencrypted: use https://, \
                     or set allow_insecure_http = true (only on a network you trust)"
                );
            }
            bail!("site must start with https:// (got {:?})", c.site);
        }
        if c.api == Api::Cloud && c.email.is_none() {
            bail!("Jira Cloud needs `email` (the account that owns the API token)");
        }
        if c.jql.trim().is_empty() {
            bail!("jql is empty: say which issues to keep, e.g. `project = ABC`");
        }
        Ok(c)
    }

    pub fn site(&self) -> &str {
        self.site.trim_end_matches('/')
    }

    /// The API token: `JIRA_API_TOKEN`, else the output of `token_command`.
    pub fn token(&self) -> Result<String> {
        if let Ok(t) = std::env::var("JIRA_API_TOKEN")
            && !t.trim().is_empty()
        {
            return Ok(t.trim().to_string());
        }
        let Some(cmd) = &self.token_command else {
            bail!("no token: set JIRA_API_TOKEN or token_command in config.toml");
        };
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(cmd)
            .output()
            .with_context(|| format!("cannot run token_command `{cmd}`"))?;
        if !out.status.success() {
            bail!(
                "token_command failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if t.is_empty() {
            bail!("token_command printed nothing");
        }
        Ok(t)
    }
}

/// The data folder: `--data-dir`, else `JDESK_DATA_DIR`, else the platform's place for app
/// data (`~/Library/Application Support/Jira Desk`, `~/.local/share/jira-desk`).
pub fn data_dir(explicit: Option<PathBuf>) -> PathBuf {
    if let Some(d) = explicit {
        return d;
    }
    if let Some(d) = std::env::var_os("JDESK_DATA_DIR") {
        return PathBuf::from(d);
    }
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Jira Desk")
    } else if let Some(x) = std::env::var_os("XDG_DATA_HOME") {
        PathBuf::from(x).join("jira-desk")
    } else {
        home.join(".local/share/jira-desk")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_parses() {
        let c: Config = toml::from_str(EXAMPLE).unwrap();
        assert_eq!(c.api, Api::Cloud);
        assert_eq!(c.refresh_minutes, 10);
        assert_eq!(c.max_image_mb, 500);
        assert!(!c.allow_insecure_http);
    }

    #[test]
    fn http_sites_need_saying_so() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let write = |site: &str, extra: &str| {
            let text = EXAMPLE.replace("https://your-site.atlassian.net", site);
            std::fs::write(&path, format!("{text}\n{extra}\n")).unwrap();
        };
        write("https://acme.atlassian.net", "");
        assert!(Config::load(&path).is_ok());
        write("http://jira.local", "");
        let e = Config::load(&path).unwrap_err().to_string();
        assert!(e.contains("unencrypted"), "{e}");
        write("http://jira.local", "allow_insecure_http = true");
        assert!(Config::load(&path).is_ok());
        write("ftp://jira.local", "allow_insecure_http = true");
        assert!(Config::load(&path).is_err());
    }
}
