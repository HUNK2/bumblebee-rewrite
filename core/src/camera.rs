//! The game's follow, driving and weapon cameras for a character,
//! as read from the original (`notes\camera.md`) and checked against the recorded game
//! (`notes\live-trace.md`). No engine types; space is the game's (x right, y forward, z up).
//!
//! The original runs its camera once per update with gains that are per update, so this one
//! runs in whole updates of the game's own length and hands back a view blended between the
//! last two, which keeps the picture smooth at any frame rate.
//!
//! Obstacles are the original's own rules (`notes\camera.md`, "Collision and corners"): on
//! foot a blocked sight line pulls the eye in, a wall he has just walked round swings the
//! camera about the corner instead, and a ceiling lowers the point looked at; the driving
//! camera is only pulled in. The arena supplies actual box edges and sphere sweeps.
//!
//! The weapon camera (`update_weapon`) follows his aim instead of the stick; what turns
//! the aim is in `aim.rs`.
//!
//! The follow camera has the overhead rule (the heading held while he passes under it).
//! Recentring, airborne tilt and the ground-punch camera mode use the executable's rules.
//! Bumblebee has no sniping or helicopter camera. The shakes are in `shake.rs`.

use glam::{Vec2, Vec3};

use crate::tuning::{CameraTuning, Tuning};
mod fade;
mod geometry;
mod near_plane;
pub use fade::Fades;

/// Recorded-update convention; the original Main update uses smoothed frame time.
/// [assumed: fixed .032 stepping, as documented in notes/status.md]
const UPDATE: f32 = 0.032;
/// The camera option "sensitivity", 0 to 1; a fresh game's turn rates in the recording fit
/// the middle of the range.
const SENSITIVITY: f32 = 0.5;
/// The radius of the sphere the driving camera sweeps instead of a line.
const CAR_SWEEP: f32 = 0.65;
/// How far above the point on the car the driving camera's floor is: neither its eye nor
/// the point it asks the world from goes lower. [trace: 0.500 all through run 1]
const DRIVE_FLOOR: f32 = 0.5;
/// Legacy flat-road replay/test fixture: the recorded point is 1.10..1.15 above the
/// root, with 1.12 in the middle. [trace] Runtime uses the rotated physics holder
/// and avatar half-height (0070abe0), rather than this constant.
pub const CAR_BODY_RISE: f32 = 1.12;
/// How long the follow camera goes on standing off by faceExtraDist once the character
/// stops telling it to (`FUN_00922580`, behaviour `+0x13c`; constant at `00b6b020`).
const STAND_OFF: f32 = 3.7;
/// The extra distance in the climbing camera mode (and in mode 7d110862): a constant in
/// the code, which the settings' climbExtraDist only switches on. [game]
const CLIMB_EXTRA: f32 = 15.0;
/// A hit nearer the point looked at than this leaves the camera where it was.
const TOO_NEAR: f32 = 0.1;
/// How far out of the wall a corner point is put.
const CORNER_CLEAR: f32 = 0.15;
/// A turn smaller than this in one update (half a degree) is not "turning away" from a corner.
const STICK_STILL: f32 = 0.008_726_65;
/// The overhead rule: how far up the last direction to the camera points (its z) before the
/// heading is held back, and where it is held all the way. [game] `notes\camera.md` step 6.
const OVERHEAD_FROM: f32 = 0.83;
const OVERHEAD_FULL: f32 = 0.88;
/// Squared horizontal movement threshold, read at 00924b37. [game]
const OVERHEAD_MOVED_SQUARED: f32 = 0.0001;

/// Character message camera modes. Names other than Normal/Climb describe their effect;
/// the original's symbolic names have not survived. [game]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Normal,
    Climb,
    Fast,
    GroundPunch,
    Transition,
}

/// Options consumed by the camera, independent of the host's input bindings. [game]
#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub sensitivity: f32,
    pub invert_yaw: bool,
    pub invert_pitch: bool,
    pub disable_stick: bool,
    pub separate_distances: bool,
    /// The original renderer's 16:9 flag selects the proxy's vertical near extent.
    pub widescreen: bool,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            sensitivity: SENSITIVITY,
            invert_yaw: false,
            invert_pitch: false,
            disable_stick: false,
            separate_distances: false,
            widescreen: true,
        }
    }
}
impl Options {
    pub fn stick(&self, stick: Vec2) -> Vec2 {
        if self.disable_stick {
            Vec2::ZERO
        } else {
            stick
                * Vec2::new(
                    if self.invert_yaw { -1.0 } else { 1.0 },
                    if self.invert_pitch { -1.0 } else { 1.0 },
                )
        }
    }
    /// Signed level requests wrap modulo three. Shared options write all forms. [game]
    pub fn cycle(&self, levels: &mut [usize; 3], form: usize, delta: i32) {
        let next = (levels[form] as i32 + delta).rem_euclid(3) as usize;
        if self.separate_distances {
            levels[form] = next;
        } else {
            *levels = [next; 3];
        }
    }
}

/// What the camera follows this step.
#[derive(Clone, Copy, Debug, Default)]
pub struct Target {
    /// Where he stands (his feet, or the car's underside).
    pub position: Vec3,
    /// Height of the point looked at above `position`.
    pub height: f32,
    /// The way he faces, on the ground plane.
    pub facing: Vec2,
    /// The velocity the original hands its camera (in the car, the speed after its caps).
    pub velocity: Vec3,
    /// How he is really moving, for placing him at the moment an update belongs to.
    pub motion: Vec3,
    pub vehicle: bool,
    /// On a wall: the camera stands off by `climbExtraDist`. [game]
    pub climbing: bool,
    /// Weapon mode: the way he aims, which the weapon camera follows. `None` otherwise.
    pub aim: Option<Vec3>,
    /// His body's radius in this form (the weapon camera's point swings on it).
    pub radius: f32,
    /// The car body's tilt, radians: nose up, and left side up. Zero on foot.
    pub pitch: f32,
    pub roll: f32,
    /// The character's timer after a melee attack runs (`sim::State::fight`): the follow
    /// camera stands off by faceExtraDist while it does and for 3.7 s after. [game]
    pub fighting: bool,
    pub mode: Mode,
    /// Minimum wheel +0x50: clamp(ray distance - 2*avatar height - rest length,0,100).
    /// This is clearance beyond suspension reach, not altitude or the grounded flag. [game]
    pub ground_distance: f32,
    /// Physics holder centre above the root (also the driving camera's floor). [game]
    /// None retains the recorded legacy height in older replay fixtures.
    pub body_height: Option<f32>,
    /// FlightMode's message flag; unused by Bumblebee, distinct from airborne wheels.
    pub flying: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct View {
    pub eye: Vec3,
    /// Unit vector the camera looks along.
    pub forward: Vec3,
    /// The field of view the original would use, in degrees.
    pub fov: f32,
    /// How far the picture leans with the car, radians (the original's `+0x128`): the
    /// camera's right side goes down by it. Zero for the cameras on foot.
    pub roll: f32,
}

/// Where a line of sight is stopped.
#[derive(Clone, Copy, Debug, Default)]
pub struct Hit {
    pub point: Vec3,
    /// Unit vector out of the surface, towards where the line came from; zero if not known
    /// (then no corner is taken on it).
    pub normal: Vec3,
    pub shape: Option<Shape>,
    /// Swept centre travel. Lux returns the contact on the obstacle separately. [game]
    pub distance: Option<f32>,
}

/// Shape information used by the original's corner routine (types 4 and 6). [game]
#[derive(Clone, Copy, Debug)]
pub enum Shape {
    Box {
        min: Vec3,
        max: Vec3,
    },
    Axis {
        origin: Vec3,
        direction: Vec3,
        radius: f32,
    },
}

/// A line of sight through the world: the first thing hit going from one point to another.
pub trait Sight {
    fn sight(&self, from: Vec3, to: Vec3) -> Option<Hit>;
    /// Obstacle contact point and sphere-centre travel at first contact. [game]
    fn sweep(&self, from: Vec3, to: Vec3, radius: f32) -> Option<Hit>;
    /// Nonblocking objects with the original collision fade flag; proxy is excluded by
    /// the scene provider. The authored arena has no such objects. [game]
    fn fade_hits(&self, _from: Vec3, _to: Vec3, _radius: f32) -> Vec<u64> {
        Vec::new()
    }
    /// Camera-proxy sphere contacts, excluding fadeable objects and ignored flag15.
    fn contacts(&self, _centre: Vec3, _radius: f32) -> Vec<Hit> {
        Vec::new()
    }
    fn bounds(&self) -> Option<(Vec3, Vec3)> {
        None
    }
}

/// Nothing in the way, for tests.
pub struct Open;

/// An object with Lux collision9e bit5. These remain nonblocking to the camera;
/// their scene/material provider consumes `Camera::fades`. [game]
#[derive(Clone, Copy, Debug)]
pub struct Fadeable {
    pub id: u64,
    pub bounds: crate::sim::Box3,
}

pub struct Scene<'a> {
    pub solids: &'a dyn Sight,
    pub fadeable: &'a [Fadeable],
}
impl Sight for Scene<'_> {
    fn sight(&self, from: Vec3, to: Vec3) -> Option<Hit> {
        self.solids.sight(from, to)
    }
    fn sweep(&self, from: Vec3, to: Vec3, radius: f32) -> Option<Hit> {
        self.solids.sweep(from, to, radius)
    }
    fn fade_hits(&self, from: Vec3, to: Vec3, radius: f32) -> Vec<u64> {
        let mut hits = self.solids.fade_hits(from, to, radius);
        hits.extend(
            self.fadeable
                .iter()
                .filter(|o| geometry::box_sweep(&o.bounds, from, to, radius).is_some())
                .map(|o| o.id),
        );
        hits
    }
    fn contacts(&self, centre: Vec3, radius: f32) -> Vec<Hit> {
        self.solids.contacts(centre, radius)
    }
    fn bounds(&self) -> Option<(Vec3, Vec3)> {
        self.solids.bounds()
    }
}

impl Sight for Open {
    fn sight(&self, _: Vec3, _: Vec3) -> Option<Hit> {
        None
    }
    fn sweep(&self, _: Vec3, _: Vec3, _: f32) -> Option<Hit> {
        None
    }
}

/// The test arena: flat ground at height zero, boxes and planar ramps.
impl Sight for crate::sim::Arena {
    fn sight(&self, from: Vec3, to: Vec3) -> Option<Hit> {
        let along = to - from;
        // How far along the line (0 to 1) the nearest thing is, and its normal.
        let mut nearest: Option<(f32, Vec3, Option<Shape>)> = None;
        if from.z > 0.0 && to.z < 0.0 {
            nearest = Some((from.z / (from.z - to.z), Vec3::Z, None));
        }
        'boxes: for b in &self.boxes {
            let (mut enter, mut leave, mut normal) = (0.0f32, 1.0f32, Vec3::ZERO);
            for axis in 0..3 {
                let (start, step) = (from[axis], along[axis]);
                if step.abs() < 1e-9 {
                    if start < b.min[axis] || start > b.max[axis] {
                        continue 'boxes;
                    }
                    continue;
                }
                let (a, c) = ((b.min[axis] - start) / step, (b.max[axis] - start) / step);
                if a.min(c) > enter {
                    enter = a.min(c);
                    normal = Vec3::ZERO;
                    normal[axis] = -step.signum();
                }
                leave = leave.min(a.max(c));
                if enter > leave {
                    continue 'boxes;
                }
            }
            // A line that starts inside a box has no face to stop on.
            if normal != Vec3::ZERO && nearest.is_none_or(|(t, _, _)| enter < t) {
                nearest = Some((
                    enter,
                    normal,
                    Some(Shape::Box {
                        min: b.min,
                        max: b.max,
                    }),
                ));
            }
        }
        for ramp in &self.ramps {
            if let Some((t, normal)) = ramp.ray(from, to)
                && nearest.is_none_or(|(near, _, _)| t < near)
            {
                nearest = Some((t, normal, None));
            }
        }
        nearest.map(|(t, normal, shape)| Hit {
            point: from + along * t,
            normal,
            shape,
            distance: Some(along.length() * t),
        })
    }
    fn sweep(&self, from: Vec3, to: Vec3, radius: f32) -> Option<Hit> {
        geometry::arena_sweep(self, from, to, radius)
    }
    fn contacts(&self, centre: Vec3, radius: f32) -> Vec<Hit> {
        geometry::arena_contacts(self, centre, radius)
    }
}

/// A wall he has walked round: the camera looks past this point instead of closing in.
#[derive(Clone, Copy, Debug, Default)]
struct Corner {
    /// Where the last clear sight line crossed the wall's surface, a little out of it.
    point: Vec3,
    /// The wall's normal.
    normal: Vec3,
    /// Along the wall from where the sight line is now stopped to `point`: the open side.
    side: Vec3,
    /// The direction of the edge `point` slides along, if there is one.
    edge: Option<(Vec3, Vec3)>,
    distance: f32,
    axis: bool,
}

impl Corner {
    /// Moves the point along its edge for this update's view. Along a level edge it stays
    /// in the upright plane through the point looked at and the eye; along an upright one it
    /// goes to the height that the camera's elevation looks through.
    fn slide(&mut self, look_at: Vec3, eye: Vec3, elevation: f32) {
        let Some((origin, edge)) = self.edge else {
            return;
        };
        if edge.z * edge.z <= edge.x * edge.x + edge.y * edge.y {
            let across = Vec3::new(-(eye.y - look_at.y), eye.x - look_at.x, 0.0);
            let rate = across.dot(edge);
            if rate.abs() > 0.001 {
                self.point -= edge * (across.dot(self.point - eye) / rate);
            }
        } else {
            let across = edge.cross(look_at - eye).truncate().normalize_or_zero()
                * self.distance
                * (1.0 + edge.truncate().length_squared() / (edge.z * edge.z)).sqrt();
            let origin = origin + across.extend(0.0);
            if self.axis {
                self.side = across.extend(0.0);
                self.normal = (look_at - eye).truncate().normalize_or_zero().extend(0.0);
            }
            let v = origin - look_at;
            let slope = elevation.tan();
            let squared = slope * slope;
            let a = edge.truncate().length_squared() * squared - edge.z * edge.z;
            let b = 2.0 * (v.truncate().dot(edge.truncate()) * squared - v.z * edge.z);
            let c = v.truncate().length_squared() * squared - v.z * v.z;
            let disc = b * b - 4.0 * a * c;
            let t = if disc < 0.0 || a.abs() < 1e-9 {
                (self.point.z - origin.z) / edge.z
            } else {
                let first = (-b + disc.sqrt()) / (2.0 * a);
                if (v.z + first * edge.z).is_sign_negative() == slope.is_sign_negative() {
                    first
                } else {
                    (-b - disc.sqrt()) / (2.0 * a)
                }
            };
            self.point = origin + edge * t;
        }
    }
}

/// The original's test that the stick is not turning the view away from a corner: `turn` is
/// what the stick turned this update, `corner` and `view` the directions (in the plane the
/// turn is in) from the point looked at to the corner and from the eye to the point.
fn not_turning_away(turn: f32, corner: Vec2, view: Vec2) -> bool {
    turn.abs() <= STICK_STILL || turn.sin() * corner.perp_dot(view) > 0.001
}

/// A direction's length over the ground and its rise, as a unit pair.
fn rise_of(v: Vec3) -> Vec2 {
    Vec2::new(Vec2::new(v.x, v.y).length(), v.z).normalize_or_zero()
}

fn flat_of(v: Vec3) -> Vec2 {
    Vec2::new(v.x, v.y).normalize_or_zero()
}

/// The original's damped spring, run once per update: the next change of a value, from how
/// far it is from where it should be and its last change.
fn spring(difference: f32, last_change: f32, gain: f32) -> f32 {
    last_change + gain * difference - 2.0 * gain.sqrt() * last_change
}

pub(crate) fn heading_of(v: Vec2) -> f32 {
    (-v.x).atan2(v.y)
}

fn direction(heading: f32) -> Vec2 {
    Vec2::new(-heading.sin(), heading.cos())
}

pub(crate) fn wrap(angle: f32) -> f32 {
    let mut a = angle % std::f32::consts::TAU;
    if a > std::f32::consts::PI {
        a -= std::f32::consts::TAU;
    } else if a < -std::f32::consts::PI {
        a += std::f32::consts::TAU;
    }
    a
}

#[derive(Clone, Debug)]
pub struct Camera {
    pub fades: Fades,
    pub options: Options,
    clock: f32,
    started: bool,
    vehicle: bool,
    /// The view after the last update and the one before, to blend between.
    now: View,
    before: View,

    /// Where the eye would be with nothing in the way.
    position: Vec3,
    last_target: Vec3,
    /// The look-at point's lag behind a rising or falling target, and its last correction.
    sponge: f32,
    sponge_change: f32,
    /// The height looked at above the target's position, the height a blend to a new one
    /// started from, and the seconds of that blend left.
    height: f32,
    height_from: f32,
    height_left: f32,
    distance: f32,
    distance_step: f32,
    extra: f32,
    extra_rate: f32,
    /// Seconds the stand-off by faceExtraDist still has to run (`+0x13c`).
    hold: f32,
    /// Elevation: the user's part, the part that keeps it in range, and their rates.
    user: f32,
    user_rate: f32,
    auto: f32,
    auto_rate: f32,
    /// Drifting back to the default elevation until the stick says otherwise.
    settling: bool,
    fov: f32,
    fov_rate: f32,
    /// How far the view's pitch and heading trail the line to the look-at point.
    lag: Vec2,
    lag_rate: Vec2,
    lag_unset: bool,
    /// An obstacle shortened the camera on the last update.
    blocked: bool,
    /// The point looked at and the eye as the last update left them.
    last_look: Vec3,
    last_eye: Vec3,
    proxy_point: Vec3,
    corner: Corner,
    cornered: bool,
    /// What the stick turned the view by on this update: heading, and minus the elevation.
    yaw_applied: f32,
    pitch_applied: f32,
    pitch: f32,
    heading: f32,
    /// The heading of the direction from the point looked at to the camera as the follow
    /// camera's last update left it (for the overhead rule).
    last_heading: f32,
    /// A turn asked for outside the stick (a mouse), in radians; used up by the next update.
    pending: Vec2,
    mode: Mode,
    recentre: bool,
    recentre_heading: f32,
    frozen: bool,
    control_left: f32,
    control_time: f32,
    distance_bias: f32,
    wanted_fov: f32,

    // The driving camera's own.
    /// Heading and elevation of the direction from the target to the camera.
    behind: f32,
    elevation: f32,
    elevation_rate: f32,
    user_elevation: f32,
    align_step: f32,
    user_step: f32,
    smooth_speed: f32,
    smooth_rate: f32,
    speed_factor: f32,
    distance_add: f32,
    fov_scale: f32,
    /// How far the picture leans with the car, and its last change (`+0x128`, `+0x184`).
    roll: f32,
    roll_change: f32,
    airborne: bool,

    // The weapon camera's own.
    weapon: bool,
    /// Unit vector from the point it turns about to the camera (behaviour `+0x15c`).
    away: Vec3,
    aim: Vec3,
    /// That point's place beside and ahead of him, in the world, and how fast it moves
    /// to his side and ahead of him, per second (`+0x24`, `+0x28`; `+0x168`, `+0x16c`).
    shoulder: Vec2,
    shoulder_rate: Vec2,
    /// Swinging round to a heading too far off to snap to (`+0xd7`).
    swinging: bool,
    /// How far the elevation trails the aim's, and its last change (`+0x170`, `+0x100`).
    elevation_lag: f32,
    elevation_lag_change: f32,
    elevation_lag_unset: bool,
}

impl Default for Camera {
    fn default() -> Self {
        let view = View {
            eye: Vec3::ZERO,
            forward: Vec3::Y,
            fov: 45.0,
            roll: 0.0,
        };
        Self {
            fades: Fades::default(),
            options: Options::default(),
            clock: 0.0,
            started: false,
            vehicle: false,
            now: view,
            before: view,
            position: Vec3::ZERO,
            last_target: Vec3::ZERO,
            sponge: 0.0,
            sponge_change: 0.0,
            height: 0.0,
            height_from: 0.0,
            height_left: 0.0,
            distance: 0.0,
            distance_step: 0.0,
            extra: 0.0,
            extra_rate: 0.0,
            hold: 0.0,
            user: 0.0,
            user_rate: 0.0,
            auto: 0.0,
            auto_rate: 0.0,
            settling: true,
            fov: 45.0,
            fov_rate: 0.0,
            lag: Vec2::ZERO,
            lag_rate: Vec2::ZERO,
            lag_unset: true,
            blocked: false,
            last_look: Vec3::ZERO,
            last_eye: Vec3::ZERO,
            proxy_point: Vec3::ZERO,
            corner: Corner::default(),
            cornered: false,
            yaw_applied: 0.0,
            pitch_applied: 0.0,
            pitch: 0.0,
            heading: 0.0,
            last_heading: 0.0,
            pending: Vec2::ZERO,
            mode: Mode::Normal,
            recentre: false,
            recentre_heading: 0.0,
            frozen: false,
            control_left: 0.0,
            control_time: 0.0,
            distance_bias: 0.0,
            wanted_fov: 45.0,
            behind: 0.0,
            elevation: 0.0,
            elevation_rate: 0.0,
            user_elevation: 0.0,
            align_step: 0.0,
            user_step: 0.0,
            smooth_speed: 0.0,
            smooth_rate: 0.0,
            speed_factor: 0.0,
            distance_add: 0.0,
            fov_scale: 1.0,
            roll: 0.0,
            roll_change: 0.0,
            airborne: false,
            weapon: false,
            away: Vec3::NEG_Y,
            aim: Vec3::Y,
            shoulder: Vec2::ZERO,
            shoulder_rate: Vec2::ZERO,
            swinging: false,
            elevation_lag: 0.0,
            elevation_lag_change: 0.0,
            elevation_lag_unset: true,
        }
    }
}

/// Which of a character's cameras follows this target.
fn settings_for<'a>(target: &Target, tuning: &'a Tuning) -> &'a CameraTuning {
    if target.vehicle {
        &tuning.drive_camera
    } else if target.aim.is_some() {
        &tuning.weapon_camera
    } else {
        &tuning.robot_camera
    }
}

impl Camera {
    /// Starts behind the target at the camera's default distance and elevation.
    pub fn behind(target: &Target, tuning: &Tuning) -> Self {
        let settings = settings_for(target, tuning);
        let look_at = target.position + Vec3::Z * (target.height + settings.height_offset);
        let elevation = settings.default_elevation.to_radians();
        let back = -target.facing.normalize_or(Vec2::Y);
        let eye = look_at + (back * elevation.cos()).extend(elevation.sin()) * settings.max_dist;
        Self::from_view(
            View {
                eye,
                forward: (look_at - eye).normalize_or(Vec3::Y),
                fov: settings.fov,
                roll: 0.0,
            },
            target,
            tuning,
        )
    }

    /// Starts from a view the host already has, so taking over does not cut.
    pub fn from_view(view: View, target: &Target, tuning: &Tuning) -> Self {
        let mut camera = Camera {
            now: view,
            before: view,
            fov: view.fov,
            ..Camera::default()
        };
        camera.position = view.eye;
        camera.start(target, tuning);
        camera
    }

    /// What both cameras do when they take over: distance and elevation come from where the
    /// camera is now, and the view's lag is set so it swings round rather than cuts.
    fn start(&mut self, target: &Target, tuning: &Tuning) {
        let settings = settings_for(target, tuning);
        let forward = self.now.forward;
        let elevation = (-forward.z).atan2(Vec2::new(forward.x, forward.y).length());
        let changing = self.started;
        let was_weapon = self.weapon;
        let old_height = self.height;
        let old_shoulder = self.shoulder;
        // The follow camera inherits the height the last camera looked at and blends from it.
        let full = target.height + settings.height_offset;
        self.height_from = self.height;
        self.height_left = if changing && !target.vehicle && target.aim.is_none() {
            settings.interp_target_height_time
        } else {
            0.0
        };
        let aim = target.position
            + Vec3::Z
                * if self.height_left > 0.0 {
                    self.height_from
                } else {
                    full
                };
        self.vehicle = target.vehicle;
        self.started = true;
        self.last_target = aim;
        // The point looked at starts on the current view ray (over the target) and is sprung
        // to the new form's height from there, so a change of form does not cut: the robot's
        // point is 4.4 above the car's.
        let flat = Vec2::new(forward.x, forward.y);
        self.shoulder = old_shoulder;
        let reach = if flat.length() > 0.1 {
            (aim.truncate() + self.shoulder - self.position.truncate()).length() / flat.length()
        } else {
            0.0
        };
        self.sponge = if reach > 0.0 {
            (self.position.z + forward.z * reach - aim.z).clamp(-20.0, 20.0)
        } else {
            0.0
        };
        self.sponge_change = 0.0;
        let aim = aim + self.shoulder.extend(self.sponge);
        self.distance = (self.position - aim).length();
        self.distance_step = 0.0;
        self.user = elevation;
        self.user_rate = 0.0;
        self.auto = 0.0;
        self.auto_rate = 0.0;
        self.settling = settings.default_elevation >= -90.0;
        self.pitch = forward.z.asin();
        self.heading = heading_of(Vec2::new(forward.x, forward.y));
        self.last_heading = heading_of(-Vec2::new(forward.x, forward.y));
        self.lag = Vec2::ZERO;
        self.lag_unset = true;
        self.lag_rate = Vec2::ZERO;
        self.blocked = false;
        self.cornered = false;
        self.last_look = aim;
        self.last_eye = self.position;
        self.proxy_point = self.position + (aim - self.position).normalize_or_zero() * 0.2;
        self.yaw_applied = 0.0;
        self.pitch_applied = 0.0;
        self.recentre = false;
        self.distance_bias = 0.0;
        self.mode = Mode::Normal;
        self.wanted_fov = settings.fov;
        // The driving camera's own.
        self.behind = heading_of(-Vec2::new(forward.x, forward.y));
        self.elevation = elevation;
        self.elevation_rate = 0.0;
        self.user_elevation = 0.0;
        self.align_step = 0.0;
        self.user_step = 0.0;
        self.smooth_speed = 0.0;
        self.smooth_rate = 0.0;
        self.speed_factor = 0.0;
        self.distance_add = 0.0;
        self.fov_scale = 1.0;
        self.roll = 0.0;
        self.roll_change = 0.0;
        self.airborne = false;
        self.fov_scale = self.fov;
        // The weapon camera's own start (slot 1, `FUN_00927f60`, after the follow camera's):
        // it keeps the direction the camera is on, does not drift to a default elevation,
        // has no view lag to spring away, and takes its elevation lag afresh. [game]
        self.weapon = target.aim.is_some() && !target.vehicle;
        self.away = -forward;
        self.aim = target.aim.unwrap_or(target.facing.extend(0.0));
        self.swinging = false;
        self.elevation_lag_change = 0.0;
        self.elevation_lag_unset = true;
        // [game] Shared start inherits the previous horizontal sponge. Its rates were
        // cleared by the shared start before the weapon start reads them: they are zero.
        self.shoulder_rate = Vec2::ZERO;
        if self.weapon {
            if changing && !was_weapon {
                self.sponge += old_height - full;
            }
            self.settling = false;
            self.lag_unset = false;
        }
    }

    /// Advances by `dt` seconds and returns the view to draw from. `stick` is the camera
    /// stick: x right, y up, each -1 to 1.
    pub fn step(
        &mut self,
        target: &Target,
        stick: Vec2,
        tuning: &Tuning,
        world: &dyn Sight,
        dt: f32,
    ) -> View {
        let weapon = target.aim.is_some() && !target.vehicle;
        if !self.started || target.vehicle != self.vehicle || weapon != self.weapon {
            self.start(target, tuning);
            self.before = self.now;
        }
        self.clock += dt;
        while self.clock >= UPDATE {
            self.clock -= UPDATE;
            // The update belongs to a moment `clock` seconds ago; put the target back where
            // it was then, or the camera sees him jump about by a frame's travel.
            let then = Target {
                position: target.position - target.motion * self.clock,
                ..*target
            };
            self.control_left = (self.control_left - UPDATE).max(0.0);
            // [game] 00922580: the dead-message bit overrides the requested mode,
            // including Climb and GroundPunch; it also bypasses stand-off refresh.
            let mode = if then.mode == Mode::Fast {
                Mode::Fast
            } else if then.climbing {
                Mode::Climb
            } else {
                then.mode
            };
            if mode == Mode::Climb && self.mode != Mode::Climb {
                self.recentre(&then);
            }
            self.mode = mode;
            let stick = self.options.stick(stick);
            if self.frozen && !self.vehicle && !self.weapon {
                let point =
                    then.position + Vec3::Z * (then.height + tuning.robot_camera.height_offset);
                if stick.length() < 0.01 && (point - self.last_target).truncate().length() < 0.01 {
                    self.fades.step(UPDATE);
                    continue;
                }
                self.frozen = false;
            }
            self.before = self.now;
            self.now = if self.vehicle {
                self.update_driving(&then, stick, &tuning.drive_camera, world)
            } else if self.weapon {
                self.update_weapon(&then, &tuning.weapon_camera, world)
            } else {
                self.update_follow(&then, stick, &tuning.robot_camera, world)
            };
            self.fades.step(UPDATE);
        }
        // Carried on from the last update at the rate it changed by, so the camera is where
        // the original's would be at this moment rather than an update behind him.
        let ahead = (self.clock / UPDATE).clamp(0.0, 1.0);
        View {
            eye: self.now.eye + (self.now.eye - self.before.eye) * ahead,
            forward: (self.now.forward + (self.now.forward - self.before.forward) * ahead)
                .normalize_or(self.now.forward),
            fov: self.now.fov + (self.now.fov - self.before.fov) * ahead,
            roll: self.now.roll + (self.now.roll - self.before.roll) * ahead,
        }
    }

    /// Turns the view by an angle instead of at a rate: radians, x to the right and y up,
    /// the same senses as the stick. It is applied whole on the next update, past the
    /// stick's turn rates. Not the original's (its mouse is a stick); kept for placing a
    /// capture's view.
    pub fn nudge(&mut self, turn: Vec2) {
        self.pending += turn;
    }

    /// The character's explicit request and its automatic request on entering Climb.
    /// Weapon cameras ignore this slot; driving clears its user elevation. [game]
    pub fn recentre(&mut self, target: &Target) {
        if self.weapon {
            return;
        }
        self.recentre = true;
        self.recentre_heading = heading_of(-target.facing.normalize_or(Vec2::Y));
    }

    /// Slot 13: hold an on-foot view until horizontal motion or stick input resumes. [game]
    pub fn freeze(&mut self) {
        self.frozen = true;
    }

    /// Control hand-over delay supplied by the previous behaviour's slot 12. [game]
    pub fn hold_control(&mut self, seconds: f32) {
        self.control_time = seconds.max(0.0);
        self.control_left = self.control_time;
    }

    fn control_fade(&self) -> f32 {
        if self.control_left == 0.0 {
            1.0
        } else if self.control_left < self.control_time * 0.5 {
            self.control_left / (self.control_time * 0.5)
        } else {
            0.0
        }
    }

    /// The view after the last whole update, without the blend.
    pub fn latest(&self) -> View {
        self.now
    }

    /// The game's slot0 reset: midpoint leash, default elevation, cleared offsets/rates.
    pub fn reset(&mut self, target: &Target, tuning: &Tuning) {
        let options = self.options;
        let settings = settings_for(target, tuning);
        let elevation = settings.default_elevation.to_radians();
        let at = target.position + Vec3::Z * (target.height + settings.height_offset);
        let away = (-target.facing.normalize_or(Vec2::Y) * elevation.cos()).extend(elevation.sin());
        let view = View {
            eye: at + away * ((settings.min_dist + settings.max_dist) * 0.5),
            forward: -away,
            fov: settings.fov,
            roll: 0.0,
        };
        *self = Self::from_view(view, target, tuning);
        self.options = options;
        self.settling = false;
        self.lag_unset = false;
    }

    fn effective_eye(&self, world: &dyn Sight, look_at: Vec3, eye: Vec3) -> Vec3 {
        let direction = (look_at - eye).normalize_or_zero();
        let mut centre = self.proxy_point + direction * 0.5; // [game] base camera near clip
        if let Some((min, max)) = world.bounds() {
            centre = centre.clamp(min + Vec3::ONE, max - Vec3::ONE);
        }
        let contacts = world.contacts(centre, 1.0);
        near_plane::adjust(
            eye,
            direction,
            self.fov,
            0.5,
            self.options.widescreen,
            &contacts,
        )
    }

    /// The look-at point: the target point, lagging behind when the target rises or drops.
    fn look_at(&mut self, target: &Target, settings: &CameraTuning) -> Vec3 {
        // On foot a new height (after a change of form) is reached in a straight line over
        // the camera's interpTargetHeightTime; the driving camera has no such blend.
        let full = target.height + settings.height_offset;
        self.height_left = (self.height_left - UPDATE).max(0.0);
        let height = if self.height_left > 0.0 && settings.interp_target_height_time > 0.0 {
            full + (self.height_from - full)
                * (self.height_left / settings.interp_target_height_time).min(1.0)
        } else {
            full
        };
        self.height = height;
        let aim = target.position + Vec3::Z * height;
        let carried = self.sponge - (aim.z - self.last_target.z);
        if aim.z != self.last_target.z || self.sponge.abs() > 0.001 {
            self.sponge_change = 0.2 * self.sponge_change - settings.min_height_gain * carried;
            self.sponge = (carried + self.sponge_change).clamp(-height, 20.0);
        }
        if self.shoulder.length() > 0.001 {
            self.shoulder *= 0.95;
        }
        self.last_target = aim;
        aim + self.shoulder.extend(self.sponge)
    }

    /// What the original keeps on its behaviour, for comparing with a recording: distance
    /// (`+0x104`), elevation (`+0x114`), the look-at point's offset (`+0x24..+0x2c`) and the
    /// extra distance (`+0x120`).
    pub fn internals(&self) -> (f32, f32, Vec3, f32) {
        (
            self.distance,
            self.elevation,
            self.shoulder.extend(self.sponge),
            self.extra,
        )
    }

    /// Selecting another distance settings object calls the behaviour's start while
    /// retaining the current view (00924310/00915740). [game]
    pub fn settings_changed(&mut self, target: &Target, tuning: &Tuning) {
        let sponge = self.shoulder;
        self.start(target, tuning);
        if self.weapon {
            self.shoulder = sponge;
        }
    }

    /// Puts the camera on a recorded view without starting it afresh, for replaying a
    /// recording: where it is, how far off and at what angles are taken from the view; its
    /// springs' rates, the car's speed smoothing and the stick's part are kept.
    pub fn reseat(&mut self, view: View) {
        let look = self.last_target + self.shoulder.extend(self.sponge);
        let line = view.eye - look;
        (self.now, self.before) = (view, view);
        self.position = view.eye;
        self.distance = line.length();
        self.pitch = view.forward.z.asin();
        self.heading = heading_of(Vec2::new(view.forward.x, view.forward.y));
        if self.vehicle {
            self.elevation = -self.pitch;
            self.behind = heading_of(-Vec2::new(view.forward.x, view.forward.y));
            self.roll = view.roll;
        } else {
            self.elevation = line.z.atan2(Vec2::new(line.x, line.y).length());
            self.user = self.elevation - self.auto;
            self.away = line.normalize_or(self.away);
        }
        (self.last_look, self.last_eye) = (look, view.eye);
        (self.blocked, self.cornered) = (false, false);
    }

    /// Whether the camera is looking past a corner, for the host's log.
    pub fn cornered(&self) -> bool {
        self.cornered
    }

    /// Whether an obstacle pulled the camera in on the last update, for the host's log.
    pub fn blocked(&self) -> bool {
        self.blocked
    }

    /// Takes the elevation from where the eye has been put.
    fn raise_to(&mut self, look_at: Vec3, eye: Vec3) {
        let line = eye - look_at;
        self.elevation = line.z.atan2(Vec2::new(line.x, line.y).length());
        self.user = self.elevation - self.auto;
    }

    /// While a corner is held, the eye is put on the line from the point looked at through
    /// the corner point, as far away as it would have been. The corner is let go when the
    /// wall is no longer between them, when the corner is further off than the eye, when the
    /// stick turns the view away from it, or when the straight line has come round to the
    /// open side of the corner point.
    fn hold_corner(&mut self, look_at: Vec3, eye: Vec3) -> Vec3 {
        if !self.cornered {
            return eye;
        }
        let line = eye - look_at;
        let front = self.corner.normal.dot(look_at - self.corner.point);
        let behind = self.corner.normal.dot(eye - self.corner.point);
        if front * behind < 0.01 && (front - behind).abs() > 1e-6 {
            self.corner.slide(look_at, eye, self.elevation);
            let to_corner = self.corner.point - look_at;
            if to_corner.length_squared() < line.length_squared()
                && not_turning_away(self.yaw_applied, flat_of(to_corner), flat_of(-line))
                && not_turning_away(self.pitch_applied, rise_of(to_corner), rise_of(-line))
            {
                let crossing = look_at + line * (front / (front - behind));
                if self.corner.side.dot(crossing - self.corner.point) < 0.0 {
                    let swung = look_at + to_corner.normalize_or(Vec3::Z) * line.length();
                    self.raise_to(look_at, swung);
                    return swung;
                }
            }
        }
        self.cornered = false;
        eye
    }

    /// The follow camera against the world, with the eye where the update wants it.
    ///
    /// Nothing in the way: the eye is used. Something in the way: the eye is pulled in to it,
    /// and from then on the camera really is there (the next update measures from the real
    /// camera, so the distance eases back out through its spring rather than jumping). But
    /// if the last update's line was clear, passed the surface now hit, and the hit is much
    /// nearer, he has gone round a corner: the camera swings to look past the point where
    /// the old line crossed the surface, and the check runs once more from there.
    fn collide(&mut self, world: &dyn Sight, look_at: Vec3, eye: Vec3, again: bool) -> Vec3 {
        for id in world.fade_hits(look_at, eye, 0.0) {
            self.fades.touch(id);
        }
        self.blocked = false;
        let line = eye - look_at;
        let full = line.length();
        if full < 1e-4 {
            return eye;
        }
        let along = line / full;
        let Some(hit) = world.sight(look_at, eye) else {
            self.last_look = look_at;
            self.last_eye = eye;
            self.proxy_point = eye - along * 0.2;
            return eye;
        };
        let reach = (hit.point - look_at).dot(along);
        if reach < TOO_NEAR {
            // Too close to tell anything: the camera stays put and its view settles afresh.
            self.last_look = look_at;
            self.lag_unset = true;
            self.proxy_point = self.last_eye;
            return self.last_eye;
        }
        let old = self.last_eye - self.last_look;
        let behind = hit.normal.dot(self.last_eye - hit.point);
        if !again
            && (!self.cornered || (self.corner.point - look_at).length() > reach)
            && old.length() > 1.0
            && reach < old.length() - 1.0
            && behind < -0.1
        {
            let front = hit.normal.dot(self.last_look - hit.point);
            if front * behind < 0.01 {
                let point =
                    self.last_look + old * (front / (front - behind)) + hit.normal * CORNER_CLEAR;
                let side = (point - hit.point).normalize_or_zero();
                let edge = match hit.shape {
                    Some(Shape::Box { min, max }) => {
                        geometry::edges(&geometry::box_vertices(min, max))
                            .into_iter()
                            .min_by(|(a, b), (c, d)| {
                                let distance = |a: Vec3, b: Vec3| {
                                    let v = b - a;
                                    (point
                                        - a
                                        - v * ((point - a).dot(v) / v.length_squared().max(1e-12)))
                                    .length_squared()
                                };
                                distance(*a, *b).total_cmp(&distance(*c, *d))
                            })
                            .map(|(a, b)| (a, b - a))
                    }
                    Some(Shape::Axis {
                        origin, direction, ..
                    }) => Some((origin, direction)),
                    None => None,
                };
                let mut distance = edge.map_or(0.0, |(a, v)| {
                    (point - a - v * ((point - a).dot(v) / v.length_squared().max(1e-12))).length()
                });
                if let Some(Shape::Axis { radius, .. }) = hit.shape {
                    distance = distance.max(radius);
                }
                if let Some((a, v)) = edge {
                    let perpendicular =
                        point - a - v * ((point - a).dot(v) / v.length_squared().max(1e-12));
                    if v.cross(look_at - eye).dot(perpendicular) < 0.0 {
                        distance = -distance;
                    }
                }
                self.corner = Corner {
                    point,
                    normal: hit.normal,
                    side,
                    edge,
                    distance,
                    axis: matches!(hit.shape, Some(Shape::Axis { .. })),
                };
                self.corner.slide(look_at, eye, self.elevation);
                let to_corner = self.corner.point - look_at;
                if not_turning_away(self.yaw_applied, flat_of(to_corner), flat_of(-line))
                    && not_turning_away(self.pitch_applied, rise_of(to_corner), rise_of(-line))
                {
                    self.cornered = true;
                    let swung = look_at + to_corner.normalize_or(Vec3::Z) * full;
                    self.raise_to(look_at, swung);
                    return self.collide(world, look_at, swung, true);
                }
            }
        }
        self.blocked = true;
        self.last_look = look_at;
        self.last_eye = look_at + along * reach.clamp(0.0, full);
        self.proxy_point = if hit.normal.dot(-along) <= 0.25 {
            self.last_eye + hit.normal * 0.1
        } else {
            self.last_eye - along * 0.2
        };
        self.last_eye
    }

    /// The driving camera against the world: only pulled in, with no memory of it, so it is
    /// back at its distance as soon as the way is clear. [game] Sphere radius 0.65.
    fn pulled_in(&mut self, world: &dyn Sight, look_at: Vec3, eye: Vec3) -> Vec3 {
        for id in world.fade_hits(look_at, eye, CAR_SWEEP) {
            self.fades.touch(id);
        }
        self.blocked = false;
        let line = eye - look_at;
        let full = line.length();
        if full < 1e-4 {
            return eye;
        }
        let along = line / full;
        match world.sweep(look_at, eye, CAR_SWEEP) {
            None => eye,
            Some(hit) => {
                let reach = (hit.point - look_at).dot(along);
                let distance = hit.distance.unwrap_or(reach);
                self.blocked = distance >= TOO_NEAR;
                look_at
                    + along
                        * if distance < TOO_NEAR {
                            0.0
                        } else {
                            reach.min(full)
                        }
            }
        }
    }

    fn update_follow(
        &mut self,
        target: &Target,
        stick: Vec2,
        s: &CameraTuning,
        world: &dyn Sight,
    ) -> View {
        // Field of view.
        self.fov_rate = spring(self.wanted_fov - self.fov, self.fov_rate, 0.02);
        self.fov += self.fov_rate;
        self.wanted_fov = s.fov;

        let was = self.last_target;
        let mut look_at = self.look_at(target, s);
        // The camera stays where it was; the target moving is what changes the offset. So he
        // drags it along at the far limit and pushes it at the near one. On a wall it is
        // different: the camera goes wherever he goes (`+0xdb`, set while the character's
        // camera mode is the climbing one, 3efeac6d), and only the point looked at lags. [game]
        if target.climbing {
            self.position += self.last_target - was;
        }
        let offset = self.position - look_at;
        let turn_scale = (s.scale_rot_speed_min
            + (s.scale_rot_speed_max - s.scale_rot_speed_min)
                * self.options.sensitivity.clamp(0.0, 1.0))
            * 0.01;
        let mut heading = heading_of(Vec2::new(offset.x, offset.y));
        // The overhead rule (`FUN_00924310`, `notes\camera.md` step 6): when the last update's
        // direction from the point looked at to the camera pointed up by more than 0.83 and
        // the target has moved, the heading is blended back toward the last update's, all of
        // the way at 0.88, so the view does not spin as he passes under it. [game]
        let moved = (self.last_target - was).truncate().length_squared() > OVERHEAD_MOVED_SQUARED;
        let up = (self.last_eye - self.last_look).normalize_or_zero().z;
        if moved && up > OVERHEAD_FROM {
            let blend = ((up - OVERHEAD_FROM) / (OVERHEAD_FULL - OVERHEAD_FROM)).clamp(0.0, 1.0);
            heading += wrap(self.last_heading - heading) * blend;
        }
        let nudge = std::mem::take(&mut self.pending);
        self.yaw_applied =
            -stick.x * self.control_fade() * s.user_rot_speed_z.to_radians() * turn_scale * UPDATE
                - nudge.x;
        let mut turn = self.yaw_applied;
        if self.recentre {
            let requested = wrap(self.recentre_heading - heading);
            let current = wrap(heading_of(-target.facing.normalize_or(Vec2::Y)) - heading);
            let off = if requested * current > 0.0 && current.abs() < requested.abs() {
                current
            } else {
                requested
            };
            let pace = std::f32::consts::PI * UPDATE;
            turn = if off.abs() < pace {
                self.recentre = false;
                off
            } else {
                pace.copysign(off) + 0.1 * (off - pace.copysign(off))
            };
        } else if target.climbing {
            // On a wall the view may not come round his side (`FUN_00922d10`): past 0.35 of
            // a half turn from behind him a push further counts for less, for nothing at
            // 0.4, and beyond that it is taken back by a tenth of the excess an update. [game]
            let (soft, hard) = (0.35 * std::f32::consts::PI, 0.4 * std::f32::consts::PI);
            let round = wrap(heading - heading_of(-target.facing.normalize_or(Vec2::Y)));
            if (round > soft && turn > 0.0) || (round < -soft && turn < 0.0) {
                turn = if round.abs() > hard {
                    -0.1 * (round - hard.copysign(round))
                } else {
                    turn * (1.0 - (round.abs() - soft) / (hard - soft))
                };
            }
        }
        heading += turn;
        self.last_heading = heading;

        // Extra distance (`FUN_009228d0`): the camera's faceExtraDist while he faces the
        // camera or the stand-off after a melee attack runs, 15 while he climbs. A camera
        // with neither a faceExtraDist nor a climbExtraDist (his two further levels) and no
        // stand-off running leaves it alone. Its change is taken with the measured length,
        // so the camera moves out and in with it at once rather than waiting to be pushed
        // by the leash (`+0x138`). [game]
        if target.fighting && self.mode != Mode::Fast {
            self.hold = STAND_OFF;
        }
        let turned = direction(heading);
        let mut bias = 0.0;
        if s.face_extra_dist > 0.01 || s.climb_extra_dist > 0.01 || self.hold > UPDATE {
            let wanted_extra =
                if target.climbing || matches!(self.mode, Mode::Fast | Mode::GroundPunch) {
                    CLIMB_EXTRA
                } else if target.facing.dot(turned) > 0.0 || self.hold > UPDATE {
                    s.face_extra_dist
                } else {
                    0.0
                };
            if self.mode == Mode::GroundPunch {
                self.wanted_fov = (s.fov * 1.5).min(70.0);
            }
            let factor = if self.mode == Mode::Fast { 2.0 } else { 1.0 };
            self.extra_rate = spring(
                wanted_extra - self.extra,
                self.extra_rate,
                s.face_extra_dist_gain * factor,
            )
            .clamp(-10.0 * factor * UPDATE, 10.0 * factor * UPDATE);
            self.extra += self.extra_rate;
            bias = self.extra_rate;
        }
        self.hold = (self.hold - UPDATE).max(0.0);
        let wanted = (offset.length() + bias + std::mem::take(&mut self.distance_bias))
            .clamp(s.min_dist + self.extra, s.max_dist + self.extra);
        if self.blocked && offset.length() < self.distance {
            // Shortened by an obstacle last update: the distance is what is left, at once.
            self.distance = offset.length() + 0.2;
            self.distance_step = 0.0;
        } else {
            self.distance_step = spring(wanted - self.distance, self.distance_step, 0.02);
            self.distance += self.distance_step;
        }

        // Elevation: the stick moves the user's part directly; left alone it settles to the
        // default; a second part keeps the sum inside the allowed range.
        let (low, high) = (s.min_elevation.to_radians(), s.max_elevation.to_radians());
        let push =
            -stick.y * self.control_fade() * s.user_rot_speed_x.to_radians() * turn_scale * UPDATE
                - nudge.y;
        self.pitch_applied = 0.0;
        if push.abs() > 2.0f32.to_radians() * UPDATE {
            if !((self.user + push < low && push <= 0.0)
                || (self.user + push > high && push >= 0.0))
            {
                self.pitch_applied = -push;
                self.user = (self.user + push).clamp(low, high);
                self.user_rate = 0.0;
                self.settling = false;
            }
        } else if self.settling {
            self.user_rate = spring(
                s.default_elevation.to_radians().clamp(low, high) - self.user,
                self.user_rate,
                0.02,
            );
            self.user += self.user_rate;
        }
        let offset = if self.mode == Mode::GroundPunch {
            std::f32::consts::FRAC_PI_4
        } else {
            0.0
        };
        let wanted_auto = (self.user + offset).clamp(low, high) - self.user;
        self.auto_rate = spring(wanted_auto - self.auto, self.auto_rate, 0.03);
        self.auto += self.auto_rate;
        self.elevation = (self.user + self.auto).clamp(low, high);
        self.auto = self.elevation - self.user;

        let wanted_eye = look_at
            + (direction(heading) * self.elevation.cos()).extend(self.elevation.sin())
                * self.distance;
        let mut eye = self.hold_corner(look_at, wanted_eye);
        // Under a ceiling the point looked at is kept 1 below it (a line from the middle of
        // his body up to 1 above the point), and the camera comes down with it.
        let middle = target.position.z + target.height * 0.5;
        if look_at.z - middle > -1.0 {
            if let Some(hit) =
                world.sight(Vec3::new(look_at.x, look_at.y, middle), look_at + Vec3::Z)
            {
                let drop = hit.point.z - 1.0 - look_at.z;
                if drop < 0.0 {
                    look_at.z += drop;
                    eye.z += drop;
                    self.sponge += drop;
                    self.sponge_change = self.sponge_change.min(0.0);
                }
            }
        }
        let eye = self.collide(world, look_at, eye, false);
        let eye = self.effective_eye(world, look_at, eye);
        self.position = eye;

        // The view turns to the look-at point with a lag that is sprung away.
        let line = look_at - eye;
        let wanted_pitch = line.z.atan2(Vec2::new(line.x, line.y).length());
        let wanted_heading = heading_of(Vec2::new(line.x, line.y));
        // The lag is only ever set when the camera takes over, from where the view pointed
        // then; after that it is sprung away and nothing feeds it. So turning the camera with
        // the stick keeps him in the middle of the picture (the recording shows the lag at
        // zero all through stick turns).
        if std::mem::take(&mut self.lag_unset) {
            self.lag = Vec2::new(
                self.pitch - wanted_pitch,
                wrap(self.heading - wanted_heading),
            );
        }
        self.lag_rate.x = spring(-self.lag.x, self.lag_rate.x, s.interp_elev_gain);
        self.lag_rate.y = spring(-self.lag.y, self.lag_rate.y, s.interp_ang_gain);
        self.lag += self.lag_rate;
        self.pitch = wanted_pitch + self.lag.x;
        self.heading = wanted_heading + self.lag.y;
        let forward = (direction(self.heading) * self.pitch.cos()).extend(self.pitch.sin());
        View {
            eye,
            forward,
            fov: self.fov,
            roll: 0.0,
        }
    }

    /// The weapon camera (`FUN_00929470`; `notes\camera.md`, "The weapon camera, step by
    /// step"). It never reads the stick: the stick turns his aim (`aim.rs`), and this
    /// follows the aim. Its heading snaps to behind the aim unless it is far off, when it
    /// swings round; its elevation trails the aim's on a spring; its distance is the
    /// follow camera's leash; and the point it turns about swings on his radius as he
    /// aims up or down.
    fn update_weapon(&mut self, target: &Target, s: &CameraTuning, world: &dyn Sight) -> View {
        let mut aim = target.aim.unwrap_or(target.facing.extend(0.0));
        if aim.truncate().length_squared() < 0.0001 {
            aim.x = self.aim.x;
            aim.y = self.aim.y;
        }
        self.aim = aim;
        self.fov_rate = spring(s.fov - self.fov, self.fov_rate, 0.02);
        self.fov += self.fov_rate;

        // The point on his axis. No blend to a new height here, and its rise is not taken
        // out of the offset: what the start left there is only sprung away. [game]
        let height = target.height + s.height_offset;
        self.height = height;
        let on_axis = target.position + Vec3::Z * height;
        if on_axis.z != self.last_target.z || self.sponge.abs() > 0.001 {
            self.sponge_change = spring(-self.sponge, self.sponge_change, s.min_height_gain);
            self.sponge = (self.sponge + self.sponge_change).clamp(-height, 20.0);
        }
        if target.climbing {
            self.position += on_axis - self.last_target;
        }
        self.last_target = on_axis;

        // The point the camera turns about (`FUN_009284e0`), kept in the aim's frame: to
        // his side by sideOffset and ahead by aheadOffset. A camera with next to neither
        // (Bumblebee's: both 0) has it swing on his radius instead: back as he aims up,
        // forward as he aims down, and the floor below comes up by the same arc.
        let ahead = Vec2::new(aim.x, aim.y).normalize_or(target.facing.normalize_or(Vec2::Y));
        let side = Vec2::new(ahead.y, -ahead.x);
        let at = Vec2::new(self.shoulder.dot(side), self.shoulder.dot(ahead));
        let mut wanted = Vec2::new(s.side_offset, s.ahead_offset);
        let mut floor_height = height;
        if s.ahead_offset.abs() < 0.1 && target.radius * 0.25 > s.side_offset {
            let unit = aim.normalize_or(ahead.extend(0.0));
            wanted.y = -unit.z * target.radius;
            floor_height -= target.radius * (1.0 - Vec2::new(unit.x, unit.y).length());
        }
        let off = wanted - at;
        let last = self.shoulder_rate * UPDATE;
        let mut push = Vec2::new(spring(off.x, last.x, 0.5), spring(off.y, last.y, 0.5)) / UPDATE
            - self.shoulder_rate;
        // Its speed may not rise by more than 50 in an update.
        if push.length_squared() > 2500.0
            && (self.shoulder_rate.length_squared() < 1e-4 || push.dot(off) > 0.0)
        {
            push *= 50.0 / push.length();
        }
        self.shoulder_rate += push;
        let mut at = at + self.shoulder_rate * UPDATE;
        if s.side_offset.abs() > target.radius - 0.7 && s.height_offset < 0.0 {
            let from = on_axis + Vec3::Z * s.height_offset;
            if let Some(hit) = world.sight(from, from + (side * (at.x + 0.7)).extend(0.0)) {
                at.x = (hit.point - from).truncate().dot(side) - 0.7;
            }
        }
        self.shoulder = side * at.x + ahead * at.y;
        let centre = on_axis + self.shoulder.extend(self.sponge);

        // Heading (slot 18, `FUN_00928c60`): to behind the aim at once. Further off than
        // maxRotSpd allows in an update, it swings instead: half a turn a second plus a
        // tenth of what is left, until one such step would reach.
        let flat = Vec2::new(self.away.x, self.away.y);
        let behind = wrap(heading_of(-ahead) - heading_of(flat));
        let mut turn = behind;
        if behind.abs() > s.max_rot_speed.to_radians() * UPDATE {
            self.swinging = true;
        }
        if self.swinging {
            let pace = std::f32::consts::PI * UPDATE;
            if pace > behind.abs() {
                self.swinging = false;
            } else {
                turn = wrap(behind.signum() * pace + (behind - behind.signum() * pace) * 0.1);
            }
        }
        let heading = heading_of(flat) + turn;
        self.away = (direction(heading) * flat.length()).extend(self.away.z);

        // Distance (`FUN_00929280`): the leash between minDist and maxDist, measured from
        // where the camera was; and never so far that the eye comes within 1 of his feet.
        self.distance_bias = 0.0;
        let leash = (self.position - centre)
            .length()
            .clamp(s.min_dist, s.max_dist);
        self.distance_step = spring(leash - self.distance, self.distance_step, 0.02);
        self.distance += self.distance_step;
        if self.away.z * self.distance < 1.0 - floor_height {
            self.distance = (1.0 - floor_height) / self.away.z;
        }

        // Elevation (slot 20, `FUN_00928f40`): that of minus the aim, trailed by a lag
        // which is set when the camera takes over and sprung away with elevGain.
        let now = self
            .away
            .z
            .atan2(Vec2::new(self.away.x, self.away.y).length());
        let of_aim = (-aim.z).atan2(Vec2::new(aim.x, aim.y).length());
        if std::mem::take(&mut self.elevation_lag_unset) {
            self.elevation_lag = now - of_aim;
        }
        self.elevation_lag_change =
            spring(-self.elevation_lag, self.elevation_lag_change, s.elev_gain);
        self.elevation_lag += self.elevation_lag_change;
        let change = wrap(self.elevation_lag + of_aim - now);
        let mut eye = centre + self.away * self.distance;
        if change.abs() > 1.745e-7 {
            self.elevation = now + change;
            self.away = (direction(heading) * self.elevation.cos()).extend(self.elevation.sin());
            eye = centre + self.away * self.distance;
        } else {
            eye.z = eye.z.max(centre.z - floor_height + 1.0);
        }
        // [game] The distance routine clears this at its next update; retained on the
        // behaviour rather than inventing a transfer to a different follow object.
        self.distance_bias = if self.elevation < (-10.0_f32).to_radians() {
            -s.max_dist.max(0.0)
        } else {
            0.0
        };

        // The world is asked from the point on his axis, not from the point beside him.
        (self.yaw_applied, self.pitch_applied) = (0.0, 0.0);
        let axis = on_axis + Vec3::Z * self.sponge;
        let eye = self.collide(world, axis, eye, false);
        let eye = self.effective_eye(world, axis, eye);
        self.position = eye;
        let forward = (centre - eye).normalize_or(-self.away);
        self.pitch = forward.z.asin();
        self.heading = heading_of(Vec2::new(forward.x, forward.y));
        View {
            eye,
            forward,
            fov: self.fov,
            roll: 0.0,
        }
    }

    fn update_driving(
        &mut self,
        target: &Target,
        stick: Vec2,
        s: &CameraTuning,
        world: &dyn Sight,
    ) -> View {
        // [game] FOV springs to the previous update's requested FOV before heading
        // computes the new one (00915740 then00914c90).
        self.fov_rate = spring(self.wanted_fov - self.fov, self.fov_rate, 0.02);
        self.fov += self.fov_rate;
        // Smoothed speed, and from it how far into "top speed" the car is (for Bumblebee
        // that is only in turbo).
        let speed = Vec2::new(target.velocity.x, target.velocity.y).length();
        self.smooth_rate = 0.9 * self.smooth_rate + 0.01 * (speed - self.smooth_speed);
        self.smooth_speed += self.smooth_rate;
        let span = (s.max_speed_align_move - s.min_speed_align_move).max(1e-3);
        let wanted_factor = ((self.smooth_speed - s.min_speed_align_move) / span).clamp(0.0, 1.0);
        self.speed_factor += 0.15 * (wanted_factor - self.speed_factor);
        let f = self.speed_factor;

        // Field of view widens with speed.
        let wanted_fov = s.fov * (1.0 + (s.max_speed_fov_scale * 0.01 - 1.0) * f);
        self.wanted_fov = wanted_fov;

        // The point the camera hangs from: the point on the car, lagging its rises and
        // drops. Neither the eye nor the point the world is asked from goes under a floor
        // (`FUN_00915740`: the height of the object from `FUN_00923a70`), which the
        // recording has 0.5 above the point on the car all through. [game; 0.5 trace]
        let mut hang = self.look_at(target, s);
        let right = Vec2::new(target.facing.y, -target.facing.x);
        self.shoulder += (right * s.side_offset - self.shoulder).clamp_length_max(5.0 * UPDATE);
        hang.x = target.position.x + self.shoulder.x;
        hang.y = target.position.y + self.shoulder.y;
        let floor = target
            .body_height
            .map_or(hang.z - self.sponge + DRIVE_FLOOR, |h| {
                target.position.z + h
            });
        let mut look_at = hang;
        look_at.z = look_at.z.max(floor);
        self.height = hang.z - self.sponge - target.position.z;

        // Heading: the stick turns it, and it is pulled back to behind the car.
        let nudge = std::mem::take(&mut self.pending);
        let mut user = -stick.x
            * self.control_fade()
            * s.user_rot_speed_z.to_radians()
            * (1.0 - f + s.user_rot_scale_max_speed * 0.01 * f)
            * UPDATE
            - nudge.x;
        let wanted_behind = heading_of(-target.facing.normalize_or(Vec2::Y));
        let off = wrap(wanted_behind - self.behind);
        // More than a quarter turn off "behind the car", a push further round counts for
        // less and less, and for nothing at 135 degrees (`FUN_00914c90`). That, against the
        // pull back, is what holds the view to the car's side while the stick is held over
        // (the recording: about 105 degrees round at full stick). [game; the constant for
        // the other side is filled in at run time and taken to be minus a quarter turn]
        let quarter = std::f32::consts::FRAC_PI_2;
        if (off > quarter && user < 0.0) || (off < -quarter && user > 0.0) {
            let past = off.abs() / std::f32::consts::PI - 0.5;
            user *= if past < 0.25 { 1.0 - 4.0 * past } else { 0.0 };
        }
        if off.abs() > 1e-6 || user.abs() > 1e-6 {
            self.align_step = s.align_gain * off + (1.0 - s.align_damp) * self.align_step;
            self.user_step = s.user_rot_gain * user + (1.0 - s.user_rot_damp) * self.user_step;
            self.behind = wrap(self.behind + self.align_step + self.user_step);
        }

        // The picture leans a part of the way the car leans (`leanAmount`, a quarter for
        // him), on a spring. [game]
        self.roll_change = spring(
            s.lean_amount * 0.01 * target.roll - self.roll,
            self.roll_change,
            s.lean_gain,
        );
        self.roll += self.roll_change;

        // [game] Distance first undoes the previous FOV ratio on the measured view
        // length. Elevation then rebuilds the final eye from the UNSCALED distance.
        let ratio = s.fov.to_radians().sin() / wanted_fov.to_radians().sin();
        let measured = (self.position - hang).length() * self.fov_scale.to_radians().sin()
            / s.fov.to_radians().sin();
        let add = 0.9 * self.distance_add + 0.1 * s.max_speed_dist_add * f;
        let bias = add - self.distance_add;
        self.distance_add = add;
        self.fov_scale = wanted_fov;
        let wanted = (measured + bias).clamp(s.min_dist + add, s.max_dist + add);
        self.distance_step = spring(wanted - self.distance, self.distance_step, 0.02);
        self.distance += self.distance_step;

        // Elevation (`FUN_009145b0`): the default, plus the car's own tilt (`tiltAmount`,
        // all of it for him: nose up brings the camera down behind it), plus what the stick
        // has added, kept in the camera's range (the stick's part is trimmed to fit), and
        // reached on a spring. [game]
        let (low, high) = (s.min_elevation.to_radians(), s.max_elevation.to_radians());
        let push =
            -stick.y * self.control_fade() * s.user_rot_speed_x.to_radians() * UPDATE - nudge.y;
        self.user_elevation += push;
        if push == 0.0 && target.flying {
            self.user_elevation *= 0.5;
        }
        let velocity = target.motion;
        let speed = velocity.truncate().length();
        self.airborne = speed > 5.0
            && self.height_left <= 0.0
            && self.mode != Mode::Transition
            && (target.ground_distance > 2.0 || (self.airborne && target.ground_distance > 0.1));
        let mut tilt = -s.tilt_amount * 0.01 * target.pitch;
        if self.airborne {
            let velocity_pitch = (-velocity.z).atan2(speed) - std::f32::consts::FRAC_PI_8;
            let from = self.elevation.max(tilt);
            tilt = tilt.max(from + (velocity_pitch - from) * 0.25);
        }
        let base = s.default_elevation.to_radians() + tilt;
        if !self.airborne && base + self.user_elevation > high {
            self.user_elevation = self.user_elevation.max(0.0).min(high - base);
        } else if !self.airborne && base + self.user_elevation < low {
            self.user_elevation = self.user_elevation.max(low - base).min(0.0);
        }
        let wanted_elevation = (base + self.user_elevation).clamp(low, high);
        if self.recentre {
            self.recentre = false;
            self.user_elevation = 0.0;
        }
        self.elevation_rate = spring(
            wrap(wanted_elevation - self.elevation),
            self.elevation_rate,
            s.tilt_gain,
        );
        self.elevation += self.elevation_rate;

        // The eye: out along that line, but never lower over the point than the car's own
        // nose-down slope would put it, nor under the floor. The view does NOT turn to the
        // car when the eye is held up so: it keeps the line's direction, so pushing the
        // camera down past the floor looks up over the car (the recording, t 448.6). [game]
        let away = (direction(self.behind) * self.elevation.cos()).extend(self.elevation.sin());
        let scale = if target.flying && self.elevation_rate.abs() <= 1.7453293e-7 {
            ratio
        } else {
            1.0
        };
        let rise = if target.flying {
            self.elevation.sin() * self.distance * scale
        } else {
            (self.elevation.sin().max(-target.pitch.sin()) * self.distance).max(floor - hang.z)
        };
        let flat = direction(self.behind) * self.elevation.cos() * self.distance * scale;
        let wanted_eye = Vec3::new(hang.x + flat.x, hang.y + flat.y, hang.z + rise);
        let eye = self.pulled_in(world, look_at, wanted_eye);
        self.position = eye;
        let forward = -away;
        self.pitch = forward.z.asin();
        self.heading = heading_of(Vec2::new(forward.x, forward.y));
        View {
            eye,
            forward,
            fov: self.fov,
            roll: self.roll,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn plane_sweep(from: Vec3, to: Vec3, normal: Vec3, offset: f32, radius: f32) -> Option<Hit> {
        let (a, b) = (
            normal.dot(from) - offset - radius,
            normal.dot(to) - offset - radius,
        );
        if a < 0.0 {
            Some(Hit {
                point: from,
                normal,
                ..Default::default()
            })
        } else if b < 0.0 {
            Some(Hit {
                point: from + (to - from) * (a / (a - b)),
                normal,
                ..Default::default()
            })
        } else {
            None
        }
    }

    /// Bumblebee's near follow camera and far driving camera, as the game's data has them.
    fn tuning() -> Tuning {
        let shared = CameraTuning {
            fov: 45.0,
            user_rot_speed_z: 180.0,
            user_rot_speed_x: 180.0,
            scale_rot_speed_min: 80.0,
            scale_rot_speed_max: 120.0,
            face_extra_dist_gain: 0.5,
            interp_elev_gain: 0.05,
            interp_ang_gain: 0.075,
            interp_target_height_time: 0.5,
            max_speed_fov_scale: 100.0,
            user_rot_scale_max_speed: 100.0,
            ..Default::default()
        };
        Tuning {
            robot_camera: CameraTuning {
                max_dist: 11.0,
                min_dist: 9.0,
                face_extra_dist: 10.0,
                min_elevation: -60.0,
                default_elevation: 8.0,
                max_elevation: 65.0,
                min_height_gain: 0.6,
                ..shared.clone()
            },
            drive_camera: CameraTuning {
                max_dist: 14.0,
                min_dist: 12.0,
                min_elevation: -8.0,
                default_elevation: 10.0,
                max_elevation: 35.0,
                user_rot_speed_z: 160.0,
                user_rot_speed_x: 100.0,
                min_height_gain: 0.275,
                height_offset: -1.0,
                min_speed_align_move: 35.0,
                max_speed_align_move: 43.0,
                align_gain: 0.5,
                align_damp: 0.9,
                max_speed_fov_scale: 145.0,
                user_rot_gain: 2.65,
                user_rot_damp: 0.15,
                tilt_gain: 0.5,
                ..shared.clone()
            },
            // `bbeeWepshoulderCamera`.
            weapon_camera: CameraTuning {
                max_dist: 11.0,
                min_dist: 9.0,
                min_elevation: -60.0,
                default_elevation: 15.0,
                max_elevation: 65.0,
                min_height_gain: 0.5,
                interp_target_height_time: 0.0,
                max_rot_speed: 720.0,
                elev_gain: 0.5,
                ..shared
            },
            ..Default::default()
        }
    }

    /// Him in weapon mode, aiming along `aim`: the point looked at is 0.8 higher.
    fn aiming(aim: Vec3) -> Target {
        Target {
            height: 6.2,
            aim: Some(aim),
            ..robot(Vec3::ZERO, Vec3::ZERO)
        }
    }

    #[test]
    fn changing_distance_settings_keeps_the_view_then_eases_to_the_new_leash() {
        let mut t = tuning();
        let target = robot(Vec3::ZERO, Vec3::ZERO);
        let mut camera = Camera::behind(&target, &t);
        for _ in 0..100 {
            camera.step(&target, Vec2::ZERO, &t, &Open, UPDATE);
        }
        let before = camera.latest();
        t.robot_camera.min_dist = 19.0;
        t.robot_camera.max_dist = 21.0;
        camera.settings_changed(&target, &t);
        assert_eq!(
            camera.latest().eye,
            before.eye,
            "selection itself must not cut"
        );
        camera.step(&target, Vec2::ZERO, &t, &Open, UPDATE);
        assert!(
            (camera.latest().eye - before.eye).length() < 1.0,
            "first step must ease"
        );
        for _ in 0..200 {
            camera.step(&target, Vec2::ZERO, &t, &Open, UPDATE);
        }
        assert!(camera.internals().0 >= 18.99);
    }

    #[test]
    fn the_weapon_camera_takes_over_without_a_cut_and_sits_behind_the_aim() {
        let t = tuning();
        let standing = robot(Vec3::ZERO, Vec3::ZERO);
        let mut camera = Camera::behind(&standing, &t);
        for _ in 0..200 {
            camera.step(&standing, Vec2::ZERO, &t, &Open, UPDATE);
        }
        let before = camera.latest();
        // He aims the way the camera looks, as on entering weapon mode.
        let target = aiming(before.forward);
        camera.step(&target, Vec2::ZERO, &t, &Open, UPDATE);
        let after = camera.latest();
        // The point looked at starts on the old view line, 0.8 under its new height, and is
        // sprung up with a gain of a half: 0.4 on the first update.
        assert!(
            (after.eye - before.eye).length() < 0.45,
            "the eye jumped {}",
            (after.eye - before.eye).length()
        );
        assert!(
            after.forward.dot(before.forward) > 0.9995,
            "the view turned {:?} to {:?}",
            before.forward,
            after.forward
        );
        for _ in 0..300 {
            camera.step(&target, Vec2::new(1.0, 1.0), &t, &Open, UPDATE);
        }
        // The stick does nothing to it: it looks along the aim from the leash's length.
        let settled = camera.latest();
        assert!(
            settled.forward.dot(before.forward) > 0.9999,
            "{:?}",
            settled.forward
        );
        assert!(
            (9.0..=11.0).contains(&camera.distance),
            "{}",
            camera.distance
        );
    }

    #[test]
    fn the_weapon_camera_snaps_to_a_small_turn_and_swings_round_a_large_one() {
        let t = tuning();
        let mut camera = Camera::behind(&aiming(Vec3::Y), &t);
        for _ in 0..100 {
            camera.step(&aiming(Vec3::Y), Vec2::ZERO, &t, &Open, UPDATE);
        }
        let heading =
            |c: &Camera| heading_of(Vec2::new(c.latest().forward.x, c.latest().forward.y));
        // 10 degrees is inside 720 a second for one update (23): taken at once.
        let small = direction(10f32.to_radians());
        camera.step(&aiming(small.extend(0.0)), Vec2::ZERO, &t, &Open, UPDATE);
        assert!(!camera.swinging);
        assert!(
            (heading(&camera) - 10f32.to_radians()).abs() < 1e-3,
            "{}",
            heading(&camera).to_degrees()
        );
        // Half a turn away: pi a second plus a tenth of the rest, 5.76 + 16.4 degrees.
        let far = direction(170f32.to_radians());
        camera.step(&aiming(far.extend(0.0)), Vec2::ZERO, &t, &Open, UPDATE);
        assert!(camera.swinging);
        let first = wrap(heading(&camera) - 10f32.to_radians()).to_degrees();
        assert!(
            (first - (5.76 + 0.1 * (160.0 - 5.76))).abs() < 0.3,
            "{first}"
        );
        for _ in 0..60 {
            camera.step(&aiming(far.extend(0.0)), Vec2::ZERO, &t, &Open, UPDATE);
        }
        assert!(!camera.swinging);
        assert!((heading(&camera) - 170f32.to_radians()).abs() < 1e-3);
    }

    #[test]
    fn aiming_up_brings_the_weapon_camera_down_behind_him_and_its_point_back() {
        let t = tuning();
        let mut camera = Camera::behind(&aiming(Vec3::Y), &t);
        for _ in 0..200 {
            camera.step(&aiming(Vec3::Y), Vec2::ZERO, &t, &Open, UPDATE);
        }
        let level = camera.latest();
        // 30 degrees up: the elevation is the aim's at once. Its lag is only what the
        // take-over left, long sprung away; what smooths aiming is the aim's own ramp.
        let up = Vec3::new(0.0, 30f32.to_radians().cos(), 30f32.to_radians().sin());
        camera.step(&aiming(up), Vec2::ZERO, &t, &Open, UPDATE);
        let first = camera.latest().forward.z.asin().to_degrees();
        assert!((first - 30.0).abs() < 0.01, "{first}");
        for _ in 0..200 {
            camera.step(&aiming(up), Vec2::ZERO, &t, &Open, UPDATE);
        }
        let view = camera.latest();
        assert!((view.forward - up).length() < 1e-3, "{:?}", view.forward);
        assert!(
            view.eye.z < level.eye.z - 3.0,
            "{} against {}",
            view.eye.z,
            level.eye.z
        );
        // The point it turns about has swung back by his radius times the sine.
        assert!(
            (camera.shoulder - Vec2::new(0.0, -1.0)).length() < 0.01,
            "{:?}",
            camera.shoulder
        );
        // And the eye is never within 1 of the ground he stands on.
        let steep = Vec3::new(0.0, 80f32.to_radians().cos(), 80f32.to_radians().sin());
        for _ in 0..300 {
            camera.step(&aiming(steep), Vec2::ZERO, &t, &Open, UPDATE);
        }
        assert!(camera.latest().eye.z > 0.99, "{}", camera.latest().eye.z);
    }

    fn robot(position: Vec3, velocity: Vec3) -> Target {
        Target {
            position,
            height: 5.4,
            facing: Vec2::Y,
            velocity,
            motion: velocity,
            radius: 2.0,
            ..Default::default()
        }
    }

    #[test]
    fn he_drags_the_follow_camera_at_its_far_limit() {
        let t = tuning();
        let mut position = Vec3::ZERO;
        let velocity = Vec3::new(0.0, 24.0, 0.0);
        let mut camera = Camera::behind(&robot(position, velocity), &t);
        for _ in 0..100 {
            position += velocity * UPDATE;
            camera.step(&robot(position, velocity), Vec2::ZERO, &t, &Open, UPDATE);
        }
        // Run 1, t 121 to 124: running straight, the eye holds 11.0 from the point looked at
        // and stays dead behind.
        let offset = camera.latest().eye - (position + Vec3::Z * 5.4);
        assert!((offset.length() - 11.0).abs() < 0.05, "{}", offset.length());
        assert!(offset.x.abs() < 0.05 && offset.y < 0.0, "{offset}");
    }

    #[test]
    fn the_stick_turns_the_follow_camera_at_its_rate() {
        let t = tuning();
        let target = robot(Vec3::ZERO, Vec3::ZERO);
        let mut camera = Camera::behind(&target, &t);
        let heading = |c: &Camera| heading_of(Vec2::new(c.latest().eye.x, c.latest().eye.y));
        camera.step(&target, Vec2::ZERO, &t, &Open, UPDATE);
        let before = heading(&camera);
        for _ in 0..10 {
            camera.step(&target, Vec2::new(-1.0, 0.0), &t, &Open, UPDATE);
        }
        // Run 1, t 116.86: the stick hard over turned the camera 0.1025 radians an update.
        let turned = wrap(heading(&camera) - before);
        assert!((turned - 1.025).abs() < 0.03, "{turned}");
        // And he stays in the middle of the picture while it turns.
        let view = camera.latest();
        let to_him = (Vec3::Z * 5.4 - view.eye).normalize();
        assert!(
            view.forward.dot(to_him) > 0.9999,
            "looking {} off him",
            view.forward.dot(to_him).acos().to_degrees()
        );
    }

    #[test]
    fn the_view_does_not_spin_when_he_steps_under_an_overhead_camera() {
        let t = tuning();
        // The eye is almost straight over the point looked at (0.3 to the side of it, 9 up):
        // the direction's z is 0.9994, over the rule's 0.88.
        let target = robot(Vec3::ZERO, Vec3::ZERO);
        let look_at = Vec3::Z * 5.4;
        let eye = look_at + Vec3::new(0.3, 0.0, 9.0);
        let view = View {
            eye,
            forward: (look_at - eye).normalize(),
            fov: 45.0,
            roll: 0.0,
        };
        let mut camera = Camera::from_view(view, &target, &t);
        let heading = |c: &Camera, at: Vec3| {
            let offset = c.latest().eye - (at + Vec3::Z * 5.4);
            heading_of(Vec2::new(offset.x, offset.y))
        };
        camera.step(&target, Vec2::ZERO, &t, &Open, UPDATE);
        let before = heading(&camera, Vec3::ZERO);
        // He steps a unit to the side in one update. From the two positions alone the
        // heading of the offset swings by some 70 degrees; the rule holds it where it was.
        let moved = robot(Vec3::new(0.0, 1.0, 0.0), Vec3::ZERO);
        camera.step(&moved, Vec2::ZERO, &t, &Open, UPDATE);
        let turned = wrap(heading(&camera, moved.position) - before).to_degrees();
        assert!(turned.abs() < 1.0, "the view spun {turned} degrees");
    }

    #[test]
    fn a_nudge_turns_the_view_by_its_angle_once() {
        let t = tuning();
        let target = robot(Vec3::ZERO, Vec3::ZERO);
        let mut camera = Camera::behind(&target, &t);
        let heading = |c: &Camera| heading_of(Vec2::new(c.latest().eye.x, c.latest().eye.y));
        camera.step(&target, Vec2::ZERO, &t, &Open, UPDATE);
        let before = heading(&camera);
        camera.nudge(Vec2::new(-0.2, 0.0));
        for _ in 0..5 {
            camera.step(&target, Vec2::ZERO, &t, &Open, UPDATE);
        }
        // The same sense as the stick held left, and only once.
        let turned = wrap(heading(&camera) - before);
        assert!((turned - 0.2).abs() < 0.005, "{turned}");
    }

    #[test]
    fn the_arena_stops_a_sight_line_on_its_ground_and_boxes() {
        use crate::sim::{Arena, Box3};
        let arena = Arena {
            boxes: vec![Box3 {
                min: Vec3::new(-2.0, 10.0, 0.0),
                max: Vec3::new(2.0, 12.0, 6.0),
            }],
            ..Default::default()
        };
        let wall = arena
            .sight(Vec3::new(0.0, 0.0, 3.0), Vec3::new(0.0, 20.0, 3.0))
            .expect("the box is in the way");
        assert!(
            (wall.point - Vec3::new(0.0, 10.0, 3.0)).length() < 1e-4 && wall.normal == Vec3::NEG_Y,
            "{wall:?}"
        );
        let ground = arena
            .sight(Vec3::new(5.0, 0.0, 2.0), Vec3::new(5.0, 4.0, -2.0))
            .expect("the ground is in the way");
        assert!(
            (ground.point - Vec3::new(5.0, 2.0, 0.0)).length() < 1e-4 && ground.normal == Vec3::Z,
            "{ground:?}"
        );
        assert!(
            arena
                .sight(Vec3::new(5.0, 0.0, 3.0), Vec3::new(5.0, 20.0, 3.0))
                .is_none()
        );
        // From inside a box there is nothing to stop on.
        assert!(
            arena
                .sight(Vec3::new(0.0, 11.0, 3.0), Vec3::new(0.0, 11.5, 3.0))
                .is_none()
        );
    }

    /// A wall across the view, 4 behind the look-at point, while `up` is set.
    struct Wall(std::cell::Cell<bool>);

    impl Sight for Wall {
        fn sight(&self, from: Vec3, to: Vec3) -> Option<Hit> {
            (self.0.get() && to.y < -4.0).then(|| Hit {
                point: from + (to - from) * ((-4.0 - from.y) / (to.y - from.y)),
                normal: Vec3::Y,
                ..Default::default()
            })
        }
        fn sweep(&self, from: Vec3, to: Vec3, radius: f32) -> Option<Hit> {
            plane_sweep(from, to, Vec3::Y, -4.0, radius).filter(|_| self.0.get())
        }
    }

    /// The same wall, but only where x is under 2: it has an end to walk round.
    struct WallEnd;

    impl Sight for WallEnd {
        fn sight(&self, from: Vec3, to: Vec3) -> Option<Hit> {
            if from.y <= -4.0 || to.y >= -4.0 {
                return None;
            }
            let point = from + (to - from) * ((-4.0 - from.y) / (to.y - from.y));
            (point.x < 2.0).then_some(Hit {
                point,
                normal: Vec3::Y,
                ..Default::default()
            })
        }
        fn sweep(&self, from: Vec3, to: Vec3, radius: f32) -> Option<Hit> {
            plane_sweep(from, to, Vec3::Y, -4.0, radius).filter(|h| h.point.x < 2.0)
        }
    }

    #[test]
    fn the_camera_swings_round_a_corner_instead_of_closing_in() {
        let t = tuning();
        // He stands past the end of the wall with the camera behind him, then walks along
        // in front of it, so the wall comes between them.
        let mut position = Vec3::new(3.0, 0.0, 0.0);
        let mut camera = Camera::behind(&robot(position, Vec3::ZERO), &t);
        for _ in 0..5 {
            camera.step(
                &robot(position, Vec3::ZERO),
                Vec2::ZERO,
                &t,
                &WallEnd,
                UPDATE,
            );
        }
        assert!(!camera.cornered() && !camera.blocked());
        let velocity = Vec3::new(-24.0, 0.0, 0.0);
        let mut swung = 0;
        for _ in 0..12 {
            position += velocity * UPDATE;
            camera.step(&robot(position, velocity), Vec2::ZERO, &t, &WallEnd, UPDATE);
            let look_at = position + Vec3::Z * 5.4;
            let eye = camera.latest().eye;
            if camera.cornered() {
                swung += 1;
                // Looking past the end of the wall, from as far away as before.
                assert!(
                    WallEnd.sight(look_at, eye).is_none(),
                    "the wall is in the way at {position}"
                );
                assert!(
                    (eye - look_at).length() > 8.5,
                    "closed in to {}",
                    (eye - look_at).length()
                );
            }
        }
        assert!(swung >= 5, "held the corner for {swung} updates");
        // Pushing the stick away from the corner lets it go.
        let mut camera = Camera::behind(&robot(Vec3::new(3.0, 0.0, 0.0), Vec3::ZERO), &t);
        let mut position = Vec3::new(3.0, 0.0, 0.0);
        camera.step(
            &robot(position, Vec3::ZERO),
            Vec2::ZERO,
            &t,
            &WallEnd,
            UPDATE,
        );
        for _ in 0..8 {
            position += velocity * UPDATE;
            camera.step(
                &robot(position, velocity),
                Vec2::new(1.0, 0.0),
                &t,
                &WallEnd,
                UPDATE,
            );
        }
        assert!(!camera.cornered());
    }

    /// A ceiling at a height.
    struct Ceiling(f32);

    impl Sight for Ceiling {
        fn sight(&self, from: Vec3, to: Vec3) -> Option<Hit> {
            (from.z < self.0 && to.z >= self.0).then(|| Hit {
                point: from + (to - from) * ((self.0 - from.z) / (to.z - from.z)),
                normal: -Vec3::Z,
                ..Default::default()
            })
        }
        fn sweep(&self, from: Vec3, to: Vec3, radius: f32) -> Option<Hit> {
            plane_sweep(from, to, -Vec3::Z, -self.0, radius)
        }
    }

    #[test]
    fn a_ceiling_lowers_the_point_looked_at() {
        let t = tuning();
        let target = robot(Vec3::ZERO, Vec3::ZERO);
        let mut camera = Camera::behind(&target, &t);
        for _ in 0..50 {
            camera.step(&target, Vec2::ZERO, &t, &Ceiling(6.0), UPDATE);
        }
        // He is 5.4 to the point looked at; under a ceiling at 6 it is held 1 below, at 5.
        let view = camera.latest();
        assert!(
            view.eye.z <= 6.0,
            "the eye is above the ceiling at {}",
            view.eye.z
        );
        assert!((camera.sponge + 0.4).abs() < 0.01, "{}", camera.sponge);
    }

    #[test]
    fn a_wall_pulls_the_camera_in_and_it_eases_back_out() {
        let t = tuning();
        let target = robot(Vec3::ZERO, Vec3::ZERO);
        let wall = Wall(std::cell::Cell::new(true));
        let mut camera = Camera::behind(&target, &t);
        for _ in 0..10 {
            camera.step(&target, Vec2::ZERO, &t, &wall, UPDATE);
        }
        let away = |c: &Camera| (c.latest().eye - Vec3::Z * 5.4).length();
        assert!(away(&camera) < 4.5, "{}", away(&camera));
        wall.0.set(false);
        camera.step(&target, Vec2::ZERO, &t, &wall, UPDATE);
        // The wall is gone: no jump back, a slow ease out to the near limit of 9.
        assert!(away(&camera) < 5.0, "{}", away(&camera));
        for _ in 0..150 {
            camera.step(&target, Vec2::ZERO, &t, &wall, UPDATE);
        }
        assert!((away(&camera) - 9.0).abs() < 0.3, "{}", away(&camera));
    }

    #[test]
    fn the_look_at_point_lags_a_jump_as_recorded() {
        let t = tuning();
        let mut camera = Camera::behind(&robot(Vec3::ZERO, Vec3::ZERO), &t);
        let mut height = 0.0;
        // Run 1, t 124.08: he rose 0.14, 1.02 and 0.86 in three updates and the look-at
        // point trailed by 0.058, 0.412 and 0.375.
        for (rise, lag) in [(0.14, -0.058), (1.02, -0.412), (0.86, -0.375)] {
            height += rise;
            camera.step(
                &robot(Vec3::new(0.0, 0.0, height), Vec3::ZERO),
                Vec2::ZERO,
                &t,
                &Open,
                UPDATE,
            );
            assert!(
                (camera.sponge - lag).abs() < 0.01,
                "{} against {lag}",
                camera.sponge
            );
        }
    }

    #[test]
    fn the_driving_view_widens_only_in_turbo() {
        let t = tuning();
        let car = |speed: f32| {
            let velocity = Vec3::new(0.0, speed, 0.0);
            Target {
                height: 1.0 + CAR_BODY_RISE,
                facing: Vec2::Y,
                velocity,
                vehicle: true,
                radius: 2.0,
                ..Default::default()
            }
        };
        let mut camera = Camera::behind(&car(35.0), &t);
        for _ in 0..300 {
            camera.step(&car(35.0), Vec2::ZERO, &t, &Open, UPDATE);
        }
        // Run 1: 45 degrees at the normal cap of 35, 14.0 behind.
        assert!(
            (camera.latest().fov - 45.0).abs() < 0.2,
            "{}",
            camera.latest().fov
        );
        assert!((camera.distance - 14.0).abs() < 0.05, "{}", camera.distance);
        for _ in 0..300 {
            camera.step(&car(43.0), Vec2::ZERO, &t, &Open, UPDATE);
        }
        // 45 x 1.45 at the turbo cap; the recording reaches 64 before the turbo runs out.
        assert!(
            (camera.latest().fov - 65.25).abs() < 0.5,
            "{}",
            camera.latest().fov
        );
        assert!((camera.distance - 14.0).abs() < 0.05, "{}", camera.distance);
    }

    #[test]
    fn a_change_of_form_does_not_cut_the_view() {
        let t = tuning();
        let robot = robot(Vec3::ZERO, Vec3::ZERO);
        let car = Target {
            height: 1.0 + CAR_BODY_RISE,
            vehicle: true,
            ..robot
        };
        let mut camera = Camera::behind(&robot, &t);
        for _ in 0..100 {
            camera.step(&robot, Vec2::ZERO, &t, &Open, UPDATE);
        }
        for target in [car, robot] {
            for _ in 0..100 {
                let before = camera.latest();
                camera.step(&target, Vec2::ZERO, &t, &Open, UPDATE);
                let after = camera.latest();
                let turned = before
                    .forward
                    .dot(after.forward)
                    .clamp(-1.0, 1.0)
                    .acos()
                    .to_degrees();
                assert!(
                    turned < 3.0,
                    "the view turned {turned} degrees in one update"
                );
                // Run 1, t 621.7: the eye comes down 0.5 an update with 1.3 of the height
                // still to go; a whole 4.4 starts at 1.3 an update.
                assert!(
                    (after.eye - before.eye).length() < 1.4,
                    "the eye jumped {}",
                    (after.eye - before.eye).length()
                );
            }
        }
        // And it ends looking at the robot's own point again.
        let view = camera.latest();
        assert!(view.forward.dot((Vec3::Z * 5.4 - view.eye).normalize()) > 0.999);
    }

    /// A road that rises behind the car by this slope (he is driving downhill).
    struct Road(f32);

    impl Sight for Road {
        fn sight(&self, from: Vec3, to: Vec3) -> Option<Hit> {
            let above = |p: Vec3| p.z + p.y * self.0;
            let (a, b) = (above(from), above(to));
            (a <= 0.0 || b <= 0.0).then(|| Hit {
                point: from + (to - from) * (a / (a - b)).clamp(0.0, 1.0),
                normal: Vec3::Z,
                ..Default::default()
            })
        }
        fn sweep(&self, from: Vec3, to: Vec3, radius: f32) -> Option<Hit> {
            plane_sweep(
                from,
                to,
                Vec3::new(0.0, self.0, 1.0).normalize(),
                0.0,
                radius,
            )
        }
    }

    #[test]
    fn the_road_does_not_pull_the_driving_camera_in() {
        let t = tuning();
        // The car as the game hands it over: its place on the road, the point on it the
        // body's rise above that (a height of 1 alone puts the point ON the road).
        let car = Target {
            position: Vec3::ZERO,
            height: 1.0 + CAR_BODY_RISE,
            facing: Vec2::Y,
            velocity: Vec3::ZERO,
            motion: Vec3::ZERO,
            vehicle: true,
            climbing: false,
            aim: None,
            radius: 2.0,
            pitch: 0.0,
            roll: 0.0,
            fighting: false,
            ..Default::default()
        };
        let mut camera = Camera::behind(&car, &t);
        for _ in 0..100 {
            camera.step(&car, Vec2::ZERO, &t, &Road(0.09), UPDATE);
            assert!(!camera.blocked(), "pulled in to {}", camera.latest().eye);
        }
        for _ in 0..100 {
            // On the flat, the stick held to look up from as low as the camera goes.
            camera.step(&car, Vec2::new(0.0, 1.0), &t, &Road(0.0), UPDATE);
            assert!(!camera.blocked(), "pulled in to {}", camera.latest().eye);
        }
        // Run 1 has the point looked at 1.1 to 1.3 above the road, never on it.
        assert!(camera.latest().eye.z >= 1.0, "{}", camera.latest().eye);
    }

    #[test]
    fn recentre_moves_to_requested_facing_and_climbing_requests_it() {
        let t = tuning();
        let target = robot(Vec3::ZERO, Vec3::ZERO);
        let at = Vec3::Z * target.height;
        let view = View {
            eye: at + Vec3::X * 10.0,
            forward: -Vec3::X,
            fov: 45.0,
            roll: 0.0,
        };
        let mut c = Camera::from_view(view, &target, &t);
        c.recentre(&target);
        c.step(&target, Vec2::ZERO, &t, &Open, UPDATE);
        assert!(c.recentre);
        assert!(c.latest().eye.y < 0.0);
        assert!(
            c.latest().eye.x > 8.0,
            "recentre must ease, {}",
            c.latest().eye
        );
        for _ in 0..40 {
            c.step(&target, Vec2::ZERO, &t, &Open, UPDATE);
        }
        assert!(!c.recentre);
        assert!(c.latest().eye.x.abs() < 0.01);
        let climbing = Target {
            climbing: true,
            ..target
        };
        let mut c = Camera::from_view(view, &target, &t);
        c.step(&climbing, Vec2::ZERO, &t, &Open, UPDATE);
        assert!(c.recentre);
    }

    #[test]
    fn airborne_clearance_has_hysteresis_and_requires_speed_and_regular_mode() {
        let t = tuning();
        let mut target = Target {
            height: 2.12,
            vehicle: true,
            ground_distance: 2.01,
            velocity: Vec3::new(0.0, 10.0, -15.0),
            motion: Vec3::new(0.0, 10.0, -15.0),
            ..robot(Vec3::ZERO, Vec3::ZERO)
        };
        let mut c = Camera::behind(&target, &t);
        c.step(&target, Vec2::ZERO, &t, &Open, UPDATE);
        assert!(c.airborne);
        target.ground_distance = 0.11;
        c.step(&target, Vec2::ZERO, &t, &Open, UPDATE);
        assert!(c.airborne);
        target.ground_distance = 0.1;
        c.step(&target, Vec2::ZERO, &t, &Open, UPDATE);
        assert!(!c.airborne);
        target.ground_distance = 1.0;
        c.step(&target, Vec2::ZERO, &t, &Open, UPDATE);
        assert!(!c.airborne);
        target.ground_distance = 20.0;
        target.mode = Mode::Transition;
        c.step(&target, Vec2::ZERO, &t, &Open, UPDATE);
        assert!(!c.airborne);
        target.mode = Mode::Normal;
        target.motion.y = 5.0;
        target.velocity.y = 5.0;
        c.step(&target, Vec2::ZERO, &t, &Open, UPDATE);
        assert!(!c.airborne);
    }

    #[test]
    fn airborne_tilt_uses_velocity_and_keeps_user_offset_at_the_limit() {
        let t = tuning();
        let target = Target {
            height: 2.12,
            vehicle: true,
            ground_distance: 3.0,
            velocity: Vec3::new(0.0, 10.0, 20.0),
            motion: Vec3::new(0.0, 10.0, 20.0),
            ..robot(Vec3::ZERO, Vec3::ZERO)
        };
        let mut c = Camera::behind(&target, &t);
        c.user_elevation = 1.0;
        let before = c.elevation;
        c.step(&target, Vec2::ZERO, &t, &Open, UPDATE);
        let velocity_tilt = (-20.0_f32).atan2(10.0) - std::f32::consts::FRAC_PI_8;
        let tilt = (before + 0.25 * (velocity_tilt - before)).max(0.0);
        let wanted = (t.drive_camera.default_elevation.to_radians() + tilt + 1.0).clamp(
            t.drive_camera.min_elevation.to_radians(),
            t.drive_camera.max_elevation.to_radians(),
        );
        assert!(
            (c.elevation - (before + t.drive_camera.tilt_gain * (wanted - before))).abs() < 1e-6
        );
        assert_eq!(c.user_elevation, 1.0);
    }

    #[test]
    fn driving_recentre_clears_user_pitch_without_cutting_the_elevation() {
        let t = tuning();
        let target = Target {
            height: 2.12,
            vehicle: true,
            ..robot(Vec3::ZERO, Vec3::ZERO)
        };
        let mut c = Camera::behind(&target, &t);
        for _ in 0..15 {
            c.step(&target, Vec2::new(0.0, -1.0), &t, &Open, UPDATE);
        }
        let before = c.elevation;
        assert!(c.user_elevation > 0.1);
        c.recentre(&target);
        c.step(&target, Vec2::ZERO, &t, &Open, UPDATE);
        assert_eq!(c.user_elevation, 0.0);
        assert!((c.elevation - before).abs() < 0.2);
        for _ in 0..150 {
            c.step(&target, Vec2::ZERO, &t, &Open, UPDATE);
        }
        assert!((c.elevation - t.drive_camera.default_elevation.to_radians()).abs() < 0.001);
    }

    #[test]
    fn ground_punch_and_death_modes_use_their_original_framing() {
        let t = tuning();
        let target = Target {
            mode: Mode::GroundPunch,
            ..robot(Vec3::ZERO, Vec3::ZERO)
        };
        let mut c = Camera::behind(&target, &t);
        for _ in 0..300 {
            c.step(&target, Vec2::ZERO, &t, &Open, UPDATE);
        }
        assert!((c.extra - 15.0).abs() < 0.001);
        assert!((c.latest().fov - 67.5).abs() < 0.01);
        assert!(
            (c.elevation - (t.robot_camera.default_elevation + 45.0).to_radians()).abs() < 0.001
        );
        let normal = robot(Vec3::ZERO, Vec3::ZERO);
        let mut fast = Camera::behind(&normal, &t);
        let mut ordinary = fast.clone();
        fast.step(
            &Target {
                mode: Mode::Fast,
                ..normal
            },
            Vec2::ZERO,
            &t,
            &Open,
            UPDATE,
        );
        ordinary.step(
            &Target {
                climbing: true,
                ..normal
            },
            Vec2::ZERO,
            &t,
            &Open,
            UPDATE,
        );
        assert!(fast.extra > ordinary.extra);
    }

    #[test]
    fn death_overrides_climb_and_does_not_refresh_attack_stand_off() {
        let t = tuning();
        let normal = robot(Vec3::ZERO, Vec3::ZERO);
        let mut c = Camera::behind(&normal, &t);
        c.hold = 1.0;
        c.step(
            &Target {
                mode: Mode::Fast,
                climbing: true,
                fighting: true,
                ..normal
            },
            Vec2::ZERO,
            &t,
            &Open,
            UPDATE,
        );
        assert_eq!(c.mode, Mode::Fast);
        assert!((c.hold - (1.0 - UPDATE)).abs() < 1e-6);
        assert!(!c.recentre);
    }

    #[test]
    fn an_attack_keeps_the_view_wide_for_the_original_stand_off_tail() {
        let t = tuning();
        let normal = robot(Vec3::ZERO, Vec3::ZERO);
        let mut c = Camera::behind(&normal, &t);
        for _ in 0..80 {
            c.step(
                &Target {
                    fighting: true,
                    ..normal
                },
                Vec2::ZERO,
                &t,
                &Open,
                UPDATE,
            );
        }
        assert!((c.hold - (3.7 - UPDATE)).abs() < 1e-5);
        assert!((c.extra - t.robot_camera.face_extra_dist).abs() < 0.001);
        for _ in 0..110 {
            c.step(&normal, Vec2::ZERO, &t, &Open, UPDATE);
        }
        assert!(c.hold > UPDATE);
        assert!((c.extra - t.robot_camera.face_extra_dist).abs() < 0.001);
        for _ in 0..200 {
            c.step(&normal, Vec2::ZERO, &t, &Open, UPDATE);
        }
        assert_eq!(c.hold, 0.0);
        assert!(c.extra.abs() < 0.001);
        assert!(
            (c.latest().eye - Vec3::Z * normal.height).length() <= t.robot_camera.max_dist + 0.01
        );
    }

    #[test]
    fn camera_options_wrap_shared_and_separate_levels_and_invert_stick() {
        let mut levels = [0, 1, 2];
        let mut options = Options::default();
        options.cycle(&mut levels, 2, -1);
        assert_eq!(levels, [1; 3]);
        options.separate_distances = true;
        options.cycle(&mut levels, 0, -2);
        assert_eq!(levels, [2, 1, 1]);
        let t = tuning();
        let target = robot(Vec3::ZERO, Vec3::ZERO);
        let mut left = Camera::behind(&target, &t);
        let mut right = left.clone();
        right.options.invert_yaw = true;
        left.step(&target, Vec2::X, &t, &Open, UPDATE);
        right.step(&target, Vec2::X, &t, &Open, UPDATE);
        assert!(
            (left.latest().eye.x + right.latest().eye.x).abs() < 5e-6,
            "{} / {}",
            left.latest().eye,
            right.latest().eye
        );
        assert!(left.latest().eye.x.abs() > 0.1);
    }

    #[test]
    fn fadeable_scene_objects_change_opacity_without_blocking_the_camera() {
        let objects = [Fadeable {
            id: 9,
            bounds: crate::sim::Box3 {
                min: Vec3::new(-2.0, -6.0, 0.0),
                max: Vec3::new(2.0, -4.0, 10.0),
            },
        }];
        let scene = Scene {
            solids: &Open,
            fadeable: &objects,
        };
        let target = robot(Vec3::ZERO, Vec3::ZERO);
        let t = tuning();
        let mut c = Camera::behind(&target, &t);
        for _ in 0..8 {
            c.step(&target, Vec2::ZERO, &t, &scene, UPDATE);
        }
        assert!(!c.blocked());
        assert_eq!(c.fades.alpha(9), 0.45);
        c.step(&target, Vec2::ZERO, &t, &Open, UPDATE);
        assert!((c.fades.alpha(9) - 0.578).abs() < 1e-6);
        // The driving sweep can touch a fadeable even if its centre line misses it.
        let edge = Scene {
            solids: &Open,
            fadeable: &objects,
        };
        assert!(
            edge.fade_hits(Vec3::new(2.4, -2.0, 5.0), Vec3::new(2.4, -8.0, 5.0), 0.0)
                .is_empty()
        );
        assert_eq!(
            edge.fade_hits(Vec3::new(2.4, -2.0, 5.0), Vec3::new(2.4, -8.0, 5.0), 0.65),
            vec![9]
        );
    }

    #[test]
    fn freeze_wakes_only_on_horizontal_motion_or_camera_stick() {
        let t = tuning();
        let target = robot(Vec3::ZERO, Vec3::ZERO);
        let mut c = Camera::behind(&target, &t);
        c.freeze();
        let eye = c.latest().eye;
        c.step(
            &Target {
                position: Vec3::Z,
                ..target
            },
            Vec2::ZERO,
            &t,
            &Open,
            UPDATE,
        );
        assert_eq!(c.latest().eye, eye);
        c.step(
            &Target {
                position: Vec3::X,
                ..target
            },
            Vec2::ZERO,
            &t,
            &Open,
            UPDATE,
        );
        assert!(!c.frozen);
        assert_ne!(c.latest().eye, eye);
    }

    #[test]
    fn reset_uses_the_midpoint_leash_and_clears_sponge() {
        let t = tuning();
        let target = robot(Vec3::ZERO, Vec3::ZERO);
        let mut c = Camera::behind(&target, &t);
        c.sponge = 5.0;
        c.options.invert_yaw = true;
        c.reset(&target, &t);
        assert!((c.distance - 10.0).abs() < 1e-5);
        assert!(c.sponge.abs() < 1e-5);
        assert!(c.options.invert_yaw);
        assert!(!c.settling);
    }

    #[test]
    fn weapon_vertical_aim_keeps_previous_xy_and_does_not_transfer_distance_bias() {
        let t = tuning();
        let target = aiming(Vec3::new(1.0, 0.0, 0.1));
        let mut c = Camera::behind(&target, &t);
        c.step(&aiming(Vec3::Z), Vec2::ZERO, &t, &Open, UPDATE);
        assert_eq!(c.aim.x, 1.0);
        assert_eq!(c.aim.y, 0.0);
        c.distance_bias = -100.0;
        c.step(
            &robot(Vec3::ZERO, Vec3::ZERO),
            Vec2::ZERO,
            &t,
            &Open,
            UPDATE,
        );
        assert_eq!(c.distance_bias, 0.0);
    }

    #[test]
    fn slanted_corner_edge_obeys_the_elevation_cone() {
        let edge = Vec3::new(0.1, 0.0, 1.0).normalize();
        let mut corner = Corner {
            point: Vec3::new(2.0, 0.0, 1.0),
            edge: Some((Vec3::new(2.0, 0.0, 0.0), edge)),
            distance: 0.15,
            ..Default::default()
        };
        corner.slide(Vec3::ZERO, Vec3::new(0.0, -5.0, 2.0), 0.3);
        assert!(
            (corner.point.z / corner.point.truncate().length() - 0.3_f32.tan()).abs() < 1e-5,
            "{}",
            corner.point
        );
    }
}
