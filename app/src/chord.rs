//! The chord hint: while a chord is half typed (⌘K, `f`, …) a card lists the keys that can
//! follow it and what each does, read from the live keymap, so a user's `keymap.json` shows
//! too. GPUI drops a half-typed chord after a fixed second; when that happens the chord is
//! held open instead, card and all, until the next key (as in Chorong).

use gpui::{Action, App, KeyContext, Keystroke, Window};
use std::time::{Duration, Instant};

/// A chord dropped at least this long after it started was dropped by GPUI's one-second
/// timeout, not by a key.
const HOLD_AFTER: Duration = Duration::from_millis(500);

/// The key context while a held chord waits for its key: nothing is bound in it, so every key
/// reaches the view's key-down listener rather than a single-key binding.
pub const HELD_CONTEXT: &str = "JiraChord";

#[derive(Default)]
pub struct Chord {
    /// The chord in progress, when it started and the key contexts it was typed in.
    started: Option<(Vec<Keystroke>, Instant, Vec<KeyContext>)>,
    /// A chord GPUI timed out: held open until the next key.
    held: Option<(Vec<Keystroke>, Vec<KeyContext>)>,
}

impl Chord {
    /// GPUI reported a change in the chord in progress.
    pub fn pending_changed(&mut self, window: &Window) {
        match window.pending_input_keystrokes().filter(|p| !p.is_empty()) {
            Some(p) => {
                self.started = Some((p.to_vec(), Instant::now(), window.context_stack()));
                self.held = None;
            }
            None => {
                // A key that answers a chord clears `started` (keystroke observer) before this
                // runs, so a chord still recorded here ran out of time.
                if let Some((keys, at, ctx)) = self.started.take()
                    && at.elapsed() >= HOLD_AFTER
                {
                    self.held = Some((keys, ctx));
                }
            }
        }
    }

    /// A key was handled (or abandoned the chord). Returns whether a held chord was closed.
    pub fn key_observed(&mut self, k: &Keystroke) -> bool {
        if self.is_replay(k) {
            return false;
        }
        self.started = None;
        self.held.take().is_some()
    }

    /// When GPUI times a chord out it replays the chord's own keys; those don't finish it.
    pub fn is_replay(&self, k: &Keystroke) -> bool {
        self.held.as_ref().is_some_and(|(keys, _)| {
            keys.iter()
                .any(|h| h.key == k.key && h.modifiers == k.modifiers)
        })
    }

    pub fn is_held(&self) -> bool {
        self.held.is_some()
    }

    pub fn clear(&mut self) -> bool {
        self.started = None;
        self.held.take().is_some()
    }

    /// The next key after a held chord: the action the whole sequence is bound to, if any.
    /// The chord is closed either way.
    pub fn finish(&mut self, k: &Keystroke, cx: &App) -> Option<Box<dyn Action>> {
        let (mut keys, ctx) = self.held.take()?;
        keys.push(k.clone());
        action_for(&keys, &ctx, cx)
    }

    /// What the card shows: the chord so far, and the contexts to read its followers in.
    pub fn showing(&self, window: &Window) -> Option<(Vec<Keystroke>, Vec<KeyContext>)> {
        if let Some(p) = window.pending_input_keystrokes().filter(|p| !p.is_empty()) {
            let ctx = self
                .started
                .as_ref()
                .map(|(_, _, c)| c.clone())
                .unwrap_or_else(|| window.context_stack());
            return Some((p.to_vec(), ctx));
        }
        self.held.clone()
    }
}

/// Whether binding `b` applies in the key contexts `ctx`.
fn applies(b: &gpui::KeyBinding, ctx: &[KeyContext]) -> bool {
    b.predicate().is_none_or(|p| p.depth_of(ctx).is_some())
}

fn same(p: &Keystroke, s: &Keystroke) -> bool {
    p.key == s.key && p.modifiers == s.modifiers
}

/// The action bound to the whole key sequence `keys` in `ctx` (the latest binding wins, as in
/// GPUI).
fn action_for(keys: &[Keystroke], ctx: &[KeyContext], cx: &App) -> Option<Box<dyn Action>> {
    let bindings = cx.key_bindings();
    let bindings = bindings.borrow();
    bindings
        .bindings()
        .rev()
        .find(|b| b.match_keystrokes(keys) == Some(false) && applies(b, ctx))
        .map(|b| b.action().boxed_clone())
}

/// The keys that can follow `pending` in `ctx`, each with the action it runs. A later binding
/// of the same key wins.
pub fn followers(
    pending: &[Keystroke],
    ctx: &[KeyContext],
    cx: &App,
) -> Vec<(Keystroke, Box<dyn Action>)> {
    let mut out: Vec<(Keystroke, Box<dyn Action>)> = Vec::new();
    let bindings = cx.key_bindings();
    let bindings = bindings.borrow();
    for b in bindings.bindings().rev() {
        let strokes = b.keystrokes();
        if strokes.len() != pending.len() + 1 || !applies(b, ctx) {
            continue;
        }
        if !pending.iter().zip(strokes).all(|(p, s)| same(p, s.inner())) {
            continue;
        }
        let next = strokes[pending.len()].inner().clone();
        if b.action().name() == "zed::NoAction" || out.iter().any(|(k, _)| same(k, &next)) {
            continue;
        }
        out.push((next, b.action().boxed_clone()));
    }
    out.reverse();
    out
}

/// A keystroke as the app shows keys: `⌘K`, `⇧N`, `→`, a bare letter as typed.
pub fn stroke_label(k: &Keystroke) -> String {
    let key = match k.key.as_str() {
        "escape" => "Esc".to_string(),
        "enter" => "Enter".to_string(),
        "space" => "Space".to_string(),
        "tab" => "Tab".to_string(),
        "backspace" => "⌫".to_string(),
        "left" => "←".to_string(),
        "right" => "→".to_string(),
        "up" => "↑".to_string(),
        "down" => "↓".to_string(),
        s => s.to_uppercase(),
    };
    let m = &k.modifiers;
    let key = if !(m.control || m.alt || m.shift || m.platform) && k.key.chars().count() == 1 {
        k.key.clone()
    } else {
        key
    };
    if cfg!(target_os = "macos") {
        format!(
            "{}{}{}{}{key}",
            if m.control { "⌃" } else { "" },
            if m.alt { "⌥" } else { "" },
            if m.shift { "⇧" } else { "" },
            if m.platform { "⌘" } else { "" },
        )
    } else {
        let mut parts: Vec<&str> = Vec::new();
        if m.control {
            parts.push("Ctrl");
        }
        if m.alt {
            parts.push("Alt");
        }
        if m.shift {
            parts.push("Shift");
        }
        if m.platform {
            parts.push("Super");
        }
        parts.push(&key);
        parts.join("-")
    }
}

/// `chorong::ToggleMaximizePane` as `Toggle maximize pane`, for an action with no title.
pub fn action_words(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.rsplit("::").next().unwrap_or(name).chars().enumerate() {
        if i > 0 && c.is_uppercase() {
            out.push(' ');
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Rows for the card: keys with the same title share a row (`⌘K 1…9` reads `1–9`).
pub fn rows(followers: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut out: Vec<(Vec<String>, String)> = Vec::new();
    for (key, title) in followers {
        match out.iter_mut().find(|(_, t)| *t == title) {
            Some((keys, _)) => keys.push(key),
            None => out.push((vec![key], title)),
        }
    }
    out.into_iter()
        .map(|(keys, title)| (join_keys(&keys), title))
        .collect()
}

fn join_keys(keys: &[String]) -> String {
    let digits: Option<Vec<u32>> = keys
        .iter()
        .map(|k| {
            let mut c = k.chars();
            match (c.next(), c.next()) {
                (Some(d), None) => d.to_digit(10),
                _ => None,
            }
        })
        .collect();
    if let Some(d) = digits
        && d.len() > 2
        && d.windows(2).all(|w| w[1] == w[0] + 1)
    {
        return format!("{}–{}", d[0], d[d.len() - 1]);
    }
    keys.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_from_an_action_name() {
        assert_eq!(action_words("chorong::SplitPaneRight"), "Split pane right");
    }

    #[test]
    fn rows_merge_a_digit_run() {
        let f = (1..=9)
            .map(|d| (d.to_string(), "Go to page".to_string()))
            .chain([("→".to_string(), "Split right".to_string())])
            .collect();
        assert_eq!(
            rows(f),
            vec![
                ("1–9".to_string(), "Go to page".to_string()),
                ("→".to_string(), "Split right".to_string())
            ]
        );
    }

    #[test]
    fn rows_keep_other_shared_titles_as_keys() {
        let f = vec![
            ("u".to_string(), "Undo".to_string()),
            ("z".to_string(), "Undo".to_string()),
        ];
        assert_eq!(rows(f), vec![("u z".to_string(), "Undo".to_string())]);
    }

    #[test]
    fn labels_read_like_the_palette() {
        let k = Keystroke::parse("cmd-k").unwrap();
        let l = stroke_label(&k);
        if cfg!(target_os = "macos") {
            assert_eq!(l, "⌘K");
        }
        assert_eq!(stroke_label(&Keystroke::parse("f").unwrap()), "f");
    }
}
