//! What each pane draws. The layout shell (Chorong's) owns docks, splits, tabs and headers;
//! it asks [`Bodies`] for the inside of each pane.
//!
//! | Pane | Content | Shows |
//! | --- | --- | --- |
//! | Filters | `Placeholder("Filters")` | sprints, quick filters, assignees, epics, statuses, labels |
//! | List | `Placeholder("List")` | the filtered list |
//! | Preview | `Editor { path: "Preview" }` | the list's selection, in full |
//! | DEMO-12 | `Editor { path: "DEMO-12" }` | one issue, in its own tab |
//! | Notes | `Placeholder("Notes")` | your local note on the current issue |
//! | Changes | `Changes` | the queue and its history |
//! | Board | `Placeholder("Board")` | the list as status columns |

use crate::assets::icon;
use crate::state::{self, FilterRow, Model, PaneFocus};
use cho_layout::{PaneContent, PaneId, Pin, Workspace};
use cho_layout_ui::colors::Colors;
use cho_layout_ui::shell::PaneBodies;
use gpui::{
    AnyElement, App, Entity, FontWeight, Hsla, InteractiveElement, IntoElement, ParentElement,
    ScrollHandle, SharedString, StatefulInteractiveElement, Styled, Window, div,
    prelude::FluentBuilder, px,
};
use jdesk_core::model::{Issue, StatusCategory};
use jdesk_core::queue::State;
use std::cell::RefCell;
use std::collections::HashMap;

pub struct Bodies {
    pub model: Entity<Model>,
    /// Each issue pane's scroll position, so j/k can scroll it.
    pub scrolls: RefCell<HashMap<PaneId, ScrollHandle>>,
}

impl Bodies {
    pub fn scroll(&self, pane: PaneId) -> ScrollHandle {
        self.scrolls.borrow_mut().entry(pane).or_default().clone()
    }
}

pub fn s(t: impl Into<String>) -> SharedString {
    SharedString::from(t.into())
}

pub const ROW: f32 = 26.;

pub fn status_colors(c: &Colors, cat: StatusCategory) -> (Hsla, Hsla) {
    match cat {
        StatusCategory::ToDo => (c.text_muted, c.element_bg),
        StatusCategory::InProgress => (c.text_accent, c.accent_wash),
        StatusCategory::Done => (
            c.success.unwrap_or(c.text),
            c.success_bg.unwrap_or(c.element_bg),
        ),
    }
}

pub fn status_chip(c: &Colors, name: &str, cat: StatusCategory) -> AnyElement {
    let (fg, bg) = status_colors(c, cat);
    div()
        .flex_none()
        .px(px(6.))
        .h(px(18.))
        .flex()
        .items_center()
        .rounded(px(4.))
        .bg(bg)
        .text_color(fg)
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .child(s(name.to_uppercase()))
        .into_any_element()
}

pub fn kind_mark(c: &Colors, kind: &str) -> AnyElement {
    let (letter, color) = match kind.to_ascii_lowercase().as_str() {
        "bug" => ("B", c.error.unwrap_or(c.text)),
        "story" => ("S", c.success.unwrap_or(c.text)),
        "task" => ("T", c.text_accent),
        "epic" => ("E", c.kind_enum),
        "spike" => ("K", c.warning.unwrap_or(c.text)),
        "sub-task" | "subtask" => ("s", c.text_muted),
        _ => ("•", c.text_muted),
    };
    div()
        .flex_none()
        .size(px(16.))
        .rounded(px(3.))
        .border_1()
        .border_color(color)
        .text_color(color)
        .text_size(px(10.))
        .font_weight(FontWeight::BOLD)
        .flex()
        .items_center()
        .justify_center()
        .child(letter)
        .into_any_element()
}

fn label_chip(c: &Colors, l: &str) -> AnyElement {
    div()
        .flex_none()
        .px(px(5.))
        .h(px(17.))
        .flex()
        .items_center()
        .rounded(px(4.))
        .border_1()
        .border_color(c.border_variant)
        .text_color(c.text_muted)
        .text_size(px(11.))
        .child(s(l))
        .into_any_element()
}

/// One of your own tags: filled, so it reads differently from Jira's labels.
pub fn tag_chip(c: &Colors, t: &str) -> AnyElement {
    div()
        .flex_none()
        .px(px(5.))
        .h(px(17.))
        .flex()
        .items_center()
        .rounded(px(4.))
        .bg(c.accent_wash)
        .text_color(c.text_accent)
        .text_size(px(11.))
        .child(s(format!("#{t}")))
        .into_any_element()
}

fn points(p: Option<f64>) -> String {
    match p {
        Some(v) if v.fract() == 0.0 => format!("{v:.0}"),
        Some(v) => format!("{v}"),
        None => String::new(),
    }
}

fn short_name(name: &str) -> String {
    let mut parts = name.split_whitespace();
    match (parts.next(), parts.next_back()) {
        (Some(first), Some(last)) => format!("{first} {}.", last.chars().next().unwrap_or(' ')),
        (Some(first), None) => first.to_string(),
        _ => String::new(),
    }
}

fn empty(c: &Colors, title: &str, hint: &str) -> AnyElement {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .p(px(16.))
        .child(div().text_color(c.text).child(s(title)))
        .child(
            div()
                .text_color(c.text_muted)
                .text_size(px(12.))
                .text_center()
                .child(s(hint)),
        )
        .into_any_element()
}

impl PaneBodies for Bodies {
    /// The tray's "Add to page" picker: "iss" or "ji" then Tab switches it to issues (the
    /// shell then asks for our issue picker with AddFileToPage).
    fn tray_tab_mode(&self) -> cho_layout_ui::shell::TrayTabMode {
        cho_layout_ui::shell::TrayTabMode::new("issues").with_alias("jira")
    }

    fn body(
        &self,
        pane: PaneId,
        content: &PaneContent,
        _ws: &Workspace,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        match PaneFocus::of(Some(content)) {
            PaneFocus::Filters => filters(&self.model, cx),
            PaneFocus::Issues => issue_list(&self.model, cx),
            PaneFocus::Preview => {
                let key = self.model.read(cx).selected.clone();
                match key {
                    Some(k) => issue_detail(
                        &self.model,
                        &k,
                        &pane_name(pane),
                        self.scroll(pane),
                        window,
                        cx,
                    ),
                    None => empty(
                        &self.model.read(cx).colors,
                        "No issue selected",
                        "Pick one in the list (j/k)",
                    ),
                }
            }
            PaneFocus::Issue(k) => issue_detail(
                &self.model,
                &k,
                &pane_name(pane),
                self.scroll(pane),
                window,
                cx,
            ),
            PaneFocus::Notes => notes(&self.model, self.scroll(pane), cx),
            PaneFocus::Changes => changes(&self.model, cx),
            PaneFocus::Board => board(&self.model, cx),
            PaneFocus::Settings => settings(&self.model, self.scroll(pane), cx),
            PaneFocus::Other => empty(
                &self.model.read(cx).colors,
                &content.title(),
                "Not used by Jira Desk. Close it with ⌘W.",
            ),
        }
    }

    fn qualifier(&self, _pane: PaneId, content: &PaneContent, cx: &App) -> String {
        let m = self.model.read(cx);
        match PaneFocus::of(Some(content)) {
            PaneFocus::Filters => {
                if m.filter.is_narrowed() {
                    "narrowed".into()
                } else {
                    String::new()
                }
            }
            PaneFocus::Issues => {
                let pts: f64 = m
                    .visible
                    .iter()
                    .filter_map(|i| m.effective[*i].story_points)
                    .sum();
                match state::points_total(pts) {
                    p if p.is_empty() => format!("{} of {}", m.visible.len(), m.effective.len()),
                    p => format!("{} of {} · {p}", m.visible.len(), m.effective.len()),
                }
            }
            PaneFocus::Preview => m.selected.clone().unwrap_or_default(),
            PaneFocus::Issue(k) => m
                .effective_issue(&k)
                .map(|i| i.status.clone())
                .unwrap_or_else(|| "not in the local copy".into()),
            PaneFocus::Notes => match &m.note_edit {
                Some((k, _)) => format!("{k} · editing"),
                None => m
                    .notes_for
                    .clone()
                    .or_else(|| m.current_key())
                    .unwrap_or_default(),
            },
            PaneFocus::Changes => {
                let (n, held) = m.pending_and_held();
                let bad = m
                    .desk
                    .queue
                    .open_changes()
                    .filter(|c| c.state != State::Pending)
                    .count()
                    + held;
                match (n, bad) {
                    (0, 0) => String::new(),
                    (n, 0) => format!("{n} to apply"),
                    (n, b) => format!("{n} to apply · {b} need attention"),
                }
            }
            PaneFocus::Board => format!("{} issues", m.visible.len()),
            PaneFocus::Settings => m
                .usage
                .as_ref()
                .map(|u| jdesk_core::storage::human(u.total))
                .unwrap_or_default(),
            PaneFocus::Other => String::new(),
        }
    }

    fn qualifier_warns(&self, _pane: PaneId, content: &PaneContent, cx: &App) -> bool {
        content == &PaneContent::Changes
            && self
                .model
                .read(cx)
                .desk
                .queue
                .open_changes()
                .any(|c| c.state != State::Pending)
    }

    fn pin(&self, _n: usize, _cx: &App) -> Option<Pin> {
        None
    }

    /// In a focus series, each page's chip is its issue key.
    fn page_chip(
        &self,
        index: usize,
        ws: &Workspace,
        cx: &App,
    ) -> Option<cho_layout_ui::shell::PageChip> {
        let m = self.model.read(cx);
        let key = m.series.get(index)?;
        let current = ws.focus().is_some_and(|f| f.current == index);
        let summary = m
            .effective_issue(key)
            .map(|i| i.summary.clone())
            .unwrap_or_default();
        Some(cho_layout_ui::shell::PageChip {
            label: s(key.clone()),
            look: if current {
                cho_layout_ui::shell::ChipLook::Current
            } else {
                cho_layout_ui::shell::ChipLook::ToDo
            },
            edited: false,
            divider_before: false,
            tooltip: Some(s(summary)),
        })
    }
}

// ----- Filters --------------------------------------------------------------------------------

fn filters(model: &Entity<Model>, cx: &mut App) -> AnyElement {
    let m = model.read(cx);
    let c = m.colors.clone();
    let focused = m.focus == PaneFocus::Filters;
    let rows = m.filter_rows();
    let cursor = m.filter_cursor;
    let mut list = div().flex().flex_col().py(px(4.));
    for (n, row) in rows.into_iter().enumerate() {
        let selected = focused && n == cursor;
        let el = match &row {
            FilterRow::Header(h) => div()
                .px(px(10.))
                .pt(px(if n == 0 { 2. } else { 10. }))
                .pb(px(3.))
                .text_size(px(10.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c.text_muted)
                .child(s(h.to_uppercase()))
                .into_any_element(),
            _ => {
                let (radio, on, label, detail, count, swatch): (
                    bool,
                    bool,
                    String,
                    String,
                    Option<usize>,
                    Option<Hsla>,
                ) = match &row {
                    FilterRow::Sprint {
                        label,
                        detail,
                        count,
                        on,
                        ..
                    } => (true, *on, label.clone(), detail.clone(), Some(*count), None),
                    FilterRow::Quick { label, on, .. } => {
                        (false, *on, label.to_string(), String::new(), None, None)
                    }
                    FilterRow::Assignee {
                        name, count, on, ..
                    } => (false, *on, name.clone(), String::new(), Some(*count), None),
                    FilterRow::Epic {
                        key,
                        summary,
                        count,
                        on,
                    } => (
                        false,
                        *on,
                        if key.is_empty() {
                            "No epic".into()
                        } else {
                            summary.clone()
                        },
                        key.clone(),
                        Some(*count),
                        None,
                    ),
                    FilterRow::Label { label, count, on } => {
                        (false, *on, label.clone(), String::new(), Some(*count), None)
                    }
                    FilterRow::Tag { tag, count, on } => (
                        false,
                        *on,
                        format!("#{tag}"),
                        String::new(),
                        Some(*count),
                        Some(c.text_accent),
                    ),
                    FilterRow::Status {
                        name,
                        category,
                        count,
                        on,
                    } => (
                        false,
                        *on,
                        name.clone(),
                        String::new(),
                        Some(*count),
                        Some(status_colors(&c, *category).0),
                    ),
                    FilterRow::More(label, _) => {
                        (false, false, label.to_string(), String::new(), None, None)
                    }
                    FilterRow::Header(_) => unreachable!(),
                };
                let more = matches!(row, FilterRow::More(..));
                let mark = if more {
                    div().w(px(14.)).flex_none()
                } else if radio {
                    div()
                        .size(px(10.))
                        .flex_none()
                        .rounded(px(5.))
                        .border_1()
                        .border_color(if on { c.text_accent } else { c.border })
                        .when(on, |d| d.bg(c.text_accent))
                } else {
                    div()
                        .size(px(10.))
                        .flex_none()
                        .rounded(px(2.))
                        .border_1()
                        .border_color(if on { c.text_accent } else { c.border })
                        .when(on, |d| d.bg(c.text_accent))
                };
                let model2 = model.clone();
                let row2 = row.clone();
                div()
                    .id(("filter-row", n))
                    .h(px(24.))
                    .mx(px(4.))
                    .px(px(6.))
                    .rounded(px(4.))
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .when(selected, |d| d.bg(c.ghost_selected))
                    .hover(|d| d.bg(c.hover))
                    .child(mark)
                    .when_some(swatch, |d, col| {
                        d.child(div().size(px(6.)).rounded(px(3.)).flex_none().bg(col))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(if more { c.text_accent } else { c.text })
                            .when(on, |d| d.font_weight(FontWeight::MEDIUM))
                            .child(s(label)),
                    )
                    .when(!detail.is_empty(), |d| {
                        d.child(
                            div()
                                .flex_none()
                                .text_size(px(11.))
                                .text_color(c.text_muted)
                                .child(s(detail)),
                        )
                    })
                    .when_some(count, |d, n| {
                        d.child(
                            div()
                                .flex_none()
                                .min_w(px(22.))
                                .text_right()
                                .text_size(px(11.))
                                .text_color(c.text_muted)
                                .child(s(n.to_string())),
                        )
                    })
                    // ⌘-click (Ctrl off macOS) adds a sprint to those shown, ⇧-click takes every
                    // sprint from the last one clicked; a plain click shows only it.
                    .on_click(move |e, window, cx| {
                        let add = e.modifiers().secondary();
                        let range = e.modifiers().shift;
                        model2.update(cx, |m, cx| {
                            m.filter_cursor = n;
                            if let FilterRow::More(_, which) = &row2 {
                                m.pending_more = Some(*which);
                            } else if range {
                                m.range_filter_row(&row2);
                            } else if add {
                                m.add_filter_row(&row2);
                            } else {
                                m.toggle_filter_row(&row2);
                            }
                            cx.notify();
                        });
                        let _ = window;
                    })
                    .into_any_element()
            }
        };
        list = list.child(el);
    }
    let m = model.read(cx);
    m.scroll_areas
        .get("filters")
        .vertical(
            "filters",
            m.stretch(),
            div().text_size(px(12.5)).child(list),
        )
        .size_full()
        .into_any_element()
}

// ----- Issues ---------------------------------------------------------------------------------

fn issue_list(model: &Entity<Model>, cx: &mut App) -> AnyElement {
    let m = model.read(cx);
    let c = m.colors.clone();
    let count = m.visible.len();
    let summary = m.filter_summary();
    let search = m.search.clone();
    let text = m.filter.text.clone();
    let scroll = m.list_scroll.clone();
    let sort = m.filter.sort.label();
    let syncing_empty = m.effective.is_empty();
    let header = div()
        .flex_none()
        .min_h(px(30.))
        .px(px(10.))
        .py(px(4.))
        .flex()
        .items_center()
        .gap(px(8.))
        .border_b_1()
        .border_color(c.border_variant)
        .child(icon("filter", 13., c.icon_muted))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(c.text_muted)
                .text_size(px(12.))
                .child(s(summary)),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(4.))
                .text_size(px(11.))
                .text_color(c.text_muted)
                .child(icon("arrow-up-down", 11., c.icon_muted))
                .child(s(sort)),
        );
    // Status chips: one per Jira status category, with how many each would show.
    let chosen = m.filter.groups.clone();
    let counts = m.facets.groups.clone();
    let mut status_row = div()
        .flex_none()
        .px(px(10.))
        .py(px(4.))
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(6.))
        .border_b_1()
        .border_color(c.border_variant);
    for (gi, name) in m.chip_names().into_iter().enumerate() {
        let on = chosen.contains(&name);
        let (fg, bg) = status_colors(&c, m.chip_category(&name));
        let n = counts.get(&name).copied().unwrap_or(0);
        let model2 = model.clone();
        status_row = status_row.child(
            div()
                .id(("status-chip", gi))
                .flex_none()
                .h(px(20.))
                .px(px(7.))
                .flex()
                .items_center()
                .gap(px(5.))
                .rounded(px(10.))
                .border_1()
                .border_color(if on { fg } else { c.border_variant })
                .bg(if on { bg } else { gpui::transparent_black() })
                .text_size(px(11.))
                .cursor_pointer()
                .hover(|d| d.bg(c.hover))
                .child(div().size(px(7.)).rounded_full().bg(fg))
                .child(
                    div()
                        .text_color(if on { fg } else { c.text })
                        .child(s(name.clone())),
                )
                .child(div().text_color(c.text_muted).child(s(n.to_string())))
                .on_click(move |_, _, cx| {
                    model2.update(cx, |m, cx| {
                        m.toggle_group(&name);
                        m.generation += 1;
                        cx.notify();
                    });
                }),
        );
    }
    let chips = crate::omni::chips(m);
    let suggestions = search.as_ref().map(|o| crate::omni::suggestions(m, o));
    let search_bar = match &search {
        Some(o) => Some(search_bar(&c, o, &chips, suggestions.unwrap_or_default())),
        None if !text.trim().is_empty() => Some(
            div()
                .flex_none()
                .px(px(10.))
                .py(px(4.))
                .flex()
                .items_center()
                .gap(px(8.))
                .border_b_1()
                .border_color(c.border_variant)
                .child(cho_layout_ui::widgets::small_icon("search", c.icon_muted))
                .child(div().flex_1().text_color(c.text).child(s(text)))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(c.text_muted)
                        .child("/ to change"),
                )
                .into_any_element(),
        ),
        None => None,
    };
    let body = if count == 0 {
        if syncing_empty {
            empty(
                &c,
                "Nothing here yet",
                "The first refresh is downloading your issues…",
            )
        } else {
            empty(
                &c,
                "No issues match",
                "f x clears the filters · f s picks another sprint",
            )
        }
    } else {
        let model2 = model.clone();
        let over = m.scroll_areas.get("issue-list").overscroll();
        crate::scroll::uniform_list("issue-list", count, move |range, _window, cx| {
            let m = model2.read(cx);
            let focused = m.focus == PaneFocus::Issues;
            range
                .map(|row| issue_row(&model2, m, row, focused))
                .collect::<Vec<_>>()
        })
        .wrap(&over, &scroll, m.stretch())
        .flex_1()
        .min_h_0()
        .into_any_element()
    };
    div()
        .size_full()
        .flex()
        .flex_col()
        .text_size(px(12.5))
        .child(header)
        .child(status_row)
        .children(search_bar)
        .child(body)
        .into_any_element()
}

fn issue_row(model: &Entity<Model>, m: &Model, row: usize, focused: bool) -> AnyElement {
    let c = &m.colors;
    let idx = m.visible[row];
    let i = &m.effective[idx];
    let f = &m.flags[idx];
    let picked = m.picked.contains(&i.key);
    let selected = m.selected.as_deref() == Some(i.key.as_str());
    let key = i.key.clone();
    let model2 = model.clone();
    let raw_status = m
        .desk
        .issue(&i.key)
        .map(|r| r.status != i.status)
        .unwrap_or(false);
    div()
        .id(("issue-row", row))
        .h(px(ROW))
        .w_full()
        .overflow_hidden()
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(8.))
        .when(selected && focused, |d| d.bg(c.ghost_selected))
        .when(selected && !focused, |d| d.bg(c.ghost_active))
        .hover(|d| d.bg(c.hover))
        .child(
            div()
                .w(px(30.))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(2.))
                .when(picked, |d| {
                    d.child(
                        div()
                            .size(px(9.))
                            .rounded(px(2.))
                            .bg(c.text_accent)
                            .flex_none(),
                    )
                })
                .when(f.review, |d| {
                    d.child(icon("circle-alert", 11., c.error.unwrap_or(c.text)))
                })
                .when(f.pending && !f.review, |d| {
                    d.child(
                        div()
                            .size(px(7.))
                            .rounded(px(4.))
                            .bg(c.warning.unwrap_or(c.text_accent)),
                    )
                })
                .when(f.starred, |d| {
                    d.child(icon("star", 11., c.warning.unwrap_or(c.text_accent)))
                })
                .when(f.noted, |d| d.child(icon("sticky-note", 11., c.icon_muted)))
                .when(f.requests, |d| d.child(icon("flag", 11., c.text_accent))),
        )
        .child(
            div()
                .w(px(74.))
                .flex_none()
                .font_family(c.mono_font.clone())
                .text_size(px(11.5))
                .text_color(c.text_muted)
                .child(s(i.key.clone())),
        )
        .child(kind_mark(c, &i.kind))
        .child(
            // Summary, then labels; when narrow the labels are clipped, never the status.
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(if i.status_category == StatusCategory::Done {
                            c.text_muted
                        } else {
                            c.text
                        })
                        .child(s(i.summary.clone())),
                )
                // One tag (and how many more), so the summary keeps its room.
                .children(f.tags.first().map(|t| {
                    let more = f.tags.len() - 1;
                    div()
                        .flex_none()
                        .max_w(px(110.))
                        .overflow_hidden()
                        .flex()
                        .gap(px(3.))
                        .child(tag_chip(c, t))
                        .when(more > 0, |d| {
                            d.child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(c.text_accent)
                                    .child(s(format!("+{more}"))),
                            )
                        })
                }))
                .children(i.labels.first().map(|l| {
                    div()
                        .flex_none()
                        .max_w(px(96.))
                        .overflow_hidden()
                        .child(label_chip(c, l))
                })),
        )
        .child(
            div()
                .w(px(96.))
                .flex_none()
                .flex()
                .when(raw_status, |d| {
                    d.border_1()
                        .border_color(c.warning.unwrap_or(c.text_accent))
                        .rounded(px(5.))
                })
                .child(status_chip(c, &i.status, i.status_category)),
        )
        .child(
            div()
                .w(px(22.))
                .flex_none()
                .text_right()
                .text_size(px(11.))
                .text_color(c.text_muted)
                .child(s(points(i.story_points))),
        )
        .child(
            div()
                .w(px(86.))
                .flex_none()
                .truncate()
                .text_size(px(11.5))
                .text_color(if i.assignee.is_some() {
                    c.text_muted
                } else {
                    c.icon_disabled
                })
                .child(s(i
                    .assignee
                    .as_ref()
                    .map_or("Unassigned".into(), |p| short_name(&p.name)))),
        )
        .on_click(move |e, window, cx| {
            let double = e.click_count() >= 2;
            model2.update(cx, |m, cx| {
                m.select_key(&key);
                if double {
                    m.pending_open = Some(key.clone());
                }
                cx.notify();
            });
            let _ = window;
        })
        .into_any_element()
}

// ----- Issue ----------------------------------------------------------------------------------

/// The red banner on an issue that changed a lot in Jira since you attached things to it.
fn review_banner(c: &Colors, m: &Model, key: &str) -> Option<AnyElement> {
    let r = m.desk.identities.open_review(key)?;
    let red = c.error.unwrap_or(c.text);
    let held = m.desk.queue.for_issue(key).count();
    let local = m.desk.sidecar.local(key);
    let mut yours: Vec<String> = Vec::new();
    if held > 0 {
        yours.push(format!(
            "{held} queued change{} (held)",
            if held == 1 { "" } else { "s" }
        ));
    }
    if m.desk.sidecar.has_note(key) {
        yours.push("your note".into());
    }
    if local.starred {
        yours.push("a star".into());
    }
    if !local.tags.is_empty() {
        yours.push(format!(
            "tags {}",
            local
                .tags
                .iter()
                .map(|t| format!("#{t}"))
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    let line = |label: &str, kind: &str, text: &str| {
        div()
            .flex()
            .gap(px(8.))
            .text_size(px(12.))
            .child(
                div()
                    .w(px(36.))
                    .flex_none()
                    .text_color(c.text_muted)
                    .child(s(label)),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(c.text_muted)
                    .child(s(format!("[{kind}]"))),
            )
            .child(div().min_w_0().child(s(text)))
    };
    Some(
        div()
            .mt(px(10.))
            .p(px(10.))
            .rounded(px(5.))
            .bg(c.error_bg.unwrap_or(c.accent_faint))
            .border_1()
            .border_color(red)
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(icon("circle-alert", 13., red))
                    .child("This may not be the issue you attached things to")
                    .child(div().flex_1())
                    .child(
                        div()
                            .font_weight(FontWeight::NORMAL)
                            .text_size(px(11.))
                            .text_color(c.text_muted)
                            .child("v decides: same work, or repurposed"),
                    ),
            )
            .child(div().text_size(px(12.)).child(s(r.summary())))
            .child(line("was", &r.was.kind, &r.was.summary))
            .child(line("now", &r.now.kind, &r.now.summary))
            .when(!yours.is_empty(), |d| {
                d.child(
                    div()
                        .text_size(px(12.))
                        .text_color(c.text_muted)
                        .child(s(format!(
                            "Yours on {key}: {}. Nothing is sent to Jira until you decide.",
                            yours.join(", ")
                        ))),
                )
            })
            .into_any_element(),
    )
}

fn field_row(c: &Colors, label: &str, value: AnyElement, queued: bool) -> AnyElement {
    div()
        .flex()
        .items_center()
        .min_h(px(24.))
        .gap(px(10.))
        .child(
            div()
                .w(px(84.))
                .flex_none()
                .text_size(px(11.5))
                .text_color(c.text_muted)
                .child(s(label)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(value)
                .when(queued, |d| {
                    d.child(
                        div()
                            .px(px(5.))
                            .rounded(px(3.))
                            .text_size(px(10.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(c.warning.unwrap_or(c.text))
                            .bg(c.warning_bg.unwrap_or(c.accent_faint))
                            .child("QUEUED"),
                    )
                }),
        )
        .into_any_element()
}

fn plain(c: &Colors, t: impl Into<String>) -> AnyElement {
    let t = t.into();
    let empty = t.is_empty();
    div()
        .min_w_0()
        .truncate()
        .text_color(if empty { c.icon_disabled } else { c.text })
        .child(s(if empty { "—".to_string() } else { t }))
        .into_any_element()
}

fn section(c: &Colors, title: &str, extra: Option<String>) -> AnyElement {
    div()
        .pt(px(14.))
        .pb(px(4.))
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c.text_muted)
                .child(s(title.to_uppercase())),
        )
        .when_some(extra, |d, e| {
            d.child(
                div()
                    .text_size(px(11.))
                    .text_color(c.text_muted)
                    .child(s(e)),
            )
        })
        .child(div().flex_1().h(px(1.)).bg(c.border_variant))
        .into_any_element()
}

/// Press, drag and release over `content` select its text (pieces in `map`): a press on text
/// starts a selection (a word on a double click, the piece on a triple, extending with Shift),
/// a press elsewhere clears it, and a drag moves its end.
fn selecting(
    content: gpui::Div,
    model: &Entity<Model>,
    pane: &str,
    map: &crate::select::TextMap,
) -> gpui::Div {
    use crate::select::TextSel;
    use gpui::{MouseButton, MouseDownEvent, MouseMoveEvent};
    let (m1, p1, map1) = (model.clone(), pane.to_string(), map.clone());
    let (m2, p2, map2) = (model.clone(), pane.to_string(), map.clone());
    let (m3, p3) = (model.clone(), pane.to_string());
    content
        .on_mouse_down(MouseButton::Left, move |ev: &MouseDownEvent, _, cx| {
            let hit = map1.hit(ev.position, false);
            m1.update(cx, |m, cx| {
                let before = m.sel(&p1);
                match hit {
                    None => {
                        m.text_sel.borrow_mut().remove(&p1);
                    }
                    Some(p) => {
                        let mut sel = match (ev.modifiers.shift, before, ev.click_count) {
                            (true, Some(b), _) => TextSel::new(b.anchor, p),
                            (_, _, 2) => map1.word(p),
                            (_, _, n) if n >= 3 => TextSel::new((p.0, 0), (p.0, map1.len_of(p.0))),
                            _ => TextSel::new(p, p),
                        };
                        sel.dragging = ev.click_count <= 1;
                        m.text_sel.borrow_mut().insert(p1.clone(), sel);
                    }
                }
                m.generation += 1;
                cx.notify();
            });
        })
        .on_mouse_move(move |ev: &MouseMoveEvent, _, cx| {
            if !m2.read(cx).sel(&p2).is_some_and(|s| s.dragging) {
                return;
            }
            let held = ev.pressed_button == Some(MouseButton::Left);
            let hit = if held {
                map2.hit(ev.position, true)
            } else {
                None
            };
            m2.update(cx, |m, cx| {
                let mut sels = m.text_sel.borrow_mut();
                let Some(s) = sels.get_mut(&p2) else {
                    return;
                };
                if !held {
                    s.dragging = false;
                } else if let Some(p) = hit
                    && s.head != p
                {
                    s.head = p;
                    drop(sels);
                    m.generation += 1;
                    cx.notify();
                }
            });
        })
        .on_mouse_up(MouseButton::Left, move |_, _, cx| {
            m3.update(cx, |m, _| {
                if let Some(s) = m.text_sel.borrow_mut().get_mut(&p3) {
                    s.dragging = false;
                }
            });
        })
}

/// A pane's name (see [`crate::select`]).
pub fn pane_name(pane: PaneId) -> String {
    format!("{pane:?}")
}

/// The name of the selectable text of `pane` while it shows `key`.
pub fn text_area(pane: &str, key: &str) -> String {
    format!("{pane}|{key}")
}

fn issue_detail(
    model: &Entity<Model>,
    key: &str,
    pane: &str,
    scroll: ScrollHandle,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let m = model.read(cx);
    let c = m.colors.clone();
    let Some(i) = m.effective_issue(key) else {
        return empty(
            &c,
            &format!("{key} is not in the local copy"),
            "It may have left the JQL in config.toml, or been deleted. ⌘⇧R does a full refresh.",
        );
    };
    let raw = m.desk.issue(key);
    let differs = |f: &dyn Fn(&Issue) -> String| raw.is_some_and(|r| f(r) != f(i));
    let flags = m.flags_of(key);
    let open: Vec<_> = m.desk.queue.for_issue(key).cloned().collect();
    // Its text can be selected: the pieces register afresh as they're drawn. A selection
    // belongs to this pane showing this issue; another issue here starts without one.
    let area = text_area(pane, key);
    m.forget_text_except(pane, &area);
    let text_map = m.text_map(&area);
    text_map.clear();
    let pane = area.as_str();
    let media = i.media_attachments();
    let rich = crate::rich::Rich::new(&c, model, m, &media, &format!("rt-{key}")).selectable(pane);
    let plain = |c: &Colors, t: String| -> AnyElement {
        if t.is_empty() {
            plain(c, t)
        } else {
            div()
                .min_w_0()
                .truncate()
                .child(rich.text(t))
                .into_any_element()
        }
    };

    let top = div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(kind_mark(&c, &i.kind))
        .child(
            div()
                .font_family(c.mono_font.clone())
                .text_size(px(12.))
                .text_color(c.text_muted)
                .child(s(format!("{} · {}", i.key, i.kind))),
        )
        .child(status_chip(&c, &i.status, i.status_category))
        .when(flags.starred, |d| {
            d.child(icon("star", 13., c.warning.unwrap_or(c.text_accent)))
        })
        .child(div().flex_1())
        .child(
            div()
                .text_size(px(11.))
                .text_color(c.text_muted)
                .child(s(format!("updated {}", state::ago(&i.updated)))),
        );

    let title = div()
        .pt(px(8.))
        .text_size(px(18.))
        .font_weight(FontWeight::SEMIBOLD)
        .line_height(px(24.))
        .text_color(c.text)
        .child(rich.text(i.summary.clone()));

    let banner = (!open.is_empty()).then(|| {
        let mut b = div()
            .mt(px(10.))
            .p(px(8.))
            .rounded(px(5.))
            .bg(c.warning_bg.unwrap_or(c.accent_faint))
            .border_1()
            .border_color(c.warning.unwrap_or(c.border))
            .flex()
            .flex_col()
            .gap(px(3.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(icon("upload", 13., c.warning.unwrap_or(c.text)))
                    .child(s(format!(
                        "{} change{} not in Jira yet",
                        open.len(),
                        if open.len() == 1 { "" } else { "s" }
                    )))
                    .child(div().flex_1())
                    .child(
                        div()
                            .font_weight(FontWeight::NORMAL)
                            .text_size(px(11.))
                            .text_color(c.text_muted)
                            .child(s(format!(
                                "u undoes the last · {} applies all",
                                state::apply_key()
                            ))),
                    ),
            );
        for ch in &open {
            let (state_text, color) = match &ch.state {
                State::Pending => ("queued".to_string(), c.text_muted),
                State::Failed { error, .. } => {
                    (format!("failed: {error}"), c.error.unwrap_or(c.text))
                }
                State::Conflict { remote, .. } => (
                    format!("conflict: Jira now says “{remote}”"),
                    c.error.unwrap_or(c.text),
                ),
                State::Applied { .. } => ("applied".into(), c.text_muted),
            };
            b = b.child(
                div()
                    .flex()
                    .gap(px(8.))
                    .text_size(px(12.))
                    .child(div().child(s(ch.edit.describe())))
                    .child(div().text_color(color).child(s(state_text))),
            );
        }
        b
    });

    let sprint = i.current_sprint().map(|sp| {
        let state = match sp.state {
            jdesk_core::model::SprintState::Active => "active",
            jdesk_core::model::SprintState::Future => "next",
            jdesk_core::model::SprintState::Closed => "closed",
        };
        let ends = sp
            .end
            .as_deref()
            .map(|e| format!(" · ends {}", state::short_date(e)))
            .unwrap_or_default();
        format!("{} ({state}){ends}", sp.name)
    });
    let epic = i
        .parent
        .as_ref()
        .map(|p| format!("{} {}", p.key, p.summary));
    let labels = div()
        .flex()
        .flex_wrap()
        .gap(px(4.))
        .children(i.labels.iter().map(|l| label_chip(&c, l)))
        .when(i.labels.is_empty(), |d| d.child(plain(&c, String::new())))
        .into_any_element();

    let fields = div()
        .mt(px(12.))
        .flex()
        .flex_col()
        .child(field_row(
            &c,
            "Assignee",
            plain(
                &c,
                i.assignee
                    .as_ref()
                    .map_or("Unassigned".into(), |p| p.name.clone()),
            ),
            differs(&|x| {
                x.assignee
                    .as_ref()
                    .map(|p| p.id.clone())
                    .unwrap_or_default()
            }),
        ))
        .child(field_row(
            &c,
            "Sprint",
            plain(&c, sprint.unwrap_or_default()),
            differs(&|x| {
                x.current_sprint()
                    .map(|s| s.id.to_string())
                    .unwrap_or_default()
            }),
        ))
        .child(field_row(
            &c,
            "Epic",
            plain(&c, epic.unwrap_or_default()),
            false,
        ))
        .child(field_row(
            &c,
            "Priority",
            plain(&c, i.priority.clone().unwrap_or_default()),
            differs(&|x| x.priority.clone().unwrap_or_default()),
        ))
        .child(field_row(
            &c,
            "Points",
            plain(&c, points(i.story_points)),
            differs(&|x| points(x.story_points)),
        ))
        .child(field_row(
            &c,
            "Labels",
            labels,
            differs(&|x| x.labels.join(",")),
        ))
        .child(field_row(
            &c,
            "Your tags",
            div()
                .flex()
                .flex_wrap()
                .gap(px(4.))
                .children(flags.tags.iter().map(|t| tag_chip(&c, t)))
                .when(flags.tags.is_empty(), |d| {
                    d.child(
                        div()
                            .text_color(c.icon_disabled)
                            .child("— (⇧T adds; local only)"),
                    )
                })
                .into_any_element(),
            false,
        ))
        .when(!i.components.is_empty(), |d| {
            d.child(field_row(
                &c,
                "Components",
                plain(&c, i.components.join(", ")),
                false,
            ))
        })
        .when(!i.fix_versions.is_empty(), |d| {
            d.child(field_row(
                &c,
                "Fix version",
                plain(&c, i.fix_versions.join(", ")),
                false,
            ))
        })
        .child(field_row(
            &c,
            "Reporter",
            plain(
                &c,
                i.reporter
                    .as_ref()
                    .map(|p| p.name.clone())
                    .unwrap_or_default(),
            ),
            false,
        ))
        .when_some(i.due.clone(), |d, due| {
            d.child(field_row(
                &c,
                "Due",
                plain(&c, state::short_date(&due)),
                false,
            ))
        })
        .child(field_row(
            &c,
            "Created",
            plain(
                &c,
                format!(
                    "{} · updated {}",
                    state::short_date(&i.created),
                    state::ago(&i.updated)
                ),
            ),
            false,
        ));

    let mut col = div()
        .flex()
        .flex_col()
        .p(px(14.))
        .text_size(px(13.))
        .line_height(px(19.))
        .child(top)
        .child(title)
        .children(review_banner(&c, m, key))
        .children(banner)
        .child(fields)
        .child(section(&c, "Description", None))
        .child(if i.description.is_empty() {
            plain(&c, String::new())
        } else {
            rich.blocks(&i.description)
        });
    if !i.attachments.is_empty() {
        col = col
            .child(section(
                &c,
                "Attachments",
                Some(i.attachments.len().to_string()),
            ))
            .child(rich.attachments(&i.attachments));
    }

    if !i.subtasks.is_empty() || !i.links.is_empty() {
        col = col.child(section(&c, "Linked", None));
        for l in i.subtasks.iter().chain(i.links.iter()) {
            let target = l.key.clone();
            let model2 = model.clone();
            col = col.child(
                div()
                    .id(s(format!("link-{}-{}", l.relation, l.key)))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(22.))
                    .rounded(px(3.))
                    .hover(|d| d.bg(c.hover))
                    .child(
                        div()
                            .w(px(90.))
                            .flex_none()
                            .text_size(px(11.5))
                            .text_color(c.text_muted)
                            .child(s(l.relation.clone())),
                    )
                    .child(
                        div()
                            .font_family(c.mono_font.clone())
                            .text_size(px(11.5))
                            .text_color(c.text_accent)
                            .child(s(l.key.clone())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(s(l.summary.clone())),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(c.text_muted)
                            .child(s(l.status.clone())),
                    )
                    .on_click(move |_, _, cx| {
                        model2.update(cx, |m, cx| {
                            m.pending_open = Some(target.clone());
                            cx.notify();
                        });
                    }),
            );
        }
    }

    let more = i.comment_total.saturating_sub(i.comments.len());
    col = col.child(section(
        &c,
        "Comments",
        Some(if more > 0 {
            format!(
                "{} (latest {} kept locally)",
                i.comment_total,
                i.comments.len()
            )
        } else {
            i.comments.len().to_string()
        }),
    ));
    if i.comments.is_empty() {
        col = col.child(plain(&c, String::new()));
    }
    for cm in &i.comments {
        col = col.child(
            div()
                .pt(px(6.))
                .pb(px(4.))
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(
                    div()
                        .flex()
                        .gap(px(8.))
                        .child(
                            div().font_weight(FontWeight::SEMIBOLD).child(s(cm
                                .author
                                .as_ref()
                                .map_or("Someone".into(), |p| p.name.clone()))),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(c.text_muted)
                                .child(s(state::ago(&cm.created))),
                        ),
                )
                .child(div().pl(px(2.)).child(rich.blocks(&cm.body))),
        );
    }
    for ch in open
        .iter()
        .filter(|ch| matches!(ch.edit, jdesk_core::queue::Edit::Comment { .. }))
    {
        if let jdesk_core::queue::Edit::Comment { text } = &ch.edit {
            col = col.child(
                div()
                    .mt(px(6.))
                    .p(px(6.))
                    .rounded(px(4.))
                    .border_1()
                    .border_dashed()
                    .border_color(c.warning.unwrap_or(c.border))
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(c.warning.unwrap_or(c.text_muted))
                            .child("Your comment · queued"),
                    )
                    .child(s(text.clone())),
            );
        }
    }

    let note = m.desk.sidecar.note(key);
    col = col.child(section(&c, "Your note", Some("local only".into())));
    col = col.child(if note.trim().is_empty() {
        div()
            .text_color(c.text_muted)
            .child("None yet. n writes one (saved in notes/".to_string() + key + ".md).")
            .into_any_element()
    } else {
        div()
            .p(px(8.))
            .rounded(px(4.))
            .bg(c.accent_faint)
            .child(s(note))
            .into_any_element()
    });
    col = col.child(div().h(px(40.)));

    let col = selecting(col, model, pane, &text_map);
    m.scroll_areas
        .get(format!("detail-{key}"))
        .tracking(scroll.clone())
        .vertical(s(format!("detail-{key}")), m.stretch(), div().child(col))
        .size_full()
        .bg(c.editor)
        .into_any_element()
}

// ----- Notes ----------------------------------------------------------------------------------

fn notes(model: &Entity<Model>, scroll: ScrollHandle, cx: &mut App) -> AnyElement {
    let m = model.read(cx);
    let c = m.colors.clone();
    // The issue it follows (`Model::follow_for_notes`): clicking in Notes, Filters or Settings
    // doesn't move it to another.
    let (key, editing) = match &m.note_edit {
        Some((k, t)) => (Some(k.clone()), Some(t.clone())),
        None => (m.notes_for.clone().or_else(|| m.current_key()), None),
    };
    let Some(key) = key else {
        return empty(&c, "No issue selected", "Notes follow the issue you're on");
    };
    let summary = m
        .effective_issue(&key)
        .map(|i| i.summary.clone())
        .unwrap_or_default();
    let head = div()
        .flex_none()
        .px(px(10.))
        .py(px(6.))
        .flex()
        .flex_col()
        .gap(px(1.))
        .border_b_1()
        .border_color(c.border_variant)
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(icon("sticky-note", 13., c.icon_muted))
                .child(
                    div()
                        .font_family(c.mono_font.clone())
                        .text_size(px(11.5))
                        .child(s(key.clone())),
                )
                .child(div().flex_1())
                .child(div().text_size(px(11.)).text_color(c.text_muted).child(
                    if editing.is_some() {
                        "⌘S or Esc saves"
                    } else {
                        "Enter or n edits"
                    },
                )),
        )
        .child(
            div()
                .truncate()
                .text_size(px(11.5))
                .text_color(c.text_muted)
                .child(s(summary)),
        );
    let body = match editing {
        Some(t) => div()
            .p(px(10.))
            .bg(c.editor)
            .border_1()
            .border_color(c.text_accent)
            .m(px(6.))
            .rounded(px(4.))
            .min_h(px(120.))
            .child(t.render(&c, "Write anything: plans, questions, who to ask…", true))
            .into_any_element(),
        None => {
            let note = m.desk.sidecar.note(&key);
            if note.trim().is_empty() {
                div()
                    .p(px(12.))
                    .text_color(c.text_muted)
                    .text_size(px(12.))
                    .child(s(format!(
                        "No note on {key}. Press n to write one. It stays on this computer, in notes/{key}.md."
                    )))
                    .into_any_element()
            } else {
                div()
                    .p(px(10.))
                    .font_family(c.mono_font.clone())
                    .text_size(px(12.5))
                    .line_height(px(19.))
                    .child(s(note))
                    .into_any_element()
            }
        }
    };
    div()
        .size_full()
        .flex()
        .flex_col()
        .child(head)
        .child(
            m.scroll_areas
                .get("notes-body")
                .tracking(scroll.clone())
                .vertical(
                    "notes-body",
                    m.stretch(),
                    div().child(body).child(requests_section(model, &key, cx)),
                )
                .flex_1()
                .min_h_0(),
        )
        .into_any_element()
}

/// The issue's requests, under its note: what you asked for outside Jira Desk, where each
/// stands, and buttons to hand one to an agent, confirm it, drop it or open it again.
fn requests_section(model: &Entity<Model>, key: &str, cx: &App) -> AnyElement {
    let m = model.read(cx);
    let c = m.colors.clone();
    let mut list: Vec<&jdesk_core::requests::Request> = m.desk.requests.for_issue(key).collect();
    // Open ones first, newest first within each.
    list.sort_by_key(|r| (!r.state.is_open(), std::cmp::Reverse(r.id)));
    let mut col = div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .px(px(10.))
        .pt(px(6.))
        .pb(px(12.))
        .border_t_1()
        .border_color(c.border_variant)
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(icon("flag", 12., c.icon_muted))
                .child(
                    div()
                        .text_size(px(11.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(c.text_muted)
                        .child("REQUESTS"),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(c.text_muted)
                        .child("⇧R adds one"),
                ),
        );
    if list.is_empty() {
        col = col.child(div().text_size(px(12.)).text_color(c.text_muted).child(
            "Nothing asked. A request is something to make true outside Jira Desk: a description rewrite, a link, a parent, a fix version.",
        ));
    }
    for r in list.into_iter().take(12) {
        let open = r.state.is_open();
        let id = r.id;
        let status = request_status(r);
        let mut row = div()
            .flex()
            .flex_col()
            .gap(px(3.))
            .p(px(6.))
            .rounded(px(5.))
            .bg(c.editor)
            .border_1()
            .border_color(if r.changed && open {
                c.text_accent
            } else {
                c.border_variant
            })
            .when(!open, |d| d.opacity(0.65))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .font_family(c.mono_font.clone())
                            .text_size(px(11.))
                            .text_color(c.text_muted)
                            .child(s(format!("#{id}"))),
                    )
                    .child({
                        // Cut to one line here; a click opens the whole request.
                        let model = model.clone();
                        div()
                            .id(s(format!("request-{id}-read")))
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .cursor_pointer()
                            .hover(|d| d.text_color(c.text_accent))
                            .child(s(r.want.describe()))
                            .on_click(move |_, _, cx| {
                                model.update(cx, |m, cx| {
                                    m.pending_request = Some(id);
                                    cx.notify();
                                });
                            })
                    })
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(11.))
                            .text_color(if r.changed && open {
                                c.text_accent
                            } else {
                                c.text_muted
                            })
                            .child(s(status)),
                    )
                    // Handed off by mistake (or the agent didn't take it): back to open.
                    .when(
                        matches!(r.state, jdesk_core::requests::State::HandedOff { .. }),
                        |d| {
                            let model = model.clone();
                            d.child(
                                div()
                                    .id(s(format!("request-{id}-undo")))
                                    .flex_none()
                                    .px(px(4.))
                                    .rounded(px(3.))
                                    .text_size(px(12.))
                                    .text_color(c.text_muted)
                                    .cursor_pointer()
                                    .hover(|d| d.bg(c.hover).text_color(c.text))
                                    .child("↺")
                                    .on_click(move |_, _, cx| request_act(&model, id, "open", cx)),
                            )
                        },
                    ),
            );
        if let jdesk_core::requests::Want::Description { instructions: t }
        | jdesk_core::requests::Want::Other { text: t } = &r.want
            && t.lines().count() > 1
        {
            row = row.child(
                div()
                    .text_size(px(11.5))
                    .text_color(c.text_muted)
                    .child(s(t.clone())),
            );
        }
        if !r.note.is_empty() {
            row = row.child(
                div()
                    .text_size(px(11.5))
                    .text_color(c.text_muted)
                    .child(s(format!("“{}”", r.note))),
            );
        }
        let buttons = request_buttons(model, r, &c, "request");
        col = col.child(row.child(buttons));
    }
    col.into_any_element()
}

/// A request's buttons (in its row, and in its dialog: `prefix` keeps their ids apart).
pub fn request_buttons(
    model: &Entity<Model>,
    r: &jdesk_core::requests::Request,
    c: &Colors,
    prefix: &str,
) -> gpui::Div {
    let open = r.state.is_open();
    let id = r.id;
    let button = |act: &'static str, label: &'static str, strong: bool| {
        let model = model.clone();
        div()
            .id(s(format!("{prefix}-{id}-{act}")))
            .px(px(6.))
            .h(px(20.))
            .flex()
            .items_center()
            .rounded(px(4.))
            .border_1()
            .border_color(if strong { c.text_accent } else { c.border })
            .text_size(px(11.))
            .text_color(if strong { c.text } else { c.text_muted })
            .cursor_pointer()
            .hover(|d| d.bg(c.hover))
            .child(label)
            .on_click(move |_, _, cx| request_act(&model, id, act, cx))
    };
    let mut buttons = div().flex().gap(px(6.)).pt(px(2.));
    // A link or parent Jira Desk can make itself: one click queues it (⇧⌘A applies).
    let queueable = matches!(
        r.want,
        jdesk_core::requests::Want::Link { .. } | jdesk_core::requests::Want::Parent { .. }
    );
    if open && queueable {
        buttons = buttons.child(button("queue", "Queue it", true));
    }
    if open {
        buttons = buttons
            .child(button("copy", "Copy for agent", !r.changed && !queueable))
            .child(button(
                "done",
                if r.changed { "Yes, done" } else { "Done" },
                r.changed,
            ))
            .child(button("drop", "Drop", false));
    } else {
        buttons = buttons.child(button("open", "Reopen", false));
    }
    buttons
}

/// What a request stands at, in words.
pub fn request_status(r: &jdesk_core::requests::Request) -> String {
    if r.changed && r.state.is_open() {
        return "changed in Jira: done?".to_string();
    }
    match &r.state {
        jdesk_core::requests::State::HandedOff { to, .. } => format!("handed to {to}"),
        jdesk_core::requests::State::Confirmed { by, .. } => format!("done ({by})"),
        st => st.word().to_string(),
    }
}

/// A request button (or the palette's Copy for agent): `copy` puts the agent text on the
/// clipboard and hands an open request to the agent; `done`, `drop` and `open` move it on.
pub fn request_act(model: &Entity<Model>, id: u64, act: &str, cx: &mut App) {
    use jdesk_core::control::{Request, data};
    if act == "copy" {
        let text = model.update(cx, |m, _| {
            let text = data(&mut m.desk, &Request::RequestPrompt { id })
                .map(|r| r.message)
                .map_err(|e| format!("{e:#}"));
            if text.is_ok()
                && m.desk
                    .requests
                    .get(id)
                    .is_some_and(|r| r.state == jdesk_core::requests::State::Open)
            {
                let _ = data(
                    &mut m.desk,
                    &Request::RequestSet {
                        id,
                        state: "handed".into(),
                        note: None,
                        by: Some("agent".into()),
                    },
                );
            }
            text
        });
        match text {
            Ok(t) => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(t));
                model.update(cx, |m, cx| {
                    m.rebuild();
                    m.note(format!(
                        "Copied request #{id} for an agent: paste it into one (it reports back with jdesk)"
                    ));
                    cx.notify();
                });
            }
            Err(e) => model.update(cx, |m, cx| {
                m.error(e);
                cx.notify();
            }),
        }
        return;
    }
    if act == "queue" {
        model.update(cx, |m, cx| {
            let Some(r) = m.desk.requests.get(id).cloned() else {
                return;
            };
            let (field, value) = match &r.want {
                jdesk_core::requests::Want::Link { relation, other } => {
                    ("link", vec![relation.clone(), other.clone()])
                }
                jdesk_core::requests::Want::Parent { to } => ("parent", vec![to.clone()]),
                _ => return,
            };
            match jdesk_core::control::parse_change(&m.desk, &r.key, field, &value) {
                Ok(edit) => {
                    m.queue(&r.key, edit);
                    let _ = data(
                        &mut m.desk,
                        &Request::RequestSet {
                            id,
                            state: "handed".into(),
                            note: Some(format!(
                                "queued in Jira Desk: applies with {}",
                                state::apply_key()
                            )),
                            by: Some("Jira Desk".into()),
                        },
                    );
                    m.rebuild();
                }
                Err(e) => m.error(format!("{e:#}")),
            }
            cx.notify();
        });
        return;
    }
    model.update(cx, |m, cx| {
        let r = data(
            &mut m.desk,
            &Request::RequestSet {
                id,
                state: act.into(),
                note: None,
                by: None,
            },
        );
        match r {
            Ok(resp) => {
                m.rebuild();
                m.note(resp.message);
            }
            Err(e) => m.error(format!("{e:#}")),
        }
        cx.notify();
    });
}

// ----- Changes --------------------------------------------------------------------------------

fn changes(model: &Entity<Model>, cx: &mut App) -> AnyElement {
    let m = model.read(cx);
    let c = m.colors.clone();
    let rows = m.change_rows();
    if rows.is_empty() {
        return empty(
            &c,
            "No changes queued",
            &format!(
                "Changes you make (t status, a assign, c comment…) wait here until you apply them with {}.",
                state::apply_key()
            ),
        );
    }
    let focused = m.focus == PaneFocus::Changes;
    let cursor = m.changes_cursor.min(rows.len() - 1);
    let applying = m.sync.applying;
    let held_n = m
        .desk
        .queue
        .changes()
        .iter()
        .filter(|ch| ch.state == State::Pending && m.held(&ch.key))
        .count();
    let pending = m.desk.queue.pending_count() - held_n;
    let head = div()
        .flex_none()
        .px(px(10.))
        .h(px(28.))
        .flex()
        .items_center()
        .gap(px(10.))
        .border_b_1()
        .border_color(c.border_variant)
        .child(icon("upload", 13., c.icon_muted))
        .child(s(if applying {
            "Applying…".to_string()
        } else {
            format!("{pending} to apply")
        }))
        .when(held_n > 0, |d| {
            d.child(
                div()
                    .text_color(c.error.unwrap_or(c.text))
                    .child(s(format!("{held_n} held for review"))),
            )
        })
        .child(div().flex_1())
        .child(
            div()
                .text_size(px(11.))
                .text_color(c.text_muted)
                .child(s(format!(
                    "{} apply all · Enter apply one · d discard · r retry · ⇧R overwrite",
                    state::apply_key()
                ))),
        );
    let mut list = div().flex().flex_col();
    let mut history_started = false;
    let mut reviews_started = false;
    let section = |label: &'static str| {
        div()
            .px(px(10.))
            .pt(px(10.))
            .pb(px(3.))
            .text_size(px(10.5))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(c.text_muted)
            .child(label)
    };
    for (n, row) in rows.iter().enumerate() {
        let id = match row {
            state::ChangeRow::Review(key) => {
                if !reviews_started {
                    reviews_started = true;
                    list = list.child(section("NEEDS REVIEW  (v decides)"));
                }
                let Some(r) = m.desk.identities.open_review(key) else {
                    continue;
                };
                let red = c.error.unwrap_or(c.text);
                let selected = n == cursor;
                let model2 = model.clone();
                list = list.child(
                    div()
                        .id(("review", n))
                        .min_h(px(ROW))
                        .px(px(10.))
                        .py(px(3.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .when(selected && focused, |d| d.bg(c.ghost_selected))
                        .when(selected && !focused, |d| d.bg(c.ghost_active))
                        .hover(|d| d.bg(c.hover))
                        .child(icon("circle-alert", 13., red))
                        .child(
                            div()
                                .w(px(74.))
                                .flex_none()
                                .font_family(c.mono_font.clone())
                                .text_size(px(11.5))
                                .text_color(c.text_muted)
                                .child(s(key.clone())),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(red)
                                .child(s(r.summary())),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(11.5))
                                .text_color(c.text_muted)
                                .child(s(format!(
                                    "{} change(s) held",
                                    m.desk.queue.for_issue(key).count()
                                ))),
                        )
                        .on_click(move |_, _, cx| {
                            model2.update(cx, |m, cx| {
                                m.changes_cursor = n;
                                m.generation += 1;
                                cx.notify();
                            });
                        }),
                );
                continue;
            }
            state::ChangeRow::Change(id) => id,
        };
        let Some(ch) = m.desk.queue.changes().iter().find(|x| x.id == *id) else {
            continue;
        };
        if !ch.is_open() && !history_started {
            history_started = true;
            list = list.child(
                div()
                    .px(px(10.))
                    .pt(px(10.))
                    .pb(px(3.))
                    .text_size(px(10.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(c.text_muted)
                    .child("APPLIED  (⇧C clears)"),
            );
        }
        let held = ch.state == State::Pending && m.held(&ch.key);
        let (glyph, color, state_text) = match &ch.state {
            State::Pending if held => (
                "circle-alert",
                c.error.unwrap_or(c.text),
                "held: the issue changed a lot in Jira (v to review)".to_string(),
            ),
            State::Pending => (
                "circle-dashed",
                c.warning.unwrap_or(c.text_muted),
                "queued".to_string(),
            ),
            State::Failed { error, .. } => {
                ("circle-alert", c.error.unwrap_or(c.text), error.clone())
            }
            State::Conflict { remote, .. } => (
                "circle-alert",
                c.error.unwrap_or(c.text),
                format!(
                    "conflict: was “{}” when you changed it, Jira now says “{remote}”",
                    ch.base_shown
                ),
            ),
            State::Applied { at } => (
                "circle-check",
                c.success.unwrap_or(c.text_muted),
                format!("applied {}", state::since(*at)),
            ),
        };
        let selected = n == cursor;
        let model2 = model.clone();
        let summary = m
            .effective_issue(&ch.key)
            .map(|i| i.summary.clone())
            .unwrap_or_default();
        list = list.child(
            div()
                .id(("change", n))
                .min_h(px(ROW))
                .px(px(10.))
                .py(px(3.))
                .flex()
                .items_center()
                .gap(px(8.))
                .when(selected && focused, |d| d.bg(c.ghost_selected))
                .when(selected && !focused, |d| d.bg(c.ghost_active))
                .hover(|d| d.bg(c.hover))
                .when(!ch.is_open(), |d| d.opacity(0.6))
                .child(icon(glyph, 13., color))
                .child(
                    div()
                        .w(px(74.))
                        .flex_none()
                        .font_family(c.mono_font.clone())
                        .text_size(px(11.5))
                        .text_color(c.text_muted)
                        .child(s(ch.key.clone())),
                )
                .child(
                    div()
                        .flex_none()
                        .max_w(px(320.))
                        .truncate()
                        .child(s(ch.edit.describe())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(11.5))
                        .text_color(c.text_muted)
                        .child(s(summary)),
                )
                .child(
                    div()
                        .flex_none()
                        .max_w(px(420.))
                        .truncate()
                        .text_size(px(11.5))
                        .text_color(color)
                        .child(s(state_text)),
                )
                .on_click(move |_, _, cx| {
                    model2.update(cx, |m, cx| {
                        m.changes_cursor = n;
                        m.generation += 1;
                        cx.notify();
                    });
                }),
        );
    }
    div()
        .size_full()
        .flex()
        .flex_col()
        .text_size(px(12.5))
        .child(head)
        .child(
            m.scroll_areas
                .get("changes-list")
                .vertical("changes-list", m.stretch(), div().child(list))
                .flex_1()
                .min_h_0(),
        )
        .into_any_element()
}

// ----- Board ----------------------------------------------------------------------------------

// Sideways scrolling (the columns, the people bar): scroll.rs stretches up and down only,
// as Chorong's does, so these scroll as GPUI does.
#[allow(clippy::disallowed_methods)]
fn board(model: &Entity<Model>, cx: &mut App) -> AnyElement {
    let m = model.read(cx);
    let c = m.colors.clone();
    let cols = m.board_columns();
    if cols.is_empty() {
        return empty(&c, "No issues match", "The board shows the List as columns");
    }
    let focused = m.focus == PaneFocus::Board;
    let (cc, cr) = m.board_cursor;
    let cc = cc.min(cols.len() - 1);
    let n = cols.len() as f32;
    let mut row = div()
        .flex()
        .flex_none()
        .gap(px(state::BOARD_GAP))
        .p(px(state::BOARD_GAP))
        .h_full()
        // Its own width, so the board scrolls sideways when the columns don't fit.
        .w(px(n * state::BOARD_COL_W + (n + 1.) * state::BOARD_GAP));
    for (ci, (name, cat, cards)) in cols.iter().enumerate() {
        let (fg, _) = status_colors(&c, *cat);
        let mut col = div().flex().flex_col().gap(px(6.)).p(px(6.)).child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .pb(px(2.))
                .child(div().size(px(7.)).rounded(px(4.)).bg(fg))
                .child(
                    div()
                        .text_size(px(11.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(c.text_muted)
                        .child(s(name.to_uppercase())),
                )
                .child(div().text_size(px(11.)).text_color(c.text_muted).child(s({
                    let pts: f64 = cards
                        .iter()
                        .filter_map(|i| m.effective[*i].story_points)
                        .sum();
                    match state::points_total(pts) {
                        p if p.is_empty() => cards.len().to_string(),
                        p => format!("{} · {p}", cards.len()),
                    }
                }))),
        );
        let cr = cr.min(cards.len().saturating_sub(1));
        for (ri, idx) in cards.iter().enumerate() {
            let i = &m.effective[*idx];
            let f = &m.flags[*idx];
            let sel = focused && ci == cc && ri == cr;
            let model2 = model.clone();
            col = col.child(
                div()
                    .id(s(format!("card-{}", i.key)))
                    .p(px(8.))
                    .rounded(px(5.))
                    .bg(c.editor)
                    .border_1()
                    .border_color(if sel { c.text_accent } else { c.border_variant })
                    .flex()
                    .flex_col()
                    .gap(px(5.))
                    .child(div().line_clamp(2).child(s(i.summary.clone())))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(kind_mark(&c, &i.kind))
                            .child(
                                div()
                                    .font_family(c.mono_font.clone())
                                    .text_size(px(11.))
                                    .text_color(c.text_muted)
                                    .child(s(i.key.clone())),
                            )
                            .when(f.pending, |d| {
                                d.child(
                                    div()
                                        .size(px(7.))
                                        .rounded(px(4.))
                                        .bg(c.warning.unwrap_or(c.text_accent)),
                                )
                            })
                            .when(f.starred, |d| {
                                d.child(icon("star", 11., c.warning.unwrap_or(c.text_accent)))
                            })
                            .when(f.requests, |d| d.child(icon("flag", 11., c.text_accent)))
                            .child(div().flex_1())
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(c.text_muted)
                                    .child(s(points(i.story_points))),
                            )
                            .child(
                                div().text_size(px(11.)).text_color(c.text_muted).child(s(i
                                    .assignee
                                    .as_ref()
                                    .map_or("—".into(), |p| short_name(&p.name)))),
                            ),
                    )
                    // A click selects the card and shows it in the Preview pane beside the board.
                    .on_click(move |_, _, cx| {
                        model2.update(cx, |m, cx| {
                            m.board_cursor = (ci, ri);
                            m.board_moved();
                            m.pending_preview = true;
                            cx.notify();
                        });
                    }),
            );
        }
        row = row.child(
            m.scroll_areas
                .get(format!("board-col-{ci}"))
                .vertical(("board-col", ci), m.stretch(), col)
                .w(px(state::BOARD_COL_W))
                .flex_none()
                .h_full()
                .rounded(px(6.))
                .bg(c.panel),
        );
    }
    div()
        .size_full()
        .flex()
        .flex_col()
        .text_size(px(12.5))
        .child(board_people(model, cx))
        .child(
            div()
                .id("board")
                .flex_1()
                .min_h_0()
                .overflow_x_scroll()
                .track_scroll(&m.board_scroll)
                .child(row),
        )
        .into_any_element()
}

/// The people on the board, with how many cards each has: a click shows only theirs (again
/// to show everyone's), the same assignee filter as `f a` and the Filters pane.
// Sideways scrolling (the columns, the people bar): scroll.rs stretches up and down only,
// as Chorong's does, so these scroll as GPUI does.
#[allow(clippy::disallowed_methods)]
fn board_people(model: &Entity<Model>, cx: &App) -> AnyElement {
    let m = model.read(cx);
    let c = m.colors.clone();
    let mut people: Vec<(String, String, usize)> = m
        .facets
        .assignees
        .iter()
        .map(|(id, (name, n))| (id.clone(), name.clone(), *n))
        .filter(|p| p.2 > 0)
        .collect();
    people.sort_by(|a, b| b.2.cmp(&a.2).then(a.1.cmp(&b.1)));
    let chosen = &m.filter.assignees;
    let chip = |id: &'static str| div().id(id);
    let everyone_on = chosen.is_empty();
    let model_all = model.clone();
    let mut bar = div()
        .id("board-people")
        .flex_none()
        .flex()
        .items_center()
        .gap(px(6.))
        .px(px(state::BOARD_GAP + 2.))
        .pt(px(8.))
        .overflow_x_scroll()
        .child(
            chip("board-everyone")
                .flex_none()
                .px(px(8.))
                .h(px(22.))
                .flex()
                .items_center()
                .rounded(px(11.))
                .border_1()
                .border_color(if everyone_on { c.text_accent } else { c.border })
                .when(everyone_on, |d| d.bg(c.accent_faint))
                .text_color(if everyone_on { c.text } else { c.text_muted })
                .cursor_pointer()
                .hover(|d| d.bg(c.hover))
                .child("Everyone")
                .on_click(move |_, _, cx| {
                    model_all.update(cx, |m, cx| {
                        m.filter.assignees.clear();
                        m.refilter();
                        cx.notify();
                    });
                }),
        );
    for (id, name, count) in people {
        let on = chosen.contains(&id);
        let model2 = model.clone();
        let label = if id.is_empty() {
            "Unassigned".to_string()
        } else {
            short_name(&name)
        };
        let initials: String = name
            .split_whitespace()
            .filter_map(|w| w.chars().next())
            .take(2)
            .collect::<String>()
            .to_uppercase();
        bar = bar.child(
            div()
                .id(s(format!("board-person-{id}")))
                .flex_none()
                .pl(px(3.))
                .pr(px(8.))
                .h(px(22.))
                .flex()
                .items_center()
                .gap(px(5.))
                .rounded(px(11.))
                .border_1()
                .border_color(if on { c.text_accent } else { c.border })
                .when(on, |d| d.bg(c.accent_faint))
                .text_color(if on { c.text } else { c.text_muted })
                .cursor_pointer()
                .hover(|d| d.bg(c.hover))
                .child(
                    div()
                        .size(px(16.))
                        .flex_none()
                        .rounded(px(8.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(if on { c.text_accent } else { c.border })
                        .text_color(c.panel)
                        .text_size(px(8.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(s(if id.is_empty() {
                            "–".to_string()
                        } else {
                            initials
                        })),
                )
                .child(s(label))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(c.text_muted)
                        .child(s(count.to_string())),
                )
                .on_click(move |_, _, cx| {
                    model2.update(cx, |m, cx| {
                        let set = &mut m.filter.assignees;
                        if !set.remove(&id) {
                            set.insert(id.clone());
                        }
                        m.refilter();
                        cx.notify();
                    });
                }),
        );
    }
    bar.into_any_element()
}

// ----- Settings -------------------------------------------------------------------------------

/// The meter's colours, one per kind of data, in a fixed order (validated categorical slots
/// 1–6, stepped for the dark or the light surface).
fn meter_colors(c: &Colors) -> [Hsla; 6] {
    let dark = c.panel.l < 0.5;
    let hex = if dark {
        [0x3987e5, 0xd95926, 0x199e70, 0xc98500, 0xd55181, 0x0ca30c]
    } else {
        [0x2a78d6, 0xeb6834, 0x1baf7a, 0xeda100, 0xe87ba4, 0x008300]
    };
    hex.map(|h| gpui::rgb(h).into())
}

fn settings(model: &Entity<Model>, scroll: ScrollHandle, cx: &mut App) -> AnyElement {
    use jdesk_core::storage::{Kind, human};
    let m = model.read(cx);
    let c = m.colors.clone();
    let colors = meter_colors(&c);
    let heading = |t: &str| {
        div()
            .pt(px(14.))
            .pb(px(6.))
            .text_size(px(11.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(c.text_muted)
            .child(s(t.to_uppercase()))
    };
    let mut col = div()
        .flex()
        .flex_col()
        .px(px(12.))
        .pb(px(24.))
        .text_size(px(12.5));

    // Behaviour: choices saved in prefs.json.
    let on = m.auto_advance;
    col = col.child(heading("Behaviour")).child(
        div()
            .id("setting-auto-advance")
            .flex()
            .items_center()
            .gap(px(10.))
            .py(px(4.))
            .rounded(px(4.))
            .cursor_pointer()
            .hover(|d| d.bg(c.hover))
            // The switch.
            .child(
                div()
                    .w(px(26.))
                    .h(px(15.))
                    .flex_none()
                    .rounded(px(8.))
                    .p(px(2.))
                    .flex()
                    .when(on, |d| d.justify_end())
                    .bg(if on { c.text_accent } else { c.border })
                    .child(div().size(px(11.)).rounded(px(6.)).bg(c.panel)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(div().text_color(c.text).child("Auto-advance"))
                    .child(div().text_size(px(11.)).text_color(c.text_muted).child(
                        "After a change, star or tag made from the List, select the next issue (a toggles)",
                    )),
            )
            .on_click(move |_, window, cx| {
                window.dispatch_action(Box::new(crate::actions::ToggleAutoAdvance), cx);
            }),
    );

    // Storage.
    col = col.child(
        div()
            .flex()
            .items_end()
            .gap(px(8.))
            .child(heading("Storage"))
            .child(div().flex_1())
            .child(
                div()
                    .pb(px(6.))
                    .text_size(px(11.))
                    .text_color(c.text_muted)
                    .child(s(match (&m.usage, m.measuring) {
                        (_, true) => "Measuring…".to_string(),
                        (Some(u), false) => {
                            format!("measured {} · r again", state::since(u.measured_at))
                        }
                        (None, false) => "r measures".to_string(),
                    })),
            ),
    );
    match &m.usage {
        None => {
            col = col.child(
                div()
                    .text_color(c.text_muted)
                    .child("Measuring the data folder…"),
            );
        }
        Some(u) => {
            let dir = u.dir.display().to_string();
            col = col
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap(px(8.))
                        .child(
                            div()
                                .text_size(px(22.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(c.text)
                                .child(s(human(u.total))),
                        )
                        .child(div().text_color(c.text_muted).child("in the data folder")),
                )
                .child(
                    div()
                        .pt(px(2.))
                        .text_size(px(11.))
                        .text_color(c.text_muted)
                        .font_family(c.mono_font.clone())
                        .child(s(format!("{dir}  (y copies)"))),
                );
            // The meter: one segment per kind, in a fixed order, 2 px apart.
            let mut bar = div()
                .mt(px(10.))
                .h(px(12.))
                .w_full()
                .flex()
                .gap(px(2.))
                .rounded(px(4.))
                .overflow_hidden()
                .bg(c.element_bg);
            for (n, p) in u.parts.iter().enumerate() {
                if p.bytes == 0 {
                    continue;
                }
                let mut seg = div().h_full().min_w(px(3.)).bg(colors[n]);
                let st = seg.style();
                st.flex_grow = Some(p.bytes as f32);
                st.flex_shrink = Some(1.);
                st.flex_basis = Some(px(0.).into());
                bar = bar.child(seg);
            }
            col = col.child(bar);
            // The legend: every kind, named, with its size and share (the colours are never
            // the only way to tell them apart).
            let issues = m.desk.issues().len();
            for (n, p) in u.parts.iter().enumerate() {
                let share = if u.total > 0 {
                    p.bytes as f64 / u.total as f64 * 100.0
                } else {
                    0.0
                };
                let detail = match p.kind {
                    Kind::Cache => format!("{issues} issues"),
                    Kind::Images => format!(
                        "{} file{} · {}",
                        p.files,
                        if p.files == 1 { "" } else { "s" },
                        match m.desk.image_cap() {
                            Some(cap) => format!(
                                "kept under {} (max_image_mb), least recently opened go first",
                                human(cap)
                            ),
                            None => "no limit (max_image_mb = 0)".to_string(),
                        }
                    ),
                    Kind::Journal => format!(
                        "{} month file{}",
                        p.files,
                        if p.files == 1 { "" } else { "s" }
                    ),
                    Kind::Notes => {
                        format!("{} file{}", p.files, if p.files == 1 { "" } else { "s" })
                    }
                    Kind::State | Kind::Other => {
                        format!("{} file{}", p.files, if p.files == 1 { "" } else { "s" })
                    }
                };
                let model2 = model.clone();
                col = col.child(
                    div()
                        .pt(px(8.))
                        .flex()
                        .gap(px(8.))
                        .child(
                            div()
                                .mt(px(4.))
                                .size(px(10.))
                                .flex_none()
                                .rounded(px(2.))
                                .bg(colors[n]),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .flex()
                                        .gap(px(8.))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .truncate()
                                                .text_color(c.text)
                                                .child(s(p.kind.label())),
                                        )
                                        .child(
                                            div()
                                                .flex_none()
                                                .font_family(c.mono_font.clone())
                                                .text_size(px(11.5))
                                                .text_color(c.text)
                                                .child(s(human(p.bytes))),
                                        )
                                        .child(
                                            div()
                                                .w(px(34.))
                                                .flex_none()
                                                .text_right()
                                                .font_family(c.mono_font.clone())
                                                .text_size(px(11.5))
                                                .text_color(c.text_muted)
                                                .child(s(if p.bytes == 0 {
                                                    "—".to_string()
                                                } else if share < 1.0 {
                                                    "<1%".to_string()
                                                } else {
                                                    format!("{share:.0}%")
                                                })),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(c.text_muted)
                                        .child(s(format!("{detail} · {}", p.kind.hint()))),
                                )
                                .when(p.kind == Kind::Images && p.bytes > 0, |d| {
                                    d.child(
                                        div()
                                            .id("clear-images")
                                            .mt(px(4.))
                                            .px(px(8.))
                                            .py(px(2.))
                                            .rounded(px(4.))
                                            .border_1()
                                            .border_color(c.border)
                                            .text_size(px(11.5))
                                            .text_color(c.text)
                                            .hover(|d| d.bg(c.hover))
                                            .cursor_pointer()
                                            .child("Clear downloaded images (⇧D)")
                                            .on_click(move |_, _, cx| {
                                                model2.update(cx, |m, cx| m.clear_images(cx));
                                            }),
                                    )
                                }),
                        ),
                );
            }
        }
    }

    // Connection: read-only here; config.toml is the place to change it.
    let (site, api, jql, refresh) = match &m.desk.source {
        jdesk_core::Source::Jira(cfg) => (
            cfg.site().to_string(),
            format!("{:?}", cfg.api),
            cfg.jql.clone(),
            m.desk.refresh_minutes(),
        ),
        jdesk_core::Source::Demo(_) => (
            m.desk.site(),
            "Demo (made-up issues)".to_string(),
            "project = DEMO".to_string(),
            m.desk.refresh_minutes(),
        ),
    };
    let row = |label: &str, value: String| {
        div()
            .flex()
            .gap(px(10.))
            .min_h(px(22.))
            .child(
                div()
                    .w(px(84.))
                    .flex_none()
                    .text_size(px(11.5))
                    .text_color(c.text_muted)
                    .child(s(label)),
            )
            .child(div().flex_1().min_w_0().text_color(c.text).child(s(value)))
    };
    col = col
        .child(heading("Connection"))
        .child(row("Site", site.trim_start_matches("https://").to_string()))
        .child(row("API", api))
        .child(row("Issues kept", jql))
        .child(row(
            "Refresh",
            if refresh == 0 {
                "only when asked (⌘R)".into()
            } else {
                format!("every {refresh} min")
            },
        ))
        .child(row(
            "Last sync",
            m.desk
                .last_sync
                .map(state::since)
                .unwrap_or_else(|| "not yet".into()),
        ))
        .child(row(
            "Signed in as",
            m.desk
                .myself
                .as_ref()
                .map(|p| p.name.clone())
                .unwrap_or_else(|| "—".into()),
        ))
        .child(
            div()
                .pt(px(8.))
                .text_size(px(11.))
                .text_color(c.text_muted)
                .child(
                    "These come from config.toml in the data folder; edit it there and restart.",
                ),
        );
    m.scroll_areas
        .get("settings")
        .tracking(scroll.clone())
        .vertical("settings", m.stretch(), div().child(col))
        .size_full()
        .into_any_element()
}

// ----- the search bar -------------------------------------------------------------------------

/// The open search bar: the filters as chips, the chip being filled in, the text, and what
/// Tab or Enter would do under it.
fn search_bar(
    c: &Colors,
    o: &crate::omni::Omni,
    chips: &[(crate::omni::Field, String, String)],
    suggestions: Vec<crate::omni::Suggestion>,
) -> AnyElement {
    use crate::omni::Suggestion;
    let pill = |text: String, strong: bool| {
        div()
            .flex_none()
            .px(px(6.))
            .py(px(1.))
            .rounded(px(4.))
            .border_1()
            .border_color(if strong { c.text_accent } else { c.border })
            .bg(if strong { c.accent_faint } else { c.element_bg })
            .text_size(px(11.5))
            .text_color(c.text)
            .child(s(text))
    };
    let n = chips.len();
    let mut row = div()
        .flex_none()
        .px(px(10.))
        .py(px(5.))
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(6.))
        .bg(c.editor)
        .border_b_1()
        .border_color(c.text_accent)
        .child(cho_layout_ui::widgets::small_icon("search", c.icon_accent));
    for (i, (f, _, label)) in chips.iter().enumerate() {
        let selected = o.chip_selected && i + 1 == n;
        let text = match f {
            crate::omni::Field::Tag => label.clone(),
            _ => format!("{}: {label}", f.name()),
        };
        row = row.child(pill(text, selected));
    }
    if let Some(f) = o.open {
        row = row.child(
            div()
                .flex_none()
                .px(px(6.))
                .py(px(1.))
                .rounded(px(4.))
                .border_1()
                .border_color(c.text_accent)
                .text_size(px(11.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c.text_accent)
                .child(s(format!("{} ▸", f.name()))),
        );
    }
    let placeholder = match o.open {
        Some(f) => format!(
            "Type to find {} {}…",
            if f.name().starts_with(['a', 'e', 'i', 'o', 'u']) {
                "an"
            } else {
                "a"
            },
            f.name()
        ),
        None if chips.is_empty() => {
            "Search, or type tag, assignee, status… then Tab (# for your tags)".into()
        }
        None => "Search, or a field then Tab · ⌫ removes chips".into(),
    };
    row = row.child(
        div()
            .flex_1()
            .min_w(px(100.))
            .child(o.input.render(c, &placeholder, false)),
    );
    let hint = div()
        .flex_none()
        .px(px(12.))
        .py(px(2.))
        .bg(c.editor)
        .text_size(px(11.))
        .text_color(c.text_muted)
        .child(s(if o.open.is_some() {
            "Tab or Enter takes the highlighted value · ↑↓ choose · ⌫ on empty or Esc cancels the chip"
        } else {
            "word + Tab or : makes a chip · # your tags · ⇧Tab reopens the last · ⌫ at the start removes one · Enter keeps · Esc clears text"
        }));
    let mut list = div()
        .flex_none()
        .flex()
        .flex_col()
        .py(px(2.))
        .bg(c.editor)
        .border_b_1()
        .border_color(c.border_variant);
    let shown = !suggestions.is_empty();
    for (i, sug) in suggestions.into_iter().enumerate() {
        let on = o.cursor == Some(i);
        let base = div()
            .h(px(24.))
            .px(px(12.))
            .flex()
            .items_center()
            .gap(px(8.))
            .text_size(px(12.))
            .when(on, |d| d.bg(c.ghost_selected));
        let el = match sug {
            Suggestion::Field(f) => base
                .child(
                    div()
                        .w(px(70.))
                        .flex_none()
                        .text_color(c.text_accent)
                        .child(s(f.name())),
                )
                .child(
                    div()
                        .flex_1()
                        .text_color(c.text_muted)
                        .child(s(format!("Tab or : to filter by {}", f.name()))),
                ),
            Suggestion::Value(item) => base
                .child(
                    div()
                        .w(px(12.))
                        .flex_none()
                        .text_color(c.text_accent)
                        .child(if item.checked { "✓" } else { "" }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(c.text)
                        .child(s(item.label)),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(11.))
                        .text_color(c.text_muted)
                        .child(s(item.detail)),
                ),
            Suggestion::GoTo(key, summary) => base
                .child(
                    div()
                        .flex_none()
                        .text_color(c.text_accent)
                        .child(s(format!("Go to {key}"))),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(c.text_muted)
                        .child(s(summary)),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(11.))
                        .text_color(c.text_muted)
                        .child("Enter"),
                ),
        };
        list = list.child(el);
    }
    div()
        .flex_none()
        .flex()
        .flex_col()
        .child(row)
        .child(hint)
        .when(shown, |d| d.child(list))
        .into_any_element()
}
