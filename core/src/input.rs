//! PC input conversion and render-frame sample retention.
//! [game] 004f0e30/004f0e60, 004f0690, 0050b6b0, 005aa700/005aa890.

use glam::Vec2;

pub const MOUSE_DIVISOR: i32 = 10;
pub const SAMPLE_SECONDS: f32 = 0.032;

/// Positive actions, merged before subtracting opposing directions.
#[derive(Clone, Copy, Debug, Default)]
pub struct Directions {
    pub left: f32,
    pub right: f32,
    pub down: f32,
    pub up: f32,
}

impl Directions {
    pub fn signed(v: Vec2) -> Self {
        Self { left: (-v.x).max(0.0), right: v.x.max(0.0), down: (-v.y).max(0.0), up: v.y.max(0.0) }
    }

    /// [game] 004f0690: strongest binding per action, not a sum of devices.
    pub fn merge(&mut self, other: Self) {
        self.left = self.left.max(other.left);
        self.right = self.right.max(other.right);
        self.down = self.down.max(other.down);
        self.up = self.up.max(other.up);
    }

    pub fn mouse(counts: Vec2, divisor: i32) -> Self {
        // [game] 004f0e60 guards zero as one; host Y is up, device Y is down.
        Self::signed((Vec2::new(counts.x, -counts.y) / divisor.max(1) as f32).clamp(Vec2::NEG_ONE, Vec2::ONE))
    }

    /// [game] Direction gate, signed16 bridge, and default axial dead zone.
    /// Return Y up for the host, undoing the engine's Y-down convention.
    pub fn filtered(self) -> Vec2 {
        let axis = |positive: f32, negative: f32, vertical: bool| {
            let gate = |v: f32| if v < 0.15 { 0.0 } else { v.min(1.0) };
            // [assumed] Truncation approximates the runtime integer helper;
            // its alternate rounding branch remains unverified. notes/status.md.
            let raw = (gate(positive) * 32767.0 - gate(negative) * 32768.0).trunc();
            let normalized = if vertical {
                // Y range is reversed (32768..-32767), then inverted for host Y-up.
                1.0 + (raw - 32768.0) * 2.0 / 65535.0
            } else {
                (raw + 32768.0) * 2.0 / 65535.0 - 1.0
            }.clamp(-1.0, 1.0);
            if normalized.abs() <= 0.25 { 0.0 } else { normalized.signum() * (normalized.abs() - 0.25) / 0.75 }
        };
        Vec2::new(axis(self.right, self.left, false), axis(self.up, self.down, true))
    }
}

/// [game] 008e8bb0 and the zero-divisor guard in 004f0e60.
pub fn mouse_divisor(setting: f32) -> i32 {
    (((1.05_f32 - setting.clamp(0.0, 1.0)) * 20.0).trunc() as i32).max(1)
}

/// Preserve render-frame deltas until each camera update consumes them.
/// [assumed] Uniform event timing within a frame and 32ms device windows.
/// Original poll scheduling has not been captured; see notes/status.md.
#[derive(Default, Debug)]
pub struct LookSampler {
    clock: f32,
    counts: Vec2,
    last: Vec2,
}

impl LookSampler {
    pub fn clear_mouse(&mut self) {
        self.counts = Vec2::ZERO;
        self.last = Vec2::ZERO;
    }

    pub fn frame(&mut self, analog: Directions, delta: Vec2, divisor: i32, dt: f32, ready: &mut Vec<Vec2>) -> Vec2 {
        ready.clear();
        if !dt.is_finite() || dt <= 0.0 { return self.last; }
        let mut need = SAMPLE_SECONDS - self.clock;
        let mut left = dt;
        self.clock += dt;
        while self.clock >= SAMPLE_SECONDS {
            let duration = need.min(left).max(0.0);
            self.counts += delta * (duration / dt);
            left = (left - duration).max(0.0);
            let mut value = analog;
            value.merge(Directions::mouse(self.counts, divisor));
            self.last = value.filtered();
            ready.push(self.last);
            self.counts = Vec2::ZERO;
            self.clock -= SAMPLE_SECONDS;
            need = SAMPLE_SECONDS;
        }
        self.counts += delta * (left / dt);
        self.last
    }
}

/// [stand-in] Capture click must be released before it becomes gameplay input.
#[derive(Default, Debug)]
pub struct MouseLatch {
    active: bool,
    blocked: [bool; 3],
}

impl MouseLatch {
    pub fn frame(&mut self, active: bool, held: [bool; 3]) -> [bool; 3] {
        if !active {
            self.active = false;
            self.blocked = [false; 3];
            return [false; 3];
        }
        if !self.active { self.blocked = held; }
        self.active = true;
        std::array::from_fn(|i| {
            self.blocked[i] &= held[i];
            held[i] && !self.blocked[i]
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_axes_use_maximum_then_cancel_opposing_actions() {
        let mut value = Directions::signed(Vec2::new(0.6, 0.0));
        value.merge(Directions::signed(Vec2::new(0.8, 0.0)));
        assert!((value.filtered().x - (0.8 - 0.25) / 0.75).abs() < 0.0001);
        value.merge(Directions::signed(Vec2::NEG_X));
        assert_eq!(value.filtered(), Vec2::ZERO);
        assert_eq!(Directions::mouse(Vec2::new(2.0, 0.0), 10).filtered(), Vec2::ZERO);
        assert!((Directions::mouse(Vec2::new(20.0, -20.0), 10).filtered() - Vec2::ONE).length() < 0.0001);
    }

    #[test]
    fn count_samples_are_independent_of_render_rate() {
        for fps in [30.0, 60.0, 144.0, 240.0] {
            let mut sampler = LookSampler::default();
            let mut ready = Vec::new();
            let mut samples = Vec::new();
            for _ in 0..fps as usize {
                sampler.frame(Directions::default(), Vec2::new(200.0 / fps, 0.0), 10, 1.0 / fps, &mut ready);
                samples.extend_from_slice(&ready);
            }
            assert_eq!(samples.len(), 31, "fps={fps}");
            let expected = Directions::mouse(Vec2::new(6.4, 0.0), 10).filtered();
            for sample in samples { assert!((sample - expected).length() < 0.0001, "fps={fps}: {sample:?}"); }
        }
    }

    #[test]
    fn short_flick_survives_frames_without_a_camera_update() {
        let mut sampler = LookSampler::default();
        let mut ready = Vec::new();
        sampler.frame(Directions::default(), Vec2::new(8.0, 0.0), 10, 0.008, &mut ready);
        assert!(ready.is_empty());
        sampler.frame(Directions::default(), Vec2::ZERO, 10, 0.024, &mut ready);
        assert_eq!(ready.len(), 1);
        assert!(ready[0].x > 0.7);
        sampler.frame(Directions::default(), Vec2::ZERO, 10, 0.032, &mut ready);
        assert_eq!(ready, [Vec2::ZERO]);
    }

    #[test]
    fn loss_of_capture_discards_pending_mouse_counts() {
        let mut sampler = LookSampler::default();
        let mut ready = Vec::new();
        sampler.frame(Directions::default(), Vec2::new(10.0, 0.0), 10, 0.008, &mut ready);
        sampler.clear_mouse();
        sampler.frame(Directions::default(), Vec2::ZERO, 10, 0.024, &mut ready);
        assert_eq!(ready, [Vec2::ZERO]);
        let mut gate = MouseLatch::default();
        assert_eq!(gate.frame(true, [true, false, false]), [false; 3]);
        assert_eq!(gate.frame(true, [true, false, false]), [false; 3]);
        gate.frame(true, [false; 3]);
        assert_eq!(gate.frame(true, [true, false, false]), [true, false, false]);
        assert_eq!(gate.frame(false, [true; 3]), [false; 3]);
    }

    #[test]
    fn sensitivity_endpoint_is_guarded() {
        assert_eq!(mouse_divisor(1.0), 1);
        assert_eq!(mouse_divisor(0.0), 21);
        assert_eq!(Directions::mouse(Vec2::ONE, 0).filtered(), Vec2::new(1.0, -1.0));
    }
}
