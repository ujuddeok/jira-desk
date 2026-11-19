//! A small text field with a caret, one line or several. Typing, paste, Cmd-A, word and line
//! delete go through `cho_layout_ui::field::field_key`, so they behave as in Chorong; the
//! caret keys (arrows, Home, End, and Enter in a multi-line field) are handled here.

use cho_layout_ui::colors::Colors;
use cho_layout_ui::field::{FieldKey, field_key};
use gpui::{
    AnyElement, HighlightStyle, IntoElement, Keystroke, ParentElement, SharedString, Styled,
    StyledText, div, px,
};

#[derive(Clone, Debug, Default)]
pub struct TextArea {
    pub text: String,
    /// Byte offset of the caret.
    pub cursor: usize,
    /// Cmd-A: the next edit replaces everything.
    pub all: bool,
    pub multiline: bool,
}

#[derive(Debug, PartialEq)]
pub enum Edited {
    No,
    Text,
    Moved,
    Copy(String),
}

impl TextArea {
    pub fn new(text: &str, multiline: bool) -> Self {
        Self {
            text: text.to_string(),
            cursor: text.len(),
            all: false,
            multiline,
        }
    }

    fn prev(&self, i: usize) -> usize {
        self.text[..i]
            .char_indices()
            .next_back()
            .map_or(0, |(j, _)| j)
    }

    fn next(&self, i: usize) -> usize {
        self.text[i..]
            .chars()
            .next()
            .map_or(i, |c| i + c.len_utf8())
    }

    fn line_start(&self, i: usize) -> usize {
        self.text[..i].rfind('\n').map_or(0, |j| j + 1)
    }

    fn line_end(&self, i: usize) -> usize {
        self.text[i..].find('\n').map_or(self.text.len(), |j| i + j)
    }

    /// Moves to the same column (in chars) on the line above or below.
    fn vertical(&mut self, down: bool) {
        let start = self.line_start(self.cursor);
        let col = self.text[start..self.cursor].chars().count();
        let target_start = if down {
            let end = self.line_end(self.cursor);
            if end >= self.text.len() {
                self.cursor = self.text.len();
                return;
            }
            end + 1
        } else {
            if start == 0 {
                self.cursor = 0;
                return;
            }
            self.line_start(start - 1)
        };
        let target_end = self.line_end(target_start);
        let mut i = target_start;
        for _ in 0..col {
            if i >= target_end {
                break;
            }
            i = self.next(i);
        }
        self.cursor = i;
    }

    fn insert(&mut self, s: &str) {
        if std::mem::take(&mut self.all) {
            self.text.clear();
            self.cursor = 0;
        }
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    /// Applies a keystroke. `paste` is the clipboard's text, for Cmd-V.
    pub fn key(&mut self, k: &Keystroke, paste: Option<&str>) -> Edited {
        let m = &k.modifiers;
        let plain = !m.control && !m.alt && !m.platform;
        let word_mod = if cfg!(target_os = "macos") {
            m.alt
        } else {
            m.control
        };
        let line_mod = if cfg!(target_os = "macos") {
            m.platform
        } else {
            false
        };
        match k.key.as_str() {
            "left" if word_mod || line_mod || plain => {
                self.all = false;
                self.cursor = if line_mod {
                    self.line_start(self.cursor)
                } else if word_mod {
                    self.word_left()
                } else {
                    self.prev(self.cursor)
                };
                return Edited::Moved;
            }
            "right" if word_mod || line_mod || plain => {
                self.all = false;
                self.cursor = if line_mod {
                    self.line_end(self.cursor)
                } else if word_mod {
                    self.word_right()
                } else {
                    self.next(self.cursor)
                };
                return Edited::Moved;
            }
            "up" | "down" if self.multiline && plain => {
                self.all = false;
                self.vertical(k.key == "down");
                return Edited::Moved;
            }
            "home" => {
                self.all = false;
                self.cursor = self.line_start(self.cursor);
                return Edited::Moved;
            }
            "end" => {
                self.all = false;
                self.cursor = self.line_end(self.cursor);
                return Edited::Moved;
            }
            "enter" if self.multiline && !m.platform && !m.control => {
                self.insert("\n");
                return Edited::Text;
            }
            "delete" if plain => {
                if std::mem::take(&mut self.all) {
                    self.text.clear();
                    self.cursor = 0;
                } else if self.cursor < self.text.len() {
                    let n = self.next(self.cursor);
                    self.text.replace_range(self.cursor..n, "");
                }
                return Edited::Text;
            }
            "v" if m.secondary() && self.multiline => {
                // Keep line breaks (field_key drops control characters).
                if let Some(p) = paste.filter(|p| !p.is_empty()) {
                    self.insert(&p.replace("\r\n", "\n"));
                    return Edited::Text;
                }
                return Edited::No;
            }
            _ => {}
        }
        match field_key(k, paste) {
            FieldKey::Insert(s) => {
                self.insert(&s);
                Edited::Text
            }
            FieldKey::Backspace => {
                if std::mem::take(&mut self.all) {
                    self.text.clear();
                    self.cursor = 0;
                } else if self.cursor > 0 {
                    let p = self.prev(self.cursor);
                    self.text.replace_range(p..self.cursor, "");
                    self.cursor = p;
                }
                Edited::Text
            }
            FieldKey::DeleteWord => {
                if std::mem::take(&mut self.all) {
                    self.text.clear();
                    self.cursor = 0;
                } else {
                    let p = self.word_left();
                    self.text.replace_range(p..self.cursor, "");
                    self.cursor = p;
                }
                Edited::Text
            }
            FieldKey::Clear => {
                // Cmd-Backspace: to the start of the line.
                let s = if self.all {
                    0
                } else {
                    self.line_start(self.cursor)
                };
                self.all = false;
                let s = if s == self.cursor && s > 0 {
                    self.prev(s)
                } else {
                    s
                };
                self.text.replace_range(s..self.cursor, "");
                self.cursor = s;
                Edited::Text
            }
            FieldKey::SelectAll => {
                self.all = !self.text.is_empty();
                Edited::Moved
            }
            FieldKey::Copy if self.all => Edited::Copy(self.text.clone()),
            FieldKey::Deselect => {
                self.all = false;
                Edited::Moved
            }
            FieldKey::Copy | FieldKey::Other => Edited::No,
        }
    }

    fn word_left(&self) -> usize {
        let mut i = self.cursor;
        while i > 0 && !is_word(self.text[..i].chars().next_back().unwrap_or(' ')) {
            i = self.prev(i);
        }
        while i > 0 && is_word(self.text[..i].chars().next_back().unwrap_or(' ')) {
            i = self.prev(i);
        }
        i
    }

    fn word_right(&self) -> usize {
        let mut i = self.cursor;
        while i < self.text.len() && !is_word(self.text[i..].chars().next().unwrap_or(' ')) {
            i = self.next(i);
        }
        while i < self.text.len() && is_word(self.text[i..].chars().next().unwrap_or(' ')) {
            i = self.next(i);
        }
        i
    }

    /// The text with a block caret (the character under it shown inverted), or the
    /// placeholder when empty.
    pub fn render(&self, c: &Colors, placeholder: &str, mono: bool) -> AnyElement {
        let caret_bg = c.text_accent;
        let caret_fg = c.editor;
        let base = div()
            .w_full()
            .text_color(c.text)
            .when_mono(mono, c)
            .line_height(px(19.));
        if self.text.is_empty() {
            let shown = format!(" {placeholder}");
            let hl = vec![(
                0..1,
                HighlightStyle {
                    background_color: Some(caret_bg),
                    ..Default::default()
                },
            )];
            return base
                .text_color(c.text_muted)
                .child(StyledText::new(SharedString::from(shown)).with_highlights(hl))
                .into_any_element();
        }
        if self.all {
            let hl = vec![(
                0..self.text.len(),
                HighlightStyle {
                    background_color: Some(c.accent_wash),
                    ..Default::default()
                },
            )];
            return base
                .child(StyledText::new(SharedString::from(self.text.clone())).with_highlights(hl))
                .into_any_element();
        }
        // The caret sits on the character after it; at a line end or the end of the text,
        // on an inserted space.
        let mut shown = self.text.clone();
        let at = self.cursor.min(shown.len());
        let ch_len = match shown[at..].chars().next() {
            Some('\n') | None => {
                shown.insert(at, ' ');
                1
            }
            Some(ch) => ch.len_utf8(),
        };
        let hl = vec![(
            at..at + ch_len,
            HighlightStyle {
                color: Some(caret_fg),
                background_color: Some(caret_bg),
                ..Default::default()
            },
        )];
        base.child(StyledText::new(SharedString::from(shown)).with_highlights(hl))
            .into_any_element()
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

trait MonoExt {
    fn when_mono(self, mono: bool, c: &Colors) -> Self;
}

impl MonoExt for gpui::Div {
    fn when_mono(self, mono: bool, c: &Colors) -> Self {
        if mono {
            self.font_family(c.mono_font.clone()).text_size(px(12.5))
        } else {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> Keystroke {
        Keystroke::parse(s).unwrap()
    }

    #[test]
    fn caret_moves_and_edits_across_lines() {
        let mut t = TextArea::new("ab\ncde", true);
        t.key(&key("up"), None);
        assert_eq!(t.cursor, 2); // end of "ab" (column 3 clamps to 2)
        t.key(&key("left"), None);
        t.key(&key("backspace"), None);
        assert_eq!(t.text, "b\ncde");
        t.key(&key("enter"), None);
        assert_eq!(t.text, "\nb\ncde");
        t.key(&key("end"), None);
        assert_eq!(t.cursor, 2);
        t.key(&key("down"), None);
        assert_eq!(t.cursor, 4);
    }
}
