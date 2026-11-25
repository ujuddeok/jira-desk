//! A fuzzy picker: a query field over a list, for going to an issue, choosing filters and
//! making changes (status, assignee, labels…). Ranking is Chorong's (`cho_layout_ui::fuzzy`).

use crate::input::TextArea;
use cho_layout_ui::fuzzy::Ranker;
use gpui::UniformListScrollHandle;
use std::ops::Range;

/// What choosing an item does.
#[derive(Clone, Debug, PartialEq)]
pub enum Purpose {
    GoToIssue,
    /// Building a focus page: each issue chosen goes on the page, and the picker stays open.
    AddToPage,
    FilterSprint,
    FilterAssignee,
    FilterLabel,
    FilterEpic,
    FilterStatus,
    Sort,
    Transition(String),
    Assign(String),
    Labels(String),
    Priority(String),
    Sprint(String),
    /// Your own tags on an issue (local only).
    Tags(String),
    FilterTags,
    /// Same work, or repurposed? (an issue under review)
    Resolve(String),
    /// The command palette: run any command.
    Command,
    /// What kind of request to add on an issue (then its words, in a prompt).
    RequestKind(String),
    /// How to link an issue ("blocks", "is blocked by"…), then which issue (`LinkTo`).
    LinkHow(String),
    /// The issue to link to, `how`.
    LinkTo(String, String),
    /// A new parent (an epic) for an issue.
    Parent(String),
    /// Which sprints the Filters pane lists (Enter shows or hides one).
    SprintsShown,
}

impl Purpose {
    /// Several choices stay open (Enter toggles; Esc closes).
    pub fn multi(&self) -> bool {
        matches!(
            self,
            Purpose::FilterSprint
                | Purpose::SprintsShown
                | Purpose::FilterAssignee
                | Purpose::FilterLabel
                | Purpose::FilterEpic
                | Purpose::FilterStatus
                | Purpose::Labels(_)
                | Purpose::Tags(_)
                | Purpose::FilterTags
        )
    }
}

#[derive(Clone, Debug)]
pub struct Item {
    pub id: String,
    pub label: String,
    /// Muted text after the label (a count, a status).
    pub detail: String,
    pub checked: bool,
}

pub struct Picker {
    pub title: String,
    pub purpose: Purpose,
    pub items: Vec<Item>,
    pub input: TextArea,
    /// Indices into `items`, best first, with the matched byte ranges of each label.
    pub shown: Vec<(usize, Vec<Range<usize>>)>,
    pub cursor: usize,
    /// Offer to create a value that isn't in the list (labels, tags): "Add <this> “…”".
    pub allow_new: bool,
    pub new_what: &'static str,
    pub scroll: UniformListScrollHandle,
    ranker: Ranker,
    haystacks: Vec<String>,
    /// How many of `items` are real (the rest is the "Add …" offer).
    real: usize,
    /// The issue pickers' field words (`who:ana #perf`, see `omni::picker_scope`): only
    /// these item ids are shown, ranked by the other words.
    pub scope: Option<(std::collections::HashSet<String>, String)>,
}

pub const NEW_PREFIX: &str = "\u{0}new:";

impl Picker {
    pub fn new(title: &str, purpose: Purpose, items: Vec<Item>) -> Self {
        let haystacks: Vec<String> = items
            .iter()
            .map(|i| format!("{} {}", i.label, i.detail))
            .collect();
        let mut p = Self {
            title: title.to_string(),
            purpose,
            items,
            input: TextArea::new("", false),
            shown: Vec::new(),
            cursor: 0,
            allow_new: false,
            new_what: "label",
            scroll: UniformListScrollHandle::new(),
            ranker: Ranker::labels(),
            real: haystacks.len(),
            haystacks,
            scope: None,
        };
        p.requery();
        p
    }

    pub fn with_new(mut self, what: &'static str) -> Self {
        self.allow_new = true;
        self.new_what = what;
        self.requery();
        self
    }

    /// Starts on the item with this id (the current value).
    pub fn at(mut self, id: &str) -> Self {
        if let Some(pos) = self.shown.iter().position(|(i, _)| self.items[*i].id == id) {
            self.cursor = pos;
            self.scroll
                .scroll_to_item(pos, gpui::ScrollStrategy::Center);
        }
        self
    }

    pub fn requery(&mut self) {
        let q = self.input.text.trim().to_string();
        self.items.truncate(self.real);
        let scope = self.scope.as_ref();
        let ranked_by = scope.map_or(q.as_str(), |(_, words)| words.as_str());
        let items = &self.items;
        self.shown = self
            .ranker
            .rank(&self.haystacks, ranked_by, usize::MAX)
            .into_iter()
            .filter(|m| scope.is_none_or(|(keys, _)| keys.contains(&items[m.index].id)))
            .take(500)
            .map(|m| {
                let label_len = self.items[m.index].label.len();
                let ranges = m
                    .ranges
                    .into_iter()
                    .filter(|r| r.end <= label_len)
                    .collect();
                (m.index, ranges)
            })
            .collect();
        if self.allow_new
            && !q.is_empty()
            && !q.contains(char::is_whitespace)
            && !self.items.iter().any(|i| i.id == q)
        {
            self.items.push(Item {
                id: format!("{NEW_PREFIX}{q}"),
                label: format!("Add {} “{q}”", self.new_what),
                detail: "new".into(),
                checked: false,
            });
            self.shown.push((self.items.len() - 1, Vec::new()));
        }
        if q.is_empty() {
            self.cursor = self.cursor.min(self.shown.len().saturating_sub(1));
        } else {
            self.cursor = 0;
            self.scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
        }
    }

    pub fn move_by(&mut self, d: isize) {
        if self.shown.is_empty() {
            return;
        }
        let n = self.shown.len() as isize;
        self.cursor = ((self.cursor as isize + d).rem_euclid(n)) as usize;
        self.scroll
            .scroll_to_item(self.cursor, gpui::ScrollStrategy::Center);
    }

    pub fn current(&self) -> Option<&Item> {
        self.shown.get(self.cursor).map(|(i, _)| &self.items[*i])
    }

    pub fn current_mut(&mut self) -> Option<&mut Item> {
        let i = self.shown.get(self.cursor)?.0;
        self.items.get_mut(i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, summary: &str) -> Item {
        Item {
            id: id.into(),
            label: format!("{id}  {summary}"),
            detail: String::new(),
            checked: false,
        }
    }

    #[test]
    fn a_scope_keeps_its_issues_and_ranks_by_the_other_words() {
        let mut p = Picker::new(
            "Go to issue",
            Purpose::GoToIssue,
            vec![
                item("A-1", "cart total"),
                item("A-2", "cart icon"),
                item("A-3", "login"),
            ],
        );
        p.scope = Some((["A-2".to_string(), "A-3".to_string()].into(), "cart".into()));
        p.requery();
        let ids: Vec<&str> = p
            .shown
            .iter()
            .map(|(i, _)| p.items[*i].id.as_str())
            .collect();
        assert_eq!(ids, ["A-2"]);

        p.scope = Some((Default::default(), String::new()));
        p.requery();
        assert!(p.shown.is_empty());
    }
}
