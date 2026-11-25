//! Scrolling, the one way (Chorong's app/src/scroll.rs): every scrollable area and list in Jira
//! Desk goes through this module, so each stretches at its ends ([`crate::overscroll`]) and
//! follows the `overscroll` setting.
//!
//! `clippy.toml` forbids GPUI's raw scrolling (`overflow_*_scroll`, `uniform_list`, `list`)
//! everywhere else, so a new scrollable area cannot be added without its overscroll:
//!
//! - a plain area: [`ScrollArea::vertical`] (or [`ScrollArea::both`]), with the area's state
//!   held by the view or taken from a [`ScrollAreas`] by name;
//! - a list: [`uniform_list`] or [`list`], which give an [`Unwrapped`] list that is not an
//!   element: it is drawn only once wrapped with its handle and an [`Overscroll`].
//!
//! The box these return is what gets sized (`flex_1` and `min_h_0` to fill, or `size_full`);
//! an area that fits its content up to a cap uses [`ScrollArea::vertical_max`] instead of a
//! `max_h` on the box. Padding and layout go on the content.

#![allow(clippy::disallowed_methods)]
// The one place GPUI's raw scrolling is used: with overscroll.
// Chorong's whole API is kept, so the two copies stay the same; Jira Desk doesn't use all of it yet.
#![allow(dead_code)]

use crate::overscroll::{Overscroll, Stretch};
use gpui::{
    AnyElement, App, Div, ElementId, InteractiveElement, IntoElement, List, ListState, Pixels,
    ScrollHandle, SharedString, StatefulInteractiveElement, Styled, UniformList,
    UniformListScrollHandle, Window,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;

/// A scrollable area's state: where it is scrolled to and how far it is stretched. Clones share
/// it, so a view keeps one per area (or asks a [`ScrollAreas`] for it by name).
#[derive(Clone, Default)]
pub struct ScrollArea {
    pub handle: ScrollHandle,
    over: Overscroll,
}

impl ScrollArea {
    /// An area scrolled by a handle the view keeps elsewhere (to scroll it from code), with its
    /// stretch.
    pub fn with(handle: ScrollHandle, over: Overscroll) -> Self {
        ScrollArea { handle, over }
    }

    /// This area scrolled by `handle`, one the view keeps to scroll it from code; the stretch
    /// stays this area's.
    pub fn tracking(mut self, handle: ScrollHandle) -> Self {
        self.handle = handle;
        self
    }

    /// The stretch, for a list wrapped with [`Unwrapped::wrap`].
    pub fn overscroll(&self) -> Overscroll {
        self.over.clone()
    }

    /// `content` scrolling up and down, stretching at its ends.
    pub fn vertical(
        &self,
        id: impl Into<ElementId>,
        stretch: impl Into<Stretch>,
        content: Div,
    ) -> Div {
        let inner = content
            .id(id)
            .overflow_y_scroll()
            .track_scroll(&self.handle);
        self.over.wrap(self.handle.clone(), stretch, inner)
    }

    /// `content` as tall as it needs up to `max`, scrolling beyond that (a font list, an output
    /// box). The cap is set on the scrolling content as well as the box: a box sized only by
    /// its content does not hold its scrolling child to the box's cap, so the child would grow
    /// to its full height and never scroll (the page behind it would). Don't add `max_h` to
    /// the returned box; pass it here.
    pub fn vertical_max(
        &self,
        id: impl Into<ElementId>,
        stretch: impl Into<Stretch>,
        max: Pixels,
        content: Div,
    ) -> Div {
        let inner = content
            .id(id)
            .max_h(max)
            .overflow_y_scroll()
            .track_scroll(&self.handle);
        self.over
            .wrap(self.handle.clone(), stretch, inner)
            .max_h(max)
    }

    /// `content` scrolling both ways (a graph); it stretches up and down.
    pub fn both(&self, id: impl Into<ElementId>, stretch: impl Into<Stretch>, content: Div) -> Div {
        let inner = content.id(id).overflow_scroll().track_scroll(&self.handle);
        self.over.wrap(self.handle.clone(), stretch, inner)
    }
}

/// Scroll areas by name, for views that draw them as they go (a dialog, a pane's column): each
/// name keeps its scroll position and stretch from one frame to the next.
#[derive(Default)]
pub struct ScrollAreas(RefCell<HashMap<SharedString, ScrollArea>>);

impl ScrollAreas {
    /// The area called `name`, made on first use.
    pub fn get(&self, name: impl Into<SharedString>) -> ScrollArea {
        self.0.borrow_mut().entry(name.into()).or_default().clone()
    }
}

/// A list that is not drawn until it is wrapped with its overscroll ([`Unwrapped::wrap`]).
/// Style it with [`Unwrapped::map`] first.
#[must_use = "a list is drawn only once wrapped: call .wrap(..)"]
pub struct Unwrapped<L>(L);

impl<L> Unwrapped<L> {
    /// Styles the list itself (`pb_2`, a sizing behaviour) before it is wrapped.
    pub fn map(self, f: impl FnOnce(L) -> L) -> Self {
        Unwrapped(f(self.0))
    }
}

impl Unwrapped<UniformList> {
    /// The list, scrolled by `handle`, in a box that stretches at its ends.
    pub fn wrap(
        self,
        over: &Overscroll,
        handle: &UniformListScrollHandle,
        stretch: impl Into<Stretch>,
    ) -> Div {
        over.wrap(handle.clone(), stretch, self.0.track_scroll(handle.clone()))
    }
}

impl Unwrapped<List> {
    /// The list, scrolled by its `state`, in a box that stretches at its ends.
    pub fn wrap(self, over: &Overscroll, state: &ListState, stretch: impl Into<Stretch>) -> Div {
        over.wrap(state.clone(), stretch, self.0)
    }
}

/// [`gpui::uniform_list`], to be wrapped.
pub fn uniform_list<R: IntoElement>(
    id: impl Into<ElementId>,
    count: usize,
    rows: impl 'static + Fn(Range<usize>, &mut Window, &mut App) -> Vec<R>,
) -> Unwrapped<UniformList> {
    Unwrapped(gpui::uniform_list(id, count, rows))
}

/// [`gpui::list`], to be wrapped.
pub fn list(
    state: ListState,
    row: impl FnMut(usize, &mut Window, &mut App) -> AnyElement + 'static,
) -> Unwrapped<List> {
    Unwrapped(gpui::list(state, row))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_named_area_keeps_its_state_from_frame_to_frame() {
        let areas = ScrollAreas::default();
        let a = areas.get("highlights");
        a.handle
            .set_offset(gpui::point(gpui::px(0.), gpui::px(-40.)));
        // The same name gives the same scroll position (the handle is shared), another name
        // its own.
        assert_eq!(areas.get("highlights").handle.offset().y, gpui::px(-40.));
        assert_eq!(areas.get("cheat-sheet").handle.offset().y, gpui::px(0.));
        // A view's own handle replaces the area's, so code that scrolls it still works.
        let own = ScrollHandle::new();
        own.set_offset(gpui::point(gpui::px(0.), gpui::px(-7.)));
        assert_eq!(
            areas.get("cmp-rows").tracking(own).handle.offset().y,
            gpui::px(-7.)
        );
    }
}
