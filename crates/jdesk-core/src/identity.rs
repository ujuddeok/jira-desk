//! Is this still the issue you attached things to?
//!
//! Jira keys get reused: an old ticket is rewritten for new work, or deleted and its key
//! recreated. Your queued changes, note, star and tags are filed under the key, so they would
//! silently move to the new work. To catch that, the first time you attach anything to an
//! issue its **fingerprint** is kept (`identity.json`): Jira's internal id, when it was
//! created, its type, project, summary and description. Every sync, and again just before your
//! changes are applied, the issue is compared with it.
//!
//! A **material change** opens a **review** (`reviews.json`) and holds the issue's queued
//! changes until you decide:
//!
//! - **Keep:** it's the same work; the fingerprint is updated and the changes can be applied.
//! - **Detach:** it's been repurposed; your note, star and tags and the queued changes are
//!   moved to `archive/KEY-<time>/`, and the key starts clean.
//!
//! Material means any of: a different internal id (another issue under the same key); a
//! different creation time (deleted and recreated); a different type or project; a rewritten
//! summary (few words in common), or a changed summary together with a rewritten description.
//! Ordinary edits (a status, an assignee, a reworded summary, a description filled in) are not.

use crate::issue_text;
use crate::journal::{Event, Journal};
use crate::model::Issue;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The description text kept in a fingerprint (enough to compare and to show).
const DESCRIPTION_KEPT: usize = 4000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    pub id: String,
    pub created: String,
    pub kind: String,
    pub project: String,
    pub summary: String,
    pub description: String,
    /// Unix seconds when it was taken.
    pub taken_at: i64,
}

impl Fingerprint {
    pub fn of(issue: &Issue, now: i64) -> Self {
        let mut description = issue_text(issue);
        if description.len() > DESCRIPTION_KEPT {
            let mut cut = DESCRIPTION_KEPT;
            while !description.is_char_boundary(cut) {
                cut -= 1;
            }
            description.truncate(cut);
        }
        Self {
            id: issue.id.clone(),
            created: issue.created.clone(),
            kind: issue.kind.clone(),
            project: issue.project.clone(),
            summary: issue.summary.clone(),
            description,
            taken_at: now,
        }
    }
}

/// One way an issue differs from its fingerprint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "signal", rename_all = "snake_case")]
pub enum Signal {
    /// Jira's internal id changed: another issue now has this key.
    DifferentIssue {
        was: String,
        now: String,
    },
    /// The creation time changed: deleted and created again.
    Recreated {
        was: String,
        now: String,
    },
    TypeChanged {
        was: String,
        now: String,
    },
    ProjectChanged {
        was: String,
        now: String,
    },
    /// Few words in common with the summary you knew.
    SummaryRewritten {
        was: String,
        now: String,
    },
    /// Few words in common with the description you knew (supporting evidence only).
    DescriptionRewritten,
}

impl Signal {
    pub fn describe(&self) -> String {
        match self {
            Signal::DifferentIssue { .. } => "a different Jira issue now has this key".into(),
            Signal::Recreated { was, now } => format!(
                "created again ({} → {})",
                was.get(..10).unwrap_or(was),
                now.get(..10).unwrap_or(now)
            ),
            Signal::TypeChanged { was, now } => format!("type {was} → {now}"),
            Signal::ProjectChanged { was, now } => format!("project {was} → {now}"),
            Signal::SummaryRewritten { was, now } => {
                format!("summary rewritten: “{was}” → “{now}”")
            }
            Signal::DescriptionRewritten => "description rewritten".into(),
        }
    }
}

fn words(s: &str) -> BTreeSet<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 1)
        .map(str::to_lowercase)
        .collect()
}

/// The Jaccard index of two texts' word sets.
pub fn jaccard(a: &str, b: &str) -> f64 {
    let (a, b) = (words(a), words(b));
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let common = a.intersection(&b).count() as f64;
    let union = a.union(&b).count() as f64;
    common / union
}

/// How `now` differs from the fingerprint `was`, and whether that is material.
pub fn compare(was: &Fingerprint, now: &Issue) -> (Vec<Signal>, bool) {
    let mut s = Vec::new();
    let mut material = false;
    if !was.id.is_empty() && !now.id.is_empty() && was.id != now.id {
        s.push(Signal::DifferentIssue {
            was: was.id.clone(),
            now: now.id.clone(),
        });
        material = true;
    } else if !was.created.is_empty() && !now.created.is_empty() && was.created != now.created {
        s.push(Signal::Recreated {
            was: was.created.clone(),
            now: now.created.clone(),
        });
        material = true;
    }
    if !was.kind.is_empty() && was.kind != now.kind {
        s.push(Signal::TypeChanged {
            was: was.kind.clone(),
            now: now.kind.clone(),
        });
        material = true;
    }
    if !was.project.is_empty() && was.project != now.project {
        s.push(Signal::ProjectChanged {
            was: was.project.clone(),
            now: now.project.clone(),
        });
        material = true;
    }
    let desc_now = issue_text(now);
    let desc_rewritten = words(&was.description).len() >= 8
        && words(&desc_now).len() >= 8
        && jaccard(&was.description, &desc_now) < 0.25;
    if was.summary != now.summary {
        let sim = jaccard(&was.summary, &now.summary);
        if sim < 0.34 || (sim < 0.6 && desc_rewritten) {
            s.push(Signal::SummaryRewritten {
                was: was.summary.clone(),
                now: now.summary.clone(),
            });
            material = true;
        }
    }
    if desc_rewritten {
        s.push(Signal::DescriptionRewritten);
    }
    (s, material)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ReviewStatus {
    Open,
    /// Same work: the changes went back in play.
    Kept {
        at: i64,
    },
    /// Repurposed: your things were moved to `archive`.
    Detached {
        at: i64,
        archive: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    pub key: String,
    pub raised_at: i64,
    pub signals: Vec<Signal>,
    /// The issue as you knew it.
    pub was: Fingerprint,
    /// The issue as Jira has it now.
    pub now: Fingerprint,
    pub status: ReviewStatus,
}

impl Review {
    pub fn is_open(&self) -> bool {
        self.status == ReviewStatus::Open
    }

    pub fn summary(&self) -> String {
        self.signals
            .iter()
            .map(Signal::describe)
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// What applying must not send: issues under review, and the fingerprints to check the
/// fresh copy against.
#[derive(Clone, Debug, Default)]
pub struct ApplyGuard {
    pub held: BTreeSet<String>,
    pub fingerprints: BTreeMap<String, Fingerprint>,
}

impl ApplyGuard {
    /// `Some(signals)` when the fresh copy of `issue` has changed materially.
    pub fn check(&self, issue: &Issue) -> Option<Vec<Signal>> {
        let fp = self.fingerprints.get(&issue.key)?;
        let (signals, material) = compare(fp, issue);
        material.then_some(signals)
    }
}

/// `identity.json` and `reviews.json` in the data folder.
pub struct Identities {
    dir: PathBuf,
    prints: BTreeMap<String, Fingerprint>,
    reviews: Vec<Review>,
    journal: Journal,
}

fn read<T: serde::de::DeserializeOwned + Default>(path: &Path) -> Result<T> {
    match std::fs::read_to_string(path) {
        Ok(t) => serde_json::from_str(&t).with_context(|| format!("{} is damaged", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

impl Identities {
    pub fn open(dir: &Path) -> Result<Self> {
        Ok(Self {
            prints: read(&dir.join("identity.json"))?,
            reviews: read(&dir.join("reviews.json"))?,
            journal: Journal::in_data(dir),
            dir: dir.to_path_buf(),
        })
    }

    fn save(&self) -> Result<()> {
        crate::write_atomic(
            &self.dir.join("identity.json"),
            serde_json::to_string_pretty(&self.prints)?.as_bytes(),
        )?;
        crate::write_atomic(
            &self.dir.join("reviews.json"),
            serde_json::to_string_pretty(&self.reviews)?.as_bytes(),
        )
    }

    pub fn fingerprint(&self, key: &str) -> Option<&Fingerprint> {
        self.prints.get(key)
    }

    /// Keeps a fingerprint of `issue` if there isn't one (you just attached something to it).
    pub fn remember(&mut self, issue: &Issue, now: i64) -> Result<()> {
        if self.prints.contains_key(&issue.key) {
            return Ok(());
        }
        self.prints
            .insert(issue.key.clone(), Fingerprint::of(issue, now));
        self.save()
    }

    pub fn open_review(&self, key: &str) -> Option<&Review> {
        self.reviews
            .iter()
            .rev()
            .find(|r| r.key == key && r.is_open())
    }

    pub fn open_reviews(&self) -> impl Iterator<Item = &Review> {
        self.reviews.iter().filter(|r| r.is_open())
    }

    pub fn reviews(&self) -> &[Review] {
        &self.reviews
    }

    /// Compares `issue` with its fingerprint; opens a review when it changed materially.
    /// Returns whether a review was opened.
    pub fn check(&mut self, issue: &Issue, now: i64) -> Result<bool> {
        let Some(fp) = self.prints.get(&issue.key) else {
            return Ok(false);
        };
        if self.open_review(&issue.key).is_some() {
            return Ok(false);
        }
        let (signals, material) = compare(fp, issue);
        if !material {
            return Ok(false);
        }
        self.raise(issue, signals, now)?;
        Ok(true)
    }

    /// Opens a review (also from apply, which found the change on the fresh copy).
    pub fn raise(&mut self, issue: &Issue, signals: Vec<Signal>, now: i64) -> Result<()> {
        if self.open_review(&issue.key).is_some() {
            return Ok(());
        }
        let was = self
            .prints
            .get(&issue.key)
            .cloned()
            .unwrap_or_else(|| Fingerprint::of(issue, now));
        self.journal.record(Event::ReviewRaised {
            key: issue.key.clone(),
            signals: signals.clone(),
        })?;
        self.reviews.push(Review {
            key: issue.key.clone(),
            raised_at: now,
            signals,
            was,
            now: Fingerprint::of(issue, now),
            status: ReviewStatus::Open,
        });
        self.save()
    }

    /// Closes the open review on `key` and takes a new fingerprint from `issue`.
    pub fn resolve(&mut self, issue: &Issue, status: ReviewStatus, now: i64) -> Result<()> {
        self.journal.record(Event::ReviewResolved {
            key: issue.key.clone(),
            outcome: status.clone(),
        })?;
        for r in self
            .reviews
            .iter_mut()
            .filter(|r| r.key == issue.key && r.is_open())
        {
            r.status = status.clone();
        }
        self.prints
            .insert(issue.key.clone(), Fingerprint::of(issue, now));
        self.save()
    }

    /// Takes a new fingerprint from `issue` if it has one (your own changes were just applied
    /// to it, after the pre-apply check passed), so your edits never read as someone else's.
    pub fn rebaseline(&mut self, issue: &Issue, now: i64) -> Result<()> {
        if !self.prints.contains_key(&issue.key) || self.open_review(&issue.key).is_some() {
            return Ok(());
        }
        self.prints
            .insert(issue.key.clone(), Fingerprint::of(issue, now));
        self.save()
    }

    /// Forgets the fingerprint of a key with nothing attached any more.
    pub fn forget(&mut self, key: &str) -> Result<()> {
        if self.prints.remove(key).is_some() {
            self.save()?;
        }
        Ok(())
    }

    /// What apply must hold back and check.
    pub fn guard(&self) -> ApplyGuard {
        ApplyGuard {
            held: self.open_reviews().map(|r| r.key.clone()).collect(),
            fingerprints: self.prints.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(id: &str, kind: &str, summary: &str, desc: &str) -> Issue {
        let mut i = crate::demo::issues(10).remove(7);
        i.id = id.into();
        i.kind = kind.into();
        i.summary = summary.into();
        i.description = crate::adf::from_plain(desc);
        i
    }

    #[test]
    fn ordinary_edits_are_not_material_but_repurposing_is() {
        let long = "Customers in the EU see the cart total without VAT when they switch currency during checkout";
        let base = issue("100", "Story", "Fix the cart total for EU customers", long);
        let fp = Fingerprint::of(&base, 0);

        let mut i = base.clone();
        i.status = "Done".into();
        i.summary = "Fix the cart total for EU customers (VAT)".into();
        assert!(!compare(&fp, &i).1, "reworded summary");

        let i = issue("100", "Story", "Fix the cart total for EU customers", "");
        assert!(!compare(&fp, &i).1, "description cleared");

        let i = issue(
            "100",
            "Story",
            "Migrate billing exports to the warehouse",
            long,
        );
        let (s, m) = compare(&fp, &i);
        assert!(
            m && matches!(s[0], Signal::SummaryRewritten { .. }),
            "{s:?}"
        );

        let i = issue("100", "Epic", "Fix the cart total for EU customers", long);
        assert!(compare(&fp, &i).1, "type changed");

        let i = issue("999", "Story", "Fix the cart total for EU customers", long);
        assert!(matches!(
            compare(&fp, &i).0[0],
            Signal::DifferentIssue { .. }
        ));

        // A modest summary change plus a rewritten description is material.
        let i = issue(
            "100",
            "Story",
            "Fix the cart page for EU partners",
            "Partners need a bulk export of invoices as CSV, scheduled nightly, with a retry when the warehouse is down",
        );
        let (s, m) = compare(&fp, &i);
        assert!(m, "{s:?}");
        assert!(s.contains(&Signal::DescriptionRewritten));
    }

    #[test]
    fn reviews_open_once_and_resolve() {
        let dir = tempfile::tempdir().unwrap();
        let mut ids = Identities::open(dir.path()).unwrap();
        let base = issue("100", "Story", "Fix the cart total for EU customers", "x");
        ids.remember(&base, 1).unwrap();
        assert!(!ids.check(&base, 2).unwrap());
        let moved = issue("200", "Story", "Something else entirely", "x");
        assert!(ids.check(&moved, 3).unwrap());
        assert!(!ids.check(&moved, 4).unwrap(), "one open review per key");
        assert!(ids.guard().held.contains(&base.key));
        ids.resolve(&moved, ReviewStatus::Kept { at: 5 }, 5)
            .unwrap();
        assert!(ids.open_review(&base.key).is_none());
        assert!(
            !ids.check(&moved, 6).unwrap(),
            "the new fingerprint is the baseline"
        );
        let reopened = Identities::open(dir.path()).unwrap();
        assert_eq!(reopened.reviews().len(), 1);
        assert_eq!(reopened.fingerprint(&base.key).unwrap().id, "200");
    }
}

#[cfg(test)]
mod workflow {
    use crate::queue::{Edit, State};
    use crate::{Desk, Source, demo::DemoJira, sync};
    use std::sync::Arc;

    fn setup() -> (tempfile::TempDir, Arc<DemoJira>, Desk, String) {
        let dir = tempfile::tempdir().unwrap();
        let jira = Arc::new(DemoJira::new(30));
        let mut desk = Desk::open_with(dir.path(), Source::Demo(jira.clone())).unwrap();
        let out = desk
            .sync_job()
            .unwrap()
            .run(sync::Mode::Full, &|_| {})
            .unwrap();
        desk.merge(out);
        let key = desk
            .issues()
            .iter()
            .find(|i| i.status != "Done" && i.kind != "Epic")
            .unwrap()
            .key
            .clone();
        (dir, jira, desk, key)
    }

    fn apply(desk: &mut Desk) -> crate::queue::ApplyReport {
        let jira = desk.jira().unwrap();
        let map = desk.field_map.clone();
        let guard = desk.apply_guard();
        let r = desk
            .queue
            .apply(jira.as_ref(), &map, None, 5_000, &guard, &|_| {})
            .unwrap();
        desk.take_apply_report(&r).unwrap();
        r
    }

    #[test]
    fn a_repurposed_issue_holds_its_changes_until_reviewed_then_detaches() {
        let (dir, jira, mut desk, key) = setup();
        desk.queue_edit(&key, Edit::Transition { to: "Done".into() })
            .unwrap();
        desk.set_note(&key, "ask Bo").unwrap();
        desk.toggle_star(&key).unwrap();
        desk.toggle_tag(&key, "follow-up").unwrap();
        assert!(desk.identities.fingerprint(&key).is_some());

        // An ordinary edit upstream is not a review.
        jira.touch(
            &key,
            &format!("{} (reworded)", desk.issue(&key).unwrap().summary),
            1_500,
        );
        let out = desk
            .sync_job()
            .unwrap()
            .run(sync::Mode::Full, &|_| {})
            .unwrap();
        desk.merge(out);
        assert!(desk.identities.open_review(&key).is_none());

        // Repurposed upstream: the next sync opens a review.
        jira.repurpose(&key);
        let out = desk
            .sync_job()
            .unwrap()
            .run(sync::Mode::Full, &|_| {})
            .unwrap();
        desk.merge(out);
        let review = desk
            .identities
            .open_review(&key)
            .expect("review opened")
            .clone();
        assert!(
            review.summary().contains("different Jira issue"),
            "{}",
            review.summary()
        );
        assert_eq!(
            review.now.summary,
            "Migrate billing exports to the new warehouse"
        );

        // Applying sends nothing for it.
        let r = apply(&mut desk);
        assert_eq!((r.applied, r.held.clone()), (0, vec![key.clone()]));
        assert_eq!(
            desk.queue.for_issue(&key).next().unwrap().state,
            State::Pending
        );
        assert_ne!(desk.issue(&key).unwrap().status, "Done");

        // Detach: everything of yours moves to the archive; the key starts clean.
        let folder = desk.detach_after_review(&key).unwrap();
        assert_eq!(
            std::fs::read_to_string(folder.join("note.md")).unwrap(),
            "ask Bo"
        );
        let meta = std::fs::read_to_string(folder.join("detached.json")).unwrap();
        assert!(
            meta.contains("follow-up")
                && meta.contains("\"starred\": true")
                && meta.contains("transition")
        );
        assert!(!desk.has_local(&key));
        assert!(desk.identities.open_review(&key).is_none());
        let (entries, _) = crate::journal::Journal::in_data(dir.path()).read().unwrap();
        let kinds: Vec<String> = entries.iter().map(crate::journal::describe).collect();
        assert!(kinds.iter().any(|l| l.contains("review   ")), "{kinds:#?}");
        assert!(
            kinds
                .iter()
                .any(|l| l.contains("repurposed, your things moved to archive/"))
        );
    }

    #[test]
    fn a_change_found_at_apply_time_is_held_and_keep_releases_it() {
        let (_dir, jira, mut desk, key) = setup();
        desk.queue_edit(&key, Edit::Transition { to: "Done".into() })
            .unwrap();
        // Repurposed after the last sync, before applying.
        jira.repurpose(&key);
        let r = apply(&mut desk);
        assert_eq!(r.applied, 0);
        assert_eq!(r.raised.len(), 1);
        assert!(desk.identities.open_review(&key).is_some());
        // The cache still has the old copy; a sync brings the new one in, and Keep releases.
        let out = desk
            .sync_job()
            .unwrap()
            .run(sync::Mode::Full, &|_| {})
            .unwrap();
        desk.merge(out);
        desk.keep_after_review(&key).unwrap();
        let r = apply(&mut desk);
        assert_eq!(r.applied, 1, "{:?}", desk.queue.changes());
    }

    #[test]
    fn your_own_applied_rename_does_not_open_a_review() {
        let (_dir, _jira, mut desk, key) = setup();
        desk.queue_edit(
            &key,
            Edit::Summary {
                to: "Completely different words entirely now".into(),
            },
        )
        .unwrap();
        let r = apply(&mut desk);
        assert_eq!(r.applied, 1);
        desk.store_refreshed(r.refreshed).unwrap();
        assert_eq!(
            desk.identities.fingerprint(&key).unwrap().summary,
            "Completely different words entirely now"
        );
        let out = desk
            .sync_job()
            .unwrap()
            .run(sync::Mode::Full, &|_| {})
            .unwrap();
        desk.merge(out);
        assert!(
            desk.identities.open_review(&key).is_none(),
            "{:?}",
            desk.identities.open_review(&key)
        );
    }
}
