//! Animation system: springs, easing curves and the frame clock.
//!
//! The main thread drives animations with a periodic high-resolution
//! timer (~16ms). Each animated value is a `Val` that eases or springs
//! from its current value to a target; per frame we sample and push
//! intermediate geometry to windows.
//!
//! Springs (niri's default for window movement and resizing) integrate
//! the damped harmonic oscillator in closed form. Their defining
//! quality is *velocity carry-over*: when the target changes mid-flight
//! the spring continues from its current position AND velocity, so
//! rapid retargeting never jumps — the "niri feel".
//!
//! Easing curves (niri's alternative) map linear time to a curve like
//! ease-out-expo — fast at first, settling towards the end.

use std::time::{Duration, Instant};

/// Easing curves (niri's `easing` kinds, names shared with CSS).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Curve {
    Linear,
    EaseOutQuad,
    EaseOutCubic,
    EaseOutExpo,
    EaseOutBack(f64),
    /// `cubic-bezier(x1, y1, x2, y2)`: two control points, CSS-style.
    /// x coordinates must be within [0, 1] (they map to time).
    CubicBezier(f64, f64, f64, f64),
}

impl Curve {
    pub fn apply(&self, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        match *self {
            Curve::Linear => t,
            Curve::EaseOutQuad => 1.0 - (1.0 - t) * (1.0 - t),
            Curve::EaseOutCubic => 1.0 - (1.0 - t).powi(3),
            Curve::EaseOutExpo => {
                if t >= 1.0 {
                    1.0
                } else {
                    1.0 - 2.0f64.powf(-10.0 * t)
                }
            }
            Curve::EaseOutBack(overshoot) => {
                let c3 = overshoot + 1.0;
                1.0 + c3 * (t - 1.0).powi(3) + overshoot * (t - 1.0).powi(2)
            }
            Curve::CubicBezier(x1, y1, x2, y2) => cubic_bezier(t, x1, y1, x2, y2),
        }
    }
}

/// Solve a CSS-style cubic Bézier easing curve at progress `t` (time
/// fraction). The X axis (time) is monotonic, so Newton's method with
/// a bisection fallback finds the parameter whose x equals `t`, then
/// the curve's y is the eased progress.
fn cubic_bezier(t: f64, x1: f64, y1: f64, x2: f64, y2: f64) -> f64 {
    if t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }
    // Bézier derivative coefficients (with respect to the parameter u).
    let dx = |u: f64| {
        3.0 * (1.0 - u) * (1.0 - u) * x1
            + 6.0 * (1.0 - u) * u * (x2 - x1)
            + 3.0 * u * u * (1.0 - x2)
    };
    let x = |u: f64| {
        let a = 1.0 - u;
        3.0 * a * a * u * x1 + 3.0 * a * u * u * x2 + u * u * u
    };
    let y = |u: f64| {
        let a = 1.0 - u;
        3.0 * a * a * u * y1 + 3.0 * a * u * u * y2 + u * u * u
    };
    // Newton iterations from a good initial guess.
    let mut u = t;
    for _ in 0..8 {
        let err = x(u) - t;
        if err.abs() < 1e-7 {
            return y(u);
        }
        let d = dx(u);
        if d.abs() < 1e-7 {
            break;
        }
        u -= err / d;
        if !(0.0..=1.0).contains(&u) {
            break;
        }
    }
    // Bisection fallback for degenerate slopes.
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for _ in 0..32 {
        u = (lo + hi) / 2.0;
        if x(u) < t {
            lo = u;
        } else {
            hi = u;
        }
    }
    y((lo + hi) / 2.0)
}

/// Spring parameters, niri-compatible:
/// `spring { damping-ratio 1.0; stiffness 800; epsilon 0.0001; }`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpringParams {
    /// 1.0 = critically damped (settles without oscillating, niri's
    /// default); < 1.0 underdamped (bounces); > 1.0 overdamped (slower).
    pub damping_ratio: f64,
    /// Spring stiffness; higher = snappier.
    pub stiffness: f64,
    /// Position threshold (px) at which the spring is considered at
    /// rest; also the snap threshold for tiny retargets.
    pub epsilon: f64,
}

impl SpringParams {
    /// Niri's default spring for window movement/resizing.
    pub const fn niri_default() -> Self {
        SpringParams {
            damping_ratio: 1.0,
            stiffness: 800.0,
            epsilon: 0.0001,
        }
    }
}

/// Which math drives an animated value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AnimKind {
    /// Timed easing: progress is `curve(elapsed / duration)`.
    Easing { duration: Duration, curve: Curve },
    /// Damped spring towards the target.
    Spring(SpringParams),
}

impl AnimKind {
    /// An animation that lands instantly (`off`).
    pub fn instant() -> Self {
        AnimKind::Easing {
            duration: Duration::ZERO,
            curve: Curve::Linear,
        }
    }

    /// Effective duration, including the global slowdown factor.
    fn effective_duration(&self, from: f64, to: f64, v0: f64, slowdown: f64) -> Duration {
        let d = match *self {
            AnimKind::Easing { duration, .. } => duration,
            AnimKind::Spring(p) => spring_duration(from, to, v0, &p),
        };
        d.mul_f64(slowdown.max(0.001))
    }
}

/// How long the spring takes to settle within epsilon of its target
/// (sampled numerically — cheap, robust, and matches niri's approach
/// of a fixed up-front duration instead of per-frame convergence
/// checks).
fn spring_duration(from: f64, to: f64, v0: f64, p: &SpringParams) -> Duration {
    if (from - to).abs() <= f64::EPSILON && v0.abs() <= f64::EPSILON {
        return Duration::ZERO;
    }
    let step = 0.001; // 1 ms samples
    let mut t = 0.0f64;
    // Cap at 3s: any real spring in this program settles way earlier.
    while t < 3.0 {
        t += step;
        if (spring_value(from, to, v0, p, t) - to).abs() < p.epsilon.max(0.000001) {
            return Duration::from_secs_f64(t);
        }
    }
    Duration::from_secs(3)
}

/// Closed-form position of the damped harmonic oscillator at time `t`
/// (seconds), for mass = 1. Handles the underdamped, critically
/// damped and overdamped solutions.
fn spring_value(from: f64, to: f64, v0: f64, p: &SpringParams, t: f64) -> f64 {
    let m = 1.0;
    let k = p.stiffness.max(0.0);
    let critical = 2.0 * (m * k).sqrt();
    let b = (p.damping_ratio.max(0.0) * critical) / m;
    let beta = b / (2.0 * m);
    let omega0 = (k / m).sqrt();
    let x0 = from - to;

    let envelope = (-beta * t).exp();
    // The critically-damped comparison needs a looser epsilon than
    // f64's (the two solutions converge there).
    if (beta - omega0).abs() <= f64::from(f32::EPSILON) {
        // Critically damped.
        to + envelope * (x0 + (beta * x0 + v0) * t)
    } else if beta < omega0 {
        // Underdamped: oscillates around the target.
        let omega1 = ((omega0 * omega0) - (beta * beta)).sqrt();
        to + envelope * (x0 * (omega1 * t).cos() + ((beta * x0 + v0) / omega1) * (omega1 * t).sin())
    } else {
        // Overdamped: no oscillation, slow approach.
        let omega2 = ((beta * beta) - (omega0 * omega0)).sqrt();
        to + envelope
            * (x0 * (omega2 * t).cosh() + ((beta * x0 + v0) / omega2) * (omega2 * t).sinh())
    }
}

/// Full parameter set for one animated value: the driving math plus
/// niri's global `slowdown` multiplier (values > 1 slow everything
/// down; durations scale by it).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnimParams {
    pub kind: AnimKind,
    pub slowdown: f64,
}

impl Default for AnimParams {
    fn default() -> Self {
        AnimParams {
            kind: AnimKind::Spring(SpringParams::niri_default()),
            slowdown: 1.0,
        }
    }
}

/// One animated scalar.
#[derive(Debug, Clone)]
pub struct Val {
    from: f64,
    to: f64,
    /// Initial velocity (px/s) — springs continue from it on retarget.
    v0: f64,
    start: Instant,
    params: AnimParams,
    /// Precomputed end time (duration includes slowdown).
    duration: Duration,
}

impl Val {
    pub fn to(value: f64, params: AnimParams) -> Self {
        Val {
            from: value,
            to: value,
            v0: 0.0,
            start: Instant::now(),
            duration: params
                .kind
                .effective_duration(value, value, 0.0, params.slowdown),
            params,
        }
    }

    /// Change the target. Springs continue smoothly from the current
    /// value AND velocity (the niri feel); easing curves restart from
    /// the current value. Tiny changes snap instead of animating.
    /// `params` are re-read on every retarget so config hot reloads
    /// take effect on the next move.
    pub fn retarget(&mut self, value: f64, params: AnimParams) {
        self.params = params;
        let cur = self.value();
        let vel = self.velocity();
        let snap = match self.params.kind {
            AnimKind::Spring(p) => (cur - value).abs() < p.epsilon.max(0.25) && vel.abs() < 100.0,
            AnimKind::Easing { .. } => (cur - value).abs() < 0.5,
        };
        if snap {
            self.from = value;
            self.to = value;
            self.v0 = 0.0;
            self.start = Instant::now();
            self.duration = Duration::ZERO;
            return;
        }
        self.from = cur;
        self.to = value;
        self.v0 = match self.params.kind {
            AnimKind::Spring(_) => vel,
            AnimKind::Easing { .. } => 0.0,
        };
        self.start = Instant::now();
        self.duration =
            self.params
                .kind
                .effective_duration(self.from, self.to, self.v0, self.params.slowdown);
    }

    /// Current (animated) value.
    pub fn value(&self) -> f64 {
        if self.finished() {
            return self.to;
        }
        let elapsed = self.start.elapsed();
        match self.params.kind {
            AnimKind::Easing { duration, curve } => {
                let total = duration.as_secs_f64().max(f64::EPSILON);
                let progress = (elapsed.as_secs_f64() / total).clamp(0.0, 1.0);
                self.from + (self.to - self.from) * curve.apply(progress)
            }
            AnimKind::Spring(p) => {
                // Slowdown scales the spring's time axis: the same
                // trajectory, stretched in real time.
                let t = elapsed.as_secs_f64() / self.params.slowdown.max(0.001);
                spring_value(self.from, self.to, self.v0, &p, t)
            }
        }
    }

    /// Current velocity in px/s, estimated by finite differences.
    /// Computed in animation time (post-slowdown scaling) so that
    /// retargets carry the visually-current velocity.
    pub fn velocity(&self) -> f64 {
        let dt = 1.0 / 120.0; // half a frame; fine for carry-over
        let now = self.start.elapsed().as_secs_f64() / self.params.slowdown.max(0.001);
        if now <= dt {
            return self.v0;
        }
        let prev = now - dt;
        match self.params.kind {
            AnimKind::Spring(p) => {
                let a = spring_value(self.from, self.to, self.v0, &p, prev);
                let b = spring_value(self.from, self.to, self.v0, &p, now);
                (b - a) / dt
            }
            AnimKind::Easing { .. } => {
                // Values before the start are the initial value.
                let a = self.from;
                let b = self.value();
                (b - a) / dt.max(now)
            }
        }
    }

    pub fn finished(&self) -> bool {
        self.duration.is_zero() || self.start.elapsed() >= self.duration
    }

    #[allow(dead_code)] // consumed by exit-restore / overview
    pub fn target(&self) -> f64 {
        self.to
    }
}

/// A (x, y, w, h) rectangle whose components animate independently:
/// position with the window-movement parameters, size with the
/// window-resize parameters (niri animates the two differently).
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
    pub fn new(x: f64, y: f64, w: f64, h: f64, movement: AnimParams, resize: AnimParams) -> Self {
        AnimatedRect {
            x: Val::to(x, movement),
            y: Val::to(y, movement),
            w: Val::to(w, resize),
            h: Val::to(h, resize),
            at_rest_pushed: false,
        }
    }

    pub fn retarget(&mut self, x: f64, y: f64, w: f64, h: f64, movement: AnimParams, resize: AnimParams) {
        self.x.retarget(x, movement);
        self.y.retarget(y, movement);
        self.w.retarget(w, resize);
        self.h.retarget(h, resize);
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
        (
            self.x.target(),
            self.y.target(),
            self.w.target(),
            self.h.target(),
        )
    }
}

/// Tracks all window animations and ticks them.
#[derive(Debug, Default)]
pub struct Animator {
    /// Per-window animated rect, keyed by window id.
    rects: std::collections::HashMap<isize, AnimatedRect>,
    movement: AnimParams,
    resize: AnimParams,
}

impl Animator {
    pub fn new(movement: AnimParams, resize: AnimParams) -> Self {
        Animator {
            rects: std::collections::HashMap::new(),
            movement,
            resize,
        }
    }

    /// Set/retarget a window's target rect.
    pub fn set_target(&mut self, id: isize, x: f64, y: f64, w: f64, h: f64) {
        match self.rects.get_mut(&id) {
            Some(r) => r.retarget(x, y, w, h, self.movement, self.resize),
            None => {
                self.rects.insert(
                    id,
                    AnimatedRect::new(x, y, w, h, self.movement, self.resize),
                );
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
    pub fn set_params(&mut self, movement: AnimParams, resize: AnimParams) {
        self.movement = movement;
        self.resize = resize;
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

    fn spring_params() -> AnimParams {
        AnimParams {
            kind: AnimKind::Spring(SpringParams {
                damping_ratio: 1.0,
                stiffness: 800.0,
                epsilon: 0.0001,
            }),
            slowdown: 1.0,
        }
    }

    fn easing_params() -> AnimParams {
        AnimParams {
            kind: AnimKind::Easing {
                duration: Duration::from_millis(250),
                curve: Curve::EaseOutCubic,
            },
            slowdown: 1.0,
        }
    }

    /// 1ms linear — effectively instant, for tests that just want
    /// animations to finish.
    fn fast_params() -> AnimParams {
        AnimParams {
            kind: AnimKind::Easing {
                duration: Duration::from_millis(1),
                curve: Curve::Linear,
            },
            slowdown: 1.0,
        }
    }

    #[test]
    fn easing_bounds() {
        for c in [
            Curve::Linear,
            Curve::EaseOutQuad,
            Curve::EaseOutCubic,
            Curve::EaseOutExpo,
            Curve::EaseOutBack(1.70158),
            Curve::CubicBezier(0.05, 0.7, 0.1, 1.0),
        ] {
            assert!((c.apply(0.0) - 0.0).abs() < 1e-9, "{c:?} at 0");
            assert!((c.apply(1.0) - 1.0).abs() < 1e-9, "{c:?} at 1");
            let mid = c.apply(0.5);
            assert!((-0.5..=1.5).contains(&mid), "{c:?} mid sane: {mid}");
            // Cubic bezier must be monotonic-ish in time: no wild jumps
            // between nearby samples.
            let a = c.apply(0.45);
            assert!((a - mid).abs() < 0.3, "{c:?} continuous: {a} vs {mid}");
        }
    }

    #[test]
    fn val_finishes_at_target() {
        let mut v = Val::to(100.0, easing_params());
        v.retarget(200.0, easing_params());
        std::thread::sleep(Duration::from_millis(300));
        assert!(v.finished());
        assert!((v.value() - 200.0).abs() < 1e-9);
    }

    #[test]
    fn spring_settles_at_target() {
        let mut v = Val::to(0.0, spring_params());
        v.retarget(500.0, spring_params());
        // Niri's default spring settles in well under a second.
        std::thread::sleep(Duration::from_millis(900));
        assert!(v.finished(), "spring finished");
        assert!((v.value() - 500.0).abs() < 0.5, "spring at target");
    }

    #[test]
    fn spring_carries_velocity_on_retarget() {
        let mut v = Val::to(0.0, spring_params());
        v.retarget(500.0, spring_params());
        // Mid-flight retarget: the spring must not restart from rest.
        std::thread::sleep(Duration::from_millis(60));
        let vel = v.velocity();
        assert!(vel > 100.0, "moving fast mid-flight: {vel} px/s");
        v.retarget(400.0, spring_params());
        // Right after the retarget the velocity estimate still reflects
        // the carried-over motion (sign preserved through the value
        // history: v0 was recorded at retarget time).
        // The key property: it did not snap back to the start.
        let cur = v.value();
        assert!(cur > 1.0, "continued from current value: {cur}");
    }

    #[test]
    fn spring_retarget_never_jumps_backwards_hard() {
        // Rapid retargeting to the same target keeps values bounded.
        let mut v = Val::to(0.0, spring_params());
        for i in 0..20 {
            v.retarget(if i % 2 == 0 { 300.0 } else { 301.0 }, spring_params());
        }
        let val = v.value();
        assert!((-5.0..=320.0).contains(&val), "bounded: {val}");
    }

    #[test]
    fn animator_ticks_and_final_frame() {
        let mut a = Animator::new(fast_params(), fast_params());
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
        let mut r = AnimatedRect::new(0.0, 0.0, 100.0, 100.0, fast_params(), fast_params());
        assert_eq!(r.value(), (0, 0, 100, 100));
        r.retarget(10.0, 20.0, 30.0, 40.0, fast_params(), fast_params());
        std::thread::sleep(Duration::from_millis(10));
        assert_eq!(r.value(), (10, 20, 30, 40));
        assert!(r.finished());
    }

    #[test]
    fn finish_all_snaps_to_targets() {
        let mut a = Animator::new(
            AnimParams {
                kind: AnimKind::Easing {
                    duration: Duration::from_millis(10_000), // never finishes
                    curve: Curve::Linear,
                },
                slowdown: 1.0,
            },
            easing_params(),
        );
        a.set_target(7, 0.0, 0.0, 100.0, 100.0);
        a.set_target(7, 50.0, 60.0, 70.0, 80.0);
        assert!(a.is_animating());
        let finals = a.finish_all();
        assert_eq!(finals, vec![(7, 50, 60, 70, 80)]);
        assert!(!a.is_animating());
        assert!(a.finish_all().is_empty());
    }

    #[test]
    fn slowdown_scales_duration() {
        let p = AnimParams {
            kind: AnimKind::Easing {
                duration: Duration::from_millis(100),
                curve: Curve::Linear,
            },
            slowdown: 3.0,
        };
        let mut v = Val::to(0.0, p);
        v.retarget(100.0, p);
        std::thread::sleep(Duration::from_millis(200));
        assert!(!v.finished(), "still running at 2x the base duration");
        std::thread::sleep(Duration::from_millis(250));
        assert!(v.finished(), "finished by 3x the base duration");
    }

    #[test]
    fn slowdown_stretches_spring_trajectory() {
        // Regression: the spring's position function must run on the
        // slowed-down time axis, not just the finished() clock —
        // otherwise slowdown changes nothing visually.
        let normal = AnimParams {
            kind: AnimKind::Spring(SpringParams::niri_default()),
            slowdown: 1.0,
        };
        let slowed = AnimParams {
            kind: AnimKind::Spring(SpringParams::niri_default()),
            slowdown: 4.0,
        };
        let mut a = Val::to(0.0, normal);
        let mut b = Val::to(0.0, slowed);
        a.retarget(1000.0, normal);
        b.retarget(1000.0, slowed);
        std::thread::sleep(Duration::from_millis(100));
        // At the same wall-clock instant the slowed spring must have
        // covered much less distance.
        let (va, vb) = (a.value(), b.value());
        assert!(
            vb < va * 0.75,
            "slowed spring lags: slow={vb} fast={va}"
        );
    }
}
