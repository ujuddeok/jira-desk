//! Jira Desk's engine, with no UI: a one-way copy of your Jira issues on disk, a queue of
//! changes you choose when to send back, and notes that never leave your computer.
//!
//! The data folder holds:
//!
//! ```text
//! config.toml       which site, how to sign in, which issues (JQL)
//! cache.sqlite      what Jira said; safe to delete (it is downloaded again)
//! queue.json        your changes waiting to be applied, and recent history
//! notes/KEY.md      your notes, one Markdown file per issue
//! local.json        stars and local tags
//! layout/           the window layout
//! ```

pub mod adf;
pub mod client;
pub mod config;
pub mod control;
pub mod demo;
pub mod filter;
pub mod identity;
pub mod journal;
pub mod mcp;
pub mod model;
pub mod parse;
pub mod queue;
pub mod sidecar;
pub mod storage;
pub mod store;
pub mod sync;

use anyhow::{Context, Result};
use client::Jira;
use config::Config;
pub use identity::{ApplyGuard, Review, Signal};
use model::{Issue, Person, Sprint, SprintState};
use parse::FieldMap;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// An issue's description as plain text (for comparing and showing).
pub fn issue_text(issue: &Issue) -> String {
    adf::to_text(&issue.description)
}

/// Writes a file through a temporary file and a rename, so a crash never leaves half of it.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("")
    ));
    std::fs::write(&tmp, bytes).with_context(|| format!("cannot write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("cannot replace {}", path.display()))?;
    Ok(())
}

/// Makes `dir` reachable by you alone (mode 0700): it holds your copy of Jira, your notes and
/// `control.sock`, which drives the app. Only a folder's owner may change its mode, so this also
/// refuses a folder someone else owns.
pub fn make_private(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir)
            .with_context(|| format!("cannot read {}", dir.display()))?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).with_context(
                || {
                    format!(
                        "cannot make {} private to you (is it yours?)",
                        dir.display()
                    )
                },
            )?;
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

pub fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

/// Where the issues come from.
#[derive(Clone)]
// One per open data folder, so the size of the Jira variant doesn't matter.
#[allow(clippy::large_enum_variant)]
pub enum Source {
    Jira(Config),
    /// Made-up issues, in memory (`--demo`).
    Demo(Arc<demo::DemoJira>),
}

/// The data folder, open: cached issues, the change queue and the sidecar.
pub struct Desk {
    pub dir: PathBuf,
    pub source: Source,
    issues: Vec<Issue>,
    index: HashMap<String, usize>,
    pub queue: queue::Queue,
    pub sidecar: sidecar::Sidecar,
    pub myself: Option<Person>,
    pub field_map: FieldMap,
    pub last_sync: Option<i64>,
    /// People who can be assigned, from the last sync.
    pub people: Vec<Person>,
    /// Fingerprints of issues you attached things to, and reviews of ones that changed a lot.
    pub identities: identity::Identities,
}

/// What a background sync hands back to the [`Desk`].
pub struct SyncOutcome {
    pub report: sync::Report,
    pub myself: Option<Person>,
    pub field_map: Option<FieldMap>,
    pub people: Vec<Person>,
}

impl Desk {
    /// Opens `dir` (created if missing). Without a `config.toml` there is nothing to sync from
    /// yet: [`Desk::open`] fails with the path, and the app offers demo mode.
    pub fn open(dir: &Path) -> Result<Self> {
        let config_path = dir.join("config.toml");
        if !config_path.exists() {
            std::fs::create_dir_all(dir)?;
            make_private(dir)?;
            std::fs::write(dir.join("config.example.toml"), config::EXAMPLE)?;
            anyhow::bail!(
                "no settings yet: copy config.example.toml to config.toml in {} and fill it in",
                dir.display()
            );
        }
        let cfg = Config::load(&config_path)?;
        Self::open_with(dir, Source::Jira(cfg))
    }

    /// Demo mode: made-up issues in `dir/demo`, with a fake Jira that accepts changes.
    pub fn open_demo(dir: &Path, issues: usize) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        make_private(dir)?;
        let dir = dir.join("demo");
        std::fs::create_dir_all(&dir)?;
        // A fresh demo each launch: the fake Jira starts from scratch, so the cache must too.
        let _ = std::fs::remove_file(dir.join("cache.sqlite"));
        let _ = std::fs::remove_file(dir.join("queue.json"));
        // Its journal too: a demo's record would only describe earlier, discarded demos.
        let _ = std::fs::remove_dir_all(dir.join("journal"));
        Self::open_with(&dir, Source::Demo(Arc::new(demo::DemoJira::live(issues))))
    }

    pub fn open_with(dir: &Path, source: Source) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        make_private(dir)?;
        let store = store::Store::open(&dir.join("cache.sqlite"))?;
        let mut desk = Self {
            dir: dir.to_path_buf(),
            source,
            issues: Vec::new(),
            index: HashMap::new(),
            queue: queue::Queue::open(&dir.join("queue.json"))?,
            sidecar: sidecar::Sidecar::open(dir)?,
            myself: store.meta("myself")?,
            field_map: store.meta("field_map")?.unwrap_or_default(),
            last_sync: store.meta("last_sync")?,
            people: store.meta("people")?.unwrap_or_default(),
            identities: identity::Identities::open(dir)?,
        };
        desk.set_issues(store.load_all()?);
        // Fingerprint anything you already attached things to (folders from before reviews).
        let keys: Vec<String> = desk
            .issues
            .iter()
            .filter(|i| desk.has_local(&i.key) && desk.identities.fingerprint(&i.key).is_none())
            .map(|i| i.key.clone())
            .collect();
        for k in keys {
            desk.remember(&k)?;
        }
        Ok(desk)
    }

    pub fn is_demo(&self) -> bool {
        matches!(self.source, Source::Demo(_))
    }

    pub fn cache_path(&self) -> PathBuf {
        self.dir.join("cache.sqlite")
    }

    /// A client for the source. (Reads the token, so it may run `token_command`.)
    pub fn jira(&self) -> Result<Arc<dyn Jira>> {
        Ok(match &self.source {
            Source::Jira(c) => Arc::new(client::HttpJira::new(c)?),
            Source::Demo(d) => d.clone(),
        })
    }

    pub fn refresh_minutes(&self) -> u64 {
        match &self.source {
            Source::Jira(c) => c.refresh_minutes,
            Source::Demo(_) => 5,
        }
    }

    /// The cap on downloaded images, in bytes (`max_image_mb`; demo mode:
    /// `JDESK_DEMO_IMAGE_MB`, else 500). `None`: no limit.
    pub fn image_cap(&self) -> Option<u64> {
        let mb = match &self.source {
            Source::Jira(c) => c.max_image_mb,
            Source::Demo(_) => std::env::var("JDESK_DEMO_IMAGE_MB")
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(500),
        };
        (mb > 0).then(|| mb * 1024 * 1024)
    }

    pub fn site(&self) -> String {
        match &self.source {
            Source::Jira(c) => c.site().to_string(),
            Source::Demo(_) => demo::SITE.to_string(),
        }
    }

    pub fn browse_url(&self, key: &str) -> String {
        format!("{}/browse/{key}", self.site())
    }

    /// What a background sync needs, cloned so it can run on another thread.
    pub fn sync_job(&self) -> Result<SyncJob> {
        let (jql, overrides) = match &self.source {
            Source::Jira(c) => (c.jql.clone(), c.fields.clone()),
            Source::Demo(_) => ("project = DEMO".to_string(), Default::default()),
        };
        Ok(SyncJob {
            jira: self.jira()?,
            cache: self.cache_path(),
            jql,
            overrides,
        })
    }

    fn set_issues(&mut self, mut issues: Vec<Issue>) {
        issues.sort_by(|a, b| {
            a.project
                .cmp(&b.project)
                .then(a.key_number().cmp(&b.key_number()))
        });
        self.index = issues
            .iter()
            .enumerate()
            .map(|(i, x)| (x.key.clone(), i))
            .collect();
        self.issues = issues;
    }

    /// Takes in a finished sync.
    pub fn merge(&mut self, out: SyncOutcome) {
        if out.myself.is_some() {
            self.myself = out.myself;
        }
        if let Some(m) = out.field_map {
            self.field_map = m;
        }
        if !out.people.is_empty() {
            self.people = out.people;
        }
        self.last_sync = Some(out.report.at);
        if !out.report.upserted.is_empty() || !out.report.removed.is_empty() || out.report.full {
            // Keys only, for the record; failing to note a sync loses nothing of yours.
            let _ = journal::Journal::in_data(&self.dir).record(journal::Event::Synced {
                full: out.report.full,
                changed: out.report.upserted.iter().map(|i| i.key.clone()).collect(),
                removed: out.report.removed.clone(),
            });
        }
        // Anything you attached things to that changed a lot opens a review.
        for i in &out.report.upserted {
            let _ = self.identities.check(i, now());
        }
        self.upsert(out.report.upserted, &out.report.removed);
        self.demo_repurpose();
    }

    /// Demo mode, for trying the review workflow: `JDESK_DEMO_REPURPOSE=DEMO-114` rewrites that
    /// issue in the fake Jira after the first sync, so the next refresh finds it repurposed.
    fn demo_repurpose(&self) {
        if let Source::Demo(d) = &self.source
            && let Ok(key) = std::env::var("JDESK_DEMO_REPURPOSE")
        {
            d.repurpose_once(key.trim());
        }
    }

    /// Whether you attached anything to `key`: a queued change, a note, a star or a tag.
    pub fn has_local(&self, key: &str) -> bool {
        let l = self.sidecar.local(key);
        l.starred
            || !l.tags.is_empty()
            || self.sidecar.has_note(key)
            || self.queue.for_issue(key).next().is_some()
    }

    /// Keeps a fingerprint of `key` as it is now (when you first attach something to it).
    pub fn remember(&mut self, key: &str) -> Result<()> {
        match self.issue(key).cloned() {
            Some(i) => self.identities.remember(&i, now()),
            None => Ok(()),
        }
    }

    pub fn set_note(&mut self, key: &str, text: &str) -> Result<()> {
        self.sidecar.set_note(key, text)?;
        self.remember(key)
    }

    pub fn toggle_star(&mut self, key: &str) -> Result<bool> {
        let on = self.sidecar.toggle_star(key)?;
        self.remember(key)?;
        Ok(on)
    }

    pub fn toggle_tag(&mut self, key: &str, tag: &str) -> Result<()> {
        self.sidecar.toggle_tag(key, tag)?;
        self.remember(key)
    }

    /// Every local tag in use.
    pub fn tags(&self) -> Vec<String> {
        self.sidecar.all_tags().into_iter().collect()
    }

    /// What apply must hold back (issues under review) and check (fingerprints).
    pub fn apply_guard(&self) -> identity::ApplyGuard {
        self.identities.guard()
    }

    /// Takes in what an apply found: issues changed materially open reviews.
    pub fn take_apply_report(&mut self, report: &queue::ApplyReport) -> Result<()> {
        for (issue, signals) in &report.raised {
            self.identities.raise(issue, signals.clone(), now())?;
        }
        // Issues your changes went to passed the check just before; what Jira has now (your
        // edits included) is the new baseline.
        for issue in &report.refreshed {
            if !report.held.contains(&issue.key) {
                self.identities.rebaseline(issue, now())?;
            }
        }
        Ok(())
    }

    /// Resolves a review as "the same work": the fingerprint follows the issue as it is now and
    /// the held changes can be applied.
    pub fn keep_after_review(&mut self, key: &str) -> Result<()> {
        let issue = self
            .issue(key)
            .cloned()
            .with_context(|| format!("{key} is not in the local copy"))?;
        self.identities
            .resolve(&issue, identity::ReviewStatus::Kept { at: now() }, now())
    }

    /// Resolves a review as "repurposed": your note, star, tags and the issue's open queued
    /// changes are copied to `archive/KEY-<time>/` and removed from the key. Returns the folder.
    pub fn detach_after_review(&mut self, key: &str) -> Result<PathBuf> {
        // The key names a folder: never let one reach outside `archive/`.
        if !model::is_issue_key(key) {
            anyhow::bail!("“{key}” is not an issue key");
        }
        let issue = self
            .issue(key)
            .cloned()
            .with_context(|| format!("{key} is not in the local copy"))?;
        let t = now();
        let folder = self.dir.join("archive").join(format!("{key}-{t}"));
        std::fs::create_dir_all(&folder)?;
        let note = self.sidecar.note(key);
        if !note.trim().is_empty() {
            write_atomic(&folder.join("note.md"), note.as_bytes())?;
        }
        let local = self.sidecar.local(key);
        let changes: Vec<queue::Change> = self.queue.for_issue(key).cloned().collect();
        let was = self.identities.fingerprint(key).cloned();
        let review = self.identities.open_review(key).cloned();
        write_atomic(
            &folder.join("detached.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "key": key,
                "detached_at": t,
                "was": was,
                "review": review,
                "starred": local.starred,
                "tags": local.tags,
                "queued_changes": changes,
            }))?
            .as_bytes(),
        )?;
        // Then take them off the key (each step journaled).
        if !note.trim().is_empty() {
            self.sidecar.set_note(key, "")?;
        }
        if local.starred {
            self.sidecar.set_star(key, false)?;
        }
        for tag in &local.tags {
            self.sidecar.toggle_tag(key, tag)?;
        }
        for c in &changes {
            self.queue.discard(c.id)?;
        }
        let rel = folder
            .strip_prefix(&self.dir)
            .unwrap_or(&folder)
            .display()
            .to_string();
        self.identities.resolve(
            &issue,
            identity::ReviewStatus::Detached {
                at: t,
                archive: rel,
            },
            t,
        )?;
        Ok(folder)
    }

    /// Replaces or adds issues (and drops `removed`), keeping the list in key order.
    pub fn upsert(&mut self, issues: Vec<Issue>, removed: &[String]) {
        if issues.is_empty() && removed.is_empty() {
            return;
        }
        let mut by_key: BTreeMap<String, Issue> = std::mem::take(&mut self.issues)
            .into_iter()
            .map(|i| (i.key.clone(), i))
            .collect();
        for k in removed {
            by_key.remove(k);
        }
        for i in issues {
            by_key.insert(i.key.clone(), i);
        }
        self.set_issues(by_key.into_values().collect());
    }

    /// Writes issues read back after applying changes to the cache too.
    pub fn store_refreshed(&mut self, issues: Vec<Issue>) -> Result<()> {
        if issues.is_empty() {
            return Ok(());
        }
        let mut store = store::Store::open(&self.cache_path())?;
        store.upsert(&issues)?;
        self.upsert(issues, &[]);
        Ok(())
    }

    /// Every cached issue as Jira last said it was, in key order.
    pub fn issues(&self) -> &[Issue] {
        &self.issues
    }

    pub fn issue(&self, key: &str) -> Option<&Issue> {
        self.index.get(key).map(|i| &self.issues[*i])
    }

    /// Every issue as the app shows it: with your queued changes applied.
    pub fn effective_issues(&self) -> Vec<Issue> {
        self.issues
            .iter()
            .map(|i| self.queue.effective(i))
            .collect()
    }

    pub fn flags(&self, key: &str) -> filter::Flags {
        filter::Flags {
            starred: self.sidecar.starred(key),
            noted: self.sidecar.has_note(key),
            pending: self.queue.for_issue(key).next().is_some(),
            tags: self.sidecar.local(key).tags,
            review: false,
        }
    }

    /// Sprints seen on any issue, newest first within each state: active, future, closed.
    pub fn sprints(&self) -> Vec<Sprint> {
        let mut by_id: BTreeMap<u64, Sprint> = BTreeMap::new();
        for i in &self.issues {
            for s in &i.sprints {
                by_id.entry(s.id).or_insert_with(|| s.clone());
            }
        }
        let mut out: Vec<Sprint> = by_id.into_values().collect();
        let order = |s: &SprintState| match s {
            SprintState::Active => 0,
            SprintState::Future => 1,
            SprintState::Closed => 2,
        };
        out.sort_by(|a, b| {
            order(&a.state)
                .cmp(&order(&b.state))
                .then_with(|| match a.state {
                    SprintState::Future => a.start.cmp(&b.start).then(a.id.cmp(&b.id)),
                    _ => b.start.cmp(&a.start).then(b.id.cmp(&a.id)),
                })
        });
        out
    }

    /// Statuses seen, with their category, for the transition picker.
    pub fn statuses(&self) -> Vec<(String, model::StatusCategory)> {
        let mut seen: BTreeMap<String, model::StatusCategory> = BTreeMap::new();
        for i in &self.issues {
            seen.entry(i.status.clone()).or_insert(i.status_category);
        }
        let mut out: Vec<_> = seen.into_iter().collect();
        out.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
        out
    }

    /// Labels in use, for the label picker.
    pub fn labels(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .issues
            .iter()
            .flat_map(|i| i.labels.iter().cloned())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    pub fn priorities(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .issues
            .iter()
            .filter_map(|i| i.priority.clone())
            .collect();
        out.sort_by_key(|p| (filter::priority_rank(Some(p)), p.clone()));
        out.dedup();
        out
    }

    /// People to offer when assigning: assignable users from Jira, plus anyone seen on an
    /// issue, by name.
    pub fn assignees(&self) -> Vec<Person> {
        let mut by_id: BTreeMap<String, Person> = BTreeMap::new();
        for p in &self.people {
            by_id.insert(p.id.clone(), p.clone());
        }
        for i in &self.issues {
            for p in i.assignee.iter().chain(i.reporter.iter()) {
                by_id.entry(p.id.clone()).or_insert_with(|| p.clone());
            }
        }
        let mut out: Vec<Person> = by_id.into_values().collect();
        out.sort_by_key(|a| a.name.to_lowercase());
        out
    }

    /// Queues a change to `key`. Returns whether the queue changed.
    pub fn queue_edit(&mut self, key: &str, edit: queue::Edit) -> Result<bool> {
        let Some(issue) = self.issue(key).cloned() else {
            anyhow::bail!("{key} is not in the local copy");
        };
        let changed = self.queue.add(&issue, edit, now())?;
        if changed {
            self.identities.remember(&issue, now())?;
        }
        Ok(changed)
    }
}

/// What [`recover`] found (and, with `write`, did).
#[derive(Debug, Default)]
pub struct Recovery {
    pub entries: usize,
    pub unreadable_lines: usize,
    /// One line per difference between the journal and the files, for people.
    pub differences: Vec<String>,
    /// Where the files were copied before being replaced (`write` only).
    pub backup: Option<PathBuf>,
}

/// Rebuilds the queue, notes and stars from the journal and compares them with the files in
/// `dir`. With `write`, copies the current files to `recovery-<time>/` and puts back what the
/// journal says. Changes found only in `queue.json` (not in the journal) are kept.
pub fn recover(dir: &Path, write: bool) -> Result<Recovery> {
    let j = journal::Journal::in_data(dir);
    let (entries, bad) = j.read()?;
    let rebuilt = journal::replay(&entries);
    let mut rec = Recovery {
        entries: entries.len(),
        unreadable_lines: bad,
        ..Default::default()
    };

    let queue_path = dir.join("queue.json");
    let current = match queue::Queue::open(&queue_path) {
        Ok(q) => q.changes().to_vec(),
        Err(e) => {
            rec.differences
                .push(format!("queue.json: unreadable ({e:#})"));
            Vec::new()
        }
    };
    let mut merged = rebuilt.changes.clone();
    for c in &current {
        match rebuilt.changes.iter().find(|r| r.id == c.id) {
            None => {
                rec.differences.push(format!(
                    "queue: #{} {} {} is in queue.json but not the journal (kept)",
                    c.id,
                    c.key,
                    c.edit.describe()
                ));
                merged.push(c.clone());
            }
            Some(r) if r != c => rec.differences.push(format!(
                "queue: #{} {} differs: journal has {} ({:?}), queue.json has {} ({:?})",
                c.id,
                c.key,
                r.edit.describe(),
                r.state,
                c.edit.describe(),
                c.state
            )),
            Some(_) => {}
        }
    }
    for r in &rebuilt.changes {
        if !current.iter().any(|c| c.id == r.id) {
            rec.differences.push(format!(
                "queue: #{} {} {} ({:?}) is missing from queue.json",
                r.id,
                r.key,
                r.edit.describe(),
                r.state
            ));
        }
    }
    merged.sort_by_key(|c| c.id);

    let mut side = sidecar::Sidecar::open(dir)?;
    let notes: Vec<(String, String)> = rebuilt
        .notes
        .iter()
        .filter(|(k, text)| side.note(k).trim_end() != text.trim_end())
        .map(|(k, t)| (k.clone(), t.clone()))
        .collect();
    for (k, t) in &notes {
        rec.differences.push(if t.trim().is_empty() {
            format!("note: {k} was removed, but the file is still there")
        } else if side.has_note(k) {
            format!("note: {k} differs from its last save in the app")
        } else {
            format!(
                "note: {k} is missing ({} lines in the journal)",
                t.lines().count()
            )
        });
    }
    let stars: Vec<(String, bool)> = rebuilt
        .stars
        .iter()
        .filter(|(k, on)| side.starred(k) != **on)
        .map(|(k, on)| (k.clone(), *on))
        .collect();
    for (k, on) in &stars {
        rec.differences.push(format!(
            "star: {k} should be {}",
            if *on { "starred" } else { "unstarred" }
        ));
    }

    if write && !rec.differences.is_empty() {
        let backup = dir.join(format!("recovery-{}", now()));
        std::fs::create_dir_all(backup.join("notes"))?;
        for name in ["queue.json", "local.json"] {
            if dir.join(name).exists() {
                std::fs::copy(dir.join(name), backup.join(name))?;
            }
        }
        for (k, _) in &notes {
            if let Some(p) = side.note_path(k)
                && p.exists()
            {
                std::fs::copy(&p, backup.join("notes").join(format!("{k}.md")))?;
            }
        }
        queue::Queue::from_changes(&queue_path, merged).save()?;
        for (k, t) in &notes {
            side.set_note(k, t)?;
        }
        for (k, on) in &stars {
            side.set_star(k, *on)?;
        }
        rec.backup = Some(backup);
    }
    Ok(rec)
}

/// A sync, ready to run on a background thread.
pub struct SyncJob {
    pub jira: Arc<dyn Jira>,
    pub cache: PathBuf,
    pub jql: String,
    pub overrides: config::FieldOverrides,
}

impl SyncJob {
    pub fn run(&self, mode: sync::Mode, progress: &dyn Fn(String)) -> Result<SyncOutcome> {
        let mut store = store::Store::open(&self.cache)?;
        let report = sync::sync(
            self.jira.as_ref(),
            &mut store,
            &self.jql,
            &self.overrides,
            mode,
            now(),
            progress,
        )?;
        // Assignable people, once per full sync, for each project in the cache.
        let mut people = Vec::new();
        if report.full {
            let mut projects: Vec<String> =
                store.load_all()?.into_iter().map(|i| i.project).collect();
            projects.sort();
            projects.dedup();
            for p in projects.iter().take(10) {
                if let Ok(list) = self.jira.assignable(p) {
                    for person in list {
                        if !people.contains(&person) {
                            people.push(person);
                        }
                    }
                }
            }
            if !people.is_empty() {
                store.set_meta("people", &people)?;
            }
        }
        Ok(SyncOutcome {
            report,
            myself: store.meta("myself")?,
            field_map: store.meta("field_map")?,
            people,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_desk_syncs_queues_and_applies() {
        let dir = tempfile::tempdir().unwrap();
        let mut desk =
            Desk::open_with(dir.path(), Source::Demo(Arc::new(demo::DemoJira::new(60)))).unwrap();
        assert!(desk.issues().is_empty());
        let job = desk.sync_job().unwrap();
        let out = job.run(sync::Mode::Auto, &|_| {}).unwrap();
        desk.merge(out);
        assert_eq!(desk.issues().len(), 60);
        assert_eq!(desk.sprints()[0].state, SprintState::Active);
        assert!(!desk.people.is_empty());
        assert_eq!(desk.myself.as_ref().unwrap().name, "Ana Ruiz");

        let key = desk
            .issues()
            .iter()
            .find(|i| i.status != "In Review" && i.kind != "Epic")
            .unwrap()
            .key
            .clone();
        assert!(
            desk.queue_edit(
                &key,
                queue::Edit::Transition {
                    to: "In Review".into()
                }
            )
            .unwrap()
        );
        assert!(desk.flags(&key).pending);
        let eff = desk.effective_issues();
        assert_eq!(
            eff.iter().find(|i| i.key == key).unwrap().status,
            "In Review"
        );

        let jira = desk.jira().unwrap();
        let map = desk.field_map.clone();
        let r = desk
            .queue
            .apply(
                jira.as_ref(),
                &map,
                None,
                now(),
                &Default::default(),
                &|_| {},
            )
            .unwrap();
        assert_eq!(r.applied, 1);
        desk.store_refreshed(r.refreshed).unwrap();
        assert_eq!(desk.issue(&key).unwrap().status, "In Review");
        assert!(!desk.flags(&key).pending);

        // Reopening reads the cache and queue from disk.
        let desk2 = Desk::open_with(dir.path(), desk.source.clone()).unwrap();
        assert_eq!(desk2.issues().len(), 60);
        assert_eq!(desk2.issue(&key).unwrap().status, "In Review");
        assert_eq!(desk2.queue.changes().len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn the_data_folder_is_yours_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir(&data).unwrap();
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o755)).unwrap();
        Desk::open_with(&data, Source::Demo(Arc::new(demo::DemoJira::new(5)))).unwrap();
        let mode = std::fs::metadata(&data).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{mode:o}");
    }

    #[test]
    fn keys_from_jira_never_name_a_folder_outside() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("a/b");
        let mut desk =
            Desk::open_with(&data, Source::Demo(Arc::new(demo::DemoJira::new(5)))).unwrap();
        let out = desk
            .sync_job()
            .unwrap()
            .run(sync::Mode::Auto, &|_| {})
            .unwrap();
        desk.merge(out);
        let mut evil = desk.issues()[0].clone();
        evil.key = "../../../escaped-1".into();
        desk.store_refreshed(vec![evil]).unwrap();
        assert!(desk.detach_after_review("../../../escaped-1").is_err());
        assert!(!dir.path().join("escaped-1").exists());
        assert!(
            std::fs::read_dir(dir.path()).unwrap().count() == 1,
            "only a/ is there"
        );
    }
}
