//! Which issues the list shows, in what order, and the counts beside each filter choice.
//!
//! Choices within one facet widen (Ana *or* Bo); facets narrow each other (Ana's *and*
//! labelled `auth`). Each facet's counts ignore that facet's own choices, so they say what
//! picking another value would show.

use crate::model::{Issue, SprintState, StatusCategory};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SprintSel {
    /// Issues in an active sprint.
    #[default]
    Active,
    /// Not in an active or future sprint.
    Backlog,
    Id(u64),
    All,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sort {
    /// Board order.
    #[default]
    Rank,
    Updated,
    Key,
    Priority,
    Status,
}

impl Sort {
    pub const ALL: [Sort; 5] = [
        Sort::Rank,
        Sort::Status,
        Sort::Priority,
        Sort::Updated,
        Sort::Key,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Sort::Rank => "Board order",
            Sort::Updated => "Recently updated",
            Sort::Key => "Key",
            Sort::Priority => "Priority",
            Sort::Status => "Status",
        }
    }
}

/// What the app knows about an issue besides Jira's fields.
#[derive(Clone, Debug, Default)]
pub struct Flags {
    pub starred: bool,
    pub noted: bool,
    pub pending: bool,
    /// Your own tags on the issue (local only).
    pub tags: Vec<String>,
    /// The issue changed a lot in Jira since you attached things to it (see
    /// [`crate::identity`]); its queued changes wait for your review.
    pub review: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Filter {
    pub sprint: SprintSel,
    /// Person ids; `""` is Unassigned.
    pub assignees: BTreeSet<String>,
    pub labels: BTreeSet<String>,
    /// Parent keys; `""` is No epic.
    pub epics: BTreeSet<String>,
    pub statuses: BTreeSet<String>,
    pub hide_done: bool,
    pub starred: bool,
    pub noted: bool,
    pub pending: bool,
    /// Your own tags (any of them).
    #[serde(default)]
    pub tags: BTreeSet<String>,
    /// Only issues waiting for a review (changed a lot in Jira).
    #[serde(default)]
    pub review: bool,
    pub text: String,
    pub sort: Sort,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dim {
    Sprint,
    Assignee,
    Label,
    Epic,
    Status,
    Tag,
}

impl Filter {
    /// Whether any narrowing beyond the sprint is on.
    pub fn is_narrowed(&self) -> bool {
        !self.assignees.is_empty()
            || !self.labels.is_empty()
            || !self.epics.is_empty()
            || !self.statuses.is_empty()
            || self.hide_done
            || self.starred
            || self.noted
            || self.pending
            || !self.tags.is_empty()
            || self.review
            || !self.text.trim().is_empty()
    }

    /// Clears everything but the sprint and sort.
    pub fn clear(&mut self) {
        *self = Filter {
            sprint: self.sprint.clone(),
            sort: self.sort,
            ..Default::default()
        };
    }

    fn sprint_ok(&self, i: &Issue) -> bool {
        match &self.sprint {
            SprintSel::All => true,
            SprintSel::Active => i.sprints.iter().any(|s| s.state == SprintState::Active),
            SprintSel::Backlog => !i.sprints.iter().any(|s| s.state != SprintState::Closed),
            SprintSel::Id(id) => i.sprints.iter().any(|s| s.id == *id),
        }
    }

    fn passes(&self, i: &Issue, f: &Flags, skip: Option<Dim>) -> bool {
        self.passes_but_text(i, f, skip) && text_matches(i, &self.text)
    }

    /// Every test but the text search (the costly one, which callers can do once).
    fn passes_but_text(&self, i: &Issue, f: &Flags, skip: Option<Dim>) -> bool {
        let want = |d: Dim| skip != Some(d);
        if want(Dim::Sprint) && !self.sprint_ok(i) {
            return false;
        }
        if want(Dim::Assignee)
            && !self.assignees.is_empty()
            && !self
                .assignees
                .contains(i.assignee.as_ref().map_or("", |p| p.id.as_str()))
        {
            return false;
        }
        if want(Dim::Label)
            && !self.labels.is_empty()
            && !i.labels.iter().any(|l| self.labels.contains(l))
        {
            return false;
        }
        if want(Dim::Epic)
            && !self.epics.is_empty()
            && !self
                .epics
                .contains(i.parent.as_ref().map_or("", |p| p.key.as_str()))
        {
            return false;
        }
        if want(Dim::Status) && !self.statuses.is_empty() && !self.statuses.contains(&i.status) {
            return false;
        }
        if self.hide_done && i.status_category == StatusCategory::Done {
            return false;
        }
        if want(Dim::Tag) && !self.tags.is_empty() && !f.tags.iter().any(|t| self.tags.contains(t))
        {
            return false;
        }
        !((self.starred && !f.starred)
            || (self.noted && !f.noted)
            || (self.pending && !f.pending)
            || (self.review && !f.review))
    }

    pub fn matches(&self, i: &Issue, f: &Flags) -> bool {
        self.passes(i, f, None)
    }

    /// The issues to show, sorted. `issues` and `flags` line up.
    pub fn apply<'a>(&self, issues: &'a [Issue], flags: &[Flags]) -> Vec<&'a Issue> {
        let mut out: Vec<&Issue> = issues
            .iter()
            .zip(flags)
            .filter(|(i, f)| self.matches(i, f))
            .map(|(i, _)| i)
            .collect();
        sort(&mut out, self.sort);
        out
    }

    /// Counts for the filter pane.
    pub fn facets(&self, issues: &[Issue], flags: &[Flags]) -> Facets {
        let mut fx = Facets::default();
        for (i, f) in issues.iter().zip(flags) {
            if !text_matches(i, &self.text) {
                continue;
            }
            if self.passes_but_text(i, f, Some(Dim::Sprint)) {
                let pts = i.story_points.unwrap_or(0.);
                for s in &i.sprints {
                    let e = fx.sprints.entry(s.id).or_insert_with(|| (s.clone(), 0));
                    e.1 += 1;
                    *fx.sprint_points.entry(s.id).or_default() += pts;
                }
                if !i.sprints.iter().any(|s| s.state != SprintState::Closed) {
                    fx.backlog += 1;
                    fx.backlog_points += pts;
                }
                if i.sprints.iter().any(|s| s.state == SprintState::Active) {
                    fx.active += 1;
                    fx.active_points += pts;
                }
                fx.all += 1;
                fx.all_points += pts;
            }
            if self.passes_but_text(i, f, Some(Dim::Assignee)) {
                let (id, name) = i
                    .assignee
                    .as_ref()
                    .map_or((String::new(), "Unassigned".to_string()), |p| {
                        (p.id.clone(), p.name.clone())
                    });
                let e = fx.assignees.entry(id).or_insert((name, 0));
                e.1 += 1;
            }
            if self.passes_but_text(i, f, Some(Dim::Label)) {
                for l in &i.labels {
                    *fx.labels.entry(l.clone()).or_default() += 1;
                }
            }
            if self.passes_but_text(i, f, Some(Dim::Epic)) {
                let (k, s) = i
                    .parent
                    .as_ref()
                    .map_or((String::new(), "No epic".to_string()), |p| {
                        (p.key.clone(), p.summary.clone())
                    });
                let e = fx.epics.entry(k).or_insert((s, 0));
                e.1 += 1;
            }
            if self.passes_but_text(i, f, Some(Dim::Tag)) {
                for t in &f.tags {
                    *fx.tags.entry(t.clone()).or_default() += 1;
                }
            }
            if self.passes_but_text(i, f, Some(Dim::Status)) {
                let e = fx
                    .statuses
                    .entry(i.status.clone())
                    .or_insert((i.status_category, 0));
                e.1 += 1;
            }
        }
        // Chosen values stay listed (at 0) so they can be unchosen.
        for a in &self.assignees {
            fx.assignees.entry(a.clone()).or_insert_with(|| {
                (
                    if a.is_empty() {
                        "Unassigned".into()
                    } else {
                        a.clone()
                    },
                    0,
                )
            });
        }
        for l in &self.labels {
            fx.labels.entry(l.clone()).or_default();
        }
        for e in &self.epics {
            fx.epics
                .entry(e.clone())
                .or_insert_with(|| (String::new(), 0));
        }
        for t in &self.tags {
            fx.tags.entry(t.clone()).or_default();
        }
        for s in &self.statuses {
            fx.statuses
                .entry(s.clone())
                .or_insert((StatusCategory::ToDo, 0));
        }
        fx
    }
}

#[derive(Debug, Default)]
pub struct Facets {
    pub active: usize,
    pub backlog: usize,
    pub all: usize,
    pub sprints: BTreeMap<u64, (crate::model::Sprint, usize)>,
    /// Story points, over the same issues as the counts above (unpointed issues add 0).
    pub active_points: f64,
    pub backlog_points: f64,
    pub all_points: f64,
    pub sprint_points: BTreeMap<u64, f64>,
    /// id → (name, count)
    pub assignees: BTreeMap<String, (String, usize)>,
    pub labels: BTreeMap<String, usize>,
    /// key → (summary, count)
    pub epics: BTreeMap<String, (String, usize)>,
    pub statuses: BTreeMap<String, (StatusCategory, usize)>,
    /// Your own tags, with counts.
    pub tags: BTreeMap<String, usize>,
}

/// Every word of `query` appears in the key, summary, labels, epic, assignee or status.
pub fn text_matches(i: &Issue, query: &str) -> bool {
    let q = query.trim();
    if q.is_empty() {
        return true;
    }
    let mut hay = String::with_capacity(128);
    hay.push_str(&i.key);
    hay.push(' ');
    hay.push_str(&i.summary);
    hay.push(' ');
    hay.push_str(&i.status);
    for l in &i.labels {
        hay.push(' ');
        hay.push_str(l);
    }
    if let Some(p) = &i.parent {
        hay.push(' ');
        hay.push_str(&p.key);
        hay.push(' ');
        hay.push_str(&p.summary);
    }
    if let Some(a) = &i.assignee {
        hay.push(' ');
        hay.push_str(&a.name);
    }
    let hay = hay.to_lowercase();
    q.to_lowercase().split_whitespace().all(|w| hay.contains(w))
}

pub fn priority_rank(p: Option<&str>) -> u8 {
    match p.map(str::to_ascii_lowercase).as_deref() {
        Some("highest" | "blocker") => 0,
        Some("high" | "critical") => 1,
        Some("medium" | "major") => 2,
        Some("low" | "minor") => 3,
        Some("lowest" | "trivial") => 4,
        _ => 2,
    }
}

pub fn sort(list: &mut [&Issue], by: Sort) {
    let rank = |i: &Issue| i.rank.clone().unwrap_or_else(|| "~".into());
    match by {
        Sort::Rank => list.sort_by(|a, b| {
            rank(a)
                .cmp(&rank(b))
                .then(a.project.cmp(&b.project))
                .then(a.key_number().cmp(&b.key_number()))
        }),
        Sort::Updated => list.sort_by(|a, b| b.updated.cmp(&a.updated)),
        Sort::Key => list.sort_by(|a, b| {
            a.project
                .cmp(&b.project)
                .then(a.key_number().cmp(&b.key_number()))
        }),
        Sort::Priority => list.sort_by(|a, b| {
            priority_rank(a.priority.as_deref())
                .cmp(&priority_rank(b.priority.as_deref()))
                .then(rank(a).cmp(&rank(b)))
        }),
        Sort::Status => list.sort_by(|a, b| {
            a.status_category
                .cmp(&b.status_category)
                .then(a.status.cmp(&b.status))
                .then(rank(a).cmp(&rank(b)))
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo;

    #[test]
    fn facets_widen_within_and_narrow_across() {
        let issues = demo::issues(200);
        let flags = vec![Flags::default(); issues.len()];
        let mut f = Filter {
            sprint: SprintSel::All,
            ..Default::default()
        };
        let all = f.apply(&issues, &flags).len();
        assert_eq!(all, 200);
        let fx = f.facets(&issues, &flags);
        let (who, (_, n)) = fx.assignees.iter().find(|(id, _)| !id.is_empty()).unwrap();
        f.assignees.insert(who.clone());
        assert_eq!(f.apply(&issues, &flags).len(), *n);
        // The assignee facet still counts the others (so you can widen).
        let fx2 = f.facets(&issues, &flags);
        assert_eq!(fx2.assignees.len(), fx.assignees.len());
        // Text narrows further.
        f.assignees.clear();
        f.text = "checkout".into();
        let hits = f.apply(&issues, &flags);
        assert!(!hits.is_empty() && hits.len() < all);
        assert!(hits.iter().all(|i| text_matches(i, "CHECKOUT")));
        f.clear();
        assert_eq!(f.apply(&issues, &flags).len(), all);
        f.sprint = SprintSel::Active;
        assert_eq!(f.apply(&issues, &flags).len(), fx.active);
    }
}

#[cfg(test)]
mod perf {
    use super::*;

    /// Not a benchmark: a guard that filtering 10,000 issues stays well inside a frame.
    #[test]
    fn ten_thousand_issues_filter_quickly() {
        let issues = crate::demo::issues(10_000);
        let flags = vec![Flags::default(); issues.len()];
        let mut f = Filter {
            sprint: SprintSel::All,
            ..Default::default()
        };
        f.text = "cache ios".into();
        let t = std::time::Instant::now();
        let shown = f.apply(&issues, &flags).len();
        let fx = f.facets(&issues, &flags);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        eprintln!(
            "10k issues: filter + facets in {ms:.1} ms ({shown} shown, {} people)",
            fx.assignees.len()
        );
        assert!(shown > 0);
        assert!(
            ms < if cfg!(debug_assertions) {
                2_000.0
            } else {
                100.0
            }
        );
    }
}
