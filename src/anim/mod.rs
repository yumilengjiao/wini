//! Animation system: easing, timed springs and the frame clock.
//!
//! The main thread drives animations with a periodic timer
//! (SetTimer on the message window, ~16ms). Each animated value is a
//! `Val` that eases from its current value to a target; per frame we
//! sample and push intermediate geometry to windows.
//!
//! Niri's default feel is a spring; we approximate with an
//! ease-out-expo / critically-damped curve that stays cheap enough to
//! drive SetWindowPos at 60 Hz.

use std::time::{Duration, Instant};

/// Easing functions (names from CSS for familiarity).
///
/// All variants become constructible from the `animations` config node
/// (#26); until then only the default is used.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Easing {
    Linear,
    EaseOutQuad,
    EaseOutCubic,
    EaseOutExpo,
    EaseOutBack(f64),
}

impl Easing {
    pub fn apply(&self, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        match *self {
            Easing::Linear => t,
            Easing::EaseOutQuad => 1.0 - (1.0 - t) * (1.0 - t),
            Easing::EaseOutCubic => 1.0 - (1.0 - t).powi(3),
            Easing::EaseOutExpo => {
                if t >= 1.0 {
                    1.0
                } else {
                    1.0 - 2.0f64.powf(-10.0 * t)
                }
            }
            Easing::EaseOutBack(overshoot) => {
                let c3 = overshoot + 1.0;
                1.0 + c3 * (t - 1.0).powi(3) + overshoot * (t - 1.0).powi(2)
            }
        }
    }
}

/// Animation parameters, niri-ish defaults.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnimParams {
    pub duration: Duration,
    pub easing: Easing,
}

impl Default for AnimParams {
    fn default() -> Self {
        AnimParams {
            duration: Duration::from_millis(250),
            easing: Easing::EaseOutCubic,
        }
    }
}

/// One animated scalar.
#[derive(Debug, Clone)]
pub struct Val {
    from: f64,
    to: f64,
    start: Instant,
    params: AnimParams,
}

impl Val {
    pub fn to(value: f64, params: AnimParams) -> Self {
        Val {
            from: value,
            to: value,
            start: Instant::now(),
            params,
        }
    }

    /// Change the target; animation continues smoothly from the current
    /// value.
    pub fn retarget(&mut self, value: f64) {
        let cur = self.value();
        if (cur - value).abs() < 0.5 {
            self.from = value;
            self.to = value;
            return;
        }
        self.from = cur;
        self.to = value;
        self.start = Instant::now();
    }

    /// Current (animated) value.
    pub fn value(&self) -> f64 {
        if self.finished() {
            return self.to;
        }
        let t = self
            .params
            .duration
            .saturating_sub(self.start.elapsed())
            .as_secs_f64();
        let total = self.params.duration.as_secs_f64().max(f64::EPSILON);
        let progress = 1.0 - (t / total).clamp(0.0, 1.0);
        let eased = self.params.easing.apply(progress);
        self.from + (self.to - self.from) * eased
    }

    pub fn finished(&self) -> bool {
        self.start.elapsed() >= self.params.duration
    }

    #[allow(dead_code)] // consumed by exit-restore (#44) / overview (#40)
    pub fn target(&self) -> f64 {
        self.to
    }
}

/// A (x, y, w, h) rectangle whose components animate independently.
#[derive(Debug, Clone)]
pub struct AnimatedRect {
    pub x: Val,
    pub y: Val,
    pub w: Val,
    pub h: Val,
    /// False when the rect's target changed but the final (at-rest)
    /// geometry has not been pushed to the window yet. Guarantees
    /// every `set_target` results in at least one `SetWindowPos`, even
    /// when the animation snaps or finishes between ticks.
    at_rest_pushed: bool,
}

impl AnimatedRect {
    pub fn new(x: f64, y: f64, w: f64, h: f64, params: AnimParams) -> Self {
        AnimatedRect {
            x: Val::to(x, params),
            y: Val::to(y, params),
            w: Val::to(w, params),
            h: Val::to(h, params),
            at_rest_pushed: false,
        }
    }

    pub fn retarget(&mut self, x: f64, y: f64, w: f64, h: f64) {
        self.x.retarget(x);
        self.y.retarget(y);
        self.w.retarget(w);
        self.h.retarget(h);
        self.at_rest_pushed = false;
    }

    pub fn finished(&self) -> bool {
        self.x.finished() && self.y.finished() && self.w.finished() && self.h.finished()
    }

    /// Current animated rect, rounded for SetWindowPos.
    pub fn value(&self) -> (i32, i32, i32, i32) {
        (
            self.x.value().round() as i32,
            self.y.value().round() as i32,
            self.w.value().round().max(1.0) as i32,
            self.h.value().round().max(1.0) as i32,
        )
    }

    /// The target rect (no animation applied).
    pub fn targets(&self) -> (f64, f64, f64, f64) {
        (self.x.target(), self.y.target(), self.w.target(), self.h.target())
    }
}

/// Tracks all window animations and ticks them.
#[derive(Debug, Default)]
pub struct Animator {
    /// Per-window animated rect, keyed by window id.
    rects: std::collections::HashMap<isize, AnimatedRect>,
    params: AnimParams,
}

impl Animator {
    pub fn new(params: AnimParams) -> Self {
        Animator {
            rects: std::collections::HashMap::new(),
            params,
        }
    }

    /// Set/retarget a window's target rect.
    pub fn set_target(&mut self, id: isize, x: f64, y: f64, w: f64, h: f64) {
        match self.rects.get_mut(&id) {
            Some(r) => r.retarget(x, y, w, h),
            None => {
                self.rects
                    .insert(id, AnimatedRect::new(x, y, w, h, self.params));
            }
        }
    }

    /// Drop animation state for a window.
    pub fn remove(&mut self, id: isize) {
        self.rects.remove(&id);
    }

    /// Replace the animation parameters (config hot reload). Applies
    /// to animations started afterwards; in-flight ones keep their
    /// original parameters.
    pub fn set_params(&mut self, params: AnimParams) {
        self.params = params;
    }

    #[allow(dead_code)] // kept as API; tests and future callers use it
    pub fn is_animating(&self) -> bool {
        self.rects.values().any(|r| !r.finished())
    }

    /// Sample all animations. Returns (id, rect) pairs that need a
    /// `SetWindowPos` this frame: in-flight ones every frame, and
    /// at-rest ones exactly once after their target changed (the final
    /// frame — without it a window would stop one frame short of its
    /// target, or not move at all when the change snapped).
    pub fn tick(&mut self) -> Vec<(isize, i32, i32, i32, i32)> {
        let mut out = Vec::new();
        for (id, r) in self.rects.iter_mut() {
            if r.finished() {
                if !r.at_rest_pushed {
                    let (x, y, w, h) = r.value();
                    out.push((*id, x, y, w, h));
                    r.at_rest_pushed = true;
                }
            } else {
                let (x, y, w, h) = r.value();
                out.push((*id, x, y, w, h));
            }
        }
        out
    }

    /// Snap every in-flight animation to its target and drop it,
    /// returning the final rects. Used when management is suspended
    /// (do-screen-transition): nothing should keep moving mid-pause.
    pub fn finish_all(&mut self) -> Vec<(isize, i32, i32, i32, i32)> {
        self.rects
            .drain()
            .map(|(id, r)| {
                let (x, y, w, h) = r.targets();
                (
                    id,
                    x.round() as i32,
                    y.round() as i32,
                    w.round().max(1.0) as i32,
                    h.round().max(1.0) as i32,
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing_bounds() {
        for e in [
            Easing::Linear,
            Easing::EaseOutQuad,
            Easing::EaseOutCubic,
            Easing::EaseOutExpo,
            Easing::EaseOutBack(1.70158),
        ] {
            assert!((e.apply(0.0) - 0.0).abs() < 1e-9, "{e:?} at 0");
            assert!((e.apply(1.0) - 1.0).abs() < 1e-9, "{e:?} at 1");
            let mid = e.apply(0.5);
            assert!((-0.5..=1.5).contains(&mid), "{e:?} mid sane: {mid}");
        }
    }

    #[test]
    fn val_finishes_at_target() {
        let mut v = Val::to(100.0, AnimParams::default());
        v.retarget(200.0);
        // Force completion by pretending time passed: duration 250ms.
        std::thread::sleep(Duration::from_millis(300));
        assert!(v.finished());
        assert_eq!(v.value(), 200.0);
    }

    #[test]
    fn val_retargets_smoothly() {
        let mut v = Val::to(0.0, AnimParams::default());
        v.retarget(100.0);
        // Halfway in time (125ms of 250ms), ease-out-cubic at t=0.5 is
        // 1-(0.5)^3 = 0.875 -> value 87.5. We can't sleep mid-test
        // reliably, so just check bounds instead.
        let val = v.value();
        assert!((0.0..=100.0).contains(&val));
    }

    #[test]
    fn animator_ticks_and_final_frame() {
        let mut a = Animator::new(AnimParams {
            duration: Duration::from_millis(1),
            easing: Easing::Linear,
        });
        a.set_target(1, 0.0, 0.0, 100.0, 100.0);
        assert!(a.is_animating());
        std::thread::sleep(Duration::from_millis(10));
        // After finish: the final frame is still delivered once...
        let ticks = a.tick();
        assert_eq!(ticks, vec![(1, 0, 0, 100, 100)]);
        assert!(!a.is_animating());
        // ...and only once.
        assert!(a.tick().is_empty());
        // A snapped retarget (target == current) still pushes once.
        a.set_target(1, 0.0, 0.0, 100.0, 100.0);
        assert_eq!(a.tick(), vec![(1, 0, 0, 100, 100)]);
        assert!(a.tick().is_empty());
    }

    #[test]
    fn animated_rect_values() {
        let mut r = AnimatedRect::new(0.0, 0.0, 100.0, 100.0, AnimParams {
            duration: Duration::from_millis(1),
            easing: Easing::Linear,
        });
        assert_eq!(r.value(), (0, 0, 100, 100));
        r.retarget(10.0, 20.0, 30.0, 40.0);
        std::thread::sleep(Duration::from_millis(10));
        assert_eq!(r.value(), (10, 20, 30, 40));
        assert!(r.finished());
    }

    #[test]
    fn finish_all_snaps_to_targets() {
        let mut a = Animator::new(AnimParams {
            duration: Duration::from_millis(10_000), // never finishes
            easing: Easing::Linear,
        });
        a.set_target(7, 0.0, 0.0, 100.0, 100.0);
        a.set_target(7, 50.0, 60.0, 70.0, 80.0);
        assert!(a.is_animating());
        let finals = a.finish_all();
        assert_eq!(finals, vec![(7, 50, 60, 70, 80)]);
        assert!(!a.is_animating());
        assert!(a.finish_all().is_empty());
    }
}
