//! The app's state, shared by the root view and every pane body: the open data folder, the
//! filter and what it shows, cursors, and the background sync and apply.

use cho_layout::PaneContent;
use cho_layout_ui::colors::Colors;
use gpui::{Context, UniformListScrollHandle, px};
use jdesk_core::filter::{Facets, Filter, Flags, SprintSel};
use jdesk_core::model::{Issue, SprintState, StatusCategory};
use jdesk_core::queue::{ApplyReport, Edit, Queue};
use jdesk_core::{Desk, SyncOutcome, sync};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub const FILTERS: &str = "Filters";
/// The filtered list (⌥2) and the pane showing its selection (⌥3). Named apart: "Issues"
/// and "Issue" were too close to tell apart in pane lists and Go: commands.
pub const ISSUES: &str = "List";
pub const PREVIEW: &str = "Preview";
/// Their names before, in layouts saved by older versions (see `view::migrate_layout`).
pub const OLD_ISSUES: &str = "Issues";
pub const OLD_PREVIEW: &str = "Issue";
pub const NOTES: &str = "Notes";
pub const BOARD: &str = "Board";
pub const SETTINGS: &str = "Settings";
/// The sprints from `anchor` to `id` inclusive, either way round, in `order` (the Filters
/// pane's); `None` if either isn't there.
pub fn sprint_range(
    order: &[u64],
    anchor: u64,
    id: u64,
) -> Option<std::collections::BTreeSet<u64>> {
    let a = order.iter().position(|x| *x == anchor)?;
    let b = order.iter().position(|x| *x == id)?;
    Some(order[a.min(b)..=a.max(b)].iter().copied().collect())
}

/// Choices made in the app's Settings, kept in `prefs.json` in the data folder. Each is
/// `None` until you choose, so config.toml's value stands until then.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Prefs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_advance: Option<bool>,
    /// Sprints you chose to show or hide in the Filters pane, by id (the rest follow the
    /// usual rule: open sprints, and the two most recent closed ones).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub sprints_shown: std::collections::BTreeMap<u64, bool>,
}

impl Prefs {
    fn path(dir: &std::path::Path) -> std::path::PathBuf {
        dir.join("prefs.json")
    }

    /// The saved choices; none when the file is missing or unreadable.
    pub fn load(dir: &std::path::Path) -> Self {
        std::fs::read_to_string(Self::path(dir))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &std::path::Path) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        jdesk_core::write_atomic(&Self::path(dir), json.as_bytes()).map_err(std::io::Error::other)
    }
}

/// A story-point total as shown beside counts: `42 pts`, `12.5 pts`; nothing for none.
pub fn points_total(p: f64) -> String {
    if p <= 0. {
        String::new()
    } else if p.fract() == 0. {
        format!("{p:.0} pts")
    } else {
        format!("{p:.1} pts")
    }
}

/// A board column's width and the gap between columns, in pixels.
pub const BOARD_COL_W: f32 = 232.;
pub const BOARD_GAP: f32 = 8.;

/// Which pane has the keyboard.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum PaneFocus {
    Filters,
    #[default]
    Issues,
    Preview,
    /// An issue opened in its own tab.
    Issue(String),
    Notes,
    Changes,
    Board,
    Settings,
    Other,
}

impl PaneFocus {
    pub fn of(content: Option<&PaneContent>) -> Self {
        match content {
            Some(PaneContent::Placeholder { name }) => match name.as_str() {
                FILTERS => Self::Filters,
                ISSUES => Self::Issues,
                NOTES => Self::Notes,
                BOARD => Self::Board,
                SETTINGS => Self::Settings,
                _ => Self::Other,
            },
            Some(PaneContent::Editor { path, .. }) if path == PREVIEW => Self::Preview,
            Some(PaneContent::Editor { path, .. }) => Self::Issue(path.clone()),
            Some(PaneContent::Changes) => Self::Changes,
            _ => Self::Other,
        }
    }

    /// The word in the key context, for pane-specific bindings.
    pub fn context(&self) -> &'static str {
        match self {
            Self::Filters => "filters",
            Self::Issues => "issues",
            Self::Preview | Self::Issue(_) => "issue",
            Self::Notes => "notes",
            Self::Changes => "changes",
            Self::Board => "board",
            Self::Settings => "settings",
            Self::Other => "other",
        }
    }
}

/// One line of the Filters pane.
#[derive(Clone, Debug)]
pub enum FilterRow {
    Header(&'static str),
    Sprint {
        sel: SprintSel,
        label: String,
        detail: String,
        count: usize,
        on: bool,
    },
    Quick {
        which: Quick,
        label: &'static str,
        on: bool,
    },
    Assignee {
        id: String,
        name: String,
        count: usize,
        on: bool,
    },
    Epic {
        key: String,
        summary: String,
        count: usize,
        on: bool,
    },
    Label {
        label: String,
        count: usize,
        on: bool,
    },
    Status {
        name: String,
        category: StatusCategory,
        count: usize,
        on: bool,
    },
    /// One of your own tags.
    Tag {
        tag: String,
        count: usize,
        on: bool,
    },
    /// Opens the full picker for a long list.
    More(&'static str, MoreOf),
}

/// One row of the Changes pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChangeRow {
    /// An issue waiting for your review (changed a lot in Jira).
    Review(String),
    Change(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quick {
    Mine,
    HideDone,
    Starred,
    Noted,
    Pending,
    Review,
    Requests,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoreOf {
    Sprints,
    Labels,
}

impl FilterRow {
    pub fn is_header(&self) -> bool {
        matches!(self, FilterRow::Header(_))
    }
}

#[derive(Default)]
pub struct SyncState {
    pub running: bool,
    pub applying: bool,
    pub progress: Arc<Mutex<String>>,
    pub error: Option<String>,
    /// "12 changed · 340 ms", for the status bar.
    pub last_result: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Notice {
    pub text: String,
    pub error: bool,
}

/// An attachment image's local copy.
#[derive(Clone, Debug)]
pub enum ImageState {
    Loading,
    Ready {
        path: std::path::PathBuf,
        width: u32,
        height: u32,
    },
    Failed(String),
}

pub struct Model {
    pub desk: Desk,
    /// Attachment images by attachment id, downloaded for the issues on screen.
    pub images: HashMap<String, ImageState>,
    jira: Option<Arc<dyn jdesk_core::client::Jira>>,
    pub colors: Colors,
    pub filter: Filter,
    /// Every issue with queued changes applied, in key order (lines up with `desk.issues()`).
    pub effective: Vec<Issue>,
    pub flags: Vec<Flags>,
    /// Indices into `effective` the list shows, in order.
    pub visible: Vec<usize>,
    pub facets: Facets,
    pub selected: Option<String>,
    pub list_scroll: UniformListScrollHandle,
    pub filter_cursor: usize,
    /// The sprint last clicked or toggled in Filters: where a Shift range starts.
    pub sprint_anchor: Option<u64>,
    pub changes_cursor: usize,
    /// Board: (column, row).
    pub board_cursor: (usize, usize),
    /// The board's sideways scroll, to keep the current column in view.
    pub board_scroll: gpui::ScrollHandle,
    /// Each scrolling area's stretch at its ends, by name.
    pub scroll_areas: crate::scroll::ScrollAreas,
    /// After a change made from the List, go to the next issue (`auto_advance`; ⌘⇧P can
    /// flip it for this session).
    pub auto_advance: bool,
    /// The issue the Notes pane shows: the current one while an issue pane has focus, kept as
    /// it was while Notes (or Filters, Settings) does, so clicking in Notes stays on it.
    pub notes_for: Option<String>,
    /// Sprints shown or hidden in the Filters pane by your choice (prefs.json).
    pub sprints_shown: std::collections::BTreeMap<u64, bool>,
    /// Each issue pane's selectable text, as last drawn, by pane.
    pub text_maps: std::cell::RefCell<HashMap<String, crate::select::TextMap>>,
    /// The text selected in each issue pane.
    pub text_sel: std::cell::RefCell<HashMap<String, crate::select::TextSel>>,
    pub focus: PaneFocus,
    pub sync: SyncState,
    pub notice: Option<Notice>,
    /// The list's search field, while typing in it.
    pub search: Option<crate::omni::Omni>,
    /// The note being edited: (issue key, text).
    pub note_edit: Option<(String, crate::input::TextArea)>,
    pub comment_drafts: HashMap<String, String>,
    /// The data folder's size by kind, for the storage meter (measured in the background).
    pub usage: Option<jdesk_core::storage::Usage>,
    pub measuring: bool,
    /// Issues picked (x) for a focus series, in the order picked.
    pub picked: Vec<String>,
    /// The issues of the open focus series, one per page, in page order.
    pub series: Vec<String>,
    /// Requests from clicks in pane bodies that need the window: open this issue in a tab,
    /// open the full picker for a long filter list.
    pub pending_open: Option<String>,
    pub pending_more: Option<MoreOf>,
    /// A board card was clicked: show the Preview pane beside the board.
    pub pending_preview: bool,
    /// A request's text was clicked: show the whole request in a dialog.
    pub pending_request: Option<u64>,
    /// Bumped on every change, so cached pane bodies know to redraw.
    pub generation: u64,
    key_index: HashMap<String, usize>,
}

/// The key that applies the queue, as the keymap has it on this platform ("⌘⇧A" on a Mac,
/// "Ctrl-Shift-A" elsewhere, or your own), for hints written into the panes. Set when the
/// window opens.
static APPLY_KEY: std::sync::OnceLock<String> = std::sync::OnceLock::new();

pub fn set_apply_key(label: String) {
    let _ = APPLY_KEY.set(label);
}

pub fn apply_key() -> &'static str {
    APPLY_KEY
        .get()
        .map(String::as_str)
        .unwrap_or(if cfg!(target_os = "macos") {
            "⌘⇧A"
        } else {
            "Ctrl-Shift-A"
        })
}

/// The status chips' names for the palette and help ("Status chip: In review"), made once:
/// config.toml is read at start-up, so they don't change while the app runs.
static CHIP_LABELS: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();

fn set_chip_labels(defs: &[jdesk_core::config::StatusGroup]) {
    let _ = CHIP_LABELS.set(
        defs.iter()
            .take(9)
            .map(|g| &*Box::leak(format!("Status chip: {}", g.name).into_boxed_str()))
            .collect(),
    );
}

/// See [`CHIP_LABELS`]; the defaults until a window opens.
pub fn chip_labels() -> Vec<&'static str> {
    CHIP_LABELS.get().cloned().unwrap_or_else(|| {
        vec![
            "Status chip: Not started",
            "Status chip: In progress · review",
            "Status chip: Done",
        ]
    })
}

impl Model {
    pub fn new(desk: Desk, colors: Colors) -> Self {
        let saved: Option<Filter> = std::fs::read_to_string(desk.dir.join("ui.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok());
        let restored = saved.is_some();
        let mut filter = saved.unwrap_or_default();
        // The chips come from config.toml; a choice saved under a name that's gone is dropped.
        filter.group_defs = desk.status_groups.clone();
        set_chip_labels(&filter.group_defs);
        let known: Vec<String> = filter.group_defs.iter().map(|g| g.name.clone()).collect();
        filter
            .groups
            .retain(|g| known.contains(g) || g == jdesk_core::filter::OTHER);
        // Kanban sites have no sprints: show everything rather than an empty list.
        if !restored
            && !desk
                .issues()
                .iter()
                .any(|i| i.sprints.iter().any(|s| s.state == SprintState::Active))
            && !desk.issues().is_empty()
        {
            filter.sprint = SprintSel::All;
        }
        let sprints_shown = Prefs::load(&desk.dir).sprints_shown;
        // Your choice in Settings (prefs.json) wins over `auto_advance` in config.toml.
        let auto_advance = Prefs::load(&desk.dir)
            .auto_advance
            .unwrap_or(desk.auto_advance);
        let mut m = Self {
            desk,
            images: HashMap::new(),
            jira: None,
            colors,
            filter,
            effective: Vec::new(),
            flags: Vec::new(),
            visible: Vec::new(),
            facets: Facets::default(),
            selected: None,
            list_scroll: UniformListScrollHandle::new(),
            filter_cursor: 1,
            sprint_anchor: None,
            changes_cursor: 0,
            board_cursor: (0, 0),
            board_scroll: gpui::ScrollHandle::new(),
            scroll_areas: Default::default(),
            auto_advance,
            notes_for: None,
            sprints_shown,
            text_maps: Default::default(),
            text_sel: Default::default(),
            focus: PaneFocus::Issues,
            sync: SyncState::default(),
            notice: None,
            search: None,
            note_edit: None,
            comment_drafts: HashMap::new(),
            usage: None,
            measuring: false,
            picked: Vec::new(),
            series: Vec::new(),
            pending_open: None,
            pending_preview: false,
            pending_request: None,
            pending_more: None,
            generation: 0,
            key_index: HashMap::new(),
        };
        m.rebuild();
        m
    }

    /// Recomputes everything derived from the issues, queue and sidecar.
    pub fn rebuild(&mut self) {
        self.effective = self.desk.effective_issues();
        self.flags = self
            .effective
            .iter()
            .map(|i| {
                let mut f = self.desk.flags(&i.key);
                f.review = self.desk.identities.open_review(&i.key).is_some();
                f
            })
            .collect();
        self.key_index = self
            .effective
            .iter()
            .enumerate()
            .map(|(n, i)| (i.key.clone(), n))
            .collect();
        self.refilter();
    }

    /// Drops the selectable text and selection `pane` kept for other issues than `area`.
    pub fn forget_text_except(&self, pane: &str, area: &str) {
        let prefix = format!("{pane}|");
        self.text_maps
            .borrow_mut()
            .retain(|k, _| !k.starts_with(&prefix) || k == area);
        self.text_sel
            .borrow_mut()
            .retain(|k, _| !k.starts_with(&prefix) || k == area);
    }

    /// The text selected in `area`, if any.
    pub fn sel(&self, area: &str) -> Option<crate::select::TextSel> {
        self.text_sel.borrow().get(area).copied()
    }

    /// The selectable text of the pane called `pane` (see [`crate::select`]).
    pub fn text_map(&self, pane: &str) -> crate::select::TextMap {
        self.text_maps
            .borrow_mut()
            .entry(pane.to_string())
            .or_default()
            .clone()
    }

    /// Whether scrolling areas stretch at their ends.
    pub fn stretch(&self) -> crate::overscroll::Stretch {
        self.desk.overscroll().into()
    }

    /// Recomputes the list and counts after the filter changed.
    pub fn refilter(&mut self) {
        let shown = self.filter.apply(&self.effective, &self.flags);
        self.visible = shown
            .iter()
            .filter_map(|i| self.key_index.get(&i.key).copied())
            .collect();
        self.facets = self.filter.facets(&self.effective, &self.flags);
        let still = self
            .selected
            .as_ref()
            .is_some_and(|k| self.visible.iter().any(|i| self.effective[*i].key == *k));
        if !still {
            self.selected = self.visible.first().map(|i| self.effective[*i].key.clone());
        }
        let rows = self.filter_rows().len();
        self.filter_cursor = self.filter_cursor.min(rows.saturating_sub(1));
        self.generation += 1;
        self.save_filter();
    }

    /// Remembers the filter (`ui.json` in the data folder) for the next launch.
    fn save_filter(&self) {
        if let Ok(json) = serde_json::to_string_pretty(&self.filter) {
            let path = self.desk.dir.join("ui.json");
            if std::fs::read_to_string(&path).ok().as_deref() != Some(json.as_str()) {
                let _ = jdesk_core::write_atomic(&path, json.as_bytes());
            }
        }
    }

    pub fn effective_issue(&self, key: &str) -> Option<&Issue> {
        self.key_index.get(key).map(|i| &self.effective[*i])
    }

    pub fn flags_of(&self, key: &str) -> Flags {
        self.key_index
            .get(key)
            .map(|i| self.flags[*i].clone())
            .unwrap_or_default()
    }

    pub fn selected_row(&self) -> Option<usize> {
        let k = self.selected.as_ref()?;
        self.visible
            .iter()
            .position(|i| self.effective[*i].key == *k)
    }

    pub fn select_row(&mut self, row: usize) {
        if let Some(i) = self.visible.get(row) {
            self.selected = Some(self.effective[*i].key.clone());
            self.list_scroll
                .scroll_to_item(row, gpui::ScrollStrategy::Center);
            self.generation += 1;
        }
    }

    pub fn select_key(&mut self, key: &str) {
        if let Some(row) = self
            .visible
            .iter()
            .position(|i| self.effective[*i].key == key)
        {
            self.select_row(row);
        } else {
            self.selected = Some(key.to_string());
            self.generation += 1;
        }
    }

    /// The issue the edit keys act on: the focused issue tab, the board's card, or the
    /// list's selection.
    pub fn current_key(&self) -> Option<String> {
        match &self.focus {
            PaneFocus::Issue(k) => Some(k.clone()),
            PaneFocus::Board => self.board_key().or_else(|| self.selected.clone()),
            PaneFocus::Changes => match self.current_change_row() {
                Some(ChangeRow::Review(k)) => Some(k),
                Some(ChangeRow::Change(id)) => self
                    .desk
                    .queue
                    .changes()
                    .iter()
                    .find(|c| c.id == id)
                    .map(|c| c.key.clone())
                    .or_else(|| self.selected.clone()),
                None => self.selected.clone(),
            },
            // Notes stays on the issue it was showing (`follow_for_notes`).
            PaneFocus::Notes => self.notes_for.clone().or_else(|| self.selected.clone()),
            _ => self.selected.clone(),
        }
    }

    /// Keeps `notes_for` on the current issue while a pane that has one is focused (an issue
    /// tab, Preview, the List, the Board, Changes); elsewhere it stays as it was.
    pub fn follow_for_notes(&mut self) {
        if !matches!(
            self.focus,
            PaneFocus::Notes | PaneFocus::Filters | PaneFocus::Settings | PaneFocus::Other
        ) {
            self.notes_for = self.current_key();
        }
    }

    pub fn note(&mut self, text: impl Into<String>) {
        self.notice = Some(Notice {
            text: text.into(),
            error: false,
        });
        self.generation += 1;
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.notice = Some(Notice {
            text: text.into(),
            error: true,
        });
        self.generation += 1;
    }

    // ----- filters --------------------------------------------------------------------------

    /// A person's name from their id, from anything we've seen.
    pub fn person_name(&self, id: &str) -> String {
        self.desk
            .assignees()
            .into_iter()
            .find(|p| p.id == id)
            .map_or(id.to_string(), |p| p.name)
    }

    pub fn me(&self) -> Option<String> {
        self.desk.myself.as_ref().map(|p| p.id.clone())
    }

    pub fn filter_rows(&self) -> Vec<FilterRow> {
        let f = &self.filter;
        let fx = &self.facets;
        let mut rows = vec![FilterRow::Header("Sprint")];
        let mut sprint = |sel: SprintSel, label: String, detail: String, count: usize| {
            let on = match &sel {
                SprintSel::Id(id) => f.sprint.has(*id),
                other => f.sprint == *other,
            };
            rows.push(FilterRow::Sprint {
                sel,
                label,
                detail,
                count,
                on,
            });
        };
        sprint(
            SprintSel::Active,
            "Active sprint".into(),
            points_total(fx.active_points),
            fx.active,
        );
        let sprints = self.desk.sprints();
        let shown = self.sprints_in_filters();
        for s in &sprints {
            // A sprint the list is showing stays, even if you hid it.
            if shown.contains(&s.id) || f.sprint.has(s.id) {
                let count = fx.sprints.get(&s.id).map_or(0, |(_, n)| *n);
                let state = match s.state {
                    SprintState::Active => "active",
                    SprintState::Future => "next",
                    SprintState::Closed => "closed",
                };
                let pts = points_total(fx.sprint_points.get(&s.id).copied().unwrap_or(0.));
                let detail = if pts.is_empty() {
                    state.to_string()
                } else {
                    format!("{state} · {pts}")
                };
                sprint(SprintSel::Id(s.id), s.name.clone(), detail, count);
            }
        }
        sprint(
            SprintSel::Backlog,
            "Backlog".into(),
            points_total(fx.backlog_points),
            fx.backlog,
        );
        sprint(
            SprintSel::All,
            "All issues".into(),
            points_total(fx.all_points),
            fx.all,
        );
        if sprints.len() > shown.len() {
            rows.push(FilterRow::More("More sprints…", MoreOf::Sprints));
        }

        rows.push(FilterRow::Header("Show"));
        let me = self.me();
        let mine = me
            .as_ref()
            .is_some_and(|m| f.assignees.len() == 1 && f.assignees.contains(m));
        for (which, label, on) in [
            (Quick::Mine, "Assigned to me", mine),
            (Quick::HideDone, "Hide done", f.hide_done),
            (Quick::Starred, "Starred", f.starred),
            (Quick::Noted, "With notes", f.noted),
            (Quick::Pending, "With queued changes", f.pending),
            (Quick::Requests, "With open requests", f.requests),
        ] {
            rows.push(FilterRow::Quick { which, label, on });
        }
        if f.review || self.desk.identities.open_reviews().next().is_some() {
            rows.push(FilterRow::Quick {
                which: Quick::Review,
                label: "Waiting for review",
                on: f.review,
            });
        }
        if !fx.tags.is_empty() {
            rows.push(FilterRow::Header("Your tags"));
            let mut tags: Vec<(&String, &usize)> = fx.tags.iter().collect();
            tags.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
            for (tag, count) in tags {
                rows.push(FilterRow::Tag {
                    tag: tag.clone(),
                    count: *count,
                    on: f.tags.contains(tag),
                });
            }
        }

        rows.push(FilterRow::Header("Assignee"));
        let mut people: Vec<(&String, &(String, usize))> = fx.assignees.iter().collect();
        people.sort_by(|a, b| {
            (a.0.is_empty())
                .cmp(&b.0.is_empty())
                .then(b.1.1.cmp(&a.1.1))
                .then(a.1.0.cmp(&b.1.0))
        });
        for (id, (name, count)) in people {
            let name = if name == id && !id.is_empty() {
                self.person_name(id)
            } else {
                name.clone()
            };
            rows.push(FilterRow::Assignee {
                id: id.clone(),
                name,
                count: *count,
                on: f.assignees.contains(id),
            });
        }

        rows.push(FilterRow::Header("Epic"));
        let mut epics: Vec<(&String, &(String, usize))> = fx.epics.iter().collect();
        epics.sort_by(|a, b| {
            (a.0.is_empty())
                .cmp(&b.0.is_empty())
                .then(b.1.1.cmp(&a.1.1))
        });
        for (key, (summary, count)) in epics {
            let summary = if summary.is_empty() && !key.is_empty() {
                self.desk
                    .issue(key)
                    .map(|i| i.summary.clone())
                    .unwrap_or_default()
            } else {
                summary.clone()
            };
            rows.push(FilterRow::Epic {
                key: key.clone(),
                summary,
                count: *count,
                on: f.epics.contains(key),
            });
        }

        rows.push(FilterRow::Header("Status"));
        let mut statuses: Vec<(&String, &(StatusCategory, usize))> = fx.statuses.iter().collect();
        statuses.sort_by(|a, b| a.1.0.cmp(&b.1.0).then(a.0.cmp(b.0)));
        for (name, (category, count)) in statuses {
            rows.push(FilterRow::Status {
                name: name.clone(),
                category: *category,
                count: *count,
                on: f.statuses.contains(name),
            });
        }

        rows.push(FilterRow::Header("Label"));
        let mut labels: Vec<(&String, &usize)> = fx.labels.iter().collect();
        labels.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        let many = labels.len() > 12;
        for (label, count) in labels
            .iter()
            .enumerate()
            .filter(|(n, (l, _))| *n < 12 || f.labels.contains(*l))
            .map(|(_, x)| x)
        {
            rows.push(FilterRow::Label {
                label: (*label).clone(),
                count: **count,
                on: f.labels.contains(*label),
            });
        }
        if many {
            rows.push(FilterRow::More("More labels…", MoreOf::Labels));
        }
        rows
    }

    /// The sprints the Filters pane lists: your choice for each (Choose which sprints show…,
    /// or h on a row), else open sprints and the two most recent closed ones.
    pub fn sprints_in_filters(&self) -> std::collections::BTreeSet<u64> {
        let mut closed = 0;
        self.desk
            .sprints()
            .iter()
            .filter(|s| {
                let usual = if s.state == SprintState::Closed {
                    closed += 1;
                    closed <= 2
                } else {
                    true
                };
                self.sprints_shown.get(&s.id).copied().unwrap_or(usual)
            })
            .map(|s| s.id)
            .collect()
    }

    /// Shows or hides a sprint in the Filters pane, and remembers it.
    pub fn show_sprint_in_filters(&mut self, id: u64, on: bool) {
        self.sprints_shown.insert(id, on);
        let mut prefs = Prefs::load(&self.desk.dir);
        prefs.sprints_shown = self.sprints_shown.clone();
        if let Err(e) = prefs.save(&self.desk.dir) {
            self.error(format!("Not saved: {e}"));
        }
        self.generation += 1;
    }

    /// Space or ⌘-click on a Filters row: a sprint is added to those shown (or taken out);
    /// any other row toggles as with Enter.
    pub fn add_filter_row(&mut self, row: &FilterRow) {
        if let FilterRow::Sprint {
            sel: SprintSel::Id(id),
            ..
        } = row
        {
            self.filter.sprint = self.filter.sprint.toggled(*id);
            self.sprint_anchor = Some(*id);
            self.refilter();
        } else {
            self.toggle_filter_row(row);
        }
    }

    /// ⇧-click or ⇧Space on a sprint row: the sprints from the last one clicked (or toggled)
    /// to this one, in the Filters pane's order, become those shown. With no earlier sprint, or
    /// on another row, as a plain click.
    pub fn range_filter_row(&mut self, row: &FilterRow) {
        let (
            FilterRow::Sprint {
                sel: SprintSel::Id(id),
                ..
            },
            Some(anchor),
        ) = (row, self.sprint_anchor)
        else {
            return self.toggle_filter_row(row);
        };
        let order: Vec<u64> = self
            .filter_rows()
            .iter()
            .filter_map(|r| match r {
                FilterRow::Sprint {
                    sel: SprintSel::Id(x),
                    ..
                } => Some(*x),
                _ => None,
            })
            .collect();
        let Some(ids) = sprint_range(&order, anchor, *id) else {
            return self.toggle_filter_row(row);
        };
        self.filter.sprint = SprintSel::from_ids(ids);
        // The anchor stays, so another ⇧-click moves the range's far end.
        self.refilter();
    }

    /// Applies a Filters row (Enter on it, or a click). A sprint row shows only that sprint.
    pub fn toggle_filter_row(&mut self, row: &FilterRow) {
        if let FilterRow::Sprint {
            sel: SprintSel::Id(id),
            ..
        } = row
        {
            self.sprint_anchor = Some(*id);
        }
        let f = &mut self.filter;
        match row {
            FilterRow::Header(_) | FilterRow::More(..) => return,
            FilterRow::Sprint { sel, .. } => f.sprint = sel.clone(),
            FilterRow::Quick { which, .. } => self.toggle_quick(*which),
            FilterRow::Assignee { id, .. } => toggle(&mut f.assignees, id),
            FilterRow::Epic { key, .. } => toggle(&mut f.epics, key),
            FilterRow::Label { label, .. } => toggle(&mut f.labels, label),
            FilterRow::Tag { tag, .. } => toggle(&mut f.tags, tag),
            FilterRow::Status { name, .. } => toggle(&mut f.statuses, name),
        }
        self.refilter();
    }

    /// A status chip on or off (they combine: in progress or done).
    pub fn toggle_group(&mut self, name: &str) {
        if !self.filter.groups.remove(name) {
            self.filter.groups.insert(name.to_string());
        }
        self.refilter();
    }

    /// The status chips, in order: the groups from config.toml, then Other when some issues
    /// fall in no group (or it's chosen).
    pub fn chip_names(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .filter
            .group_defs
            .iter()
            .map(|g| g.name.clone())
            .collect();
        let other = jdesk_core::filter::OTHER;
        if self.facets.groups.get(other).copied().unwrap_or(0) > 0
            || self.filter.groups.contains(other)
        {
            out.push(other.to_string());
        }
        out
    }

    /// The Jira category a chip mostly holds, for its colour: its own `category`, else that of
    /// its first status Jira knows.
    pub fn chip_category(&self, name: &str) -> jdesk_core::model::StatusCategory {
        let Some(g) = self.filter.group_defs.iter().find(|g| g.name == name) else {
            return jdesk_core::model::StatusCategory::ToDo;
        };
        if let Some(c) = g.category {
            return c;
        }
        let known = self.desk.statuses();
        g.statuses
            .iter()
            .find_map(|s| {
                known
                    .iter()
                    .find(|(n, _)| n.eq_ignore_ascii_case(s))
                    .map(|(_, c)| *c)
            })
            .unwrap_or_default()
    }

    pub fn toggle_quick(&mut self, which: Quick) {
        let f = &mut self.filter;
        match which {
            Quick::Mine => {
                if let Some(me) = self.desk.myself.as_ref().map(|p| p.id.clone()) {
                    if f.assignees.len() == 1 && f.assignees.contains(&me) {
                        f.assignees.clear();
                    } else {
                        f.assignees.clear();
                        f.assignees.insert(me);
                    }
                }
            }
            Quick::HideDone => f.hide_done = !f.hide_done,
            Quick::Starred => f.starred = !f.starred,
            Quick::Noted => f.noted = !f.noted,
            Quick::Pending => f.pending = !f.pending,
            Quick::Review => f.review = !f.review,
            Quick::Requests => f.requests = !f.requests,
        }
    }

    /// A one-line summary of the filter, for the List pane's header.
    pub fn filter_summary(&self) -> String {
        let f = &self.filter;
        let mut parts = vec![match &f.sprint {
            SprintSel::Active => "Active sprint".to_string(),
            SprintSel::Backlog => "Backlog".into(),
            SprintSel::All => "All issues".into(),
            SprintSel::Id(_) | SprintSel::Ids(_) => {
                let all = self.desk.sprints();
                f.sprint
                    .ids()
                    .iter()
                    .map(|id| {
                        all.iter()
                            .find(|s| s.id == *id)
                            .map_or(format!("Sprint {id}"), |s| s.name.clone())
                    })
                    .collect::<Vec<_>>()
                    .join(" + ")
            }
        }];
        let names = |ids: &std::collections::BTreeSet<String>, f: &dyn Fn(&str) -> String| {
            ids.iter().map(|i| f(i)).collect::<Vec<_>>().join(" or ")
        };
        if !f.assignees.is_empty() {
            parts.push(names(&f.assignees, &|id| {
                if id.is_empty() {
                    "Unassigned".into()
                } else if Some(id.to_string()) == self.me() {
                    "me".into()
                } else {
                    self.person_name(id)
                }
            }));
        }
        if !f.epics.is_empty() {
            parts.push(names(&f.epics, &|k| {
                if k.is_empty() {
                    "No epic".into()
                } else {
                    k.to_string()
                }
            }));
        }
        if !f.statuses.is_empty() {
            parts.push(names(&f.statuses, &|s| s.to_string()));
        }
        if !f.labels.is_empty() {
            parts.push(names(&f.labels, &|l| format!("label {l}")));
        }
        if !f.tags.is_empty() {
            parts.push(names(&f.tags, &|t| format!("#{t}")));
        }
        if !f.groups.is_empty() {
            // In chip order, as the chips read.
            let chosen: Vec<String> = self
                .chip_names()
                .into_iter()
                .filter(|n| f.groups.contains(n))
                .map(|n| n.replace(" · ", " or ").to_lowercase())
                .collect();
            parts.push(chosen.join(" or "));
        }
        for (on, w) in [
            // A chosen chip outranks Hide done for its own issues.
            (f.hide_done && f.groups.is_empty(), "not done"),
            (f.starred, "starred"),
            (f.noted, "noted"),
            (f.pending, "queued"),
            (f.review, "to review"),
            (f.requests, "with requests"),
        ] {
            if on {
                parts.push(w.into());
            }
        }
        if !f.text.trim().is_empty() {
            parts.push(format!("“{}”", f.text.trim()));
        }
        parts.join(" · ")
    }

    // ----- board ----------------------------------------------------------------------------

    /// Board columns: each status among the listed issues, in workflow order, with its cards.
    pub fn board_columns(&self) -> Vec<(String, StatusCategory, Vec<usize>)> {
        let mut cols: Vec<(String, StatusCategory, Vec<usize>)> = Vec::new();
        for &i in &self.visible {
            let issue = &self.effective[i];
            match cols.iter_mut().find(|c| c.0 == issue.status) {
                Some(c) => c.2.push(i),
                None => cols.push((issue.status.clone(), issue.status_category, vec![i])),
            }
        }
        let rank = |s: &str| match s.to_ascii_lowercase().as_str() {
            "backlog" => 0,
            "to do" | "open" | "selected for development" => 1,
            "in progress" => 2,
            "in review" | "review" | "code review" => 3,
            "blocked" => 4,
            "qa" | "testing" => 5,
            _ => 6,
        };
        cols.sort_by(|a, b| {
            a.1.cmp(&b.1)
                .then(rank(&a.0).cmp(&rank(&b.0)))
                .then(a.0.cmp(&b.0))
        });
        cols
    }

    /// The board's cursor moved: the list's selection follows it (so the Preview pane shows the
    /// card), and its column scrolls into view.
    pub fn board_moved(&mut self) {
        if let Some(k) = self.board_key() {
            self.select_key(&k);
        }
        let col = self
            .board_cursor
            .0
            .min(self.board_columns().len().saturating_sub(1));
        let left = px(col as f32 * (BOARD_COL_W + BOARD_GAP));
        let right = left + px(BOARD_COL_W + 2. * BOARD_GAP);
        let h = &self.board_scroll;
        let width = h.bounds().size.width;
        let o = h.offset();
        if width > px(0.) {
            if left < -o.x {
                h.set_offset(gpui::point(-left, o.y));
            } else if right > -o.x + width {
                h.set_offset(gpui::point(-(right - width), o.y));
            }
        }
        self.generation += 1;
    }

    pub fn board_key(&self) -> Option<String> {
        let cols = self.board_columns();
        let (c, r) = self.board_cursor;
        let col = cols.get(c.min(cols.len().saturating_sub(1)))?;
        let i = col.2.get(r.min(col.2.len().saturating_sub(1)))?;
        Some(self.effective[*i].key.clone())
    }

    // ----- changes --------------------------------------------------------------------------

    /// Queues a change to `key`, with a status-bar note.
    /// Rebuilds after a change to `key` without losing your place in the List: if the issue
    /// no longer fits the filter, the one that took its row is selected (not the first row);
    /// with `advance` and auto-advance on, the next issue is. Only when the List has focus and
    /// `key` is its selection: a change made from a tab or the Board leaves the List alone.
    pub fn rebuild_after_change(&mut self, key: &str, advance: bool) {
        let ours = self.focus == PaneFocus::Issues && self.selected.as_deref() == Some(key);
        let row = self.selected_row();
        self.rebuild();
        let (true, Some(row)) = (ours, row) else {
            return;
        };
        let last = self.visible.len().saturating_sub(1);
        match self.selected_row() {
            // Still listed (refilter keeps the selection when it can).
            Some(now) if self.selected.as_deref() == Some(key) => {
                if advance && self.auto_advance {
                    self.select_row((now + 1).min(last));
                }
            }
            // It left the list: the issue now in its row.
            _ if !self.visible.is_empty() => self.select_row(row.min(last)),
            _ => {}
        }
    }

    pub fn queue(&mut self, key: &str, edit: Edit) {
        if self.sync.applying {
            self.error("Wait for the changes being applied to finish");
            return;
        }
        let what = edit.describe();
        match self.desk.queue_edit(key, edit) {
            Ok(true) => {
                let n = self.desk.queue.pending_count();
                self.rebuild_after_change(key, true);
                self.note(format!(
                    "Queued {key}: {what} · {n} change{} to apply ({})",
                    if n == 1 { "" } else { "s" },
                    apply_key()
                ));
            }
            Ok(false) => self.note(format!("{key}: no change")),
            Err(e) => self.error(e.to_string()),
        }
    }

    /// What the Changes pane lists: issues waiting for review, then open changes (in order),
    /// then history, newest first.
    pub fn change_rows(&self) -> Vec<ChangeRow> {
        let changes = self.desk.queue.changes();
        let mut out: Vec<ChangeRow> = self
            .desk
            .identities
            .open_reviews()
            .map(|r| ChangeRow::Review(r.key.clone()))
            .collect();
        out.extend(
            changes
                .iter()
                .filter(|c| c.is_open())
                .map(|c| ChangeRow::Change(c.id)),
        );
        out.extend(
            changes
                .iter()
                .rev()
                .filter(|c| !c.is_open())
                .map(|c| ChangeRow::Change(c.id)),
        );
        out
    }

    pub fn current_change_row(&self) -> Option<ChangeRow> {
        let rows = self.change_rows();
        rows.get(self.changes_cursor.min(rows.len().saturating_sub(1)))
            .cloned()
    }

    /// The change under the cursor in the Changes pane (not a review row).
    pub fn current_change(&self) -> Option<u64> {
        match self.current_change_row()? {
            ChangeRow::Change(id) => Some(id),
            ChangeRow::Review(_) => None,
        }
    }

    /// Changes ready to apply (pending, not held for review), and how many are held.
    pub fn pending_and_held(&self) -> (usize, usize) {
        let mut out = (0, 0);
        for c in self.desk.queue.changes() {
            if c.state == jdesk_core::queue::State::Pending {
                if self.held(&c.key) {
                    out.1 += 1;
                } else {
                    out.0 += 1;
                }
            }
        }
        out
    }

    /// Whether `key`'s queued changes are held for review.
    pub fn held(&self, key: &str) -> bool {
        self.desk.identities.open_review(key).is_some()
    }

    /// Picks or unpicks an issue for a focus series.
    pub fn toggle_pick(&mut self, key: &str) {
        match self.picked.iter().position(|k| k == key) {
            Some(i) => {
                self.picked.remove(i);
            }
            None => self.picked.push(key.to_string()),
        }
        self.generation += 1;
    }

    // ----- storage --------------------------------------------------------------------------

    /// Measures the data folder in the background (the Settings pane's meter).
    pub fn measure_storage(&mut self, cx: &mut Context<Self>) {
        if self.measuring {
            return;
        }
        self.measuring = true;
        let dir = self.desk.dir.clone();
        let task = cx
            .background_executor()
            .spawn(async move { jdesk_core::storage::measure(&dir) });
        cx.spawn(async move |this, cx| {
            let usage = task.await;
            this.update(cx, |m, cx| {
                m.usage = Some(usage);
                m.measuring = false;
                m.generation += 1;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Deletes the downloaded images; they come back when you open those issues.
    pub fn clear_images(&mut self, cx: &mut Context<Self>) {
        match jdesk_core::storage::clear_images(&self.desk.dir) {
            Ok(freed) => {
                self.images.clear();
                self.note(format!(
                    "Cleared downloaded images: {} freed (they come back when you open those issues)",
                    jdesk_core::storage::human(freed)
                ));
            }
            Err(e) => self.error(format!("Couldn't clear the images: {e}")),
        }
        self.measure_storage(cx);
        cx.notify();
    }

    // ----- attachments ----------------------------------------------------------------------

    /// The image files of `keys` (on screen now: the size cap never removes them).
    fn shown_images(&self, keys: &[String]) -> HashSet<PathBuf> {
        let dir = self.desk.dir.join("attachments");
        keys.iter()
            .filter_map(|k| self.desk.issue(k))
            .flat_map(|i| i.attachments.iter().filter(|a| a.is_image()))
            .map(|a| dir.join(a.local_name()))
            .collect()
    }

    /// Keeps the downloaded images under `max_image_mb`, least recently opened first, in the
    /// background; what goes is forgotten here so it downloads again when next shown.
    pub fn cap_images(&mut self, keep: HashSet<PathBuf>, cx: &mut Context<Self>) {
        let Some(cap) = self.desk.image_cap() else {
            return;
        };
        let dir = self.desk.dir.clone();
        let task = cx
            .background_executor()
            .spawn(async move { jdesk_core::storage::prune_images(&dir, cap, &keep) });
        cx.spawn(async move |this, cx| {
            let removed = task.await;
            if removed.is_empty() {
                return;
            }
            this.update(cx, |m, cx| {
                let gone: HashSet<&PathBuf> = removed.iter().map(|(p, _)| p).collect();
                m.images.retain(
                    |_, st| !matches!(st, ImageState::Ready { path, .. } if gone.contains(path)),
                );
                if m.usage.is_some() {
                    m.measure_storage(cx);
                }
                m.generation += 1;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The Jira client, made once (making one may run `token_command`).
    fn client(&mut self) -> anyhow::Result<Arc<dyn jdesk_core::client::Jira>> {
        if let Some(j) = &self.jira {
            return Ok(j.clone());
        }
        let j = self.desk.jira()?;
        self.jira = Some(j.clone());
        Ok(j)
    }

    /// Makes sure the images attached to `keys` are on disk (`attachments/` in the data
    /// folder), downloading the missing ones in the background, one at a time.
    pub fn ensure_images(&mut self, keys: &[String], cx: &mut Context<Self>) {
        let dir = self.desk.dir.join("attachments");
        let mut wanted = Vec::new();
        for k in keys {
            let Some(issue) = self.desk.issue(k) else {
                continue;
            };
            for a in issue.attachments.iter().filter(|a| a.is_image()) {
                if self.images.contains_key(&a.id) {
                    continue;
                }
                let path = dir.join(a.local_name());
                // Saved only under an image's own extension: the file name is Jira's, and a
                // click opens the saved file with whatever app its extension names.
                if image::ImageFormat::from_path(&path).is_err() {
                    self.images.insert(
                        a.id.clone(),
                        ImageState::Failed("not an image file name".into()),
                    );
                    continue;
                }
                if path.exists() {
                    // Opened again: the size cap removes it last.
                    jdesk_core::storage::touch(&path);
                    let state = match image::image_dimensions(&path) {
                        Ok((width, height)) => ImageState::Ready {
                            path,
                            width,
                            height,
                        },
                        Err(e) => ImageState::Failed(e.to_string()),
                    };
                    self.images.insert(a.id.clone(), state);
                } else {
                    self.images.insert(a.id.clone(), ImageState::Loading);
                    wanted.push((a.id.clone(), a.content.clone(), path));
                }
            }
        }
        if wanted.is_empty() {
            return;
        }
        let jira = match self.client() {
            Ok(j) => j,
            Err(e) => {
                for (id, _, _) in wanted {
                    self.images.insert(id, ImageState::Failed(e.to_string()));
                }
                return;
            }
        };
        let keep = self.shown_images(keys);
        cx.spawn(async move |this, cx| {
            for (id, url, path) in wanted {
                let jira = jira.clone();
                let state = cx
                    .background_executor()
                    .spawn(async move {
                        let fetched = jira.download(&url).and_then(|bytes| {
                            jdesk_core::write_atomic(&path, &bytes)?;
                            image::image_dimensions(&path).map_err(|e| {
                                // Not an image after all: don't keep it.
                                let _ = std::fs::remove_file(&path);
                                e.into()
                            })
                        });
                        match fetched {
                            Ok((width, height)) => ImageState::Ready {
                                path,
                                width,
                                height,
                            },
                            Err(e) => ImageState::Failed(format!("{e:#}")),
                        }
                    })
                    .await;
                if this
                    .update(cx, |m, cx| {
                        m.images.insert(id, state);
                        m.generation += 1;
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
            this.update(cx, |m, cx| m.cap_images(keep, cx)).ok();
        })
        .detach();
        self.generation += 1;
        cx.notify();
    }

    // ----- sync and apply -------------------------------------------------------------------

    pub fn start_sync(&mut self, mode: sync::Mode, cx: &mut Context<Self>) {
        if self.sync.running || self.sync.applying {
            return;
        }
        let job = match self.desk.sync_job() {
            Ok(j) => j,
            Err(e) => {
                self.sync.error = Some(e.to_string());
                self.error(format!("Can't refresh: {e}"));
                cx.notify();
                return;
            }
        };
        self.sync.running = true;
        *self.sync.progress.lock().expect("progress") = match mode {
            sync::Mode::Full => "Full refresh".into(),
            sync::Mode::Auto => "Refreshing".into(),
        };
        let progress = self.sync.progress.clone();
        let task = cx.background_executor().spawn(async move {
            job.run(mode, &|p| {
                if let Ok(mut s) = progress.lock() {
                    *s = p;
                }
            })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |m, cx| m.finish_sync(result, cx)).ok();
        })
        .detach();
        // Redraw the progress text while it runs.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(250))
                    .await;
                let running = this
                    .update(cx, |m, cx| {
                        cx.notify();
                        m.sync.running || m.sync.applying
                    })
                    .unwrap_or(false);
                if !running {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn finish_sync(&mut self, result: anyhow::Result<SyncOutcome>, cx: &mut Context<Self>) {
        self.sync.running = false;
        match result {
            Ok(out) => {
                let r = &out.report;
                let summary = format!(
                    "{}{} changed{} · {} ms",
                    if r.full { "Full · " } else { "" },
                    r.upserted.len(),
                    if r.removed.is_empty() {
                        String::new()
                    } else {
                        format!(", {} removed", r.removed.len())
                    },
                    r.millis
                );
                let first = self.desk.issues().is_empty();
                let before: Vec<String> = self
                    .desk
                    .identities
                    .open_reviews()
                    .map(|r| r.key.clone())
                    .collect();
                self.desk.merge(out);
                let new_reviews: Vec<String> = self
                    .desk
                    .identities
                    .open_reviews()
                    .map(|r| r.key.clone())
                    .filter(|k| !before.contains(k))
                    .collect();
                self.sync.error = None;
                self.sync.last_result = Some(summary);
                if first
                    && self.filter.sprint == SprintSel::Active
                    && !self
                        .desk
                        .sprints()
                        .iter()
                        .any(|s| s.state == SprintState::Active)
                {
                    self.filter.sprint = SprintSel::All;
                }
                self.rebuild();
                // What sync found about requests: seen in Jira, or changed and waiting for you.
                let news = std::mem::take(&mut self.desk.request_news);
                if !news.is_empty() {
                    self.note(if news.len() == 1 {
                        news[0].clone()
                    } else {
                        format!("{} requests moved: {}", news.len(), news.join(" · "))
                    });
                }
                if !new_reviews.is_empty() {
                    self.error(format!(
                        "{} changed a lot in Jira: your changes to {} are held until you review (v)",
                        new_reviews.join(", "),
                        if new_reviews.len() == 1 { "it" } else { "them" }
                    ));
                }
            }
            Err(e) => {
                let msg = format!("{e:#}");
                self.sync.error = Some(msg.clone());
                self.error(format!("Refresh failed: {msg}"));
            }
        }
        if self.usage.is_some() {
            self.measure_storage(cx);
        }
        // A lowered max_image_mb takes effect here too, not only after the next download.
        let keep = self.shown_images(&self.current_key().into_iter().collect::<Vec<_>>());
        self.cap_images(keep, cx);
        cx.notify();
    }

    /// Whether a background refresh is due.
    pub fn refresh_due(&self) -> bool {
        let every = self.desk.refresh_minutes();
        if every == 0 || self.sync.running || self.sync.applying {
            return false;
        }
        self.desk
            .last_sync
            .is_none_or(|t| jdesk_core::now() - t >= (every * 60) as i64)
    }

    pub fn start_apply(&mut self, only: Option<Vec<u64>>, cx: &mut Context<Self>) {
        if self.sync.applying || self.sync.running {
            self.error("Wait for the refresh to finish");
            return;
        }
        let pending = self
            .desk
            .queue
            .changes()
            .iter()
            .filter(|c| {
                c.state == jdesk_core::queue::State::Pending
                    && only.as_ref().is_none_or(|ids| ids.contains(&c.id))
            })
            .count();
        if pending == 0 {
            self.note("Nothing to apply");
            return;
        }
        let jira = match self.desk.jira() {
            Ok(j) => j,
            Err(e) => {
                self.error(format!("Can't apply: {e}"));
                return;
            }
        };
        let map = self.desk.field_map.clone();
        let guard = self.desk.apply_guard();
        let mut queue: Queue = self.desk.queue.clone();
        self.sync.applying = true;
        *self.sync.progress.lock().expect("progress") = format!("Applying {pending} changes");
        let progress = self.sync.progress.clone();
        let task = cx.background_executor().spawn(async move {
            let r = queue.apply(
                jira.as_ref(),
                &map,
                only.as_deref(),
                jdesk_core::now(),
                &guard,
                &|p| {
                    if let Ok(mut s) = progress.lock() {
                        *s = p;
                    }
                },
            );
            (queue, r)
        });
        cx.spawn(async move |this, cx| {
            let (queue, result) = task.await;
            this.update(cx, |m, cx| m.finish_apply(queue, result, cx))
                .ok();
        })
        .detach();
        cx.notify();
    }

    fn finish_apply(
        &mut self,
        queue: Queue,
        result: anyhow::Result<ApplyReport>,
        cx: &mut Context<Self>,
    ) {
        self.sync.applying = false;
        self.desk.queue = queue;
        match result {
            Ok(r) => {
                if let Err(e) = self.desk.take_apply_report(&r) {
                    self.error(format!("A review could not be opened: {e}"));
                }
                let held = r.held.len();
                let raised: Vec<String> = r.raised.iter().map(|(i, _)| i.key.clone()).collect();
                let mut read = r.refreshed;
                read.extend(r.also_read);
                if let Err(e) = self.desk.store_refreshed(read) {
                    self.error(format!("Applied, but the cache could not be updated: {e}"));
                }
                let mut parts = vec![format!("{} applied", r.applied)];
                if r.conflicts > 0 {
                    parts.push(format!(
                        "{} conflict{} (r to retry, ⇧R to overwrite)",
                        r.conflicts,
                        if r.conflicts == 1 { "" } else { "s" }
                    ));
                }
                if r.failed > 0 {
                    parts.push(format!("{} failed", r.failed));
                }
                if !raised.is_empty() {
                    parts.push(format!(
                        "{} changed a lot in Jira, held for review (v): {}",
                        raised.join(", "),
                        if raised.len() == 1 {
                            "check it"
                        } else {
                            "check them"
                        }
                    ));
                } else if held > 0 {
                    parts.push(format!("{held} issue(s) held for review (v)"));
                }
                let text = parts.join(" · ");
                self.rebuild();
                if r.conflicts + r.failed > 0 || !raised.is_empty() {
                    self.error(text);
                } else {
                    self.note(text);
                }
            }
            Err(e) => {
                self.rebuild();
                self.error(format!("Apply stopped: {e:#}"));
            }
        }
        cx.notify();
    }
}

fn toggle(set: &mut std::collections::BTreeSet<String>, v: &str) {
    if !set.remove(v) {
        set.insert(v.to_string());
    }
}

/// "5m ago", "3h ago", "Nov 18" from a Jira timestamp.
pub fn ago(stamp: &str) -> String {
    let fmt = time::macros::format_description!(
        "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond][offset_hour sign:mandatory][offset_minute]"
    );
    let Ok(t) = time::OffsetDateTime::parse(stamp, &fmt) else {
        return stamp.get(..10).unwrap_or(stamp).to_string();
    };
    let secs = jdesk_core::now() - t.unix_timestamp();
    match secs {
        s if s < 60 => "just now".into(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s if s < 7 * 86_400 => format!("{}d ago", s / 86_400),
        _ => format!("{} {}", month(t.month() as u8), t.day()),
    }
}

/// "Nov 18" from a date or timestamp.
pub fn short_date(stamp: &str) -> String {
    let d = stamp.get(..10).unwrap_or(stamp);
    let parts: Vec<&str> = d.split('-').collect();
    match parts.as_slice() {
        [_, m, day] => format!(
            "{} {}",
            month(m.parse().unwrap_or(0)),
            day.trim_start_matches('0')
        ),
        _ => d.to_string(),
    }
}

fn month(m: u8) -> &'static str {
    [
        "?", "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .get(m as usize)
    .copied()
    .unwrap_or("?")
}

/// Seconds since a unix time, said briefly.
pub fn since(unix: i64) -> String {
    match jdesk_core::now() - unix {
        s if s < 60 => "just now".into(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86_400),
    }
}

#[cfg(test)]
mod points_tests {
    use super::points_total;

    #[test]
    fn totals_read_as_points() {
        assert_eq!(points_total(0.), "");
        assert_eq!(points_total(42.), "42 pts");
        assert_eq!(points_total(12.5), "12.5 pts");
    }
}

#[cfg(test)]
mod prefs_tests {
    use super::Prefs;

    #[test]
    fn a_choice_is_saved_and_read_back() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            Prefs::load(d.path()).auto_advance,
            None,
            "unset until chosen"
        );
        let p = Prefs {
            auto_advance: Some(true),
            ..Default::default()
        };
        p.save(d.path()).unwrap();
        assert_eq!(Prefs::load(d.path()).auto_advance, Some(true));
        let mut p = Prefs::load(d.path());
        p.sprints_shown.insert(41, true);
        p.sprints_shown.insert(45, false);
        p.save(d.path()).unwrap();
        let back = Prefs::load(d.path());
        assert_eq!(back.sprints_shown.get(&41), Some(&true));
        assert_eq!(back.sprints_shown.get(&45), Some(&false));
        assert_eq!(
            back.auto_advance,
            Some(true),
            "one choice doesn't drop another"
        );
        std::fs::write(d.path().join("prefs.json"), "not json").unwrap();
        assert_eq!(
            Prefs::load(d.path()),
            Prefs::default(),
            "a broken file is ignored"
        );
    }
}

#[cfg(test)]
mod sprint_range_tests {
    use super::sprint_range;

    #[test]
    fn a_range_runs_either_way_in_pane_order() {
        // Pane order: active, then future by date, then closed.
        let order = [43, 44, 45, 42, 41];
        assert_eq!(sprint_range(&order, 43, 45), Some([43, 44, 45].into()));
        assert_eq!(sprint_range(&order, 45, 43), Some([43, 44, 45].into()));
        assert_eq!(sprint_range(&order, 44, 44), Some([44].into()));
        assert_eq!(sprint_range(&order, 44, 41), Some([44, 45, 42, 41].into()));
        assert_eq!(
            sprint_range(&order, 99, 44),
            None,
            "an anchor no longer shown"
        );
    }
}
