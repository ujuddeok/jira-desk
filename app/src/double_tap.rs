//! Shift pressed twice quickly, on its own: opens the command palette (as in JetBrains IDEs).
//!
//! GPUI keymaps cannot bind a modifier tapped twice, so this watches modifier changes. A tap
//! is Shift pressed and released alone within [`TAP`]; two taps no more than [`GAP`] apart
//! make a double tap. Any other key or modifier in between cancels it, so typing capitals
//! never opens the palette.

use gpui::Modifiers;
use std::time::{Duration, Instant};

/// The longest Shift may be held and still count as a tap.
const TAP: Duration = Duration::from_millis(300);
/// The longest gap between the two taps.
const GAP: Duration = Duration::from_millis(400);

#[derive(Debug, Default)]
pub struct ShiftTaps {
    /// When Shift went down alone, while it is held.
    down: Option<Instant>,
    /// When the last tap ended, while a second one may follow.
    last_tap: Option<Instant>,
}

impl ShiftTaps {
    /// The modifiers changed to `m`. True when this completes a double tap.
    pub fn modifiers(&mut self, m: &Modifiers, now: Instant) -> bool {
        let only_shift = m.shift && !m.control && !m.alt && !m.platform && !m.function;
        if only_shift {
            self.down.get_or_insert(now);
            return false;
        }
        if m.modified() {
            self.key();
            return false;
        }
        let Some(down) = self.down.take() else {
            return false;
        };
        if now.saturating_duration_since(down) > TAP {
            self.last_tap = None;
            return false;
        }
        if self
            .last_tap
            .take()
            .is_some_and(|t| now.saturating_duration_since(t) <= GAP)
        {
            return true;
        }
        self.last_tap = Some(now);
        false
    }

    /// A key was pressed: whatever Shift was doing, it was not a tap.
    pub fn key(&mut self) {
        self.down = None;
        self.last_tap = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shift() -> Modifiers {
        Modifiers {
            shift: true,
            ..Modifiers::default()
        }
    }

    fn at(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    /// Taps Shift down at `from` and up at `to` (milliseconds after `t0`).
    fn tap(s: &mut ShiftTaps, t0: Instant, from: u64, to: u64) -> bool {
        assert!(!s.modifiers(&shift(), at(t0, from)));
        s.modifiers(&Modifiers::default(), at(t0, to))
    }

    #[test]
    fn two_quick_taps_open_the_palette() {
        let (mut s, t0) = (ShiftTaps::default(), Instant::now());
        assert!(!tap(&mut s, t0, 0, 80));
        assert!(tap(&mut s, t0, 200, 280));
        // It starts over afterwards: a third tap alone does nothing.
        assert!(!tap(&mut s, t0, 400, 450));
    }

    #[test]
    fn slow_taps_held_shift_and_typing_do_not() {
        let t0 = Instant::now();
        let mut s = ShiftTaps::default();
        assert!(!tap(&mut s, t0, 0, 80));
        assert!(!tap(&mut s, t0, 700, 780), "too far apart");

        let mut s = ShiftTaps::default();
        assert!(!tap(&mut s, t0, 0, 80));
        assert!(!tap(&mut s, t0, 150, 600), "held too long");

        let mut s = ShiftTaps::default();
        assert!(!tap(&mut s, t0, 0, 80));
        s.key(); // typed a capital
        assert!(!tap(&mut s, t0, 150, 200));

        let mut s = ShiftTaps::default();
        assert!(!tap(&mut s, t0, 0, 80));
        let cmd_shift = Modifiers {
            shift: true,
            platform: true,
            ..Modifiers::default()
        };
        assert!(!s.modifiers(&cmd_shift, at(t0, 120)));
        assert!(!tap(&mut s, t0, 150, 200), "Cmd-Shift in between cancels");
    }
}
