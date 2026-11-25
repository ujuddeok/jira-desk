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
    /// Scrolling past an end stretches and springs back (rubber banding), as on macOS.
    #[serde(default = "yes")]
    pub overscroll: bool,
    #[serde(default)]
    pub fields: FieldOverrides,
    /// Lets `site` be `http://`, which sends the token unencrypted: only for a Jira on your own
    /// computer or a network you trust.
    #[serde(default)]
    pub allow_insecure_http: bool,
}

/// One chip over the List: a name and the statuses it gathers (`[[status_group]]` in
/// config.toml, in chip order).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StatusGroup {
    pub name: String,
    /// Status names, as Jira spells them (case doesn't matter).
    #[serde(default)]
    pub statuses: Vec<String>,
    /// Also the statuses of this Jira category (`to_do`, `in_progress`, `done`) that no group
    /// names.
    #[serde(default)]
    pub category: Option<crate::model::StatusCategory>,
}

/// The chips when config.toml sets none: Jira's own three categories.
pub fn default_status_groups() -> Vec<StatusGroup> {
    use crate::model::StatusCategory as C;
    [
        ("Not started", C::ToDo),
        ("In progress · review", C::InProgress),
        ("Done", C::Done),
    ]
    .into_iter()
    .map(|(name, c)| StatusGroup {
        name: name.into(),
        statuses: Vec::new(),
        category: Some(c),
    })
    .collect()
}

/// `auto_advance` in `dir/config.toml`: after a change made from the List, move to the next
/// issue. Read on its own, as [`status_groups`] is, so the demo uses it too. Off by default.
pub fn auto_advance(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join("config.toml")).is_ok_and(|t| auto_advance_in(&t))
}

/// [`auto_advance`] from config.toml's text.
pub fn auto_advance_in(text: &str) -> bool {
    #[derive(Deserialize)]
    struct Only {
        #[serde(default)]
        auto_advance: bool,
    }
    toml::from_str::<Only>(text).is_ok_and(|o| o.auto_advance)
}

/// The status groups set in `dir/config.toml`, read on their own so the demo (which has no
/// settings otherwise) uses them too. A problem gives the default groups and says why.
pub fn status_groups(dir: &Path) -> (Vec<StatusGroup>, Option<String>) {
    match std::fs::read_to_string(dir.join("config.toml")) {
        Ok(text) => status_groups_in(&text),
        Err(_) => (default_status_groups(), None),
    }
}

/// [`status_groups`] from config.toml's text.
pub fn status_groups_in(text: &str) -> (Vec<StatusGroup>, Option<String>) {
    #[derive(Deserialize)]
    struct Only {
        #[serde(default)]
        status_group: Vec<StatusGroup>,
    }
    match toml::from_str::<Only>(text) {
        Ok(o) if o.status_group.is_empty() => (default_status_groups(), None),
        Ok(o) => {
            if let Some(g) = o.status_group.iter().find(|g| {
                g.name.trim().is_empty() || (g.statuses.is_empty() && g.category.is_none())
            }) {
                return (
                    default_status_groups(),
                    Some(format!(
                        "config.toml: each [[status_group]] needs a name and statuses or a category (“{}”); using the default chips",
                        g.name
                    )),
                );
            }
            (o.status_group, None)
        }
        Err(e) => (
            default_status_groups(),
            Some(format!(
                "config.toml: status_group: {e}; using the default chips"
            )),
        ),
    }
}

fn default_refresh() -> u64 {
    10
}

fn yes() -> bool {
    true
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
overscroll = true             # scrolling past an end stretches and springs back, as on macOS
auto_advance = false          # after a change from the List, go to the next issue (until set in Settings)

# The status chips over the List, in order (default: Jira's three categories). A status goes to
# the first group naming it; `category` (to_do, in_progress, done) takes the rest of that kind;
# anything left over shows as "Other". f 1 to f 9 toggle them.
# [[status_group]]
# name = "Not started"
# category = "to_do"
# [[status_group]]
# name = "In progress"
# statuses = ["In Progress", "Blocked"]
# [[status_group]]
# name = "In review"
# statuses = ["In Review", "QA"]
# [[status_group]]
# name = "Done"
# category = "done"

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
        c.check()?;
        Ok(c)
    }

    /// config.toml's text, checked as [`Config::load`] does.
    pub fn parse(text: &str) -> Result<Self> {
        let c: Config = toml::from_str(text)?;
        c.check()?;
        Ok(c)
    }

    /// What `load` refuses beyond TOML: an unencrypted site, Cloud without an email, no JQL.
    fn check(&self) -> Result<()> {
        let c = self;
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
        Ok(())
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
        // The commented-out groups in the example are the documented shape: they parse.
        let groups: String = EXAMPLE
            .lines()
            .skip_while(|l| !l.starts_with("# [[status_group]]"))
            .take_while(|l| l.starts_with('#'))
            .map(|l| format!("{}\n", l.trim_start_matches("# ")))
            .collect();
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("config.toml"), &groups).unwrap();
        let (g, problem) = status_groups(d.path());
        assert_eq!(problem, None);
        assert_eq!(
            g.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(),
            ["Not started", "In progress", "In review", "Done"]
        );
        assert_eq!(g[1].statuses, ["In Progress", "Blocked"]);
        // A group with nothing in it, or a typo in a category, falls back with a reason.
        std::fs::write(
            d.path().join("config.toml"),
            "[[status_group]]\nname = \"Empty\"\n",
        )
        .unwrap();
        let (g, problem) = status_groups(d.path());
        assert_eq!(g, default_status_groups());
        assert!(problem.unwrap().contains("Empty"));
        std::fs::write(
            d.path().join("config.toml"),
            "[[status_group]]\nname = \"X\"\ncategory = \"doing\"\n",
        )
        .unwrap();
        assert!(status_groups(d.path()).1.unwrap().contains("status_group"));
        // No file, or no groups: Jira's three categories.
        assert_eq!(
            status_groups(&d.path().join("none")).0,
            default_status_groups()
        );
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
