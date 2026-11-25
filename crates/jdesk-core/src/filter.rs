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
    /// Issues in any of these sprints (two or more: one is `Id`).
    Ids(BTreeSet<u64>),
    All,
}

impl SprintSel {
    /// Whether this sprint is one of those chosen.
    pub fn has(&self, id: u64) -> bool {
        match self {
            SprintSel::Id(x) => *x == id,
            SprintSel::Ids(s) => s.contains(&id),
            _ => false,
        }
    }

    /// The sprints chosen by id (none for Active, Backlog and All).
    pub fn ids(&self) -> Vec<u64> {
        match self {
            SprintSel::Id(x) => vec![*x],
            SprintSel::Ids(s) => s.iter().copied().collect(),
            _ => Vec::new(),
        }
    }

    /// `id` added to the sprints chosen, or taken out if it was one. Active, Backlog and All
    /// are choices of their own: adding a sprint to one of them starts afresh with that sprint.
    /// Taking out the last leaves the active sprint.
    pub fn toggled(&self, id: u64) -> SprintSel {
        let mut set: BTreeSet<u64> = self.ids().into_iter().collect();
        if !set.remove(&id) {
            set.insert(id);
        }
        SprintSel::from_ids(set)
    }

    /// One sprint as `Id`, several as `Ids`, none as `Active`.
    pub fn from_ids(set: BTreeSet<u64>) -> SprintSel {
        match set.len() {
            0 => SprintSel::Active,
            1 => SprintSel::Id(*set.iter().next().expect("one")),
            _ => SprintSel::Ids(set),
        }
    }
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
    /// Open requests (made outside Jira Desk) on it.
    pub requests: bool,
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
    /// The status chips chosen, by name (see [`crate::config::StatusGroup`]); [`OTHER`] is the
    /// statuses no group takes.
    #[serde(default)]
    pub groups: BTreeSet<String>,
    /// The chips' definitions (from config.toml; not saved with the filter).
    #[serde(skip)]
    pub group_defs: Vec<crate::config::StatusGroup>,
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
    /// Only issues with open requests.
    #[serde(default)]
    pub requests: bool,
    pub text: String,
    pub sort: Sort,
}

/// The chip for statuses no group takes.
pub const OTHER: &str = "Other";

/// See [`Filter::group_of`]. With no groups (a filter made in code), Jira's categories.
pub fn group_of<'a>(defs: &'a [crate::config::StatusGroup], i: &Issue) -> &'a str {
    if defs.is_empty() {
        return match i.status_category {
            StatusCategory::ToDo => "Not started",
            StatusCategory::InProgress => "In progress · review",
            StatusCategory::Done => "Done",
        };
    }
    defs.iter()
        .find(|g| g.statuses.iter().any(|s| s.eq_ignore_ascii_case(&i.status)))
        .or_else(|| defs.iter().find(|g| g.category == Some(i.status_category)))
        .map_or(OTHER, |g| g.name.as_str())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dim {
    Sprint,
    Assignee,
    Label,
    Epic,
    Status,
    Group,
    Tag,
}

impl Filter {
    /// Whether any narrowing beyond the sprint is on.
    pub fn is_narrowed(&self) -> bool {
        !self.assignees.is_empty()
            || !self.labels.is_empty()
            || !self.epics.is_empty()
            || !self.statuses.is_empty()
            || !self.groups.is_empty()
            || self.hide_done
            || self.starred
            || self.noted
            || self.pending
            || !self.tags.is_empty()
            || self.review
            || self.requests
            || !self.text.trim().is_empty()
    }

    /// Clears everything but the sprint and sort.
    pub fn clear(&mut self) {
        *self = Filter {
            sprint: self.sprint.clone(),
            sort: self.sort,
            group_defs: std::mem::take(&mut self.group_defs),
            ..Default::default()
        };
    }

    /// The status chip `i` falls in: the first group naming its status, else the first taking
    /// its category, else [`OTHER`].
    pub fn group_of<'a>(&'a self, i: &Issue) -> &'a str {
        group_of(&self.group_defs, i)
    }

    fn sprint_ok(&self, i: &Issue) -> bool {
        match &self.sprint {
            SprintSel::All => true,
            SprintSel::Active => i.sprints.iter().any(|s| s.state == SprintState::Active),
            SprintSel::Backlog => !i.sprints.iter().any(|s| s.state != SprintState::Closed),
            SprintSel::Id(id) => i.sprints.iter().any(|s| s.id == *id),
            SprintSel::Ids(ids) => i.sprints.iter().any(|s| ids.contains(&s.id)),
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
        let group = (!self.groups.is_empty() || self.hide_done).then(|| self.group_of(i));
        if want(Dim::Group)
            && !self.groups.is_empty()
            && !group.is_some_and(|g| self.groups.contains(g))
        {
            return false;
        }
        // Choosing a chip says you want its issues, done ones included: it outranks Hide done
        // (also while counting the chips, so each shows how many it would bring back).
        if self.hide_done
            && i.status_category == StatusCategory::Done
            && !group.is_some_and(|g| self.groups.contains(g))
            && skip != Some(Dim::Group)
        {
            return false;
        }
        if want(Dim::Tag) && !self.tags.is_empty() && !f.tags.iter().any(|t| self.tags.contains(t))
        {
            return false;
        }
        !((self.starred && !f.starred)
            || (self.noted && !f.noted)
            || (self.pending && !f.pending)
            || (self.review && !f.review)
            || (self.requests && !f.requests))
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
            if self.passes_but_text(i, f, Some(Dim::Group)) {
                *fx.groups.entry(self.group_of(i).to_string()).or_default() += 1;
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
    /// Per status chip (by name), ignoring the chips' own choice.
    pub groups: BTreeMap<String, usize>,
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

    #[test]
    fn status_chips_filter_and_count() {
        let issues: Vec<Issue> = crate::demo::issues(120);
        let flags = vec![Flags::default(); issues.len()];
        let mut f = Filter {
            sprint: SprintSel::All,
            group_defs: crate::config::default_status_groups(),
            ..Default::default()
        };
        let total = f.apply(&issues, &flags).len();
        let fx = f.facets(&issues, &flags);
        assert_eq!(fx.groups.values().sum::<usize>(), total);
        f.groups.insert("In progress · review".into());
        let shown = f.apply(&issues, &flags);
        assert!(!shown.is_empty());
        assert!(
            shown
                .iter()
                .all(|i| i.status_category == StatusCategory::InProgress)
        );
        // The counts ignore the chips' own choice, so every chip still shows its number.
        assert_eq!(f.facets(&issues, &flags).groups, fx.groups);
        // Hide done hides done issues, unless a chosen chip holds them.
        f.groups.clear();
        f.hide_done = true;
        assert!(
            f.apply(&issues, &flags)
                .iter()
                .all(|i| i.status_category != StatusCategory::Done)
        );
        assert_eq!(
            f.facets(&issues, &flags).groups.get("Done"),
            fx.groups.get("Done")
        );
        f.groups.insert("Done".into());
        assert_eq!(f.apply(&issues, &flags).len(), fx.groups["Done"]);
    }

    #[test]
    fn groups_from_config_split_and_catch_the_rest() {
        use crate::config::StatusGroup;
        let issues: Vec<Issue> = crate::demo::issues(120);
        let flags = vec![Flags::default(); issues.len()];
        let g = |name: &str, statuses: &[&str], category| StatusGroup {
            name: name.into(),
            statuses: statuses.iter().map(|s| s.to_string()).collect(),
            category,
        };
        let mut f = Filter {
            sprint: SprintSel::All,
            group_defs: vec![
                g("Not started", &[], Some(StatusCategory::ToDo)),
                g("In progress", &["in progress", "Blocked"], None),
                g("In review", &["In Review"], None),
            ],
            ..Default::default()
        };
        let fx = f.facets(&issues, &flags);
        let n = |s: &str| issues.iter().filter(|i| i.status == s).count();
        assert_eq!(
            fx.groups["In progress"],
            n("In Progress") + n("Blocked"),
            "names ignore case"
        );
        assert_eq!(fx.groups["In review"], n("In Review"));
        assert_eq!(fx.groups[OTHER], n("Done"), "no group takes Done");
        f.groups.insert("In review".into());
        assert!(
            f.apply(&issues, &flags)
                .iter()
                .all(|i| i.status == "In Review")
        );
    }
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

    #[test]
    fn sprints_are_added_and_taken_out() {
        let s = SprintSel::Active.toggled(44);
        assert_eq!(s, SprintSel::Id(44), "adding to Active starts afresh");
        let s = s.toggled(45);
        assert_eq!(s, SprintSel::Ids([44, 45].into()));
        assert!(s.has(44) && s.has(45) && !s.has(43));
        assert_eq!(s.toggled(44), SprintSel::Id(45));
        assert_eq!(
            SprintSel::Id(45).toggled(45),
            SprintSel::Active,
            "none left: Active"
        );
        assert_eq!(SprintSel::All.toggled(7), SprintSel::Id(7));
        // An old saved filter (one sprint) still reads.
        let old: SprintSel = serde_json::from_str(r#"{"Id":44}"#).unwrap();
        assert_eq!(old, SprintSel::Id(44));
    }

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
