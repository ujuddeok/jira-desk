//! Selecting and copying text in the issue pane (Preview and issue tabs), as in Chorong's code
//! view: press and drag, double click a word, triple click a paragraph, Shift-click to extend,
//! ⌘A and ⌘C.
//!
//! The pane's text is many pieces (the title, field values, paragraphs, list items, table
//! cells, code blocks, comments). Each piece registers itself in the pane's [`TextMap`] as it is
//! drawn, in reading order, with its text layout; a selection runs from one (piece, byte) to
//! another across them. A piece also records where it was drawn (an invisible canvas beside
//! it), so the pointer is only ever matched against pieces that have been laid out.

use gpui::{
    Bounds, HighlightStyle, Hsla, IntoElement, ParentElement, Pixels, Point, SharedString, Styled,
    StyledText, TextLayout, canvas, div,
};
use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

/// A place in a pane's text: which piece (in reading order) and which byte of it.
pub type TextPos = (usize, usize);

/// A selection in one pane's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextSel {
    pub anchor: TextPos,
    pub head: TextPos,
    /// The button is down: moves extend it.
    pub dragging: bool,
}

impl TextSel {
    pub fn new(anchor: TextPos, head: TextPos) -> Self {
        TextSel {
            anchor,
            head,
            dragging: false,
        }
    }

    /// Start and end, in reading order.
    pub fn span(&self) -> (TextPos, TextPos) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// The bytes of piece `i` (of `len` bytes) inside the selection.
    pub fn within(&self, i: usize, len: usize) -> Option<Range<usize>> {
        let (a, b) = self.span();
        if i < a.0 || i > b.0 {
            return None;
        }
        let start = if i == a.0 { a.1.min(len) } else { 0 };
        let end = if i == b.0 { b.1.min(len) } else { len };
        (start < end).then_some(start..end)
    }
}

struct Piece {
    text: SharedString,
    layout: TextLayout,
    /// Where it was last drawn; `None` until it has been.
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

/// One pane's selectable text, in reading order, as last drawn. Clones share it.
#[derive(Clone, Default)]
pub struct TextMap(Rc<RefCell<Vec<Piece>>>);

impl TextMap {
    /// Before drawing the pane again: its pieces are registered afresh.
    pub fn clear(&self) {
        self.0.borrow_mut().clear();
    }

    /// The index the next piece will get.
    pub fn next(&self) -> usize {
        self.0.borrow().len()
    }

    pub fn len_of(&self, i: usize) -> usize {
        self.0.borrow().get(i).map_or(0, |p| p.text.len())
    }

    pub fn last(&self) -> Option<TextPos> {
        let pieces = self.0.borrow();
        pieces
            .len()
            .checked_sub(1)
            .map(|i| (i, pieces[i].text.len()))
    }

    /// `styled` (showing `text`) as the next piece: it registers, and records where it's drawn.
    pub fn piece(&self, text: SharedString, styled: StyledText) -> gpui::Div {
        let bounds = Rc::new(Cell::new(None));
        self.0.borrow_mut().push(Piece {
            text,
            layout: styled.layout().clone(),
            bounds: bounds.clone(),
        });
        div().relative().child(styled).child(
            canvas(move |b, _, _| bounds.set(Some(b)), |_, _, _, _| {})
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
        )
    }

    /// The piece and byte under `p`. With `nearest` (a drag), a point between pieces gives the
    /// closest end: past a piece's right or below it, its end; left of it, its start.
    pub fn hit(&self, p: Point<Pixels>, nearest: bool) -> Option<TextPos> {
        let pieces = self.0.borrow();
        let drawn = pieces
            .iter()
            .enumerate()
            .filter_map(|(i, pc)| pc.bounds.get().map(|b| (i, pc, b)));
        let mut best: Option<TextPos> = None;
        for (i, pc, b) in drawn {
            if b.contains(&p) {
                let (Ok(ix) | Err(ix)) = pc.layout.index_for_position(p);
                return Some((i, ix.min(pc.text.len())));
            }
            if !nearest {
                continue;
            }
            if p.y >= b.top() && p.y < b.bottom() {
                // Beside it, on one of its lines.
                let (Ok(ix) | Err(ix)) = pc.layout.index_for_position(p);
                best = Some((i, ix.min(pc.text.len())));
            } else if p.y >= b.bottom() {
                best = Some((i, pc.text.len()));
            } else if best.is_none() {
                best = Some((i, 0));
                break;
            }
        }
        best
    }

    /// The word around byte `at` of piece `i`.
    pub fn word(&self, (i, at): TextPos) -> TextSel {
        let pieces = self.0.borrow();
        let Some(pc) = pieces.get(i) else {
            return TextSel::new((i, at), (i, at));
        };
        let t = pc.text.as_str();
        let is_word = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
        let at = at.min(t.len());
        let start = t[..at]
            .char_indices()
            .rev()
            .take_while(|(_, c)| is_word(*c))
            .last()
            .map_or(at, |(b, _)| b);
        let end = t[at..]
            .char_indices()
            .find(|(_, c)| !is_word(*c))
            .map_or(t.len(), |(b, _)| at + b);
        TextSel::new((i, start), (i, end))
    }

    /// The selected text: whole pieces on their own lines.
    pub fn text(&self, sel: &TextSel) -> String {
        let pieces = self.0.borrow();
        let mut out = Vec::new();
        for (i, pc) in pieces.iter().enumerate() {
            if let Some(r) = sel.within(i, pc.text.len()) {
                out.push(pc.text[r].to_string());
            }
        }
        out.join("\n")
    }
}

/// `hl` (sorted, not overlapping, as `StyledText` takes them) with `sel` laid over: inside it,
/// each run also gets the selection's background.
pub fn overlay(
    hl: Vec<(Range<usize>, HighlightStyle)>,
    sel: Option<Range<usize>>,
    bg: Hsla,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let Some(sel) = sel else { return hl };
    let mut cuts: Vec<usize> = vec![sel.start, sel.end];
    for (r, _) in &hl {
        cuts.push(r.start);
        cuts.push(r.end);
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut out = Vec::new();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let base = hl
            .iter()
            .find(|(r, _)| r.start <= a && b <= r.end)
            .map(|(_, h)| *h);
        let inside = sel.start <= a && b <= sel.end;
        let style = match (base, inside) {
            (Some(h), true) => Some(HighlightStyle {
                background_color: Some(bg),
                ..h
            }),
            (Some(h), false) => Some(h),
            (None, true) => Some(HighlightStyle {
                background_color: Some(bg),
                ..Default::default()
            }),
            (None, false) => None,
        };
        if let Some(st) = style {
            out.push((a..b, st));
        }
    }
    out
}

/// A plain piece of text, selectable.
pub fn plain(
    map: &TextMap,
    sel: Option<TextSel>,
    bg: Hsla,
    text: impl Into<SharedString>,
) -> impl IntoElement {
    let text: SharedString = text.into();
    let i = map.next();
    let hl = overlay(Vec::new(), sel.and_then(|s| s.within(i, text.len())), bg);
    let styled = StyledText::new(text.clone()).with_highlights(hl);
    map.piece(text, styled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::FontWeight;

    #[test]
    fn a_selection_spans_pieces_in_reading_order() {
        let s = TextSel::new((3, 2), (1, 4));
        assert_eq!(s.span(), ((1, 4), (3, 2)));
        assert_eq!(s.within(0, 10), None);
        assert_eq!(s.within(1, 10), Some(4..10));
        assert_eq!(s.within(2, 7), Some(0..7));
        assert_eq!(s.within(3, 10), Some(0..2));
        assert_eq!(s.within(4, 10), None);
        assert!(TextSel::new((1, 1), (1, 1)).is_empty());
    }

    #[test]
    fn copied_text_puts_pieces_on_their_own_lines() {
        let map = TextMap::default();
        for t in ["Retry CSV import", "Unassigned", "Sprint 43"] {
            let _ = map.piece(t.into(), StyledText::new(t));
        }
        assert_eq!(map.last(), Some((2, 9)));
        let sel = TextSel::new((0, 6), (2, 6));
        assert_eq!(map.text(&sel), "CSV import\nUnassigned\nSprint");
        assert_eq!(map.word((0, 8)), TextSel::new((0, 6), (0, 9)));
    }

    #[test]
    fn the_selection_lays_over_existing_styles() {
        let bold = HighlightStyle {
            font_weight: Some(FontWeight::BOLD),
            ..Default::default()
        };
        let bg = gpui::red();
        let out = overlay(vec![(2..6, bold)], Some(4..9), bg);
        let ranges: Vec<_> = out.iter().map(|(r, _)| r.clone()).collect();
        assert_eq!(ranges, vec![2..4, 4..6, 6..9]);
        assert_eq!(out[0].1.background_color, None);
        assert_eq!(out[1].1.font_weight, Some(FontWeight::BOLD));
        assert_eq!(out[1].1.background_color, Some(bg));
        assert_eq!(out[2].1.font_weight, None);
        assert_eq!(out[2].1.background_color, Some(bg));
        assert_eq!(overlay(vec![(0..3, bold)], None, bg), vec![(0..3, bold)]);
    }
}
