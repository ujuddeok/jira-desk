//! Draws Jira's rich text (descriptions and comments, from `jdesk_core::adf`).
//!
//! - Links are clickable (a click on the paragraph, resolved to the character under it). A link to an issue in your local copy opens it in a tab and shows
//!   its key and summary; any other link opens in the browser.
//! - Tables are grids, with header cells, spanned columns and cell colours, and whatever the
//!   cells hold (lists, code, formatting).
//! - Panels, quotes and expands draw their whole content.
//! - Images attached to the issue show inline once downloaded (click for full size); other
//!   files show as chips.
//! - Text colour, highlight, bold, italic, underline, strikethrough, code and @mentions.

use crate::panes::s;
use crate::state::{ImageState, Model};
use cho_layout_ui::colors::Colors;
use gpui::{
    AnyElement, ElementId, Entity, FontStyle, FontWeight, HighlightStyle, Hsla, InteractiveElement,
    IntoElement, ObjectFit, ParentElement, StatefulInteractiveElement, StrikethroughStyle,
    StyleRefinement, Styled, StyledImage, StyledText, UnderlineStyle, div, img,
    prelude::FluentBuilder, px,
};
use jdesk_core::adf::{Block, Cell, Span};
use jdesk_core::model::Attachment;
use std::cell::Cell as Counter;
use std::collections::HashMap;

/// The widest an inline image is drawn.
pub const IMAGE_MAX_W: f32 = 560.;

pub struct Rich<'a> {
    c: &'a Colors,
    model: &'a Entity<Model>,
    m: &'a Model,
    /// Which attachment each media node shows.
    media: &'a HashMap<String, Attachment>,
    /// Keeps element ids unique within one pane.
    prefix: String,
    next: Counter<usize>,
}

/// `#rrggbb` as a colour.
pub fn hex(c: &str) -> Option<Hsla> {
    let v = u32::from_str_radix(c.strip_prefix('#')?, 16).ok()?;
    Some(gpui::rgb(v).into())
}

/// The issue key in a link to an issue on any Jira site (`…/browse/ABC-123`).
pub fn issue_key_in(url: &str) -> Option<&str> {
    let tail = url.split("/browse/").nth(1)?;
    let key = tail.split(['?', '#', '/']).next()?;
    let (proj, num) = key.split_once('-')?;
    (!proj.is_empty()
        && proj.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !num.is_empty()
        && num.chars().all(|c| c.is_ascii_digit()))
    .then_some(key)
}

pub fn file_size(n: u64) -> String {
    match n {
        n if n < 1024 => format!("{n} B"),
        n if n < 1024 * 1024 => format!("{:.0} KB", n as f64 / 1024.),
        n => format!("{:.1} MB", n as f64 / 1024. / 1024.),
    }
}

/// Whether a link from Jira may be handed to the system: web and mail links only. Links are
/// written by other people, and any other scheme (`file:`, `smb:`, an app's own scheme) would
/// open a file or start another app at a click.
pub fn web_link(url: &str) -> bool {
    let scheme = url
        .trim()
        .split_once(':')
        .map(|(s, _)| s.to_ascii_lowercase());
    matches!(scheme.as_deref(), Some("https" | "http" | "mailto"))
}

/// Opens a link: an issue we have opens in a tab, a web link in the browser. Any other link is
/// copied instead, to open yourself if you trust it.
pub fn follow(model: &Entity<Model>, url: &str, cx: &mut gpui::App) {
    let local = issue_key_in(url)
        .filter(|k| model.read(cx).desk.issue(k).is_some())
        .map(str::to_string);
    match local {
        Some(k) => model.update(cx, |m, cx| {
            m.pending_open = Some(k);
            cx.notify();
        }),
        None if web_link(url) => cx.open_url(url),
        None => {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(url.to_string()));
            model.update(cx, |m, cx| {
                m.error(format!(
                    "Not opened: only web links open from Jira text. Copied it: {url}"
                ));
                cx.notify();
            });
        }
    }
}

/// A downloaded image's own file (never a URL from Jira): it is only shown once it decoded as
/// an image, under a name with an image extension (see `Model::ensure_images`).
fn file_url(path: &std::path::Path) -> String {
    format!("file://{}", path.display())
}

impl<'a> Rich<'a> {
    pub fn new(
        c: &'a Colors,
        model: &'a Entity<Model>,
        m: &'a Model,
        media: &'a HashMap<String, Attachment>,
        prefix: &str,
    ) -> Self {
        Self {
            c,
            model,
            m,
            media,
            prefix: prefix.to_string(),
            next: Counter::new(0),
        }
    }

    fn id(&self, what: &str) -> ElementId {
        let n = self.next.get();
        self.next.set(n + 1);
        ElementId::Name(s(format!("{}-{what}-{n}", self.prefix)))
    }

    /// Inline text, with clickable links.
    pub fn spans(&self, spans: &[Span]) -> AnyElement {
        let c = self.c;
        let mut text = String::new();
        let mut hl: Vec<(std::ops::Range<usize>, HighlightStyle)> = Vec::new();
        let mut links: Vec<(std::ops::Range<usize>, String)> = Vec::new();
        for sp in spans {
            // A bare link to an issue we have reads as "ABC-12 Its summary".
            let shown = match &sp.link {
                Some(url) if *url == sp.text => match issue_key_in(url) {
                    Some(k) => match self.m.effective_issue(k) {
                        Some(i) => format!("{k} {}", i.summary),
                        None => k.to_string(),
                    },
                    None => sp.text.clone(),
                },
                _ => sp.text.clone(),
            };
            let start = text.len();
            text.push_str(&shown);
            let end = text.len();
            if start == end {
                continue;
            }
            let mut h = HighlightStyle::default();
            if sp.bold {
                h.font_weight = Some(FontWeight::BOLD);
            }
            if sp.italic {
                h.font_style = Some(FontStyle::Italic);
            }
            if let Some(col) = sp.color.as_deref().and_then(hex) {
                h.color = Some(col);
            }
            if let Some(col) = sp.highlight.as_deref().and_then(hex) {
                h.background_color = Some(col.opacity(0.35));
            }
            if sp.code {
                h.background_color = Some(c.element_bg);
                h.color = Some(c.text);
            }
            if sp.strike {
                h.strikethrough = Some(StrikethroughStyle {
                    thickness: px(1.),
                    color: Some(c.text_muted),
                });
            }
            if sp.underline {
                h.underline = Some(UnderlineStyle {
                    thickness: px(1.),
                    color: None,
                    wavy: false,
                });
            }
            if let Some(url) = &sp.link {
                h.color = Some(c.text_accent);
                h.underline = Some(UnderlineStyle {
                    thickness: px(1.),
                    color: Some(c.text_accent.opacity(0.6)),
                    wavy: false,
                });
                links.push((start..end, url.clone()));
            }
            if sp.mention {
                h.color = Some(c.text_accent);
                h.background_color = Some(c.accent_faint);
            }
            if h != HighlightStyle::default() {
                hl.push((start..end, h));
            }
        }
        let styled = StyledText::new(s(text)).with_highlights(hl);
        if links.is_empty() {
            return div().child(styled).into_any_element();
        }
        // A plain click on the paragraph, then which character it hit: pane bodies are cached
        // views, where InteractiveText's mouse-up listener (added on a redraw after
        // mouse-down) never arrives.
        let layout = styled.layout().clone();
        let model = self.model.clone();
        div()
            .id(self.id("text"))
            .child(styled)
            .on_click(move |ev, _window, cx| {
                if let Ok(ix) = layout.index_for_position(ev.position())
                    && let Some((_, url)) = links.iter().find(|(r, _)| r.contains(&ix))
                {
                    follow(&model, url, cx);
                }
            })
            .into_any_element()
    }

    pub fn blocks(&self, blocks: &[Block]) -> AnyElement {
        let mut col = div().flex().flex_col().gap(px(6.)).min_w_0();
        for b in blocks {
            col = col.child(self.block(b));
        }
        col.into_any_element()
    }

    fn block(&self, b: &Block) -> AnyElement {
        let c = self.c;
        match b {
            Block::Paragraph { spans } => self.spans(spans),
            Block::Heading { level, spans } => div()
                .pt(px(4.))
                .text_size(px(match level {
                    1 => 17.,
                    2 => 15.,
                    _ => 13.5,
                }))
                .font_weight(FontWeight::SEMIBOLD)
                .child(self.spans(spans))
                .into_any_element(),
            Block::Item {
                depth,
                number,
                checked,
                spans,
            } => {
                let mark = match (checked, number) {
                    (Some(true), _) => "☑".to_string(),
                    (Some(false), _) => "☐".to_string(),
                    (None, Some(n)) => format!("{n}."),
                    (None, None) => if depth % 2 == 0 { "•" } else { "◦" }.to_string(),
                };
                div()
                    .pl(px(4. + 16. * f32::from(*depth)))
                    .flex()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex_none()
                            .min_w(px(12.))
                            .text_color(c.text_muted)
                            .child(s(mark)),
                    )
                    .child(div().flex_1().min_w_0().child(self.spans(spans)))
                    .into_any_element()
            }
            Block::Code { lang, text } => div()
                .relative()
                .p(px(8.))
                .rounded(px(4.))
                .bg(c.element_bg)
                .font_family(c.mono_font.clone())
                .text_size(px(12.))
                .line_height(px(17.))
                .child(s(text.clone()))
                .when_some(
                    lang.clone().filter(|l| !l.is_empty() && l != "text"),
                    |d, l| {
                        d.child(
                            div()
                                .absolute()
                                .top(px(4.))
                                .right(px(6.))
                                .text_size(px(10.))
                                .text_color(c.text_muted)
                                .child(s(l)),
                        )
                    },
                )
                .into_any_element(),
            Block::Quote { blocks } => div()
                .pl(px(10.))
                .border_l_2()
                .border_color(c.border)
                .text_color(c.text_muted)
                .child(self.blocks(blocks))
                .into_any_element(),
            Block::Panel { kind, blocks } => {
                let (edge, tint, glyph) = match kind.as_str() {
                    "warning" => (c.warning, c.warning_bg, "circle-alert"),
                    "error" => (c.error, c.error_bg, "circle-alert"),
                    "success" => (c.success, c.success_bg, "circle-check"),
                    "note" => (Some(c.kind_enum), None, "sticky-note"),
                    _ => (Some(c.text_accent), None, "circle-dot"),
                };
                let edge = edge.unwrap_or(c.text_accent);
                div()
                    .p(px(8.))
                    .rounded(px(4.))
                    .bg(tint.unwrap_or(edge.opacity(0.12)))
                    .border_l_2()
                    .border_color(edge)
                    .flex()
                    .gap(px(8.))
                    .child(
                        div()
                            .pt(px(2.))
                            .child(crate::assets::icon(glyph, 13., edge)),
                    )
                    .child(div().flex_1().min_w_0().child(self.blocks(blocks)))
                    .into_any_element()
            }
            Block::Expand { title, blocks } => div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .text_color(c.text_muted)
                        .font_weight(FontWeight::MEDIUM)
                        .child("▾")
                        .child(s(if title.is_empty() {
                            "Details".to_string()
                        } else {
                            title.clone()
                        })),
                )
                .child(
                    div()
                        .ml(px(4.))
                        .pl(px(10.))
                        .border_l_1()
                        .border_color(c.border_variant)
                        .child(self.blocks(blocks)),
                )
                .into_any_element(),
            Block::Table { rows } => self.table(rows),
            Block::Media {
                id,
                alt,
                url,
                width,
                height,
            } => self.media(id, alt.as_deref(), url.as_deref(), *width, *height),
            Block::Rule => div().h(px(1.)).my(px(4.)).bg(c.border).into_any_element(),
        }
    }

    fn table(&self, rows: &[Vec<Cell>]) -> AnyElement {
        let c = self.c;
        let cols = rows
            .iter()
            .map(|r| r.iter().map(|x| u32::from(x.colspan)).sum::<u32>())
            .max()
            .unwrap_or(1)
            .max(1);
        let mut t = div()
            .flex()
            .flex_col()
            .w_full()
            .border_1()
            .border_color(c.border)
            .rounded(px(4.))
            .overflow_hidden()
            .text_size(px(12.5));
        let last = rows.len().saturating_sub(1);
        for (ri, row) in rows.iter().enumerate() {
            let mut r = div()
                .flex()
                .w_full()
                .when(ri < last, |d| d.border_b_1().border_color(c.border_variant));
            let mut used = 0;
            let n = row.len();
            for (ci, cell) in row.iter().enumerate() {
                let span = u32::from(cell.colspan);
                used += span;
                let mut d = div()
                    .min_w_0()
                    .px(px(8.))
                    .py(px(5.))
                    .when(ci + 1 < n, |d| {
                        d.border_r_1().border_color(c.border_variant)
                    })
                    .when(cell.header, |d| {
                        d.bg(c.element_bg).font_weight(FontWeight::SEMIBOLD)
                    })
                    .when_some(cell.background.as_deref().and_then(hex), |d, col| {
                        d.bg(col.opacity(0.2))
                    })
                    .child(self.blocks(&cell.blocks));
                let st: &mut StyleRefinement = d.style();
                st.flex_grow = Some(span as f32);
                st.flex_shrink = Some(1.);
                st.flex_basis = Some(px(0.).into());
                r = r.child(d);
            }
            // A short row: fill the rest so the grid lines up.
            if used < cols {
                let mut pad = div();
                let st = pad.style();
                st.flex_grow = Some((cols - used) as f32);
                st.flex_basis = Some(px(0.).into());
                r = r.child(pad);
            }
            t = t.child(r);
        }
        t.into_any_element()
    }

    fn media(
        &self,
        id: &str,
        alt: Option<&str>,
        url: Option<&str>,
        width: Option<u32>,
        height: Option<u32>,
    ) -> AnyElement {
        let c = self.c;
        if let Some(url) = url {
            // An image linked from elsewhere: not fetched; a link to it.
            return self.file_chip(
                alt.unwrap_or(url),
                "linked image",
                Some(url.to_string()),
                None,
            );
        }
        let Some(a) = self.media.get(id) else {
            return self.file_chip(
                alt.unwrap_or("attachment"),
                "not attached to this issue",
                None,
                None,
            );
        };
        if !a.is_image() {
            return self.file_chip(
                &a.filename,
                &file_size(a.size),
                Some(a.content.clone()),
                None,
            );
        }
        let fit = |w: u32, h: u32| {
            let dw = (w as f32).clamp(40., IMAGE_MAX_W);
            (dw, dw * h.max(1) as f32 / w.max(1) as f32)
        };
        match self.m.images.get(&a.id) {
            Some(ImageState::Ready {
                path,
                width: w,
                height: h,
            }) => {
                let (dw, dh) = fit(*w, *h);
                let url = file_url(path);
                div()
                    .id(self.id("img"))
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        img(path.clone())
                            .w(px(dw))
                            .h(px(dh))
                            .max_w_full()
                            .object_fit(ObjectFit::Contain)
                            .rounded(px(4.))
                            .border_1()
                            .border_color(c.border_variant),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(c.text_muted)
                            .child(s(format!(
                                "{} · {}×{} · click for full size",
                                a.filename, w, h
                            ))),
                    )
                    .cursor_pointer()
                    .on_click(move |_, _, cx| cx.open_url(&url))
                    .into_any_element()
            }
            Some(ImageState::Failed(e)) => self.file_chip(
                &a.filename,
                &format!("couldn't download: {e}"),
                Some(a.content.clone()),
                Some(c.error.unwrap_or(c.text)),
            ),
            _ => {
                let (dw, dh) = fit(width.unwrap_or(640), height.unwrap_or(360));
                div()
                    .w(px(dw))
                    .h(px(dh.min(240.)))
                    .max_w_full()
                    .rounded(px(4.))
                    .border_1()
                    .border_dashed()
                    .border_color(c.border)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(c.text_muted)
                    .text_size(px(12.))
                    .child(s(format!("Downloading {}…", a.filename)))
                    .into_any_element()
            }
        }
    }

    /// A file as a chip; click opens `url` (in the browser: Jira asks you to sign in there; the
    /// URL is Jira's, so it goes through [`follow`]).
    pub fn file_chip(
        &self,
        name: &str,
        detail: &str,
        url: Option<String>,
        tone: Option<Hsla>,
    ) -> AnyElement {
        let c = self.c;
        let model = self.model.clone();
        div()
            .id(self.id("file"))
            .flex()
            .items_center()
            .gap(px(6.))
            .px(px(8.))
            .py(px(4.))
            .rounded(px(4.))
            .border_1()
            .border_color(c.border_variant)
            .bg(c.element_bg)
            .max_w_full()
            .child(cho_layout_ui::widgets::small_icon("file", c.icon_muted))
            .child(div().min_w_0().truncate().child(s(name)))
            .child(
                div()
                    .flex_none()
                    .text_size(px(11.))
                    .text_color(tone.unwrap_or(c.text_muted))
                    .child(s(detail)),
            )
            .when_some(url, |d, url| {
                d.cursor_pointer()
                    .hover(|d| d.border_color(c.text_accent))
                    .on_click(move |_, _, cx| follow(&model, &url, cx))
            })
            .into_any_element()
    }

    /// Every attachment, images as thumbnails, for the issue pane's Attachments section.
    pub fn attachments(&self, list: &[Attachment]) -> AnyElement {
        let c = self.c;
        let mut wrap = div().flex().flex_wrap().items_start().gap(px(8.));
        for a in list {
            let el = match self.m.images.get(&a.id) {
                Some(ImageState::Ready {
                    path,
                    width,
                    height,
                }) if a.is_image() => {
                    let h = 90.;
                    let w = (h * *width as f32 / (*height).max(1) as f32).clamp(60., 220.);
                    let url = file_url(path);
                    div()
                        .id(self.id("thumb"))
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .w(px(w))
                        .child(
                            img(path.clone())
                                .w(px(w))
                                .h(px(h))
                                .object_fit(ObjectFit::Cover)
                                .rounded(px(4.))
                                .border_1()
                                .border_color(c.border_variant),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(px(11.))
                                .text_color(c.text_muted)
                                .child(s(a.filename.clone())),
                        )
                        .cursor_pointer()
                        .on_click(move |_, _, cx| cx.open_url(&url))
                        .into_any_element()
                }
                _ => self.file_chip(
                    &a.filename,
                    &file_size(a.size),
                    Some(a.content.clone()),
                    None,
                ),
            };
            wrap = wrap.child(el);
        }
        wrap.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_links_are_recognised() {
        assert_eq!(
            issue_key_in("https://acme.atlassian.net/browse/ABC-12"),
            Some("ABC-12")
        );
        assert_eq!(
            issue_key_in("https://x/browse/ABC-12?focusedCommentId=3"),
            Some("ABC-12")
        );
        assert_eq!(issue_key_in("https://x/browse/ABC"), None);
        assert_eq!(issue_key_in("https://example.com/design"), None);
        assert!(web_link("https://example.com/design"));
        assert!(web_link("HTTP://example.com"));
        assert!(web_link("mailto:ana@example.com"));
        for bad in [
            "file:///etc/passwd",
            "smb://server/share",
            "vscode://file/x",
            "javascript:alert(1)",
            "jiradesk://open/A-1",
            "x-apple.systempreferences:",
            "no scheme",
        ] {
            assert!(!web_link(bad), "{bad}");
        }
        assert_eq!(file_size(48_213), "47 KB");
        assert!(hex("#ff5630").is_some() && hex("red").is_none());
    }
}
