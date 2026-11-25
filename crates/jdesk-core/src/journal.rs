//! An append-only record of everything you did that isn't (yet) in Jira, so it can be rebuilt
//! if a file is lost or damaged: every change queued, edited, dropped, applied, refused or
//! retried, every note saved (its full text), every star, and what each sync changed.
//!
//! One JSON object per line, in `journal/YYYY-MM.jsonl` in the data folder (a new file each
//! month, never rewritten). Each line is written and flushed to disk *before* the file it
//! describes (`queue.json`, a note, `local.json`) is replaced, so the journal is never behind
//! them. [`replay`] folds it back into a queue, notes and stars; `jdesk recover` uses it.
//!
//! The issues themselves are not journaled: they are Jira's, and a refresh downloads them again.

use crate::queue::{Change, Edit, Queue, State};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Serialises appends from the UI thread and the background apply.
static APPEND: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// A change entered the queue (as it was then, with what the field was).
    Queued {
        change: Change,
    },
    /// A waiting change was replaced (the same field changed again, labels merged).
    Edited {
        id: u64,
        key: String,
        edit: Edit,
    },
    /// A waiting change left the queue without being applied.
    Dropped {
        id: u64,
        key: String,
        reason: String,
    },
    /// What applying a change did.
    Result {
        id: u64,
        key: String,
        state: State,
    },
    /// A failed or conflicting change went back in the queue (`force`: overwrite Jira's value).
    Retried {
        id: u64,
        force: bool,
    },
    /// Applied changes removed from the history (cleared, or the oldest pruned).
    Forgotten {
        ids: Vec<u64>,
    },
    /// A note's full text after saving; empty means it was removed.
    Note {
        key: String,
        text: String,
    },
    Star {
        key: String,
        on: bool,
    },
    Tag {
        key: String,
        tag: String,
        on: bool,
    },
    /// An issue changed so much in Jira that its queued changes are held for your review.
    ReviewRaised {
        key: String,
        signals: Vec<crate::identity::Signal>,
    },
    /// You decided: the same work (kept), or repurposed (detached, with the archive folder).
    ReviewResolved {
        key: String,
        outcome: crate::identity::ReviewStatus,
    },
    /// A request as it is after a change (made, handed off, seen, confirmed, dropped).
    Request {
        request: crate::requests::Request,
    },
    /// What a sync changed in the cache (keys only: the issues are Jira's).
    Synced {
        full: bool,
        changed: Vec<String>,
        removed: Vec<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Unix seconds.
    pub at: i64,
    #[serde(flatten)]
    pub event: Event,
}

#[derive(Clone, Debug)]
pub struct Journal {
    dir: PathBuf,
}

impl Journal {
    /// The journal in `data_dir/journal/`.
    pub fn in_data(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join("journal"),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Appends one event and flushes it to disk.
    pub fn record(&self, event: Event) -> Result<()> {
        let now = time::OffsetDateTime::now_utc();
        let entry = Entry {
            at: now.unix_timestamp(),
            event,
        };
        let mut line = serde_json::to_string(&entry)?;
        line.push('\n');
        let file = self
            .dir
            .join(format!("{:04}-{:02}.jsonl", now.year(), now.month() as u8));
        let _guard = APPEND.lock().unwrap_or_else(|e| e.into_inner());
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("cannot create {}", self.dir.display()))?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file)
            .with_context(|| format!("cannot open the journal {}", file.display()))?;
        f.write_all(line.as_bytes())
            .and_then(|()| f.sync_data())
            .with_context(|| format!("cannot write the journal {}", file.display()))?;
        Ok(())
    }

    /// Every entry, oldest first, and how many lines could not be read (a line cut short by a
    /// crash, say; the rest still count).
    pub fn read(&self) -> Result<(Vec<Entry>, usize)> {
        let mut files: Vec<PathBuf> = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e).context("cannot read the journal folder"),
        };
        files.sort();
        let mut out = Vec::new();
        let mut bad = 0;
        for f in files {
            let text = std::fs::read_to_string(&f)
                .with_context(|| format!("cannot read {}", f.display()))?;
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                match serde_json::from_str::<Entry>(line) {
                    Ok(e) => out.push(e),
                    Err(_) => bad += 1,
                }
            }
        }
        Ok((out, bad))
    }
}

/// Records an event if there is a journal (tests and demo copies may have none).
pub(crate) fn record(j: &Option<Journal>, event: Event) -> Result<()> {
    match j {
        Some(j) => j.record(event),
        None => Ok(()),
    }
}

/// What the journal says the queue, notes and stars should be.
#[derive(Debug, Default)]
pub struct Rebuilt {
    /// Every change the journal knows that isn't forgotten, in the order queued.
    pub changes: Vec<Change>,
    /// Each note's last saved text (empty: removed).
    pub notes: BTreeMap<String, String>,
    pub stars: BTreeMap<String, bool>,
    pub tags: BTreeMap<(String, String), bool>,
    /// Each request as last recorded.
    pub requests: BTreeMap<u64, crate::requests::Request>,
    pub entries: usize,
}

/// Folds the journal back into state.
pub fn replay(entries: &[Entry]) -> Rebuilt {
    let mut r = Rebuilt {
        entries: entries.len(),
        ..Default::default()
    };
    for e in entries {
        match &e.event {
            Event::Queued { change } => {
                r.changes.retain(|c| c.id != change.id);
                r.changes.push(change.clone());
            }
            Event::Edited { id, edit, .. } => {
                if let Some(c) = r.changes.iter_mut().find(|c| c.id == *id) {
                    c.edit = edit.clone();
                }
            }
            Event::Dropped { id, .. } => r.changes.retain(|c| c.id != *id),
            Event::Result { id, state, .. } => {
                if let Some(c) = r.changes.iter_mut().find(|c| c.id == *id) {
                    c.state = state.clone();
                }
            }
            Event::Retried { id, force } => {
                if let Some(c) = r.changes.iter_mut().find(|c| c.id == *id) {
                    c.state = State::Pending;
                    if *force {
                        c.base = None;
                    }
                }
            }
            Event::Forgotten { ids } => r.changes.retain(|c| !ids.contains(&c.id)),
            Event::Note { key, text } => {
                r.notes.insert(key.clone(), text.clone());
            }
            Event::Star { key, on } => {
                r.stars.insert(key.clone(), *on);
            }
            Event::Tag { key, tag, on } => {
                r.tags.insert((key.clone(), tag.clone()), *on);
            }
            Event::Request { request } => {
                r.requests.insert(request.id, request.clone());
            }
            Event::Synced { .. } | Event::ReviewRaised { .. } | Event::ReviewResolved { .. } => {}
        }
    }
    r
}

impl Rebuilt {
    /// The rebuilt queue, saved to `path` when [`Queue::save`] is called.
    pub fn queue(&self, path: &Path) -> Queue {
        Queue::from_changes(path, self.changes.clone())
    }
}

/// One line for people (`jdesk journal`).
pub fn describe(e: &Entry) -> String {
    let when = time::OffsetDateTime::from_unix_timestamp(e.at)
        .map(|t| {
            format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}Z",
                t.year(),
                t.month() as u8,
                t.day(),
                t.hour(),
                t.minute(),
                t.second()
            )
        })
        .unwrap_or_default();
    let what = match &e.event {
        Event::Queued { change } => {
            format!(
                "queued   #{} {} {}",
                change.id,
                change.key,
                change.edit.describe()
            )
        }
        Event::Edited { id, key, edit } => format!("edited   #{id} {key} {}", edit.describe()),
        Event::Dropped { id, key, reason } => format!("dropped  #{id} {key} ({reason})"),
        Event::Result { id, key, state } => match state {
            State::Applied { .. } => format!("applied  #{id} {key}"),
            State::Conflict { remote, .. } => format!("conflict #{id} {key}: Jira has “{remote}”"),
            State::Failed { error, .. } => format!("failed   #{id} {key}: {error}"),
            State::Pending => format!("pending  #{id} {key}"),
        },
        Event::Retried { id, force } => {
            format!("retry    #{id}{}", if *force { " (overwrite)" } else { "" })
        }
        Event::Forgotten { ids } => format!("forgot   {} applied change(s)", ids.len()),
        Event::Note { key, text } if text.trim().is_empty() => format!("note     {key} removed"),
        Event::Note { key, text } => format!(
            "note     {key} saved ({} line{})",
            text.lines().count(),
            if text.lines().count() == 1 { "" } else { "s" }
        ),
        Event::Star { key, on } => format!("star     {key} {}", if *on { "on" } else { "off" }),
        Event::Tag { key, tag, on } => {
            format!("tag      {key} {}{tag}", if *on { "+" } else { "-" })
        }
        Event::ReviewRaised { key, signals } => format!(
            "review   {key}: {}",
            signals
                .iter()
                .map(|s| s.describe())
                .collect::<Vec<_>>()
                .join("; ")
        ),
        Event::ReviewResolved { key, outcome } => match outcome {
            crate::identity::ReviewStatus::Kept { .. } => {
                format!("resolved {key}: same work, kept")
            }
            crate::identity::ReviewStatus::Detached { archive, .. } => {
                format!("resolved {key}: repurposed, your things moved to {archive}")
            }
            crate::identity::ReviewStatus::Open => format!("review   {key} reopened"),
        },
        Event::Request { request } => {
            format!("request  {}: {}", request.describe(), request.state.word())
        }
        Event::Synced {
            full,
            changed,
            removed,
        } => format!(
            "sync     {}{} changed, {} removed",
            if *full { "full: " } else { "" },
            changed.len(),
            removed.len()
        ),
    };
    format!("{when}  {what}")
}

#[cfg(test)]
mod tests {
    use crate::queue::{Edit, State};
    use crate::{Desk, Source, demo::DemoJira, sync};
    use std::sync::Arc;

    #[test]
    fn lost_files_are_rebuilt_from_the_journal() {
        let dir = tempfile::tempdir().unwrap();
        let jira = Arc::new(DemoJira::new(30));
        let mut desk = Desk::open_with(dir.path(), Source::Demo(jira.clone())).unwrap();
        let out = desk
            .sync_job()
            .unwrap()
            .run(sync::Mode::Auto, &|_| {})
            .unwrap();
        desk.merge(out);
        let open: Vec<String> = desk
            .issues()
            .iter()
            .filter(|i| i.status != "Done" && i.kind != "Epic")
            .map(|i| i.key.clone())
            .collect();
        let (a, b, c) = (&open[0], &open[1], &open[2]);

        desk.queue_edit(a, Edit::Transition { to: "Done".into() })
            .unwrap();
        desk.queue_edit(
            a,
            Edit::Labels {
                add: vec!["x".into()],
                remove: vec![],
            },
        )
        .unwrap();
        desk.queue_edit(
            a,
            Edit::Labels {
                add: vec!["y".into()],
                remove: vec![],
            },
        )
        .unwrap(); // edited
        desk.queue_edit(b, Edit::Summary { to: "Mine".into() })
            .unwrap();
        desk.queue_edit(
            c,
            Edit::Comment {
                text: "first\nsecond".into(),
            },
        )
        .unwrap();
        let undone = desk
            .queue
            .changes()
            .iter()
            .find(|x| x.key == *c)
            .unwrap()
            .id;
        desk.queue.discard(undone).unwrap(); // dropped
        desk.queue_edit(
            c,
            Edit::Comment {
                text: "kept".into(),
            },
        )
        .unwrap();
        // b conflicts on apply; the rest apply.
        jira.touch(b, "Theirs", 2_000);
        let map = desk.field_map.clone();
        let only: Vec<u64> = desk
            .queue
            .changes()
            .iter()
            .filter(|x| x.key == *a || x.key == *b)
            .map(|x| x.id)
            .collect();
        desk.queue
            .apply(
                jira.as_ref(),
                &map,
                Some(&only),
                3_000,
                &Default::default(),
                &|_| {},
            )
            .unwrap();
        desk.sidecar.set_note(c, "# plan\nask Bo").unwrap();
        desk.sidecar.toggle_star(a).unwrap();
        let before = desk.queue.changes().to_vec();
        assert_eq!(before.len(), 4, "{before:#?}");
        assert!(
            before
                .iter()
                .any(|x| matches!(x.state, State::Conflict { .. }))
        );

        // Nothing to recover while the files are intact.
        assert!(
            crate::recover(dir.path(), false)
                .unwrap()
                .differences
                .is_empty()
        );

        // Lose the queue, the note and the stars.
        std::fs::remove_file(dir.path().join("queue.json")).unwrap();
        std::fs::remove_file(dir.path().join(format!("notes/{c}.md"))).unwrap();
        std::fs::remove_file(dir.path().join("local.json")).unwrap();
        // A line cut short by a crash is skipped, not fatal.
        let jdir = dir.path().join("journal");
        let file = std::fs::read_dir(&jdir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .and_then(|mut f| {
                use std::io::Write;
                f.write_all(b"{\"at\":1,\"event\":\"queu")
            })
            .unwrap();

        let dry = crate::recover(dir.path(), false).unwrap();
        assert_eq!(dry.unreadable_lines, 1);
        assert!(dry.backup.is_none());
        assert_eq!(dry.differences.len(), 4 + 1 + 1, "{:#?}", dry.differences);
        assert!(
            !dir.path().join("queue.json").exists(),
            "a dry run writes nothing"
        );

        let done = crate::recover(dir.path(), true).unwrap();
        assert!(done.backup.as_ref().unwrap().exists());
        let back = Desk::open_with(dir.path(), Source::Demo(jira)).unwrap();
        assert_eq!(back.queue.changes(), &before[..]);
        assert_eq!(back.sidecar.note(c), "# plan\nask Bo");
        assert!(back.sidecar.starred(a));
        assert!(
            crate::recover(dir.path(), false)
                .unwrap()
                .differences
                .is_empty()
        );
    }
}
