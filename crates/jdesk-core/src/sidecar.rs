//! Your own things about issues, kept only on this computer and never sent to Jira:
//!
//! - `notes/ABC-123.md`: a plain Markdown note per issue, so you can also edit, grep or back
//!   them up outside the app.
//! - `local.json`: stars and local tags, by issue key.

use crate::journal::{Event, Journal};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Local {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub starred: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

impl Local {
    fn is_empty(&self) -> bool {
        !self.starred && self.tags.is_empty()
    }
}

pub struct Sidecar {
    dir: PathBuf,
    /// Every note, star and tag is recorded here first (see [`crate::journal`]).
    journal: Journal,
    local: BTreeMap<String, Local>,
    /// Keys that have a note file.
    noted: BTreeSet<String>,
}

/// Issue keys are `PROJ-123`; anything else never becomes a file name.
fn safe_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() < 64
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl Sidecar {
    pub fn open(dir: &Path) -> Result<Self> {
        let notes = dir.join("notes");
        std::fs::create_dir_all(&notes)
            .with_context(|| format!("cannot create {}", notes.display()))?;
        let local = match std::fs::read_to_string(dir.join("local.json")) {
            Ok(t) => serde_json::from_str(&t).context("local.json is damaged")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e).context("cannot read local.json"),
        };
        let mut noted = BTreeSet::new();
        for entry in std::fs::read_dir(&notes)?.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(key) = name.strip_suffix(".md")
                && safe_key(key)
                && entry.metadata().is_ok_and(|m| m.len() > 0)
            {
                noted.insert(key.to_string());
            }
        }
        Ok(Self {
            journal: Journal::in_data(dir),
            dir: dir.to_path_buf(),
            local,
            noted,
        })
    }

    pub fn note_path(&self, key: &str) -> Option<PathBuf> {
        safe_key(key).then(|| self.dir.join("notes").join(format!("{key}.md")))
    }

    pub fn has_note(&self, key: &str) -> bool {
        self.noted.contains(key)
    }

    pub fn note(&self, key: &str) -> String {
        self.note_path(key)
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default()
    }

    /// Saves a note; an empty one removes the file.
    pub fn set_note(&mut self, key: &str, text: &str) -> Result<()> {
        let Some(path) = self.note_path(key) else {
            anyhow::bail!("not an issue key: {key:?}");
        };
        self.journal.record(Event::Note {
            key: key.to_string(),
            text: if text.trim().is_empty() {
                String::new()
            } else {
                text.to_string()
            },
        })?;
        if text.trim().is_empty() {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            self.noted.remove(key);
        } else {
            crate::write_atomic(&path, text.as_bytes())?;
            self.noted.insert(key.to_string());
        }
        Ok(())
    }

    pub fn local(&self, key: &str) -> Local {
        self.local.get(key).cloned().unwrap_or_default()
    }

    pub fn starred(&self, key: &str) -> bool {
        self.local.get(key).is_some_and(|l| l.starred)
    }

    pub fn toggle_star(&mut self, key: &str) -> Result<bool> {
        let now = !self.starred(key);
        self.set_star(key, now)?;
        Ok(now)
    }

    /// Stars or unstars an issue.
    pub fn set_star(&mut self, key: &str, on: bool) -> Result<()> {
        self.journal.record(Event::Star {
            key: key.to_string(),
            on,
        })?;
        self.local.entry(key.to_string()).or_default().starred = on;
        self.save()
    }

    pub fn toggle_tag(&mut self, key: &str, tag: &str) -> Result<()> {
        let on = !self.local(key).tags.iter().any(|t| t == tag);
        self.journal.record(Event::Tag {
            key: key.to_string(),
            tag: tag.to_string(),
            on,
        })?;
        let l = self.local.entry(key.to_string()).or_default();
        if let Some(i) = l.tags.iter().position(|t| t == tag) {
            l.tags.remove(i);
        } else {
            l.tags.push(tag.to_string());
            l.tags.sort();
        }
        self.save()
    }

    /// Every local tag in use.
    pub fn all_tags(&self) -> BTreeSet<String> {
        self.local
            .values()
            .flat_map(|l| l.tags.iter().cloned())
            .collect()
    }

    fn save(&mut self) -> Result<()> {
        self.local.retain(|_, l| !l.is_empty());
        crate::write_atomic(
            &self.dir.join("local.json"),
            serde_json::to_string_pretty(&self.local)?.as_bytes(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_and_stars_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Sidecar::open(dir.path()).unwrap();
        s.set_note("ABC-1", "# Plan\n- ask Bo").unwrap();
        s.toggle_star("ABC-1").unwrap();
        s.toggle_tag("ABC-2", "follow-up").unwrap();
        assert!(s.set_note("../etc/passwd", "x").is_err());
        let s = Sidecar::open(dir.path()).unwrap();
        assert!(s.has_note("ABC-1"));
        assert_eq!(s.note("ABC-1"), "# Plan\n- ask Bo");
        assert!(s.starred("ABC-1"));
        assert_eq!(s.local("ABC-2").tags, vec!["follow-up"]);
        let mut s = s;
        s.set_note("ABC-1", "  ").unwrap();
        assert!(!s.has_note("ABC-1"));
        assert!(!dir.path().join("notes/ABC-1.md").exists());
    }
}
