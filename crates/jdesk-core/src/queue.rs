//! Changes you make in the app are queued here, never sent straight away. You review them in
//! the Changes pane and apply them when you choose.
//!
//! Each change remembers what the field was when you made it (its *base*). Applying first
//! reads the issue from Jira again: if someone else changed that field since, the change
//! stops as a **conflict** instead of overwriting their edit. Labels and comments can't
//! conflict (labels are sent as add/remove, not as a whole list).
//!
//! Saved as `queue.json` in the data folder after every change, so nothing queued is lost.

use crate::client::Jira;
use crate::journal::{self, Event, Journal};
use crate::model::{Issue, Person, Sprint};
use crate::parse::{self, FieldMap};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Edit {
    /// Move to the status with this name.
    Transition {
        to: String,
    },
    Assign {
        to: Option<Person>,
    },
    Summary {
        to: String,
    },
    Points {
        to: Option<f64>,
    },
    Priority {
        to: String,
    },
    Labels {
        add: Vec<String>,
        remove: Vec<String>,
    },
    Sprint {
        to: Sprint,
    },
    Comment {
        text: String,
    },
}

impl Edit {
    /// Changes of the same kind to the same issue replace each other (except comments).
    fn slot(&self) -> &'static str {
        match self {
            Edit::Transition { .. } => "status",
            Edit::Assign { .. } => "assignee",
            Edit::Summary { .. } => "summary",
            Edit::Points { .. } => "points",
            Edit::Priority { .. } => "priority",
            Edit::Labels { .. } => "labels",
            Edit::Sprint { .. } => "sprint",
            Edit::Comment { .. } => "comment",
        }
    }

    /// Whether the change can conflict with someone else's edit.
    fn checked(&self) -> bool {
        !matches!(self, Edit::Labels { .. } | Edit::Comment { .. })
    }

    /// The field's value as this change sets it, in the form [`current`] reads it.
    fn target(&self) -> String {
        match self {
            Edit::Transition { to } => to.to_lowercase(),
            Edit::Assign { to } => to.as_ref().map(|p| p.id.clone()).unwrap_or_default(),
            Edit::Summary { to } => to.clone(),
            Edit::Points { to } => points(*to),
            Edit::Priority { to } => to.clone(),
            Edit::Sprint { to } => to.id.to_string(),
            Edit::Labels { .. } | Edit::Comment { .. } => String::new(),
        }
    }

    /// One line for the Changes pane: "Status → In Review".
    pub fn describe(&self) -> String {
        match self {
            Edit::Transition { to } => format!("Status → {to}"),
            Edit::Assign { to: Some(p) } => format!("Assignee → {}", p.name),
            Edit::Assign { to: None } => "Unassign".into(),
            Edit::Summary { to } => format!("Summary → “{to}”"),
            Edit::Points { to: Some(p) } => format!("Story points → {}", points(Some(*p))),
            Edit::Points { to: None } => "Clear story points".into(),
            Edit::Priority { to } => format!("Priority → {to}"),
            Edit::Labels { add, remove } => {
                let mut parts: Vec<String> = add.iter().map(|l| format!("+{l}")).collect();
                parts.extend(remove.iter().map(|l| format!("−{l}")));
                format!("Labels {}", parts.join(" "))
            }
            Edit::Sprint { to } => format!("Sprint → {}", to.name),
            Edit::Comment { text } => {
                let first = text.lines().next().unwrap_or_default();
                let more = if text.lines().count() > 1 || first.chars().count() > 60 {
                    "…"
                } else {
                    ""
                };
                format!(
                    "Comment “{}{more}”",
                    first.chars().take(60).collect::<String>()
                )
            }
        }
    }
}

fn points(p: Option<f64>) -> String {
    match p {
        Some(v) if v.fract() == 0.0 => format!("{v:.0}"),
        Some(v) => format!("{v}"),
        None => String::new(),
    }
}

/// The field a change touches, as it is on `issue` now.
pub fn current(issue: &Issue, edit: &Edit) -> String {
    match edit {
        Edit::Transition { .. } => issue.status.to_lowercase(),
        Edit::Assign { .. } => issue
            .assignee
            .as_ref()
            .map(|p| p.id.clone())
            .unwrap_or_default(),
        Edit::Summary { .. } => issue.summary.clone(),
        Edit::Points { .. } => points(issue.story_points),
        Edit::Priority { .. } => issue.priority.clone().unwrap_or_default(),
        Edit::Sprint { .. } => issue
            .current_sprint()
            .filter(|s| s.state != crate::model::SprintState::Closed)
            .map(|s| s.id.to_string())
            .unwrap_or_default(),
        Edit::Labels { .. } | Edit::Comment { .. } => String::new(),
    }
}

/// How the value read for a conflict looks to a person.
fn shown(issue: &Issue, edit: &Edit) -> String {
    match edit {
        Edit::Transition { .. } => issue.status.clone(),
        Edit::Assign { .. } => issue
            .assignee
            .as_ref()
            .map_or("Unassigned".into(), |p| p.name.clone()),
        Edit::Sprint { .. } => issue
            .current_sprint()
            .map_or("no sprint".into(), |s| s.name.clone()),
        _ => match current(issue, edit) {
            s if s.is_empty() => "(empty)".into(),
            s => s,
        },
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum State {
    Pending,
    /// Jira refused it (the message says why). Retry puts it back in the queue.
    Failed {
        error: String,
        at: i64,
    },
    /// Someone changed the field in Jira since you queued this. `remote` is what it is now.
    Conflict {
        remote: String,
        at: i64,
    },
    Applied {
        at: i64,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Change {
    pub id: u64,
    pub key: String,
    pub edit: Edit,
    /// The field's value when queued; `None` when the change can't conflict or was forced.
    pub base: Option<String>,
    /// How `base` looks to a person, for the Changes pane.
    #[serde(default)]
    pub base_shown: String,
    pub queued_at: i64,
    pub state: State,
}

impl Change {
    pub fn is_open(&self) -> bool {
        !matches!(self.state, State::Applied { .. })
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Queue {
    next_id: u64,
    changes: Vec<Change>,
    #[serde(skip)]
    path: Option<PathBuf>,
    /// Where every change to the queue is recorded first (see [`crate::journal`]).
    #[serde(skip)]
    journal: Option<Journal>,
}

/// Applied changes kept for the history in the Changes pane.
const HISTORY: usize = 200;

/// What applying the queue did.
#[derive(Debug, Default)]
pub struct ApplyReport {
    pub applied: usize,
    pub failed: usize,
    pub conflicts: usize,
    /// Issues read back from Jira after their changes, for the cache.
    pub refreshed: Vec<Issue>,
    /// Issues whose changes were held: under review, or found changed materially just now.
    pub held: Vec<String>,
    /// Issues found changed materially on the fresh copy (a review should open for each).
    pub raised: Vec<(Issue, Vec<crate::identity::Signal>)>,
}

impl Queue {
    /// Loads `queue.json` (an empty queue if there is none).
    pub fn open(path: &Path) -> Result<Self> {
        let mut q: Queue = match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("{} is damaged; fix or move it aside", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Queue::default(),
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
        };
        q.path = Some(path.to_path_buf());
        q.journal = path.parent().map(Journal::in_data);
        Ok(q)
    }

    /// A queue holding `changes` (rebuilt from the journal), saved to `path`.
    pub fn from_changes(path: &Path, changes: Vec<Change>) -> Self {
        Queue {
            next_id: changes.iter().map(|c| c.id).max().unwrap_or(0),
            changes,
            path: Some(path.to_path_buf()),
            journal: path.parent().map(Journal::in_data),
        }
    }

    /// The journal this queue records to, if any.
    pub fn journal(&self) -> Option<&Journal> {
        self.journal.as_ref()
    }

    fn record(&self, event: Event) -> Result<()> {
        journal::record(&self.journal, event)
    }

    pub fn save(&self) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        crate::write_atomic(path, serde_json::to_string_pretty(self)?.as_bytes())
    }

    pub fn changes(&self) -> &[Change] {
        &self.changes
    }

    pub fn open_changes(&self) -> impl Iterator<Item = &Change> {
        self.changes.iter().filter(|c| c.is_open())
    }

    pub fn pending_count(&self) -> usize {
        self.changes
            .iter()
            .filter(|c| c.state == State::Pending)
            .count()
    }

    pub fn for_issue<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a Change> + 'a {
        self.changes
            .iter()
            .filter(move |c| c.key == key && c.is_open())
    }

    /// Queues `edit` on `issue` (as Jira last said it was). A change to a field that already
    /// has one waiting replaces it; one that puts the field back as it was removes it.
    /// Returns whether the queue changed.
    pub fn add(&mut self, issue: &Issue, edit: Edit, now: i64) -> Result<bool> {
        if let Edit::Comment { text } = &edit
            && text.trim().is_empty()
        {
            return Ok(false);
        }
        let waiting = self.changes.iter().position(|c| {
            c.key == issue.key
                && c.edit.slot() == edit.slot()
                && c.state == State::Pending
                && !matches!(c.edit, Edit::Comment { .. })
        });
        let edit = match (edit, waiting) {
            (Edit::Labels { add, remove }, w) => {
                let (mut a, mut r) = match w.map(|i| &self.changes[i].edit) {
                    Some(Edit::Labels { add, remove }) => (add.clone(), remove.clone()),
                    _ => (Vec::new(), Vec::new()),
                };
                for l in add {
                    if let Some(i) = r.iter().position(|x| *x == l) {
                        r.remove(i);
                    } else if !issue.labels.contains(&l) && !a.contains(&l) {
                        a.push(l);
                    }
                }
                for l in remove {
                    if let Some(i) = a.iter().position(|x| *x == l) {
                        a.remove(i);
                    } else if issue.labels.contains(&l) && !r.contains(&l) {
                        r.push(l);
                    }
                }
                Edit::Labels { add: a, remove: r }
            }
            (e, _) => e,
        };
        let noop = match &edit {
            Edit::Labels { add, remove } => add.is_empty() && remove.is_empty(),
            Edit::Comment { .. } => false,
            e => {
                let base = waiting
                    .and_then(|i| self.changes[i].base.clone())
                    .unwrap_or_else(|| current(issue, e));
                e.target() == base
            }
        };
        // Each branch records to the journal before the queue changes or is saved.
        match (waiting, noop) {
            (Some(i), true) => {
                let c = &self.changes[i];
                self.record(Event::Dropped {
                    id: c.id,
                    key: c.key.clone(),
                    reason: "set back to its value in Jira".into(),
                })?;
                self.changes.remove(i);
            }
            (Some(i), false) => {
                let c = &self.changes[i];
                self.record(Event::Edited {
                    id: c.id,
                    key: c.key.clone(),
                    edit: edit.clone(),
                })?;
                self.changes[i].edit = edit;
            }
            (None, true) => return Ok(false),
            (None, false) => {
                let change = Change {
                    id: self.next_id + 1,
                    key: issue.key.clone(),
                    base: edit.checked().then(|| current(issue, &edit)),
                    base_shown: shown(issue, &edit),
                    edit,
                    queued_at: now,
                    state: State::Pending,
                };
                self.record(Event::Queued {
                    change: change.clone(),
                })?;
                self.next_id += 1;
                self.changes.push(change);
            }
        }
        self.save()?;
        Ok(true)
    }

    /// Drops a change that hasn't been applied.
    pub fn discard(&mut self, id: u64) -> Result<bool> {
        if let Some(c) = self.changes.iter().find(|c| c.id == id && c.is_open()) {
            self.record(Event::Dropped {
                id,
                key: c.key.clone(),
                reason: "discarded".into(),
            })?;
        }
        let before = self.changes.len();
        self.changes.retain(|c| !(c.id == id && c.is_open()));
        let changed = self.changes.len() != before;
        if changed {
            self.save()?;
        }
        Ok(changed)
    }

    /// Puts a failed change back in the queue. For a conflict, `force` drops the check, so
    /// applying overwrites the other edit.
    pub fn retry(&mut self, id: u64, force: bool) -> Result<bool> {
        let Some(c) = self.changes.iter().find(|c| c.id == id) else {
            return Ok(false);
        };
        let force = match c.state {
            State::Failed { .. } => false,
            State::Conflict { .. } if force => true,
            _ => return Ok(false),
        };
        self.record(Event::Retried { id, force })?;
        let c = self
            .changes
            .iter_mut()
            .find(|c| c.id == id)
            .expect("found above");
        if force {
            c.base = None;
        }
        c.state = State::Pending;
        self.save()?;
        Ok(true)
    }

    /// Forgets applied changes (the history).
    pub fn clear_history(&mut self) -> Result<()> {
        let ids: Vec<u64> = self
            .changes
            .iter()
            .filter(|c| !c.is_open())
            .map(|c| c.id)
            .collect();
        if !ids.is_empty() {
            self.record(Event::Forgotten { ids })?;
        }
        self.changes.retain(Change::is_open);
        self.save()
    }

    /// `issue` with every open change on it applied, as the app shows it.
    pub fn effective(&self, issue: &Issue) -> Issue {
        let mut out = issue.clone();
        for c in self.for_issue(&issue.key) {
            if c.state != State::Pending {
                continue;
            }
            match &c.edit {
                Edit::Transition { to } => out.status = to.clone(),
                Edit::Assign { to } => out.assignee = to.clone(),
                Edit::Summary { to } => out.summary = to.clone(),
                Edit::Points { to } => out.story_points = *to,
                Edit::Priority { to } => out.priority = Some(to.clone()),
                Edit::Labels { add, remove } => {
                    out.labels.retain(|l| !remove.contains(l));
                    out.labels.extend(add.iter().cloned());
                }
                Edit::Sprint { to } => {
                    out.sprints
                        .retain(|s| s.state == crate::model::SprintState::Closed);
                    out.sprints.push(to.clone());
                }
                Edit::Comment { .. } => {}
            }
        }
        out
    }

    /// Applies every pending change (or only `only`), issue by issue: read the issue from
    /// Jira, check each change against it, send it, then read the issue back.
    pub fn apply(
        &mut self,
        jira: &dyn Jira,
        map: &FieldMap,
        only: Option<&[u64]>,
        now: i64,
        guard: &crate::identity::ApplyGuard,
        progress: &dyn Fn(String),
    ) -> Result<ApplyReport> {
        let mut report = ApplyReport::default();
        let mut keys: Vec<String> = Vec::new();
        for c in &self.changes {
            if c.state == State::Pending
                && only.is_none_or(|ids| ids.contains(&c.id))
                && !keys.contains(&c.key)
            {
                keys.push(c.key.clone());
            }
        }
        let fields = map.search_fields();
        for key in keys {
            if guard.held.contains(&key) {
                // Waiting for your review: nothing is sent, nothing is fetched.
                report.held.push(key);
                continue;
            }
            progress(format!("Applying changes to {key}"));
            let raw = match jira.issue(&key, &fields) {
                Ok(v) => v,
                Err(e) => {
                    self.fail_all(&key, only, &e.to_string(), now, &mut report)?;
                    self.save()?;
                    continue;
                }
            };
            let Some(remote) = parse::issue(&raw, map) else {
                self.fail_all(
                    &key,
                    only,
                    "Jira's copy of the issue could not be read",
                    now,
                    &mut report,
                )?;
                self.save()?;
                continue;
            };
            // Repurposed since your changes were made? Then send nothing; a review opens.
            if let Some(signals) = guard.check(&remote) {
                report.held.push(key.clone());
                report.raised.push((remote, signals));
                continue;
            }
            let ids: Vec<u64> = self
                .changes
                .iter()
                .filter(|c| {
                    c.key == key
                        && c.state == State::Pending
                        && only.is_none_or(|ids| ids.contains(&c.id))
                })
                .map(|c| c.id)
                .collect();
            for id in ids {
                let c = self
                    .changes
                    .iter()
                    .find(|c| c.id == id)
                    .expect("listed")
                    .clone();
                let state = if c.edit.checked() && current(&remote, &c.edit) == c.edit.target() {
                    // Already so in Jira (someone else did it, or an earlier apply got through).
                    State::Applied { at: now }
                } else if c
                    .base
                    .as_ref()
                    .is_some_and(|b| *b != current(&remote, &c.edit))
                {
                    State::Conflict {
                        remote: shown(&remote, &c.edit),
                        at: now,
                    }
                } else {
                    match send(jira, map, &raw, &key, &c.edit) {
                        Ok(()) => State::Applied { at: now },
                        Err(e) => State::Failed {
                            error: e.to_string(),
                            at: now,
                        },
                    }
                };
                match state {
                    State::Applied { .. } => report.applied += 1,
                    State::Failed { .. } => report.failed += 1,
                    State::Conflict { .. } => report.conflicts += 1,
                    State::Pending => {}
                }
                self.record(Event::Result {
                    id,
                    key: key.clone(),
                    state: state.clone(),
                })?;
                if let Some(slot) = self.changes.iter_mut().find(|x| x.id == id) {
                    slot.state = state;
                }
                self.save()?;
            }
            if let Ok(v) = jira.issue(&key, &fields)
                && let Some(i) = parse::issue(&v, map)
            {
                report.refreshed.push(i);
            }
        }
        // Keep only the newest applied changes as history.
        let applied = self.changes.iter().filter(|c| !c.is_open()).count();
        if applied > HISTORY {
            let mut drop = applied - HISTORY;
            let oldest: Vec<u64> = self
                .changes
                .iter()
                .filter(|c| !c.is_open())
                .take(drop)
                .map(|c| c.id)
                .collect();
            self.record(Event::Forgotten { ids: oldest })?;
            self.changes.retain(|c| {
                if !c.is_open() && drop > 0 {
                    drop -= 1;
                    false
                } else {
                    true
                }
            });
        }
        self.save()?;
        Ok(report)
    }

    fn fail_all(
        &mut self,
        key: &str,
        only: Option<&[u64]>,
        error: &str,
        now: i64,
        r: &mut ApplyReport,
    ) -> Result<()> {
        let ids: Vec<u64> = self
            .changes
            .iter()
            .filter(|c| {
                c.key == key
                    && c.state == State::Pending
                    && only.is_none_or(|ids| ids.contains(&c.id))
            })
            .map(|c| c.id)
            .collect();
        let state = State::Failed {
            error: error.to_string(),
            at: now,
        };
        for id in ids {
            self.record(Event::Result {
                id,
                key: key.to_string(),
                state: state.clone(),
            })?;
            if let Some(c) = self.changes.iter_mut().find(|c| c.id == id) {
                c.state = state.clone();
            }
            r.failed += 1;
        }
        Ok(())
    }
}

/// Sends one change to Jira.
fn send(
    jira: &dyn Jira,
    map: &FieldMap,
    raw: &serde_json::Value,
    key: &str,
    edit: &Edit,
) -> Result<()> {
    match edit {
        Edit::Transition { to } => {
            let ts = jira.transitions(key)?;
            let t = ts
                .iter()
                .find(|t| t.to.eq_ignore_ascii_case(to))
                .or_else(|| ts.iter().find(|t| t.name.eq_ignore_ascii_case(to)));
            match t {
                Some(t) => jira.transition(key, &t.id),
                None => bail!(
                    "no transition to “{to}” from here (Jira offers: {})",
                    ts.iter()
                        .map(|t| t.to.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }
        }
        Edit::Assign { to } => jira.assign(key, to.as_ref().map(|p| p.id.as_str())),
        Edit::Summary { to } => jira.edit(key, &json!({"fields": {"summary": to}})),
        Edit::Priority { to } => jira.edit(key, &json!({"fields": {"priority": {"name": to}}})),
        Edit::Points { to } => {
            // The story-points field this issue uses: the one with a value, else the first.
            let field = map
                .story_points
                .iter()
                .find(|f| !raw["fields"][f.as_str()].is_null())
                .or_else(|| map.story_points.first());
            match field {
                Some(f) => jira.edit(key, &json!({"fields": {f.as_str(): to}})),
                None => bail!("this site has no story points field"),
            }
        }
        Edit::Labels { add, remove } => {
            let mut ops: Vec<serde_json::Value> = add.iter().map(|l| json!({"add": l})).collect();
            ops.extend(remove.iter().map(|l| json!({"remove": l})));
            jira.edit(key, &json!({"update": {"labels": ops}}))
        }
        Edit::Sprint { to } => jira.move_to_sprint(to.id, key),
        Edit::Comment { text } => jira.comment(key, text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::DemoJira;
    use crate::sync;

    fn setup() -> (DemoJira, FieldMap, Vec<Issue>) {
        let jira = DemoJira::new(12);
        let mut store = crate::store::Store::in_memory().unwrap();
        sync::sync(
            &jira,
            &mut store,
            "project = DEMO",
            &Default::default(),
            sync::Mode::Full,
            1_000,
            &|_| {},
        )
        .unwrap();
        let map = store.meta::<FieldMap>("field_map").unwrap().unwrap();
        let mut issues = store.load_all().unwrap();
        issues.sort_by_key(Issue::key_number);
        (jira, map, issues)
    }

    #[test]
    fn same_field_replaces_and_putting_it_back_drops_the_change() {
        let (_, _, issues) = setup();
        let i = &issues[1];
        let mut q = Queue::default();
        assert!(q.add(i, Edit::Summary { to: "A".into() }, 1).unwrap());
        assert!(q.add(i, Edit::Summary { to: "B".into() }, 2).unwrap());
        assert_eq!(q.changes().len(), 1);
        assert_eq!(q.effective(i).summary, "B");
        assert!(
            q.add(
                i,
                Edit::Summary {
                    to: i.summary.clone()
                },
                3
            )
            .unwrap()
        );
        assert!(q.changes().is_empty());
        // Labels merge into one add/remove change, and cancel out.
        q.add(
            i,
            Edit::Labels {
                add: vec!["x".into()],
                remove: vec![],
            },
            4,
        )
        .unwrap();
        q.add(
            i,
            Edit::Labels {
                add: vec!["y".into()],
                remove: vec![],
            },
            5,
        )
        .unwrap();
        assert_eq!(q.changes().len(), 1);
        assert!(q.effective(i).labels.contains(&"y".to_string()));
        q.add(
            i,
            Edit::Labels {
                add: vec![],
                remove: vec!["x".into(), "y".into()],
            },
            6,
        )
        .unwrap();
        assert!(q.changes().is_empty());
    }

    #[test]
    fn apply_sends_changes_and_stops_on_conflicts() {
        let (jira, map, issues) = setup();
        let open: Vec<&Issue> = issues
            .iter()
            .filter(|i| i.status != "Done" && i.kind != "Epic")
            .collect();
        let (a, b) = (open[0], open[1]);
        let mut q = Queue::default();
        q.add(a, Edit::Transition { to: "Done".into() }, 1).unwrap();
        q.add(
            a,
            Edit::Comment {
                text: "shipped".into(),
            },
            1,
        )
        .unwrap();
        q.add(
            a,
            Edit::Labels {
                add: vec!["shipped".into()],
                remove: vec![],
            },
            1,
        )
        .unwrap();
        q.add(b, Edit::Summary { to: "Mine".into() }, 1).unwrap();
        // Someone renames b in Jira meanwhile.
        jira.touch(&b.key, "Theirs", 2_000);
        let r = q
            .apply(&jira, &map, None, 3_000, &Default::default(), &|_| {})
            .unwrap();
        assert_eq!(
            (r.applied, r.conflicts, r.failed),
            (3, 1, 0),
            "{:#?}",
            q.changes()
        );
        let a2 = r.refreshed.iter().find(|i| i.key == a.key).unwrap();
        assert_eq!(a2.status, "Done");
        assert!(a2.labels.contains(&"shipped".to_string()));
        assert_eq!(a2.comment_total, a.comment_total + 1);
        let conflict = q.changes().iter().find(|c| c.key == b.key).unwrap();
        assert_eq!(
            conflict.state,
            State::Conflict {
                remote: "Theirs".into(),
                at: 3_000
            }
        );
        // Forcing it overwrites theirs.
        q.retry(conflict.id, true).unwrap();
        let r = q
            .apply(&jira, &map, None, 4_000, &Default::default(), &|_| {})
            .unwrap();
        assert_eq!(r.applied, 1);
        assert_eq!(r.refreshed[0].summary, "Mine");
    }
}
