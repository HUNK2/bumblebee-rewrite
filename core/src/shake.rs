//! Camera shakes, as read from the original (`notes\camera.md`, "Shakes"). A shake is a
//! settings object from `camera.lxb` (`DefaultCameraShake`, `MassiveCameraShake`...)
//! started by name, at a place or everywhere; each update it draws random angles and
//! offsets inside limits that fade with its age and with the camera's distance, and chases
//! them. All running shakes are put together into one turn and one offset of the camera.
//! No engine types; space is the game's (x right, y forward, z up).

use glam::{Mat3, Vec2, Vec3};

use crate::camera::View;

/// A step shorter than this changes nothing (`00b6a404`). [game]
const SHORTEST_STEP: f32 = 0.0001;
/// A `shakeMaxDist`, or a gap between it and `shakeFullDist`, no bigger than this does not
/// count (`00b343a0`). [game]
const NO_DISTANCE: f32 = 0.01;

/// A shake's settings (loader `FUN_0091f0f0`). Angles are in radians: the loader turns the
/// data's degrees over, `shakeMaxAngAcc` included. [data]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShakeDef {
    /// The most the view turns: to the side, up and down, and about its own line.
    pub ang_horiz: f32,
    pub ang_vert: f32,
    pub tilt: f32,
    /// The most the eye moves: to the side and up.
    pub pos_horiz: f32,
    pub pos_vert: f32,
    /// A shake with a place is whole within `full_dist` of the camera and gone at `max_dist`.
    pub full_dist: f32,
    pub max_dist: f32,
    pub duration: f32,
    /// Shapes the fade (see `envelope`).
    pub half_duration: f32,
    /// The chase of the drawn angles: the most its speed may change in a second (0: no
    /// limit), the share of the drawn angle aimed at, and the share of speed lost a second.
    pub max_ang_acc: f32,
    pub ang_gain: f32,
    pub ang_damp: f32,
    /// The same for the offsets.
    pub max_offs_acc: f32,
    pub offs_gain: f32,
    pub offs_damp: f32,
}

impl Default for ShakeDef {
    /// Missing-field defaults in 0091f0f0. [game]
    fn default() -> Self {
        Self {
            ang_horiz: 0.0,
            ang_vert: 0.0,
            tilt: 0.0,
            pos_horiz: 0.0,
            pos_vert: 0.0,
            full_dist: f32::MAX,
            max_dist: f32::MAX,
            duration: 1.0,
            half_duration: 0.5,
            max_ang_acc: 0.0,
            ang_gain: 1.0,
            ang_damp: 0.0,
            max_offs_acc: 100.0,
            offs_gain: 1.0,
            offs_damp: 0.0,
        }
    }
}

impl ShakeDef {
    /// How strong the shake is `age` seconds in, 1 to 0 (`FUN_0091f5d0`): with h the half
    /// duration and t the age, both as shares of the duration, and
    /// k = (h - 1) / h * (h - 0.5), it is (k t - (k + 1)) t + 1 kept within 0..=1. A half
    /// duration of half the duration gives a straight line. [game]
    pub fn envelope(&self, age: f32) -> f32 {
        let h = self.half_duration / self.duration;
        let t = age / self.duration;
        let k = (h - 1.0) / h * (h - 0.5);
        let strength = (k * t - (k + 1.0)) * t + 1.0;
        // The original's two compares leave a NaN (a duration of 0) at 0.
        if strength > 0.0 {
            strength.min(1.0)
        } else {
            0.0
        }
    }

    /// The share left of a shake with a place, `distance` from the camera (`FUN_0091f5d0`).
    /// [game]
    pub fn reach(&self, distance: f32) -> f32 {
        if self.max_dist <= NO_DISTANCE {
            return 1.0;
        }
        if self.max_dist - self.full_dist <= NO_DISTANCE {
            return if distance > self.max_dist { 0.0 } else { 1.0 };
        }
        (1.0 - (distance - self.full_dist) / (self.max_dist - self.full_dist)).clamp(0.0, 1.0)
    }
}

/// One running shake (0x58 bytes in the original, made by `FUN_0091dc70` or, with a place,
/// `FUN_0091dd30`).
#[derive(Clone, Debug)]
struct Shake {
    def: ShakeDef,
    /// Where it is; `None` shakes the camera wherever it is (`+0x55` clear).
    at: Option<Vec3>,
    /// Up and down, about the view's own line, to the side (`+0x18`), and their speeds
    /// (`+0x24`).
    angles: Vec3,
    angle_speeds: Vec3,
    /// To the side and up (`+0x30`), and their speeds (`+0x38`).
    offsets: Vec2,
    offset_speeds: Vec2,
    age: f32,
}

/// One value's chase of a freshly drawn one (`FUN_0091f5d0`): the speed wanted is the one
/// that would put the value on `drawn * gain` after this step, less what `damp` takes; the
/// change of speed is limited, and the value is kept inside `limit`. Its speed is not: it
/// goes on as if the value had not been stopped. [game]
fn chase(
    value: &mut f32,
    speed: &mut f32,
    drawn: f32,
    gain: f32,
    damp: f32,
    max_acc: f32,
    limit: f32,
    dt: f32,
) {
    let most = if max_acc > SHORTEST_STEP {
        max_acc * dt
    } else {
        f32::MAX
    };
    let change = ((1.0 - damp) * dt * *speed + drawn * gain - *value) / dt - *speed;
    *speed += change.clamp(-most, most);
    *value = (*value + *speed * dt).max(-limit).min(limit);
}

impl Shake {
    /// One update (`FUN_0091f5d0`); false once it is over. [game]
    fn step(
        &mut self,
        eye: Vec3,
        dt: f32,
        random: &mut Random,
        clock: &mut impl FnMut() -> u32,
    ) -> bool {
        if dt < SHORTEST_STEP {
            return true;
        }
        let d = &self.def;
        let mut strength = d.envelope(self.age);
        if let Some(at) = self.at {
            strength *= d.reach(eye.distance(at));
        }
        // Drawn in this order: up and down, the tilt, to the side; then the two offsets.
        let limits = Vec3::new(d.ang_vert, d.tilt, d.ang_horiz) * strength;
        for i in 0..3 {
            let drawn = random.between(-limits[i], limits[i], clock());
            chase(
                &mut self.angles[i],
                &mut self.angle_speeds[i],
                drawn,
                d.ang_gain,
                d.ang_damp,
                d.max_ang_acc,
                limits[i],
                dt,
            );
        }
        let limits = Vec2::new(d.pos_horiz, d.pos_vert) * strength;
        for i in 0..2 {
            let drawn = random.between(-limits[i], limits[i], clock());
            chase(
                &mut self.offsets[i],
                &mut self.offset_speeds[i],
                drawn,
                d.offs_gain,
                d.offs_damp,
                d.max_offs_acc,
                limits[i],
                dt,
            );
        }
        self.age += dt;
        self.age <= d.duration
    }

    /// Its turn of the camera, in the camera's own axes (`FUN_0091fc80`): the tilt about
    /// the view's line first, then up and down, then to the side. [game]
    fn turn(&self) -> Mat3 {
        Mat3::from_rotation_z(self.angles.z)
            * Mat3::from_rotation_x(self.angles.x)
            * Mat3::from_rotation_y(self.angles.y)
    }
}

/// The original's random numbers (`FUN_00551d60`): the C library's multiplier and step,
/// and fifteen bits mixed with clock seconds as a share of the range. [game]
/// [stand-in] This stream is camera-local; the original shares its LCG with other
/// systems. Their complete random-call order is not reconstructed.
#[derive(Clone, Debug)]
struct Random(u32);

impl Random {
    fn between(&mut self, low: f32, high: f32, seconds: u32) -> f32 {
        self.0 = self.0.wrapping_mul(0x343fd).wrapping_add(0x26_9ec3);
        let share =
            f32::from_bits(((self.0 >> 8) ^ seconds.wrapping_shl(8)) & 0x7f_ff00 | 0x3f80_0000)
                - 1.0;
        low + (high - low) * share
    }
}

/// What the running shakes do to the camera this update: a turn in the camera's own axes
/// (x right, y along the view, z up) and an offset to its right and up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Jolt {
    pub turn: Mat3,
    pub offset: Vec2,
}

impl Default for Jolt {
    fn default() -> Self {
        Self {
            turn: Mat3::IDENTITY,
            offset: Vec2::ZERO,
        }
    }
}

/// A view with the shakes on it: it may be rolled, so it carries its own up.
#[derive(Clone, Copy, Debug)]
pub struct Shaken {
    pub eye: Vec3,
    pub forward: Vec3,
    pub up: Vec3,
}

impl Jolt {
    /// The view with the jolt on it. The offset goes along the unshaken camera's right and
    /// up (`FUN_0091df50`), then the renderer composes the camera affine with that
    /// shake affine (0077a72c / 0052fbf0). [game] Thus the stored world offset is
    /// transformed once more by the camera basis; preserve that original behaviour.
    pub fn apply(&self, view: &View) -> Shaken {
        let level = view.forward.cross(Vec3::Z).normalize_or(Vec3::X);
        // The driving camera's lean with the car: its right side down by the view's roll.
        let right = level * view.roll.cos() - level.cross(view.forward) * view.roll.sin();
        let up = right.cross(view.forward);
        let axes = Mat3::from_cols(right, view.forward, up);
        Shaken {
            eye: view.eye + axes * (right * self.offset.x + up * self.offset.y),
            forward: axes * (self.turn * Vec3::Y),
            up: axes * (self.turn * Vec3::Z),
        }
    }
}

/// The camera's running shakes (its list at `+0x184`).
#[derive(Clone, Debug)]
pub struct Shakes {
    running: Vec<Shake>,
    random: Random,
}

impl Default for Shakes {
    fn default() -> Self {
        // The original's seed as the executable holds it (`00c02ef0`).
        Self {
            running: Vec::new(),
            random: Random(0x57),
        }
    }
}

impl Shakes {
    /// Starts a shake: at a place (`FUN_0091dd30`), or with none wherever the camera is
    /// (`FUN_0091dc70`). Shakes do not replace each other; they add up. [game]
    pub fn start(&mut self, def: ShakeDef, at: Option<Vec3>) {
        self.running.push(Shake {
            def,
            at,
            angles: Vec3::ZERO,
            angle_speeds: Vec3::ZERO,
            offsets: Vec2::ZERO,
            offset_speeds: Vec2::ZERO,
            age: 0.0,
        });
    }

    pub fn any(&self) -> bool {
        !self.running.is_empty()
    }

    /// One update of every shake with the camera at `eye` (`FUN_0091df50`): a shake that
    /// ends this update is dropped without a last say; the turns of the rest are
    /// multiplied in the order they were started, oldest first, and their offsets added.
    /// Clock zero gives deterministic offline replay; runtime supplies Unix seconds
    /// per draw with `step_with_clock`. [game]
    pub fn step(&mut self, eye: Vec3, dt: f32) -> Jolt {
        self.step_with_clock(eye, dt, || 0)
    }

    /// The original reads `_time64` for each of its five random draws (00551d60).
    /// Clock is injected so recorded/reproducible tests do not depend on wall time. [game]
    pub fn step_with_clock(&mut self, eye: Vec3, dt: f32, mut clock: impl FnMut() -> u32) -> Jolt {
        let random = &mut self.random;
        let mut jolt = Jolt::default();
        self.running.retain_mut(|shake| {
            if !shake.step(eye, dt, random, &mut clock) {
                return false;
            }
            jolt.turn = shake.turn() * jolt.turn;
            jolt.offset += shake.offsets;
            true
        });
        jolt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `MassiveCameraShake` as `camera.lxb` has it, angles turned to radians.
    fn massive() -> ShakeDef {
        ShakeDef {
            ang_horiz: 1.2_f32.to_radians(),
            ang_vert: 1.2_f32.to_radians(),
            tilt: 0.5_f32.to_radians(),
            pos_horiz: 0.35,
            pos_vert: 0.35,
            full_dist: 20.0,
            max_dist: 90.0,
            duration: 2.5,
            half_duration: 1.25,
            max_ang_acc: 0.0,
            ang_gain: 1.0,
            ang_damp: 0.0,
            max_offs_acc: 0.0,
            offs_gain: 1.0,
            offs_damp: 0.0,
        }
    }

    #[test]
    fn the_envelope_runs_from_one_to_nothing() {
        let d = massive();
        assert_eq!(d.envelope(0.0), 1.0);
        // Half the duration for a half duration: a straight line.
        assert!((d.envelope(1.25) - 0.5).abs() < 1e-6);
        assert!(d.envelope(2.5).abs() < 1e-6);
        assert_eq!(d.envelope(3.0), 0.0);
        // `MedHardCameraShake` (0.65 and 0.15): k = (h - 1) / h * (h - 0.5), about 0.897.
        let d = ShakeDef {
            duration: 0.65,
            half_duration: 0.15,
            ..d
        };
        let (h, t) = (0.15 / 0.65_f32, 0.5_f32);
        let k = (h - 1.0) / h * (h - 0.5);
        assert!((k - 0.897).abs() < 1e-3);
        assert!((d.envelope(0.325) - ((k * t - (k + 1.0)) * t + 1.0)).abs() < 1e-6);
        assert!(d.envelope(0.65).abs() < 1e-6);
    }

    #[test]
    fn a_shake_with_a_place_fades_with_distance() {
        let d = massive();
        assert_eq!(d.reach(5.0), 1.0);
        assert_eq!(d.reach(20.0), 1.0);
        assert!((d.reach(55.0) - 0.5).abs() < 1e-6);
        assert_eq!(d.reach(90.0), 0.0);
        assert_eq!(d.reach(500.0), 0.0);
        // No room between the two distances: all or nothing at the far one.
        let d = ShakeDef {
            full_dist: 30.0,
            max_dist: 30.0,
            ..d
        };
        assert_eq!((d.reach(29.0), d.reach(31.0)), (1.0, 0.0));
        // No far distance: everywhere.
        assert_eq!(ShakeDef { max_dist: 0.0, ..d }.reach(1000.0), 1.0);
    }

    #[test]
    fn a_shake_stays_inside_its_limits_and_ends() {
        let d = massive();
        let mut shakes = Shakes::default();
        shakes.start(d, Some(Vec3::ZERO));
        let eye = Vec3::new(0.0, -10.0, 4.0);
        let mut moved = false;
        let mut updates = 0;
        while shakes.any() {
            let age = updates as f32 * 0.032;
            let jolt = shakes.step(eye, 0.032);
            updates += 1;
            if !shakes.any() {
                // Dropped the update it ends: nothing of it is left on the camera.
                assert_eq!(jolt, Jolt::default());
                break;
            }
            let strength = d.envelope(age) + 1e-6;
            assert!(
                jolt.offset.x.abs() <= d.pos_horiz * strength
                    && jolt.offset.y.abs() <= d.pos_vert * strength
            );
            let forward = jolt.turn * Vec3::Y;
            assert!(
                forward.x.abs() <= (d.ang_horiz * strength).sin() + 1e-6,
                "to the side at update {updates}"
            );
            assert!(
                forward.z.abs() <= (d.ang_vert * strength).sin() + 1e-6,
                "up and down at update {updates}"
            );
            moved |= jolt.offset != Vec2::ZERO && jolt.turn != Mat3::IDENTITY;
        }
        assert!(moved);
        // 2.5 s in updates of 0.032: over on the first update that takes the age past it.
        assert_eq!(updates, (2.5_f32 / 0.032).floor() as i32 + 1);
    }

    #[test]
    fn out_of_reach_or_out_of_time_nothing_moves() {
        let mut shakes = Shakes::default();
        shakes.start(massive(), Some(Vec3::ZERO));
        assert_eq!(
            shakes.step(Vec3::new(0.0, 200.0, 0.0), 0.032),
            Jolt::default()
        );
        // A step of no length leaves it as it is, and still there.
        let mut shakes = Shakes::default();
        shakes.start(massive(), None);
        let first = shakes.step(Vec3::ZERO, 0.032);
        assert_ne!(first, Jolt::default());
        assert_eq!(shakes.step(Vec3::ZERO, 0.0), first);
    }

    #[test]
    fn with_gain_one_and_no_damping_the_value_lands_on_the_draw() {
        // From rest the first update puts the value on what was drawn: the speed wanted is
        // (drawn - value) / dt.
        let (mut value, mut speed) = (0.0, 0.0);
        chase(&mut value, &mut speed, 0.3, 1.0, 0.0, 0.0, 1.0, 0.032);
        assert!((value - 0.3).abs() < 1e-6 && (speed - 0.3 / 0.032).abs() < 1e-3);
        // The speed is kept: the next draw is added to where that speed carries it.
        chase(&mut value, &mut speed, -0.1, 1.0, 0.0, 0.0, 1.0, 0.032);
        assert!((value - (-0.1 + 0.3)).abs() < 1e-5);
        // A limit on the change of speed holds it back.
        let (mut value, mut speed) = (0.0, 0.0);
        chase(&mut value, &mut speed, 0.3, 1.0, 0.0, 50.0, 1.0, 0.032);
        assert!((speed - 50.0 * 0.032).abs() < 1e-6 && (value - 50.0 * 0.032 * 0.032).abs() < 1e-6);
    }

    #[test]
    fn the_jolt_moves_the_view_about_its_own_axes() {
        let view = View {
            eye: Vec3::new(1.0, 2.0, 3.0),
            forward: Vec3::Y,
            fov: 60.0,
            roll: 0.0,
        };
        let still = Jolt::default().apply(&view);
        assert!(
            still.forward.distance(Vec3::Y) < 1e-6
                && still.up.distance(Vec3::Z) < 1e-6
                && still.eye == view.eye
        );
        // A turn to the side about the camera's up, and an offset right and up.
        let jolt = Jolt {
            turn: Mat3::from_rotation_z(0.1),
            offset: Vec2::new(0.5, 0.25),
        };
        let shaken = jolt.apply(&view);
        assert!(
            shaken
                .forward
                .distance(Vec3::new(-0.1_f32.sin(), 0.1_f32.cos(), 0.0))
                < 1e-6
        );
        assert!(shaken.eye.distance(Vec3::new(1.5, 2.0, 3.25)) < 1e-6);
        // A tilt rolls the up and leaves the line of sight.
        let shaken = Jolt {
            turn: Mat3::from_rotation_y(0.2),
            offset: Vec2::ZERO,
        }
        .apply(&view);
        assert!(
            shaken.forward.distance(Vec3::Y) < 1e-6 && (shaken.up.x - 0.2_f32.sin()).abs() < 1e-6
        );
    }

    #[test]
    fn clock_seconds_mix_into_each_draw_without_changing_the_lcg() {
        let mut zero = Random(0x57);
        let mut flipped = zero.clone();
        let a = zero.between(0.0, 1.0, 0);
        let b = flipped.between(0.0, 1.0, 0x7fff);
        assert_eq!(zero.0, flipped.0);
        assert_eq!(a + b, 32767.0 / 32768.0);
        let mut draws = 0;
        let mut shakes = Shakes::default();
        shakes.start(massive(), None);
        shakes.step_with_clock(Vec3::ZERO, 0.032, || {
            draws += 1;
            12345
        });
        assert_eq!(draws, 5);
    }

    #[test]
    fn omitted_shake_settings_use_the_original_loader_defaults() {
        let d = ShakeDef::default();
        assert_eq!(d.envelope(0.0), 1.0);
        assert_eq!(d.envelope(0.5), 0.5);
        assert_eq!(d.envelope(1.0), 0.0);
        assert_eq!(d.reach(1000.0), 1.0);
        assert_eq!((d.ang_gain, d.offs_gain, d.max_offs_acc), (1.0, 1.0, 100.0));
    }

    #[test]
    fn render_composition_transforms_the_stored_world_offset_again() {
        let view = View {
            eye: Vec3::new(1.0, 2.0, 3.0),
            forward: Vec3::X,
            fov: 60.0,
            roll: 0.0,
        };
        let jolt = Jolt {
            turn: Mat3::IDENTITY,
            offset: Vec2::new(0.5, 0.25),
        };
        let shaken = jolt.apply(&view);
        // Camera right=-Y. Stored offset=-.5Y+.25Z. camera * offset=-.5X+.25Z.
        assert!(shaken.eye.distance(Vec3::new(0.5, 2.0, 3.25)) < 1e-6);
        assert!(shaken.forward.distance(view.forward) < 1e-6);
    }
}
