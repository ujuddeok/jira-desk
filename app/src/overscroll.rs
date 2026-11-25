//! From Chorong's app (app/src/overscroll.rs), without its settings probe.
//!
//! Rubber-band overscroll for scrolling lists, as on macOS: a trackpad scroll that runs past
//! an end stretches the list with growing resistance, and it springs back once the fingers
//! lift and the momentum dies away.
//!
//! GPUI clamps a list's offset at its ends, so a flick in a short pane stops dead. An
//! [`Overscroll`] sits beside a list's scroll handle, and [`Overscroll::wrap`] puts the list
//! in a clipping box with a listener painted before it (filling the box): the listener sees each
//! scroll event first (capture phase) and takes the part that would run past an end, and
//! the list is drawn moved by the stretch. Only pixel (trackpad) deltas stretch; a mouse wheel scrolls in
//! lines and is left alone. [`Band`] is the physics, kept free of GPUI so it can be tested.

use gpui::{
    Bounds, DispatchPhase, Div, IntoElement, ListState, ParentElement, Pixels, ScrollDelta,
    ScrollHandle, ScrollWheelEvent, Styled, TouchPhase, UniformListScrollHandle, Window, canvas,
    div, px,
};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

thread_local! {
    /// The overscroll areas laid out most recently, in layout order. An area nested in another
    /// (a font list in the settings page) is laid out after it, so among the areas under the
    /// pointer the last one here is the innermost.
    static AREAS: RefCell<VecDeque<gpui::HitboxId>> = const { RefCell::new(VecDeque::new()) };
}

/// Enough for every area of a few frames; older entries are no longer hovered anyway.
const AREAS_KEPT: usize = 256;

fn laid_out(id: gpui::HitboxId) {
    AREAS.with(|a| {
        let mut a = a.borrow_mut();
        a.push_back(id);
        while a.len() > AREAS_KEPT {
            a.pop_front();
        }
    });
}

/// Whether `id` is the innermost overscroll area under the pointer: a scroll there is that
/// area's, not the one around it (which would otherwise take it first, in the capture phase).
fn innermost(id: gpui::HitboxId, window: &Window) -> bool {
    AREAS.with(|a| a.borrow().iter().rev().find(|h| h.is_hovered(window)) == Some(&id))
}

/// How hard the stretch resists: the drawn offset approaches the pane height as the pull
/// grows, at this rate (Apple's constant).
const RESISTANCE: f32 = 0.55;
/// The spring's natural frequency (rad/s). Critically damped, it settles a released stretch
/// in about 0.35 s, and a flick hitting an end at speed `v` overshoots by `v / (OMEGA * e)`
/// before the resistance: about 30 px drawn for a fast flick on a 400 px pane.
const OMEGA: f32 = 14.;
/// The fastest a flick may hit an end, in px/s, so a burst of events cannot fling the list.
const MAX_SPEED: f32 = 3000.;
/// A touch with no events for this long counts as lifted (a lost Ended event); momentum
/// with a gap this long has ended, so the next flick may bounce again.
const STALE_TOUCH: Duration = Duration::from_millis(300);

/// The stretch state of one list.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Band {
    /// The pull past an end before resistance, in pixels: positive past the top (the
    /// content moves down), negative past the bottom.
    pub raw: f32,
    /// How fast `raw` is changing, in px/s, while the spring runs.
    vel: f32,
    pub touching: bool,
    /// Momentum hit an end and bounced: the rest of that momentum is swallowed (the bounce
    /// already carries its speed) until the fingers come back or the momentum stops.
    absorbing: bool,
    last_event: Option<Instant>,
}

/// What a scroll event does to the list itself.
#[derive(Debug, PartialEq)]
pub enum Take {
    /// Not at an end: the list scrolls as usual.
    Pass,
    /// The band took the event; the list moves to this scroll top (if any) and must not
    /// see the event.
    Consume(Option<f32>),
}

impl Band {
    /// A vertical scroll of `dy` (GPUI's sign: positive moves the content down) on a list
    /// scrolled `top` pixels of `max`.
    pub fn scroll(&mut self, top: f32, max: f32, dy: f32, phase: TouchPhase, now: Instant) -> Take {
        let gap = self.last_event.map(|t| now.saturating_duration_since(t));
        self.last_event = Some(now);
        if gap.is_none_or(|g| g > STALE_TOUCH) {
            self.absorbing = false;
        }
        match phase {
            TouchPhase::Started => {
                self.touching = true;
                self.absorbing = false;
                self.vel = 0.;
            }
            TouchPhase::Ended => self.touching = false,
            TouchPhase::Moved => {}
        }
        if dy == 0. {
            return if self.raw == 0. && !self.absorbing {
                Take::Pass
            } else {
                Take::Consume(None)
            };
        }
        if !self.touching {
            return self.momentum(top, max, dy, gap);
        }
        if self.raw != 0. {
            let next = self.raw + dy;
            if next == 0. || next.signum() == self.raw.signum() {
                self.raw = next;
                return Take::Consume(None);
            }
            // Pulled back through the end: the rest scrolls the list from that end.
            let edge = if self.raw > 0. { 0. } else { max };
            self.raw = 0.;
            return match self.stretch_from(edge, max, next) {
                Take::Pass => Take::Consume(Some((edge - next).clamp(0., max))),
                consumed => consumed,
            };
        }
        self.stretch_from(top, max, dy)
    }

    /// Momentum (no fingers): it scrolls the list until it runs into an end, where its speed
    /// becomes the spring's, and the rest of it is swallowed.
    fn momentum(&mut self, top: f32, max: f32, dy: f32, gap: Option<Duration>) -> Take {
        if self.absorbing || self.raw != 0. {
            self.absorbing = true;
            return Take::Consume(None);
        }
        let target = top - dy;
        if (0. ..=max).contains(&target) {
            return Take::Pass;
        }
        // Event spacing gives the speed; an odd gap counts as one 60 Hz frame.
        let dt = gap
            .map(|g| g.as_secs_f32())
            .filter(|d| (0.004..0.05).contains(d))
            .unwrap_or(1. / 60.);
        self.vel = (dy / dt).clamp(-MAX_SPEED, MAX_SPEED);
        self.absorbing = true;
        Take::Consume(Some(target.clamp(0., max)))
    }

    fn stretch_from(&mut self, top: f32, max: f32, dy: f32) -> Take {
        let target = top - dy;
        if target < 0. {
            self.raw = -target;
            Take::Consume(Some(0.))
        } else if target > max {
            self.raw = max - target;
            Take::Consume(Some(max))
        } else {
            Take::Pass
        }
    }

    /// Whether the spring is running (nothing is holding the stretch).
    pub fn released(&self, now: Instant) -> bool {
        !self.touching
            || self
                .last_event
                .is_none_or(|t| now.saturating_duration_since(t) > STALE_TOUCH)
    }

    /// Advances the spring by `dt` seconds. Returns whether it is still moving.
    pub fn step(&mut self, dt: f32, now: Instant) -> bool {
        if self.raw == 0. && self.vel == 0. {
            return false;
        }
        if !self.released(now) {
            return true;
        }
        self.touching = false;
        // Critically damped spring, in small steps so a slow frame stays stable.
        let steps = (dt / 0.004).ceil().max(1.);
        let h = dt / steps;
        for _ in 0..steps as usize {
            let acc = -OMEGA * OMEGA * self.raw - 2. * OMEGA * self.vel;
            self.vel += acc * h;
            self.raw += self.vel * h;
        }
        if self.raw.abs() < 0.5 && self.vel.abs() < 20. {
            self.raw = 0.;
            self.vel = 0.;
        }
        self.raw != 0. || self.vel != 0.
    }

    /// The offset to draw the list at, in a pane `height` pixels tall.
    pub fn offset(&self, height: f32) -> f32 {
        if self.raw == 0. || height <= 0. {
            return 0.;
        }
        let d = self.raw.abs();
        self.raw.signum() * height * (1. - 1. / (d * RESISTANCE / height + 1.))
    }
}

/// The scroll state of a list that can stretch.
#[derive(Clone, Copy)]
enum Source<'a> {
    Uniform(&'a UniformListScrollHandle),
    List(&'a ListState),
    Div(&'a ScrollHandle),
}

/// A list's scroll handle, for [`Overscroll::listener`].
#[derive(Clone)]
pub enum Scroller {
    Uniform(UniformListScrollHandle),
    List(ListState),
    Div(ScrollHandle),
}

impl From<UniformListScrollHandle> for Scroller {
    fn from(h: UniformListScrollHandle) -> Self {
        Scroller::Uniform(h)
    }
}

impl From<ListState> for Scroller {
    fn from(s: ListState) -> Self {
        Scroller::List(s)
    }
}

impl From<ScrollHandle> for Scroller {
    fn from(h: ScrollHandle) -> Self {
        Scroller::Div(h)
    }
}

impl Scroller {
    fn source(&self) -> Source<'_> {
        match self {
            Scroller::Uniform(h) => Source::Uniform(h),
            Scroller::List(s) => Source::List(s),
            Scroller::Div(h) => Source::Div(h),
        }
    }

    /// The scroll top and the largest it can be, in pixels.
    fn position(&self) -> (f32, f32) {
        let (offset, max) = match self.source() {
            Source::Uniform(h) => {
                let base = h.0.borrow().base_handle.clone();
                (base.offset().y, base.max_offset().height)
            }
            Source::List(s) => (
                s.scroll_px_offset_for_scrollbar().y,
                s.max_offset_for_scrollbar().height,
            ),
            Source::Div(h) => (h.offset().y, h.max_offset().height),
        };
        let max = f32::from(max).max(0.);
        ((-f32::from(offset)).clamp(0., max), max)
    }

    fn set_top(&self, top: f32) {
        match self.source() {
            Source::Uniform(h) => {
                let base = h.0.borrow().base_handle.clone();
                let x = base.offset().x;
                base.set_offset(gpui::point(x, px(-top)));
            }
            Source::List(s) => {
                let (now, _) = self.position();
                s.scroll_by(px(top - now));
            }
            Source::Div(h) => {
                let x = h.offset().x;
                h.set_offset(gpui::point(x, px(-top)));
            }
        }
    }
}

#[derive(Default)]
struct State {
    band: Band,
    height: f32,
    last_step: Option<Instant>,
}

/// Whether a list stretches at its ends (`overscroll` in config.toml).
#[derive(Clone, Copy)]
pub enum Stretch {
    Off,
    On,
}

impl From<bool> for Stretch {
    fn from(on: bool) -> Self {
        if on { Stretch::On } else { Stretch::Off }
    }
}

/// One list's overscroll. Clones share it.
#[derive(Clone, Default)]
pub struct Overscroll(Rc<RefCell<State>>);

impl Overscroll {
    /// How far the list is drawn from where it lies.
    fn offset(&self) -> Pixels {
        let s = self.0.borrow();
        px(s.band.offset(s.height))
    }

    /// `list` (which scrolls with `scroller`) in a box that clips it, stretching at its ends.
    /// Size the box as the list was sized; the list fills it. With [`Stretch::Off`] (the setting is
    /// off, or Lightweight mode) the list rests in place.
    pub fn wrap(
        &self,
        scroller: impl Into<Scroller>,
        stretch: impl Into<Stretch>,
        list: impl IntoElement + Styled,
    ) -> Div {
        div()
            .relative()
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(self.listener(scroller, stretch.into()))
            .child(
                list.flex_1()
                    .min_h_0()
                    .w_full()
                    .relative()
                    .top(self.offset()),
            )
    }

    /// An element filling the box that sees scroll events before the list painted after it.
    fn listener(&self, scroller: impl Into<Scroller>, stretch: Stretch) -> impl IntoElement {
        let scroller = scroller.into();
        let state = self.0.clone();
        if matches!(stretch, Stretch::Off) {
            *state.borrow_mut() = State::default();
        }
        // It fills the box, which stays put while the list inside it is drawn shifted: the
        // pane's own bounds decide which scrolls are this list's. Its hitbox makes a modal
        // card on top (a picker and its preview, a dialog) take those scrolls instead: the
        // listener runs before the card's own, so it must ask whether the pane is hovered.
        canvas(
            |bounds, window, _| {
                let hitbox = window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal);
                laid_out(hitbox.id);
                hitbox
            },
            move |pane, hitbox, window, _| {
                let Stretch::On = stretch else { return };
                paint(state, scroller, pane, hitbox, window);
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }
}

fn paint(
    state: Rc<RefCell<State>>,
    scroller: Scroller,
    pane: Bounds<Pixels>,
    hitbox: gpui::Hitbox,
    window: &mut Window,
) {
    {
        let now = Instant::now();
        let mut s = state.borrow_mut();
        s.height = f32::from(pane.size.height);
        let dt = s.last_step.map_or(0., |t| {
            now.saturating_duration_since(t).as_secs_f32().min(0.05)
        });
        if s.band.step(dt, now) {
            s.last_step = Some(now);
            window.request_animation_frame();
        } else {
            s.last_step = None;
        }
    }
    window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
        if phase != DispatchPhase::Capture {
            return;
        }
        let ScrollDelta::Pixels(delta) = event.delta else {
            return;
        };
        // Under something that blocks the pointer (a picker, a dialog), the scroll is not ours;
        // nor over a scroll area nested in this one (a list inside a page): that one takes it.
        if !pane.contains(&event.position)
            || !hitbox.is_hovered(window)
            || !innermost(hitbox.id, window)
        {
            return;
        }
        let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
        let mut s = state.borrow_mut();
        // A sideways scroll is the list's own, unless it lands while stretched.
        if dx.abs() > dy.abs() && s.band.raw == 0. {
            return;
        }
        let (top, max) = scroller.position();
        match s
            .band
            .scroll(top, max, dy, event.touch_phase, Instant::now())
        {
            Take::Pass => {}
            Take::Consume(to) => {
                if let Some(to) = to {
                    scroller.set_top(to);
                }
                cx.stop_propagation();
                // A redraw starts the spring: a flick that hits an end has its speed but no
                // stretch yet, so nothing else would ask for a frame.
                window.refresh();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A band with fingers on the trackpad.
    fn finger() -> Band {
        Band {
            touching: true,
            ..Band::default()
        }
    }

    fn moved(b: &mut Band, top: f32, max: f32, dy: f32) -> Take {
        b.scroll(top, max, dy, TouchPhase::Moved, Instant::now())
    }

    #[test]
    fn mid_list_scrolls_pass_through() {
        let mut b = Band::default();
        assert_eq!(moved(&mut b, 100., 500., 30.), Take::Pass);
        assert_eq!(moved(&mut b, 100., 500., -30.), Take::Pass);
        assert_eq!(b.raw, 0.);
    }

    #[test]
    fn past_the_top_pins_the_list_and_stretches() {
        let mut b = finger();
        // 10 px from the top, a 30 px pull up: 10 scrolls, 20 stretches.
        assert_eq!(moved(&mut b, 10., 500., 30.), Take::Consume(Some(0.)));
        assert_eq!(b.raw, 20.);
        let o = b.offset(400.);
        assert!(o > 0. && o < 20., "resisted: {o}");
    }

    #[test]
    fn past_the_bottom_stretches_the_other_way() {
        let mut b = finger();
        assert_eq!(moved(&mut b, 490., 500., -30.), Take::Consume(Some(500.)));
        assert_eq!(b.raw, -20.);
        assert!(b.offset(400.) < 0.);
    }

    #[test]
    fn a_short_list_still_stretches() {
        let mut b = finger();
        assert_eq!(moved(&mut b, 0., 0., -15.), Take::Consume(Some(0.)));
        assert_eq!(b.raw, -15.);
    }

    #[test]
    fn pulling_back_through_the_end_scrolls_the_rest() {
        let mut b = Band {
            raw: 20.,
            ..finger()
        };
        assert_eq!(moved(&mut b, 0., 500., -50.), Take::Consume(Some(30.)));
        assert_eq!(b.raw, 0.);
    }

    #[test]
    fn resistance_never_reaches_the_pane_height() {
        let b = Band {
            raw: 1e6,
            ..Band::default()
        };
        let o = b.offset(300.);
        assert!(o < 300. && o > 290., "{o}");
    }

    #[test]
    fn spring_waits_for_the_fingers_then_settles() {
        let now = Instant::now();
        let mut b = Band::default();
        b.scroll(0., 500., 0., TouchPhase::Started, now);
        b.scroll(0., 500., 40., TouchPhase::Moved, now);
        assert!(b.step(0.016, now) && b.raw == 40., "held while touching");
        b.scroll(0., 500., 0., TouchPhase::Ended, now);
        let mut frames = 0;
        while b.step(0.016, now) {
            frames += 1;
            assert!(frames < 120, "settles within two seconds");
        }
        assert_eq!(b.raw, 0.);
        assert!(frames > 10, "animates rather than snapping: {frames}");
    }

    #[test]
    fn a_lost_ended_event_still_springs_back() {
        let then = Instant::now();
        let mut b = Band::default();
        b.scroll(0., 500., 0., TouchPhase::Started, then);
        b.scroll(0., 500., 40., TouchPhase::Moved, then);
        let later = then + STALE_TOUCH + Duration::from_millis(1);
        assert!(b.step(0.016, later) && b.raw < 40.);
    }

    #[test]
    fn a_flick_into_an_end_bounces_by_its_speed_and_settles() {
        let t0 = Instant::now();
        let mut b = Band::default();
        // Momentum 10 px from the top, 40 px per 16 ms frame (2,500 px/s).
        b.scroll(10., 500., 0., TouchPhase::Moved, t0);
        let t1 = t0 + Duration::from_millis(16);
        assert_eq!(
            b.scroll(10., 500., 40., TouchPhase::Moved, t1),
            Take::Consume(Some(0.))
        );
        // The rest of the momentum is swallowed, not added.
        for i in 2..30 {
            let t = t0 + Duration::from_millis(16 * i);
            assert_eq!(
                b.scroll(0., 500., 30., TouchPhase::Moved, t),
                Take::Consume(None)
            );
        }
        let (mut peak, mut frames) = (0f32, 0);
        while b.step(1. / 60., t1) {
            peak = peak.max(b.raw);
            frames += 1;
            assert!(frames < 120, "settles within two seconds");
        }
        let drawn = Band {
            raw: peak,
            ..Band::default()
        }
        .offset(400.);
        assert!(
            (20. ..45.).contains(&drawn),
            "a small bounce: {drawn} px (raw {peak})"
        );
        assert!(frames > 10, "animates: {frames} frames");
        assert_eq!(b.raw, 0.);
    }

    #[test]
    fn the_next_gesture_can_bounce_again() {
        let t0 = Instant::now();
        let mut b = Band::default();
        b.scroll(0., 500., 40., TouchPhase::Moved, t0);
        while b.step(1. / 60., t0) {}
        let later = t0 + STALE_TOUCH + Duration::from_millis(1);
        assert_eq!(
            b.scroll(0., 500., 40., TouchPhase::Moved, later),
            Take::Consume(Some(0.))
        );
        assert!(b.vel > 0.);
    }

    #[test]
    fn a_finger_is_not_capped() {
        let mut b = Band::default();
        b.scroll(0., 500., 0., TouchPhase::Started, Instant::now());
        moved(&mut b, 0., 500., 300.);
        assert_eq!(b.raw, 300.);
    }
}
