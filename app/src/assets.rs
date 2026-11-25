//! Icons: Jira Desk's own (Lucide, `assets/icons`), then the layout shell's.

use gpui::{AnyElement, AssetSource, Hsla, IntoElement, SharedString, Styled, px, svg};
use std::borrow::Cow;

macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_bytes!(concat!("../assets/icons/", $name, ".svg")))),*]
    };
}

const ICONS: &[(&str, &[u8])] = icons![
    "arrow-up-down",
    "calendar",
    "circle-alert",
    "circle-check",
    "circle-dashed",
    "circle-dot",
    "cloud-off",
    "filter",
    "flag",
    "kanban",
    "keyboard",
    "message-square",
    "refresh-cw",
    "star",
    "sticky-note",
    "tag",
    "upload",
    "user",
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let own = path
            .strip_prefix("jd/")
            .and_then(|p| p.strip_suffix(".svg"))
            .and_then(|n| ICONS.iter().find(|(i, _)| *i == n))
            .map(|(_, b)| Cow::Borrowed(*b));
        match own {
            Some(b) => Ok(Some(b)),
            None => cho_layout_ui::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        let mut out: Vec<SharedString> = ICONS
            .iter()
            .map(|(n, _)| SharedString::from(format!("jd/{n}.svg")))
            .filter(|p| p.starts_with(path))
            .collect();
        out.extend(cho_layout_ui::assets::Assets.list(path)?);
        Ok(out)
    }
}

/// One of Jira Desk's icons at `size` px.
pub fn icon(name: &str, size: f32, color: Hsla) -> AnyElement {
    debug_assert!(ICONS.iter().any(|(n, _)| *n == name), "unknown icon {name}");
    svg()
        .path(SharedString::from(format!("jd/{name}.svg")))
        .size(px(size))
        .flex_none()
        .text_color(color)
        .into_any_element()
}
