//! The keymap: `keymap/default.json`, in Zed's format (context, then keystroke → action), as
//! in Chorong. `cmd` means Cmd on macOS and Ctrl elsewhere. A `keymap.json` in the data
//! folder, in the same format, is loaded on top: later bindings win.

use gpui::{App, KeyBinding, KeyBindingContextPredicate};
use serde::Deserialize;
use std::rc::Rc;

pub const DEFAULT: &str = include_str!("../keymap/default.json");

#[derive(Deserialize)]
struct Section {
    context: Option<String>,
    bindings: serde_json::Map<String, serde_json::Value>,
}

fn for_platform(keys: &str) -> String {
    keys.split_whitespace()
        .map(|stroke| {
            if cfg!(target_os = "macos") {
                stroke.to_string()
            } else {
                stroke
                    .split('-')
                    .map(|p| if p == "cmd" { "ctrl" } else { p })
                    .collect::<Vec<_>>()
                    .join("-")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub struct Entry {
    pub context: Option<String>,
    pub keys: String,
    pub action: String,
    pub data: Option<serde_json::Value>,
}

pub fn entries(json: &str) -> Result<Vec<Entry>, String> {
    let sections: Vec<Section> = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for s in sections {
        for (keys, action) in s.bindings {
            let (action, data) = match action {
                serde_json::Value::String(a) => (a, None),
                serde_json::Value::Array(mut v) if v.len() == 2 && v[0].is_string() => {
                    let data = v.pop();
                    (v[0].as_str().unwrap_or_default().to_string(), data)
                }
                _ => return Err(format!("binding `{keys}` must name an action")),
            };
            out.push(Entry {
                context: s.context.clone(),
                keys: for_platform(&keys),
                action,
                data,
            });
        }
    }
    Ok(out)
}

fn load(cx: &mut App, es: &[Entry]) -> Result<Vec<KeyBinding>, String> {
    es.iter()
        .map(|e| {
            let action = cx
                .build_action(&e.action, e.data.clone())
                .map_err(|err| format!("{}: {err}", e.action))?;
            let predicate = e
                .context
                .as_deref()
                .map(KeyBindingContextPredicate::parse)
                .transpose()
                .map_err(|err| err.to_string())?
                .map(Rc::new);
            KeyBinding::load(
                &e.keys,
                action,
                predicate,
                false,
                None,
                &gpui::DummyKeyboardMapper,
            )
            .map_err(|err| format!("{}: {err}", e.keys))
        })
        .collect()
}

/// Binds the bundled keymap, then the user's (`user`, if any). Returns a problem with the
/// user's keymap, to show in the status bar; the bundled one ships with the app, so a
/// failure there is a bug.
pub fn bind(cx: &mut App, user: Option<&str>) -> Option<String> {
    let bundled = entries(DEFAULT).expect("bundled keymap parses");
    let bindings = load(cx, &bundled).expect("bundled keymap is valid");
    cx.bind_keys(bindings);
    let user = user?;
    match entries(user).and_then(|es| load(cx, &es)) {
        Ok(b) => {
            cx.bind_keys(b);
            None
        }
        Err(e) => Some(format!("keymap.json not loaded: {e}")),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn bundled_keymap_parses() {
        let es = super::entries(super::DEFAULT).unwrap();
        assert!(es.iter().any(|e| e.action == "jdesk::Refresh"));
        assert!(
            es.iter()
                .any(|e| e.action == "chorong::EnterLayoutMode" && e.keys == "w")
        );
    }
}
