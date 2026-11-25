//! The search bar (`/`, ⌘F): free text searches the list, and a field word (`tag`, `assignee`,
//! …, or two letters of one) followed by Tab or `:` opens a chip that picks a value for that
//! filter. The chips are the list's filters, so the bar always shows what is filtering it.
//! The value lists and toggles are the same as the `f` pickers'.

use crate::input::TextArea;
use crate::picker::{Item, Purpose};
use crate::state::Model;
use cho_layout_ui::fuzzy::Ranker;
use jdesk_core::filter::SprintSel;
use jdesk_core::model::SprintState;

/// A filter the bar can make a chip for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Sprint,
    Assignee,
    Label,
    Epic,
    Status,
    Tag,
}

impl Field {
    /// In the order chips are shown.
    pub const ALL: [Field; 6] = [
        Field::Sprint,
        Field::Assignee,
        Field::Label,
        Field::Epic,
        Field::Status,
        Field::Tag,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Field::Sprint => "sprint",
            Field::Assignee => "assignee",
            Field::Label => "label",
            Field::Epic => "epic",
            Field::Status => "status",
            Field::Tag => "tag",
        }
    }

    /// The words that name it (the first is its name).
    fn words(self) -> &'static [&'static str] {
        match self {
            Field::Sprint => &["sprint"],
            Field::Assignee => &["assignee", "who"],
            Field::Label => &["label"],
            Field::Epic => &["epic"],
            Field::Status => &["status"],
            Field::Tag => &["tag", "tags"],
        }
    }

    /// The field a typed word names: a whole word, or the start of one, two letters or more,
    /// naming only one field. Single letters never do, so "a fix" stays text.
    pub fn parse(word: &str) -> Option<Field> {
        let w = word.to_lowercase();
        if w.chars().count() < 2 {
            return None;
        }
        let hits: Vec<Field> = Field::ALL
            .into_iter()
            .filter(|f| f.words().iter().any(|x| x.starts_with(&w)))
            .collect();
        (hits.len() == 1).then(|| hits[0])
    }

    /// The fields a word could be starting (for hints while typing).
    pub fn starting(word: &str) -> Vec<Field> {
        let w = word.to_lowercase();
        if w.chars().count() < 2 {
            return Vec::new();
        }
        Field::ALL
            .into_iter()
            .filter(|f| f.words().iter().any(|x| x.starts_with(&w)))
            .collect()
    }

    pub fn purpose(self) -> Purpose {
        match self {
            Field::Sprint => Purpose::FilterSprint,
            Field::Assignee => Purpose::FilterAssignee,
            Field::Label => Purpose::FilterLabel,
            Field::Epic => Purpose::FilterEpic,
            Field::Status => Purpose::FilterStatus,
            Field::Tag => Purpose::FilterTags,
        }
    }
}

/// The bar while it is open.
#[derive(Clone)]
pub struct Omni {
    /// The free text, or, while a chip is open, the value being typed for it.
    pub input: TextArea,
    /// The chip being filled in.
    pub open: Option<Field>,
    /// The free text (and caret) kept while a chip is open.
    saved: Option<(String, usize)>,
    /// Backspace at the start selected the last chip; another removes it.
    pub chip_selected: bool,
    /// The highlighted suggestion (in a chip one always is; in free text only after ↓).
    pub cursor: Option<usize>,
}

impl Omni {
    pub fn new(text: &str) -> Self {
        let mut input = TextArea::new(text, false);
        input.all = !text.is_empty();
        Self {
            input,
            open: None,
            saved: None,
            chip_selected: false,
            cursor: None,
        }
    }

    /// The free text (what searches the list), also while a chip is open.
    pub fn text(&self) -> &str {
        match &self.saved {
            Some((t, _)) => t,
            None => &self.input.text,
        }
    }

    /// The word just before the caret in the free text, with where it starts, and the
    /// separator typed after it, if any (`tag:` → `Some(':')`).
    fn word_before_caret(&self) -> (usize, &str) {
        let t = &self.input.text[..self.input.cursor.min(self.input.text.len())];
        let start = t.rfind(char::is_whitespace).map_or(0, |i| i + 1);
        (start, &t[start..])
    }

    /// The field named by the word before the caret, if Tab should open a chip for it.
    pub fn field_at_caret(&self) -> Option<Field> {
        if self.open.is_some() {
            return None;
        }
        Field::parse(self.word_before_caret().1)
    }

    /// Opens a chip for `field`, taking its key word (and any `:`) out of the free text.
    pub fn open_chip(&mut self, field: Field, take_word: bool) {
        let mut text = self.input.text.clone();
        let mut caret = self.input.cursor.min(text.len());
        if take_word {
            let (start, _) = self.word_before_caret();
            text.replace_range(start..caret, "");
            caret = start;
            let trimmed = text[..caret].trim_end().len();
            text.replace_range(trimmed..caret, "");
            caret = trimmed;
            if trimmed > 0 && caret < text.len() {
                text.insert(caret, ' ');
            }
        }
        self.saved = Some((text, caret));
        self.input = TextArea::new("", false);
        self.open = Some(field);
        self.chip_selected = false;
        self.cursor = Some(0);
    }

    /// Back to the free text (after choosing a value, or cancelling the chip).
    pub fn close_chip(&mut self) {
        if let Some((t, caret)) = self.saved.take() {
            self.input = TextArea::new(&t, false);
            self.input.cursor = caret.min(t.len());
        }
        self.open = None;
        self.cursor = None;
    }

    /// After an edit: `tag:` opens a tag chip straight away, and so does `#` starting a
    /// word (your tags show as #tag).
    pub fn colon_opens(&mut self) -> bool {
        if self.open.is_some() {
            return false;
        }
        let (_, word) = self.word_before_caret();
        if word == "#" {
            self.open_chip(Field::Tag, true);
            return true;
        }
        let Some(w) = word.strip_suffix(':') else {
            return false;
        };
        match Field::parse(w) {
            Some(f) => {
                self.open_chip(f, true);
                true
            }
            None => false,
        }
    }
}

/// One row under the bar.
pub enum Suggestion {
    /// Tab (or `:`) opens a chip for this field.
    Field(Field),
    /// A value for the open chip; Tab or Enter takes it.
    Value(Item),
    /// Enter goes to this issue.
    GoTo(String, String),
}

/// What the bar offers now.
pub fn suggestions(m: &Model, o: &Omni) -> Vec<Suggestion> {
    if let Some(f) = o.open {
        let (_, items) = filter_choices(m, &f.purpose());
        // Names only: a status's category ("In progress") would otherwise match "prog" on
        // Blocked.
        let hay: Vec<String> = items.iter().map(|i| i.label.clone()).collect();
        return Ranker::labels()
            .rank(&hay, o.input.text.trim(), 8)
            .into_iter()
            .map(|r| Suggestion::Value(items[r.index].clone()))
            .collect();
    }
    let text = o.input.text.trim();
    if text.is_empty() {
        return Field::ALL.into_iter().map(Suggestion::Field).collect();
    }
    let mut out: Vec<Suggestion> = Field::starting(o.word_before_caret().1)
        .into_iter()
        .map(Suggestion::Field)
        .collect();
    if !text.contains(char::is_whitespace)
        && let Some(i) = m
            .effective
            .iter()
            .find(|i| i.key.eq_ignore_ascii_case(text))
    {
        out.push(Suggestion::GoTo(i.key.clone(), i.summary.clone()));
    }
    out
}

/// The chips: every filter on the list now, in [`Field::ALL`] order, as (field, value id,
/// what it shows).
pub fn chips(m: &Model) -> Vec<(Field, String, String)> {
    let mut out = Vec::new();
    let sprint: Vec<String> = match &m.filter.sprint {
        SprintSel::Active => vec!["active".to_string()],
        SprintSel::Backlog => vec!["backlog".to_string()],
        SprintSel::Id(_) | SprintSel::Ids(_) => {
            m.filter.sprint.ids().iter().map(u64::to_string).collect()
        }
        SprintSel::All => Vec::new(),
    };
    for f in Field::ALL {
        let chosen: Vec<String> = match f {
            Field::Sprint => sprint.clone(),
            Field::Assignee => m.filter.assignees.iter().cloned().collect(),
            Field::Label => m.filter.labels.iter().cloned().collect(),
            Field::Epic => m.filter.epics.iter().cloned().collect(),
            Field::Status => m.filter.statuses.iter().cloned().collect(),
            Field::Tag => m.filter.tags.iter().cloned().collect(),
        };
        if chosen.is_empty() {
            continue;
        }
        let (_, items) = filter_choices(m, &f.purpose());
        for id in chosen {
            let label = items
                .iter()
                .find(|i| i.id == id)
                .map(|i| i.label.clone())
                .unwrap_or_else(|| id.clone());
            out.push((f, id, label));
        }
    }
    out
}

/// What typing `query` into the bar and pressing Enter does, for `jdesk app search`: words
/// search the text; `field:value` and `#tag` add filters (a loose match on the value's name).
pub fn apply_query(m: &mut Model, query: &str) -> Result<String, String> {
    let mut words = Vec::new();
    let mut chosen: Vec<(Field, String, String)> = Vec::new();
    for tok in query.split_whitespace() {
        let (field, value) = if let Some(t) = tok.strip_prefix('#') {
            (Some(Field::Tag), t)
        } else if let Some((f, v)) = tok.split_once(':')
            && !v.is_empty()
            && let Some(field) = Field::parse(f)
        {
            (Some(field), v)
        } else {
            (None, tok)
        };
        let Some(f) = field else {
            words.push(tok);
            continue;
        };
        let (_, items) = filter_choices(m, &f.purpose());
        let hay: Vec<String> = items
            .iter()
            .map(|i| i.label.trim_start_matches('#').to_string())
            .collect();
        let Some(best) = Ranker::labels().rank(&hay, value, 1).into_iter().next() else {
            return Err(format!("no {} matches “{value}”", f.name()));
        };
        let item = &items[best.index];
        chosen.push((f, item.id.clone(), item.label.clone()));
    }
    let mut added = Vec::new();
    for (f, id, label) in chosen {
        let on = chips(m).iter().any(|(cf, cid, _)| *cf == f && *cid == id);
        if !on {
            apply_filter(m, &f.purpose(), &id);
        }
        added.push(format!("{}: {label}", f.name()));
    }
    m.filter.text = words.join(" ");
    m.refilter();
    Ok(format!(
        "{} issues{}{}",
        m.visible.len(),
        if added.is_empty() {
            String::new()
        } else {
            format!(" · {}", added.join(", "))
        },
        if m.filter.text.is_empty() {
            String::new()
        } else {
            format!(" · “{}”", m.filter.text)
        }
    ))
}

/// Removes one chip's filter.
pub fn remove_chip(m: &mut Model, field: Field, id: &str) {
    if field == Field::Sprint {
        // One of several sprints goes; the last (or Active, Backlog) leaves all issues.
        m.filter.sprint = match (&m.filter.sprint, id.parse::<u64>()) {
            (SprintSel::Ids(_), Ok(n)) => m.filter.sprint.toggled(n),
            _ => SprintSel::All,
        };
        m.refilter();
    } else {
        apply_filter(m, &field.purpose(), id);
    }
}

/// A count with its story points: `35 · 42 pts`, or just `35`.
fn with_points(n: usize, pts: f64) -> String {
    match crate::state::points_total(pts) {
        p if p.is_empty() => n.to_string(),
        p => format!("{n} · {p}"),
    }
}

/// The values of a filter (the `f` pickers' lists), with counts and what's chosen.
pub fn filter_choices(m: &Model, purpose: &Purpose) -> (String, Vec<Item>) {
    match purpose {
        Purpose::FilterSprint => {
            let mut items = vec![Item {
                id: "active".into(),
                label: "Active sprint".into(),
                detail: with_points(m.facets.active, m.facets.active_points),
                checked: m.filter.sprint == SprintSel::Active,
            }];
            for sp in m.desk.sprints() {
                items.push(Item {
                    id: sp.id.to_string(),
                    label: sp.name.clone(),
                    detail: format!(
                        "{} · {}",
                        match sp.state {
                            SprintState::Active => "active",
                            SprintState::Future => "next",
                            SprintState::Closed => "closed",
                        },
                        with_points(
                            m.facets.sprints.get(&sp.id).map_or(0, |(_, n)| *n),
                            m.facets.sprint_points.get(&sp.id).copied().unwrap_or(0.)
                        )
                    ),
                    checked: m.filter.sprint.has(sp.id),
                });
            }
            items.push(Item {
                id: "backlog".into(),
                label: "Backlog".into(),
                detail: with_points(m.facets.backlog, m.facets.backlog_points),
                checked: m.filter.sprint == SprintSel::Backlog,
            });
            items.push(Item {
                id: "all".into(),
                label: "All issues".into(),
                detail: with_points(m.facets.all, m.facets.all_points),
                checked: m.filter.sprint == SprintSel::All,
            });
            ("Sprints (Enter adds or removes one; Active, Backlog or All replace them; Esc closes)".into(), items)
        }
        Purpose::FilterAssignee => {
            let mut items: Vec<Item> = vec![Item {
                id: String::new(),
                label: "Unassigned".into(),
                detail: m
                    .facets
                    .assignees
                    .get("")
                    .map_or(0, |(_, n)| *n)
                    .to_string(),
                checked: m.filter.assignees.contains(""),
            }];
            for p in m.desk.assignees() {
                items.push(Item {
                    detail: m
                        .facets
                        .assignees
                        .get(&p.id)
                        .map_or(0, |(_, n)| *n)
                        .to_string(),
                    checked: m.filter.assignees.contains(&p.id),
                    label: if Some(&p.id) == m.desk.myself.as_ref().map(|x| &x.id) {
                        format!("{} (me)", p.name)
                    } else {
                        p.name.clone()
                    },
                    id: p.id,
                });
            }
            ("Assignee (Enter toggles, Esc closes)".into(), items)
        }
        Purpose::FilterLabel => (
            "Label (Enter toggles, Esc closes)".into(),
            m.desk
                .labels()
                .into_iter()
                .map(|l| Item {
                    detail: m.facets.labels.get(&l).copied().unwrap_or(0).to_string(),
                    checked: m.filter.labels.contains(&l),
                    label: l.clone(),
                    id: l,
                })
                .collect(),
        ),
        Purpose::FilterEpic => {
            let mut items = vec![Item {
                id: String::new(),
                label: "No epic".into(),
                detail: m.facets.epics.get("").map_or(0, |(_, n)| *n).to_string(),
                checked: m.filter.epics.contains(""),
            }];
            let mut epics: Vec<(String, String)> = m
                .effective
                .iter()
                .filter_map(|i| {
                    i.parent
                        .as_ref()
                        .map(|p| (p.key.clone(), p.summary.clone()))
                })
                .collect();
            epics.sort();
            epics.dedup_by(|a, b| a.0 == b.0);
            for (k, summary) in epics {
                items.push(Item {
                    detail: m.facets.epics.get(&k).map_or(0, |(_, n)| *n).to_string(),
                    checked: m.filter.epics.contains(&k),
                    label: format!("{k}  {summary}"),
                    id: k,
                });
            }
            ("Epic (Enter toggles, Esc closes)".into(), items)
        }
        Purpose::FilterStatus => (
            "Status (Enter toggles, Esc closes)".into(),
            m.desk
                .statuses()
                .into_iter()
                .map(|(name, cat)| Item {
                    detail: format!(
                        "{} · {}",
                        cat.label(),
                        m.facets.statuses.get(&name).map_or(0, |(_, n)| *n)
                    ),
                    checked: m.filter.statuses.contains(&name),
                    label: name.clone(),
                    id: name,
                })
                .collect(),
        ),
        Purpose::FilterTags => (
            "Your tags (Enter toggles, Esc closes)".into(),
            m.desk
                .tags()
                .into_iter()
                .map(|t| Item {
                    detail: m.facets.tags.get(&t).copied().unwrap_or(0).to_string(),
                    checked: m.filter.tags.contains(&t),
                    label: format!("#{t}"),
                    id: t,
                })
                .collect(),
        ),
        _ => (String::new(), Vec::new()),
    }
}

/// Chooses a value of a filter: Active, Backlog or All replace the sprint choice; a sprint and
/// any other value is added, or taken out if it was chosen.
pub fn apply_filter(m: &mut Model, purpose: &Purpose, id: &str) {
    toggle(&mut m.filter, purpose, id);
    m.refilter();
}

/// What `apply_filter` does to a filter.
fn toggle(filter: &mut jdesk_core::filter::Filter, purpose: &Purpose, id: &str) {
    if *purpose == Purpose::FilterSprint {
        filter.sprint = match id {
            "active" => SprintSel::Active,
            "backlog" => SprintSel::Backlog,
            "all" => SprintSel::All,
            // A sprint is added to those chosen, or taken out (`sp:44 sp:45`, the picker).
            id => filter.sprint.toggled(id.parse().unwrap_or(0)),
        };
    } else {
        let set = match purpose {
            Purpose::FilterAssignee => &mut filter.assignees,
            Purpose::FilterLabel => &mut filter.labels,
            Purpose::FilterEpic => &mut filter.epics,
            Purpose::FilterStatus => &mut filter.statuses,
            Purpose::FilterTags => &mut filter.tags,
            _ => return,
        };
        if !set.remove(id) {
            set.insert(id.to_string());
        }
    }
}

/// The issue pickers (Go to issue…, adding issues to a focus page) take the search bar's
/// words: `who:ana st:prog #perf cart`. Field words narrow to the downloaded issues that match
/// them all, in any sprint and whatever the list's filters; the other words are what the
/// picker ranks by. `None` when the query has no field words. A field word whose value
/// matches nothing leaves no issues; one still being typed (`who:`, `#`) is skipped.
pub fn picker_scope(m: &Model, query: &str) -> Option<(std::collections::HashSet<String>, String)> {
    let mut words = Vec::new();
    let mut filter = jdesk_core::filter::Filter {
        sprint: jdesk_core::filter::SprintSel::All,
        ..Default::default()
    };
    let mut any = false;
    let mut none = false;
    for tok in query.split_whitespace() {
        let (field, value) = if let Some(t) = tok.strip_prefix('#') {
            (Some(Field::Tag), t)
        } else if let Some((f, v)) = tok.split_once(':')
            && let Some(field) = Field::parse(f)
        {
            (Some(field), v)
        } else {
            (None, tok)
        };
        let Some(f) = field else {
            words.push(tok);
            continue;
        };
        any = true;
        if value.is_empty() {
            continue;
        }
        let (_, items) = filter_choices(m, &f.purpose());
        let hay: Vec<String> = items
            .iter()
            .map(|i| i.label.trim_start_matches('#').to_string())
            .collect();
        match Ranker::labels().rank(&hay, value, 1).into_iter().next() {
            Some(best) => toggle(&mut filter, &f.purpose(), &items[best.index].id),
            None => none = true,
        }
    }
    if !any {
        return None;
    }
    let keys = if none {
        Default::default()
    } else {
        m.effective
            .iter()
            .zip(&m.flags)
            .filter(|(i, f)| filter.matches(i, f))
            .map(|(i, _)| i.key.clone())
            .collect()
    };
    Some((keys, words.join(" ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_letters_name_a_field_one_never_does() {
        assert_eq!(Field::parse("ta"), Some(Field::Tag));
        assert_eq!(Field::parse("tags"), Some(Field::Tag));
        assert_eq!(Field::parse("ASS"), Some(Field::Assignee));
        assert_eq!(Field::parse("who"), Some(Field::Assignee));
        assert_eq!(Field::parse("sp"), Some(Field::Sprint));
        assert_eq!(Field::parse("st"), Some(Field::Status));
        assert_eq!(Field::parse("ep"), Some(Field::Epic));
        assert_eq!(Field::parse("la"), Some(Field::Label));
        assert_eq!(Field::parse("s"), None);
        assert_eq!(Field::parse("a"), None);
        assert_eq!(Field::parse("cache"), None);
        assert_eq!(Field::starting("s").len(), 0);
    }

    #[test]
    fn a_field_word_becomes_a_chip_and_the_text_stays() {
        let mut o = Omni::new("");
        o.input = TextArea::new("cache ta", false);
        o.input.cursor = o.input.text.len();
        assert_eq!(o.field_at_caret(), Some(Field::Tag));
        o.open_chip(Field::Tag, true);
        assert_eq!(o.open, Some(Field::Tag));
        assert_eq!(o.text(), "cache");
        assert_eq!(o.input.text, "");
        o.close_chip();
        assert_eq!(o.input.text, "cache");
        assert_eq!(o.open, None);

        let mut o = Omni::new("");
        o.input = TextArea::new("status:", false);
        o.input.cursor = o.input.text.len();
        assert!(o.colon_opens());
        assert_eq!(o.open, Some(Field::Status));
        assert_eq!(o.text(), "");

        let mut o = Omni::new("");
        o.input = TextArea::new("cache #", false);
        o.input.cursor = o.input.text.len();
        assert!(o.colon_opens(), "# starts a tag");
        assert_eq!(o.open, Some(Field::Tag));
        assert_eq!(o.text(), "cache");

        let mut o = Omni::new("");
        o.input = TextArea::new("issue#", false);
        o.input.cursor = o.input.text.len();
        assert!(!o.colon_opens(), "only at the start of a word");

        let mut o = Omni::new("");
        o.input = TextArea::new("time:", false);
        o.input.cursor = o.input.text.len();
        assert!(!o.colon_opens(), "not a field");
    }
}
