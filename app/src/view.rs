//! The window: Chorong's layout shell with Jira Desk's panes inside, a status bar, and the
//! overlays (pickers, text prompts, help). Keys arrive here as actions (see
//! `keymap/default.json`) and act on the focused pane; typed text goes to whichever field is
//! open.

use crate::actions::*;
use crate::assets::icon;
use crate::input::{Edited, TextArea};
use crate::omni;
use crate::panes::{Bodies, s};
use crate::picker::{Item, NEW_PREFIX, Picker, Purpose};
use crate::state::{
    self, BOARD, FILTERS, ISSUES, Model, MoreOf, NOTES, PREVIEW, PaneFocus, SETTINGS,
};
use cho_layout::{DockId, Edge, PaneContent, PaneId, Target, Workspace};
use cho_layout_ui::colors::Colors;
use cho_layout_ui::keys;
use cho_layout_ui::shell::{Shell, bind_actions};
use gpui::{
    Action, AnyElement, App, ClipboardItem, Context, Entity, FocusHandle, Focusable, FontWeight,
    InteractiveElement, IntoElement, KeyContext, KeyDownEvent, ParentElement, Render,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};
use jdesk_core::control::{Request, Response};
use jdesk_core::filter::Sort;
use jdesk_core::model::{Person, SprintState};
use jdesk_core::queue::{Edit, State};
use jdesk_core::sync;
use std::rc::Rc;

pub const LAYOUT_KEY: &str = "jira-desk";

/// A text prompt over the window.
pub struct Prompt {
    pub title: String,
    pub hint: String,
    pub area: TextArea,
    pub purpose: PromptFor,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub enum PromptFor {
    /// A request on an issue: its key and kind (description, parent, link, fix-version, other).
    Request(String, String),
    Summary(String),
    Points(String),
    Comment(String),
}

pub enum Overlay {
    Picker(Picker),
    Prompt(Prompt),
    ConfirmApply(usize),
    Help,
    /// One request, whole (a click on its line in the Notes pane).
    Request(u64),
}

pub struct DeskView {
    pub model: Entity<Model>,
    pub shell: Entity<Shell>,
    pub bodies: Rc<Bodies>,
    focus: FocusHandle,
    overlay: Option<Overlay>,
    help_scroll: gpui::ScrollHandle,
    /// The palette's commands while it is open (item ids index this).
    palette: Vec<Box<dyn Action>>,
    /// A command chosen in the palette, run once the palette has closed.
    run_next: Option<Box<dyn Action>>,
    shift_taps: crate::double_tap::ShiftTaps,
    /// Issue tabs a focus series opened, closed again when it ends.
    series_opened: Vec<PaneId>,
    /// A Board focus mode opened (⌥6, or added to a page when there was none): closed when
    /// focus mode ends, as a series closes its tabs.
    board_opened: Option<PaneId>,
    /// The layout from before the Board was added for focus mode, put back when it ends (so
    /// the panes get their widths back, not just the Board's neighbour).
    board_restore: Option<Workspace>,
    /// The chord being typed, for its hint card (see `chord`).
    chord: crate::chord::Chord,
    /// Whether a shell picker was open at the last redraw: the key context depends on it.
    shell_overlay: bool,
    /// The focus layout's page, page count and whether the tray has anything, at the last
    /// redraw: the status bar's hint shows them.
    focus_shape: Option<(usize, usize, bool)>,
    /// The list's selection at the last model change, to notice it moving.
    last_selected: Option<String>,
}

/// The arrangement on first launch: Filters on the left, the list and the selected issue
/// side by side, Notes on the right, Changes along the bottom.
pub fn default_workspace() -> Workspace {
    let mut ws = Workspace::new();
    let preview = ws
        .open(preview_content(), None)
        .expect("an empty workspace takes the issue pane");
    let group = ws.group_of(preview).expect("issue pane placed");
    let issues = ws
        .open(
            placeholder(ISSUES),
            Some(Target::Beside {
                group,
                edge: Edge::Left,
            }),
        )
        .expect("the list goes beside it");
    let _ = ws.open(placeholder(FILTERS), Some(Target::Dock(DockId::Left)));
    let _ = ws.open(placeholder(NOTES), Some(Target::Dock(DockId::Right)));
    let _ = ws.open(PaneContent::Changes, Some(Target::Dock(DockId::Bottom)));
    let _ = ws.resize_dock(DockId::Bottom, 150.);
    let _ = ws.resize_dock(DockId::Left, 250.);
    let _ = ws.resize_dock(DockId::Right, 300.);
    // The list gets the larger share of the editor area.
    if let Some(cho_layout::Node::Split(split)) = &ws.arrangement().editor {
        let id = split.id;
        let _ = ws.resize_split(id, 0, 0.12);
    }
    let _ = ws.activate(issues);
    ws
}

/// The Board's row in the add-to-page picker (not an issue key).
const BOARD_ITEM: &str = "\u{0}board";

/// The Preview pane: an editor-kind pane (so issues opened in tabs can join its group) that
/// shows the list's selection.
pub fn preview_content() -> PaneContent {
    PaneContent::Editor {
        path: PREVIEW.into(),
        caret: 0,
    }
}

fn placeholder(name: &str) -> PaneContent {
    PaneContent::Placeholder { name: name.into() }
}

/// The saved layout, if it is one of ours; else the default.
pub fn load_workspace(dir: &std::path::Path) -> Workspace {
    let file = dir.join(cho_layout::layout_file_name(LAYOUT_KEY));
    if !file.exists() {
        return default_workspace();
    }
    migrate_layout(&file);
    let loaded = cho_layout::load(dir, LAYOUT_KEY);
    let ws = loaded.workspace;
    let ours = ws
        .panes()
        .values()
        .any(|c| PaneFocus::of(Some(c)) == PaneFocus::Issues);
    if ours { ws } else { default_workspace() }
}

/// Renames the List and Preview panes in a layout saved before they had those names
/// ("Issues" and "Issue"), wherever the layout names them: the panes, and the copies kept for
/// Reopen focus layout. The file is rewritten only when something changed.
fn migrate_layout(file: &std::path::Path) {
    let Ok(text) = std::fs::read_to_string(file) else {
        return;
    };
    let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return;
    };
    if rename_old_panes(&mut json)
        && let Ok(out) = serde_json::to_string_pretty(&json)
    {
        let _ = jdesk_core::write_atomic(file, out.as_bytes());
    }
}

/// The work of [`migrate_layout`] on the parsed layout. True when anything was renamed.
fn rename_old_panes(v: &mut serde_json::Value) -> bool {
    use serde_json::Value;
    match v {
        Value::Object(o) => {
            let kind = o.get("kind").and_then(Value::as_str).map(str::to_string);
            let mut changed = false;
            let mut set = |field: &str, old: &str, new: &str| {
                if o.get(field).and_then(Value::as_str) == Some(old) {
                    o.insert(field.into(), Value::String(new.into()));
                    changed = true;
                }
            };
            match kind.as_deref() {
                Some("placeholder") => set("name", state::OLD_ISSUES, ISSUES),
                Some("editor") => set("path", state::OLD_PREVIEW, PREVIEW),
                _ => {}
            }
            for child in o.values_mut() {
                changed |= rename_old_panes(child);
            }
            changed
        }
        Value::Array(a) => a.iter_mut().fold(false, |c, x| rename_old_panes(x) | c),
        _ => false,
    }
}

impl DeskView {
    pub fn new(
        model: Entity<Model>,
        shell: Entity<Shell>,
        bodies: Rc<Bodies>,
        focus: FocusHandle,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&model, |this, _, cx| this.model_changed(cx))
            .detach();
        cx.observe(&shell, |this, _, cx| this.shell_changed(cx))
            .detach();
        // Background refresh: check every 30 s whether one is due.
        let m = model.clone();
        cx.spawn(async move |_, cx| {
            loop {
                let _ = m.update(cx, |m, cx| {
                    if m.refresh_due() {
                        m.start_sync(sync::Mode::Auto, cx);
                    }
                });
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(30))
                    .await;
            }
        })
        .detach();
        let mut v = Self {
            model,
            shell,
            bodies,
            focus,
            overlay: None,
            help_scroll: gpui::ScrollHandle::new(),
            palette: Vec::new(),
            run_next: None,
            shift_taps: Default::default(),
            series_opened: Vec::new(),
            board_opened: None,
            board_restore: None,
            chord: Default::default(),
            shell_overlay: false,
            focus_shape: None,
            last_selected: None,
        };
        v.shell_changed(cx);
        v
    }

    fn model_changed(&mut self, cx: &mut Context<Self>) {
        let (open, more, preview, request) = self.model.update(cx, |m, _| {
            (
                m.pending_open.take(),
                m.pending_more.take(),
                std::mem::take(&mut m.pending_preview),
                m.pending_request.take(),
            )
        });
        if let Some(id) = request {
            self.overlay = Some(Overlay::Request(id));
            cx.notify();
        }
        if preview {
            self.preview_beside_board(cx);
        }
        if let Some(k) = open {
            self.open_issue_tab(&k, cx);
        }
        if let Some(which) = more {
            match which {
                MoreOf::Sprints => self.open_picker(Purpose::FilterSprint, cx),
                MoreOf::Labels => self.open_picker(Purpose::FilterLabel, cx),
            }
        }
        // Notes follows the current issue, except while it (or Filters) has focus.
        self.model.update(cx, |m, _| m.follow_for_notes());
        let selected = self.model.read(cx).selected.clone();
        if selected != self.last_selected {
            self.last_selected = selected;
            self.show_issue_pane_tab(cx);
        }
        self.fetch_images(cx);
        self.shell.update(cx, |s, cx| {
            s.refresh_all_panes(cx);
            cx.notify();
        });
        cx.notify();
    }

    /// Downloads the images of the issues on screen: the selection and every open issue tab.
    fn fetch_images(&mut self, cx: &mut Context<Self>) {
        let mut keys: Vec<String> = self
            .shell
            .read(cx)
            .workspace()
            .panes()
            .values()
            .filter_map(|c| match PaneFocus::of(Some(c)) {
                PaneFocus::Issue(k) => Some(k),
                _ => None,
            })
            .collect();
        let m = self.model.read(cx);
        keys.extend(m.selected.clone());
        keys.extend(m.current_key());
        let missing = keys.iter().any(|k| {
            m.desk.issue(k).is_some_and(|i| {
                i.attachments
                    .iter()
                    .any(|a| a.is_image() && !m.images.contains_key(&a.id))
            })
        });
        if missing {
            self.model.update(cx, |m, cx| m.ensure_images(&keys, cx));
        }
    }

    fn shell_changed(&mut self, cx: &mut Context<Self>) {
        let focus = {
            let ws = self.shell.read(cx).workspace();
            PaneFocus::of(ws.focused().and_then(|p| ws.content(p)))
        };
        // A shell picker opening or closing changes the key context (see `render`).
        let overlay = self.shell.read(cx).overlay_open();
        let shape = {
            let ws = self.shell.read(cx).workspace();
            ws.focus()
                .map(|f| (f.current, f.pages.len(), !ws.tray().is_empty()))
        };
        if overlay != self.shell_overlay || shape != self.focus_shape {
            self.shell_overlay = overlay;
            self.focus_shape = shape;
            cx.notify();
        }
        let changed = self.model.read(cx).focus != focus;
        if changed {
            self.model.update(cx, |m, cx| {
                m.focus = focus;
                m.follow_for_notes();
                m.generation += 1;
                cx.notify();
            });
        }
        // The focus series ended (Esc, or its last page closed): tidy away the tabs it opened.
        let in_focus = self.shell.read(cx).workspace().in_focus();
        // A Board that focus mode opened closes with it, rather than staying in the layout.
        if !in_focus && let Some(board) = self.board_opened.take() {
            let restore = self.board_restore.take();
            self.shell.update(cx, |sh, cx| {
                let _ = sh.update_workspace(cx, |ws| {
                    match restore {
                        // The layout as it was: every pane's width back, the Board gone.
                        Some(before) => *ws = before,
                        None if ws.content(board).is_some() => ws.close(board)?,
                        None => {}
                    }
                    Ok(())
                });
            });
        }
        if !in_focus && !self.model.read(cx).series.is_empty() {
            self.model.update(cx, |m, cx| {
                m.series.clear();
                // The series' own message ("Esc ends") would otherwise stay up after it ended.
                m.note("Focus series ended: its tabs are closed");
                cx.notify();
            });
            let opened = std::mem::take(&mut self.series_opened);
            self.shell.update(cx, |sh, cx| {
                let _ = sh.update_workspace(cx, |ws| {
                    for p in opened {
                        if ws.content(p).is_some() {
                            let _ = ws.close(p);
                        }
                    }
                    Ok(())
                });
            });
        }
    }

    /// Opens the picked issues (or, with none picked, the listed ones, up to 40) as a focus
    /// series: one issue per focus page, as Chorong opens a tour's steps. `]` and `[` step
    /// through the pages; Tab and Shift-Tab bring another issue from the tray into the focused
    /// spot; Esc goes back and closes the tabs the series opened.
    fn open_series(&mut self, cx: &mut Context<Self>) {
        const MAX: usize = 40;
        let (mut keys, from_list) = {
            let m = self.model.read(cx);
            if m.picked.is_empty() {
                (
                    m.visible
                        .iter()
                        .map(|i| m.effective[*i].key.clone())
                        .collect::<Vec<_>>(),
                    true,
                )
            } else {
                (m.picked.clone(), false)
            }
        };
        let total = keys.len();
        keys.truncate(MAX);
        if keys.is_empty() {
            self.model.update(cx, |m, cx| {
                m.error("Nothing to open: pick issues with x, or list some");
                cx.notify();
            });
            return;
        }
        let preview = self.find_pane(cx, |c| PaneFocus::of(Some(c)) == PaneFocus::Preview);
        let now = jdesk_core::now() as u64;
        let mut opened: Vec<PaneId> = Vec::new();
        let result = self.shell.update(cx, |sh, cx| {
            sh.update_workspace(cx, |ws| {
                if ws.in_focus() {
                    ws.restore(now)?;
                }
                // Each issue as a tab beside the Preview pane (an existing tab is reused).
                let mut panes = Vec::new();
                for k in &keys {
                    let existing = ws
                        .panes()
                        .iter()
                        .find(|(_, c)| matches!(c, PaneContent::Editor { path, .. } if path == k))
                        .map(|(id, _)| *id);
                    let pane = match existing {
                        Some(p) => p,
                        None => {
                            let target =
                                preview
                                    .and_then(|p| ws.group_of(p))
                                    .map(|group| Target::Tabs {
                                        group,
                                        index: usize::MAX,
                                    });
                            let p = ws.open(
                                PaneContent::Editor {
                                    path: k.clone(),
                                    caret: 0,
                                },
                                target,
                            )?;
                            opened.push(p);
                            p
                        }
                    };
                    panes.push(pane);
                }
                // Page 1 is the first issue maximised; each other issue gets a page of its own,
                // declared like a tour step so it can be reset (⌘K R) to just that issue.
                ws.maximize(panes[0], now)?;
                for k in keys.iter().skip(1) {
                    let i = ws.new_page(Some(cho_layout::PageOrigin {
                        tour_step: Some(k.clone()),
                        template: None,
                        panes: vec![cho_layout::PaneSource::Editor { path: k.clone() }],
                        arrangement: cho_layout::PageArrangement::Single,
                    }))?;
                    ws.reset_page(i, |_| true)?;
                }
                ws.go_to_page(0)?;
                ws.activate(panes[0])
            })
        });
        self.series_opened = opened;
        match result {
            Ok(()) => self.model.update(cx, |m, cx| {
                m.series = keys.clone();
                m.note(format!(
                    "Focus series: {} issue{}{} · ] and [ step · Tab brings one from the tray · Esc ends",
                    keys.len(),
                    if keys.len() == 1 { "" } else { "s" },
                    if from_list && total > MAX {
                        format!(" (the first {MAX} of {total} listed; pick with x for others)")
                    } else {
                        String::new()
                    }
                ));
                cx.notify();
            }),
            Err(e) => self.model.update(cx, |m, cx| {
                m.error(format!("Can't open the series: {e}"));
                cx.notify();
            }),
        }
        self.shell_changed(cx);
    }

    // ----- panes ----------------------------------------------------------------------------

    fn find_pane(&self, cx: &App, f: impl Fn(&PaneContent) -> bool) -> Option<PaneId> {
        self.shell
            .read(cx)
            .workspace()
            .panes()
            .iter()
            .find(|(_, c)| f(c))
            .map(|(id, _)| *id)
    }

    /// Focuses a pane, showing its dock if hidden and opening it if it was closed.
    fn focus_pane(&mut self, content: PaneContent, cx: &mut Context<Self>) {
        let existing = self.find_pane(cx, |c| {
            PaneFocus::of(Some(c)) == PaneFocus::of(Some(&content))
        });
        self.shell.update(cx, |sh, cx| {
            let _ = sh.update_workspace(cx, |ws| {
                let pane = match existing {
                    Some(p) => p,
                    None => {
                        let target = match &content {
                            PaneContent::Placeholder { name } if name == FILTERS => {
                                Target::Dock(DockId::Left)
                            }
                            PaneContent::Placeholder { name }
                                if name == NOTES || name == SETTINGS =>
                            {
                                Target::Dock(DockId::Right)
                            }
                            PaneContent::Changes => Target::Dock(DockId::Bottom),
                            _ => Target::EditorArea,
                        };
                        ws.open(content.clone(), Some(target))?
                    }
                };
                // In a focus layout, a pane that isn't on the page comes onto it.
                if ws.in_focus() && ws.group_of(pane).is_none() {
                    let current = ws.focus().map_or(0, |f| f.current);
                    ws.move_pane_to_page(pane, current)?;
                }
                if ws.activate(pane).is_err() {
                    // In a hidden dock: show the docks, then try again.
                    for d in [DockId::Left, DockId::Right, DockId::Bottom] {
                        if ws.activate(pane).is_ok() {
                            break;
                        }
                        let _ = ws.toggle_dock(d);
                        if ws.activate(pane).is_err() {
                            let _ = ws.toggle_dock(d);
                        }
                    }
                    ws.activate(pane)?;
                }
                Ok(())
            });
        });
        self.shell_changed(cx);
    }

    /// Shows the board maximised (Esc, or the shell's restore, goes back to the panes).
    fn open_board(&mut self, cx: &mut Context<Self>) {
        let existed = self.find_pane(cx, |c| *c == placeholder(BOARD)).is_some();
        let before = (!existed && !self.shell.read(cx).workspace().in_focus())
            .then(|| self.shell.read(cx).workspace().clone());
        self.focus_pane(placeholder(BOARD), cx);
        if !existed && let Some(p) = self.find_pane(cx, |c| *c == placeholder(BOARD)) {
            let now = jdesk_core::now() as u64;
            self.shell.update(cx, |sh, cx| {
                let _ = sh.update_workspace(cx, |ws| ws.toggle_maximize(p, now).map(|_| ()));
            });
            if self.shell.read(cx).workspace().in_focus() {
                self.board_opened = Some(p);
                self.board_restore = before;
            }
            self.shell_changed(cx);
        }
    }

    /// ⌘Enter / ⌘⇧Enter: the issue in a new pane at `edge` of the issue panes (the Preview's
    /// group, else the focused one), as Chorong opens a file beside. An issue that already has
    /// a pane is shown there instead.
    fn open_issue_at(&mut self, key: &str, edge: Edge, cx: &mut Context<Self>) {
        let existing = self.find_pane(
            cx,
            |c| matches!(c, PaneContent::Editor { path, .. } if path == key),
        );
        let preview = self.find_pane(cx, |c| PaneFocus::of(Some(c)) == PaneFocus::Preview);
        self.shell.update(cx, |sh, cx| {
            let _ = sh.update_workspace(cx, |ws| {
                if let Some(p) = existing {
                    ws.reveal(p)?;
                    return ws.activate(p);
                }
                let group = preview
                    .and_then(|p| ws.group_of(p))
                    .or_else(|| ws.focused().and_then(|p| ws.group_of(p)));
                let content = PaneContent::Editor {
                    path: key.to_string(),
                    caret: 0,
                };
                let pane = ws.open(content, group.map(|group| Target::Beside { group, edge }))?;
                if ws.in_focus() && ws.group_of(pane).is_none() {
                    ws.place_on_empty_page(pane)?;
                }
                ws.reveal(pane)?;
                ws.activate(pane)
            });
        });
        self.shell_changed(cx);
    }

    /// Opens an issue in its own tab, beside the Preview pane (or focuses the tab it has).
    fn open_issue_tab(&mut self, key: &str, cx: &mut Context<Self>) {
        let existing = self.find_pane(
            cx,
            |c| matches!(c, PaneContent::Editor { path, .. } if path == key),
        );
        let preview = self.find_pane(cx, |c| PaneFocus::of(Some(c)) == PaneFocus::Preview);
        self.shell.update(cx, |sh, cx| {
            let r = sh.update_workspace(cx, |ws| {
                let pane = match existing {
                    Some(p) => p,
                    None => {
                        let content = PaneContent::Editor {
                            path: key.to_string(),
                            caret: 0,
                        };
                        // Beside the Preview pane; in a focus layout (a maximised pane) that may
                        // be off screen, so then with the focused pane, or beside it.
                        let focused_group = ws.focused().and_then(|p| ws.group_of(p));
                        let mut targets: Vec<Option<Target>> = Vec::new();
                        if let Some(group) = preview.and_then(|p| ws.group_of(p)) {
                            targets.push(Some(Target::Tabs {
                                group,
                                index: usize::MAX,
                            }));
                        }
                        if let Some(group) = focused_group {
                            targets.push(Some(Target::Tabs {
                                group,
                                index: usize::MAX,
                            }));
                            targets.push(Some(Target::Beside {
                                group,
                                edge: Edge::Right,
                            }));
                        }
                        let mut opened = None;
                        for t in targets {
                            if let Ok(p) = ws.open(content.clone(), t) {
                                if ws.group_of(p).is_some() {
                                    opened = Some(p);
                                    break;
                                }
                                // Placed off screen: take it back and try the next spot.
                                let _ = ws.close(p);
                            }
                        }
                        match opened {
                            Some(p) => p,
                            None => ws.open(content, None)?,
                        }
                    }
                };
                ws.activate(pane)
            });
            if let Err(e) = r {
                let _ = e;
            }
        });
        self.shell_changed(cx);
    }

    /// A board card was clicked: the Preview pane (which shows it) comes beside the board (from
    /// the tray, in a focus layout) and the board keeps the keyboard.
    fn preview_beside_board(&mut self, cx: &mut Context<Self>) {
        let preview = self.find_pane(cx, |c| PaneFocus::of(Some(c)) == PaneFocus::Preview);
        let board = self.find_pane(cx, |c| *c == placeholder(BOARD));
        let (Some(preview), Some(board)) = (preview, board) else {
            return;
        };
        self.shell.update(cx, |sh, cx| {
            let _ = sh.update_workspace(cx, |ws| {
                ws.reveal(preview)?;
                ws.activate(preview)?;
                ws.activate(board)
            });
        });
        self.shell_changed(cx);
    }

    /// The list's selection moved: if an issue tab is in front of the Preview pane in its group,
    /// bring the Preview pane forward so the selection shows. The keyboard stays where it was.
    fn show_issue_pane_tab(&mut self, cx: &mut Context<Self>) {
        let Some(preview) = self.find_pane(cx, |c| PaneFocus::of(Some(c)) == PaneFocus::Preview)
        else {
            return;
        };
        let hidden = {
            let ws = self.shell.read(cx).workspace();
            ws.group_of(preview)
                .and_then(|g| ws.group(g))
                .is_some_and(|g| g.tabs.get(g.active) != Some(&preview))
        };
        if !hidden {
            return;
        }
        self.shell.update(cx, |sh, cx| {
            let _ = sh.update_workspace(cx, |ws| {
                let back = ws.focused();
                ws.activate(preview)?;
                match back {
                    Some(p) if p != preview => ws.activate(p),
                    _ => Ok(()),
                }
            });
        });
    }

    /// Go to issue… with an issue tab focused: the tab shows `key` instead (as Chorong's file
    /// picker replaces what the active code pane shows). If `key` already has a tab, that tab
    /// is shown instead. False when no issue tab is focused.
    fn swap_focused_issue_tab(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let Some(pane) = self.shell.read(cx).workspace().focused() else {
            return false;
        };
        let is_tab = matches!(
            PaneFocus::of(self.shell.read(cx).workspace().content(pane)),
            PaneFocus::Issue(_)
        );
        if !is_tab {
            return false;
        }
        let existing = self.find_pane(
            cx,
            |c| matches!(c, PaneContent::Editor { path, .. } if path == key),
        );
        self.shell.update(cx, |sh, cx| {
            let _ = sh.update_workspace(cx, |ws| match existing {
                Some(p) => ws.reveal(p).and_then(|_| ws.activate(p)),
                None => ws.set_editor_position(pane, key, 0),
            });
            if existing.is_none() {
                sh.refresh_pane(pane, cx);
            }
        });
        if existing.is_none() {
            let h = self.bodies.scroll(pane);
            h.set_offset(gpui::point(px(0.), px(0.)));
        }
        self.shell_changed(cx);
        true
    }

    /// Building a focus page (as Chorong adds files): `key` goes on the current page at `edge`
    /// of the focused pane. An issue already open in the tray comes back from there; one
    /// already on the page is just focused. On an empty page it becomes the page's only pane.
    ///
    /// `key` may also be [`BOARD_ITEM`]: the Board, from the tray, or opened if it was closed.
    fn add_issue_to_page(&mut self, key: &str, edge: Edge, cx: &mut Context<Self>) {
        let board = key == BOARD_ITEM;
        let existing = self.find_pane(cx, |c| {
            if board {
                *c == placeholder(BOARD)
            } else {
                matches!(c, PaneContent::Editor { path, .. } if path == key)
            }
        });
        let name = if board { "the Board" } else { key };
        let r = self.shell.update(cx, |sh, cx| {
            sh.update_workspace(cx, |ws| {
                if let Some(p) = existing {
                    if ws.tray().contains(&p) {
                        ws.bring_from_tray(p, edge)?;
                    }
                    return ws.reveal(p);
                }
                let target = ws
                    .focused()
                    .and_then(|p| ws.group_of(p))
                    .map(|group| Target::Beside { group, edge });
                let content = if board {
                    placeholder(BOARD)
                } else {
                    PaneContent::Editor {
                        path: key.to_string(),
                        caret: 0,
                    }
                };
                let pane = ws.open(content, target)?;
                if ws.in_focus() && ws.group_of(pane).is_none() {
                    ws.place_on_empty_page(pane)?;
                }
                ws.reveal(pane)
            })
        });
        if board && existing.is_none() && r.is_ok() && self.shell.read(cx).workspace().in_focus() {
            self.board_opened = self.find_pane(cx, |c| *c == placeholder(BOARD));
        }
        self.model.update(cx, |m, cx| {
            match r {
                Ok(()) => m.note(format!("Added {name} to the page")),
                Err(e) => m.error(format!("Couldn't add {name} to the page: {e}")),
            }
            cx.notify();
        });
        self.shell_changed(cx);
    }

    // ----- overlays -------------------------------------------------------------------------

    fn typing(&self, cx: &App) -> Option<&'static str> {
        match &self.overlay {
            Some(Overlay::Prompt(p)) if p.area.multiline => Some("JiraText"),
            Some(_) => Some("JiraInput"),
            None => {
                let m = self.model.read(cx);
                if m.note_edit.is_some() {
                    Some("JiraText")
                } else if m.search.is_some() {
                    Some("JiraSearch")
                } else {
                    None
                }
            }
        }
    }

    fn open_picker(&mut self, purpose: Purpose, cx: &mut Context<Self>) {
        // Adding to a focus page: the Board is offered too, unless it is on the page already.
        let board_row = purpose == Purpose::AddToPage && {
            let ws = self.shell.read(cx).workspace();
            ws.panes()
                .iter()
                .find(|(_, c)| **c == placeholder(BOARD))
                .is_none_or(|(p, _)| ws.group_of(*p).is_none())
        };
        let m = self.model.read(cx);
        let mut start: Option<String> = None;
        let (title, items): (String, Vec<Item>) = match &purpose {
            Purpose::SprintsShown => {
                let shown = m.sprints_in_filters();
                (
                    "Sprints in the Filters pane (Enter shows or hides one; Esc closes)".into(),
                    m.desk
                        .sprints()
                        .into_iter()
                        .map(|sp| Item {
                            id: sp.id.to_string(),
                            label: sp.name.clone(),
                            detail: format!(
                                "{} · {} issues",
                                match sp.state {
                                    SprintState::Active => "active",
                                    SprintState::Future => "next",
                                    SprintState::Closed => "closed",
                                },
                                m.facets.sprints.get(&sp.id).map_or(0, |(_, n)| *n)
                            ),
                            checked: shown.contains(&sp.id),
                        })
                        .collect(),
                )
            }
            Purpose::LinkHow(k) => {
                let mut seen: Vec<String> = Vec::new();
                let mut items = Vec::new();
                for t in &m.desk.link_types {
                    for phrase in [&t.outward, &t.inward] {
                        let p = phrase.to_lowercase();
                        if !seen.contains(&p) {
                            seen.push(p.clone());
                            items.push(Item {
                                id: p.clone(),
                                label: format!("{k} {p} …"),
                                detail: t.name.clone(),
                                checked: false,
                            });
                        }
                    }
                }
                (format!("Link {k}: how?"), items)
            }
            Purpose::Parent(k) => {
                start = m
                    .effective_issue(k)
                    .and_then(|i| i.parent.as_ref().map(|p| p.key.clone()));
                (
                    format!(
                        "Parent of {k}: an epic (queued; {} applies)",
                        state::apply_key()
                    ),
                    m.effective
                        .iter()
                        .filter(|i| i.kind.eq_ignore_ascii_case("epic") && i.key != *k)
                        .map(|i| Item {
                            id: i.key.clone(),
                            label: format!("{}  {}", i.key, i.summary),
                            detail: i.status.clone(),
                            checked: start.as_deref() == Some(i.key.as_str()),
                        })
                        .collect(),
                )
            }
            Purpose::GoToIssue | Purpose::AddToPage | Purpose::LinkTo(..) => (
                if let Purpose::LinkTo(k, how) = &purpose {
                    format!(
                        "{k} {how} …: which issue? (queued; {} applies)",
                        state::apply_key()
                    )
                } else if purpose == Purpose::AddToPage {
                    "Add issues or the Board to the page · Enter to the right, ⇧Enter below, Esc when done"
                        .into()
                } else if let PaneFocus::Issue(k) = &m.focus {
                    format!("Go to issue · shows it in this tab instead of {k}")
                } else {
                    "Go to issue".into()
                },
                board_row
                    .then(|| Item {
                        id: BOARD_ITEM.into(),
                        label: "Board".into(),
                        detail: "the list as status columns".into(),
                        checked: false,
                    })
                    .into_iter()
                    .chain(m.effective.iter().map(|i| Item {
                        id: i.key.clone(),
                        label: format!("{}  {}", i.key, i.summary),
                        detail: i.status.clone(),
                        checked: false,
                    }))
                    .collect(),
            ),
            p @ (Purpose::FilterSprint
            | Purpose::FilterAssignee
            | Purpose::FilterLabel
            | Purpose::FilterEpic
            | Purpose::FilterStatus
            | Purpose::FilterTags) => crate::omni::filter_choices(m, p),
            Purpose::Sort => (
                "Sort by".into(),
                Sort::ALL
                    .iter()
                    .map(|so| Item {
                        id: format!("{so:?}"),
                        label: so.label().into(),
                        detail: String::new(),
                        checked: m.filter.sort == *so,
                    })
                    .collect(),
            ),
            Purpose::Transition(k) => {
                let cur = m
                    .effective_issue(k)
                    .map(|i| i.status.clone())
                    .unwrap_or_default();
                start = Some(cur.clone());
                (
                    format!("Move {k} to…"),
                    m.desk
                        .statuses()
                        .into_iter()
                        .map(|(name, cat)| Item {
                            checked: name == cur,
                            detail: cat.label().into(),
                            label: name.clone(),
                            id: name,
                        })
                        .collect(),
                )
            }
            Purpose::Assign(k) => {
                let cur = m
                    .effective_issue(k)
                    .and_then(|i| i.assignee.as_ref().map(|p| p.id.clone()))
                    .unwrap_or_default();
                let me = m.desk.myself.clone();
                let mut items = vec![Item {
                    id: String::new(),
                    label: "Unassigned".into(),
                    detail: String::new(),
                    checked: cur.is_empty(),
                }];
                let mut people = m.desk.assignees();
                if let Some(me) = &me
                    && let Some(pos) = people.iter().position(|p| p.id == me.id)
                {
                    let p = people.remove(pos);
                    people.insert(0, p);
                }
                for p in people {
                    items.push(Item {
                        checked: p.id == cur,
                        detail: if me.as_ref().is_some_and(|x| x.id == p.id) {
                            "me".into()
                        } else {
                            String::new()
                        },
                        label: p.name.clone(),
                        id: p.id,
                    });
                }
                start = Some(cur);
                (format!("Assign {k} to…"), items)
            }
            Purpose::Labels(k) => {
                let cur: Vec<String> = m
                    .effective_issue(k)
                    .map(|i| i.labels.clone())
                    .unwrap_or_default();
                let mut all = m.desk.labels();
                for l in &cur {
                    if !all.contains(l) {
                        all.push(l.clone());
                    }
                }
                all.sort_by_key(|l| (!cur.contains(l), l.clone()));
                (
                    format!("Labels on {k} (Enter toggles, type to add, Esc closes)"),
                    all.into_iter()
                        .map(|l| Item {
                            checked: cur.contains(&l),
                            detail: String::new(),
                            label: l.clone(),
                            id: l,
                        })
                        .collect(),
                )
            }
            Purpose::Priority(k) => {
                let cur = m
                    .effective_issue(k)
                    .and_then(|i| i.priority.clone())
                    .unwrap_or_default();
                let mut ps = m.desk.priorities();
                if ps.is_empty() {
                    ps = ["Highest", "High", "Medium", "Low", "Lowest"]
                        .map(String::from)
                        .to_vec();
                }
                start = Some(cur.clone());
                (
                    format!("Priority of {k}"),
                    ps.into_iter()
                        .map(|p| Item {
                            checked: p == cur,
                            detail: String::new(),
                            label: p.clone(),
                            id: p,
                        })
                        .collect(),
                )
            }
            Purpose::Sprint(k) => {
                let cur = m
                    .effective_issue(k)
                    .and_then(|i| i.current_sprint().map(|s| s.id.to_string()))
                    .unwrap_or_default();
                let open: Vec<_> = m
                    .desk
                    .sprints()
                    .into_iter()
                    .filter(|sp| sp.state != SprintState::Closed)
                    .collect();
                // The cursor starts on the sprint after the issue's (the usual move: on to the
                // next one); from the backlog, on the first (the active sprint); from the last,
                // where it is. Enter takes it, or the arrows pick another.
                let next = match open.iter().position(|sp| sp.id.to_string() == cur) {
                    Some(i) => open.get(i + 1).or(open.get(i)),
                    None => open.first(),
                };
                start = next.map(|sp| sp.id.to_string());
                (
                    format!("Move {k} to sprint…"),
                    open.into_iter()
                        .map(|sp| Item {
                            checked: sp.id.to_string() == cur,
                            detail: match sp.state {
                                SprintState::Active => "active".into(),
                                _ => "next".into(),
                            },
                            label: sp.name.clone(),
                            id: sp.id.to_string(),
                        })
                        .collect(),
                )
            }
            Purpose::Tags(k) => {
                let cur = m.desk.sidecar.local(k).tags;
                let mut all = m.desk.tags();
                all.sort_by_key(|t| (!cur.contains(t), t.clone()));
                (
                    format!(
                        "Your tags on {k} (Enter toggles, type to add, Esc closes; local only)"
                    ),
                    all.into_iter()
                        .map(|t| Item {
                            checked: cur.contains(&t),
                            detail: m
                                .facets
                                .tags
                                .get(&t)
                                .map(|n| n.to_string())
                                .unwrap_or_default(),
                            label: t.clone(),
                            id: t,
                        })
                        .collect(),
                )
            }
            Purpose::Command => return,
            Purpose::RequestKind(k) => (
                format!("A request on {k}: something to make true of it outside Jira Desk"),
                [
                    (
                        "description",
                        "Description rewrite",
                        "say what should change; an agent or you does it",
                    ),
                    (
                        "link",
                        "Link to another issue",
                        "blocks, is blocked by, relates to… — seen by sync",
                    ),
                    (
                        "parent",
                        "Parent (epic)",
                        "the issue it should sit under — seen by sync",
                    ),
                    (
                        "fix-version",
                        "Fix version",
                        "add or take off a version — seen by sync",
                    ),
                    ("other", "Anything else", "in your words"),
                ]
                .into_iter()
                .map(|(id, label, detail)| Item {
                    id: id.into(),
                    label: label.into(),
                    detail: detail.into(),
                    checked: false,
                })
                .collect(),
            ),
            Purpose::Resolve(k) => {
                let r = m.desk.identities.open_review(k);
                let what = r.map(|r| r.summary()).unwrap_or_default();
                (
                    format!("{k}: {what}"),
                    vec![
                        Item {
                            id: "keep".into(),
                            label: "Same work: keep my queued changes, note, star and tags".into(),
                            detail: "the changes can be applied".into(),
                            checked: false,
                        },
                        Item {
                            id: "detach".into(),
                            label: "Repurposed: move my things to the archive, start clean".into(),
                            detail: "drops the queued changes".into(),
                            checked: false,
                        },
                        Item {
                            id: "later".into(),
                            label: "Decide later".into(),
                            detail: "changes stay held".into(),
                            checked: false,
                        },
                    ],
                )
            }
        };
        let new_what = match purpose {
            Purpose::Labels(_) => Some("label"),
            Purpose::Tags(_) => Some("tag"),
            _ => None,
        };
        let mut p = Picker::new(&title, purpose, items);
        if let Some(what) = new_what {
            p = p.with_new(what);
        }
        if let Some(st) = start {
            p = p.at(&st);
        }
        self.overlay = Some(Overlay::Picker(p));
        cx.notify();
    }

    /// The command palette: every command in [`commands`], named, with the key that runs it
    /// here (read from the keymap, so a remapped key shows as remapped).
    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.overlay.is_some() || self.typing(cx).is_some() {
            return;
        }
        let focus = self.model.read(cx).focus.clone();
        let ctx = format!("JiraDesk {}", focus.context());
        let in_focus = self.shell.read(cx).workspace().in_focus();
        let auto_advance = self.model.read(cx).auto_advance;
        let mut actions: Vec<Box<dyn Action>> = Vec::new();
        let mut items = Vec::new();
        for (section, rows) in commands() {
            // Pane-only commands show only in that pane.
            let shown = match section {
                CHANGES_SECTION => matches!(focus, PaneFocus::Changes),
                SETTINGS_SECTION => matches!(focus, PaneFocus::Settings),
                _ => true,
            };
            if !shown {
                continue;
            }
            let group = palette_group(section);
            for (action, what) in rows {
                if PALETTE_SKIPS.contains(&action.name()) {
                    continue;
                }
                let key = keys::label_in(action.as_ref(), window, &ctx)
                    .or_else(|| keys::label(action.as_ref(), window))
                    .unwrap_or_default();
                // In a focus layout, closing sends the pane to the tray: say so.
                // A switch is named for what choosing it does now.
                let what: String = if in_focus && action.name() == "chorong::ClosePane" {
                    "Close pane (to the tray)".into()
                } else if action.name() == "jdesk::ToggleAutoAdvance" {
                    if auto_advance {
                        "Disable auto-advance".into()
                    } else {
                        "Enable auto-advance".into()
                    }
                } else {
                    what.into()
                };
                items.push(Item {
                    id: actions.len().to_string(),
                    label: format!("{group}: {what}"),
                    detail: key,
                    checked: false,
                });
                actions.push(action);
            }
        }
        self.palette = actions;
        self.overlay = Some(Overlay::Picker(Picker::new(
            "Run a command",
            Purpose::Command,
            items,
        )));
        cx.notify();
    }

    /// Runs the command chosen in the palette, as if its key were pressed.
    fn run_chosen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(action) = self.run_next.take() {
            self.palette.clear();
            window.focus(&self.focus);
            window.dispatch_action(action, cx);
        }
    }

    // ----- jdesk and links -------------------------------------------------------------------

    /// Shows the chord hint while a chord is half typed, and holds a chord GPUI timed out.
    pub fn watch_chords(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.observe_pending_input(window, |this, window, cx| {
            this.chord.pending_changed(window);
            cx.notify();
        })
        .detach();
        cx.observe_keystrokes(|this, e, _, cx| {
            if this.chord.key_observed(&e.keystroke) {
                cx.notify();
            }
        })
        .detach();
    }

    /// The card listing what can follow the chord being typed.
    fn chord_card(&self, window: &Window, cx: &App) -> Option<AnyElement> {
        let (pending, ctx) = self.chord.showing(window)?;
        let titles: Vec<(&'static str, String)> = commands()
            .into_iter()
            .flat_map(|(_, rows)| rows)
            .map(|(a, t)| (a.name(), t.to_string()))
            .collect();
        let followers: Vec<(String, String)> = crate::chord::followers(&pending, &ctx, cx)
            .into_iter()
            .map(|(k, a)| {
                let title = titles
                    .iter()
                    .find(|(n, _)| *n == a.name())
                    .map_or_else(|| crate::chord::action_words(a.name()), |(_, t)| t.clone());
                (crate::chord::stroke_label(&k), title)
            })
            .collect();
        if followers.is_empty() {
            return None;
        }
        let rows = crate::chord::rows(followers);
        let prefix = pending
            .iter()
            .map(crate::chord::stroke_label)
            .collect::<Vec<_>>()
            .join(" ");
        let c = self.model.read(cx).colors.clone();
        let cap = |k: String| {
            div()
                .px(px(5.))
                .rounded(px(4.))
                .border_1()
                .border_color(c.border)
                .bg(c.panel)
                .font_family(c.mono_font.clone())
                .text_color(c.text)
                .child(k)
        };
        let two_cols = rows.len() > 6;
        Some(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom(px(34.))
                .flex()
                .justify_center()
                .child(
                    div()
                        .w(px(if two_cols { 560. } else { 320. }))
                        .p(px(10.))
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .bg(c.elevated)
                        .border_1()
                        .border_color(c.border)
                        .rounded(px(8.))
                        .shadow_lg()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .text_color(c.text_muted)
                                .child(cap(prefix))
                                .child(if self.chord.is_held() {
                                    "then… (Esc cancels)"
                                } else {
                                    "then…"
                                }),
                        )
                        .child(div().flex().flex_wrap().children(rows.into_iter().map(
                            |(k, title)| {
                                div()
                                    .w(gpui::relative(if two_cols { 0.5 } else { 1. }))
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .py(px(2.))
                                    .child(div().w(px(64.)).flex_none().child(cap(k)))
                                    .child(div().overflow_hidden().whitespace_nowrap().child(title))
                            },
                        ))),
                )
                .into_any_element(),
        )
    }

    /// Answers `jdesk` (and `jiradesk://` links) from now on: their requests arrive on `jobs`.
    pub fn start_control(
        &mut self,
        jobs: futures::channel::mpsc::UnboundedReceiver<crate::control::Job>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use futures::StreamExt;
        let mut jobs = jobs;
        cx.spawn_in(window, async move |this, cx| {
            while let Some((req, reply)) = jobs.next().await {
                let answer = this
                    .update_in(cx, |this, window, cx| this.control(req, window, cx))
                    .unwrap_or_else(|_| Response::err("Jira Desk is closing"));
                if let Some(reply) = reply {
                    let _ = reply.send(answer);
                }
            }
        })
        .detach();
    }

    /// One request from `jdesk` or a link. Data requests run the same code as `jdesk` does on
    /// the files; the rest act on the window.
    fn control(&mut self, req: Request, window: &mut Window, cx: &mut Context<Self>) -> Response {
        let known = |this: &Self, cx: &App, k: &str| this.model.read(cx).desk.issue(k).is_some();
        let answer = match req {
            Request::State => Response::ok("state", self.state_json(cx)),
            Request::Commands => {
                let list: Vec<serde_json::Value> = commands()
                    .into_iter()
                    .flat_map(|(section, rows)| {
                        let group = palette_group(section);
                        rows.into_iter()
                            .filter(|(a, _)| !PALETTE_SKIPS.contains(&a.name()))
                            .map(move |(a, what)| (format!("{group}: {what}"), a))
                    })
                    .map(|(name, a)| {
                        let key = keys::label(a.as_ref(), window).unwrap_or_default();
                        serde_json::json!({ "name": name, "key": key, "action": a.name() })
                    })
                    .collect();
                Response::ok(
                    format!("{} commands", list.len()),
                    serde_json::Value::Array(list),
                )
            }
            Request::Run { name } => match find_command(&name) {
                // Applying and the like are yours, or apply's (`jdesk apply`, MCP's apply_queue,
                // which tells an agent to ask first): never a palette command run from outside.
                Ok((full, action)) if run_refuses(action.as_ref()) => Response::err(format!(
                    "“{full}” isn't run from outside the window: changes reach Jira only through \
                     apply (jdesk apply, or apply_queue for an agent)"
                )),
                Ok((full, action)) => {
                    self.overlay = None;
                    window.focus(&self.focus);
                    window.dispatch_action(action, cx);
                    Response::ok(format!("Ran “{full}”"), serde_json::Value::Null)
                }
                Err(e) => Response::err(e),
            },
            Request::Open { key } => {
                if known(self, cx, &key) {
                    self.overlay = None;
                    self.go_to_issue(&key, cx);
                    Response::ok(format!("Showing {key}"), serde_json::Value::Null)
                } else {
                    Response::err(format!("{key} is not in the local copy"))
                }
            }
            Request::Search { query } => {
                let r = self.model.update(cx, |m, cx| {
                    let r = omni::apply_query(m, &query);
                    m.generation += 1;
                    cx.notify();
                    r
                });
                match r {
                    Ok(msg) => {
                        // The board shows the same filtered issues: filtering it keeps it.
                        if self.model.read(cx).focus != PaneFocus::Board {
                            self.focus_pane(placeholder(ISSUES), cx);
                        }
                        Response::ok(msg, self.state_json(cx)["visible"].clone())
                    }
                    Err(e) => Response::err(e),
                }
            }
            Request::Series { keys } => {
                if let Some(k) = keys.iter().find(|k| !known(self, cx, k)) {
                    Response::err(format!("{k} is not in the local copy"))
                } else {
                    self.overlay = None;
                    self.model.update(cx, |m, _| m.picked = keys.clone());
                    self.open_series(cx);
                    Response::ok(
                        format!("Opened {} issues as a focus series", keys.len()),
                        serde_json::Value::Null,
                    )
                }
            }
            Request::Sync { full } => self.model.update(cx, |m, cx| {
                if m.sync.running {
                    return Response::ok("A refresh is already running", serde_json::Value::Null);
                }
                m.start_sync(
                    if full {
                        sync::Mode::Full
                    } else {
                        sync::Mode::Auto
                    },
                    cx,
                );
                Response::ok(
                    "Refresh started in the app (app state shows when it's done)",
                    serde_json::Value::Null,
                )
            }),
            Request::Apply { ids } => self.model.update(cx, |m, cx| {
                if m.sync.applying || m.sync.running {
                    return Response::err("Wait: the app is refreshing or applying");
                }
                let n = m.desk.queue.pending_count();
                m.start_apply((!ids.is_empty()).then_some(ids), cx);
                Response::ok(
                    format!("Applying in the app ({n} pending; app state shows the result)"),
                    serde_json::Value::Null,
                )
            }),
            req => self.model.update(cx, |m, cx| {
                if req.writes() && m.sync.applying {
                    return Response::err("Wait for the changes being applied to finish");
                }
                match jdesk_core::control::data(&mut m.desk, &req) {
                    Ok(r) => {
                        if req.writes() {
                            m.rebuild();
                            m.note(format!("{} (from jdesk)", r.message));
                            m.generation += 1;
                            cx.notify();
                        }
                        r
                    }
                    Err(e) => Response::err(format!("{e:#}")),
                }
            }),
        };
        cx.notify();
        answer
    }

    /// What the window shows, for `jdesk app state`.
    fn state_json(&self, cx: &App) -> serde_json::Value {
        let m = self.model.read(cx);
        let visible: Vec<jdesk_core::control::Row> = m
            .visible
            .iter()
            .take(200)
            .map(|i| jdesk_core::control::Row::of(&m.desk, &m.effective[*i]))
            .collect();
        // Where things are: for a script or agent driving the app to check the layout.
        let ws = self.shell.read(cx).workspace();
        let title = |p: &PaneId| ws.content(*p).map(|c| c.title());
        let on_screen: Vec<String> = ws
            .panes()
            .keys()
            // The front tab of each group showing: not the tabs behind it.
            .filter(|p| {
                ws.group_of(**p)
                    .and_then(|g| ws.group(g))
                    .is_some_and(|g| g.tabs.get(g.active) == Some(*p))
            })
            .filter_map(title)
            .collect();
        let tray: Vec<String> = ws.tray().iter().filter_map(title).collect();
        let layout = serde_json::json!({
            "focus": ws.in_focus(),
            "page": ws.focus().map(|f| f.current + 1),
            "pages": ws.focus().map(|f| f.pages.len()),
            "on_screen": on_screen,
            "tray": tray,
        });
        serde_json::json!({
            "pane": m.focus.context(),
            "layout": layout,
            "selected": m.selected,
            "filter": m.filter,
            "filter_summary": m.filter_summary(),
            "visible_count": m.visible.len(),
            "visible": visible,
            "picked": m.picked,
            "queue_pending": m.desk.queue.pending_count(),
            "syncing": m.sync.running,
            "applying": m.sync.applying,
            "last_sync": m.desk.last_sync,
            "sync_error": m.sync.error,
            "search_open": m.search.is_some(),
            "overlay": match &self.overlay {
                None => None,
                Some(Overlay::Picker(p)) => Some(p.title.clone()),
                Some(Overlay::Prompt(_)) => Some("prompt".to_string()),
                Some(Overlay::ConfirmApply(_)) => Some("confirm apply".to_string()),
                Some(Overlay::Help) => Some("help".to_string()),
                Some(Overlay::Request(id)) => Some(format!("request #{id}")),
            },
        })
    }

    /// The focused issue pane's selectable text, by name: the pane and the issue it shows.
    fn focused_text_pane(&self, cx: &App) -> Option<String> {
        let ws = self.shell.read(cx).workspace();
        let pane = ws.focused()?;
        let key = match PaneFocus::of(ws.content(pane)) {
            PaneFocus::Preview => self.model.read(cx).selected.clone()?,
            PaneFocus::Issue(k) => k,
            _ => return None,
        };
        Some(crate::panes::text_area(
            &crate::panes::pane_name(pane),
            &key,
        ))
    }

    /// ⌘C in the issue pane: the selected text to the clipboard.
    fn copy_selection(&mut self, with_link: bool, cx: &mut Context<Self>) {
        let Some(pane) = self.focused_text_pane(cx) else {
            return;
        };
        let (text, link) = {
            let m = self.model.read(cx);
            let text = m
                .sel(&pane)
                .filter(|s| !s.is_empty())
                .map(|s| m.text_map(&pane).text(&s));
            // The area is "pane|KEY": the issue the text came from.
            let key = pane.rsplit('|').next().unwrap_or_default();
            let link = m
                .effective_issue(key)
                .map(|i| format!("{key} {}: {}", i.summary, m.desk.browse_url(key)));
            (text, link)
        };
        self.model.update(cx, |m, cx| {
            let n = text.as_ref().map(|t| t.chars().count());
            let copied = match (text, with_link.then_some(link).flatten()) {
                (Some(t), Some(l)) => Some(format!("{t}\n— {l}")),
                (Some(t), None) => Some(t),
                (None, Some(l)) => Some(l),
                (None, None) => None,
            };
            match copied {
                Some(c) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(c));
                    m.note(match (n, with_link) {
                        (Some(n), false) => {
                            format!("Copied {n} character{}", if n == 1 { "" } else { "s" })
                        }
                        (Some(n), true) => format!(
                            "Copied {n} character{} with the issue's link",
                            if n == 1 { "" } else { "s" }
                        ),
                        (None, _) => "Copied the issue's link".to_string(),
                    });
                }
                None => m.note(
                    "Nothing selected: drag over the text first (⌘A selects it all; ⇧⌘C copies the link)",
                ),
            }
            cx.notify();
        });
    }

    /// ⌘A in the issue pane: all its text.
    fn select_all_text(&mut self, cx: &mut Context<Self>) {
        let Some(pane) = self.focused_text_pane(cx) else {
            return;
        };
        self.model.update(cx, |m, cx| {
            if let Some(end) = m.text_map(&pane).last() {
                m.text_sel
                    .borrow_mut()
                    .insert(pane, crate::select::TextSel::new((0, 0), end));
                m.generation += 1;
                cx.notify();
            }
        });
    }

    /// Selects `key` in the list if it's there, else opens it in a tab.
    fn go_to_issue(&mut self, key: &str, cx: &mut Context<Self>) {
        let m = self.model.read(cx);
        let visible = m.visible.iter().any(|i| m.effective[*i].key == key);
        if visible {
            self.model.update(cx, |m, cx| {
                m.select_key(key);
                cx.notify();
            });
            self.focus_pane(placeholder(ISSUES), cx);
        } else {
            self.open_issue_tab(key, cx);
        }
    }

    // ----- the search bar -------------------------------------------------------------------

    /// Tab: open a chip for the field word before the caret (or the highlighted field), or,
    /// in a chip, take the highlighted value.
    fn search_tab(&mut self, cx: &mut Context<Self>) {
        self.model.update(cx, |m, cx| {
            let Some(o) = m.search.as_ref() else {
                return;
            };
            if o.open.is_some() {
                search_take(m);
            } else {
                let sugg = omni::suggestions(m, o);
                let (field, take_word) = match o.field_at_caret() {
                    Some(f) => (Some(f), true),
                    None => match o.cursor.and_then(|c| sugg.get(c)) {
                        Some(omni::Suggestion::Field(f)) => (Some(*f), false),
                        _ => (None, false),
                    },
                };
                if let Some(f) = field
                    && let Some(o) = m.search.as_mut()
                {
                    o.open_chip(f, take_word);
                    m.filter.text = o.text().to_string();
                    m.refilter();
                }
            }
            m.generation += 1;
            cx.notify();
        });
    }

    /// ⇧Tab: reopen the last chip's field, to change it.
    fn search_reopen(&mut self, cx: &mut Context<Self>) {
        self.model.update(cx, |m, cx| {
            let last = omni::chips(m).last().map(|c| c.0);
            if let (Some(f), Some(o)) = (last, m.search.as_mut()) {
                if o.open.is_some() {
                    o.close_chip();
                }
                o.open_chip(f, false);
                m.generation += 1;
                cx.notify();
            }
        });
    }

    /// Enter in the search bar: take the chip's value, go to the issue typed, open the
    /// highlighted field, or keep the search and close the bar.
    fn search_enter(&mut self, cx: &mut Context<Self>) {
        let mut go_to: Option<String> = None;
        self.model.update(cx, |m, cx| {
            let Some(o) = m.search.as_ref() else {
                return;
            };
            if o.open.is_some() {
                search_take(m);
            } else {
                let sugg = omni::suggestions(m, o);
                let chosen = match o.cursor {
                    Some(c) => sugg.get(c),
                    None => sugg
                        .iter()
                        .find(|s| matches!(s, omni::Suggestion::GoTo(..))),
                };
                match chosen {
                    Some(omni::Suggestion::Field(f)) => {
                        let f = *f;
                        let take = o.field_at_caret() == Some(f);
                        if let Some(o) = m.search.as_mut() {
                            o.open_chip(f, take);
                            m.filter.text = o.text().to_string();
                            m.refilter();
                        }
                    }
                    Some(omni::Suggestion::GoTo(k, _)) => {
                        go_to = Some(k.clone());
                        m.search = None;
                        m.filter.text.clear();
                        m.refilter();
                    }
                    _ => m.search = None,
                }
            }
            m.generation += 1;
            cx.notify();
        });
        if let Some(k) = go_to {
            self.go_to_issue(&k, cx);
        }
    }

    fn pick(&mut self, close: bool, cx: &mut Context<Self>) {
        let Some(Overlay::Picker(p)) = &mut self.overlay else {
            return;
        };
        let Some(item) = p.current().cloned() else {
            return;
        };
        let purpose = p.purpose.clone();
        let multi = purpose.multi();
        if multi && let Some(it) = p.current_mut() {
            it.checked = !it.checked;
        }
        let mut close_now = !multi || close;
        let model = self.model.clone();
        match purpose {
            Purpose::GoToIssue => {
                if !self.swap_focused_issue_tab(&item.id, cx) {
                    self.go_to_issue(&item.id, cx);
                }
            }
            Purpose::AddToPage => {
                self.add_issue_to_page(&item.id, Edge::Right, cx);
                close_now = false;
            }
            p @ (Purpose::FilterSprint
            | Purpose::FilterAssignee
            | Purpose::FilterLabel
            | Purpose::FilterEpic
            | Purpose::FilterStatus
            | Purpose::FilterTags) => {
                model.update(cx, |m, cx| {
                    crate::omni::apply_filter(m, &p, &item.id);
                    cx.notify();
                });
                // Active, Backlog and All replace the sprints chosen: the ticks come from the
                // filter, not from flipping the one chosen.
                if p == Purpose::FilterSprint {
                    let (_, fresh) = crate::omni::filter_choices(self.model.read(cx), &p);
                    if let Some(Overlay::Picker(pk)) = &mut self.overlay {
                        for it in pk.items.iter_mut() {
                            if let Some(f) = fresh.iter().find(|f| f.id == it.id) {
                                it.checked = f.checked;
                            }
                        }
                    }
                }
            }
            Purpose::Sort => model.update(cx, |m, cx| {
                if let Some(so) = Sort::ALL.iter().find(|so| format!("{so:?}") == item.id) {
                    m.filter.sort = *so;
                }
                m.refilter();
                cx.notify();
            }),
            Purpose::Transition(k) => model.update(cx, |m, cx| {
                m.queue(
                    &k,
                    Edit::Transition {
                        to: item.id.clone(),
                    },
                );
                cx.notify();
            }),
            Purpose::Assign(k) => model.update(cx, |m, cx| {
                let to = (!item.id.is_empty()).then(|| Person {
                    id: item.id.clone(),
                    name: item.label.clone(),
                });
                m.queue(&k, Edit::Assign { to });
                cx.notify();
            }),
            Purpose::Labels(k) => {
                let (label, add) = match item.id.strip_prefix(NEW_PREFIX) {
                    Some(new) => (new.to_string(), true),
                    None => (item.id.clone(), !item.checked),
                };
                model.update(cx, |m, cx| {
                    let edit = if add {
                        Edit::Labels {
                            add: vec![label.clone()],
                            remove: vec![],
                        }
                    } else {
                        Edit::Labels {
                            add: vec![],
                            remove: vec![label.clone()],
                        }
                    };
                    m.queue(&k, edit);
                    cx.notify();
                });
                if item.id.starts_with(NEW_PREFIX) {
                    // Reopen so the new label shows as a ticked item.
                    self.open_picker(Purpose::Labels(k), cx);
                    close_now = false;
                }
            }
            Purpose::Priority(k) => model.update(cx, |m, cx| {
                m.queue(
                    &k,
                    Edit::Priority {
                        to: item.id.clone(),
                    },
                );
                cx.notify();
            }),
            Purpose::Sprint(k) => {
                let sprint = model
                    .read(cx)
                    .desk
                    .sprints()
                    .into_iter()
                    .find(|sp| sp.id.to_string() == item.id);
                if let Some(to) = sprint {
                    model.update(cx, |m, cx| {
                        m.queue(&k, Edit::Sprint { to });
                        cx.notify();
                    });
                }
            }
            Purpose::Tags(k) => {
                let tag = item
                    .id
                    .strip_prefix(NEW_PREFIX)
                    .map(|t| t.trim_start_matches('#').to_string())
                    .unwrap_or_else(|| item.id.clone());
                model.update(cx, |m, cx| {
                    match m.desk.toggle_tag(&k, &tag) {
                        Ok(()) => {
                            m.rebuild_after_change(&k, false);
                            let on = m.desk.sidecar.local(&k).tags.contains(&tag);
                            m.note(format!(
                                "{} #{tag} on {k}",
                                if on { "Tagged" } else { "Untagged" }
                            ));
                        }
                        Err(e) => m.error(e.to_string()),
                    }
                    cx.notify();
                });
                if item.id.starts_with(NEW_PREFIX) {
                    self.open_picker(Purpose::Tags(k), cx);
                    close_now = false;
                }
            }
            Purpose::RequestKind(k) => {
                self.open_prompt(PromptFor::Request(k, item.id.clone()), cx);
                close_now = false;
            }
            Purpose::LinkHow(k) => {
                self.open_picker(Purpose::LinkTo(k, item.id.clone()), cx);
                close_now = false;
            }
            Purpose::SprintsShown => {
                // The tick was flipped above: it says whether to show.
                if let Ok(id) = item.id.parse::<u64>() {
                    model.update(cx, |m, cx| {
                        m.show_sprint_in_filters(id, !item.checked);
                        cx.notify();
                    });
                }
            }
            Purpose::LinkTo(k, how) => {
                self.queue_parsed(&k, "link", vec![how, item.id.clone()], cx)
            }
            Purpose::Parent(k) => self.queue_parsed(&k, "parent", vec![item.id.clone()], cx),
            Purpose::Command => {
                self.run_next = item
                    .id
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| self.palette.get(i))
                    .map(|a| a.boxed_clone());
            }
            Purpose::Resolve(k) => model.update(cx, |m, cx| {
                match item.id.as_str() {
                    "keep" => match m.desk.keep_after_review(&k) {
                        Ok(()) => m.note(format!(
                            "{k}: same work. Its queued changes can be applied ({})",
                            state::apply_key()
                        )),
                        Err(e) => m.error(e.to_string()),
                    },
                    "detach" => match m.desk.detach_after_review(&k) {
                        Ok(folder) => m.note(format!(
                            "{k}: repurposed. Your note, star, tags and queued changes are in {}",
                            folder
                                .strip_prefix(&m.desk.dir)
                                .unwrap_or(&folder)
                                .display()
                        )),
                        Err(e) => m.error(e.to_string()),
                    },
                    _ => m.note(format!("{k} stays held for review")),
                }
                m.rebuild();
                cx.notify();
            }),
        }
        if close_now {
            self.overlay = None;
        }
        cx.notify();
    }

    fn open_prompt(&mut self, purpose: PromptFor, cx: &mut Context<Self>) {
        let m = self.model.read(cx);
        let p = match &purpose {
            PromptFor::Request(k, kind) => {
                let (title, hint, multiline) = match kind.as_str() {
                    "description" => (
                        format!("Description rewrite on {k}: what should change"),
                        "In your words; an agent (Copy for agent) or you makes it in Jira · ⌘Enter saves · Esc cancels",
                        true,
                    ),
                    "link" => (
                        format!("Link {k} to: how and which issue"),
                        "e.g. blocks DEMO-40, is blocked by DEMO-12, relates to DEMO-7 · Enter saves",
                        false,
                    ),
                    "parent" => (
                        format!("Parent of {k}: the issue key"),
                        "e.g. DEMO-5 (an epic, for most issues) · Enter saves",
                        false,
                    ),
                    "fix-version" => (
                        format!("Fix version on {k}"),
                        "+NAME adds it, -NAME takes it off: +2.3 · Enter saves",
                        false,
                    ),
                    _ => (
                        format!("A request on {k}"),
                        "In your words · ⌘Enter saves · Esc cancels",
                        true,
                    ),
                };
                Prompt {
                    title,
                    hint: hint.into(),
                    area: TextArea::new("", multiline),
                    purpose,
                    error: None,
                }
            }
            PromptFor::Summary(k) => Prompt {
                title: format!("Summary of {k}"),
                hint: "Enter queues the change · Esc cancels".into(),
                area: TextArea::new(
                    &m.effective_issue(k)
                        .map(|i| i.summary.clone())
                        .unwrap_or_default(),
                    false,
                ),
                purpose,
                error: None,
            },
            PromptFor::Points(k) => Prompt {
                title: format!("Story points for {k}"),
                hint: "A number, or empty to clear · Enter queues · Esc cancels".into(),
                area: TextArea::new(
                    &m.effective_issue(k)
                        .and_then(|i| i.story_points)
                        .map(|p| {
                            if p.fract() == 0.0 {
                                format!("{p:.0}")
                            } else {
                                p.to_string()
                            }
                        })
                        .unwrap_or_default(),
                    false,
                ),
                purpose,
                error: None,
            },
            PromptFor::Comment(k) => Prompt {
                title: format!("Comment on {k}"),
                hint: "Enter for a new line · ⌘Enter queues the comment · Esc keeps it as a draft"
                    .into(),
                area: TextArea::new(
                    m.comment_drafts.get(k).map(String::as_str).unwrap_or(""),
                    true,
                ),
                purpose,
                error: None,
            },
        };
        let mut p = p;
        p.area.all = !p.area.multiline && !p.area.text.is_empty();
        self.overlay = Some(Overlay::Prompt(p));
        cx.notify();
    }

    fn submit_prompt(&mut self, cx: &mut Context<Self>) {
        let Some(Overlay::Prompt(p)) = &mut self.overlay else {
            return;
        };
        let text = p.area.text.clone();
        if let PromptFor::Request(k, kind) = &p.purpose {
            let req = Request::RequestAdd {
                key: k.clone(),
                kind: kind.clone(),
                value: text,
            };
            // Announce the change, so the Notes pane (and the row's flag) redraw at once.
            let r = self.model.update(cx, |m, cx| {
                cx.notify();
                match jdesk_core::control::data(&mut m.desk, &req) {
                    Ok(resp) if resp.ok => {
                        m.rebuild();
                        m.note(format!(
                            "{} (Notes shows it; Copy for agent hands it over)",
                            resp.message
                        ));
                        Ok(())
                    }
                    Ok(resp) => Err(resp.message),
                    Err(e) => Err(format!("{e:#}")),
                }
            });
            match r {
                Ok(()) => self.overlay = None,
                Err(e) => {
                    if let Some(Overlay::Prompt(p)) = &mut self.overlay {
                        p.error = Some(e);
                    }
                }
            }
            cx.notify();
            return;
        }
        let edit = match &p.purpose {
            // Handled above: a request isn't a queued change.
            PromptFor::Request(..) => return,
            PromptFor::Summary(k) => {
                if text.trim().is_empty() {
                    p.error = Some("A summary can't be empty".into());
                    cx.notify();
                    return;
                }
                (
                    k.clone(),
                    Edit::Summary {
                        to: text.trim().to_string(),
                    },
                )
            }
            PromptFor::Points(k) => {
                let t = text.trim();
                let to = if t.is_empty() {
                    None
                } else {
                    match t.parse::<f64>() {
                        Ok(v) if v >= 0.0 => Some(v),
                        _ => {
                            p.error = Some(format!("“{t}” is not a number"));
                            cx.notify();
                            return;
                        }
                    }
                };
                (k.clone(), Edit::Points { to })
            }
            PromptFor::Comment(k) => {
                if text.trim().is_empty() {
                    self.overlay = None;
                    cx.notify();
                    return;
                }
                (
                    k.clone(),
                    Edit::Comment {
                        text: text.trim_end().to_string(),
                    },
                )
            }
        };
        self.overlay = None;
        self.model.update(cx, |m, cx| {
            if matches!(edit.1, Edit::Comment { .. }) {
                m.comment_drafts.remove(&edit.0);
            }
            m.queue(&edit.0, edit.1);
            cx.notify();
        });
        cx.notify();
    }

    // ----- typing ---------------------------------------------------------------------------

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        if self.chord.is_replay(k) {
            return;
        }
        if self.chord.is_held() {
            // The key after a held chord finishes it; one that follows nothing closes it.
            if let Some(action) = self.chord.finish(k, cx) {
                window.dispatch_action(action, cx);
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let paste = (k.key == "v" && k.modifiers.secondary())
            .then(|| cx.read_from_clipboard().and_then(|c| c.text()))
            .flatten();
        // Adding issues to a page: Backspace on an empty query goes back to the tray (as the
        // Files chip does in Chorong).
        if k.key == "backspace"
            && !k.modifiers.modified()
            && matches!(&self.overlay, Some(Overlay::Picker(p))
                if p.purpose == Purpose::AddToPage && p.input.text.is_empty())
            && self.tray_to_return_to(cx)
        {
            cx.stop_propagation();
            return self.back_to_tray(window, cx);
        }
        let edited = match &mut self.overlay {
            Some(Overlay::Picker(p)) => {
                let e = p.input.key(k, paste.as_deref());
                if e == Edited::Text {
                    if matches!(
                        p.purpose,
                        Purpose::GoToIssue | Purpose::AddToPage | Purpose::LinkTo(..)
                    ) {
                        p.scope = omni::picker_scope(self.model.read(cx), &p.input.text);
                    }
                    p.requery();
                }
                e
            }
            Some(Overlay::Prompt(p)) => {
                let e = p.area.key(k, paste.as_deref());
                if e == Edited::Text {
                    p.error = None;
                }
                e
            }
            Some(_) => Edited::No,
            None => self.model.update(cx, |m, cx| {
                if let Some((_, t)) = &mut m.note_edit {
                    let e = t.key(k, paste.as_deref());
                    if e != Edited::No {
                        m.generation += 1;
                        cx.notify();
                    }
                    e
                } else if m.search.is_some() {
                    let e = search_key(m, k, paste.as_deref());
                    if e != Edited::No {
                        m.generation += 1;
                        cx.notify();
                    }
                    e
                } else {
                    Edited::No
                }
            }),
        };
        match edited {
            Edited::No => return,
            Edited::Copy(t) => cx.write_to_clipboard(ClipboardItem::new_string(t)),
            Edited::Text | Edited::Moved => {}
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn input_cancel(&mut self, _: &InputCancel, _w: &mut Window, cx: &mut Context<Self>) {
        if let Some(Overlay::Prompt(p)) = &self.overlay
            && let PromptFor::Comment(k) = &p.purpose
        {
            let (k, t) = (k.clone(), p.area.text.clone());
            self.model.update(cx, |m, _| {
                if t.trim().is_empty() {
                    m.comment_drafts.remove(&k);
                } else {
                    m.comment_drafts.insert(k.clone(), t);
                    m.note(format!("Comment on {k} kept as a draft (c to continue)"));
                }
            });
        }
        if self.overlay.take().is_some() {
            cx.notify();
            return;
        }
        self.model.update(cx, |m, cx| {
            if m.note_edit.is_some() {
                save_note(m);
            } else if let Some(o) = m.search.as_mut() {
                // Esc steps back: a selected chip, then an open chip, then the bar (and its
                // text; the chips stay, they're the filters).
                if o.chip_selected {
                    o.chip_selected = false;
                } else if o.open.is_some() {
                    o.close_chip();
                } else {
                    m.search = None;
                    m.filter.text.clear();
                    m.refilter();
                }
                m.generation += 1;
            }
            cx.notify();
        });
    }

    fn input_confirm(&mut self, _: &InputConfirm, w: &mut Window, cx: &mut Context<Self>) {
        match &self.overlay {
            Some(Overlay::Picker(_)) => {
                self.pick(false, cx);
                self.run_chosen(w, cx);
            }
            Some(Overlay::Prompt(_)) => self.submit_prompt(cx),
            Some(Overlay::ConfirmApply(_)) => {
                self.overlay = None;
                self.model.update(cx, |m, cx| m.start_apply(None, cx));
            }
            Some(Overlay::Help | Overlay::Request(_)) => self.overlay = None,
            None if self.model.read(cx).search.is_some() => self.search_enter(cx),
            None => {}
        }
        cx.notify();
    }

    /// ⇧Enter: adding issues to a focus page, this one goes below the focused pane; elsewhere
    /// it is Enter.
    fn input_confirm_alt(&mut self, _: &InputConfirmAlt, w: &mut Window, cx: &mut Context<Self>) {
        let adding =
            matches!(&self.overlay, Some(Overlay::Picker(p)) if p.purpose == Purpose::AddToPage);
        if !adding {
            return self.input_confirm(&InputConfirm, w, cx);
        }
        let key = match &self.overlay {
            Some(Overlay::Picker(p)) => p.current().map(|i| i.id.clone()),
            _ => None,
        };
        if let Some(k) = key {
            self.add_issue_to_page(&k, Edge::Bottom, cx);
        }
        cx.notify();
    }

    /// Queues a change as `jdesk change KEY FIELD VALUE…` would, with the same checks (a
    /// sub-task's parent, a link phrase this site doesn't have): refused ones say why.
    fn queue_parsed(&mut self, key: &str, field: &str, value: Vec<String>, cx: &mut Context<Self>) {
        self.model.update(cx, |m, cx| {
            match jdesk_core::control::parse_change(&m.desk, key, field, &value) {
                Ok(edit) => m.queue(key, edit),
                Err(e) => m.error(format!("{e:#}")),
            }
            cx.notify();
        });
    }

    /// Back from adding issues to Bring from tray… (Backspace on an empty query, or the
    /// Issues chip).
    fn back_to_tray(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.overlay = None;
        self.shell
            .update(cx, |shell, cx| shell.open_tray_picker(window, cx));
        cx.notify();
    }

    /// Whether the add-issues picker can step back to the tray: it has something to bring.
    fn tray_to_return_to(&self, cx: &App) -> bool {
        !self.shell.read(cx).workspace().tray().is_empty()
    }

    fn input_submit(&mut self, _: &InputSubmit, w: &mut Window, cx: &mut Context<Self>) {
        match &self.overlay {
            Some(Overlay::Picker(_)) => {
                self.pick(true, cx);
                self.run_chosen(w, cx);
            }
            Some(Overlay::Prompt(_)) => self.submit_prompt(cx),
            Some(_) => self.input_confirm(&InputConfirm, w, cx),
            None if self.model.read(cx).search.is_some() => self.search_enter(cx),
            None => self.model.update(cx, |m, cx| {
                if m.note_edit.is_some() {
                    save_note(m);
                }
                cx.notify();
            }),
        }
        cx.notify();
    }

    fn input_move(&mut self, d: isize, cx: &mut Context<Self>) {
        match &mut self.overlay {
            None => self.model.update(cx, |m, cx| {
                let Some(o) = m.search.as_ref() else {
                    return;
                };
                let n = omni::suggestions(m, o).len();
                let o = m.search.as_mut().expect("checked");
                o.cursor = match (o.cursor, d > 0) {
                    _ if n == 0 => None,
                    (None, true) => Some(0),
                    (None, false) => None,
                    (Some(c), true) => Some((c + 1).min(n - 1)),
                    (Some(0), false) if o.open.is_none() => None,
                    (Some(c), false) => Some(c.saturating_sub(1)),
                };
                m.generation += 1;
                cx.notify();
            }),
            Some(Overlay::Picker(p)) => p.move_by(d),
            Some(Overlay::Help) => {
                let o = self.help_scroll.offset();
                self.help_scroll
                    .set_offset(gpui::point(o.x, o.y - px(60. * d as f32)));
            }
            _ => {}
        }
        cx.notify();
    }

    // ----- moving ---------------------------------------------------------------------------

    fn cursor(&mut self, delta: isize, to_end: Option<bool>, cx: &mut Context<Self>) {
        let focus = self.model.read(cx).focus.clone();
        match focus {
            PaneFocus::Issues | PaneFocus::Other => self.model.update(cx, |m, cx| {
                let n = m.visible.len();
                if n == 0 {
                    return;
                }
                let cur = m.selected_row().unwrap_or(0) as isize;
                let row = match to_end {
                    Some(false) => 0,
                    Some(true) => n - 1,
                    None => (cur + delta).clamp(0, n as isize - 1) as usize,
                };
                m.select_row(row);
                cx.notify();
            }),
            PaneFocus::Filters => self.model.update(cx, |m, cx| {
                let rows = m.filter_rows();
                if rows.is_empty() {
                    return;
                }
                let n = rows.len() as isize;
                let mut i = match to_end {
                    Some(false) => 0,
                    Some(true) => n - 1,
                    None => m.filter_cursor as isize + delta,
                }
                .clamp(0, n - 1);
                let step = if delta < 0 || to_end == Some(true) {
                    -1
                } else {
                    1
                };
                while rows[i as usize].is_header() {
                    i += step;
                    if i < 0 || i >= n {
                        i = (i - step).clamp(0, n - 1);
                        let back = -step;
                        while rows[i as usize].is_header() && i + back >= 0 && i + back < n {
                            i += back;
                        }
                        break;
                    }
                }
                m.filter_cursor = i as usize;
                m.generation += 1;
                cx.notify();
            }),
            PaneFocus::Changes => self.model.update(cx, |m, cx| {
                let n = m.change_rows().len();
                if n == 0 {
                    return;
                }
                m.changes_cursor = match to_end {
                    Some(false) => 0,
                    Some(true) => n - 1,
                    None => (m.changes_cursor as isize + delta).clamp(0, n as isize - 1) as usize,
                };
                m.generation += 1;
                cx.notify();
            }),
            PaneFocus::Board => self.model.update(cx, |m, cx| {
                let cols = m.board_columns();
                let Some(col) = cols.get(m.board_cursor.0.min(cols.len().saturating_sub(1))) else {
                    return;
                };
                let n = col.2.len() as isize;
                let r = match to_end {
                    Some(false) => 0,
                    Some(true) => n - 1,
                    None => m.board_cursor.1.min(n as usize - 1) as isize + delta,
                };
                m.board_cursor.1 = r.clamp(0, n - 1) as usize;
                m.board_moved();
                cx.notify();
            }),
            PaneFocus::Preview | PaneFocus::Issue(_) | PaneFocus::Notes | PaneFocus::Settings => {
                let ws = self.shell.read(cx).workspace();
                if let Some(p) = ws.focused() {
                    let h = self.bodies.scroll(p);
                    let o = h.offset();
                    let max = h.max_offset().height;
                    let y = match to_end {
                        Some(false) => px(0.),
                        Some(true) => -max,
                        None => (o.y - px(48. * delta as f32)).clamp(-max, px(0.)),
                    };
                    h.set_offset(gpui::point(o.x, y));
                    self.shell.update(cx, |s, cx| s.refresh_pane(p, cx));
                }
            }
        }
    }

    fn board_col(&mut self, d: isize, cx: &mut Context<Self>) {
        self.model.update(cx, |m, cx| {
            let n = m.board_columns().len() as isize;
            if n > 0 {
                m.board_cursor.0 = (m.board_cursor.0 as isize + d).clamp(0, n - 1) as usize;
                m.board_moved();
                cx.notify();
            }
        });
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        let focus = self.model.read(cx).focus.clone();
        match focus {
            PaneFocus::Filters => self.toggle_filter_row(cx),
            PaneFocus::Notes => self.edit_note(cx),
            _ => {
                let from_list = focus == PaneFocus::Issues;
                if let Some(k) = self.model.read(cx).current_key() {
                    self.open_issue_tab(&k, cx);
                }
                self.advance_after_open(from_list, cx);
            }
        }
    }

    /// With auto-advance on, opening an issue from the List (Enter, o, ⌘Enter, ⌘⇧Enter) moves
    /// the List on to the next issue and keeps the keyboard there, for opening a run of them.
    fn advance_after_open(&mut self, from_list: bool, cx: &mut Context<Self>) {
        if !from_list || !self.model.read(cx).auto_advance {
            return;
        }
        self.model.update(cx, |m, cx| {
            if let Some(row) = m.selected_row()
                && row + 1 < m.visible.len()
            {
                m.select_row(row + 1);
            }
            cx.notify();
        });
        self.focus_pane(placeholder(ISSUES), cx);
    }

    fn toggle_filter_row(&mut self, cx: &mut Context<Self>) {
        self.toggle_filter_row_with(false, cx)
    }

    /// ⇧Space in Filters: a range of sprints (see `Model::range_filter_row`).
    fn range_filter_row(&mut self, cx: &mut Context<Self>) {
        let row = {
            let m = self.model.read(cx);
            m.filter_rows().get(m.filter_cursor).cloned()
        };
        if let Some(row) = row {
            self.model.update(cx, |m, cx| {
                m.range_filter_row(&row);
                cx.notify();
            });
        }
    }

    /// Enter (or a click) on a Filters row; with `add` (Space), a sprint row adds that sprint to
    /// those shown rather than showing only it.
    fn toggle_filter_row_with(&mut self, add: bool, cx: &mut Context<Self>) {
        let (row, more) = {
            let m = self.model.read(cx);
            let rows = m.filter_rows();
            let row = rows.get(m.filter_cursor).cloned();
            let more = match &row {
                Some(state::FilterRow::More(_, w)) => Some(*w),
                _ => None,
            };
            (row, more)
        };
        if let Some(w) = more {
            self.open_picker(
                match w {
                    MoreOf::Sprints => Purpose::FilterSprint,
                    MoreOf::Labels => Purpose::FilterLabel,
                },
                cx,
            );
        } else if let Some(row) = row {
            self.model.update(cx, |m, cx| {
                if add {
                    m.add_filter_row(&row);
                } else {
                    m.toggle_filter_row(&row);
                }
                cx.notify();
            });
        }
    }

    fn edit_note(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.model.read(cx).current_key() else {
            return;
        };
        let notes_shown = self.find_pane(cx, |c| *c == placeholder(NOTES)).is_some();
        self.model.update(cx, |m, cx| {
            if m.note_edit.is_some() {
                save_note(m);
            }
            let text = m.desk.sidecar.note(&key);
            m.note_edit = Some((key.clone(), TextArea::new(&text, true)));
            m.generation += 1;
            cx.notify();
        });
        if !notes_shown {
            self.focus_pane(placeholder(NOTES), cx);
        }
    }

    /// Runs `f` with the current issue's key, or says there is none.
    fn with_issue(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Self, String, &mut Context<Self>),
    ) {
        match self.model.read(cx).current_key() {
            Some(k) if self.model.read(cx).desk.issue(&k).is_some() => f(self, k, cx),
            _ => self.model.update(cx, |m, cx| {
                m.error("No issue selected");
                cx.notify();
            }),
        }
    }

    // ----- rendering ------------------------------------------------------------------------

    fn status_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let left = self.shell.update(cx, |s, cx| s.status_left(window, cx));
        let hint = self.shell.update(cx, |s, cx| s.hint_bar(window, cx));
        let in_mode = self.shell.read(cx).in_layout_mode();
        let shell_notice = self.shell.read(cx).notice();
        // In a focus layout, a quiet reminder of the keys that step through it: [ ] for pages
        // (with more than one) and Tab for the tray (when something waits there).
        let focus_hint = {
            let ws = self.shell.read(cx).workspace();
            ws.focus()
                .map(|f| (f.current + 1, f.pages.len(), !ws.tray().is_empty()))
        }
        .filter(|_| !in_mode)
        .and_then(|(page, pages, tray)| {
            let ctx = "JiraDesk focus";
            let key = |a: &dyn Action| keys::label_in(a, window, ctx);
            let mut parts: Vec<(String, String)> = Vec::new();
            if pages > 1
                && let (Some(prev), Some(next)) = (
                    key(&cho_layout_ui::actions::PrevFocusPage),
                    key(&cho_layout_ui::actions::NextFocusPage),
                )
            {
                parts.push((format!("{prev} {next}"), format!("page {page} of {pages}")));
            }
            if tray && let Some(k) = key(&cho_layout_ui::actions::NextTrayFile) {
                parts.push((k, "next from the tray".into()));
            }
            (!parts.is_empty()).then_some(parts)
        });
        let m = self.model.read(cx);
        let c = m.colors.clone();
        let busy = m.sync.running || m.sync.applying;
        let progress = m
            .sync
            .progress
            .lock()
            .map(|p| p.clone())
            .unwrap_or_default();
        let sync_text = if busy {
            format!("{progress}…")
        } else if let Some(e) = &m.sync.error {
            format!("Offline: {}", e.lines().next().unwrap_or_default())
        } else {
            match m.desk.last_sync {
                Some(t) => format!(
                    "Synced {}{}",
                    state::since(t),
                    m.sync
                        .last_result
                        .as_ref()
                        .map(|r| format!(" · {r}"))
                        .unwrap_or_default()
                ),
                None => "Not synced yet".into(),
            }
        };
        let sync_err = !busy && m.sync.error.is_some();
        let (pending, held) = m.pending_and_held();
        let requests_open = m.desk.requests.open_count();
        let attention = m
            .desk
            .queue
            .open_changes()
            .filter(|ch| ch.state != State::Pending)
            .count()
            + held;
        let notice = m.notice.clone();
        let demo = m.desk.is_demo();
        let site = m.desk.site();
        let refresh_key = keys::label(&Refresh, window);
        let apply_key = keys::label(&ApplyAll, window);
        let help_key = keys::label(&ShowHelp, window);
        let model = self.model.clone();
        div()
            .h(px(26.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(6.))
            .bg(c.status_bar)
            .border_t_1()
            .border_color(c.border)
            .text_size(px(11.5))
            .text_color(c.text_muted)
            .child(left)
            .child(
                div()
                    .id("sync-status")
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .px(px(4.))
                    .rounded(px(4.))
                    .hover(|d| d.bg(c.hover))
                    .child(icon(
                        if sync_err { "cloud-off" } else { "refresh-cw" },
                        12.,
                        if sync_err {
                            c.error.unwrap_or(c.text)
                        } else if busy {
                            c.icon_accent
                        } else {
                            c.icon_muted
                        },
                    ))
                    .child(
                        div()
                            .max_w(px(360.))
                            .truncate()
                            .when(sync_err, |d| d.text_color(c.error.unwrap_or(c.text)))
                            .child(s(sync_text)),
                    )
                    .when_some(refresh_key, |d, k| {
                        d.child(div().text_color(c.icon_disabled).child(s(k)))
                    })
                    .on_click(move |_, _, cx| {
                        model.update(cx, |m, cx| m.start_sync(sync::Mode::Auto, cx));
                    }),
            )
            .when(pending + attention > 0, |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(5.))
                        .px(px(6.))
                        .rounded(px(4.))
                        .bg(c.warning_bg.unwrap_or(c.accent_wash))
                        .text_color(c.text)
                        .child(icon("upload", 12., c.warning.unwrap_or(c.text)))
                        .child(s(if attention > 0 {
                            format!("{pending} to apply · {attention} need attention")
                        } else {
                            format!("{pending} to apply")
                        }))
                        .when_some(apply_key, |d, k| {
                            d.child(div().text_color(c.text_muted).child(s(k)))
                        }),
                )
            })
            // Requests: things to make true outside Jira Desk (f r lists their issues).
            .when(requests_open > 0, |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(5.))
                        .child(icon("flag", 12., c.text_accent))
                        .child(s(format!(
                            "{requests_open} request{} open",
                            if requests_open == 1 { "" } else { "s" }
                        ))),
                )
            })
            .when_some(notice.filter(|_| !in_mode), |d, n| {
                d.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(if n.error {
                            c.error.unwrap_or(c.text)
                        } else {
                            c.text
                        })
                        .child(s(n.text)),
                )
            })
            .when_some(shell_notice, |d, n| {
                d.child(div().text_color(c.warning.unwrap_or(c.text)).child(s(n)))
            })
            .when_some(hint, |d, h| d.child(h))
            .when_some(focus_hint, |d, parts| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .text_color(c.text_muted)
                        .children(parts.into_iter().map(|(k, what)| {
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .child(cho_layout_ui::widgets::key_chip(&c, &k))
                                .child(s(what))
                        })),
                )
            })
            .child(div().flex_1())
            .when(demo, |d| {
                d.child(
                    div()
                        .px(px(6.))
                        .rounded(px(4.))
                        .bg(c.accent_wash)
                        .text_color(c.text)
                        .font_weight(FontWeight::MEDIUM)
                        .child("DEMO"),
                )
            })
            .child(
                div()
                    .max_w(px(220.))
                    .truncate()
                    .child(s(site.trim_start_matches("https://").to_string())),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(icon("keyboard", 12., c.icon_muted))
                    .child(s(format!(
                        "Keys {}",
                        help_key.unwrap_or_else(|| "?".into())
                    ))),
            )
            .into_any_element()
    }

    fn overlay_el(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let c = self.model.read(cx).colors.clone();
        let card = |w: f32| {
            div()
                .w(px(w))
                .max_h(px(560.))
                .flex()
                .flex_col()
                .bg(c.elevated)
                .border_1()
                .border_color(c.border)
                .rounded(px(8.))
                .shadow_lg()
                .overflow_hidden()
        };
        let inner: AnyElement = match self.overlay.as_ref()? {
            Overlay::Picker(p) => {
                let n = p.shown.len();
                let view = cx.entity().downgrade();
                let list = if n == 0 {
                    div()
                        .p(px(12.))
                        .text_color(c.text_muted)
                        .child("No match")
                        .into_any_element()
                } else {
                    let (over, stretch) = {
                        let m = self.model.read(cx);
                        (m.scroll_areas.get("picker").overscroll(), m.stretch())
                    };
                    crate::scroll::uniform_list("picker", n, move |range, _w, cx| {
                        let Some(v) = view.upgrade() else {
                            return Vec::new();
                        };
                        let v = v.read(cx);
                        let Some(Overlay::Picker(p)) = &v.overlay else {
                            return Vec::new();
                        };
                        let c = v.model.read(cx).colors.clone();
                        let multi = p.purpose.multi();
                        range
                            .map(|row| {
                                let (idx, ranges) = &p.shown[row];
                                let it = &p.items[*idx];
                                let hl: Vec<_> = ranges
                                    .iter()
                                    .map(|r| {
                                        (
                                            r.clone(),
                                            gpui::HighlightStyle {
                                                color: Some(c.text_accent),
                                                font_weight: Some(FontWeight::SEMIBOLD),
                                                ..Default::default()
                                            },
                                        )
                                    })
                                    .collect();
                                div()
                                    .h(px(28.))
                                    .w_full()
                                    .px(px(10.))
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .when(row == p.cursor, |d| d.bg(c.ghost_selected))
                                    .child(if multi {
                                        div()
                                            .size(px(11.))
                                            .flex_none()
                                            .rounded(px(2.))
                                            .border_1()
                                            .border_color(if it.checked {
                                                c.text_accent
                                            } else {
                                                c.border
                                            })
                                            .when(it.checked, |d| d.bg(c.text_accent))
                                            .into_any_element()
                                    } else {
                                        div()
                                            .w(px(11.))
                                            .flex_none()
                                            .text_color(c.text_accent)
                                            .child(if it.checked { "✓" } else { "" })
                                            .into_any_element()
                                    })
                                    .child(
                                        div().flex_1().min_w_0().truncate().child(
                                            gpui::StyledText::new(s(it.label.clone()))
                                                .with_highlights(hl),
                                        ),
                                    )
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_size(px(11.))
                                            .text_color(c.text_muted)
                                            .child(s(it.detail.clone())),
                                    )
                                    .into_any_element()
                            })
                            .collect()
                    })
                    .wrap(&over, &p.scroll, stretch)
                    .h(px((n.min(14) as f32) * 28.))
                    .into_any_element()
                };
                card(560.)
                    .child(
                        div()
                            .px(px(12.))
                            .pt(px(10.))
                            .pb(px(4.))
                            .text_size(px(11.))
                            .text_color(c.text_muted)
                            .child(s(p.title.clone())),
                    )
                    .child(
                        div()
                            .px(px(12.))
                            .pb(px(8.))
                            .border_b_1()
                            .border_color(c.border_variant)
                            .flex()
                            .items_center()
                            // Adding issues to a page: the Issues chip goes back to the tray.
                            .when(
                                p.purpose == Purpose::AddToPage && self.tray_to_return_to(cx),
                                |d| {
                                    d.child(
                                        cho_layout_ui::widgets::mode_chip(
                                            &c,
                                            "issues-mode-chip",
                                            "file-text",
                                            "Issues",
                                        )
                                        .mr(px(8.))
                                        .cursor_pointer()
                                        .on_click(
                                            cx.listener(|this, _, window, cx| {
                                                this.back_to_tray(window, cx)
                                            }),
                                        ),
                                    )
                                },
                            )
                            .child(div().flex_1().min_w_0().child(p.input.render(
                                &c,
                                "Type to filter",
                                false,
                            ))),
                    )
                    .child(list)
                    .into_any_element()
            }
            Overlay::Prompt(p) => card(if p.area.multiline { 620. } else { 520. })
                .p(px(12.))
                .gap(px(8.))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(s(p.title.clone())),
                )
                .child(
                    div()
                        .p(px(8.))
                        .rounded(px(4.))
                        .bg(c.editor)
                        .border_1()
                        .border_color(c.text_accent)
                        .when(p.area.multiline, |d| d.min_h(px(140.)))
                        .child(p.area.render(&c, "", p.area.multiline)),
                )
                .when_some(p.error.clone(), |d, e| {
                    d.child(div().text_color(c.error.unwrap_or(c.text)).child(s(e)))
                })
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(c.text_muted)
                        .child(s(p.hint.clone())),
                )
                .into_any_element(),
            Overlay::ConfirmApply(n) => {
                let m = self.model.read(cx);
                let mut body = div().flex().flex_col().gap(px(2.)).text_size(px(12.));
                for ch in m
                    .desk
                    .queue
                    .changes()
                    .iter()
                    .filter(|ch| ch.state == State::Pending && !m.held(&ch.key))
                    .take(12)
                {
                    body = body.child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(
                                div()
                                    .w(px(74.))
                                    .flex_none()
                                    .font_family(c.mono_font.clone())
                                    .text_color(c.text_muted)
                                    .child(s(ch.key.clone())),
                            )
                            .child(div().min_w_0().truncate().child(s(ch.edit.describe()))),
                    );
                }
                if *n > 12 {
                    body = body.child(
                        div()
                            .text_color(c.text_muted)
                            .child(s(format!("and {} more", n - 12))),
                    );
                }
                card(560.)
                    .p(px(14.))
                    .gap(px(10.))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(s(format!(
                                "Apply {n} change{} to {}?",
                                if *n == 1 { "" } else { "s" },
                                m.desk.site().trim_start_matches("https://")
                            ))),
                    )
                    .child(body)
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(c.text_muted)
                            .child("Each issue is read again first: a field someone else changed since you queued yours stops as a conflict instead of being overwritten."),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(c.text)
                            .child("Enter applies · Esc goes back"),
                    )
                    .into_any_element()
            }
            Overlay::Help => {
                let m = self.model.read(cx);
                let area = m
                    .scroll_areas
                    .get("help")
                    .tracking(self.help_scroll.clone());
                help(&c, window, &area, m.stretch())
            }
            Overlay::Request(id) => {
                let m = self.model.read(cx);
                let r = m.desk.requests.get(*id)?;
                let summary = m
                    .effective_issue(&r.key)
                    .map(|i| i.summary.clone())
                    .unwrap_or_default();
                let full = match &r.want {
                    jdesk_core::requests::Want::Description { instructions: t }
                    | jdesk_core::requests::Want::Other { text: t } => Some(t.clone()),
                    _ => None,
                };
                let what = match &r.want {
                    jdesk_core::requests::Want::Description { .. } => {
                        "Rewrite the description".to_string()
                    }
                    jdesk_core::requests::Want::Other { .. } => {
                        "Something else, in your words".to_string()
                    }
                    w => w.describe(),
                };
                let status = crate::panes::request_status(r);
                let body = div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .text_size(px(13.))
                    .line_height(px(19.))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(s(what)))
                    .children(full.map(|t| div().text_color(c.text).child(s(t))))
                    .when(!r.note.is_empty(), |d| {
                        d.child(
                            div()
                                .text_size(px(12.))
                                .text_color(c.text_muted)
                                .child(s(format!("Last word: “{}”", r.note))),
                        )
                    });
                let area = m.scroll_areas.get("request-dialog");
                card(560.)
                    .p(px(14.))
                    .gap(px(10.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .font_family(c.mono_font.clone())
                                    .text_size(px(12.))
                                    .text_color(c.text_muted)
                                    .child(s(format!("Request #{} · {}", r.id, r.key))),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(12.))
                                    .text_color(c.text_muted)
                                    .child(s(summary)),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px(11.5))
                                    .text_color(if r.changed && r.state.is_open() {
                                        c.text_accent
                                    } else {
                                        c.text_muted
                                    })
                                    .child(s(status)),
                            ),
                    )
                    .child(area.vertical_max("request-dialog-body", m.stretch(), px(360.), body))
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(c.text_muted)
                            .child(s(format!("Asked {}", state::since(r.created)))),
                    )
                    .child(crate::panes::request_buttons(
                        &self.model,
                        r,
                        &c,
                        "request-dialog",
                    ))
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(c.text)
                            .child("Esc or Enter closes"),
                    )
                    .into_any_element()
            }
        };
        // Deferred, so it paints over the panes (cached views), as the shell's own pickers do.
        Some(
            gpui::deferred(
                div()
                    .id("overlay")
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .pt(px(70.))
                    .bg(gpui::hsla(0., 0., 0., 0.3))
                    .occlude()
                    .child(inner),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }
}

fn save_note(m: &mut Model) {
    if let Some((k, t)) = m.note_edit.take() {
        match m.desk.set_note(&k, &t.text) {
            Ok(()) => {
                m.rebuild();
                m.note(if t.text.trim().is_empty() {
                    format!("Note on {k} removed")
                } else {
                    format!("Note on {k} saved")
                });
            }
            Err(e) => m.error(format!("Note not saved: {e}")),
        }
    }
}

/// A titled list of actions and what each does, for the help overlay.
type HelpSection = (&'static str, Vec<(Box<dyn Action>, &'static str)>);

const CHANGES_SECTION: &str = "In the Changes pane";
const SETTINGS_SECTION: &str = "In Settings";

/// A palette command by name: the whole name (ignoring case), else the only one containing
/// `want`.
fn find_command(want: &str) -> Result<(String, Box<dyn Action>), String> {
    let w = want.trim().to_lowercase();
    let all: Vec<(String, Box<dyn Action>)> = commands()
        .into_iter()
        .flat_map(|(section, rows)| {
            let group = palette_group(section);
            rows.into_iter()
                .map(move |(a, what)| (format!("{group}: {what}"), a))
        })
        .collect();
    if let Some(i) = all.iter().position(|(n, _)| n.to_lowercase() == w) {
        let (n, a) = &all[i];
        return Ok((n.clone(), a.boxed_clone()));
    }
    let hits: Vec<&(String, Box<dyn Action>)> = all
        .iter()
        .filter(|(n, _)| n.to_lowercase().contains(&w))
        .collect();
    match hits.as_slice() {
        [(n, a)] => Ok((n.clone(), a.boxed_clone())),
        [] => Err(format!(
            "no command called “{want}” (jdesk app commands lists them)"
        )),
        many => Err(format!(
            "“{want}” could be: {}",
            many.iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(" | ")
        )),
    }
}

/// Palette commands `jdesk app run` (and MCP's `app_run`) refuses: they send to Jira or throw
/// away queued work, so they must not be one loose name away from an agent.
fn run_refuses(action: &dyn Action) -> bool {
    let refused: [Box<dyn Action>; 7] = [
        Box::new(ApplyAll),
        Box::new(ApplySelected),
        Box::new(RetryChange),
        Box::new(ForceChange),
        Box::new(DiscardChange),
        Box::new(UndoChange),
        Box::new(ClearHistory),
    ];
    refused.iter().any(|r| r.partial_eq(action))
}

/// Commands that only make sense as keys (moving the cursor): help lists them, the palette
/// doesn't.
const PALETTE_SKIPS: &[&str] = &[
    "jdesk::CursorDown",
    "jdesk::CursorUp",
    "jdesk::CursorTop",
    "jdesk::CursorBottom",
    "jdesk::PageDown",
    "jdesk::Confirm",
];

/// The palette's prefix for each help section, so "filter" finds every filter.
fn palette_group(section: &str) -> &'static str {
    match section {
        "Moving around" => "Go",
        "Panes and splits (as in Chorong)" => "Layout",
        "Filtering the list" => "Filter",
        "Changing an issue (queued until you apply)" => "Change",
        "Local only" => "Issue",
        CHANGES_SECTION => "Changes",
        SETTINGS_SECTION => "Settings",
        _ => "App",
    }
}

/// Every command, by section: the help overlay and the command palette both read this, so
/// they list the same things.
fn commands() -> Vec<HelpSection> {
    let mut sections = commands_fixed();
    if let Some((_, rows)) = sections
        .iter_mut()
        .find(|(t, _)| *t == "Filtering the list")
    {
        for (i, label) in crate::state::chip_labels().into_iter().enumerate() {
            rows.insert(i, (Box::new(ToggleStatusGroup(i + 1)), label));
        }
    }
    sections
}

fn commands_fixed() -> Vec<HelpSection> {
    vec![
        (
            "Moving around",
            vec![
                (Box::new(CursorDown), "Next row (or scroll)"),
                (Box::new(CursorUp), "Previous row"),
                (Box::new(CursorTop), "First row"),
                (Box::new(CursorBottom), "Last row"),
                (Box::new(PageDown), "Page down"),
                (Box::new(Confirm), "Open issue in a tab · toggle a filter"),
                (Box::new(GoToIssue), "Go to issue…"),
                (Box::new(FocusFilters), "Filters pane"),
                (Box::new(FocusIssues), "List pane"),
                (Box::new(FocusPreview), "Preview pane"),
                (Box::new(FocusNotes), "Notes pane"),
                (Box::new(FocusChanges), "Changes pane"),
                (Box::new(ShowSettings), "Settings and storage"),
                (Box::new(ShowPalette), "Command palette (also Shift twice)"),
                (Box::new(ShowHelp), "Keyboard shortcuts"),
                (Box::new(OpenBoard), "Board"),
            ],
        ),
        (
            "Panes and splits (as in Chorong)",
            vec![
                (
                    Box::new(cho_layout_ui::actions::EnterLayoutMode),
                    "Layout mode (as in Chorong): letters jump, arrows focus, ⇧ moves, p move to…, r resizes, v/s split",
                ),
                (
                    Box::new(cho_layout_ui::actions::FocusPaneLeft),
                    "Focus pane to the left (and right, up, down)",
                ),
                (
                    Box::new(cho_layout_ui::actions::SplitPaneRight),
                    "Split right",
                ),
                (
                    Box::new(cho_layout_ui::actions::SplitPaneDown),
                    "Split down",
                ),
                (
                    Box::new(cho_layout_ui::actions::ToggleMaximizePane),
                    "Maximise pane",
                ),
                (Box::new(cho_layout_ui::actions::ClosePane), "Close pane"),
                (
                    Box::new(cho_layout_ui::actions::ToggleLeftDock),
                    "Show/hide left dock",
                ),
                (
                    Box::new(cho_layout_ui::actions::ToggleRightDock),
                    "Show/hide right dock",
                ),
                (
                    Box::new(cho_layout_ui::actions::ToggleBottomBar),
                    "Show/hide bottom bar",
                ),
                (Box::new(cho_layout_ui::actions::FocusPane), "Focus pane…"),
                (
                    Box::new(cho_layout_ui::actions::MovePaneTo),
                    "Move pane to…",
                ),
                (
                    Box::new(cho_layout_ui::actions::MinimizePane),
                    "Minimise pane",
                ),
                (
                    Box::new(cho_layout_ui::actions::NewFocusPage),
                    "New focus page",
                ),
                (
                    Box::new(cho_layout_ui::actions::BringFromTray),
                    "Add to page from the tray or issues…",
                ),
                (
                    Box::new(cho_layout_ui::actions::ReopenFocusLayout),
                    "Reopen the last focus layout",
                ),
                (
                    Box::new(cho_layout_ui::actions::OpenSwitcher),
                    "Recent panes",
                ),
                (
                    Box::new(cho_layout_ui::actions::UndoLayout),
                    "Undo layout change",
                ),
            ],
        ),
        (
            "Filtering the list",
            vec![
                (
                    Box::new(Search),
                    "Search bar: text, and a field word then Tab for a chip",
                ),
                (Box::new(FilterSprint), "Sprint…"),
                (Box::new(FilterAssignee), "Assignee…"),
                (Box::new(ToggleMine), "Only mine"),
                (Box::new(FilterEpic), "Epic…"),
                (Box::new(FilterLabel), "Label…"),
                (Box::new(FilterStatus), "Status…"),
                (Box::new(ToggleHideDone), "Hide done"),
                (Box::new(ToggleStarredOnly), "Only starred"),
                (Box::new(ToggleNotedOnly), "Only with notes"),
                (Box::new(FilterTags), "Your tags…"),
                (Box::new(ToggleReviewOnly), "Only waiting for review"),
                (Box::new(TogglePendingOnly), "Only with queued changes"),
                (Box::new(ChooseShownSprints), "Choose which sprints show…"),
                (Box::new(ToggleRequestsOnly), "Only with open requests"),
                (Box::new(ChooseSort), "Sort…"),
                (Box::new(ClearFilters), "Clear filters"),
            ],
        ),
        (
            "Changing an issue (queued until you apply)",
            vec![
                (Box::new(Transition), "Status…"),
                (Box::new(Assign), "Assign…"),
                (Box::new(AssignToMe), "Assign to me"),
                (Box::new(EditLabels), "Labels…"),
                (Box::new(Comment), "Comment"),
                (Box::new(Rename), "Edit summary"),
                (Box::new(SetPoints), "Story points"),
                (Box::new(SetPriority), "Priority…"),
                (Box::new(MoveToSprint), "Move to sprint…"),
                (Box::new(UndoChange), "Undo the last queued change on it"),
                (Box::new(ApplyAll), "Apply all queued changes…"),
            ],
        ),
        (
            "Local only",
            vec![
                (Box::new(EditNote), "Edit your note (Notes pane)"),
                (Box::new(ToggleStar), "Star"),
                (Box::new(EditTags), "Add or remove tags…"),
                (Box::new(OpenIssueRight), "Open in a new pane to the right"),
                (Box::new(OpenIssueBelow), "Open in a new pane below"),
                (Box::new(LinkIssue), "Link to another issue…"),
                (Box::new(SetParent), "Set parent (epic)…"),
                (
                    Box::new(AddRequest),
                    "Add a request (to do outside Jira Desk)…",
                ),
                (
                    Box::new(CopyRequestForAgent),
                    "Copy its open request for an agent",
                ),
                (Box::new(TogglePick), "Pick for a focus series"),
                (Box::new(ClearPicks), "Clear picks"),
                (
                    Box::new(ToggleAutoAdvance),
                    "Enable or disable auto-advance",
                ),
                (
                    Box::new(FocusSeries),
                    "Open picked (or listed) issues as focus pages",
                ),
                (
                    Box::new(ResolveReview),
                    "Review an issue that changed a lot in Jira",
                ),
                (Box::new(CopyKey), "Copy key"),
                (Box::new(CopyLink), "Copy link"),
                (Box::new(OpenInBrowser), "Open in browser"),
                (
                    Box::new(CopySelection),
                    "Copy the selected text (issue pane)",
                ),
                (
                    Box::new(CopyWithLink),
                    "Copy the selected text with the issue's link (issue pane)",
                ),
                (Box::new(SelectAllText), "Select all its text (issue pane)"),
                (Box::new(Refresh), "Refresh now"),
                (
                    Box::new(FullRefresh),
                    "Full refresh (also drops deleted issues)",
                ),
            ],
        ),
        (
            SETTINGS_SECTION,
            vec![
                (Box::new(MeasureStorage), "Measure storage again"),
                (Box::new(ClearImages), "Clear downloaded images"),
                (Box::new(CopyDataPath), "Copy the data folder's path"),
            ],
        ),
        (
            CHANGES_SECTION,
            vec![
                (Box::new(ApplySelected), "Apply this change"),
                (Box::new(DiscardChange), "Discard it"),
                (Box::new(RetryChange), "Retry a failed one"),
                (Box::new(ForceChange), "Overwrite a conflict"),
                (Box::new(ClearHistory), "Clear the applied history"),
            ],
        ),
    ]
}

/// The keys, read from the keymap (so a remapped key shows as remapped).
fn help(
    c: &Colors,
    window: &Window,
    area: &crate::scroll::ScrollArea,
    stretch: crate::overscroll::Stretch,
) -> AnyElement {
    let sections = commands();
    let mut cols = div().flex().flex_wrap().gap(px(18.)).p(px(16.));
    for (title, rows) in sections {
        let ctx = match title {
            CHANGES_SECTION => "JiraDesk changes",
            SETTINGS_SECTION => "JiraDesk settings",
            _ => "JiraDesk issues",
        };
        let mut sec = div().w(px(380.)).flex().flex_col().gap(px(2.)).child(
            div()
                .pb(px(4.))
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c.text_muted)
                .child(s(title.to_uppercase())),
        );
        for (action, what) in rows {
            let k = keys::label_in(action.as_ref(), window, ctx)
                .or_else(|| keys::label(action.as_ref(), window))
                .unwrap_or_else(|| "—".into());
            sec = sec.child(
                div()
                    .flex()
                    .gap(px(10.))
                    .text_size(px(12.))
                    .child(
                        div()
                            .w(px(110.))
                            .flex_none()
                            .font_family(c.mono_font.clone())
                            .text_size(px(11.5))
                            .text_color(c.text_accent)
                            .child(s(k)),
                    )
                    .child(div().flex_1().min_w_0().child(s(what))),
            );
        }
        cols = cols.child(sec);
    }
    div()
        .w(px(820.))
        .max_h(px(620.))
        .bg(c.elevated)
        .border_1()
        .border_color(c.border)
        .rounded(px(8.))
        .shadow_lg()
        .flex()
        .flex_col()
        .child(
            div()
                .px(px(16.))
                .pt(px(12.))
                .flex()
                .items_center()
                .child(div().font_weight(FontWeight::SEMIBOLD).child("Keyboard"))
                .child(div().flex_1())
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(c.text_muted)
                        .child("↑↓ scroll · Esc closes · remap in keymap.json in the data folder"),
                ),
        )
        .child(
            area.vertical("help", stretch, div().child(cols))
                .flex_1()
                .min_h_0(),
        )
        .into_any_element()
}

impl Focusable for DeskView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

macro_rules! on {
    ($el:expr, $cx:expr, $( $action:ty => $body:expr ),* $(,)?) => {{
        let mut el = $el;
        $(
            el = el.on_action($cx.listener(|this: &mut DeskView, _: &$action, window: &mut Window, cx: &mut Context<DeskView>| {
                #[allow(clippy::redundant_closure_call)]
                ($body)(this, window, cx);
            }));
        )*
        el
    }};
}

impl Render for DeskView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = self.model.read(cx).colors.clone();
        let context = match self.typing(cx) {
            Some(ctx) => ctx.to_string(),
            // No single-key bindings while a held chord waits for its key.
            None if self.chord.is_held() => crate::chord::HELD_CONTEXT.to_string(),
            // A shell picker (Focus pane…, Add to page from the tray…) types a query. Its own
            // context sits deeper, but `JiraDesk` here would still match the app's
            // single-letter keys and take the letters before the query sees them (as in Chorong).
            None if self.shell.read(cx).overlay_open() => "JiraPaneOverlay".to_string(),
            None => format!(
                "JiraDesk {}{}",
                self.model.read(cx).focus.context(),
                if self.shell.read(cx).workspace().in_focus() {
                    " focus"
                } else {
                    ""
                }
            ),
        };
        let status = self.status_bar(window, cx);
        let overlay = self.overlay_el(window, cx);
        let chord_card = self.chord_card(window, cx);
        let root = div()
            .key_context(KeyContext::parse(&context).unwrap_or_default())
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(c.panel)
            .font_family(c.ui_font.clone())
            .text_color(c.text)
            .text_size(px(12.5))
            .on_key_down(cx.listener(Self::on_key))
            // Shift twice quickly opens the command palette (as in Chorong); any key in
            // between cancels it, so typing capitals never does.
            .capture_key_down(cx.listener(|this, _: &KeyDownEvent, _, _| this.shift_taps.key()))
            .on_modifiers_changed(cx.listener(
                |this, e: &gpui::ModifiersChangedEvent, window, cx| {
                    if this
                        .shift_taps
                        .modifiers(&e.modifiers, std::time::Instant::now())
                    {
                        this.open_palette(window, cx);
                    }
                },
            ));
        let root = bind_actions(root, &self.shell);
        let root = root.on_action(cx.listener(
            |this: &mut Self, a: &ToggleStatusGroup, _w, cx: &mut Context<Self>| {
                this.model.update(cx, |m, cx| {
                    if let Some(name) = m.chip_names().get(a.0.saturating_sub(1)) {
                        let name = name.clone();
                        m.toggle_group(&name);
                        m.generation += 1;
                        cx.notify();
                    }
                })
            },
        ));
        let root = on!(root, cx,
            Refresh => |this: &mut Self, _w, cx: &mut Context<Self>| this.model.update(cx, |m, cx| m.start_sync(sync::Mode::Auto, cx)),
            FullRefresh => |this: &mut Self, _w, cx: &mut Context<Self>| this.model.update(cx, |m, cx| m.start_sync(sync::Mode::Full, cx)),
            ApplyAll => |this: &mut Self, _w, cx: &mut Context<Self>| {
                let (n, held) = this.model.read(cx).pending_and_held();
                if n == 0 {
                    this.model.update(cx, |m, cx| {
                        if held > 0 {
                            m.note(format!("Nothing ready: {held} change(s) held until you review (v)"));
                        } else {
                            m.note("Nothing to apply");
                        }
                        cx.notify();
                    });
                } else {
                    this.overlay = Some(Overlay::ConfirmApply(n));
                    cx.notify();
                }
            },
            GoToIssue => |this: &mut Self, _w, cx: &mut Context<Self>| this.open_picker(Purpose::GoToIssue, cx),
            ShowHelp => |this: &mut Self, _w, cx: &mut Context<Self>| { this.overlay = Some(Overlay::Help); cx.notify(); },
            ShowPalette => |this: &mut Self, w, cx: &mut Context<Self>| this.open_palette(w, cx),
            ToggleAutoAdvance => |this: &mut Self, _w, cx: &mut Context<Self>| this.model.update(cx, |m, cx| {
                m.auto_advance = !m.auto_advance;
                // Saved (prefs.json): the choice holds across launches, over config.toml's.
                let mut prefs = state::Prefs::load(&m.desk.dir);
                prefs.auto_advance = Some(m.auto_advance);
                if let Err(e) = prefs.save(&m.desk.dir) {
                    m.error(format!("Auto-advance not saved: {e}"));
                } else {
                    m.note(if m.auto_advance {
                        "Auto-advance on: after a change from the List, the next issue is selected"
                    } else {
                        "Auto-advance off: you stay on the issue you changed"
                    });
                }
                m.generation += 1;
                cx.notify();
            }),
            SearchTab => |this: &mut Self, _w, cx: &mut Context<Self>| this.search_tab(cx),
            CopySelection => |this: &mut Self, _w, cx: &mut Context<Self>| this.copy_selection(false, cx),
            CopyWithLink => |this: &mut Self, _w, cx: &mut Context<Self>| this.copy_selection(true, cx),
            SelectAllText => |this: &mut Self, _w, cx: &mut Context<Self>| this.select_all_text(cx),
            SearchReopen => |this: &mut Self, _w, cx: &mut Context<Self>| this.search_reopen(cx),
            Quit => |_this: &mut Self, _w, cx: &mut Context<Self>| cx.quit(),
            FocusFilters => |this: &mut Self, _w, cx: &mut Context<Self>| this.focus_pane(placeholder(FILTERS), cx),
            FocusIssues => |this: &mut Self, _w, cx: &mut Context<Self>| this.focus_pane(placeholder(ISSUES), cx),
            FocusPreview => |this: &mut Self, _w, cx: &mut Context<Self>| this.focus_pane(preview_content(), cx),
            FocusNotes => |this: &mut Self, _w, cx: &mut Context<Self>| this.focus_pane(placeholder(NOTES), cx),
            FocusChanges => |this: &mut Self, _w, cx: &mut Context<Self>| this.focus_pane(PaneContent::Changes, cx),
            OpenBoard => |this: &mut Self, _w, cx: &mut Context<Self>| this.open_board(cx),
            CursorDown => |this: &mut Self, _w, cx: &mut Context<Self>| this.cursor(1, None, cx),
            CursorUp => |this: &mut Self, _w, cx: &mut Context<Self>| this.cursor(-1, None, cx),
            CursorTop => |this: &mut Self, _w, cx: &mut Context<Self>| this.cursor(0, Some(false), cx),
            CursorBottom => |this: &mut Self, _w, cx: &mut Context<Self>| this.cursor(0, Some(true), cx),
            PageDown => |this: &mut Self, _w, cx: &mut Context<Self>| this.cursor(15, None, cx),
            PageUp => |this: &mut Self, _w, cx: &mut Context<Self>| this.cursor(-15, None, cx),
            CursorLeft => |this: &mut Self, _w, cx: &mut Context<Self>| this.board_col(-1, cx),
            CursorRight => |this: &mut Self, _w, cx: &mut Context<Self>| this.board_col(1, cx),
            Confirm => |this: &mut Self, _w, cx: &mut Context<Self>| this.confirm(cx),
            ToggleRange => |this: &mut Self, _w, cx: &mut Context<Self>| {
                if this.model.read(cx).focus == PaneFocus::Filters { this.range_filter_row(cx) }
            },
            Toggle => |this: &mut Self, _w, cx: &mut Context<Self>| {
                if this.model.read(cx).focus == PaneFocus::Filters { this.toggle_filter_row_with(true, cx) } else { this.cursor(15, None, cx) }
            },
            Search => |this: &mut Self, _w, cx: &mut Context<Self>| {
                this.model.update(cx, |m, cx| {
                    m.search = Some(omni::Omni::new(&m.filter.text));
                    m.generation += 1;
                    cx.notify();
                });
                this.focus_pane(placeholder(ISSUES), cx);
            },
            FilterSprint => |this: &mut Self, _w, cx: &mut Context<Self>| this.open_picker(Purpose::FilterSprint, cx),
            FilterAssignee => |this: &mut Self, _w, cx: &mut Context<Self>| this.open_picker(Purpose::FilterAssignee, cx),
            FilterLabel => |this: &mut Self, _w, cx: &mut Context<Self>| this.open_picker(Purpose::FilterLabel, cx),
            FilterEpic => |this: &mut Self, _w, cx: &mut Context<Self>| this.open_picker(Purpose::FilterEpic, cx),
            FilterStatus => |this: &mut Self, _w, cx: &mut Context<Self>| this.open_picker(Purpose::FilterStatus, cx),
            ChooseSort => |this: &mut Self, _w, cx: &mut Context<Self>| this.open_picker(Purpose::Sort, cx),
            ToggleMine => |this: &mut Self, _w, cx: &mut Context<Self>| this.model.update(cx, |m, cx| { m.toggle_quick(state::Quick::Mine); m.refilter(); cx.notify(); }),
            ToggleHideDone => |this: &mut Self, _w, cx: &mut Context<Self>| this.model.update(cx, |m, cx| { m.toggle_quick(state::Quick::HideDone); m.refilter(); cx.notify(); }),
            ToggleStarredOnly => |this: &mut Self, _w, cx: &mut Context<Self>| this.model.update(cx, |m, cx| { m.toggle_quick(state::Quick::Starred); m.refilter(); cx.notify(); }),
            ToggleNotedOnly => |this: &mut Self, _w, cx: &mut Context<Self>| this.model.update(cx, |m, cx| { m.toggle_quick(state::Quick::Noted); m.refilter(); cx.notify(); }),
            TogglePendingOnly => |this: &mut Self, _w, cx: &mut Context<Self>| this.model.update(cx, |m, cx| { m.toggle_quick(state::Quick::Pending); m.refilter(); cx.notify(); }),
            ToggleRequestsOnly => |this: &mut Self, _w, cx: &mut Context<Self>| this.model.update(cx, |m, cx| { m.toggle_quick(state::Quick::Requests); m.refilter(); cx.notify(); }),
            AddRequest => |this: &mut Self, _w, cx: &mut Context<Self>| this.with_issue(cx, |v, k, cx| v.open_picker(Purpose::RequestKind(k), cx)),
            OpenIssueRight => |this: &mut Self, _w, cx: &mut Context<Self>| {
                let from_list = this.model.read(cx).focus == PaneFocus::Issues;
                if let Some(k) = this.model.read(cx).current_key() { this.open_issue_at(&k, Edge::Right, cx) }
                this.advance_after_open(from_list, cx);
            },
            OpenIssueBelow => |this: &mut Self, _w, cx: &mut Context<Self>| {
                let from_list = this.model.read(cx).focus == PaneFocus::Issues;
                if let Some(k) = this.model.read(cx).current_key() { this.open_issue_at(&k, Edge::Bottom, cx) }
                this.advance_after_open(from_list, cx);
            },
            ChooseShownSprints => |this: &mut Self, _w, cx: &mut Context<Self>| this.open_picker(Purpose::SprintsShown, cx),
            HideSprintRow => |this: &mut Self, _w, cx: &mut Context<Self>| this.model.update(cx, |m, cx| {
                let row = m.filter_rows().get(m.filter_cursor).cloned();
                match row {
                    Some(state::FilterRow::Sprint { sel: jdesk_core::filter::SprintSel::Id(id), label, .. }) => {
                        m.show_sprint_in_filters(id, false);
                        m.note(format!("{label} hidden from Filters (“Choose which sprints show…” in the palette brings it back)"));
                        let n = m.filter_rows().len();
                        m.filter_cursor = m.filter_cursor.min(n.saturating_sub(1));
                    }
                    _ => m.note("h hides a sprint row from Filters; “Choose which sprints show…” picks them all"),
                }
                cx.notify();
            }),
            LinkIssue => |this: &mut Self, _w, cx: &mut Context<Self>| this.with_issue(cx, |v, k, cx| v.open_picker(Purpose::LinkHow(k), cx)),
            SetParent => |this: &mut Self, _w, cx: &mut Context<Self>| this.with_issue(cx, |v, k, cx| v.open_picker(Purpose::Parent(k), cx)),
            CopyRequestForAgent => |this: &mut Self, _w, cx: &mut Context<Self>| {
                let Some(k) = this.model.read(cx).current_key() else { return };
                let newest = this.model.read(cx).desk.requests.for_issue(&k).filter(|r| r.state.is_open()).map(|r| r.id).max();
                match newest {
                    Some(id) => crate::panes::request_act(&this.model, id, "copy", cx),
                    None => this.model.update(cx, |m, cx| { m.note(format!("No open request on {k}: ⇧R adds one")); cx.notify(); }),
                }
            },
            ClearFilters => |this: &mut Self, _w, cx: &mut Context<Self>| this.model.update(cx, |m, cx| { m.filter.clear(); m.refilter(); m.note("Filters cleared (the sprint stays)"); cx.notify(); }),
            Transition => |this: &mut Self, _w, cx: &mut Context<Self>| this.with_issue(cx, |v, k, cx| v.open_picker(Purpose::Transition(k), cx)),
            Assign => |this: &mut Self, _w, cx: &mut Context<Self>| this.with_issue(cx, |v, k, cx| v.open_picker(Purpose::Assign(k), cx)),
            AssignToMe => |this: &mut Self, _w, cx: &mut Context<Self>| this.with_issue(cx, |v, k, cx| v.model.update(cx, |m, cx| {
                match m.desk.myself.clone() {
                    Some(me) => m.queue(&k, Edit::Assign { to: Some(me) }),
                    None => m.error("Not known who you are until the first refresh"),
                }
                cx.notify();
            })),
            EditLabels => |this: &mut Self, _w, cx: &mut Context<Self>| this.with_issue(cx, |v, k, cx| v.open_picker(Purpose::Labels(k), cx)),
            Comment => |this: &mut Self, _w, cx: &mut Context<Self>| this.with_issue(cx, |v, k, cx| v.open_prompt(PromptFor::Comment(k), cx)),
            Rename => |this: &mut Self, _w, cx: &mut Context<Self>| this.with_issue(cx, |v, k, cx| v.open_prompt(PromptFor::Summary(k), cx)),
            SetPoints => |this: &mut Self, _w, cx: &mut Context<Self>| this.with_issue(cx, |v, k, cx| v.open_prompt(PromptFor::Points(k), cx)),
            SetPriority => |this: &mut Self, _w, cx: &mut Context<Self>| this.with_issue(cx, |v, k, cx| v.open_picker(Purpose::Priority(k), cx)),
            MoveToSprint => |this: &mut Self, _w, cx: &mut Context<Self>| this.with_issue(cx, |v, k, cx| v.open_picker(Purpose::Sprint(k), cx)),
            UndoChange => |this: &mut Self, _w, cx: &mut Context<Self>| this.with_issue(cx, |v, k, cx| v.model.update(cx, |m, cx| {
                let last = m.desk.queue.for_issue(&k).filter(|c| c.state == State::Pending).last().map(|c| (c.id, c.edit.describe()));
                match last {
                    Some((id, what)) => {
                        let _ = m.desk.queue.discard(id);
                        m.rebuild();
                        m.note(format!("Dropped {k}: {what}"));
                    }
                    None => m.note(format!("{k} has no queued changes")),
                }
                cx.notify();
            })),
            ToggleStar => |this: &mut Self, _w, cx: &mut Context<Self>| this.with_issue(cx, |v, k, cx| v.model.update(cx, |m, cx| {
                match m.desk.toggle_star(&k) {
                    Ok(on) => { m.rebuild_after_change(&k, true); m.note(if on { format!("Starred {k}") } else { format!("Unstarred {k}") }); }
                    Err(e) => m.error(e.to_string()),
                }
                cx.notify();
            })),
            EditNote => |this: &mut Self, _w, cx: &mut Context<Self>| this.with_issue(cx, |v, _k, cx| v.edit_note(cx)),
            OpenIssueTab => |this: &mut Self, _w, cx: &mut Context<Self>| {
                let from_list = this.model.read(cx).focus == PaneFocus::Issues;
                this.with_issue(cx, |v, k, cx| v.open_issue_tab(&k, cx));
                this.advance_after_open(from_list, cx);
            },
            OpenInBrowser => |this: &mut Self, _w, cx: &mut Context<Self>| this.with_issue(cx, |v, k, cx| {
                let url = v.model.read(cx).desk.browse_url(&k);
                cx.open_url(&url);
            }),
            CopyKey => |this: &mut Self, _w, cx: &mut Context<Self>| this.with_issue(cx, |v, k, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(k.clone()));
                v.model.update(cx, |m, cx| { m.note(format!("Copied {k}")); cx.notify(); });
            }),
            CopyLink => |this: &mut Self, _w, cx: &mut Context<Self>| this.with_issue(cx, |v, k, cx| {
                let url = v.model.read(cx).desk.browse_url(&k);
                cx.write_to_clipboard(ClipboardItem::new_string(url.clone()));
                v.model.update(cx, |m, cx| { m.note(format!("Copied {url}")); cx.notify(); });
            }),
            ApplySelected => |this: &mut Self, _w, cx: &mut Context<Self>| this.model.update(cx, |m, cx| {
                if let Some(id) = m.current_change() { m.start_apply(Some(vec![id]), cx); }
                cx.notify();
            }),
            DiscardChange => |this: &mut Self, _w, cx: &mut Context<Self>| this.model.update(cx, |m, cx| {
                if let Some(id) = m.current_change()
                    && m.desk.queue.discard(id).unwrap_or(false) { m.rebuild(); m.note("Change discarded"); }
                cx.notify();
            }),
            RetryChange => |this: &mut Self, _w, cx: &mut Context<Self>| this.model.update(cx, |m, cx| {
                if let Some(id) = m.current_change() {
                    if m.desk.queue.retry(id, false).unwrap_or(false) { m.rebuild(); m.note("Back in the queue"); }
                    else { m.note("Only a failed change can be retried; a conflict needs ⇧R to overwrite"); }
                }
                cx.notify();
            }),
            ForceChange => |this: &mut Self, _w, cx: &mut Context<Self>| this.model.update(cx, |m, cx| {
                if let Some(id) = m.current_change()
                    && m.desk.queue.retry(id, true).unwrap_or(false) { m.rebuild(); m.note("Back in the queue: applying will overwrite Jira's value"); }
                cx.notify();
            }),
            ClearHistory => |this: &mut Self, _w, cx: &mut Context<Self>| this.model.update(cx, |m, cx| {
                let _ = m.desk.queue.clear_history();
                m.changes_cursor = 0;
                m.rebuild();
                cx.notify();
            }),
            EditTags => |this: &mut Self, _w, cx: &mut Context<Self>| this.with_issue(cx, |v, k, cx| v.open_picker(Purpose::Tags(k), cx)),
            FilterTags => |this: &mut Self, _w, cx: &mut Context<Self>| {
                if this.model.read(cx).desk.tags().is_empty() {
                    this.model.update(cx, |m, cx| { m.note("No tags yet: ⇧T tags the current issue"); cx.notify(); });
                } else {
                    this.open_picker(Purpose::FilterTags, cx);
                }
            },
            ToggleReviewOnly => |this: &mut Self, _w, cx: &mut Context<Self>| this.model.update(cx, |m, cx| { m.toggle_quick(state::Quick::Review); m.refilter(); cx.notify(); }),
            TogglePick => |this: &mut Self, _w, cx: &mut Context<Self>| this.with_issue(cx, |v, k, cx| v.model.update(cx, |m, cx| {
                m.toggle_pick(&k);
                let n = m.picked.len();
                let on = m.picked.contains(&k);
                m.note(format!("{} {k} · {n} picked · ⇧F opens them as a focus series", if on { "Picked" } else { "Unpicked" }));
                // Move on, so x x x picks a run of issues.
                if on && m.focus == PaneFocus::Issues
                    && let Some(row) = m.selected_row() { m.select_row(row + 1); }
                cx.notify();
            })),
            ClearPicks => |this: &mut Self, _w, cx: &mut Context<Self>| this.model.update(cx, |m, cx| { m.picked.clear(); m.generation += 1; m.note("No issues picked"); cx.notify(); }),
            FocusSeries => |this: &mut Self, _w, cx: &mut Context<Self>| this.open_series(cx),
            ResolveReview => |this: &mut Self, _w, cx: &mut Context<Self>| {
                let key = this.model.read(cx).current_key().filter(|k| this.model.read(cx).held(k))
                    .or_else(|| this.model.read(cx).desk.identities.open_reviews().next().map(|r| r.key.clone()));
                match key {
                    Some(k) => this.open_picker(Purpose::Resolve(k), cx),
                    None => this.model.update(cx, |m, cx| { m.note("No issues waiting for review"); cx.notify(); }),
                }
            },
            ShowSettings => |this: &mut Self, _w, cx: &mut Context<Self>| {
                this.focus_pane(placeholder(SETTINGS), cx);
                this.model.update(cx, |m, cx| m.measure_storage(cx));
            },
            MeasureStorage => |this: &mut Self, _w, cx: &mut Context<Self>| this.model.update(cx, |m, cx| m.measure_storage(cx)),
            ClearImages => |this: &mut Self, _w, cx: &mut Context<Self>| this.model.update(cx, |m, cx| m.clear_images(cx)),
            CopyDataPath => |this: &mut Self, _w, cx: &mut Context<Self>| {
                let path = this.model.read(cx).desk.dir.display().to_string();
                cx.write_to_clipboard(ClipboardItem::new_string(path.clone()));
                this.model.update(cx, |m, cx| { m.note(format!("Copied {path}")); cx.notify(); });
            },
            InputUp => |this: &mut Self, _w, cx: &mut Context<Self>| this.input_move(-1, cx),
            InputDown => |this: &mut Self, _w, cx: &mut Context<Self>| this.input_move(1, cx),
        );
        root.on_action(cx.listener(Self::input_cancel))
            .on_action(cx.listener(Self::input_confirm))
            .on_action(cx.listener(Self::input_confirm_alt))
            // From the layout shell: Tab in Bring from tray…, or New Focus Page with an empty
            // tray. Issues take the place of Chorong's files.
            .on_action(
                cx.listener(|this, _: &cho_layout_ui::actions::AddFileToPage, _, cx| {
                    this.overlay = None;
                    this.open_picker(Purpose::AddToPage, cx);
                }),
            )
            .on_action(cx.listener(Self::input_submit))
            .child(div().flex_1().min_h_0().flex().child(self.shell.clone()))
            .child(status)
            .children(overlay)
            .children(chord_card)
            // A click closes a held chord.
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.chord.clear() {
                        cx.notify();
                    }
                }),
            )
    }
}

/// A key typed into the search bar. Backspace at the start selects the last chip, and again
/// removes it; in an empty open chip it cancels the chip. `field:` opens a chip.
fn search_key(m: &mut Model, k: &gpui::Keystroke, paste: Option<&str>) -> Edited {
    let last_chip = omni::chips(m).pop();
    let Some(o) = m.search.as_mut() else {
        return Edited::No;
    };
    let at_start = o.input.cursor == 0 && !o.input.all;
    if k.key == "backspace" && !k.modifiers.modified() && at_start {
        if o.open.is_some() {
            if o.input.text.is_empty() {
                o.close_chip();
            }
        } else if o.chip_selected {
            o.chip_selected = false;
            if let Some((f, id, _)) = last_chip {
                omni::remove_chip(m, f, &id);
            }
        } else if last_chip.is_some() {
            o.chip_selected = true;
        }
        return Edited::Moved;
    }
    let e = o.input.key(k, paste);
    if e != Edited::No {
        o.chip_selected = false;
    }
    if e == Edited::Text {
        if o.open.is_some() {
            o.cursor = Some(0);
        } else {
            o.colon_opens();
            if o.open.is_none() {
                o.cursor = None;
            }
            m.filter.text = o.text().to_string();
            m.refilter();
        }
    }
    e
}

/// Takes the open chip's highlighted value (toggling it, as the `f` pickers do) and goes back
/// to the free text.
fn search_take(m: &mut Model) {
    let Some(o) = m.search.as_ref() else {
        return;
    };
    let Some(field) = o.open else {
        return;
    };
    let sugg = omni::suggestions(m, o);
    let id = match sugg.get(o.cursor.unwrap_or(0)) {
        Some(omni::Suggestion::Value(item)) => Some(item.id.clone()),
        _ => None,
    };
    if let Some(id) = id {
        omni::apply_filter(m, &field.purpose(), &id);
    }
    if let Some(o) = m.search.as_mut() {
        o.close_chip();
    }
}

#[cfg(test)]
mod layout_migration_tests {
    use super::rename_old_panes;

    #[test]
    fn old_pane_names_become_list_and_preview_everywhere() {
        let mut v = serde_json::json!({
            "panes": [
                {"id": 3, "kind": "placeholder", "name": "Issues"},
                {"id": 4, "kind": "editor", "path": "Issue", "caret": 0},
                {"id": 5, "kind": "editor", "path": "DEMO-84", "caret": 0},
                {"id": 6, "kind": "placeholder", "name": "Notes"}
            ],
            "focus_history": [{"maximized": {"kind": "placeholder", "name": "Issues"}}]
        });
        assert!(rename_old_panes(&mut v));
        assert_eq!(v["panes"][0]["name"], "List");
        assert_eq!(v["panes"][1]["path"], "Preview");
        assert_eq!(v["panes"][2]["path"], "DEMO-84");
        assert_eq!(v["panes"][3]["name"], "Notes");
        assert_eq!(v["focus_history"][0]["maximized"]["name"], "List");
        assert!(!rename_old_panes(&mut v), "a second pass changes nothing");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_from_outside_never_sends_to_jira() {
        for name in [
            "apply this change",
            "Apply all queued changes",
            "overwrite a conflict",
            "retry a failed",
            "discard it",
            "undo the last queued",
            "clear the applied history",
        ] {
            let (_, a) = find_command(name).unwrap();
            assert!(run_refuses(a.as_ref()), "{name}");
        }
        for name in ["hide done", "Changes pane", "refresh now", "Issue: Star"] {
            let (_, a) = find_command(name).unwrap();
            assert!(!run_refuses(a.as_ref()), "{name}");
        }
    }
}
