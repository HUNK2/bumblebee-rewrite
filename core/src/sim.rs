//! Bumblebee's movement, as a deterministic fixed-step routine with no engine types in it.
//!
//! Space is the game's: x right, y forward, z up, distances as the game's data gives them.
//! Every tuning number comes from the install (see `tuning.rs`). How those numbers are *applied*
//! is only partly read from the original so far, and each rule below says which it is:
//!
//!   [game]     behaviour read from the original's movement code (see notes/bumblebee-mechanics.md)
//!   [data]     follows directly from the character's own data (clips, rules, tuning)
//!   [assumed]  a plausible reading of a data value whose use has not been read yet
//!   [stand-in] invented so the test is playable; to be replaced
//!   [trace]    measured on the running game (see notes/live-trace.md)

use glam::{Affine3A, EulerRot, Quat, Vec2, Vec3};

use crate::animrules::{self, Base, Value};
use crate::climb::{self, Climb, Ledge, Wall};
use crate::control::{Facts, Pad, Runner, button, env};
use crate::formats::anim::Action as ClipAction;
use crate::formats::hash::crc32;
use crate::formats::scene::Shape;
use crate::melee::{self, Hit, Target};
use crate::rumble::{self, Rumble};
use crate::sound::{GearboxInput, TyreInput};
use crate::tuning::{ActionClip, JumpKind, Locomotion, Tuning};

/// Stick deflection (squared) below which the original treats the stick as centred. [game]
const STICK_DEAD_SQUARED: f32 = 0.01;
/// Speed (squared) under which leaving vehicle form gives the high standing jump. [game]
const VEHICLE_EXIT_SLOW_SQUARED: f32 = 3.0;
/// Stick deflection (squared) above which a running jump takes off at exactly the run speed. [game]
const FULL_STICK_SQUARED: f32 = 0.9801;
/// The standing jump's height as a multiple of the running jump's. [game]
const STANDING_JUMP_SCALE: f32 = 1.2;
/// The per-update air-control shares are applied this many times a second: the game's update
/// on PC is 0.032 s. [trace]
const CONTROL_FRAME_RATE: f32 = 31.25;
/// A ledge this low is walked up instead of blocking. [stand-in]
const STEP_HEIGHT: f32 = 0.6;
/// Throttle or brake below this counts as released. [stand-in]
const TRIGGER_DEAD: f32 = 0.1;
/// The length of one of the game's updates on PC, in seconds. [trace]
pub const GAME_UPDATE: f32 = 0.032;
/// The vehicle body's mass and its resistance to turning about the vertical, read from the
/// running game's physics body (5417 and 16670 about the other two axes). [trace]
pub const VEHICLE_MASS: f32 = 5000.0;
/// The state machine's "on the ground" flag (owner `+0x26c`) trails the body: in run 1 every
/// landing stays in the jump mode for the update it touches down and the one after, and
/// leaves it on the second. So the flag the machine reads changes once the body's contact has
/// held for one of the game's updates. [trace]
const GROUND_FLAG_LAG: f32 = GAME_UPDATE - 1e-4;
/// The jump state may not be left until the jump mode has run this long (`FUN_0087e380`). [game]
const JUMP_LEAVE_TIME: f32 = 0.1;
/// MoveMode's share of the way to what the clips ask for, per update, while the ground flag is
/// clear (`FUN_00852220`); on the ground it is 1. [game]
const MOVE_AIR_CONTROL: f32 = 0.1;
/// The stick lines up with his motion for the car (`FUN_0087bcf0`): cosine of the angle. [game]
const DRIVE_LINE_UP: f32 = 0.9;
/// The unfold runs out to the run if the stick is held past this when it starts
/// (`FUN_00863670`). [game]
const UNFOLD_RUN_STICK: f32 = 0.05;
/// In the air the stick pushes the car this hard, and a body tipped further than this is
/// turned back toward level. [game]
const CAR_AIR_PUSH: f32 = 10.0;
/// The car rumbles the pad when it touches something faster than this (`FUN_00860870`),
/// and when it comes down later than this into the drive mode (`00b345ec`). [game]
const CAR_KNOCK_SPEED: f32 = 5.0;
const CAR_LANDING_AFTER: f32 = 0.1;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JumpType {
    Standing,
    Moving,
    HighStanding,
    Long,
    /// Walked off an edge: airborne without a launch.
    Fall,
    /// Off a wall (type dfb7146d): straight up it or away from it.
    Climb,
}

/// The motion mode: what moves the body this update. The control state machine chooses it
/// (each state sets one when entered); the names are the original's mode classes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Standing (the unnamed first mode, class 1ad6ae68): the clips move him, nothing turns
    /// him. `stateIdle` sets it.
    Stand,
    /// MoveMode: turns him toward the stick; the clips move him.
    Move,
    /// StrafeMode (class 3c956e02), weapon mode on foot: he faces his aim and the weapon
    /// set's clips move him the way the stick points. `stateWeaponMode` sets it.
    Strafe,
    Jump(JumpType),
    Drive,
    /// The change back out of the car (the drive companion mode, class f081d468): he slows
    /// at the exit rate and turns toward the stick at the exit's own turn speed.
    Unfold,
    /// On a wall (`notes\climbing.md`).
    Climb,
    /// One of the control states that move him by their clip: the attacks (the attack mode),
    /// the dodge (the dash mode), the ground punch's landing.
    Action,
}

/// What the stick asks of the robot on the ground; it picks the clip, and the clip sets the pace.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Gait {
    #[default]
    Idle,
    Walk,
    Run,
}

/// The clip that sets the pace on the ground. [trace]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Pace {
    #[default]
    Idle,
    Walk,
    Run,
    /// Run to idle.
    Stop,
    /// The landing that goes straight into a run, and the one after a long fall.
    LandRun,
    BigLandRun,
}

/// One clip in the cross-fade: how far it has faded in over what was there, and how long it
/// has played.
#[derive(Clone, Copy, Debug)]
struct Layer {
    pace: Pace,
    weight: f32,
    time: f32,
}

/// One clip of the weapon set in its cross-fade: how far it has faded in and where it is.
/// The bottom one can be what he was doing before instead: a control state's clip playing
/// on (`action`), or with `clip` 0 the velocity he had, in his own frame.
#[derive(Clone, Copy, Debug)]
struct RootLayer {
    clip: u32,
    action: bool,
    weight: f32,
    blend: f32,
    tick: f32,
    carry: Vec2,
    elapsed_ticks: f32,
    overrun: f32,
}

/// StrafeMode and the weapon set it drives through three animation variables.
#[derive(Clone, Debug, Default)]
pub struct Strafe {
    /// `F_STRAFE_ANG`, degrees, positive to the left: moving, the stick against the aim;
    /// standing, the aim against his body. [game]
    pub angle: f32,
    /// `I_IS_STRAFING`: set by the mode's enter, cleared by its exit. [game]
    pub strafing: bool,
    /// The aim heading (owner `+0x25c`) and how far his spine is twisted toward it while
    /// he stands (the mode's `+0x24`), radians. [game]
    pub aim_yaw: f32,
    pub twist: f32,
    /// The weapon set's clip that leads, by the CRC-32 of its id, and its tick.
    pub clip: u32,
    pub tick: f32,
    layers: Vec<RootLayer>,
    /// The mode has not had an update since its enter.
    entering: bool,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Input {
    /// Stick direction in world space, length up to 1.
    pub stick: Vec2,
    /// Stick left/right as the pad gives it, for steering the car.
    pub steer: f32,
    /// The stick as the pad gives it, x right and y up, for climbing (the input source's
    /// slot `+0x30`, which the car steers by too). [game]
    pub pad_stick: Vec2,
    /// Jump button went down this step.
    pub jump: bool,
    /// Jump button is down.
    pub jump_held: bool,
    /// Vehicle-mode trigger, 0 to 1. Held: car form and throttle. Released: back to robot.
    pub vehicle: f32,
    /// Brake, 0 to 1.
    pub brake: f32,
    /// Turbo button went down this step.
    pub turbo: bool,
    /// The slide (drift) button is down.
    pub drift: bool,
    /// Weapon mode: the direction he aims along the ground. `None` when not aiming.
    pub aim: Option<Vec2>,
    /// Buttons the fields above do not cover, by the game's numbers (`control::button`):
    /// down now, and went down this step. Melee, dodge and climb come this way.
    pub buttons_down: u64,
    pub buttons_hit: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct Box3 {
    pub min: Vec3,
    pub max: Vec3,
}

/// What the movement needs to know about its surroundings. The Bevy test answers from a flat
/// arena of boxes and slopes; a host game answers from its own collision.
pub trait World {
    /// Vehicle queries use the world's actual ray/contact geometry, without walking steps.
    fn wheel_ray(&self, _from: Vec3, _to: Vec3) -> Option<crate::vehicle::GroundHit> { None }
    fn vehicle_sweep(&self, _centre: Vec3, _turn: Quat, _half: Vec3, _travel: Vec3) -> Option<crate::vehicle::Contact> { None }
    /// Existing arena target separation, independent of the static vehicle solver.
    fn separate_targets(&self,_pos:&mut Vec3,_radius:f32,_height:f32) {}
    fn surface_contacts(&self, _body: &melee::Body) -> Vec<crate::ssd::Contact> { Vec::new() }
    /// Height of the surface to stand on at this spot, for feet at height `z`: the highest
    /// surface no more than a step above the feet.
    fn floor(&self, x: f32, y: f32, z: f32) -> f32;
    /// Optional continuous support along a grounded move, across ramps and their joins.
    fn follow_floor(&self, _from: Vec3, _to: Vec2) -> Option<f32> { None }
    /// Moves an upright cylinder out of anything it cannot step onto. Returns true if it hit.
    fn push_out(&self, pos: &mut Vec3, radius: f32, height: f32) -> bool;
    /// For the climbable test: the first step up along `toward` (a unit vector) from `from`,
    /// within `reach`, to ground higher than `from.z`. The original answers it from its
    /// level's 2.5D ledge mesh; a world without one has nothing to climb.
    fn ledge(&self, _from: Vec3, _toward: Vec2, _reach: f32) -> Option<Ledge> {
        None
    }
    /// For the jump state's ledge catch: a ball of `radius` let straight down from `from` to
    /// height `to_z`, and where it first touches. The original sweeps its collision
    /// (`FUN_005be5a0`, mask 0x5d); a world that does not answer catches nothing.
    fn drop_onto(&self, _from: Vec3, _to_z: f32, _radius: f32) -> Option<Vec3> {
        None
    }
    /// What his attacks can go for and hit. A world with nothing to fight has none.
    fn targets(&self) -> &[Target] {
        &[]
    }
}

/// How high a surface can be above the feet and still be stepped onto.
pub const STEP: f32 = STEP_HEIGHT;

/// [stand-in] Flat ground at height zero with authored solid boxes and planar ramps.
#[derive(Clone, Default, Debug)]
pub struct Arena {
    pub boxes: Vec<Box3>,
    pub ramps: Vec<crate::terrain::Ramp>,
    /// Things to hit, each standing as an upright cylinder he cannot walk through.
    pub targets: Vec<Target>,
}

impl World for Arena {
    fn separate_targets(&self,pos:&mut Vec3,radius:f32,height:f32) {
        // [stand-in] Retain the existing target-cylinder separation. Original avatar
        // contacts/ramming remain separate from the new static-world chassis solver.
        for target in &self.targets {
            if target.pos.z >= pos.z + height || pos.z >= target.pos.z + target.height {continue;}
            let away=Vec2::new(pos.x-target.pos.x,pos.y-target.pos.y);
            let (distance,apart)=(away.length(),radius+target.radius);
            if distance<apart && distance>1e-4 {
                let push=away/distance*(apart-distance);
                pos.x+=push.x;pos.y+=push.y;
            }
        }
    }
    fn wheel_ray(&self, from: Vec3, to: Vec3) -> Option<crate::vehicle::GroundHit> {
        crate::camera::Sight::sight(self,from,to).map(|hit|crate::vehicle::GroundHit {
            point:hit.point, normal:hit.normal, surface:melee::surface::DEFAULT,
        })
    }
    fn vehicle_sweep(&self, centre: Vec3, turn: Quat, half: Vec3, travel: Vec3) -> Option<crate::vehicle::Contact> {
        let mut found=self.ramps.iter().enumerate().filter_map(|(i,r)|r.vehicle_sweep(centre,turn,half,travel)
            .map(|hit|crate::vehicle::Contact {key:(self.boxes.len()+i+1) as u32,..hit})).collect::<Vec<_>>();
        for (i,b) in self.boxes.iter().enumerate() {
            let vertices=(0..8).map(|i|Vec3::new(if i&1==0 {b.min.x} else {b.max.x},
                if i&2==0 {b.min.y} else {b.max.y}, if i&4==0 {b.min.z} else {b.max.z})).collect::<Vec<_>>();
            if let Some(hit)=crate::terrain::sweep_cuboid(centre,turn,half,travel,&vertices,
                &[Vec3::X,Vec3::Y,Vec3::Z],&[Vec3::X,Vec3::Y,Vec3::Z]) {found.push(crate::vehicle::Contact {key:i as u32+1,..hit});}
        }
        let axes=[turn*Vec3::X,turn*Vec3::Y,turn*Vec3::Z];
        let bottom=centre.z-half.x*axes[0].z.abs()-half.y*axes[1].z.abs()-half.z*axes[2].z.abs();
        if bottom<0.0 || (travel.z<0.0 && bottom+travel.z<=0.0) {
            let fraction=if bottom<0.0 {0.0} else {bottom/-travel.z};
            found.push(crate::vehicle::Contact {key:0,fraction,point:Vec3::new(centre.x,centre.y,0.0),normal:Vec3::Z,
                depth:(-bottom).max(0.0),surface:melee::surface::DEFAULT,velocity:Vec3::ZERO});
        }
        found.into_iter().min_by(|a,b|a.fraction.total_cmp(&b.fraction).then(b.depth.total_cmp(&a.depth)))
    }
    fn surface_contacts(&self, body: &melee::Body) -> Vec<crate::ssd::Contact> {
        let mut contacts = crate::ssd::contacts(body, &self.boxes);
        for (i, ramp) in self.ramps.iter().enumerate() {
            if let Some((point, normal)) = ramp.contact(body) {
                contacts.push(crate::ssd::Contact { surface: self.boxes.len() + i + 1, point, normal });
            }
        }
        contacts
    }
    fn floor(&self, x: f32, y: f32, z: f32) -> f32 {
        Arena::floor(self, x, y, z)
    }

    fn push_out(&self, pos: &mut Vec3, radius: f32, height: f32) -> bool {
        Arena::push_out(self, pos, radius, height)
    }

    fn follow_floor(&self, from: Vec3, to: Vec2) -> Option<f32> {
        Arena::follow_floor(self, from, to)
    }

    fn ledge(&self, from: Vec3, toward: Vec2, reach: f32) -> Option<Ledge> {
        Arena::ledge(self, from, toward, reach)
    }

    /// The highest top (or the ground) under the spot between the two heights. [stand-in:
    /// the ball's width is ignored, so it touches straight under its centre]
    fn drop_onto(&self, from: Vec3, to_z: f32, _radius: f32) -> Option<Vec3> {
        let z = self
            .boxes
            .iter()
            .filter(|b| from.x >= b.min.x && from.x <= b.max.x && from.y >= b.min.y && from.y <= b.max.y)
            .map(|b| b.max.z)
            .chain(self.ramps.iter().filter(|r| r.contains(from.truncate())).map(|r| r.top(from.truncate())))
            .chain([0.0])
            .filter(|&z| z <= from.z && z >= to_z)
            .fold(f32::MIN, f32::max);
        (z > f32::MIN).then(|| Vec3::new(from.x, from.y, z))
    }

    fn targets(&self) -> &[Target] {
        &self.targets
    }
}

impl Arena {
    /// Height of the surface to stand on at this spot, for feet at height `z`.
    pub fn floor(&self, x: f32, y: f32, z: f32) -> f32 {
        self.boxes
            .iter()
            .filter(|b| x >= b.min.x && x <= b.max.x && y >= b.min.y && y <= b.max.y && b.max.z <= z + STEP_HEIGHT)
            .map(|b| b.max.z)
            .chain(self.ramps.iter().filter(|r| r.contains(Vec2::new(x, y)))
                .map(|r| r.top(Vec2::new(x, y))).filter(|&top| top <= z + STEP_HEIGHT))
            .fold(0.0, f32::max)
    }

    /// [stand-in] Follow continuous arena support in substeps shorter than a step height.
    /// Only ramp traversals use this; a discontinuous drop still launches/falls normally.
    /// This prevents a fast car treating the rise in one frame as a vertical wall.
    pub fn follow_floor(&self, from: Vec3, to: Vec2) -> Option<f32> {
        let start = from.truncate();
        let low = start.min(to);
        let high = start.max(to);
        if !self.ramps.iter().any(|r| low.cmple(r.max).all() && high.cmpge(r.min).all())
            || (self.floor(from.x, from.y, from.z) - from.z).abs() > 0.01 {
            return None;
        }
        let steps = (start.distance(to) / (STEP_HEIGHT * 0.25)).ceil().max(1.0) as usize;
        let mut z = from.z;
        let mut on_ramp = false;
        for i in 1..=steps {
            let p = start.lerp(to, i as f32 / steps as f32);
            for r in self.ramps.iter().filter(|r| r.contains(p)) {
                if r.base < z + STEP_HEIGHT && r.top(p) > z + STEP_HEIGHT { return None; }
                on_ramp |= (r.top(p) - z).abs() <= STEP_HEIGHT;
            }
            let next = self.floor(p.x, p.y, z);
            if (next - z).abs() > STEP_HEIGHT { return None; }
            z = next;
        }
        on_ramp.then_some(z)
    }

    /// Moves an upright cylinder out of any box it cannot step onto. Returns true if it hit one.
    pub fn push_out(&self, pos: &mut Vec3, radius: f32, height: f32) -> bool {
        // Two bodies do not share a spot: he is moved back to touching. That is not a wall,
        // so it does not count as a hit. [stand-in for the collision between two characters]
        self.separate_targets(pos,radius,height);
        let mut hit = false;
        // [stand-in] The upright cylinder meets the ramp's vertical sides at their local
        // height. The planar top is handled by floor/follow_floor, not a bounding box.
        let ramp_sides = self.ramps.iter().filter_map(|r| {
            let p = pos.truncate().clamp(r.min, r.max);
            let top = r.top(p);
            (top > pos.z + STEP_HEIGHT && r.base < pos.z + height)
                .then_some(Box3 { min: r.min.extend(r.base), max: r.max.extend(top) })
        }).collect::<Vec<_>>();
        for b in self.boxes.iter().chain(&ramp_sides) {
            if b.max.z <= pos.z + STEP_HEIGHT || b.min.z >= pos.z + height {
                continue;
            }
            let nearest = Vec2::new(pos.x.clamp(b.min.x, b.max.x), pos.y.clamp(b.min.y, b.max.y));
            let away = Vec2::new(pos.x, pos.y) - nearest;
            let distance = away.length();
            if distance >= radius {
                continue;
            }
            hit = true;
            if distance > 1e-4 {
                let push = away / distance * (radius - distance);
                pos.x += push.x;
                pos.y += push.y;
            } else {
                // Centre is inside the box: leave by the nearest side.
                let sides = [
                    (pos.x - b.min.x, Vec2::NEG_X),
                    (b.max.x - pos.x, Vec2::X),
                    (pos.y - b.min.y, Vec2::NEG_Y),
                    (b.max.y - pos.y, Vec2::Y),
                ];
                let (depth, normal) = sides.into_iter().fold(sides[0], |a, s| if s.0 < a.0 { s } else { a });
                pos.x += normal.x * (depth + radius);
                pos.y += normal.y * (depth + radius);
            }
        }
        hit
    }

    /// The ledge mesh the original climbs by, made from the boxes: every box stands on the
    /// ground, so each of its sides is a step from the ground to its top. The first side the
    /// line crosses into ground above `from.z` is the wall; it runs on across the same side
    /// of other boxes in line with it and touching it. The original also lets a wall run past
    /// a corner that turns by less than 45 degrees; boxes have none. [stand-in: boxes for
    /// the ledge mesh; the rules are the original's, `notes\climbing.md`]
    pub fn ledge(&self, from: Vec3, toward: Vec2, reach: f32) -> Option<Ledge> {
        let origin = Vec2::new(from.x, from.y);
        let inside = |b: &Box3, p: Vec2| p.x >= b.min.x && p.x <= b.max.x && p.y >= b.min.y && p.y <= b.max.y;
        let rises = |b: &Box3| b.max.z > from.z && b.min.z <= from.z;
        // Where the line enters each box, by the slab test, and through which side.
        let mut nearest: Option<(f32, Vec2, usize)> = None;
        for (i, b) in self.boxes.iter().enumerate() {
            if !rises(b) || inside(b, origin) {
                continue;
            }
            let (mut enter, mut leave, mut normal) = (0.0f32, reach, Vec2::ZERO);
            let mut missed = false;
            for (o, d, lo, hi, axis) in [(origin.x, toward.x, b.min.x, b.max.x, Vec2::X), (origin.y, toward.y, b.min.y, b.max.y, Vec2::Y)] {
                if d.abs() < 1e-6 {
                    missed |= o < lo || o > hi;
                    continue;
                }
                let (near, far, side) = if d > 0.0 { ((lo - o) / d, (hi - o) / d, -axis) } else { ((hi - o) / d, (lo - o) / d, axis) };
                if near > enter {
                    enter = near;
                    normal = side;
                }
                leave = leave.min(far);
            }
            if missed || enter > leave || normal == Vec2::ZERO || nearest.is_some_and(|(at, _, _)| at <= enter) {
                continue;
            }
            nearest = Some((enter, normal, i));
        }
        let (at, normal, first) = nearest?;
        let hit = origin + toward * at;
        let beyond = hit - normal * 0.01;
        let top = self.boxes.iter().filter(|b| inside(b, beyond) && b.min.z <= from.z).map(|b| b.max.z).fold(f32::MIN, f32::max);
        let near = hit + normal * 0.01;
        let bottom = self.floor(near.x, near.y, from.z - STEP_HEIGHT);
        // The side's line, and how far it runs along it (measured along the wall's x axis,
        // to his right as he faces it), joined across sides in line with it.
        let along = Vec2::new(-normal.y, normal.x);
        let side_of = |b: &Box3| -> (f32, f32, f32) {
            // (where the side is across the line, its two ends along it)
            let corner_lo = if normal.x != 0.0 { Vec2::new(if normal.x < 0.0 { b.min.x } else { b.max.x }, b.min.y) } else { Vec2::new(b.min.x, if normal.y < 0.0 { b.min.y } else { b.max.y }) };
            let corner_hi = if normal.x != 0.0 { Vec2::new(corner_lo.x, b.max.y) } else { Vec2::new(b.max.x, corner_lo.y) };
            let (a, c) = (corner_lo.dot(along), corner_hi.dot(along));
            (corner_lo.dot(normal), a.min(c), a.max(c))
        };
        let (plane, mut lo, mut hi) = side_of(&self.boxes[first]);
        loop {
            let mut grew = false;
            for b in self.boxes.iter().filter(|b| rises(b)) {
                let (p, a, c) = side_of(b);
                if (p - plane).abs() < 1e-3 && a <= hi + 1e-3 && c >= lo - 1e-3 && (a < lo - 1e-3 || c > hi + 1e-3) {
                    (lo, hi) = (lo.min(a), hi.max(c));
                    grew = true;
                }
            }
            if !grew {
                break;
            }
        }
        let s = hit.dot(along);
        Some(Ledge { hit, normal, bottom, top, left: s - lo, right: hi - s, extra: 0.0 })
    }
}

#[derive(Clone, Debug)]
pub struct State {
    pub pos: Vec3,
    pub vel: Vec3,
    /// Facing: 0 looks along +y, positive turns toward -x.
    pub yaw: f32,
    pub mode: Mode,
    pub gait: Gait,
    /// Forward speed along the facing while on the ground; negative when the car reverses.
    pub speed: f32,
    pub turbo_left: f32,
    pub turbo_cooldown: f32,
    /// Player's duration/cooldown upgrade levels;0 is the base tuning. [game]
    pub turbo_upgrades: [u8; 2],
    /// [game: 00732e60] Either timed turbo started during this step (sound event).
    pub turbo_started: bool,
    /// [game: 00733290/00733ae0] Last independent turbo FX on/off message. A timer
    /// keeps running after DriveMode exit, while this is switched off by that exit.
    pub turbo_fx_on: bool,
    /// Front wheel angle in radians, for drawing.
    pub steer_angle: f32,
    pub health: f32,
    pub since_damage: f32,
    pub damage_timers: crate::damage::Runtime,
    /// Receiving-side flags4/5, consumed by the host's weapon manager.
    pub heat_damage: Vec<(u32,f32)>,
    pub actor: crate::damage::Actor,
    pub difficulty: crate::damage::Difficulty,
    /// [game] Single-player death completion asks the host to restart the mission.
    pub restart_requested: bool,
    regen_delay: f32,
    /// Height of the last launch point and the highest point reached since, for the readout.
    pub launch_z: f32,
    pub apex_z: f32,
    /// The clips cross-fading on the ground, oldest first. Empty means standing.
    layers: Vec<Layer>,
    /// StrafeMode's own values and the weapon set's clips.
    pub strafe: Strafe,
    /// The clip the base layer's rules have picked while he stands, walks, jumps or falls
    /// (`animrules::Base`); empty while a control state plays a clip of its own or the weapon
    /// set plays.
    pub base: Base,
    /// `C_JUMP_TYPE`: what the jump mode last stored, which stays after it ends (the landing's
    /// entry rules read it). [game]
    jump_type: u32,
    /// Seconds spent going down since leaving the ground, and seconds since the launch.
    fall_time: f32,
    air_time: f32,
    /// Seconds in the jump mode (its `+0x2c`).
    jump_time: f32,
    /// The jump mode's slow motion while the weapon set plays: its time factor (`+0x28`, 1
    /// when there is none), the factor the body's gravity was last scaled by (`+0x24` over
    /// `+0x20`), and the height the slow motion began at (`+0x30`). [game]
    pub dilation: f32,
    gravity_scale: f32,
    dilation_from: Option<f32>,
    /// Whether the body touched the ground at the end of the last step, and for how long
    /// that has been so; the state machine's ground flag follows it late (`GROUND_FLAG_LAG`).
    touching: bool,
    touching_for: f32,
    /// The unfold started with the stick held: it runs out to the run (`+0x1c`). [game]
    unfold_run: bool,
    /// The car body's tilt, radians: nose up, and left side up.
    pub pitch: f32,
    pub roll: f32,
    /// Seconds left of the character's timer that a melee attack starts (his `+0x234`
    /// object's `+0x108`, 4.0). While it runs the camera is told to stand off
    /// (`FUN_0071a5c0`, the message's second flag byte, bit 0). The routine read to set it
    /// is the melee hit's (`FUN_007189b0`); the recording has the camera told from the
    /// update an attack state starts, hit or no hit, and that is what is done here. [trace]
    pub fight: f32,
    car: Car,
    /// The control state machine, the state it was in before this one (`+0x20`, which the
    /// fall, jump and weapon states look at) [game], and the clip the state is playing, if
    /// it plays one of its own.
    runner: Runner,
    previous_state: Option<usize>,
    pub action: Option<ActionState>,
    /// Seconds before another dodge may start, and the extra pull of a ground punch.
    dash_cooldown: f32,
    punch_gravity: f32,
    /// The climb mode, while he is on a wall.
    pub climb: Option<Climb>,
    /// The climb mode's top target and facing there (`+0x2c`, `+0x38`) as the last climb left
    /// them: the mode object outlives the climb, and the jump state reads them. [game]
    climb_top: Vec3,
    climb_top_facing: Vec2,
    /// What the climb mode told the state machine this update, for the next to act on.
    messages: Vec<u32>,
    /// The levels of the three melee upgrades (`melee::UPGRADES`); nothing raises them yet.
    pub melee_levels: [u32; 3],
    /// The hits that landed in the last step, for whoever owns the targets to act on.
    pub hits: Vec<Hit>,
    pub surface_hits: Vec<crate::ssd::Hit>,
    surface_hit_list: Vec<usize>,
    /// What the car's updates showed its gearbox in the last step, one entry an update, for
    /// whoever owns the engine's sound. [game]
    pub gearbox_inputs: Vec<GearboxInput>,
    pub vehicle_gearbox: Option<crate::sound::Gearbox>,
    /// The same for the tyres' and the suspension's sounds (`sound::tyre_sounds`). [game]
    pub tyre_inputs: Vec<TyreInput>,
    /// Vehicle convex contacts generated by this frame's physics, for CollisionFX.
    pub vehicle_contacts: Vec<crate::vehicle::Contact>,
    /// The targets the attack state he is in has hit already (owner `+0x228`): a target is
    /// hit once per attack state, and the list is emptied when the state is left
    /// (`FUN_00878be0`). [game]
    hit_list: Vec<u32>,
    /// The explosions his clips have set off that are still there.
    pub blasts: Vec<Blast>,
    /// The camera shakes and pad rumbles started in the last step, for whoever owns the
    /// camera and the pad.
    pub shakes: Vec<ShakeStart>,
    pub rumbles: Vec<Rumble>,
    /// The trigger messages his clip sent in the last step, by the `LuxScriptName` each is
    /// for: whoever draws starts the effects on his object that go by that name.
    pub triggers: Vec<u32>,
    /// The firing state (`stateWeaponModeAttack`, an overlay state) is running and its fire
    /// button was down at its last update: it told the weapon manager "trigger held"
    /// (`FUN_00878740` -> `FUN_007b37f0`). When this goes false the trigger was let go
    /// (`FUN_007b3a20`) or the state ended. [game]
    pub firing: bool,
    /// The special ability: seconds before it may be used again (his damage object's
    /// `+0x84`; the whole wait is its `+0x88`) and seconds it still counts as on
    /// (`+0x8c`, `specialAbilityDuration`). [game]
    pub special_cooldown: f32,
    pub special_left: f32,
    /// His control mode is the car's (character `+0x31c` is 3): from the enter of a drive
    /// state (`FUN_0087ba10`, so from the START of the change into the car) to the enter
    /// of a state whose `robotForm` puts him back on foot, or of a weapon state. The
    /// weapon manager takes the car's weapon in hand, and starts its wait, when this
    /// changes (`FUN_007b43b0`). [game]
    pub car_mode: bool,
    /// He has no health left; DeathSet's entry and exit rules run on the base layer. [game]
    pub death: Option<Death>,
    /// Animation damage kind supplied by the receiving side (`C_DAMAGE_TYPE`). [game]
    pub damage_type: u32,
    /// Character's special-idle countdown, initialized to 8 (00cad478). [game]
    pub idle_delay: f32,
    /// The additive layer above the weapon arm, started by stateHitReactOverlay. [game]
    pub hit_partial: Base,
    hit_pending: Option<bool>,
    hit_full: bool,
    hit_flyback: bool,
    hit_start_z: f32,
    hit_last: Option<(u32, u32, f32)>,
    /// Damage object's +0x108, set to 4 by full hit-reaction enter. [game]
    hit_cooldown: f32,
    stun_pending: bool,
    stun_full: bool,
    hit_direction: Vec3,
    wall_splatted: bool,
}

/// The base layer's clip as the picture needs it: the set and clip by name, the tick, the
/// seconds it fades in over, and a count that goes up each time a clip starts.
#[derive(Clone, Copy, Debug)]
pub struct BaseClip<'a> {
    pub set: &'a str,
    pub id: &'a str,
    pub tick: f32,
    pub blend: f32,
    pub serial: u32,
}

/// The death clip playing: its id by CRC-32, its tick, where it started from and facing.
#[derive(Clone, Debug)]
pub struct Death {
    pub clip: u32,
    pub tick: f32,
    last_tick: f32,
    heading: f32,
    /// StateDeath's elapsed time, used by its 3s/10s completion clocks.
    pub age: f32,
    /// [game] BasicMotionMode enters with (1,1,1) for explosion deaths.
    pub root_z: bool,
}

/// A camera shake to start: the CRC-32 of its settings' name, and where it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShakeStart {
    pub name: u32,
    pub at: Vec3,
}

/// An explosion in the world: the ground punch's blast. An `Explosion` on a spawned object
/// whose sphere grows and hits what touches it, each thing once (`FUN_007e5f00`,
/// `FUN_007e60f0`). [game]
#[derive(Clone, Debug)]
pub struct Blast {
    /// The clip that set it off and which of the clip's blasts it is.
    pub clip: u32,
    index: usize,
    /// Set off by a script and not by a clip (the special ability's shockwave): `index` is
    /// then into his own object's spawners (`Actions::spawners`), or with `true` the car's.
    script: Option<bool>,
    /// Seconds still to wait before it starts, while that is not negative; then seconds
    /// since it started.
    delay: f32,
    age: f32,
    /// Its middle, and its sphere's radius now; nothing touches it before it has started.
    pub centre: Vec3,
    pub radius: f32,
    pub live: bool,
    /// What it has hit already (its list at `+0xb8`).
    hit: Vec<u32>,
    surfaces: Vec<usize>,
}

/// A control state that plays a clip of its own: an attack, a dodge, the ground punch, the
/// halves of the change of form.
#[derive(Clone, Debug)]
pub struct ActionState {
    /// The control state (an index into `Tuning::control`) and its clip, by the CRC-32 of the
    /// clip's id; the clip's tick, and whether a non-looping clip has run out.
    pub state: usize,
    pub clip: u32,
    pub tick: f32,
    pub finished: bool,
    /// The tick the step before, for the root motion in between.
    last_tick: f32,
    /// Events of the clip already passed.
    events_done: usize,
    /// Branch points passed, counting from 1 as the original does. [game]
    branch: i32,
    /// The attack button window, and whether the attack button went down inside it; when the
    /// window shuts on a press, the next hit may chain. [game]
    window_open: bool,
    pressed_in_window: bool,
    can_branch: bool,
    /// What the attack mode may do now (`AttackFaceAndSlideEvent`). [game]
    slide: bool,
    face: bool,
    stick: bool,
    /// He is attacking (owner `+0xc8` is 1): set by an `AttackCollEvent` that switches a
    /// body on, taken off by one that switches any off (`FUN_0074af60`). [game]
    pub hitting: bool,
    /// The bodies switched on, by the CRC-32 of their node's name. [game]
    pub live: Vec<u32>,
    /// The trigger messages the clip has sent that nothing has acted on yet, by the
    /// `LuxScriptName` they are for.
    triggers: Vec<u32>,
    /// The attack mode's target (its `+0x20`): looked for once and then kept, until an
    /// attack that hit a character is over. And whether this attack has hit one (`+0x18`).
    /// [game]
    pub target: Option<u32>,
    has_hit: bool,
    /// The velocity there was when the clip started, faded out over the clip's blend time.
    carry: Vec2,
    fade: f32,
    fade_time: f32,
    /// The heading a dodge keeps, and the one a clip that turns its root turns from.
    heading: f32,
    /// Started on the wall (the two jumps off it).
    pub from_climb: bool,
    /// The punch out of the car (`stateDriveAttackAirPunch`, in the first mode): its clip
    /// lifts him 3.08 before `stateGroundPunchFall` takes over. [assumed: the first mode's
    /// control vector lets the clip's height through there, as Idle's update sets it on the
    /// ground; the state's own enter was not read for this]
    pub lifted: bool,
}

/// A car frame for presentation only; gameplay continues to use State's current pose.
#[derive(Clone, Copy, Debug)]
pub struct CarPresentation {
    pub pos: Vec3,
    pub rotation: Quat,
    pub velocity: Vec3,
    pub camera_velocity: Vec3,
    pub steer_angle: f32,
    pub wheels: [crate::vehicle::Wheel; 4],
}

impl CarPresentation {
    /// Yaw, pitch, roll in the same order as vehicle::rotation.
    pub fn angles(&self) -> (f32, f32, f32) { self.rotation.to_euler(EulerRot::ZXY) }

    fn interpolate(self, next: Self, alpha: f32) -> Self {
        let mut wheels=self.wheels;
        for (wheel,new) in wheels.iter_mut().zip(next.wheels) {
            wheel.render_spin=wheel.render_spin+(new.render_spin-wheel.render_spin)*alpha;
            // Contact changes are discrete. Keep the older contact through this interval;
            // interpolate the contact plane only while both frames have wheel support.
            if wheel.grounded && new.grounded {
                wheel.point=wheel.point.lerp(new.point,alpha);
                wheel.normal=wheel.normal.lerp(new.normal,alpha).normalize_or_zero();
            }
        }
        Self { pos:self.pos.lerp(next.pos,alpha), rotation:self.rotation.slerp(next.rotation,alpha),
            velocity:self.velocity.lerp(next.velocity,alpha),
            camera_velocity:self.camera_velocity.lerp(next.camera_velocity,alpha),
            steer_angle:self.steer_angle+(next.steer_angle-self.steer_angle)*alpha,wheels }
    }
}

/// What the car form keeps between updates.
#[derive(Clone, Copy, Default, Debug)]
struct Car {
    chassis: crate::vehicle::Chassis,
    /// Time not yet used up by whole updates of the car's rules.
    clock: f32,
    previous_presentation: Option<CarPresentation>,
    /// Turning rate about the vertical, radians a second, positive toward -x like `yaw`.
    yaw_rate: f32,
    /// How fast each wheel spins, radians a second: front left, front right, rear left, rear right.
    spin: [f32; 4],
    /// The speed being brought down while the brake is held, and after a turbo.
    braking_from: Option<f32>,
    after_turbo: Option<f32>,
    brake_held: f32,
    reversing: bool,
    /// The turbo button went down since the last update.
    turbo_asked: bool,
    /// The speed after the caps and before the wheels pushed.
    capped: f32,
    /// No wheel is on the ground.
    airborne: bool,
    /// [game] Drift turbo's charge/ready/active states, and DriveMode +0x204.
    drift_turbo: crate::driving::DriftTurbo,
    filtered_trigger: f32,
    sliding: bool,
    /// Time in the drive mode (its `+0x1fc`), counted in its updates. [game]
    time: f32,
}

impl State {
    /// The clip that leads the pace on the ground right now.
    pub fn pace(&self) -> Pace {
        self.layers.last().map_or(Pace::Idle, |layer| layer.pace)
    }

    pub fn new(tuning: &Tuning) -> Self {
        Self {
            pos: Vec3::ZERO,
            vel: Vec3::ZERO,
            yaw: 0.0,
            mode: Mode::Stand,
            gait: Gait::Idle,
            speed: 0.0,
            turbo_left: 0.0,
            turbo_cooldown: 0.0,
            turbo_upgrades: [0; 2],
            turbo_started: false,
            turbo_fx_on: false,
            steer_angle: 0.0,
            health: tuning.base_health,
            since_damage: f32::MAX,
            damage_timers: crate::damage::Runtime::default(),
            heat_damage: Vec::new(),
            actor: crate::damage::Actor { id: 0, team: tuning.damage.team, player: true, character: crc32(tuning.name.as_bytes()), ..Default::default() },
            difficulty: tuning.damage.difficulties[1],
            restart_requested: false,
            regen_delay: tuning.health_regen_delay,
            launch_z: 0.0,
            apex_z: 0.0,
            layers: Vec::new(),
            strafe: Strafe::default(),
            base: Base::default(),
            jump_type: 0,
            fall_time: 0.0,
            air_time: 0.0,
            jump_time: 0.0,
            touching: true,
            touching_for: f32::MAX,
            unfold_run: false,
            pitch: 0.0,
            roll: 0.0,
            fight: 0.0,
            car: Car::default(),
            runner: Runner::default(),
            previous_state: None,
            action: None,
            dash_cooldown: 0.0,
            punch_gravity: 0.0,
            dilation: 1.0,
            gravity_scale: 1.0,
            dilation_from: None,
            climb: None,
            climb_top: Vec3::ZERO,
            climb_top_facing: Vec2::ZERO,
            messages: Vec::new(),
            melee_levels: [0; 3],
            hits: Vec::new(),
            surface_hits: Vec::new(),
            surface_hit_list: Vec::new(),
            gearbox_inputs: Vec::new(),
            vehicle_gearbox: None,
            tyre_inputs: Vec::new(),
            vehicle_contacts: Vec::new(),
            hit_list: Vec::new(),
            blasts: Vec::new(),
            shakes: Vec::new(),
            rumbles: Vec::new(),
            triggers: Vec::new(),
            firing: false,
            special_cooldown: 0.0,
            special_left: 0.0,
            car_mode: false,
            death: None,
            damage_type: 0,
            idle_delay: 8.0,
            hit_partial: Base::default(),
            hit_pending: None,
            hit_full: false,
            hit_flyback: false,
            hit_start_z: 0.0,
            hit_last: None,
            hit_cooldown: 0.0,
            stun_pending: false,
            stun_full: false,
            hit_direction: Vec3::ZERO,
            wall_splatted: false,
        }
    }

    pub fn dead(&self) -> bool {
        self.death.is_some()
    }

    /// The clip a control state or the climb is playing: set, id and tick.
    pub fn action_clip<'a>(&self, t: &'a Tuning) -> Option<(&'a str, &'a str, f32)> {
        if let Some(death) = &self.death {
            let clip = t.actions.clips.get(&death.clip)?;
            return Some((&clip.set, &clip.id, death.tick));
        }
        if let Some(climb) = &self.climb {
            let clip = climb.clip_data(t)?;
            return Some((&clip.set, &clip.id, climb.tick));
        }
        if self.mode == Mode::Strafe {
            let clip = t.weapon_set.clip(self.strafe.clip)?;
            return Some(("WeaponSet", &clip.name, self.strafe.tick));
        }
        if self.weapon_air() {
            if let Some(clip) = t.weapon_set.clip(self.strafe.clip) {
                return Some(("WeaponSet", &clip.name, self.strafe.tick));
            }
        }
        let action = self.action.as_ref()?;
        let clip = t.actions.clips.get(&action.clip)?;
        Some((&clip.set, &clip.id, action.tick))
    }

    /// The clip the base layer's own rules picked, when no control state, climb, death or
    /// weapon clip is playing in its place: standing, walking, running, jumping, falling and
    /// the landings. [game: the sets' entry and exit rules, `animrules::Base`]
    pub fn base_clip<'a>(&self, t: &'a Tuning) -> Option<BaseClip<'a>> {
        if self.action_clip(t).is_some() {
            return None;
        }
        let clip = self.base.current(&t.rule_sets)?;
        let set = t.rule_sets.get(&self.base.set)?;
        Some(BaseClip { set: &set.set_name, id: &clip.name, tick: self.base.tick, blend: self.base.blend, serial: self.base.serial })
    }

    /// His collision bodies that can hit right now, where they are in the world: the ones
    /// the clip has switched on, at the clip's tick. [game]
    pub fn attack_bodies<'a>(&self, t: &'a Tuning) -> Vec<(&'a melee::AttackBody, melee::Body)> {
        let Some(action) = self.action.as_ref().filter(|a| a.hitting) else { return Vec::new() };
        let Some(data) = t.actions.clips.get(&action.clip) else { return Vec::new() };
        let placed = Affine3A::from_rotation_translation(Quat::from_rotation_z(self.yaw), self.pos);
        data.bodies
            .iter()
            .filter(|body| action.live.contains(&body.node))
            .filter_map(|body| body.at(action.tick).map(|frame| (body, frame.moved(&placed))))
            .collect()
    }

    /// The class of the control state he is in (`StateAttack`, `StateIdle`...).
    pub fn state_class<'a>(&self, t: &'a Tuning) -> Option<&'a str> {
        self.runner.state.and_then(|state| t.control.states.get(state)).map(|def| def.class.as_str())
    }

    /// Includes the two active overlay levels, as the battle chatter checks do.
    pub fn has_state_class(&self, t: &Tuning, class: &str) -> bool {
        [self.runner.state, self.runner.overlay, self.runner.nested].into_iter().flatten()
            .any(|state| t.control.states.get(state).is_some_and(|def| def.class == class))
    }

    /// In the air with the weapon set playing (the weapon states' jump and fall): the jump
    /// mode then keeps him facing his aim and slows time near the top. [game]
    pub fn weapon_air(&self) -> bool {
        matches!(self.mode, Mode::Jump(_)) && self.strafe.clip != 0
    }

    /// The fade the weapon set's leading clip came in with, while that set plays: its own
    /// blend time, or the one the clip before it asked for on its way out. [data]
    pub fn weapon_fade(&self) -> Option<f32> {
        let playing = self.mode == Mode::Strafe || self.weapon_air();
        self.strafe.layers.last().filter(|layer| playing && !layer.action && layer.clip == self.strafe.clip).map(|layer| layer.blend)
    }

    /// The name of the overlay state running on top of the control state, if any.
    pub fn overlay_state<'a>(&self, t: &'a Tuning) -> Option<&'a str> {
        self.runner.overlay.and_then(|state| t.control.states.get(state)).map(|state| state.name.as_str())
    }

    /// The name of the control state being run, for the readout.
    pub fn control_state<'a>(&self, t: &'a Tuning) -> Option<&'a str> {
        self.runner.state.and_then(|state| t.control.states.get(state)).map(|state| state.name.as_str())
    }

    /// Changing back to the robot after leaving the car.
    pub fn unfolding(&self) -> bool {
        self.mode == Mode::Unfold
    }

    /// On the ground as the state machine sees it: the body's contact, once it has held for
    /// an update. [trace]
    pub fn ground_flag(&self) -> bool {
        if self.touching_for >= GROUND_FLAG_LAG { self.touching } else { !self.touching }
    }

    /// The car is off the ground.
    pub fn car_airborne(&self) -> bool {
        self.mode == Mode::Drive && self.car.airborne
    }

    pub fn car_wheels(&self) -> &[crate::vehicle::Wheel; 4] { &self.car.chassis.wheels }

    fn car_snapshot(&self) -> CarPresentation {
        CarPresentation { pos:self.pos,rotation:crate::vehicle::rotation(self.yaw,self.pitch,self.roll),
            velocity:self.vel,camera_velocity:self.camera_velocity(),steer_angle:self.steer_angle,
            wheels:self.car.chassis.wheels }
    }

    /// [assumed] Interpolate the completed car frames, one 32ms physics step behind,
    /// for the body, wheel models and camera together. The original render scheduler is
    /// unread; this adapter removes fixed-step display jitter without changing physics.
    /// History is reset on drive entry/reset. See notes/status.md vehicle assumption 8.
    pub fn car_presentation(&self) -> CarPresentation {
        let current=self.car_snapshot();
        if self.mode!=Mode::Drive {return current;}
        self.car.previous_presentation.map_or(current,|previous|
            previous.interpolate(current,(self.car.clock/GAME_UPDATE).clamp(0.0,1.0)))
    }

    /// The car is sliding on its slide button.
    pub fn sliding(&self) -> bool {
        self.mode == Mode::Drive && self.car.sliding
    }

    pub fn facing(&self) -> Vec2 {
        Vec2::new(-self.yaw.sin(), self.yaw.cos())
    }

    /// [game: 0071af5c] The character object's forward row, used for aim outside
    /// weapon mode. Roll does not change the nose direction; car pitch does.
    pub fn body_forward(&self) -> Vec3 {
        let pitch = if self.is_vehicle() { self.pitch } else { 0.0 };
        (self.facing() * pitch.cos()).extend(pitch.sin())
    }

    /// The velocity the camera is told about. In the car that is the speed after the caps
    /// (35, or 43 in turbo), not the body's, which runs a little past them: in the recording
    /// the driving camera's view only widens in turbo. [trace]
    pub fn camera_velocity(&self) -> Vec3 {
        if self.mode == Mode::Drive {
            let flat = Vec2::new(self.vel.x, self.vel.y).clamp_length_max(self.car.capped);
            Vec3::new(flat.x, flat.y, self.vel.z)
        } else {
            self.vel
        }
    }

    pub fn is_vehicle(&self) -> bool {
        self.mode == Mode::Drive
    }

    pub fn damage(&mut self, amount: f32) {
        if self.death.is_some() {
            return;
        }
        self.health = (self.health - amount).max(0.0);
        self.since_damage = 0.0;
        self.damage_timers.stop_regen(self.regen_delay);
    }

    /// A received hit, with its already-scaled damage, game damage flags and resolved
    /// impulse. Receiving-side scaling/collision stays with the caller. [game: reaction
    /// kind FUN_0072ddf0; full vs overlay FUN_00729120, single-player, no immunities]
    pub fn receive_hit(&mut self, amount: f32, flags: u32, impulse: Vec3) {
        if self.dead() { return; }
        self.damage(amount);
        self.hit_direction = impulse;
        self.damage_type = if self.health <= 0.0 && flags & melee::damage::BY_EXPLOSION != 0 { 0xf712_8ccc }
            else { animation_damage_kind(flags) };
        if flags & melee::damage::THROWS != 0 {
            self.vel = impulse;
        }
        let flail = flags & (melee::damage::BY_FAST_FLAIL | melee::damage::BY_STRONG_FLAIL) != 0;
        self.hit_pending = Some(flags & 0x17_0100 != 0 && (self.hit_cooldown <= 0.0 || flail));
    }

    /// Every gameplay hit uses the shared receiver. `receive_hit` is only the capture
    /// harness for already resolved damage/impulse. [game: 00719830 / 00729120]
    pub fn receive_damage(&mut self, packet: crate::damage::Packet, t: &Tuning) -> crate::damage::Resolved {
        use crate::damage::{self, flags};
        let shield = if self.special_left > 0.0 { match t.special.kind {
            0 => Some(t.damage.shield_multiplier), 1 => Some(t.damage.flight_shield_multiplier), _ => None,
        }} else { None };
        let resolved = damage::resolve(packet, damage::Context { actor: self.actor, health: self.health,
            vehicle: self.is_vehicle(), shield, enabled: !self.dead() && self.health > 0.0,
            reject: false, invincible: t.damage.invincible, controller_accepts: true, external_health: false,
            difficulty: self.difficulty, immune_to_overdrive:false, double_damage:false }, &t.damage);
        if !resolved.accepted { return resolved; }
        self.health = resolved.health.min(t.base_health);
        self.since_damage = 0.0;
        self.hit_direction = packet.direction;
        self.damage_timers.receive(packet, shield.is_some(), t.health_regen_delay, &t.damage);
        if shield.is_none() && packet.heat>0.0 && packet.flags & (flags::OVERHEAT_CURRENT|flags::OVERHEAT_ALL)!=0 {self.heat_damage.push((packet.flags,packet.heat));}
        if let Some(velocity) = resolved.velocity && !t.damage.immovable { self.vel = velocity; }
        self.damage_type = if self.health <= 0.0 { resolved.death_kind } else { animation_damage_kind(resolved.flags) };
        let immune = t.damage.reaction_immunity;
        if shield.is_none() && packet.flags & flags::NO_HIT_REACT == 0 {
            if packet.flags & flags::STUN != 0 && immune & flags::STUN == 0 && self.damage_timers.stunned > 0.0 {
                self.stun_pending = true;
            } else {
                let flail = packet.flags & (flags::BY_FAST_FLAIL | flags::BY_STRONG_FLAIL) != 0;
                self.hit_pending = Some(packet.flags & 0x17_0100 != 0 && immune & flags::BY_MELEE == 0 &&
                    !self.is_vehicle() && (self.hit_cooldown <= 0.0 || flail));
            }
        }
        resolved
    }
}

/// The executable's damage flags -> animation variable. [game: FUN_0072ddf0]
pub fn animation_damage_kind(flags: u32) -> u32 {
    if flags & (1 << 17) != 0 { 0xfea0_bd86 }
    else if flags & (1 << 16) != 0 { 0x6a58_53c5 }
    else if flags & (1 << 18) != 0 { 0xd475_6749 }
    else if flags & (1 << 20) != 0 { 0x37b0_d2e1 }
    else if flags & 0x8_0200 != 0 { 0xe968_e103 }
    else if flags & (1 << 8) != 0 { 0xd475_6749 }
    else { 0 }
}

fn yaw_of(direction: Vec2) -> f32 {
    (-direction.x).atan2(direction.y)
}

/// Turns `yaw` toward `target` by at most `max_step`, the short way round.
fn turn_toward(yaw: f32, target: f32, max_step: f32) -> f32 {
    let mut delta = (target - yaw) % std::f32::consts::TAU;
    if delta > std::f32::consts::PI {
        delta -= std::f32::consts::TAU;
    } else if delta < -std::f32::consts::PI {
        delta += std::f32::consts::TAU;
    }
    yaw + delta.clamp(-max_step, max_step)
}

fn approach(value: f32, target: f32, max_step: f32) -> f32 {
    value + (target - value).clamp(-max_step, max_step)
}

/// Speed, control share and turn speed of a jump type. [game]
fn jump_kind(tuning: &Tuning, kind: JumpType) -> JumpKind {
    match kind {
        JumpType::Standing | JumpType::Fall => tuning.jump.standing,
        JumpType::Moving => tuning.jump.run,
        JumpType::HighStanding => tuning.jump.high_standing,
        JumpType::Long => tuning.jump.long,
        // Its own speed, the standing jump's turn, and no hold on it in the air. [game]
        JumpType::Climb => JumpKind {
            height: tuning.jump.climb_off_height,
            speed: tuning.jump.climb_off_speed,
            control: 0.0,
            turn_speed: tuning.jump.standing.turn_speed,
        },
    }
}

/// How high a jump type goes. The original does not use each type's own height: the running
/// jump uses the run height, the standing jump the run height and a fifth more, and both jumps
/// out of vehicle form the long jump's height. [game]
pub fn jump_height(tuning: &Tuning, kind: JumpType) -> f32 {
    match kind {
        JumpType::Standing | JumpType::Fall => tuning.jump.run.height * STANDING_JUMP_SCALE,
        JumpType::Moving => tuning.jump.run.height,
        JumpType::HighStanding | JumpType::Long => tuning.jump.long.height,
        // Every jump that starts in the climb set is this type, the one up the wall included
        // (its own height, `climbJumpUpHeight`, belongs to a type whose start is not read;
        // Bumblebee's two are both 15). [game]
        JumpType::Climb => tuning.jump.climb_off_height,
    }
}

pub fn step(s: &mut State, input: &Input, t: &Tuning, arena: &dyn World, dt: f32) {
    s.hits.clear();
    s.surface_hits.clear();
    s.shakes.clear();
    s.rumbles.clear();
    s.vehicle_contacts.clear();
    s.triggers.clear();
    s.turbo_started = false;
    s.fight = (s.fight - dt).max(0.0);
    advance(s, input, t, arena, dt);
    // The original's hits are contacts found by the physics step, which follows the modes.
    land_hits(s, t, arena);
    blast_hits(s, t, arena, dt);
    hit_rumbles(s, t);
}

/// Starts one of the pad's preset rumbles by its number (`FUN_00717c40` -> `FUN_007dc0a0`).
/// [game] He is always a local player's here, which is the original's condition.
fn preset_rumble(s: &mut State, t: &Tuning, number: usize) {
    if let Some(p) = t.rumble_presets.get(number) {
        s.rumbles.push(Rumble { kind: p.kind, duration: p.duration, strength: p.strength });
    }
}

/// The attacker's side of a hit that a target took (`FUN_00717d80`, called from the
/// target's damage routine): a hit whose flags have `DAMAGE_BY_MELEE` rumbles his pad, the
/// strong preset while the clip he is playing is the charge or the slam out of the car,
/// else the fast one. [game] The ground punch's blast carries the melee flag too, so each
/// target it reaches does the same, in place of the blast's own rumble that was running.
/// [assumed: the blast's damage record names him as the attacker] [stand-in: the dummies
/// take every hit; the original's target can refuse one (`FUN_00719830`'s first tests)]
/// [data] His pack supplies neither soundTable field406e38ba nor0ae3b908, so the
/// optional attacker-hit sounds at+284/+288 are both zero (notes/melee.md).
fn hit_rumbles(s: &mut State, t: &Tuning) {
    let strong = s
        .action
        .as_ref()
        .is_some_and(|a| a.clip == crc32(b"AttackFast_3_ChargeAttk") || a.clip == crc32(b"Trans_Vehicle2RobotSlam"));
    for _ in 0..s.hits.iter().filter(|hit| hit.flags & melee::damage::BY_MELEE != 0).count() {
        preset_rumble(s, t, if strong { rumble::preset::STRONG_HIT_ATTACKER } else { rumble::preset::FAST_HIT_ATTACKER });
    }
}

/// The explosions his clips set off (`ClipBlast`): the ground punch's blast. Each update of
/// one (`FUN_007e5f00`): while its delay has not run out, that counts down and nothing
/// else happens; then its age goes up and its sphere is sized by it, and once the age was
/// past `explosion_time` both before and after the update it is gone. What its sphere
/// touches is hit, once each (`FUN_007e60f0`): the damage by the distance from its middle,
/// the blow flat from its middle to where the target stands, the throw its own. With
/// `attachToBone` it rides the node it was put at. [game] The melee upgrades and Optimus's
/// scalings are not applied to it: they sit in the fist's routine (`FUN_007189b0`). [game]
/// [assumed: an explosion's first update is the one after the trigger; it is at
/// `start_scale` until its age first goes up; a target standing at its very middle is
/// thrown nowhere (the original takes the contact's normal there)] [stand-in: the touch,
/// and the target's side of it, as for the fists; once the clip that set it off is left
/// it stays where it was, where the original's stays on the hand]
fn blast_hits(s: &mut State, t: &Tuning, arena: &dyn World, dt: f32) {
    let placed = Affine3A::from_rotation_translation(Quat::from_rotation_z(s.yaw), s.pos);
    let playing = s.action.as_ref().map(|a| (a.clip, a.tick));
    let mut blasts = std::mem::take(&mut s.blasts);
    blasts.retain_mut(|blast| {
        let data = match blast.script {
            None => t.actions.clips.get(&blast.clip).and_then(|clip| clip.blasts.get(blast.index)),
            Some(false) => t.actions.spawners.get(blast.index),
            Some(true) => t.actions.vehicle_spawners.get(blast.index),
        };
        let Some(data) = data else { return false };
        let def = &data.explosion;
        if blast.delay >= 0.0 {
            blast.delay -= dt;
            if blast.delay >= 0.0 {
                return true;
            }
            blast.live = true;
        } else {
            let before = blast.age;
            blast.age += dt;
            if blast.age > def.time && before > def.time {
                return false;
            }
        }
        blast.radius = def.radius * if blast.age > 0.0 { def.scale(blast.age, dt) } else { def.start_scale };
        if blast.script.is_some() {
            // A script's explosion rides his own node, wherever he goes. [game:
            // `attachToBone`]
            if data.attach {
                blast.centre = placed.transform_point3(data.centre(0.0));
            }
        } else if let Some((_, tick)) = playing.filter(|&(clip, _)| data.attach && clip == blast.clip) {
            blast.centre = placed.transform_point3(data.centre(tick));
        }
        let sphere = melee::Body { shape: Shape::Sphere { radius: blast.radius }, at: Affine3A::from_translation(blast.centre) };
        if !data.ssd.is_empty() {
            for contact in arena.surface_contacts(&sphere) {
                if !blast.surfaces.contains(&contact.surface) {
                    blast.surfaces.push(contact.surface);
                    s.surface_hits.push(crate::ssd::Hit { contact, infos:Some(data.ssd.clone()) });
                }
            }
        }
        let (damage, min_damage) = def.owned_damage(t.ground_pound_damage, t.ground_pound_damage_weak);
        for target in arena.targets() {
            if blast.hit.contains(&target.id) {
                continue;
            }
            let Some(point) = melee::touch(&sphere, &melee::Body::of_target(target)) else { continue };
            s.hits.push(Hit {
                target: target.id,
                clip: blast.clip,
                damage: def.damage_at(damage, min_damage, point.distance(blast.centre), blast.radius),
                point,
                body: data.node,
                direction: Vec2::new(target.pos.x - blast.centre.x, target.pos.y - blast.centre.y).normalize_or_zero(),
                knock_back_speed: def.knock_back_speed,
                knock_back_angle: def.knock_back_angle.to_radians(),
                flags: def.damage_flags,
                surface: target.surface,
                blast: true,
                stun_time: def.stun_time,
            });
            blast.hit.push(target.id);
        }
        true
    });
    // The triggers the clip sent this update: each spawner by that name puts its object at
    // its node (`FUN_007fc270`). [game]
    if let Some(action) = s.action.as_mut() {
        let clip = action.clip;
        for name in action.triggers.drain(..) {
            s.triggers.push(name);
            let Some(data) = t.actions.clips.get(&clip) else { continue };
            for (index, blast) in data.blasts.iter().enumerate().filter(|(_, b)| b.name == name) {
                let centre = placed.transform_point3(blast.centre(action.tick));
                // The explosion starts its camera shake where it is (`FUN_007e5d30`), and
                // the object's `TriggerOnSpawn` its rumble, by each player's distance from
                // the object (`FUN_007dbdc0`). [game] [assumed: the explosion's routine
                // runs as the object is put there, not after its delay (both of his are
                // 0); the player's place for the distance is where he stands]
                if blast.explosion.camera_shake != 0 {
                    s.shakes.push(ShakeStart { name: blast.explosion.camera_shake, at: centre });
                }
                if let Some(rumble) = &blast.rumble
                    && let Some(strength) = rumble.strength_at(centre.distance(s.pos))
                {
                    s.rumbles.push(Rumble { kind: rumble.kind, duration: rumble.duration, strength });
                }
                blasts.push(Blast {
                    clip,
                    index,
                    script: None,
                    delay: blast.explosion.delay,
                    age: 0.0,
                    centre,
                    radius: blast.explosion.radius * blast.explosion.start_scale,
                    live: false,
                    hit: Vec::new(),
                    surfaces: Vec::new(),
                });
            }
        }
    }
    s.blasts = blasts;
}

/// While he is attacking, each target one of his switched-on bodies touches is hit, once
/// per attack state (`FUN_00719540` -> `FUN_007189b0`): the damage is the clip's, the blow
/// goes the way its `attack_dir` says and carries its throw. The bodies are his own, where
/// the clip has them at this tick; a foot's touch counts only against a character. [game]
/// The damage is scaled by the melee upgrade the clip belongs to (`FUN_0079d060`). [game]
/// [stand-in: the target's side of the touch, one upright cylinder] The other scalings are
/// not his: Optimus's special ability (`FUN_0079b680`, `FUN_0079b6d0`) and the difficulty's
/// `scaleEnemyDamage` for attackers no player runs. The hit carries the clip's damage type
/// flags for the target's size and the surface it struck, by which whoever owns the sound
/// picks the hit's sound from his impact preset (`FUN_0072d9c0`). [game]
fn land_hits(s: &mut State, t: &Tuning, arena: &dyn World) {
    let Some(action) = s.action.as_ref().filter(|a| a.hitting) else { return };
    let Some(data) = t.actions.clips.get(&action.clip) else { return };
    let (clip, facing) = (action.clip, s.facing());
    let bodies = s.attack_bodies(t);
    // [game] Non-character surfaces count for hand bodies; feet count only on characters.
    // [stand-in] The host supplies arena contacts rather than the original level bodies.
    for (_,body) in bodies.iter().filter(|(def,_)| !def.foot) {
        for contact in arena.surface_contacts(body) {
            if !s.surface_hit_list.contains(&contact.surface) {
                s.surface_hit_list.push(contact.surface);
                s.surface_hits.push(crate::ssd::Hit { contact, infos:None });
            }
        }
    }
    let Some(attack) = data.attack else { return };
    for target in arena.targets() {
        if s.hit_list.contains(&target.id) {
            continue;
        }
        let theirs = melee::Body::of_target(target);
        let touched = bodies
            .iter()
            .filter(|(body, _)| target.character || !body.foot)
            .find_map(|(body, mine)| melee::touch(mine, &theirs).map(|point| (body.node, point)));
        let Some((body, point)) = touched else { continue };
        s.hits.push(Hit {
            target: target.id,
            clip,
            // He is a player's, and this is not multiplayer.
            damage: attack.damage_for(s.actor.player, false) * if s.actor.player { t.melee.upgrade_scale(clip, s.melee_levels) } else { s.difficulty.enemy_damage },
            point,
            body,
            direction: melee::attack_direction(attack.attack_dir, facing, s.pos, target.pos),
            knock_back_speed: attack.knock_back_speed,
            knock_back_angle: attack.knock_back_angle.to_radians(),
            flags: attack.flags_for(target.character.then_some(target.large as u8)),
            surface: target.surface,
            blast: false,
            stun_time: 0.0,
        });
        s.hit_list.push(target.id);
    }
}

/// StateDeath's enter: BasicMotionMode with XY control (except explosion deaths), then
/// DeathSet's entry walk. Form 3 is the control mode, including the transform's start.
/// [game: FUN_0087b070 / FUN_0070cae0]
fn die(s: &mut State, input: &Input, t: &Tuning) {
    let from = playing_clip(s, t);
    let fallback = crc32(if s.car_mode { b"Death_V2R".as_slice() } else { b"Death".as_slice() });
    s.base = Base::default();
    base_enter(s, input, t, crc32(b"DeathSet"), from);
    let clip = s.base.playing().map_or(fallback, |(_, id)| id);
    s.death = Some(Death { clip, tick: s.base.tick, last_tick: s.base.tick, heading: s.yaw, age: 0.0, root_z: s.damage_type == 0xf712_8ccc });
    (s.mode, s.action, s.climb, s.runner.state, s.runner.overlay) = (Mode::Stand, None, None, None, None);
    (s.firing, s.speed, s.turbo_left, s.dilation, s.gravity_scale) = (false, 0.0, 0.0, 1.0, 1.0);
    s.turbo_fx_on = false;
    s.layers.clear();
    s.strafe = Strafe::default();
    s.hit_partial = Base::default();
    s.hit_pending = None;
    s.hit_full = false;
    s.stun_full = false;
    s.stun_pending = false;
    if let Some(index) = t.control.states.iter().position(|def| def.class == "StateDeath") {
        s.runner.enter(&t.control, index);
    }
}

/// [game] Ordinary death controls XY root motion, explosion death controls XYZ.
/// BasicMotionMode enter 00856cc0 and StateDeath enter 0087b070.
fn step_death(s: &mut State, input: &Input, t: &Tuning, arena: &dyn World, dt: f32) {
    let old = s.base.playing();
    let before = s.base.tick;
    let motion_scale=if s.damage_timers.slowed>0.0 {t.damage.slow_multiplier} else {1.0};
    base_step(s, input, t, dt*motion_scale);
    let Some(death) = s.death.as_mut() else { return };
    death.age += dt;
    if let Some((_, clip)) = s.base.playing() {
        death.clip = clip;
        death.last_tick = if old == s.base.playing() { before } else { 0.0 };
        death.tick = s.base.tick;
    }
    let Some(data) = t.actions.clips.get(&death.clip) else { return };
    if s.base.playing().is_none() {
        death.last_tick = death.tick;
        death.tick = (death.tick + dt * 60.0 * data.speed).min(data.ticks);
    }
    let moved = if dt > 1e-6 { (data.root_at(death.tick) - data.root_at(death.last_tick)) / dt } else { Vec3::ZERO };
    let facing = Vec2::new(-death.heading.sin(), death.heading.cos());
    let right = Vec2::new(facing.y, -facing.x);
    let flat = right * moved.x + facing * moved.y;
    (s.vel.x, s.vel.y) = (flat.x, flat.y);
    s.pos.x += s.vel.x * dt;
    s.pos.y += s.vel.y * dt;
    let floor = arena.floor(s.pos.x, s.pos.y, s.pos.z);
    if death.root_z {
        s.vel.z = moved.z;
        s.pos.z = (s.pos.z + s.vel.z * dt).max(floor);
    } else if s.pos.z > floor + 0.01 {
        s.vel.z -= t.gravity * dt;
        s.pos.z = (s.pos.z + s.vel.z * dt).max(floor);
    } else {
        s.pos.z = floor;
        s.vel.z = 0.0;
    }
    let finished = data.ticks <= death.tick && !data.looping;
    let age = death.age;
    arena.push_out(&mut s.pos, t.robot_radius, t.robot_height);
    touch(s, s.pos.z <= floor + 0.01, dt);
    s.restart_requested = crate::damage::death_finished(age, s.pos.z <= floor + 0.01, finished, false, false);
}

fn advance(s: &mut State, input: &Input, t: &Tuning, arena: &dyn World, dt: f32) {
    // Character update lowers this until negative; it is not reset by locomotion or damage.
    // [game: FUN_00713300 / FUN_0071dc20; the idle's own rules also require 8s unharmed]
    if s.idle_delay >= 0.0 { s.idle_delay -= dt; }
    s.hit_cooldown = (s.hit_cooldown - dt).max(0.0);
    s.since_damage += dt;
    if let Some(packet) = s.damage_timers.step(dt) { s.receive_damage(packet, t); }
    s.damage_timers.regenerate(&mut s.health, t.base_health, t.health_regen_rate > 0.0,
        t.health_regen_delay, &t.damage, dt);
    if s.health <= 0.0 && s.death.is_none() {
        die(s, input, t);
    }
    if s.death.is_some() {
        step_death(s, input, t, arena, dt);
        return;
    }
    // [game: 00733180/0085ed50] The standard turbo stays registered on the character
    // when DriveMode exits. Its FX stop, but its active/cooldown timers keep running.
    if crate::driving::step_turbo(&mut s.turbo_left, &mut s.turbo_cooldown,
        crate::driving::turbo_times(&t.drive, s.turbo_upgrades).1, dt) {
        s.car.after_turbo = Some(t.drive.max_turbo_speed);
        s.turbo_fx_on = false;
    }
    s.dash_cooldown = (s.dash_cooldown - dt).max(0.0);
    // His damage object's update (`FUN_0072a550`) runs the special's two clocks down.
    // [game] At the end of the second it sends the script its "off" message
    // (`FUN_0072b980`), which nothing of his listens for: his shockwave, particles and ring
    // have all ended themselves by then.
    s.special_cooldown = (s.special_cooldown - dt).max(0.0);
    s.special_left = (s.special_left - dt).max(0.0);

    // The state machine first, then the motion mode it leaves him in, in the same update: in
    // run 1 the update that starts a run already moves him, and the one that starts a jump
    // already applies the jump's air control. [trace]
    control(s, input, t, arena, dt);
    // [game] 0072a550 sets controller speed and animation delta308 to slowMultiplier-1.
    let motion_scale=if s.damage_timers.slowed>0.0 {t.damage.slow_multiplier} else {1.0};
    let animation_dt=dt*motion_scale;
    base_step(s, input, t, animation_dt);
    let mut partial = std::mem::take(&mut s.hit_partial);
    let base = playing_clip(s, t);
    partial.update_partial(&t.rule_sets, base, &|variable| rule_value(s, input, variable));
    partial.advance(&t.rule_sets, animation_dt);
    s.hit_partial = partial;
    match s.mode {
        Mode::Stand | Mode::Move => step_move(s, input, t, dt),
        Mode::Strafe => step_strafe(s, input, t, animation_dt),
        Mode::Jump(kind) => step_jump(s, input, t, kind, dt),
        Mode::Drive => step_drive(s, input, t, arena, dt),
        Mode::Unfold => step_unfold(s, input, t, dt),
        Mode::Action if s.hit_full || s.stun_full => step_hit_reaction(s, t, dt),
        Mode::Action => step_action(s, input, t, arena, animation_dt),
        Mode::Climb => step_climb(s, input, t, arena, animation_dt),
    }
    if motion_scale!=1.0 && !s.hit_flyback {
        s.vel.x*=motion_scale; s.vel.y*=motion_scale;
        if s.mode==Mode::Climb {s.vel.z*=motion_scale;}
    }
    if s.mode==Mode::Drive {
        arena.separate_targets(&mut s.pos,t.vehicle_radius,t.vehicle_height);
        s.car.airborne=!s.car.chassis.grounded();
        s.speed=s.vel.truncate().dot(s.facing());
        touch(s,!s.car.airborne,dt);
        return;
    }
    if s.mode == Mode::Climb || s.action.as_ref().is_some_and(|a| a.from_climb || a.lifted) {
        // On the wall the clips and the climb mode move him and nothing pulls him down; the
        // ground and the wall still hold him. [game: the climb's control vector is (1, 1, 1)]
        // [assumed: the same for the two jumps off it, until they take off]
        s.pos += s.vel * dt;
        arena.push_out(&mut s.pos, t.robot_radius, t.robot_height);
        s.pos.z = s.pos.z.max(arena.floor(s.pos.x, s.pos.y, s.pos.z));
        let floor = arena.floor(s.pos.x, s.pos.y, s.pos.z);
        touch(s, s.pos.z <= floor + 0.01, dt);
        return;
    }
    // In the jump mode the body's gravity carries the mode's slow motion. [game]
    let gravity = t.gravity * if matches!(s.mode, Mode::Jump(_)) { s.gravity_scale } else { 1.0 } + s.punch_gravity;

    // Integrate, then settle against the arena. In the air the height follows the exact arc for
    // steady gravity, so a jump peaks where its launch speed says whatever the step length.
    let airborne = matches!(s.mode, Mode::Jump(_))
        || s.car_airborne()
        || (s.mode != Mode::Drive && s.pos.z > arena.floor(s.pos.x, s.pos.y, s.pos.z) + STEP_HEIGHT)
        || (s.mode != Mode::Drive && s.vel.z > 0.0);
    let previous_feet = s.pos;
    s.pos.x += s.vel.x * dt;
    s.pos.y += s.vel.y * dt;
    if airborne {
        s.pos.z += s.vel.z * dt - 0.5 * gravity * dt * dt;
        s.vel.z -= gravity * dt;
        if s.vel.z < 0.0 {
            s.fall_time += dt;
        }
    }
    if !airborne && s.vel.z <= 0.0 {
        if let Some(z) = arena.follow_floor(previous_feet, s.pos.truncate()) {
            s.pos.z = z;
        }
    } else if s.vel.z <= 0.0 {
        // Sweep down before side collision so a falling body cannot skip a ramp top.
        // [stand-in] The arena's centre-only drop query also serves this landing sweep.
        if let Some(hit) = arena.drop_onto(Vec3::new(s.pos.x, s.pos.y, previous_feet.z), s.pos.z,
            if s.is_vehicle() { t.vehicle_radius } else { t.robot_radius }) {
            s.pos.z = hit.z;
        }
    }
    let (radius, height) = if s.is_vehicle() { (t.vehicle_radius, t.vehicle_height) } else { (t.robot_radius, t.robot_height) };
    let before_push = s.pos;
    let pushed = arena.push_out(&mut s.pos, radius, height);
    if pushed && s.hit_full && s.hit_flyback && !s.wall_splatted {
        // [stand-in] Arena push supplies the contact normal; a level host must provide
        // the actual physics contact flags and body. Character contacts are excluded.
        let normal = (s.pos - before_push).normalize_or_zero();
        let character = arena.targets().iter().any(|target| (s.pos - target.pos).truncate().length() <= radius + target.radius + 0.01);
        let finished = s.base.current(&t.rule_sets).is_some_and(|c| !c.looping && s.base.tick >= c.ticks);
        if crate::damage::wall_splat(normal, s.facing().extend(0.0), finished, character, false) {
            s.wall_splatted = true;
            s.damage_type = 0x5060_153e;
            s.yaw = yaw_of(normal.truncate());
            s.hit_flyback = false;
            s.hit_last = None;
            s.vel = Vec3::ZERO;
        }
    }
    // The car touching something at a speed over 5 (`FUN_00860870`, the drive mode's contact
    // handler; the speed is the body's, all three axes). [game] [assumed: every touch of a
    // wall is the contact kind 3 the original asks for; that the speed is the one before
    // the bounce] [stand-in: the arena's push for the contact]
    if pushed && s.mode == Mode::Drive && s.vel.length() > CAR_KNOCK_SPEED {
        preset_rumble(s, t, rumble::preset::VEHICLE_HIT_WALL);
    }
    if pushed && s.mode != Mode::Jump(JumpType::Fall) {
        // Hitting a wall scrubs the speed that was going into it. [stand-in]
        s.speed *= 0.5;
        if s.mode == Mode::Drive {
            s.vel.x *= 0.5;
            s.vel.y *= 0.5;
        }
    }
    let floor = arena.floor(s.pos.x, s.pos.y, s.pos.z);
    s.apex_z = s.apex_z.max(s.pos.z);
    match s.mode {
        // Whatever the mode, the body comes down on the ground and stays on it; which mode he
        // is in after a landing or after running off an edge is the state machine's to say
        // (`connectionOnGround`, `connectionFall`), not the body's. [game]
        _ if airborne => {
            if s.pos.z <= floor && s.vel.z <= 0.0 {
                s.pos.z = floor;
                s.vel.z = 0.0;
            }
        }
        _ if s.pos.z > floor + STEP_HEIGHT => {
            // Ran off an edge: he falls from the next update on, in whatever mode he is in.
        }
        _ => {
            // [stand-in] A step up is climbed at 12 a second instead of in one frame.
            s.pos.z = floor.min(s.pos.z + 12.0 * dt);
            s.vel.z = 0.0;
        }
    }
    let contact = match s.mode {
        Mode::Drive => !s.car.airborne,
        _ => s.pos.z <= floor + 0.01 && s.vel.z <= 0.0,
    };
    touch(s, contact, dt);
    tilt(s, t, arena, dt);
}

/// Keeps the body's contact with the ground, for the state machine's ground flag. Time going
/// down is counted from the moment he leaves the ground (`animRuleLongFall`). [trace]
fn touch(s: &mut State, contact: bool, dt: f32) {
    if contact == s.touching {
        s.touching_for += dt;
    } else {
        s.touching = contact;
        s.touching_for = 0.0;
        if !contact {
            s.fall_time = 0.0;
        }
    }
}

/// Out of vehicle form the rendered root returns upright. [stand-in: blend rate]
fn tilt(s: &mut State, _t: &Tuning, _arena: &dyn World, dt: f32) {
    s.pitch = approach(s.pitch, 0.0, 2.0 * dt);
    s.roll = approach(s.roll, 0.0, 2.0 * dt);
}

/// The pace on the ground for this step. The body moves at the root motion of the clips that
/// are playing, and clips cross-fade in a stack: each new one fades in, straight-line over its
/// own blend time, on top of whatever was there, which keeps fading and playing underneath.
/// Checked against the recorded game update by update (`notes\live-trace.md`): starts, stops
/// from a run and from a walk, and both landings. [trace]
fn step_pace(s: &mut State, walk: &Locomotion, dt: f32) -> f32 {
    let curve = |pace| match pace {
        Pace::Stop => Some(&walk.stop),
        Pace::LandRun => Some(&walk.land_run),
        Pace::BigLandRun => Some(&walk.big_land_run),
        _ => None,
    };
    let fade = |pace| match pace {
        Pace::Idle => walk.idle_blend,
        Pace::Walk => walk.walk_blend,
        Pace::Run => walk.run_blend,
        other => curve(other).map_or(0.0, |c| c.crossfade),
    };
    let top = s.pace();
    let finished = s.layers.last().is_some_and(|l| curve(l.pace).is_some_and(|c| l.time >= c.duration()));
    let moving = if s.gait == Gait::Run { Pace::Run } else { Pace::Walk };

    if finished {
        // A one-off that has played out hands over at once: the stop to standing, a landing
        // to the walk or run.
        s.layers.clear();
        if s.gait != Gait::Idle {
            s.layers.push(Layer { pace: moving, weight: 1.0, time: 0.0 });
        }
    } else {
        let next = match (s.gait, top) {
            (Gait::Idle, Pace::Idle | Pace::Stop) => None,
            // Only the run stops through the run-to-idle clip; anything else fades to idle.
            (Gait::Idle, Pace::Run) => Some(Pace::Stop),
            (Gait::Idle, _) => Some(Pace::Idle),
            // A landing plays out before the walk or run takes over.
            (_, Pace::LandRun | Pace::BigLandRun) => None,
            (_, top) if top == moving => None,
            _ => Some(moving),
        };
        if let Some(pace) = next {
            s.layers.push(Layer { pace, weight: 0.0, time: 0.0 });
        }
    }

    let mut speed = 0.0;
    for layer in &mut s.layers {
        let blend = fade(layer.pace);
        layer.weight = if blend > 0.0 { (layer.weight + dt / blend).min(1.0) } else { 1.0 };
        let own = match layer.pace {
            Pace::Idle => 0.0,
            Pace::Walk => walk.walk_speed,
            Pace::Run => walk.run_speed,
            other => curve(other).map_or(0.0, |c| c.speed(layer.time, layer.time + dt)),
        };
        layer.time += dt;
        speed += (own - speed) * layer.weight;
    }
    // Whatever lies under a clip that has fully faded in no longer counts.
    if let Some(full) = s.layers.iter().rposition(|l| l.weight >= 1.0) {
        s.layers.drain(..full);
    }
    if s.layers.len() == 1 && s.layers[0].pace == Pace::Idle {
        s.layers.clear();
    }
    speed
}

fn gait_of(deflection: f32, walk: &Locomotion) -> Gait {
    if deflection > walk.run_stick {
        Gait::Run
    } else if deflection > walk.move_stick {
        Gait::Walk
    } else {
        Gait::Idle
    }
}

/// Standing (the first mode) and MoveMode. Neither translates him: the clip that plays does,
/// by its root motion. The animation rules pick walk or run from the stick, and each clip has
/// one speed. [data] MoveMode turns him toward the stick; standing does not. [game]
fn step_move(s: &mut State, input: &Input, t: &Tuning, dt: f32) {
    let moving = s.mode == Mode::Move;
    let deflection = input.stick.length().min(1.0);
    let pushing = deflection * deflection > STICK_DEAD_SQUARED;
    if moving && pushing {
        s.yaw = turn_toward(s.yaw, yaw_of(input.stick), t.moving.turn_speed.to_radians() * dt);
    }
    let walk = &t.locomotion;
    s.gait = if moving { gait_of(deflection, walk) } else { Gait::Idle };
    let wanted = if let Some(moved) = clip_motion(s, t, dt) {
        // A state that plays its own clip in this mode (the first half of the change into the
        // car, while the stick does not yet line up): that clip's root moves him. [game]
        s.layers.clear();
        moved
    } else {
        s.speed = step_pace(s, walk, dt);
        s.facing() * s.speed
    };
    let horizontal = Vec2::new(s.vel.x, s.vel.y);
    let blended = if moving && !s.ground_flag() {
        // MoveMode off the ground follows the clips only a tenth of the way each update. [game]
        horizontal.lerp(wanted, 1.0 - (1.0 - MOVE_AIR_CONTROL).powf(dt * CONTROL_FRAME_RATE))
    } else {
        wanted
    };
    (s.vel.x, s.vel.y) = (blended.x, blended.y);
}

/// Standing in weapon mode his spine twists toward the aim no further than this; past it
/// his body turns by the rest. And the least squared stick StrafeMode counts as pushed.
/// [game]
const STRAFE_TWIST_MOST: f32 = 0.794_124_84;
const STRAFE_STICK_SQUARED: f32 = 0.0001;
const WEAPON_SET: u32 = crc32(b"WeaponSet");
/// The sets the control states start on the base layer, whose clips `animrules::Base` picks.
/// [game: `notes\state-machine.md`: Idle enter plays IdleStandSet, Move's WalkSet, Jump's
/// JumpSet (after the jump mode), Fall's FallSet (before it)]
const IDLE_SET: u32 = crc32(b"IdleStandSet");
const WALK_SET: u32 = crc32(b"WalkSet");
const JUMP_SET: u32 = crc32(b"JumpSet");
const FALL_SET: u32 = crc32(b"FallSet");
const DASH_SET: u32 = crc32(b"DashSet");

/// An angle brought into half a turn either side of zero, as the game does it.
fn wrapped(angle: f32) -> f32 {
    let turns = angle / std::f32::consts::TAU + 0.5;
    (turns - turns.floor() - 0.5) * std::f32::consts::TAU
}

/// What the animation rules of the weapon set ask about him. [game: `F_L_STICK_AMP` is the
/// stick's length, `F_L_STICK_DIR` the stick against the aim in degrees (`FUN_0070c380`);
/// `F_STRAFE_ANG` and `I_IS_STRAFING` are what StrafeMode last wrote; integer getter
/// `FUN_0070cae0` reads `I_IN_AIR` / `I_ON_GROUND` from the controller's ground flag]
fn rule_value(s: &State, input: &Input, variable: u32) -> Option<Value> {
    use crate::animrules::var;
    let stick = input.stick;
    Some(match variable {
        var::L_STICK_AMP => Value::Float(stick.length()),
        var::L_STICK_DIR => Value::Float(wrapped(yaw_of(stick) - s.strafe.aim_yaw).to_degrees()),
        var::STRAFE_ANG => Value::Float(s.strafe.angle),
        var::IS_STRAFING => Value::Int(s.strafe.strafing as i32),
        var::FALL_DUR => Value::Float(s.fall_time),
        var::Z_VEL => Value::Float(s.vel.z),
        var::IN_AIR => Value::Int(!s.ground_flag() as i32),
        var::ON_GROUND => Value::Int(s.ground_flag() as i32),
        // What the base layer's rules (idle, walk, jump, fall) ask besides. [game: the jump
        // mode writes `C_JUMP_TYPE`; `F_TIME_SINCE_DAMAGED` is now minus the last hit's time]
        // [assumed: nothing is carried, so the interaction type is none]
        var::JUMP_TYPE => Value::Hash(s.jump_type),
        var::INTERACT_TYPE => Value::Hash(0),
        var::SPECIAL_IDLE_ON => Value::Int((s.idle_delay < 0.0) as i32),
        var::TIME_SINCE_DAMAGED => Value::Float(s.since_damage),
        var::TIME_IN_ANIM => Value::Float(if s.strafe.clip != 0 {
            s.strafe.layers.last().map_or(0.0, |layer| layer.elapsed_ticks / 60.0)
        } else { s.base.seconds() }),
        var::CHAR_FORM => Value::Int(if s.car_mode { 3 } else if s.mode == Mode::Strafe || s.weapon_air() { 2 } else { 1 }),
        var::DAMAGE_TYPE => Value::Hash(s.damage_type),
        _ => return None,
    })
}

/// What the base layer's rules read: the same variables, with the playing clip's own time
/// (the controller is taken out of the state while it walks them).
fn base_value(s: &State, input: &Input, seconds: f32, variable: u32) -> Option<Value> {
    if variable == animrules::var::TIME_IN_ANIM {
        return Some(Value::Float(seconds));
    }
    rule_value(s, input, variable)
}

/// A control state starts one of the base layer's sets: its entry rules pick the clip,
/// coming from the set and clip that were playing (`from`). [game]
fn base_enter(s: &mut State, input: &Input, t: &Tuning, set: u32, from: (u32, u32)) {
    let mut base = std::mem::take(&mut s.base);
    let seconds = base.seconds();
    base.enter(&t.rule_sets, set, from, &|variable| base_value(s, input, seconds, variable));
    s.base = base;
}

/// The animation controller's update for the base layer: the playing clip's exit rules, then
/// the clip goes on playing. [game: the walk before the mode's own update, as the weapon
/// set's is]
fn base_step(s: &mut State, input: &Input, t: &Tuning, dt: f32) {
    if s.base.playing().is_none() {
        return;
    }
    let mut base = std::mem::take(&mut s.base);
    let seconds = base.seconds();
    base.update(&t.rule_sets, &|variable| base_value(s, input, seconds, variable));
    base.advance(&t.rule_sets, dt);
    s.base = base;
}

/// `C_JUMP_TYPE`'s value for each jump the mode picks (`notes\bumblebee-mechanics.md`,
/// "JumpMode"). [game]
fn jump_type_hash(kind: JumpType) -> u32 {
    match kind {
        JumpType::Standing | JumpType::Fall => 0xaa18_5eeb,
        JumpType::Moving => 0x0ec5_1d8e,
        JumpType::HighStanding => 0xf431_2e7f,
        JumpType::Long => 0x225a_6ca8,
        JumpType::Climb => 0xdfb7_146d,
    }
}

/// Starts a clip of the weapon set on top of what is playing; `blend` is the fade the clip
/// being left asks for, if it does. [game: a clip with `usePrevAnimFrame` starts at the
/// time the one before had reached; the fade is the new clip's `xfadeTime`, or the old
/// one's `xfadeTimeOnExit` when that names the new clip or none; keeping the previous
/// frame wins over continuing the old clip's overrun. FUN_00748f00]
fn strafe_start(s: &mut State, t: &Tuning, id: u32) {
    let Some(clip) = t.weapon_set.clip(id) else { return };
    let old = t.weapon_set.clip(s.strafe.clip);
    let handed = old.filter(|old| old.exit_crossfade >= 0.0 && (old.exit_crossfade_to == 0 || old.exit_crossfade_to == id));
    let blend = handed.map_or(if clip.crossfade < 0.0 { DEFAULT_CROSSFADE } else { clip.crossfade }, |old| old.exit_crossfade);
    let previous = s.strafe.layers.last();
    let elapsed = if clip.keeps_time { previous.map_or(s.strafe.tick, |layer| layer.elapsed_ticks) }
        else if old.is_some_and(|old| old.continue_time) { previous.map_or(0.0, |layer| layer.overrun) * clip.speed }
        else { 0.0 };
    let tick = if clip.looping { elapsed % clip.ticks.max(1.0) } else { elapsed.min(clip.ticks) };
    s.strafe.layers.push(RootLayer { clip: id, action: false, weight: 0.0, blend, tick, carry: Vec2::ZERO,
        elapsed_ticks: if clip.looping { elapsed } else { elapsed.min(clip.ticks) },
        overrun: if clip.looping { 0.0 } else { (elapsed-clip.ticks).max(0.0) } });
    (s.strafe.clip, s.strafe.tick) = (id, tick);
}

/// The clip the weapon set's entry rules pick coming from `from_set` and `from_id`, if one
/// fits (`FUN_0084ddb0` with no clip named, `FUN_0070acc0`). [game]
fn strafe_entry(s: &mut State, input: &Input, t: &Tuning, from_set: u32, from_id: u32) {
    let get = |variable: u32| rule_value(s, input, variable);
    let picked = animrules::entry(&t.weapon_set.incoming, from_set, from_id, &get);
    if let Some((_, id)) = picked.filter(|&(set, id)| set == WEAPON_SET && id != 0 && id != s.strafe.clip) {
        strafe_start(s, t, id);
    }
}

/// StrafeMode (`FUN_00875ef0`), for a character whose data leaves `useCodeDrivenStrafing`
/// off, as Bumblebee's does: the mode moves nothing itself. With the stick pushed it turns
/// his body to the aim at once and writes `F_STRAFE_ANG`, the stick against the aim, which
/// the weapon set's rules turn into one of eight run clips; each travels 20 a second along
/// a root turned its own way. Standing, `F_STRAFE_ANG` is the aim against his body and his
/// spine twists by it, to 45.5 degrees at most; past that his body turns by the rest (the
/// rules start a turning clip at 45, which turns him first). [game]
///
/// Around it, each update: the leading clip's exit rules are walked BEFORE the mode's
/// update, so they see the angle the update before wrote (standing it is the twist, about
/// zero, so every start is one update of the forward clip: run 1 at 493.86 reads 2.13
/// ahead, then 4.39 at 29 degrees); then the clips cross-fade in a stack as on foot, each
/// fading in over its blend time on what was there, and their blended root motion is his
/// velocity (run 1: 20.00 steady, every start, stop and change of direction to 0.05).
/// [trace]
///
/// The update the mode is entered leaves `F_STRAFE_ANG` as its enter wrote it (the stick
/// against the facing he had) while his body already turns to the aim: run 1 at 489.79
/// runs on for two updates along the new facing before the sideways clip starts. Why is
/// not read (the stick the mode is given is kept relative to his body, and is taken to be
/// a frame old there). [trace] [assumed]
///
/// [stand-in: code-driven strafing (`+0x20`: speed toward `strafeSpeed`, heading turned at
/// 600 degrees a second) is not built; no character flag zeroes the stick (the special
/// attack's state does); the smoothing of the twist for characters no player controls]
fn step_strafe(s: &mut State, input: &Input, t: &Tuning, dt: f32) {
    let entering = std::mem::take(&mut s.strafe.entering);
    if !entering {
        weapon_rules(s, input, t);
    }

    // The mode's own update.
    let aim_yaw = input.aim.filter(|aim| aim.length_squared() > 1e-6).map_or(s.strafe.aim_yaw, yaw_of);
    s.strafe.aim_yaw = aim_yaw;
    if input.stick.length_squared() >= STRAFE_STICK_SQUARED {
        if !entering {
            s.strafe.angle = wrapped(yaw_of(input.stick) - aim_yaw).to_degrees();
        }
        s.yaw = aim_yaw;
        s.strafe.twist = 0.0;
    } else {
        let off = wrapped(aim_yaw - s.yaw);
        s.strafe.angle = off.to_degrees();
        if off.abs() > STRAFE_TWIST_MOST {
            s.yaw = wrapped(s.yaw + off - STRAFE_TWIST_MOST.copysign(off));
        }
        s.strafe.twist = off.clamp(-STRAFE_TWIST_MOST, STRAFE_TWIST_MOST);
    }

    let (moved, turned) = weapon_layers(s, t, dt);
    // A turning clip turns him (`Weapon_IdleTurnL` and `R`, 67.5 degrees). [data]
    // [assumed: its turn is blended by its fade like its travel; no recording has one]
    s.yaw = wrapped(s.yaw + turned);
    let facing = s.facing();
    let right = Vec2::new(facing.y, -facing.x);
    let v = right * moved.x + facing * moved.y;
    (s.vel.x, s.vel.y) = (v.x, v.y);
    s.speed = moved.length();
    s.gait = gait_of(input.stick.length().min(1.0), &t.locomotion);
}

/// The animation controller's turn for the weapon set (`FUN_0084e3c0`): the leading clip's
/// exit rules are walked, and the first that fits names the clip to start, or none, which
/// walks the set's entry rules from the clip being left. [game]
fn weapon_rules(s: &mut State, input: &Input, t: &Tuning) {
    let Some(clip) = t.weapon_set.clip(s.strafe.clip) else { return };
    let finished = !clip.looping && s.strafe.tick >= clip.ticks;
    let get = |variable: u32| rule_value(s, input, variable);
    if let Some(exit) = animrules::exit(&clip.exits, clip.branch(s.strafe.tick), finished, &get).filter(|exit| exit.to_set == WEAPON_SET) {
        match exit.to_id {
            0 => strafe_entry(s, input, t, WEAPON_SET, clip.id),
            id if id != clip.id => strafe_start(s, t, id),
            _ => {}
        }
    }
}

/// Plays the weapon set's clips on by a step, cross-fading down their stack, and gives
/// their blended root motion: his velocity in his own frame, and his turn. [data] [trace]
fn weapon_layers(s: &mut State, t: &Tuning, dt: f32) -> (Vec2, f32) {
    let (mut moved, mut turned) = (Vec2::ZERO, 0.0);
    for layer in &mut s.strafe.layers {
        layer.weight = if layer.blend > 0.0 { (layer.weight + dt / layer.blend).min(1.0) } else { 1.0 };
        let ticks = dt * crate::formats::anim::TICKS_PER_SECOND;
        let (own, turn) = match (layer.action, t.weapon_set.clip(layer.clip), t.actions.clips.get(&layer.clip)) {
            (false, Some(clip), _) => {
                let to = layer.tick + ticks * clip.speed;
                let (travel, turn) = clip.travel(layer.tick, to);
                if clip.looping { layer.elapsed_ticks += ticks * clip.speed; }
                else {
                    layer.elapsed_ticks = (layer.elapsed_ticks + ticks * clip.speed).min(clip.ticks);
                    layer.overrun += (to - clip.ticks).max(0.0);
                }
                layer.tick = if clip.looping { to % clip.ticks.max(1.0) } else { to.min(clip.ticks) };
                (Vec2::new(travel.x, travel.y) / dt.max(1e-6), turn)
            }
            (true, _, Some(clip)) => {
                let to = (layer.tick + ticks * clip.speed).min(clip.ticks);
                let travel = clip.root_at(to) - clip.root_at(layer.tick);
                layer.tick = to;
                (Vec2::new(travel.x, travel.y) / dt.max(1e-6), 0.0)
            }
            _ => (layer.carry, 0.0),
        };
        moved += (own - moved) * layer.weight;
        turned += (turn - turned) * layer.weight;
    }
    if let Some(top) = s.strafe.layers.last() {
        s.strafe.tick = top.tick;
    }
    // Whatever lies under a clip that has fully faded in no longer counts.
    if let Some(full) = s.strafe.layers.iter().rposition(|l| l.weight >= 1.0) {
        s.strafe.layers.drain(..full);
    }
    (moved, turned)
}

/// The set and clip playing on the base layer, as a set's entry rules ask where he comes
/// from: the weapon set's clip while it plays, a control state's own clip, else the set
/// the state he is in starts (the clip there is the chooser's and is not kept: no entry
/// rule of the weapon set names one of those sets' clips). [game: which set each state
/// class starts]
fn playing_clip(s: &State, t: &Tuning) -> (u32, u32) {
    if s.strafe.clip != 0 {
        return (WEAPON_SET, s.strafe.clip);
    }
    if let Some(clip) = s.action.as_ref().and_then(|a| t.actions.clips.get(&a.clip)) {
        return (crc32(clip.set.as_bytes()), crc32(clip.id.as_bytes()));
    }
    // What the base layer's rules have been playing (idle, walk, jump, fall and the landings).
    if let Some(playing) = s.base.playing() {
        return playing;
    }
    let class = s.runner.state.and_then(|state| t.control.states.get(state)).map(|d| d.class.as_str());
    let set: &[u8] = match class {
        Some("StateJump") => b"JumpSet",
        Some("StateFall") => b"FallSet",
        Some("StateMove") => b"WalkSet",
        Some("StateIdle") => b"IdleStandSet",
        _ => return (0, 0),
    };
    (crc32(set), 0)
}

/// StrafeMode's enter (`FUN_00876990`) and the weapon set's start that follows it in the
/// weapon state's enter: `I_IS_STRAFING` goes to 1; unless the mode before was the dash or
/// the jump the aim heading becomes his facing; with the stick pushed `F_STRAFE_ANG` is the
/// stick against that; the twist is cleared. Then the set's entry rules pick its first
/// clip, coming from what plays: out of the weapon states' jump and fall that is the weapon
/// set's own air clip (`Weapon_Fall` leads to the landings), which goes on fading out
/// underneath. [game] [assumed: from the ground, what cross-fades out underneath is the
/// clip of the control state being left playing on (the dodge), else the velocity he had,
/// in his own frame, held (the run clip plays on in the original, at a steady 24)]
fn enter_strafe(s: &mut State, input: &Input, t: &Tuning, old: Mode) {
    let from_air = matches!(old, Mode::Jump(_));
    // The state being left is still the runner's while the next one is entered.
    let dash = s.runner.state.and_then(|state| t.control.states.get(state)).is_some_and(|d| d.class == "StateDash");
    let facing = s.facing();
    let had = Vec2::new(s.vel.x, s.vel.y);
    let carry = if from_air { Vec2::ZERO } else { Vec2::new(had.dot(Vec2::new(facing.y, -facing.x)), had.dot(facing)) };
    let aim_yaw = if from_air || dash { input.aim.map_or(s.strafe.aim_yaw, yaw_of) } else { s.yaw };
    let (from_set, from_id) = playing_clip(s, t);
    let old = std::mem::take(&mut s.strafe);
    s.strafe = Strafe { strafing: true, aim_yaw, entering: true, ..Strafe::default() };
    if input.stick.length_squared() >= STRAFE_STICK_SQUARED {
        s.strafe.angle = wrapped(yaw_of(input.stick) - aim_yaw).to_degrees();
    }
    if old.clip != 0 {
        (s.strafe.clip, s.strafe.tick, s.strafe.layers) = (old.clip, old.tick, old.layers);
    } else {
        // A control state's clip (the dodge) plays on underneath from where it had got to.
        let (clip, tick) = s.action.as_ref().map_or((0, 0.0), |a| (a.clip, a.tick));
        s.strafe.layers.push(RootLayer { clip, action: clip != 0, weight: 1.0, blend: 0.0, tick, carry, elapsed_ticks:tick, overrun:0.0 });
    }
    strafe_entry(s, input, t, from_set, from_id);
}

/// The velocity the playing state clip's root motion gives over this step, in the world, if
/// a state clip is playing. [data]
fn clip_motion(s: &State, t: &Tuning, dt: f32) -> Option<Vec2> {
    let action = s.action.as_ref()?;
    let data = t.actions.clips.get(&action.clip)?;
    let moved = if dt > 1e-6 && action.tick >= action.last_tick {
        (data.root_at(action.tick) - data.root_at(action.last_tick)) / dt
    } else {
        Vec3::ZERO
    };
    let facing = s.facing();
    let right = Vec2::new(facing.y, -facing.x);
    Some(right * moved.x + facing * moved.y)
}

/// The change back out of the car (the drive companion mode, `FUN_00863070`). It takes the
/// body's speed along the ground; while that is over the exit's least speed it comes down by
/// the exit's deceleration (and stops at zero, not at the least speed, which is why run 1
/// settles at 23.7 and not 24); the stick turns him at the exit's turn speed; and the body
/// is sent along his facing at that speed. Run or stop is chosen once, by the stick, when the
/// mode starts. The clip's root motion does not move him. [game] [trace]
fn step_unfold(s: &mut State, input: &Input, t: &Tuning, dt: f32) {
    let d = &t.drive;
    let (least, slowing, turn) = if s.unfold_run {
        (d.exit_to_run_min_speed, d.exit_to_run_deceleration, d.exit_to_run_turn_speed)
    } else {
        (d.exit_to_idle_min_speed, d.exit_to_idle_deceleration, d.exit_to_idle_turn_speed)
    };
    let mut speed = Vec2::new(s.vel.x, s.vel.y).length();
    if speed > least {
        speed = (speed - slowing * dt).max(0.0);
    }
    if input.stick.length_squared() > STICK_DEAD_SQUARED {
        s.yaw = turn_toward(s.yaw, yaw_of(input.stick), turn.to_radians() * dt);
    }
    let v = s.facing() * speed;
    (s.vel.x, s.vel.y) = (v.x, v.y);
    s.speed = speed;
    s.gait = Gait::Idle;
}

fn step_jump(s: &mut State, input: &Input, t: &Tuning, kind: JumpType, dt: f32) {
    // The ground punch's fall: each update the body's pull grows by the punch's acceleration,
    // up to its limit, with no time step in it (so it was tied to the frame rate). [game]
    let punching = s.action.as_ref().is_some_and(|a| t.control.states[a.state].name.contains("GroundPunch"));
    if punching {
        let most = (t.ground_punch_speed_limit - t.gravity).max(0.0);
        s.punch_gravity = (s.punch_gravity + t.ground_punch_acceleration * dt / GAME_UPDATE).min(most);
    }
    let weapon = s.weapon_air();
    // The slow motion of a jump with the weapon set playing (`FUN_00872fe0`). The mode keeps
    // a time factor (`+0x28`). The body's gravity is the factor times its own, and when that
    // changes the vertical speed is rescaled so that what is left of the rise or fall keeps
    // its height; the mode's own step is dt times the factor's root. Both use the factor as
    // the update before left it. So up and down he moves as if time ran at the factor's
    // root: half speed at Bumblebee's 0.25. [game]
    if s.dilation != s.gravity_scale {
        s.vel.z *= (s.dilation / s.gravity_scale).sqrt();
        s.gravity_scale = s.dilation;
    }
    let slowed = s.dilation.sqrt();
    // The factor goes down while he is near the top: with no height noted yet, while he
    // rises or falls slower than the start speed, and the height he is at is then noted;
    // with one noted, while he is above it, whatever his speed. Else it goes back up to 1.
    // [game] [game: the other way in is `FUN_00721b50`, a state of class dc24f161 with value
    // 4 on the character's `+0x314`; not Bumblebee's weapon states, not built]
    let dilating = weapon
        && match s.dilation_from {
            None => s.vel.z.abs() < t.jump.weapon_dilation_start,
            Some(from) => s.pos.z > from,
        };
    if dilating {
        s.dilation_from.get_or_insert(s.pos.z);
        s.dilation = (s.dilation - t.jump.weapon_dilation_rate * dt).max(t.jump.weapon_dilation);
    } else {
        s.dilation = (s.dilation + t.jump.weapon_dilation_rate * dt).min(1.0);
    }
    if weapon {
        // The animation controller's turn comes before the mode's, as on foot.
        weapon_rules(s, input, t);
    }

    let jump = jump_kind(t, kind);
    let stick = input.stick.clamp_length_max(1.0);
    // What the stick asks for is kept up to date in the air, so the landing knows it.
    s.gait = gait_of(stick.length(), &t.locomotion);
    let horizontal = Vec2::new(s.vel.x, s.vel.y);
    // What the mode asks for each frame: the stick times the jump type's speed, turning toward
    // the stick at the type's turn speed. The long jump with the stick centred asks for its
    // speed along the way he is already going. [game]
    let asked = if stick.length_squared() > STICK_DEAD_SQUARED {
        if !weapon {
            s.yaw = turn_toward(s.yaw, yaw_of(stick), jump.turn_speed.to_radians() * dt * slowed);
        }
        stick * jump.speed
    } else if kind == JumpType::Long {
        horizontal.try_normalize().map_or(Vec2::ZERO, |direction| direction * jump.speed)
    } else {
        // Stick centred: the mode moves nothing, so the speed he took off with bleeds away. [trace]
        Vec2::ZERO
    };
    if weapon {
        // With the weapon set playing the mode does not turn him to the stick: it rebuilds
        // his matrix from the aim heading each update, stick or none, so he jumps sideways
        // and backwards facing his aim. [game]
        s.strafe.aim_yaw = input.aim.filter(|aim| aim.length_squared() > 1e-6).map_or(s.strafe.aim_yaw, yaw_of);
        s.yaw = s.strafe.aim_yaw;
    }
    // The clip's own root motion counts as well: the take-off clip pushes him forward, then
    // the in-air clip drifts a little. Seen in the recording as the extra gain in the first
    // five updates of a running jump and the creep forward in a standing one. [trace]
    // With the weapon set it is that set's clips' (next to nothing: the rise drifts half a
    // unit forward). [data] [assumed: the clips play on at their own rate in the slow
    // motion; whether the animation player is slowed too is not read]
    let root = if weapon {
        let (moved, _) = weapon_layers(s, t, dt);
        let facing = s.facing();
        Vec2::new(facing.y, -facing.x) * moved.x + facing * moved.y
    } else {
        let along = match kind {
            JumpType::Standing | JumpType::Moving => {
                let takeoff = &t.locomotion.takeoff;
                if s.air_time < takeoff.duration() {
                    takeoff.speed(s.air_time, s.air_time + dt)
                } else {
                    t.locomotion.in_air_speed
                }
            }
            _ => 0.0,
        };
        s.facing() * along
    };
    s.air_time += dt;
    s.jump_time += dt;
    // What the mode asks for is a step of speed times its own, slowed, dt. [game]
    let wanted = asked * slowed + root;
    // Each of the game's updates the body's velocity moves the jump type's control share of the
    // way to what is asked. Read from the code and confirmed update by update in the recording
    // (a running jump climbs from 24 to 33 m/s in the air exactly this way). [game] [trace]
    let share = 1.0 - (1.0 - jump.control).powf(dt * CONTROL_FRAME_RATE);
    let blended = horizontal.lerp(wanted, share);
    (s.vel.x, s.vel.y) = (blended.x, blended.y);
}

/// Changes the motion mode, with what leaving the old one and entering the new one do. A mode
/// that is already running is not entered again (`FUN_00852150`). [game]
fn set_mode(s: &mut State, input: &Input, t: &Tuning, mode: Mode) {
    if mode == s.mode {
        return;
    }
    let old = s.mode;
    let landed = matches!(old, Mode::Jump(_));
    if landed && mode == Mode::Move {
        // The jump mode's exit into MoveMode caps the speed along the ground at the
        // walk-run boundary, keeping its direction (run 1 shows the 5.0). [game] [trace]
        let flat = Vec2::new(s.vel.x, s.vel.y).clamp_length_max(t.moving.walk_run_speed);
        (s.vel.x, s.vel.y) = (flat.x, flat.y);
    }
    if landed && mode == Mode::Strafe {
        // Into StrafeMode the jump mode's exit does the same with the strafe speed. [game]
        let flat = Vec2::new(s.vel.x, s.vel.y).clamp_length_max(t.strafe_speed);
        (s.vel.x, s.vel.y) = (flat.x, flat.y);
    }
    if old == Mode::Drive {
        // [game: 0085ed50] Drift turbo is removed; standard turbo is retained.
        s.car.drift_turbo = crate::driving::DriftTurbo::default();
        s.turbo_fx_on = false;
    }
    if old == Mode::Strafe {
        // StrafeMode's exit (`FUN_00876ea0`): `I_IS_STRAFING` and `F_STRAFE_ANG` go to 0
        // and the spine's twist is taken off. The weapon set's clips are not the mode's:
        // they play on until a state starts another set (`enter_state`). [game]
        let old = std::mem::take(&mut s.strafe);
        s.strafe = Strafe { aim_yaw: old.aim_yaw, clip: old.clip, tick: old.tick, layers: old.layers, ..Strafe::default() };
    }
    if landed {
        // The jump mode's exit puts the body's gravity back and its time factor to 1
        // (`FUN_00874850`). [game]
        s.dilation = 1.0;
        s.gravity_scale = 1.0;
    }
    s.mode = mode;
    match mode {
        Mode::Strafe => {
            s.layers.clear();
            enter_strafe(s, input, t, old);
        }
        Mode::Stand | Mode::Move => {
            s.speed = Vec2::new(s.vel.x, s.vel.y).dot(s.facing()).max(0.0);
            if landed || old == Mode::Climb {
                s.layers.clear();
                if mode == Mode::Move {
                    // Landing into the walk set plays the landing that runs on; after a fall
                    // of a second or more, the big one (`animRuleLongFall`). [data] [trace]
                    // The walk set's entry rules have picked the landing by now (the state
                    // starts the set before the mode): the big one for a fall of a second
                    // or more, or of three quarters of one after a long jump. [game]
                    let long_fall = match s.base.current(&t.rule_sets) {
                        Some(clip) if s.base.set == WALK_SET => clip.name == "BigJump_LandRun",
                        _ => t.locomotion.long_fall > 0.0 && s.fall_time >= t.locomotion.long_fall,
                    };
                    let pace = if long_fall { Pace::BigLandRun } else { Pace::LandRun };
                    s.layers.push(Layer { pace, weight: 0.0, time: 0.0 });
                }
            } else if old == Mode::Unfold {
                // Out of the change back to the robot the walk or run carries on from the
                // speed he has, not from a standstill. [trace]
                s.layers.clear();
                let walk = &t.locomotion;
                if mode == Mode::Move {
                    let (pace, own) = if gait_of(input.stick.length(), walk) == Gait::Run {
                        (Pace::Run, walk.run_speed)
                    } else {
                        (Pace::Walk, walk.walk_speed)
                    };
                    s.layers.push(Layer { pace, weight: (s.speed / own.max(0.1)).clamp(0.0, 1.0), time: 0.0 });
                }
            } else if old != Mode::Stand && old != Mode::Move {
                s.layers.clear();
            }
        }
        Mode::Drive => {
            s.layers.clear();
            // The wheels start out rolling with the ground, so nothing grabs or pushes at first.
            let forward = Vec2::new(s.vel.x, s.vel.y).dot(s.facing());
            let d = &t.drive;
            s.vehicle_gearbox=Some(crate::sound::Gearbox::new(t.drive_gears.clone(),d.engine_torque,
                d.front_wheel_radius,VEHICLE_MASS,d.max_speed,d.turning_velocity_delta));
            let roll = |radius: f32| forward / radius.max(0.1);
            s.car = Car {
                spin: [roll(d.front_wheel_radius), roll(d.front_wheel_radius), roll(d.rear_wheel_radius), roll(d.rear_wheel_radius)],
                // The update the car mode starts does not push: run 1 reads 0.000 on it and
                // the car moves from the next (t 433.81). [trace]
                clock: -GAME_UPDATE,
                // [game: 0085e130] Enter starts the trigger filter at full, not zero.
                filtered_trigger: 1.0,
                ..Car::default()
            };
        }
        Mode::Unfold => {
            s.layers.clear();
            s.unfold_run = input.stick.length() > UNFOLD_RUN_STICK;
            s.speed = Vec2::new(s.vel.x, s.vel.y).length();
        }
        Mode::Jump(_) | Mode::Action | Mode::Climb => s.layers.clear(),
    }
}

/// JumpMode's enter (`FUN_008740e0`): which jump, from what was playing, the mode before and
/// the stick, and the launch when there is one. `set` is the set playing as the mode starts:
/// the fall state starts its set first, the jump state after. [game]
fn enter_jump_mode(s: &mut State, input: &Input, t: &Tuning, set: Option<&str>, on_ground: bool) {
    if matches!(s.mode, Mode::Jump(_)) {
        return;
    }
    let old = s.mode;
    let horizontal = Vec2::new(s.vel.x, s.vel.y);
    let stick_squared = input.stick.length_squared();
    let kind = match set {
        // A jump that starts while the climb set plays: vertical speed cleared, a push along
        // his facing on top of what he was moving at, the climb jump's height. [game]
        Some("ClimbSet") => {
            let push = s.facing() * CLIMB_JUMP_PUSH;
            s.vel = Vec3::new(s.vel.x + push.x, s.vel.y + push.y, 0.0);
            Some(JumpType::Climb)
        }
        Some("GroundPunchSet") => None,
        // Out of the car on the ground: the high standing jump from a crawl, the long jump
        // otherwise, keeping the car's speed.
        _ if matches!(old, Mode::Drive | Mode::Unfold) && on_ground => Some(if horizontal.length_squared() <= VEHICLE_EXIT_SLOW_SQUARED {
            JumpType::HighStanding
        } else {
            JumpType::Long
        }),
        // Falling, or off the ground: no launch.
        Some("FallSet") => None,
        _ if !on_ground => None,
        _ if stick_squared <= STICK_DEAD_SQUARED => Some(JumpType::Standing),
        _ => {
            // A running jump at full stick takes off at exactly MoveMode's top speed, whatever
            // point of the stride the run clip was at.
            if old == Mode::Move && stick_squared > FULL_STICK_SQUARED && horizontal.length_squared() > 1e-4 {
                let v = s.facing() * t.moving.max_speed;
                (s.vel.x, s.vel.y) = (v.x, v.y);
            }
            // Out of StrafeMode likewise at the strafe speed, the way he was going. [game]
            if old == Mode::Strafe && stick_squared > FULL_STICK_SQUARED && horizontal.length_squared() > 1e-4 && t.strafe_speed > 0.1 {
                let v = horizontal.normalize() * t.strafe_speed;
                (s.vel.x, s.vel.y) = (v.x, v.y);
            }
            Some(JumpType::Moving)
        }
    };
    set_mode(s, input, t, Mode::Jump(kind.unwrap_or(JumpType::Fall)));
    // The animation variable the mode writes, which outlives it. [game: the ground punch's
    // fall is type 2494da83]
    s.jump_type = if set == Some("GroundPunchSet") { 0x2494_da83 } else { jump_type_hash(kind.unwrap_or(JumpType::Fall)) };
    // The mode's time factor and the height its slow motion began at start afresh. [game]
    s.dilation = 1.0;
    s.gravity_scale = 1.0;
    s.dilation_from = None;
    s.jump_time = 0.0;
    s.air_time = 0.0;
    s.launch_z = s.pos.z;
    s.apex_z = s.pos.z;
    if let Some(kind) = kind {
        // Straight up against gravity at the speed that peaks at the jump's height.
        s.vel.z = (2.0 * t.gravity * jump_height(t, kind)).sqrt();
    }
}

/// The motion mode classes the control states name. [game]
/// What a melee attack sets the character's fight timer to (`State::fight`). [game]
const FIGHT_TIME: f32 = 4.0;
const ATTACK_MODE: u32 = 0xcd2f_4b0c;
const JUMP_MODE: u32 = 0x6da1_d12d;
/// The unnamed first mode: the clips move him.
const FIRST_MODE: u32 = 0x1ad6_ae68;
const CLIMB_MODE: u32 = 0xd706_8c31;
const UNFOLD_MODE: u32 = 0xf081_d468;
/// A jump that starts while the climb set plays is pushed this hard along his facing. [game]
const CLIMB_JUMP_PUSH: f32 = 20.0;
/// Blend time for clips whose own is negative ("none given"): the clip starter
/// `FUN_00748f00` takes 0.1 for any blend time under 0. [game]
const DEFAULT_CROSSFADE: f32 = 0.1;

/// The control state machine decides every change of what he is doing: standing, moving,
/// jumping, falling, the change of form both ways, weapon mode, the attacks, the dodge, the
/// climb. Each update it runs the current state's own update, then takes the transition the
/// graph allows, and the state it enters sets the motion mode. [game: the graph, the
/// detectors, the choice, and the enter and update of the states named in `enter_state`]
/// [stand-in: the states not built (carrying, hover) are never
/// entered]
fn control(s: &mut State, input: &Input, t: &Tuning, arena: &dyn World, dt: f32) {
    let graph = &t.control;
    let messages = std::mem::take(&mut s.messages);
    if s.runner.state.is_none() {
        // He starts standing. [assumed]
        let Some(idle) = graph.find("stateIdle") else { return };
        s.runner.enter(graph, idle);
        base_enter(s, input, t, IDLE_SET, (0, 0));
    }
    if let Some(full) = s.hit_pending.take() {
        let permission = if full { 1 << 1 } else { 1 << 2 };
        let allowed = s.runner.state.is_some_and(|index| graph.states[index].can_go_to & permission != 0);
        if allowed {
            if full {
                if let Some(next) = graph.find("stateHitReact") {
                    let on_ground = s.ground_flag();
                    enter_state(s, input, t, next, None, on_ground);
                    s.previous_state = s.runner.state;
                    s.runner.enter(graph, next);
                }
            } else {
                // The character's animation message handler starts layer 2 immediately;
                // the control overlay is requested for the next update. [game]
                let mut layer = std::mem::take(&mut s.hit_partial);
                let from = layer.playing().unwrap_or((animrules::EMPTY_SET, 0));
                layer.enter_partial(&t.rule_sets, crc32(b"HitReactPartialSet"), from, playing_clip(s, t), &|v| rule_value(s, input, v));
                s.hit_partial = layer;
                s.runner.pending = graph.find("stateHitReactOverlay");
            }
        }
    }
    if std::mem::take(&mut s.stun_pending) && s.damage_timers.stunned > 0.0 &&
        s.runner.state.is_some_and(|index| graph.states[index].can_go_to & (1 << 3) != 0) &&
        let Some(next) = graph.find("stateStun") {
        let on_ground = s.ground_flag();
        enter_state(s, input, t, next, None, on_ground);
        s.previous_state = s.runner.state;
        s.runner.enter(graph, next);
    }
    let Some(current) = s.runner.state else { return };
    let climbing = s.climb.is_some();

    // The pad as the detectors see it. The movement's own inputs are folded in under the
    // game's button numbers. [stand-in: the stick is not the camera-relative one, which only
    // matters to detectors with an angle range measured from the camera; Bumblebee has none]
    let facing = s.facing();
    let right = Vec2::new(facing.y, -facing.x);
    let mut pad = Pad { down: input.buttons_down, hit: input.buttons_hit, left: input.stick, left_character: Vec2::ZERO };
    pad.left_character = Vec2::new(input.stick.dot(right), input.stick.dot(facing));
    pad.set(button::JUMP, input.jump_held || input.jump, input.jump);
    // The jump out of the car is on the jump button in the game's bindings. [data]
    pad.set(button::TRANSFORM_JUMP, input.jump_held || input.jump, input.jump);
    // The two attacks out of the car: the slam is on the melee button, the punch on the
    // action (climb) button. [data]
    let held = |b: u32| (input.buttons_down >> b & 1 != 0, input.buttons_hit >> b & 1 != 0);
    let (slam, punch) = (held(button::ATTACK_FAST), held(button::CLIMB));
    pad.set(button::TRANSFORM_SLAM, slam.0, slam.1);
    pad.set(button::TRANSFORM_PUNCH, punch.0, punch.1);
    pad.set(button::WEAPON_MODE, input.aim.is_some(), false);
    pad.set(button::VEHICLE_MODE, input.vehicle > TRIGGER_DEAD, false);

    if s.action.is_some() {
        play_action(s, t, &pad, dt);
    }
    s.runner.update(graph, &pad, dt);
    let on_ground = s.ground_flag();
    // The drive states' own update: until the car mode runs, the first half of the change
    // into the car keeps MoveMode while the stick does not line up with his motion. [game]
    if graph.states[current].class == "StateDrive" && s.mode != Mode::Drive {
        drive_or_move(s, input, t);
    }
    // The jump state's: after the jump up off a wall, the catch onto the ledge above. [game]
    if graph.states[current].class == "StateJump" && s.previous_state.is_some_and(|p| graph.states[p].name == "stateClimbJumpUp") {
        ledge_catch(s, t, arena);
    }
    // The idle state's (`FUN_0087d790`) sets the first mode's control to (1, 1, 1) while he
    // is on the ground (height over it, owner `+0x334`, under 0.01) and nothing is pushing
    // him (`+0x34c`), else (1, 1, 0): standing, the body's height follows the animated
    // one, so gravity builds no speed. The body here keeps `vel.z` at zero on the ground in
    // every mode, which comes to the same; nothing pushes him yet. [game]

    let floor = arena.floor(s.pos.x, s.pos.y, s.pos.z);
    let branch = if s.stun_full {
        if s.damage_timers.stunned <= 0.0 { 100 } else { 1 }
    } else if s.hit_full {
        let done = if s.hit_flyback { s.base.set != crc32(b"HitReactionSet") }
            else { s.base.current(&t.rule_sets).is_some_and(|c| !c.looping && s.base.tick >= c.ticks) || s.base.set != crc32(b"HitReactionSet") };
        if done { 100 } else { 1 }
    } else { match (&s.climb, &s.action) {
        (Some(climb), _) => if climb.finished { 100 } else { 1 },
        (None, action) => action.as_ref().map_or(1, |a| if a.finished { 100 } else { a.branch }),
    }};
    let can_branch = s.action.as_ref().is_some_and(|a| a.can_branch);
    // The climbable test, from where he is along his facing. [game]
    let wall = if climbing || s.mode == Mode::Drive {
        None
    } else {
        climb::find(arena, s.pos, s.facing(), t.robot_radius, t.robot_height, t.jump.standing.height)
    };
    let environment = |test: u32| match test {
        env::CLIMBABLE => wall.is_some(),
        // Nothing to carry or fly out of yet. [stand-in]
        env::CAN_SWITCH_AVATAR | env::NOT_HOLD_VIP | env::INSIDE_FLIGHT_BOUNDARY => true,
        _ => false,
    };
    let facts = Facts {
        on_ground,
        z_velocity: s.vel.z,
        holding: false,
        branch,
        attack_can_branch: can_branch,
        // Character flag 0x20, which the drive mode's update (`FUN_0085cad0`) sets once it
        // has run `advTransformDelay` and its enter and exit clear. [game]
        can_adv_transform: s.mode == Mode::Drive && s.car.time >= t.transform_delay,
        ground_distance: s.pos.z - floor,
        env: &environment,
        messages: &messages,
    };
    // Whether a state agrees to be entered (slot 11 of its table). [game for the dash and
    // the drive; the others have none, or are not built]
    let (dash_ready, special_ready) = (s.dash_cooldown <= 0.0, s.special_cooldown <= 0.0);
    let vehicle_enabled = s.damage_timers.disabled_vehicle <= 0.0 && s.damage_timers.disabled_transform <= 0.0;
    let can_transform = s.damage_timers.disabled_transform <= 0.0 || !s.car_mode;
    // [game] StateAttack animName0 starts CombatSet through its entry rules (008788b0).
    let generic_attack = t.rule_sets.get(&crc32(b"CombatSet")).and_then(|set| {
        let (from_set,from_id)=playing_clip(s,t);
        animrules::entry(&set.incoming,from_set,from_id,&|v|rule_value(s,input,v))
    }).map(|(_,id)|id).filter(|id|t.actions.clips.contains_key(id));
    let may_enter = |state: usize| {
        let def = &graph.states[state];
        if def.overlay {
            return match def.class.as_str() {
                // The two firing states, on foot and in the car: an attack state's own
                // test (slot 11, `FUN_0076c530`) is always yes. [game]
                "StateAttack" => def.ranged,
                // The special (`FUN_008799c0`): not while its cooldown runs (his damage
                // object's `+0x84`). [game] Left out: its refusal in a multiplayer mode
                // that switches specials off.
                "StateAttackSpecial" => special_ready,
                "StateHitReact" => true,
                _ => false,
            };
        }
        match def.class.as_str() {
            "StateClimb" => wall.is_some(),
            // The pull-up: the climb mode again, so it only follows the climb.
            "StateBasic" if def.motion_mode == CLIMB_MODE => climbing,
            "StateAttack" => if def.anim==0 {generic_attack.is_some()} else {t.actions.clips.contains_key(&def.anim)},
            "StateBasic" => def.anim != 0 && t.actions.clips.contains_key(&def.anim),
            "StateDash" => dash_ready && t.actions.named("DodgeFront").is_some(),
            // Its own test (`FUN_0087c550`) refuses while his damage object's
            // "vehicle disabled" timer (`+0x234` -> `+0x48`) runs: a hit flagged
            // `DISABLE_VEHICLE` starts it. [game]
            "StateDrive" => vehicle_enabled,
            // `StateWeapon` (`FUN_0087fcf0`) refuses only while sniping, and only where the
            // data sets `canEnterOnlyIfNotSniping` (the weapon jump). No sniping yet. [game]
            "StateIdle" | "StateMove" | "StateFall" | "StateJump" => true,
            "StateWeapon" => can_transform,
            "StateStun" => true,
            "StateHitReact" => true,
            // Carrying, hover and the rest are not built. [stand-in]
            _ => false,
        }
    };
    // Whether the current state lets him go (slot 8): the jump state only once the jump mode
    // has run a tenth of a second, and the fall state the same when it followed weapon mode's
    // jump. [game]
    let jump_young = matches!(s.mode, Mode::Jump(_)) && s.jump_time <= JUMP_LEAVE_TIME;
    let may_leave = match graph.states[current].class.as_str() {
        "StateStun" => s.damage_timers.stunned <= 0.0,
        "StateJump" => !jump_young,
        "StateFall" => !(jump_young && s.previous_state.is_some_and(|p| graph.states[p].name == "stateWeaponModeJump")),
        "StateHitReact" if s.hit_flyback => s.base.set != crc32(b"HitReactionSet") || s.pos.z < s.hit_start_z - 2.5,
        _ => true,
    };
    let basic_hit_waiting = s.hit_full && !s.hit_flyback && branch != 100;
    let allowed = |state: usize| {
        // A basic reaction refuses a fall until its clip finishes; other outgoing states
        // are allowed. Flybacks can also leave after a 2.5m drop. [game: FUN_0087d2b0]
        let basic_hit_can_fall = !basic_hit_waiting || graph.states[state].class != "StateFall";
        may_leave && basic_hit_can_fall && may_enter(state)
    };

    // The overlay state (`FUN_00878370`, at the start of a state's own transition step):
    // the one asked for last update takes its place and has its update. The ranged attack
    // state's (`FUN_00878740`) gives its fire button to the weapon manager: down, "trigger
    // held"; let go this update, "released" (which `weapons::Weapon` sees as `firing` going
    // false). Then its own transitions: `connectionReleaseWeaponModeAttack` ends it. [game]
    //
    // The special is an overlay state too (`stateAttackSpecial`), on top of an ordinary
    // state or of a firing state: its enter starts the ability (`FUN_00879110` ->
    // `FUN_0072afd0`), and being an overlay state with no clip it is done at once (its
    // slot 9, `FUN_00879950`), so it is left at the start of the next update. [game]
    let entered = s.runner.swap_overlay(graph);
    for state in [entered.overlay, entered.nested].into_iter().flatten() {
        if graph.states[state].class == "StateAttackSpecial" {
            start_special(s, t);
        }
    }
    s.firing = s.runner.overlay.is_some_and(|overlay| {
        let def = &graph.states[overlay];
        (def.ranged && pad.is_down(def.fire_button)) || (def.class == "StateHitReact" &&
            (s.car_mode || s.mode == Mode::Strafe || s.weapon_air()) &&
            pad.is_down(if s.car_mode { button::ATTACK_VEHICLE } else { button::WEAPON_FIRE }))
    });
    let done = |state: usize| graph.states[state].class == "StateAttackSpecial" ||
        (graph.states[state].class == "StateHitReact" && s.hit_partial.current(&t.rule_sets).is_none_or(|c| !c.looping && s.hit_partial.tick >= c.ticks));
    s.runner.update_overlay(graph, &pad, &facts, &may_enter, &done, dt);

    let Some(transition) = s.runner.pick(graph, &facts, &allowed) else {
        // Nothing to go to: a transition into an overlay state is looked for, if none is
        // running; it is entered at the start of the next update. A state's "may leave" is
        // not asked for these (`FUN_00878230`). [game]
        if let Some(over) = s.runner.pick_overlay(graph, &facts, &may_enter) {
            s.runner.ask_overlay(over);
        } else if (s.hit_full || s.stun_full) && branch == 100 {
            // A completed ordinary state requests the owner's default state (+0x318).
            // Character setup binds that default to stateIdle, even when another state
            // starts first. [game: FUN_00877d90, FUN_00850a70, 00716d8b..00716d9b]
            if let Some(next) = graph.find("stateIdle") {
                enter_state(s, input, t, next, wall, on_ground);
                s.previous_state = Some(current);
                s.runner.enter(graph, next);
            }
        }
        return;
    };
    let Some(next) = transition.output else { return };
    if graph.states[current].class == "StateAttack" {
        // The attack state's exit forgets whom it hit (`FUN_00878be0`). [game]
        s.hit_list.clear();
        s.surface_hit_list.clear();
    }
    enter_state(s, input, t, next, wall, on_ground);
    s.previous_state = Some(current);
    s.runner.enter(graph, next);
}

/// The special ability starts (`FUN_0072afd0`, called by the special state's enter): the
/// cooldown is set to `specialAbilityCooldown` (`FUN_0072bc40`), the script the character's
/// attributes name for the form he is in is sent to himself as a trigger message, and the
/// ability counts as on for `specialAbilityDuration`. [game] What the script sets off is
/// whatever on his own object goes by that name. [data] Bumblebee's (`specialAttack`, and
/// `specialAttackVehicle` in the car): an `ObjectSpawnerOnBone` that puts a stun shockwave
/// on him (an `Explosion` of no damage, flag `DAMAGE_STUN`, `stunTime` 2.5, a sphere of 15
/// growing from half size over 0.25 s), a `TriggerableParticle`
/// (`r_attack_emp_stun_bumblebee`), an `EffectSpawner` (`r_empstun_bumblebee`, a ring mesh)
/// and a `SoundPlayer`; the last three are for whoever draws and plays, through
/// `State::triggers`. Not in: the types 0 to 5 of other characters (his is -1), the weapon
/// a special may fire (`specialAbilityWeaponFireName`, his 0), the single player upgrade
/// that shortens the cooldown (`FUN_0079d060`; levels all 0), the HUD's messages.
fn start_special(s: &mut State, t: &Tuning) {
    s.special_cooldown = t.special.cooldown;
    if t.special.duration > 0.0 {
        s.special_left = t.special.duration;
    }
    let (script, spawners) =
        if s.car_mode { (t.special.vehicle_script, &t.actions.vehicle_spawners) } else { (t.special.script, &t.actions.spawners) };
    if script == 0 {
        return;
    }
    s.triggers.push(script);
    let placed = Affine3A::from_rotation_translation(Quat::from_rotation_z(s.yaw), s.pos);
    for (index, blast) in spawners.iter().enumerate().filter(|(_, b)| b.name == script) {
        let centre = placed.transform_point3(blast.centre(0.0));
        if blast.explosion.camera_shake != 0 {
            s.shakes.push(ShakeStart { name: blast.explosion.camera_shake, at: centre });
        }
        if let Some(rumble) = &blast.rumble
            && let Some(strength) = rumble.strength_at(centre.distance(s.pos))
        {
            s.rumbles.push(Rumble { kind: rumble.kind, duration: rumble.duration, strength });
        }
        s.blasts.push(Blast {
            clip: 0,
            index,
            script: Some(s.car_mode),
            delay: blast.explosion.delay,
            age: 0.0,
            centre,
            radius: blast.explosion.radius * blast.explosion.start_scale,
            live: false,
            hit: Vec::new(),
            surfaces: Vec::new(),
        });
    }
}

/// What entering each kind of state does to the motion. [game: the enter of `StateIdle`
/// (`FUN_0087d830`), `StateMove` (`FUN_0087e4d0`), `StateJump` (`FUN_0087e210`), `StateFall`
/// (`FUN_0087c070`), `StateDrive` (`FUN_0087ba10`), `StateBasic` (`FUN_00879ef0`), and of the
/// attack, dash and climb states, and `StateWeapon` (`FUN_0087f4a0`): its `motionModeType`
/// first, then `WeaponSet`; coming from anything but a dash or another weapon state the gun
/// is drawn (`WEAPON_ACTION_PULL_OUT_GUN` on `WeaponPartialSet`, or at once after the change
/// from the car), and its exit (`FUN_0087f6f0`) puts it away again unless a dash or weapon
/// state follows]
fn enter_state(s: &mut State, input: &Input, t: &Tuning, next: usize, wall: Option<Wall>, on_ground: bool) {
    let def = &t.control.states[next];
    if s.hit_full && def.class != "StateHitReact" && s.damage_timers.stunned > 0.0 { s.stun_pending = true; }
    if def.class != "StateStun" { s.stun_full = false; }
    let playing = playing_set(s, t);
    // The set and clip the new one comes from, for the entry rules of the set it starts.
    let from = playing_clip(s, t);
    // Exiting an ordinary state exits its overlays too. [game: FUN_00878310]
    s.hit_partial = Base::default();
    if def.class != "StateHitReact" {
        s.hit_full = false;
        s.hit_last = None;
    }
    // The control mode: a drive state's enter makes it the car's (`FUN_0087ba10`), a
    // weapon state's the weapon's (`FUN_0087f4a0`), and an attack or plain state's is its
    // `robotForm` unless that is 5. [game]
    match def.class.as_str() {
        "StateDrive" => s.car_mode = true,
        "StateWeapon" => s.car_mode = false,
        "StateAttack" | "StateBasic" if def.robot_form != 5 => s.car_mode = def.robot_form == 3,
        _ => {}
    }
    match def.class.as_str() {
        "StateHitReact" => {
            stop_clip(s, t);
            base_enter(s, input, t, crc32(b"HitReactionSet"), from);
            s.mode = Mode::Action;
            s.hit_full = true;
            s.hit_flyback = matches!(s.damage_type, 0x6a58_53c5 | 0xfea0_bd86);
            s.hit_start_z = s.pos.z;
            s.hit_last = None;
            s.hit_cooldown = 4.0;
            s.wall_splatted = false;
            s.car_mode = false;
            if s.hit_direction.truncate().length_squared() > 1e-8 { s.yaw = yaw_of(-s.hit_direction.truncate()); }
        }
        "StateStun" => {
            stop_clip(s, t);
            base_enter(s, input, t, crc32(b"StunSet"), from);
            s.mode = Mode::Action;
            s.stun_full = true;
            s.car_mode = false;
            s.hit_flyback = false;
            s.hit_last = None;
            s.firing = false;
        }
        "StateClimb" => begin_climb(s, input, t, wall.expect("entered only with a wall")),
        // The climb mode plays the pull-up on.
        "StateBasic" if def.motion_mode == CLIMB_MODE => {}
        "StateAttack" | "StateDash" | "StateBasic" => begin_action(s, input, t, next, on_ground),
        "StateDrive" => {
            // Plays its half of the change in the drive set; on the ground it picks the car
            // mode, or MoveMode for the first half while the stick does not line up. [game]
            stop_clip(s, t);
            if def.anim != 0 {
                start_clip(s, input, t, next, def.anim);
            }
            if on_ground {
                drive_or_move(s, input, t);
            }
        }
        // The set starts before the mode does (`FUN_0087d830`, `FUN_0087e4d0`), so the mode's
        // enter sees the clip the rules picked.
        "StateIdle" => {
            stop_clip(s, t);
            base_enter(s, input, t, IDLE_SET, from);
            set_mode(s, input, t, Mode::Stand);
        }
        "StateMove" => {
            stop_clip(s, t);
            base_enter(s, input, t, WALK_SET, from);
            set_mode(s, input, t, Mode::Move);
        }
        // The jump mode starts before the jump set does, so it still sees what was playing.
        "StateJump" => {
            stop_clip(s, t);
            enter_jump_mode(s, input, t, playing.as_deref(), on_ground);
            base_enter(s, input, t, JUMP_SET, from);
        }
        // The fall set starts first: no launch.
        "StateFall" => {
            stop_clip(s, t);
            base_enter(s, input, t, FALL_SET, from);
            enter_jump_mode(s, input, t, Some("FallSet"), on_ground);
        }
        "StateWeapon" => match def.motion_mode {
            // The weapon states' jump and fall: the jump mode, then the weapon set, whose
            // entry rules pick the clip from what was playing: a take-off out of the set's
            // own clips (to the left or right with the stick that way against the aim),
            // `Weapon_Fall` out of the jump and fall sets or when already going down. [game]
            JUMP_MODE => {
                let (from_set, from_id) = playing_clip(s, t);
                stop_clip(s, t);
                enter_jump_mode(s, input, t, playing.as_deref(), on_ground);
                strafe_entry(s, input, t, from_set, from_id);
            }
            // Weapon mode on foot: StrafeMode, then the weapon set; the clip of the state
            // being left is still there for the set to fade in over. [game]
            _ => {
                set_mode(s, input, t, Mode::Strafe);
                stop_clip(s, t);
            }
        },
        _ => {}
    }
    if def.class != "StateWeapon" {
        // Every other state starts a set of its own, which ends the weapon set's clips.
        (s.strafe.clip, s.strafe.tick) = (0, 0.0);
        s.strafe.layers.clear();
    }
    if !matches!(def.class.as_str(), "StateIdle" | "StateMove" | "StateJump" | "StateFall" | "StateHitReact" | "StateStun") {
        // A set of the states that play a clip of their own, or the weapon set, is not the
        // base controller's to run.
        s.base = Base::default();
    }
}

/// HitReactionSet's full-body clips drive BasicMotionMode. Flybacks keep the supplied
/// velocity in KnockBackMode until grounded, then use XY root motion for recovery.
/// [game: FUN_0087cd10, FUN_00875100, 00875088..008750c9]
fn step_hit_reaction(s: &mut State, t: &Tuning, dt: f32) {
    let Some(data) = s.base.current(&t.rule_sets) else { return };
    if s.hit_flyback && !s.ground_flag() {
        s.hit_last = Some((s.base.set, s.base.clip, s.base.tick));
        return;
    }
    let from = s.hit_last.filter(|&(set, id, _)| (set, id) == (s.base.set, s.base.clip)).map_or(0.0, |(_, _, tick)| tick);
    let moved = data.travel(from, s.base.tick).0 / dt.max(1e-6);
    let facing = s.facing();
    let right = Vec2::new(facing.y, -facing.x);
    let flat = right * moved.x + facing * moved.y;
    (s.vel.x, s.vel.y) = (flat.x, flat.y);
    s.hit_last = Some((s.base.set, s.base.clip, s.base.tick));
}

/// The set playing now, as JumpMode's enter looks at it: the climb's, or the state clip's.
fn playing_set(s: &State, t: &Tuning) -> Option<String> {
    if s.climb.is_some() {
        return Some("ClimbSet".to_owned());
    }
    let action = s.action.as_ref()?;
    t.actions.clips.get(&action.clip).map(|clip| clip.set.clone())
}

/// The first half of the change into the car (`Trans_R2V_A` and its running version) keeps
/// MoveMode, which turns him toward the stick, until the stick lines up with the way he is
/// moving (`FUN_0087bcf0`: within about 26 degrees, or the stick centred); everything else in
/// the drive states is the car mode. [game]
fn drive_or_move(s: &mut State, input: &Input, t: &Tuning) {
    let first_half = s.action.as_ref().is_some_and(|a| a.clip == crc32(b"Trans_R2V_A") || a.clip == crc32(b"Trans_R2V_Run_A"));
    let stick = input.stick;
    let lined_up = stick.length() <= 0.01 || {
        let moving = Vec2::new(s.vel.x, s.vel.y);
        let along = if moving.length() > 1e-8 { moving.normalize() } else { moving };
        stick.normalize().dot(along) >= DRIVE_LINE_UP
    };
    let mode = if first_half && !lined_up { Mode::Move } else { Mode::Drive };
    set_mode(s, input, t, mode);
}

/// Onto the wall: the climb mode's enter. [game]
fn begin_climb(s: &mut State, input: &Input, t: &Tuning, wall: Wall) {
    stop_clip(s, t);
    // The set's entry rule reads `I_IN_AIR`; taken to be the state machine's ground flag.
    let in_air = !s.ground_flag();
    s.climb = Some(Climb::enter(wall, &mut s.pos, s.vel, t, t.robot_radius, in_air));
    set_mode(s, input, t, Mode::Climb);
    s.gait = Gait::Idle;
    s.speed = 0.0;
}

/// Ends the clip of the state being left, and the climb: the dash mode's exit starts the
/// dodge's cooldown, and the ground punch's extra pull goes. [game]
fn stop_clip(s: &mut State, t: &Tuning) {
    if let Some(action) = s.action.take() {
        if t.control.states[action.state].class == "StateDash" {
            s.dash_cooldown = t.dash_cooldown;
        }
    }
    if let Some(climb) = s.climb.take() {
        (s.climb_top, s.climb_top_facing) = (climb.top, climb.top_facing);
    }
    s.punch_gravity = 0.0;
}

/// How far past the top target, in radii, the ledge is looked for; how far under it his feet
/// may be; and the least the ball must drop before what it touches counts. [game]
const CATCH_REACH_RADII: f32 = 2.25;
const CATCH_BELOW: f32 = 1.5;
const CATCH_LEAST_DROP: f32 = 0.25;
/// How square to the top's facing he must be, and how long that facing must be to count as
/// set (the climb mode's enter zeroes it). [game]
const CATCH_FACING: f32 = 0.98;
const CATCH_FACING_SET: f32 = 0.9;

/// The jump state's own update (`FUN_0087dde0`), run only when the state before was
/// `stateClimbJumpUp`: once he is coming down, still under the top the climb mode aimed for,
/// no further than his radius past it and facing as he did at the top, a ball of his radius
/// is let down 2.25 radii beyond the top; if it lands on something within 1.5 above his feet
/// he is put there, facing as at the top, with no speed (`FUN_00854790`, `FUN_00855940`).
/// The climb mode only sets the target when he goes over the top, so this follows the
/// "exit jump-up" of a wall with a ledge above it. [game] True if it caught him.
/// The reach above is his height (owner `+0x160`) plus how far his body's foot stands over
/// his own position when it does (`FUN_0070abe0`, the sum the camera's look-at height also
/// uses). The body here never leaves his position, so that part is zero. [game]
fn ledge_catch(s: &mut State, t: &Tuning, arena: &dyn World) -> bool {
    let (top, facing) = (s.climb_top, s.climb_top_facing);
    if s.vel.z >= 0.0 || facing.length_squared() <= CATCH_FACING_SET || top.z <= s.pos.z {
        return false;
    }
    let (radius, height) = (t.robot_radius, t.robot_height);
    let past = (Vec2::new(s.pos.x, s.pos.y) - Vec2::new(top.x, top.y)).dot(facing);
    if past >= radius || s.facing().dot(facing) <= CATCH_FACING {
        return false;
    }
    let over = Vec2::new(top.x, top.y) + facing * (radius * CATCH_REACH_RADII);
    let from = Vec3::new(over.x, over.y, top.z + height);
    let Some(hit) = arena.drop_onto(from, s.pos.z, radius) else { return false };
    let feet = s.pos.z;
    if hit.z <= feet || hit.z - CATCH_BELOW >= feet || hit.z >= feet + height || (hit.z - from.z).abs() <= CATCH_LEAST_DROP {
        return false;
    }
    s.pos = hit;
    s.yaw = yaw_of(facing);
    s.vel = Vec3::ZERO;
    s.speed = 0.0;
    true
}

/// Starts a control state that plays its own clip, and the motion mode it names: the attack
/// and dash modes (the clip's root motion moves him), the jump mode (air attacks, the ground
/// punch's fall, the leap out of the car), the unfold. [game]
fn begin_action(s: &mut State, input: &Input, t: &Tuning, state: usize, on_ground: bool) {
    let def = &t.control.states[state];
    let from_climb = s.climb.is_some();
    let lifted = def.motion_mode == FIRST_MODE && s.mode == Mode::Drive;
    // The attack mode is not started again while it runs (`FUN_00852150`), so a chain of
    // attacks keeps its target, unless the attack before hit a character: then the mode
    // lets the target go once the hit list is emptied. [game]
    let kept = s
        .action
        .as_ref()
        .filter(|a| t.control.states[a.state].motion_mode == ATTACK_MODE && !a.has_hit)
        .and_then(|a| a.target)
        .filter(|_| def.motion_mode == ATTACK_MODE);
    stop_clip(s, t);
    let mut heading = s.yaw;
    let clip = if def.class == "StateDash" {
        // The dodge set's incoming rules pick the clip from the stick against the aim
        // (`F_L_STICK_DIR`, degrees, positive to the left). [data]
        heading = input.aim.map_or(s.yaw, yaw_of);
        let mut angle = (yaw_of(input.stick) - heading).to_degrees() % 360.0;
        if angle > 180.0 {
            angle -= 360.0;
        } else if angle < -180.0 {
            angle += 360.0;
        }
        // The set's entry rules are walked as the controller walks them: front within 45
        // degrees (from -45 up to, not including, 45), back from 135 round either way, left
        // and right between. [game] Without the set loaded, the thresholds the rules are
        // named for do the same but for the two edges they include. [stand-in]
        let by_rules = t.rule_sets.get(&DASH_SET).and_then(|set| {
            let get = |variable: u32| match variable {
                animrules::var::L_STICK_DIR => Some(Value::Float(angle)),
                _ => rule_value(s, input, variable),
            };
            animrules::entry(&set.incoming, 0, 0, &get).filter(|&(to_set, _)| to_set == DASH_SET).map(|(_, id)| id)
        });
        let a = &t.actions;
        let id = by_rules.unwrap_or_else(|| {
            crc32(
                if angle.abs() <= a.dodge_forward {
                    "DodgeFront"
                } else if angle.abs() >= a.dodge_back {
                    "DodgeBack"
                } else if angle < 0.0 {
                    "DodgeRight"
                } else {
                    "DodgeLeft"
                }
                .as_bytes(),
            )
        });
        s.yaw = heading;
        id
    } else if def.class=="StateAttack" && def.anim==0 {
        let (from_set,from_id)=playing_clip(s,t);
        t.rule_sets.get(&crc32(b"CombatSet")).and_then(|set|animrules::entry(&set.incoming,from_set,from_id,&|v|rule_value(s,input,v))).map_or(0,|(_,id)|id)
    } else {
        def.anim
    };
    if !start_clip(s, input, t, state, clip) {
        return;
    }
    if let Some(action) = s.action.as_mut() {
        action.heading = heading;
        action.from_climb = from_climb;
        action.lifted = lifted;
        action.target = kept;
    }
    if def.motion_mode == ATTACK_MODE {
        s.fight = FIGHT_TIME;
    }
    match def.motion_mode {
        // The set starts before the mode, so the jump mode sees the state's own set. [game]
        JUMP_MODE => {
            let set = playing_set(s, t);
            enter_jump_mode(s, input, t, set.as_deref(), on_ground);
        }
        UNFOLD_MODE => set_mode(s, input, t, Mode::Unfold),
        _ => {
            set_mode(s, input, t, Mode::Action);
            s.gait = Gait::Idle;
        }
    }
}

/// Starts a state's clip. False if the character has no such clip. [data]
fn start_clip(s: &mut State, _input: &Input, t: &Tuning, state: usize, clip: u32) -> bool {
    let Some(data) = t.actions.clips.get(&clip) else { return false };
    let fade_time = if data.crossfade < 0.0 { DEFAULT_CROSSFADE } else { data.crossfade };
    let heading = s.yaw;
    s.action = Some(ActionState {
        state,
        clip,
        tick: 0.0,
        finished: false,
        last_tick: 0.0,
        events_done: 0,
        branch: 1,
        window_open: false,
        pressed_in_window: false,
        can_branch: false,
        slide: false,
        face: false,
        stick: false,
        hitting: false,
        live: Vec::new(),
        triggers: Vec::new(),
        target: None,
        has_hit: false,
        carry: Vec2::new(s.vel.x, s.vel.y),
        fade: if fade_time > 0.0 { 0.0 } else { 1.0 },
        fade_time,
        heading,
        from_climb: false,
        lifted: false,
    });
    // The clip's events at its first tick count at once.
    pass_events(s.action.as_mut().unwrap(), data, &Pad::default());
    true
}

/// Moves the clip on and handles what its events say. [game]
fn play_action(s: &mut State, t: &Tuning, pad: &Pad, dt: f32) {
    let Some(action) = s.action.as_mut() else { return };
    let Some(data) = t.actions.clips.get(&action.clip) else { return };
    action.last_tick = action.tick;
    action.tick += dt * crate::formats::anim::TICKS_PER_SECOND * data.speed;
    if action.tick >= data.ticks {
        if data.looping {
            pass_events(action, data, pad);
            action.tick %= data.ticks.max(1.0);
            action.events_done = 0;
            action.branch = 1;
        } else {
            action.tick = data.ticks;
            action.finished = true;
        }
    }
    pass_events(action, data, pad);
    // The attack state notes a press of the attack button while the window is open. [game]
    if action.window_open && !action.pressed_in_window && pad.was_hit(button::ATTACK_FAST) {
        action.pressed_in_window = true;
    }
    action.fade = (action.fade + dt / action.fade_time.max(1e-3)).min(1.0);
}

fn pass_events(action: &mut ActionState, data: &ActionClip, pad: &Pad) {
    while let Some(event) = data.events.get(action.events_done).filter(|e| e.time <= action.tick) {
        action.events_done += 1;
        match event.action {
            ClipAction::BranchPoint => action.branch += 1,
            ClipAction::ButtonWindow { open: true } => {
                action.window_open = true;
                action.pressed_in_window = pad.was_hit(button::ATTACK_FAST);
            }
            ClipAction::ButtonWindow { open: false } => {
                if action.pressed_in_window {
                    action.can_branch = true;
                }
                action.window_open = false;
                action.pressed_in_window = false;
            }
            ClipAction::FaceAndSlide { slide, face, stick } => (action.slide, action.face, action.stick) = (slide, face, stick),
            ClipAction::Hit { on, node } => {
                action.hitting = on;
                action.live.retain(|&live| live != node);
                if on {
                    action.live.push(node);
                }
            }
            ClipAction::Trigger { name } => action.triggers.push(name),
        }
    }
}

/// The attack and dash modes: the clip's root motion moves him, faded in over what he was
/// doing; the attacks turn toward the stick while their clip allows it, the dodge keeps the
/// heading it started with. [game]
fn step_action(s: &mut State, input: &Input, t: &Tuning, arena: &dyn World, dt: f32) {
    let Some(action) = s.action.as_ref() else {
        s.mode = Mode::Move;
        return;
    };
    let def = &t.control.states[action.state];
    if def.class == "StateDash" {
        s.yaw = action.heading;
    } else if def.motion_mode == ATTACK_MODE {
        attack_aim(s, input, t, arena, dt);
    }
    let Some(action) = s.action.as_ref() else { return };
    let Some(data) = t.actions.clips.get(&action.clip) else { return };
    // Root motion over this step, from the clip's own frame to the world's. A loop that came
    // round counts for nothing on that step.
    let moved = if dt > 1e-6 && action.tick >= action.last_tick {
        (data.root_at(action.tick) - data.root_at(action.last_tick)) / dt
    } else {
        Vec3::ZERO
    };
    // A clip whose root turns turns him with it, and its travel is in the frame it started
    // in (only `Wall_JumpOff` turns: 124 degrees). [assumed: the original moves the body by
    // the root's whole transform; the dodges, whose root sits turned and never turns, agree]
    let mut facing = s.facing();
    if data.turn.iter().any(|turn| turn.abs() > 1e-3) {
        s.yaw = action.heading + data.turn_at(action.tick);
        facing = Vec2::new(-action.heading.sin(), action.heading.cos());
    }
    let right = Vec2::new(facing.y, -facing.x);
    let own = right * moved.x + facing * moved.y;
    let mut v = action.carry.lerp(own, action.fade);
    // `stop_on_hit` (the charge attack): once it has hit a character, on the ground, the
    // animation's root mode goes to 2 (`FUN_00855af0`: the clip's root no longer moves the
    // body) and the hit stops him dead (`FUN_00717d80`: a melee hit that is not a knock-back
    // kind zeroes his velocity). Taken back when the attack state ends. [game]
    if def.motion_mode == ATTACK_MODE
        && action.has_hit
        && s.ground_flag()
        && data.attack.is_some_and(|attack| attack.stop_on_hit)
    {
        v = Vec2::ZERO;
    }
    (s.vel.x, s.vel.y) = (v.x, v.y);
    if action.from_climb || action.lifted {
        // Off the wall the clip lifts him too, as the climb's clips do. [assumed]
        s.vel.z = moved.z;
    }
    s.speed = v.dot(s.facing());
    if def.motion_mode == ATTACK_MODE && !s.ground_flag() {
        // Off the ground the attack mode lets him rise no faster than this. [game]
        s.vel.z = s.vel.z.min(ATTACK_RISE_MOST);
    }
}

/// The attack mode turns to a target only when it is more than this off it (`FUN_00717770`),
/// and caps his upward speed at this while he is off the ground. [game]
const ATTACK_TURN_LEAST: f32 = 0.017_453_292;
const ATTACK_RISE_MOST: f32 = 2.0;
/// The slide wants the target further than this (squared), and the stick counts as held for
/// choosing a target from this deflection. [game]
const SLIDE_LEAST_SQUARED: f32 = 0.1;
const AIM_STICK_LEAST: f32 = 0.001;

/// The attack mode's own part of its update (`FUN_00855af0`), before the clip moves him.
/// Until the attack has hit a character, and while the clip allows a slide or a turn
/// (`AttackFaceAndSlideEvent`): it looks along the stick, or along his facing with no stick,
/// for a target (`melee::Query`), once, and keeps it; while the target is further than
/// touching (the two radii) it turns him to it at `auto_target_turn_speed` and slides him
/// toward it at `slideSpeed`, by moving the body itself, as long as the target is between
/// `minRange` and `maxRange` away. Otherwise, while the clip allows stick movement, he
/// turns toward the stick at `turn_speed`; not after a hit if the clip says `stop_on_hit`.
/// [game] Not read, so not here: character flag 0x15, which also stops the stick's turn;
/// (`stop_on_hit` itself is done in `step_action`);
/// the test that leaves a target alone in one of its states (`FUN_007258d0`); the attack
/// mode in the air (its `+0x2a`), whose slide keeps to his height: his air attack has no
/// slide speed.
fn attack_aim(s: &mut State, input: &Input, t: &Tuning, arena: &dyn World, dt: f32) {
    use std::f32::consts::PI;
    let Some(action) = s.action.as_ref() else { return };
    let Some(attack) = t.actions.clips.get(&action.clip).and_then(|clip| clip.attack) else { return };
    let (slide, face, stick) = (action.slide, action.face, action.stick);
    let targets = arena.targets();
    let find = |id: u32| targets.iter().find(|target| target.id == id);
    let has_hit = action.has_hit || s.hit_list.iter().any(|&id| find(id).is_some_and(|target| target.character));
    // A target that is gone is looked for again.
    let mut target = action.target.filter(|&id| find(id).is_some());
    let mut turned = false;
    if !has_hit && (slide || face) {
        let mut direction = s.facing();
        if input.stick.length() >= AIM_STICK_LEAST {
            direction = input.stick.normalize();
            // A stick further round than the clip allows is taken at the limit. As the
            // original computes it, the limit is put on the side AWAY from the stick, which
            // looks like a slip of theirs; kept as it is. Only the slam out of the car has a
            // limit under half a turn (110 degrees). [game]
            let most = attack.stick_choose_target_max_degrees.to_radians().clamp(0.0, PI);
            let off = melee::wrap(s.yaw - yaw_of(direction));
            if most < PI && off.abs() > most {
                let yaw = melee::wrap(if off > 0.0 { s.yaw + most } else { s.yaw - most });
                direction = Vec2::new(-yaw.sin(), yaw.cos());
            }
        }
        if target.is_none() {
            let angle = attack.auto_target_search_angle.to_radians().clamp(0.0, PI);
            target = melee::Query::new(s.pos, direction.extend(0.0), attack.max_range, angle, &t.melee).best(targets, &t.melee);
        }
        if let Some(found) = target.and_then(find) {
            let d = found.pos - s.pos;
            let flat = Vec2::new(d.x, d.y);
            let touching = t.robot_radius + if found.character { found.radius } else { 0.0 };
            if flat.length_squared() > touching * touching {
                if attack.auto_target_turn_speed > 0.0 && face {
                    let off = melee::wrap(yaw_of(flat) - s.yaw);
                    if off.abs() > ATTACK_TURN_LEAST {
                        let most = attack.auto_target_turn_speed.to_radians() * dt;
                        s.yaw += off.clamp(-most, most);
                    }
                    turned = true;
                }
                let away = d.length_squared();
                let in_range = away > attack.min_range * attack.min_range && away < attack.max_range * attack.max_range;
                if attack.slide_speed > 0.0 && slide && in_range && away > SLIDE_LEAST_SQUARED {
                    let away = away.sqrt();
                    // A whole step even if it ends inside touching distance; the bodies'
                    // own collision parts them again. [game]
                    s.pos += d / away * (attack.slide_speed * dt).min(away);
                }
            }
        }
    }
    let may_turn = stick && attack.turn_speed > 0.0 && !(attack.stop_on_hit && has_hit);
    if !turned && may_turn && input.stick.length_squared() > STICK_DEAD_SQUARED {
        s.yaw = turn_toward(s.yaw, yaw_of(input.stick), attack.turn_speed.to_radians() * dt);
    }
    if let Some(action) = s.action.as_mut() {
        (action.target, action.has_hit) = (target, has_hit);
    }
}

/// The climb mode's update, and the climbable test run again from where he is. [game]
fn step_climb(s: &mut State, input: &Input, t: &Tuning, arena: &dyn World, dt: f32) {
    let Some(mut climb) = s.climb.take() else {
        s.mode = Mode::Jump(JumpType::Fall);
        return;
    };
    let on_ground = s.pos.z <= arena.floor(s.pos.x, s.pos.y, s.pos.z) + 0.01;
    let wall = climb::find(arena, s.pos, s.facing(), t.robot_radius, t.robot_height, t.jump.standing.height);
    let body = climb::Body { pos: &mut s.pos, yaw: &mut s.yaw, radius: t.robot_radius, height: t.robot_height, on_ground };
    let (velocity, message) = climb::update(&mut climb, body, input.pad_stick, wall, t, dt);
    s.vel = velocity;
    s.speed = 0.0;
    s.gait = Gait::Idle;
    s.messages.extend(message);
    // Every update, by what he is doing on the wall after it (`FUN_00856d00`): up and the
    // shimmies start preset 9, down preset 10. [game] Preset 9 lasts 0 seconds, so going
    // up or sideways is silent and going down hums; that is what the data gives. [data]
    match climb.phase {
        climb::Phase::Up | climb::Phase::ShimLeft | climb::Phase::ShimRight => preset_rumble(s, t, rumble::preset::CLIMB_MOVED),
        climb::Phase::Down => preset_rumble(s, t, rumble::preset::CLIMB_SLIDING),
        _ => {}
    }
    s.climb = Some(climb);
}

/// The car mode. When he leaves it, and how (the unfold, the leap out with the jump button
/// held as the trigger goes up), is the state machine's (`stateDrive`'s transitions). [game]
fn step_drive(s: &mut State, input: &Input, t: &Tuning, arena: &dyn World, dt: f32) {
    // The game's rules are written per update and several depend on the update's length (the
    // wheel angle has it built in, and the engine pushes for one update past the speed cap),
    // so they are run in whole updates of the game's own length whatever the caller's step.
    // Wheel queries, drive limits, spring accumulation and body integration form
    // one physics update. Splitting integration at the render rate changes the
    // spring damping and clips the response between forces. [game/trace]
    s.car.turbo_asked |= input.turbo;
    s.car.clock += dt;
    s.gearbox_inputs.clear();
    s.tyre_inputs.clear();
    if s.car.chassis.wheels[0].rest_length<=0.0 {
        s.car.chassis.probe(s.pos,s.vel,crate::vehicle::rotation(s.yaw,s.pitch,s.roll),
            t.vehicle_height,&t.drive,arena,GAME_UPDATE);
        s.car.airborne=!s.car.chassis.grounded();
    }
    while s.car.clock >= GAME_UPDATE {
        s.car.clock -= GAME_UPDATE;
        s.car.previous_presentation=Some(s.car_snapshot());
        let mut angles=Vec3::new(s.pitch,s.roll,s.yaw);
        s.car.chassis.prepare_frame(&mut s.pos,&mut angles,&t.drive);
        (s.pitch,s.roll,s.yaw)=(angles.x,angles.y,angles.z);
        let was_grounded=s.car.chassis.grounded();
        s.car.chassis.probe(s.pos,s.vel,crate::vehicle::rotation(s.yaw,s.pitch,s.roll),
            t.vehicle_height,&t.drive,arena,GAME_UPDATE);
        s.car.airborne=!s.car.chassis.grounded();
        if !was_grounded && !s.car.airborne && s.car.time>CAR_LANDING_AFTER {
            preset_rumble(s,t,rumble::preset::VEHICLE_HIT_GROUND);
        }
        drive_update(s, input, t, GAME_UPDATE);
        s.car.chassis.finish_step(&mut s.pos,&mut s.vel,GAME_UPDATE);
        let speed_before=s.vel.length();
        let mut angles=Vec3::new(s.pitch,s.roll,s.yaw);
        s.vehicle_contacts.extend(s.car.chassis.advance(&mut s.pos,&mut s.vel,&mut angles,
            t.vehicle_height,t.gravity,&t.drive,arena,GAME_UPDATE));
        (s.pitch,s.roll,s.yaw)=(angles.x,angles.y,angles.z);
        s.car.yaw_rate=s.car.chassis.angular.z;
        if speed_before>CAR_KNOCK_SPEED && s.vehicle_contacts.iter().any(|c|c.normal.z<0.1) {
            preset_rumble(s,t,rumble::preset::VEHICLE_HIT_WALL);
        }
    }
    s.speed = Vec2::new(s.vel.x, s.vel.y).dot(s.facing());
}

/// One update of the car, as the drive mode and its wheel set do it (`notes\driving.md`),
/// with independent 3D wheel forces; caps/spin response checked against run1. [game/trace]
fn drive_update(s: &mut State, input: &Input, t: &Tuning, h: f32) {
    let d = &t.drive;
    let facing = s.facing();
    let mut velocity = Vec2::new(s.vel.x, s.vel.y);
    let set_speed = |velocity: &mut Vec2, speed: f32| {
        if velocity.length() > 1e-4 {
            *velocity = velocity.normalize() * speed.max(0.0);
        }
    };
    let grounded = !s.car.airborne;
    s.car.time += h;
    let braking = input.brake > TRIGGER_DEAD && grounded;

    // [game: 0085cd56..0085cda0] Player trigger filtering happens before the caps.
    s.car.filtered_trigger = crate::driving::filter_trigger(s.car.filtered_trigger,
        input.vehicle.clamp(0.0, 1.0), d.trigger_filter_strength, h);
    // [game: 00733e10] The boost needs lateral motion, not just a fast slide. Its
    // active state continues if the drift button is pressed again, even in the air.
    // [assumed] The registered drift ability is sampled before the car's caps here;
    // its scheduler order relative to the mode is unread (notes/status.md, fidelity).
    let right = Vec2::new(facing.y, -facing.x);
    let drift = s.car.drift_turbo.step(velocity.dot(right), input.drift,
        s.turbo_left > 0.0, d, h);
    if drift.started {
        s.turbo_started = true;
        s.turbo_fx_on = true;
        preset_rumble(s, t, rumble::preset::VEHICLE_TURBO);
    }
    if drift.ended {
        s.car.after_turbo = Some(d.max_drift_turbo_speed);
        s.turbo_fx_on = false;
    }
    let slide_boost = s.car.drift_turbo.active();

    // The slide: while its button is held with the wheels down, every tyre keeps only a
    // little of its grip and the car is slowed along its own motion. [game]
    let sliding = input.drift && grounded;
    if sliding {
        let speed = velocity.length();
        set_speed(&mut velocity, (speed - d.skid_out_deceleration * h).max(0.0));
    }
    s.car.sliding = sliding;

    // Turbo: runs its time, then waits out its cooldown. [game: 00733180/00732e60]
    if std::mem::take(&mut s.car.turbo_asked) && s.turbo_left <= 0.0 && s.turbo_cooldown <= 0.0 {
        s.turbo_left = crate::driving::turbo_times(d, s.turbo_upgrades).0;
        s.turbo_started = true;
        s.turbo_fx_on = true;
        // The turbo starting rumbles the pad (`FUN_0085cad0`). [game]
        preset_rumble(s, t, rumble::preset::VEHICLE_TURBO);
    }
    let boosting = s.turbo_left > 0.0;

    // Reverse: the brake held at a stop for a moment; over when the brake is up and he is
    // no longer rolling backward.
    let forward_speed = velocity.dot(facing);
    let mut stopped_by_brake = false;
    if !braking {
        s.car.brake_held = 0.0;
        if forward_speed >= 0.0 {
            s.car.reversing = false;
        }
    }

    // The speed caps, first that applies. The trigger scales the top speed, not the push.
    let speed = velocity.length();
    if !braking || s.car.reversing {
        s.car.braking_from = None;
    }
    if boosting {
        s.car.after_turbo = None;
        if speed > d.max_turbo_speed {
            set_speed(&mut velocity, d.max_turbo_speed);
        }
    } else if slide_boost {
        if speed > d.max_drift_turbo_speed {
            set_speed(&mut velocity, d.max_drift_turbo_speed);
        }
    } else if s.car.reversing {
        if speed > d.max_reverse_speed {
            set_speed(&mut velocity, d.max_reverse_speed);
        }
    } else if braking {
        let from = s.car.braking_from.unwrap_or(speed);
        let now = (from - d.braking_deceleration * h).max(0.0);
        s.car.braking_from = Some(now);
        set_speed(&mut velocity, now);
        if now <= 0.0 {
            // Braked to a stop: the engine is cut, and holding on a moment longer reverses.
            velocity = Vec2::ZERO;
            s.car.spin = [0.0; 4];
            stopped_by_brake = true;
            s.car.brake_held += h;
            if s.car.brake_held >= d.min_reverse_brake_time {
                s.car.reversing = true;
            }
        }
    } else {
        let normal = crate::driving::allowed_speed(d, s.car.filtered_trigger, true);
        let cap = match s.car.after_turbo {
            Some(ramp) if ramp - d.post_turbo_deceleration * h > normal => {
                let ramp = ramp - d.post_turbo_deceleration * h;
                s.car.after_turbo = Some(ramp);
                ramp
            }
            _ => {
                s.car.after_turbo = None;
                normal
            }
        };
        if speed > cap {
            set_speed(&mut velocity, cap);
        }
    }

    s.car.capped = velocity.length();
    // What the wheel set shows the gearbox this update (`FUN_007a48e0`). [game]
    // [game] The trigger is filtered; turbo is either cee630d9 (standard) or ea8020ae
    // (drift), whose identities are confirmed in DriveMode's constructor 0085c2f0.
    // [assumed] (`notes\sound.md`, 3 and 5): brake amount and the velocity's sampling
    // order relative to the vehicle sound update. The velocity is taken
    // here, after the caps, where the game reads the body's at its sound update.
    s.gearbox_inputs.push(GearboxInput {
        velocity: [s.vel.x, s.vel.y, s.vel.z],
        trigger: s.car.filtered_trigger,
        stick_x: input.steer,
        braking: braking && !s.car.reversing,
        skid: sliding,
        reversing: s.car.reversing,
        stopped_by_brake,
        grounded,
        turbo: boosting || slide_boost,
    });
    if let Some(gearbox)=s.vehicle_gearbox.as_mut() {
        gearbox.update(s.gearbox_inputs.last().unwrap(),h);
    }
    // What the same update shows the tyres' and the suspension's sounds (`FUN_007347c0`,
    // `notes\sound.md` "The tyres and the suspension"). [game]
    // [game: 0085f8b0, dt=0] The tyre sound's cap skips the trigger, even if filtered.
    // [assumed] the brake button's amount and the steering stick's identity.
    // Contact/material and shortening are independent for all four wheels. [game]
    let wheels=s.car.chassis.wheels.map(|w|crate::sound::TyreWheel {
        grounded:w.grounded,was_grounded:w.was_grounded,point:w.point,surface:w.surface,
        compression:(w.previous_length-w.length)/w.rest_length.max(0.01),
    });
    let sound_wheel=if velocity.dot(right)<0.0 {2} else {3};
    let suspension_wheel=if wheels[2].compression>wheels[3].compression {2} else {3};
    s.tyre_inputs.push(TyreInput {
        speed: s.vel.length(),
        drift: input.drift,
        brake: input.brake,
        reversing: s.car.reversing,
        waiting_to_reverse: stopped_by_brake,
        stick_x: input.steer,
        cap: crate::driving::allowed_speed(d, s.car.filtered_trigger, false),
        grounded,
        surface: if wheels[sound_wheel].grounded {wheels[sound_wheel].surface} else {0},
        compression: wheels[suspension_wheel].compression,
        wheels,per_wheel:true,sound_wheel,suspension_wheel,
    });

    // What the engine is asked for.
    let throttle = if stopped_by_brake {
        0.0
    } else if boosting {
        d.throttle * d.turbo_throttle_boost
    } else if slide_boost {
        d.throttle * d.drift_turbo_boost_factor
    } else if s.car.reversing {
        -d.throttle
    } else if braking {
        if forward_speed >= 0.0 { d.throttle } else { -d.throttle }
    } else if forward_speed < 0.0 {
        d.post_reverse_throttle
    } else {
        d.throttle
    };

    // Steering: the front wheels are turned by this much, an angle with the update's length
    // in it. Fast at a standstill, slower at speed.
    let pace = (velocity.length() / d.max_speed.max(1.0)).clamp(0.0, 1.0);
    let steer = (d.max_turn_speed - (d.max_turn_speed - d.turn_speed) * pace) * -input.steer.clamp(-1.0, 1.0) * h;
    // 007a10d0 rotates physical tyre axes by NEGATIVE mode steer;008601d0
    // multiplies that mode steer by -3 for the visual. Keep the mode value,
    // rather than exposing the already-negated physical angle to animation. [game]
    s.steer_angle = -steer;

    // Each wheel: the engine spins it up, and the tyre pushes on the body by how far its rim
    // and the ground under it disagree. Sideways slip is opposed outright; forward push comes
    // only from the wheel spinning faster than the ground, so it builds over about a second.
    let turn=crate::vehicle::rotation(s.yaw,s.pitch,s.roll);
    // 007a34c0 aligns the wheel-force frame to the support plane before 007a2860.
    // The spring anchors use the restored body's own orientation in physics. [game]
    let support=crate::vehicle::support_rotation(turn,s.car.chassis.normal);
    let mut velocity3=velocity.extend(s.vel.z);
    s.car.chassis.render_step(velocity3,turn,stopped_by_brake,boosting||slide_boost,h);
    s.car.chassis.angular.z=s.car.yaw_rate;
    let (side_grip, forward_grip) = if sliding { (d.wheel_drift_factor, d.drift_steering_factor) } else { (1.0, 1.0) };
    if !grounded {
        // 007a2860 clears torque for airborne wheels; no fictitious engine spin-up.
        velocity3+=(turn*Vec3::X*input.steer.clamp(-1.0,1.0)+turn*Vec3::Y*input.stick.y)*CAR_AIR_PUSH*h;
        s.car.yaw_rate -= s.car.yaw_rate * h;
        s.car.chassis.angular.z=s.car.yaw_rate;
        s.car.chassis.damp_angular(d);
        // 00862000's airborne recovery precedes world physics. [game]
        for angle in [&mut s.pitch,&mut s.roll] {
            if angle.abs()>0.17453294 {*angle-=angle.signum()*(0.17453294*h).min(angle.abs());}
        }
        s.vel=velocity3;
        return;
    }
    let up=turn*Vec3::Z;
    if up.z>0.9 && velocity3.dot(up)>0.0 {velocity3-=up*velocity3.dot(up)*0.95;}
    for (index,mount) in crate::vehicle::mounts(d).into_iter().enumerate() {
        if !s.car.chassis.wheels[index].grounded {continue;}
        let spin=&mut s.car.spin[index];
        let radius=if index<2 {d.front_wheel_radius} else {d.rear_wheel_radius};
        let radius=radius.max(0.1);
        let arm=support*mount;
        let at_wheel=velocity3+s.car.chassis.angular.cross(arm);
        let wheel_turn=support*Quat::from_rotation_z(if index<2 {steer} else {0.0});
        let rolling=wheel_turn*Vec3::Y;
        let sideways=wheel_turn*Vec3::X;
        let slip = at_wheel - rolling * (*spin * radius);
        let lateral = slip.dot(sideways);
        let lateral = if lateral.abs() < 0.01 { 0.0 } else { lateral };
        let along = slip.dot(rolling);
        let push = -sideways * lateral * d.wheel_friction * side_grip - rolling * along * forward_grip;
        *spin += (radius * along + d.engine_torque * throttle) / (radius * radius) * h;
        if !stopped_by_brake {s.car.chassis.add_force(push,arm);}
        s.car.chassis.wheels[index].spin=*spin;
    }
    s.car.chassis.transfer_weight(velocity3,support,d,stopped_by_brake,s.car.reversing,
        s.vehicle_gearbox.as_ref().map_or(0.0,crate::sound::Gearbox::progress));
    s.car.chassis.damp_angular(d);
    s.car.yaw_rate=s.car.chassis.angular.z;
    s.vel=velocity3;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tuning::{DriveTuning, JumpTuning, Locomotion, MoveTuning, RootCurve};

    const DT: f32 = 1.0 / 60.0;

    /// The numbers Bumblebee has in the game's data, so the rules can be checked without the install.
    fn tuning() -> Tuning {
        let kind = |height, speed, control, turn_speed| JumpKind { height, speed, control, turn_speed };
        let mut t = Tuning {
            base_health: 320.0,
            health_regen_rate: 39.0,
            health_regen_delay: 1.5,
            robot_height: 5.4,
            robot_radius: 2.0,
            vehicle_height: 1.0,
            vehicle_radius: 2.5,
            wheelbase: 3.36,
            gravity: 35.0,
            transform_delay: 0.25,
            moving: MoveTuning { max_speed: 24.0, turn_speed: 360.0, walk_run_speed: 5.0 },
            locomotion: Locomotion {
                walk_speed: 5.61,
                run_speed: 24.0,
                move_stick: 0.01,
                run_stick: 0.7,
                idle_blend: 0.2,
                walk_blend: 0.25,
                run_blend: 0.25,
                // The `Root` channel of Run2Idle and Jump_LandRun, read every second tick.
                stop: every_second_tick(0.1, &[1.20, 1.70, 2.31, 2.97, 3.58, 4.05, 4.37, 4.56, 4.65, 4.68, 4.69]),
                land_run: every_second_tick(0.1, &[0.99, 1.02, 1.53, 1.97, 2.38, 2.80, 3.21, 3.63, 4.04, 4.46, 4.88]),
                big_land_run: RootCurve::new(0.0, (0..=28).map(|tick| 6.18 * tick as f32 / 28.0).collect()),
                takeoff: RootCurve::new(0.0, (0..=10).map(|tick| 0.99 * tick as f32 / 10.0).collect()),
                in_air_speed: 0.31,
                long_fall: 1.0,
            },
            jump: JumpTuning {
                standing: kind(6.0, 15.0, 0.1, 120.0),
                run: kind(10.0, 35.0, 0.038, 120.0),
                high_standing: kind(10.0, 35.0, 0.1, 65.0),
                long: kind(14.0, 40.0, 0.025, 65.0),
                climb_up_height: 15.0,
                climb_off_height: 15.0,
                climb_off_speed: 30.0,
                weapon_dilation_start: 5.0,
                weapon_dilation: 0.25,
                weapon_dilation_rate: 4.0,
            },
            climb: crate::tuning::ClimbTuning {
                pull_up_dist: 8.0,
                edge_limit_dist: 3.0,
                wall_offset: 0.0,
                arrival_deceleration: 50.0,
            },
            drive: DriveTuning {
                max_speed: 35.0,
                trigger_filter_strength: 1.0,
                turning_velocity_delta: 3.0,
                max_turbo_speed: 43.0,
                max_turbo_time: 2.4,
                max_turbo_cooldown_time: 3.2,
                throttle: 12.0,
                turbo_throttle_boost: 2.0,
                post_turbo_deceleration: 20.0,
                braking_deceleration: 15.0,
                max_reverse_speed: 15.0,
                post_reverse_throttle: 30.0,
                exit_to_run_deceleration: 35.0,
                turn_speed: 4.0,
                max_turn_speed: 8.0,
                engine_torque: 15000.0,
                wheel_friction: 5000.0,
                front_axle_length: 2.0,
                rear_axle_length: 2.0,
                long_axle_length: 5.0,
                front_wheel_radius: 1.6,
                rear_wheel_radius: 2.0,
                max_drift_turbo_speed: 38.0,
                min_reverse_brake_time: 0.05,
                max_angular_vel_z: 4.0,
                max_angular_vel_x: 0.35,
                max_angular_vel_y: 0.35,
                exit_to_run_min_speed: 24.0,
                exit_to_run_turn_speed: 115.0,
                exit_to_idle_deceleration: 35.0,
                exit_to_idle_min_speed: 0.0,
                exit_to_idle_turn_speed: 115.0,
                wheel_drift_factor: 0.05,
                drift_steering_factor: 0.4,
                skid_out_deceleration: 40.0,
                min_drift_time: 0.2,
                min_drift_turbo_speed: 20.0,
                drift_turbo_time: 2.0,
                drift_turbo_boost_factor: 2.0,
                suspension_length: 1.0,
                suspension_k: 100000.0,
                suspension_damp: 0.5,
                suspension_wt_scale: 0.85,
                angular_vel_damp_x: 0.75,
                angular_vel_damp_y: 0.75,
                body_half: Vec3::new(1.5,3.0,1.0),
                body_offset_z: 1.6,
                render_wheel_radius: 0.4,
                min_render_wheel_offset: -0.1,
                max_render_wheel_offset: 0.1,
                ..Default::default()
            },
            ..Default::default()
        };
        common_graph(&mut t);
        t
    }

    /// Bumblebee's graph for standing, moving, jumping, falling, weapon mode and the change of
    /// form both ways, copied from `bnxglobal.str#16` (`work\bb_machines.txt`, machines Common,
    /// DriveAdvance and Weapon), with the change-of-form clips' lengths and branch points.
    fn common_graph(t: &mut Tuning) {
        use crate::control::{Detector, Graph, Stage, StateDef, StickRange, Test, Transition};
        use crate::formats::anim::{Action as A, ActionEvent};
        let stick = |amp_min: f32, amp_max: f32| StickRange {
            amp_min,
            amp_max,
            angle_min: -std::f32::consts::TAU,
            angle_max: std::f32::consts::TAU,
            camera_relative: true,
        };
        let stage = |include: &[u32], exclude: &[u32], hit: Option<u32>, amp: (f32, f32)| Stage {
            time: 0.0,
            charge: 0.0,
            include: include.to_vec(),
            exclude: exclude.to_vec(),
            hit,
            left: stick(amp.0, amp.1),
        };
        let any = (-1.0, 100.0);
        let detect = |stages: Vec<Stage>, stays_on: f32| Test::Detect(Detector { stages, stays_on, hold_with_last_stage: false });
        let mut g = Graph::default();
        let mv = g.add_connection("connectionMove", detect(vec![stage(&[], &[], None, (0.18, 2.0))], 1e5));
        let release_move = g.add_connection("connectionReleaseMove", detect(vec![stage(&[], &[], None, (0.0, 0.15))], 1e5));
        let jump = g.add_connection("connectionJump", detect(vec![stage(&[], &[], Some(button::JUMP), any)], 0.1));
        g.connections[jump].on_ground = true;
        let fall = g.add_connection("connectionFall", Test::DistToGround { limit: 3.0, greater: true });
        g.connections[fall].in_air = true;
        g.connections[fall].max_z_velocity = -5.0;
        let on_ground = g.add_connection("connectionOnGround", detect(vec![], 1e5));
        g.connections[on_ground].on_ground = true;
        let move_on_ground = g.add_connection("connectionMoveOnGround", detect(vec![stage(&[], &[], None, (0.25, 2.0))], 0.05));
        g.connections[move_on_ground].on_ground = true;
        let delay = g.add_connection("connectionJumpSwitchDelay", Test::TimeInState(0.2));
        let exit_jump = g.add_connection("connectionListExitJump", Test::All(vec![delay, on_ground]));
        let exit_jump_move = g.add_connection("connectionListExitJumpMove", Test::All(vec![delay, move_on_ground]));
        let switch = g.add_connection("connectionEnvCanSwitchAvatar", Test::Environment(env::CAN_SWITCH_AVATAR));
        let vehicle = [button::VEHICLE_MODE];
        let weapon = [button::WEAPON_MODE];
        let drive = g.add_connection("connectionDrive", detect(vec![stage(&vehicle, &weapon, None, any)], 1e5));
        let drive_run = g.add_connection("connectionDriveRun", detect(vec![stage(&vehicle, &weapon, None, (0.11, 10.0))], 1e5));
        let drive_light = g.add_connection("connectionDriveLight", detect(vec![stage(&vehicle, &[], None, any)], 1e5));
        let release = g.add_connection("connectionReleaseDrive", detect(vec![stage(&[], &vehicle, None, (-1.0, 10.0))], 1e5));
        let release_run = g.add_connection("connectionReleaseDriveToRun", detect(vec![stage(&[], &vehicle, None, (0.11, 10.0))], 1e5));
        let jump_drive = g.add_connection("connectionJumpDrive", detect(vec![stage(&[button::TRANSFORM_JUMP], &vehicle, None, any)], 1e5));
        g.connections[jump_drive].on_ground = true;
        g.connections[jump_drive].can_adv_transform = true;
        let done = g.add_connection("connectionWaitForAnimDone", Test::None);
        (g.connections[done].branch_begin, g.connections[done].branch_end) = (100, 100);
        let second = g.add_connection("connectionWaitForSecondAnimBranch", Test::None);
        (g.connections[second].branch_begin, g.connections[second].branch_end) = (2, 2);
        let no_input = g.add_connection("connectionNoInput", detect(vec![stage(&[], &[], None, any)], 1e5));
        g.connections[no_input].branch_begin = 100;
        let weapon_mode = g.add_connection("connectionWeaponMode", detect(vec![stage(&weapon, &[], None, any)], 1e5));
        let release_weapon = g.add_connection("connectionReleaseWeapon", detect(vec![stage(&[], &weapon, None, any)], 1e5));
        let fire = [button::WEAPON_FIRE];
        let fire_held = g.add_connection("connectionWeaponModeAttack", detect(vec![stage(&fire, &[], None, any)], 1e5));
        let fire_let_go = g.add_connection("connectionReleaseWeaponModeAttack", detect(vec![stage(&[], &fire, None, any)], 1e5));
        let mut all = |name: &str, members: Vec<usize>| g.add_connection(name, Test::All(members));
        let list_drive = all("connectionListDrive", vec![drive, switch]);
        let list_drive_run = all("connectionListDriveRun", vec![drive_run, switch]);
        let list_release = all("connectionListDriveRelease", vec![release, switch]);
        let list_release_run = all("connectionListDriveReleaseRun", vec![release_run, switch]);
        let list_jump_drive = all("connectionListJumpDrive", vec![jump_drive, switch]);
        let light_done = all("connectionListDriveLightAndAnimDone", vec![drive_light, done]);
        let release_done = all("connectionListDriveReleaseAndAnimDone", vec![release, switch, done]);
        let release_run_done = all("connectionListDriveReleaseRunAndAnimDone", vec![release_run, switch, done]);
        let release_second = all("connectionListDriveReleaseAndSecondAnimBranch", vec![release, switch, second]);
        let release_run_second = all("connectionListDriveReleaseRunAndSecondAnimBranch", vec![release_run, switch, second]);
        let drive_done = all("connectionListDriveAndAnimDone", vec![drive, switch, done]);
        let drive_run_done = all("connectionListDriveRunAndAnimDone", vec![drive_run, switch, done]);
        let move_second = all("connectionListMoveAndSecondAnimBranch", vec![mv, second]);
        let idle_done = all("connectionListNoInputAndAnimDone", vec![no_input, done]);
        let fall_second = all("connectionListFallAndSecondAnimBranch", vec![fall, second]);
        let weapon_done = all("connectionListWeaponAndAnimDone", vec![weapon_mode, done]);

        let mut state = |name: &str, class: &str, anim: &str, motion: u32| {
            g.add_state(StateDef {
                name: name.into(),
                class: class.into(),
                anim: if anim.is_empty() { 0 } else { crc32(anim.as_bytes()) },
                motion_mode: motion,
                ..Default::default()
            })
        };
        let idle = state("stateIdle", "StateIdle", "", 0);
        let mov = state("stateMove", "StateMove", "", 0);
        let jmp = state("stateJump", "StateJump", "", 0);
        let jump_adv = state("stateJumpAdv", "StateJump", "", 0);
        let fal = state("stateFall", "StateFall", "", 0);
        let weapon_state = state("stateWeaponMode", "StateWeapon", "", 0x3c95_6e02);
        let weapon_jump = state("stateWeaponModeJump", "StateWeapon", "", JUMP_MODE);
        let weapon_fall = state("stateWeaponModeFall", "StateWeapon", "", JUMP_MODE);
        let car = state("stateDrive", "StateDrive", "", 0);
        let enter_a = state("stateDriveEnterA", "StateDrive", "Trans_R2V_A", 0);
        let enter_run_a = state("stateDriveEnterRunA", "StateDrive", "Trans_R2V_Run_A", 0);
        let enter_b = state("stateDriveEnterB", "StateDrive", "Trans_R2V_B", 0);
        let enter_run_b = state("stateDriveEnterRunB", "StateDrive", "Trans_R2V_Run_B", 0);
        let exit_a = state("stateDriveExitA", "StateBasic", "Trans_V2R_A", UNFOLD_MODE);
        let exit_run_a = state("stateDriveExitRunA", "StateBasic", "Trans_V2R_Run_A", UNFOLD_MODE);
        let exit_b = state("stateDriveExitB", "StateBasic", "Trans_V2R_B", UNFOLD_MODE);
        let exit_run_b = state("stateDriveExitRunB", "StateBasic", "Trans_V2R_Run_B", UNFOLD_MODE);
        let exit_jump_state = state("stateDriveExitJump", "StateBasic", "Trans_V2R_Jump", JUMP_MODE);

        let to = |connection: usize, output: usize, priority: i32| Transition { connection, output: Some(output), priority };
        let enter_drive = |a: usize, run_a: usize| vec![to(list_drive, a, 5), to(list_drive_run, run_a, 6)];
        g.states[idle].transitions = vec![to(mv, mov, 4), to(jump, jmp, 1), to(fall, fal, 0), to(weapon_mode, weapon_state, 2)];
        g.states[idle].transitions.extend(enter_drive(enter_a, enter_run_a));
        g.states[mov].transitions = vec![
            to(release_move, idle, 1),
            to(jump, jmp, 3),
            to(fall, fal, 2),
            to(weapon_mode, weapon_state, 0),
            to(list_drive, enter_a, 4),
            to(list_drive_run, enter_run_a, 5),
        ];
        for j in [jmp, jump_adv] {
            g.states[j].transitions = vec![
                to(exit_jump, idle, 1),
                to(exit_jump_move, mov, 2),
                to(list_drive, enter_a, 0),
                to(list_drive_run, enter_run_a, 1),
                to(weapon_mode, weapon_fall, 0),
            ];
        }
        g.states[fal].transitions = vec![
            to(move_on_ground, mov, 3),
            to(on_ground, idle, 2),
            to(list_drive, enter_a, 0),
            to(list_drive_run, enter_run_a, 1),
            to(weapon_mode, weapon_fall, 0),
        ];
        // The firing state, as the data has it: an overlay state on top of the weapon states.
        let firing = g.add_state(StateDef {
            name: "stateWeaponModeAttack".into(),
            class: "StateAttack".into(),
            overlay: true,
            ranged: true,
            fire_button: button::WEAPON_FIRE,
            ..Default::default()
        });
        g.states[firing].transitions = vec![Transition { connection: fire_let_go, output: None, priority: 0 }];
        g.states[weapon_state].transitions =
            vec![to(release_weapon, idle, 1), to(fire_held, firing, 3), to(fall, weapon_fall, 4), to(jump, weapon_jump, 5)];
        g.states[weapon_jump].transitions = vec![to(release_weapon, fal, 1), to(exit_jump, weapon_state, 2), to(fire_held, firing, 3)];
        g.states[weapon_fall].transitions = vec![to(release_weapon, fal, 1), to(on_ground, weapon_state, 2), to(fire_held, firing, 3)];
        g.states[car].transitions = vec![to(list_release, exit_a, 0), to(list_release_run, exit_run_a, 1), to(list_jump_drive, exit_jump_state, 2)];
        // The car's gun and the special, as the data has them: overlay states too
        // (machines DriveAttacksAdvance and GenericSpecial).
        let car_fire = [button::ATTACK_VEHICLE];
        let car_fire_held = g.add_connection("connectionWeaponModeVehicleAttack", detect(vec![stage(&car_fire, &[], None, any)], 1e5));
        let car_fire_let_go = g.add_connection("connectionReleaseVehicleWeapon", detect(vec![stage(&[], &car_fire, None, any)], 1e5));
        let special_pressed = g.add_connection("connectionAttackSpecial", detect(vec![stage(&[], &[], Some(button::ATTACK_SPECIAL), any)], 0.5));
        let overlay = |name: &str, class: &str, fire_button: u32| StateDef {
            name: name.into(),
            class: class.into(),
            overlay: true,
            ranged: class == "StateAttack",
            fire_button,
            robot_form: 5,
            ..Default::default()
        };
        let vehicle_fire = g.add_state(overlay("stateWeaponModeVehicleAttack", "StateAttack", button::ATTACK_VEHICLE));
        g.states[vehicle_fire].transitions = vec![Transition { connection: car_fire_let_go, output: None, priority: 0 }];
        g.states[car].transitions.push(to(car_fire_held, vehicle_fire, 5));
        let special = g.add_state(overlay("stateAttackSpecial", "StateAttackSpecial", 0));
        for under in [idle, mov, weapon_state, firing, car, vehicle_fire] {
            g.states[under].transitions.push(to(special_pressed, special, 10));
        }
        for (a, b) in [(enter_a, enter_b), (enter_run_a, enter_run_b)] {
            g.states[a].transitions = vec![
                to(done, b, 0),
                to(light_done, b, 1),
                to(release_done, exit_b, 2),
                to(release_run_done, exit_run_b, 3),
                to(list_jump_drive, exit_jump_state, 4),
            ];
        }
        for b in [enter_b, enter_run_b] {
            g.states[b].transitions = vec![
                to(done, car, 0),
                to(light_done, car, 1),
                to(release_second, exit_a, 2),
                to(release_run_second, exit_run_a, 3),
                to(list_jump_drive, exit_jump_state, 4),
            ];
        }
        g.states[exit_a].transitions = vec![to(drive_done, enter_b, 1), to(drive_run_done, enter_run_b, 2), to(done, exit_b, 0)];
        g.states[exit_run_a].transitions = vec![to(drive_done, enter_b, 1), to(drive_run_done, enter_run_b, 2), to(done, exit_run_b, 0)];
        for b in [exit_b, exit_run_b] {
            g.states[b].transitions = vec![
                to(move_second, mov, 2),
                to(drive_done, enter_a, 1),
                to(drive_run_done, enter_run_a, 1),
                to(idle_done, idle, 0),
                to(fall_second, fal, 3),
                to(weapon_done, weapon_state, 4),
            ];
        }
        g.states[exit_jump_state].transitions = vec![to(idle_done, jump_adv, 0), to(weapon_done, weapon_fall, 1)];
        t.control = g;

        // The clips: length in ticks, root travel forward, branch point. [data]
        let clips = [
            ("DriveSet", "Trans_R2V_A", 26.0, 3.5, None),
            ("DriveSet", "Trans_R2V_B", 72.0, 0.06, Some(40.0)),
            ("DriveSet", "Trans_R2V_Run_A", 34.0, 11.17, None),
            ("DriveSet", "Trans_R2V_Run_B", 76.0, 2.03, Some(32.0)),
            ("Vehicle2RobotSet", "Trans_V2R_A", 10.0, 0.0, None),
            ("Vehicle2RobotSet", "Trans_V2R_B", 50.0, 1.63, None),
            ("Vehicle2RobotSet", "Trans_V2R_Run_A", 16.0, 6.39, None),
            ("Vehicle2RobotSet", "Trans_V2R_Run_B", 44.0, 22.08, None),
            ("Vehicle2RobotSet", "Trans_V2R_Jump", 42.0, 0.0, None),
        ];
        for (set, id, ticks, travel, branch) in clips {
            let n = ticks as usize;
            t.actions.clips.insert(
                crc32(id.as_bytes()),
                ActionClip {
                    set: set.into(),
                    id: id.into(),
                    ticks,
                    speed: 1.0,
                    crossfade: 0.0,
                    looping: false,
                    path: (0..=n).map(|tick| Vec3::Y * (travel * tick as f32 / ticks)).collect(),
                    turn: Vec::new(),
                    events: branch.map(|time| vec![ActionEvent { time, action: A::BranchPoint }]).unwrap_or_default(),
                    attack: None,
                    bodies: Vec::new(),
                    blasts: Vec::new(),
                },
            );
        }
    }

    fn every_second_tick(crossfade: f32, samples: &[f32]) -> RootCurve {
        let mut forward = Vec::new();
        for pair in samples.windows(2) {
            forward.push(pair[0] - samples[0]);
            forward.push((pair[0] + pair[1]) / 2.0 - samples[0]);
        }
        forward.push(samples[samples.len() - 1] - samples[0]);
        RootCurve::new(crossfade, forward)
    }

    /// The game's own update length on the PC it was recorded on (`notes\live-trace.md`). Its
    /// updates ran from 31 to 33 ms, so the replays below agree to about 0.3, not exactly.
    const GAME_DT: f32 = 0.032;

    /// Steps at the game's rate and returns the ground speed after each update.
    fn speeds(s: &mut State, input: Input, t: &Tuning, updates: usize) -> Vec<f32> {
        let arena = Arena::default();
        (0..updates)
            .map(|_| {
                step(s, &input, t, &arena, GAME_DT);
                s.speed
            })
            .collect()
    }

    fn assert_close(got: &[f32], recorded: &[f32], within: f32) {
        assert_eq!(got.len(), recorded.len());
        for (i, (g, r)) in got.iter().zip(recorded).enumerate() {
            assert!((g - r).abs() <= within, "update {i}: {g:.2} here, {r:.2} in the game\n{got:.2?}\n{recorded:?}");
        }
    }

    const FORWARD: Input =
        Input {
            stick: Vec2::Y,
            steer: 0.0,
            pad_stick: Vec2::Y,
            jump: false,
            jump_held: false,
            vehicle: 0.0,
            brake: 0.0,
            turbo: false,
            drift: false,
            aim: None,
            buttons_down: 0,
            buttons_hit: 0,
        };

    #[test]
    fn leaving_the_car_on_the_ground_slows_to_a_run_or_a_stop_as_recorded() {
        let t = tuning();
        let gas = Input { vehicle: 1.0, ..Default::default() };
        // Stick held (run 1, t 433.8): 1.15 off an update down to the run's speed, held
        // there for the rest of the 1.03 s, then the run itself at 24.
        let mut s = State::new(&t);
        run(&mut s, gas, &t, 4.0);
        let before = s.speed;
        let got = speeds(&mut s, FORWARD, &t, 40);
        assert_eq!(s.mode, Mode::Move);
        // Run 1 (t 435.78, 442.14): 1.14 off every update from the first, until it is under
        // the run's 24; there it holds (23.7 in the game, from where it started), and the run
        // takes over when the two clips' 60 ticks are out, 32 updates on.
        for i in 1..10 {
            assert!((got[i - 1] - got[i] - 1.12).abs() < 0.03, "{before:.2} then {got:.2?}");
        }
        assert!((before - got[0] - 1.12).abs() < 0.03, "{before:.2} then {got:.2?}");
        assert!((22.8..24.0).contains(&got[20]) && got[30] == got[20], "{got:.2?}");
        assert!((got[33] - 24.0).abs() < 0.01 && !s.unfolding(), "{got:.2?}");
        assert!((got[39] - 24.0).abs() < 0.01 && s.pace() == Pace::Run, "{got:.2?}");
        // Stick centred: on down to nothing, and standing after.
        let mut s = State::new(&t);
        run(&mut s, gas, &t, 4.0);
        let got = speeds(&mut s, Input::default(), &t, 40);
        assert!(got[20] < got[10] - 10.0 && got[39] == 0.0 && s.pace() == Pace::Idle, "{got:.2?}");
    }

    /// Ground that climbs along y.
    struct Slope(f32);

    impl World for Slope {
        fn wheel_ray(&self,from:Vec3,to:Vec3)->Option<crate::vehicle::GroundHit> {
            let n=Vec3::new(0.0,-self.0,1.0).normalize();
            let fraction=-n.dot(from)/n.dot(to-from);
            (fraction>=0.0 && fraction<=1.0).then_some(crate::vehicle::GroundHit {
                point:from.lerp(to,fraction),normal:n,surface:melee::surface::DEFAULT,
            })
        }
        fn floor(&self, _: f32, y: f32, _: f32) -> f32 {
            (y * self.0).max(0.0)
        }

        fn push_out(&self, _: &mut Vec3, _: f32, _: f32) -> bool {
            false
        }
    }

    #[test]
    fn arena_ramps_support_fast_driving_launching_and_landing() {
        let t = tuning();
        let arena = Arena { ramps: vec![crate::terrain::Ramp {
            min: Vec2::new(-20.0, 0.0), max: Vec2::new(20.0, 50.0),
            base: 0.0, height: 0.0, slope: Vec2::new(0.0, 0.28),
        }], ..Default::default() };
        let gas = Input { vehicle: 1.0, ..Default::default() };
        let mut s = State::new(&t);
        s.pos.y = -20.0;
        let (mut pitched, mut launched, mut landed) = (false, false, false);
        for _ in 0..700 {
            step(&mut s, &gas, &t, &arena, GAME_UPDATE);
            if (10.0..45.0).contains(&s.pos.y) {
                // Suspension permits root movement; the old exact plane snap is gone.
                assert!((s.pos.z - 0.28 * s.pos.y).abs() < t.drive.suspension_length, "{:?}", s.pos);
                assert!(!s.car_airborne());
                pitched |= s.pitch > 0.15;
            }
            if s.car_airborne() && s.pos.y > 50.0 {
                launched |= s.pos.z >= 14.0-t.drive.suspension_length && s.vel.z > 0.0;
            }
            landed |= launched && !s.car_airborne() && s.pos.z.abs()<t.drive.suspension_length;
        }
        assert!(pitched && launched && landed, "pitch {pitched}, launch {launched}, land {landed}");
    }

    #[test]
    fn arena_hill_has_continuous_support_up_and_down_in_both_forms() {
        let t = tuning();
        let arena = Arena { ramps: vec![
            crate::terrain::Ramp { min: Vec2::new(-20.0, 0.0), max: Vec2::new(20.0, 40.0),
                base: 0.0, height: 0.0, slope: Vec2::new(0.0, 0.2) },
            crate::terrain::Ramp { min: Vec2::new(-20.0, 40.0), max: Vec2::new(20.0, 80.0),
                base: 0.0, height: 8.0, slope: Vec2::new(0.0, -0.2) },
        ], ..Default::default() };
        for vehicle in [0.0, 1.0] {
            let mut s = State::new(&t);
            s.pos.y = -10.0;
            let input = Input { vehicle, stick: Vec2::Y, ..Default::default() };
            let mut crossed_crest = false;
            for _ in 0..500 {
                step(&mut s, &input, &t, &arena, GAME_UPDATE);
                if (2.0..78.0).contains(&s.pos.y) {
                    let expected = if s.pos.y < 40.0 { s.pos.y * 0.2 } else { (80.0 - s.pos.y) * 0.2 };
                    if vehicle==0.0 {
                        assert!((s.pos.z-expected).abs()<1e-3 && s.touching);
                    } else {
                        // A sprung car may unload at the crest and carry upward velocity.
                        assert!(s.pos.z>expected-t.drive.suspension_length && s.pos.z<expected+5.0,
                            "vehicle {vehicle}: {:?}",s.pos);
                    }
                    crossed_crest |= s.pos.y > 40.0;
                }
            }
            assert!(crossed_crest && s.pos.y > 80.0 && s.pos.z.abs()<0.2);
        }
    }

    #[test]
    fn ramp_sides_block_entry_but_the_low_edge_is_walkable() {
        let arena = Arena { ramps: vec![crate::terrain::Ramp {
            min: Vec2::ZERO, max: Vec2::new(20.0, 40.0),
            base: 0.0, height: 0.0, slope: Vec2::new(0.0, 0.25),
        }], ..Default::default() };
        let mut high = Vec3::new(10.0, 40.5, 0.0);
        assert!(arena.push_out(&mut high, 1.0, 3.0));
        assert!((high.y - 41.0).abs() < 1e-5);
        let mut low = Vec3::new(10.0, -0.5, 0.0);
        assert!(!arena.push_out(&mut low, 1.0, 3.0));
        assert!(arena.follow_floor(high, Vec2::new(10.0, 35.0)).is_none());
        assert!(arena.follow_floor(Vec3::new(10.0, -1.0, 0.0), Vec2::new(10.0, 5.0)).is_some());
        let landed = arena.drop_onto(Vec3::new(10.0, 20.0, 8.0), 2.0, 1.0).unwrap();
        assert_eq!(landed.z, 5.0);
    }

    #[test]
    fn banked_arena_surface_rolls_the_car() {
        let t = tuning();
        let arena = Arena { ramps: vec![crate::terrain::Ramp {
            min: Vec2::new(-20.0, -50.0), max: Vec2::new(20.0, 500.0),
            base: 0.0, height: 0.0, slope: Vec2::new(0.15, 0.0),
        }], ..Default::default() };
        let mut s = State::new(&t);
        s.pos.z = 3.0;
        for _ in 0..120 {
            step(&mut s, &Input { vehicle: 1.0, ..Default::default() }, &t, &arena, DT);
        }
        assert!((s.roll + 0.15_f32.atan()).abs() < 0.02, "roll {}", s.roll);
        assert!(s.pitch.abs() < 0.01 && !s.car_airborne());
    }

    #[test]
    fn car_presentation_moves_on_every_render_frame_at_steady_speed() {
        let t=tuning();let arena=Arena::default();
        // Include irregular frames and a hitch spanning multiple physics updates.
        // A direct display of State.pos pauses, then jumps, in every sequence here.
        for frames in [vec![1.0/60.0],vec![1.0/120.0],vec![1.0/144.0],
            vec![0.007,0.021,0.012,0.045,0.009]] {
            let mut s=State::new(&t);
            let input=Input {vehicle:1.0,..Default::default()};
            for _ in 0..200 {step(&mut s,&input,&t,&arena,GAME_UPDATE);}
            let mut previous=s.car_presentation();
            let mut raw_previous=s.pos;
            let mut raw_holds=0;
            for frame in 0..300 {
                let dt=frames[frame%frames.len()];
                step(&mut s,&input,&t,&arena,dt);
                let pose=s.car_presentation();
                let expected=s.vel.y*dt;
                let moved=pose.pos.y-previous.pos.y;
                assert!((moved-expected).abs()<0.002,
                    "dt {dt}, display {moved}, expected {expected}");
                assert!((pose.wheels[0].render_spin-previous.wheels[0].render_spin).abs()>0.01,
                    "tyre paused while body moved, dt {dt}");
                assert!((s.pos-pose.pos).length()<=s.vel.length()*GAME_UPDATE+0.01,
                    "presentation must stay within one completed physics interval");
                raw_holds+=usize::from(s.pos==raw_previous);
                raw_previous=s.pos;previous=pose;
            }
            assert!(raw_holds>50,"test must reproduce the old held-frame bug");
        }
    }

    #[test]
    fn car_presentation_keeps_turns_continuous_and_discards_old_drive_history() {
        let t=tuning();let arena=Arena::default();let mut s=State::new(&t);
        let mut pose=s.car_presentation();
        let mut crossed_heading_wrap=false;
        let mut last_yaw=s.yaw;
        for i in 0..2400 {
            let input=Input {vehicle:1.0,steer:if i<400 {0.0} else if i<1500 {1.0} else {-1.0},
                ..Default::default()};
            step(&mut s,&input,&t,&arena,1.0/120.0);
            let next=s.car_presentation();
            if i>400 {
                assert!(pose.rotation.angle_between(next.rotation)<0.05,"display heading jumped");
                assert!((next.pos-pose.pos).length()<0.4,"display root jumped");
                crossed_heading_wrap|=(s.yaw-last_yaw).abs()>6.0;
            }
            last_yaw=s.yaw;pose=next;
        }
        assert!(crossed_heading_wrap,"must exercise the quaternion heading seam");
        set_mode(&mut s,&Input::default(),&t,Mode::Stand);
        assert_eq!(s.car_presentation().pos,s.pos);
        s.pos=Vec3::new(1000.0,2000.0,0.0);
        set_mode(&mut s,&Input::default(),&t,Mode::Drive);
        assert!(s.car.previous_presentation.is_none());
        assert_eq!(s.car_presentation().pos,s.pos,"re-entry must not reuse the old car position");
        let reset=State::new(&t);
        assert_eq!(reset.car_presentation().pos,Vec3::ZERO);
    }

    #[test]
    fn sharp_turn_suspension_stays_within_render_travel_in_both_directions() {
        let t=tuning();let arena=Arena::default();
        // Installed model's tyre centres [data], rounded to millimetres. These differ
        // from the physics mounts; testing only mounts misses the visible penetration.
        let centres=[Vec3::new(-0.980,2.200,0.444),Vec3::new(0.964,2.200,0.444),
            Vec3::new(-1.010,-1.156,0.444),Vec3::new(0.994,-1.155,0.445)];
        for dt in [GAME_UPDATE,DT,1.0/120.0] {
            for direction in [-1.0,1.0] {
                let mut s=State::new(&t);
                for i in 0..(12.0/dt) as usize {
                    let time=i as f32*dt;
                    let steer=if time<3.0 {0.0} else if time<8.0 {direction} else {-direction};
                    step(&mut s,&Input {vehicle:1.0,steer,..Default::default()},&t,&arena,dt);
                    if s.mode!=Mode::Drive || time<2.0 {continue;}
                    assert!(s.roll.abs()<0.2,"dt {dt}, t {time}, roll {}",s.roll);
                    // Check both the authoritative physics and the interpolated model.
                    for pose in [s.car_snapshot(),s.car_presentation()] {
                        let turn=pose.rotation;let up=turn*Vec3::Z;
                        for (index,(w,centre)) in pose.wheels.iter().zip(centres).enumerate() {
                            let point=pose.pos+turn*centre;
                            let offset=crate::vehicle::Chassis::render_offset(w,point.z,up.z,&t.drive);
                            let wheel_turn=turn*Quat::from_rotation_z(if index<2 {-3.0*pose.steer_angle} else {0.0});
                            let bottom=point.z-up.z*offset-t.drive.render_wheel_radius*
                                (1.0-(wheel_turn*Vec3::X).z.powi(2)).sqrt();
                            // Installed .4 radius and +/- .1 travel. The original capture's
                            // flat-contact nominal clearance p05 is -.0417m; shallow overlap
                            // exists in the original. Reject the old .24m penetration. [trace]
                            assert!(bottom> -0.045,"dt {dt}, t {time}, tyre {index} bottom {bottom}, roll {}",s.roll);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_car_leans_to_a_slope_and_flies_off_a_ledge() {
        let t = tuning();
        let gas = Input { vehicle: 1.0, ..Default::default() };
        let mut s = State::new(&t);
        s.pos.y = -20.0;
        for _ in 0..180 {
            step(&mut s, &gas, &t, &Slope(0.2), DT);
        }
        // Nose up by the slope's own angle, and no faster than the body may tip.
        assert!(s.pos.z > 5.0 && (s.pitch - 0.2f32.atan()).abs() < 0.02 && s.roll.abs() < 0.01, "{} {}", s.pitch, s.roll);
        // Off the edge of a box: in the air, then down on the ground again and level.
        let arena = Arena { boxes: vec![Box3 { min: Vec3::new(-50.0, -50.0, 0.0), max: Vec3::new(50.0, 60.0, 8.0) }], ..Default::default() };
        let mut s = State::new(&t);
        s.pos.z = 8.0;
        let mut flew = 0;
        for _ in 0..400 {
            step(&mut s, &gas, &t, &arena, DT);
            flew += s.car_airborne() as usize;
        }
        // Independent wheels lose/regain support before the old centre-only touchdown.
        assert!(flew>0, "never left wheel contact");
        assert!(s.mode == Mode::Drive && s.pos.z.abs()<0.2 && s.pos.y > 60.0 && s.speed > 30.0, "{:?}", s.pos);
    }

    #[test]
    fn the_car_tells_its_tyres_what_to_sound_like() {
        use crate::sound::tyre_sounds;
        let t = tuning();
        let gas = Input { vehicle: 1.0, ..Default::default() };
        // Off a ledge 8 high: rolling on the way, silent in the air, one thump coming down.
        let arena = Arena { boxes: vec![Box3 { min: Vec3::new(-50.0, -50.0, 0.0), max: Vec3::new(50.0, 60.0, 8.0) }], ..Default::default() };
        let mut s = State::new(&t);
        s.pos.z = 8.0;
        let (mut rolled, mut silent, mut thumps) = (0, 0, Vec::new());
        for _ in 0..400 {
            step(&mut s, &gas, &t, &arena, DT);
            for heard in s.tyre_inputs.iter().map(tyre_sounds) {
                assert_eq!(heard.skid, None, "nothing here makes them skid");
                rolled += heard.road.is_some() as usize;
                silent += heard.road.is_none() as usize;
                thumps.extend(heard.suspension);
            }
        }
        assert!(rolled > 100 && silent > 15, "{rolled} rolling, {silent} silent");
        // Thumps now use actual rear shortening; unloading/reloading can cause more
        // than the old invented single touchdown thump. Every cue obeys the code gate.
        assert!(!thumps.is_empty() && thumps.iter().all(|force|*force>0.5),"{thumps:?}");
        // The slide and the brake make them skid; plain driving does not.
        // (A step shorter than the game's update may hold no update: the last one that did.)
        let mut heard = |input: Input, seconds: f32| {
            let mut last = None;
            for _ in 0..(seconds / DT).round() as usize {
                step(&mut s, &input, &t, &arena, DT);
                last = s.tyre_inputs.last().map(tyre_sounds).or(last);
            }
            last.unwrap()
        };
        assert!(heard(Input { drift: true, ..gas }, 0.3).skid.is_some());
        let rolling = heard(gas, 1.0);
        assert!(rolling.skid.is_none() && rolling.road.is_some());
        assert!(heard(Input { brake: 1.0, ..gas }, 0.2).skid.is_some());
    }

    #[test]
    fn the_slide_slows_him_swings_the_tail_but_forward_speed_alone_earns_no_boost() {
        let t = tuning();
        let gas = Input { vehicle: 1.0, ..Default::default() };
        let turn = Input { steer: 1.0, ..gas };
        let mut gripping = State::new(&t);
        run(&mut gripping, gas, &t, 4.0);
        let mut sliding = gripping.clone();
        run(&mut gripping, turn, &t, 0.3);
        run(&mut sliding, Input { drift: true, ..turn }, &t, 0.3);
        assert!(sliding.sliding());
        // Slowed along his own motion at 40 a second, less what the spun-up wheels still
        // give through the little forward grip they keep (0.4 of about 81).
        let lost = Vec2::new(gripping.vel.x, gripping.vel.y).length() - Vec2::new(sliding.vel.x, sliding.vel.y).length();
        assert!((2.0..6.0).contains(&lost), "lost {lost}");
        // All four tyres lose their sideways grip, so he carries on much the way he was
        // going: the path bends far less than with grip.
        let bend = |s: &State| Vec2::new(s.vel.x, s.vel.y).normalize().x.abs();
        assert!(bend(&sliding) < bend(&gripping) * 0.5, "{} against {}", bend(&sliding), bend(&gripping));
        // [game: 00733e10] This short turn never produces enough sideways velocity to
        // charge. The old guessed rule wrongly awarded a boost for forward speed.
        assert!(!sliding.car.drift_turbo.active());
        run(&mut sliding, gas, &t, 1.5);
        assert!(sliding.speed < 39.0, "{}", sliding.speed);
    }

    #[test]
    fn leaving_the_car_keeps_standard_turbo_and_its_cooldown_ticking() {
        let t = tuning();
        let gas = Input { vehicle: 1.0, ..Default::default() };
        let mut s = State::new(&t);
        run(&mut s, gas, &t, 2.0);
        run(&mut s, Input { turbo: true, ..gas }, &t, 0.2);
        assert!(s.turbo_left > 2.0);
        assert!(s.turbo_fx_on);
        let left = s.turbo_left;
        step(&mut s, &Input::default(), &t, &Arena::default(), GAME_DT);
        assert_eq!(s.mode, Mode::Unfold);
        assert!(!s.turbo_fx_on);
        assert!((s.turbo_left - (left - GAME_DT)).abs() < 1e-5);
        run(&mut s, Input::default(), &t, 2.5);
        assert_eq!(s.turbo_left, 0.0);
        assert!(s.turbo_cooldown > 2.5, "{}", s.turbo_cooldown);
    }

    #[test]
    fn the_drive_update_uses_lateral_charge_and_reports_drift_turbo_start() {
        let t = tuning();
        let mut s = State::new(&t);
        s.mode = Mode::Drive;
        let gas = Input { vehicle: 1.0, drift: true, ..Default::default() };
        // Hold a qualifying sideways velocity, like the external physics body sampled
        // by the original ability. A plain forward slide is checked above separately.
        for _ in 0..7 {
            s.vel = Vec3::new(25.0, 0.0, 0.0);
            drive_update(&mut s, &gas, &t, GAME_DT);
        }
        assert!(!s.turbo_started);
        drive_update(&mut s, &Input { drift: false, ..gas }, &t, GAME_DT);
        assert!(s.car.drift_turbo.active());
        assert!(s.turbo_started && s.turbo_fx_on);
        assert!(crate::fxstate::kept(&s, &t, false).contains(&crate::fxstate::TURBO));
        assert!(s.gearbox_inputs.last().unwrap().turbo);
    }

    #[test]
    fn drive_filter_feeds_the_gearbox_but_does_not_reduce_the_tyre_sound_cap() {
        let mut t = tuning();
        t.drive.trigger_filter_strength = 10.0;
        let mut s = State::new(&t);
        s.mode = Mode::Drive;
        drive_update(&mut s, &Input { vehicle: 0.5, ..Default::default() }, &t, GAME_DT);
        assert!((s.gearbox_inputs.last().unwrap().trigger - 0.16).abs() < 1e-5);
        assert_eq!(s.tyre_inputs.last().unwrap().cap, t.drive.max_speed);
    }

    #[test]
    fn drive_entry_starts_with_full_trigger_then_filters_partial_throttle() {
        let t = tuning();
        let mut s = State::new(&t);
        let gas = Input { vehicle: 1.0, ..Default::default() };
        run(&mut s, gas, &t, 2.0);
        assert_eq!(s.car.filtered_trigger, 1.0);
        drive_update(&mut s, &Input { vehicle: 0.5, ..gas }, &t, GAME_DT);
        assert!((s.car.filtered_trigger - 0.984).abs() < 1e-6);
    }

    #[test]
    fn letting_the_trigger_go_with_jump_held_leaps_out_of_the_car() {
        let t = tuning();
        let arena = Arena::default();
        let mut s = State::new(&t);
        let gas = Input { vehicle: 1.0, ..Default::default() };
        run(&mut s, gas, &t, 3.0);
        step(&mut s, &Input { turbo: true, jump_held: true, ..gas }, &t, &arena, DT);
        run(&mut s, Input { jump_held: true, ..gas }, &t, 1.5);
        let before = Vec2::new(s.vel.x, s.vel.y).length();
        step(&mut s, &Input { jump_held: true, ..Default::default() }, &t, &arena, DT);
        assert_eq!(s.mode, Mode::Jump(JumpType::Long));
        // t 651.96 in run 1: 46.6 along the ground and 30.2 up one update after leaving.
        assert!((Vec2::new(s.vel.x, s.vel.y).length() - before).abs() < 0.5 && before > 44.0, "{before}");
        assert!((s.vel.z - 31.3).abs() < 0.7, "{}", s.vel.z);
    }

    fn half_stick() -> Input {
        Input { stick: Vec2::Y * 0.5, ..Default::default() }
    }

    // The four tests below replay sequences from the recorded game, one value per update.

    #[test]
    fn run_start_matches_the_recording() {
        let t = tuning();
        // The stick crosses the walk zone for one update (t 120.36 in run 1).
        let mut s = State::new(&t);
        let mut got = speeds(&mut s, half_stick(), &t, 1);
        got.extend(speeds(&mut s, FORWARD, &t, 9));
        assert_close(&got, &[0.72, 4.32, 7.73, 10.96, 14.03, 16.93, 19.65, 22.22, 24.0, 24.0], 0.3);
        // Straight to full stick (t 132.72).
        let mut s = State::new(&t);
        assert_close(&speeds(&mut s, FORWARD, &t, 4), &[3.1, 6.2, 9.3, 12.5], 0.3);
    }

    #[test]
    fn stop_from_a_run_matches_the_recording() {
        let t = tuning();
        let mut s = State::new(&t);
        run(&mut s, FORWARD, &t, 1.0);
        // Stick let go at once (t 173.8): the run-to-idle clip's own root motion.
        let got = speeds(&mut s, Input::default(), &t, 12);
        assert_close(&got, &[21.1, 20.1, 19.8, 18.5, 14.7, 10.2, 6.2, 3.3, 1.3, 0.1, 0.0, 0.0], 0.6);
        assert_eq!(s.pace(), Pace::Idle);
        // Stick passing through the walk zone for two updates first (t 130.48): no
        // run-to-idle, the idle clip fades in over the run fading to a walk.
        let mut s = State::new(&t);
        run(&mut s, FORWARD, &t, 1.0);
        let mut got = speeds(&mut s, half_stick(), &t, 2);
        got.extend(speeds(&mut s, Input::default(), &t, 7));
        assert_close(&got, &[21.62, 19.22, 14.08, 9.72, 6.12, 3.32, 1.31, 0.13, 0.0], 0.3);
    }

    #[test]
    fn stop_from_a_walk_matches_the_recording() {
        let t = tuning();
        let mut s = State::new(&t);
        // t 171.06: four updates into a walk, then the stick is let go.
        let mut got = speeds(&mut s, half_stick(), &t, 4);
        got.extend(speeds(&mut s, Input::default(), &t, 7));
        assert_close(&got, &[0.7, 1.4, 2.1, 2.8, 2.9, 2.8, 2.5, 1.9, 1.0, 0.1, 0.0], 0.3);
    }

    #[test]
    fn landing_into_a_run_matches_the_recording() {
        let t = tuning();
        let arena = Arena::default();
        let mut s = State::new(&t);
        run(&mut s, FORWARD, &t, 1.0);
        step(&mut s, &Input { jump: true, ..FORWARD }, &t, &arena, GAME_DT);
        while s.mode != Mode::Move {
            step(&mut s, &FORWARD, &t, &arena, GAME_DT);
        }
        // t 125.6 and a dozen like it: the landing clip's root motion from the update the
        // jump mode ends (two after touching down), then the run at once.
        let mut got = vec![s.speed];
        got.extend(speeds(&mut s, FORWARD, &t, 11));
        assert_eq!(s.pace(), Pace::Run);
        assert_close(&got[..6], &[0.3, 9.5, 12.9, 12.6, 12.4, 12.4], 0.6);
        assert_eq!(got[11], 24.0);
    }

    #[test]
    fn running_jump_gains_speed_in_the_air_as_recorded() {
        let t = tuning();
        let arena = Arena::default();
        let mut s = State::new(&t);
        run(&mut s, FORWARD, &t, 1.0);
        step(&mut s, &Input { jump: true, ..FORWARD }, &t, &arena, GAME_DT);
        // t 125.77: off the ground at exactly the run speed (24.00, seen mid-update), and the
        // jump's air control already in by the end of that update; faster every update after.
        let mut got = vec![Vec2::new(s.vel.x, s.vel.y).length()];
        let mut updates = 1;
        while s.mode != Mode::Move {
            step(&mut s, &FORWARD, &t, &arena, GAME_DT);
            got.push(Vec2::new(s.vel.x, s.vel.y).length());
            updates += 1;
        }
        assert_close(&got[..9], &[24.52, 25.19, 25.88, 26.50, 26.95, 27.26, 27.54, 27.81, 28.08], 0.3);
        // In the game the jump mode ran for 48 updates (t 125.774 to 127.328), the last two
        // on the ground, and came down at 32.9. Full stick here gives 33.7; the recorded gains
        // late in the jump fit a stick held a little short of full, which is the player's
        // hand and not a rule.
        assert!((47..=50).contains(&updates), "{updates} updates in the jump mode");
        assert!((got[got.len() - 3] - 32.9).abs() < 1.0, "{:.2?}", &got[got.len() - 5..]);
    }

    #[test]
    fn standing_jump_creeps_forward_and_then_slows_as_recorded() {
        let t = tuning();
        let arena = Arena::default();
        let mut s = State::new(&t);
        step(&mut s, &Input { jump: true, ..Default::default() }, &t, &arena, GAME_DT);
        // t 370.6: from rest the take-off clip carries him forward to about 2.4, then the
        // speed falls by about a tenth each update.
        let got = speeds_in_air(&mut s, &t, 16);
        let peak = got.iter().copied().fold(0.0, f32::max);
        assert!((peak - 2.45).abs() < 0.5, "peak {peak:.2}\n{got:.2?}");
        assert!((got[15] - 1.2).abs() < 0.4, "{got:.2?}");
    }

    fn speeds_in_air(s: &mut State, t: &Tuning, updates: usize) -> Vec<f32> {
        let arena = Arena::default();
        (0..updates)
            .map(|_| {
                step(s, &Input::default(), t, &arena, GAME_DT);
                Vec2::new(s.vel.x, s.vel.y).length()
            })
            .collect()
    }

    fn run(s: &mut State, input: Input, t: &Tuning, seconds: f32) {
        let arena = Arena::default();
        for _ in 0..(seconds / DT).round() as usize {
            step(s, &input, t, &arena, DT);
        }
    }

    /// A `Death` clip like his: 100 ticks, the root going 10.8 back.
    fn with_death_clip(t: &mut Tuning) {
        let path = (0..=100).map(|tick| Vec3::new(0.0, -10.8 * tick as f32 / 100.0, 0.0)).collect();
        let clip = ActionClip { set: "DeathSet".into(), id: "Death".into(), ticks: 100.0, speed: 1.0, path, ..Default::default() };
        t.actions.clips.insert(crc32(b"Death"), clip);
    }

    fn installed_animation_tuning() -> Tuning {
        let dir = std::path::Path::new("C:/Games2");
        let mut t = crate::tuning::load(dir, "Bumblebee").unwrap();
        let data = crate::character::load(dir, "bumblebee").unwrap();
        t.actions = crate::tuning::Actions::from_animations(&data.animations, &data.robot, &mut t.missing);
        t.locomotion = crate::tuning::Locomotion::from_animations(&data.animations, &mut t.missing);
        t.weapon_set = crate::tuning::RuleSet::from_animations(&data.animations, "WeaponSet", &mut t.missing);
        t.rule_sets = crate::tuning::RuleSet::all(&data.animations);
        t
    }

    #[test]
    fn installed_partial_hit_plays_above_movement_and_does_not_interrupt_fire() {
        let t = installed_animation_tuning();
        let mut s = State::new(&t);
        let input = Input { stick: Vec2::Y, aim: Some(Vec2::Y), buttons_down: 1 << button::WEAPON_FIRE, ..Input::default() };
        run(&mut s, input, &t, 0.6);
        assert!(s.firing);
        s.receive_hit(15.0, 0, Vec3::ZERO);
        run(&mut s, input, &t, 0.1);
        assert_eq!(s.hit_partial.clip, crc32(b"HitFront"));
        assert!(s.firing);
        assert_eq!(s.mode, Mode::Strafe);
        run(&mut s, input, &t, 0.6);
        assert!(s.hit_partial.playing().is_none());
        assert!(s.firing);
    }

    #[test]
    fn installed_flybacks_recover_and_release_the_control_state() {
        let t = installed_animation_tuning();
        for (flag, fly, recover) in [(melee::damage::BY_FAST_FLAIL, "Flyback_Fast", "Recover_Fast"),
            (melee::damage::BY_STRONG_FLAIL, "Flyback_Slow", "Recover_Slow")] {
            let mut s = State::new(&t);
            run(&mut s, Input::default(), &t, 0.1);
            s.receive_hit(30.0, flag, Vec3::new(0.0, -18.0, 12.0));
            run(&mut s, Input::default(), &t, 0.15);
            assert_eq!(s.base.clip, crc32(fly.as_bytes()));
            assert_eq!(s.control_state(&t), Some("stateHitReact"));
            let mut recovered = false;
            for _ in 0..300 {
                step(&mut s, &Input::default(), &t, &Arena::default(), DT);
                recovered |= s.base.clip == crc32(recover.as_bytes());
            }
            assert!(recovered, "{fly} never reached {recover}");
            assert_eq!(s.control_state(&t), Some("stateIdle"));
            assert!(!s.hit_full && s.health > 0.0);
        }
    }

    #[test]
    fn installed_air_death_and_vehicle_death_follow_their_exit_rules() {
        let t = installed_animation_tuning();
        let mut s = State::new(&t);
        s.pos.z = 10.0;
        s.touching = false;
        s.damage(f32::MAX);
        run(&mut s, Input::default(), &t, 0.1);
        assert_eq!(s.death.as_ref().unwrap().clip, crc32(b"Death_Explode_Fall"));
        run(&mut s, Input::default(), &t, 2.0);
        assert_eq!(s.death.as_ref().unwrap().clip, crc32(b"Death_Explode_HitGround"));
        assert_eq!(s.control_state(&t), Some("stateDeath"));
        let mut s = State::new(&t);
        s.car_mode = true;
        s.damage(f32::MAX);
        step(&mut s, &Input::default(), &t, &Arena::default(), DT);
        assert_eq!(s.death.as_ref().unwrap().clip, crc32(b"Death_V2R"));
        run(&mut s, Input::default(), &t, 3.0);
        assert_eq!(s.death.as_ref().unwrap().clip, crc32(b"Death_Explode_HitGround"));
    }

    #[test]
    fn installed_weapon_landings_hand_their_overrun_to_running_clips() {
        let t=installed_animation_tuning();
        let land=t.weapon_set.clips.iter().find(|c|c.continue_time).unwrap();
        let mut s=State::new(&t);
        s.strafe.strafing=true;
        strafe_start(&mut s,&t,land.id);
        weapon_layers(&mut s,&t,land.ticks/(60.0*land.speed)+0.01);
        let previous=*s.strafe.layers.last().unwrap();
        assert!(previous.overrun>0.0);
        weapon_rules(&mut s,&Input { stick:Vec2::Y,aim:Some(Vec2::Y),..Input::default() },&t);
        assert_ne!(s.strafe.clip,land.id);
        let next=t.weapon_set.clip(s.strafe.clip).unwrap();
        let elapsed=if next.keeps_time { previous.elapsed_ticks } else { previous.overrun*next.speed };
        assert!(s.strafe.tick>0.0);
        assert!((s.strafe.tick - if next.looping { elapsed%next.ticks } else { elapsed.min(next.ticks) }).abs()<1e-4);
    }

    #[test]
    fn installed_hit_reaction_fall_permission_obeys_the_drop_and_clip_gate() {
        let t = installed_animation_tuning();
        for (flag, early_fall) in [(melee::damage::BY_FAST_FLAIL, true), (melee::damage::BY_KNOCKBACK, false)] {
            let mut s = State::new(&t);
            run(&mut s, Input::default(), &t, 0.1);
            s.pos.z = 30.0;
            s.touching = false;
            s.touching_for = 1.0;
            s.receive_hit(10.0, flag, Vec3::new(0.0,-18.0,0.0));
            run(&mut s, Input::default(), &t, 0.12);
            assert_eq!(s.control_state(&t), Some("stateHitReact"));
            // Drop more than 2.5m while the reaction clip still plays.
            s.pos.z = 26.0;
            step(&mut s, &Input::default(), &t, &Arena::default(), DT);
            assert_eq!(s.control_state(&t), Some(if early_fall { "stateFall" } else { "stateHitReact" }));
        }
    }

    #[test]
    fn health_gone_he_falls_back_along_the_death_clip_and_stays_down() {
        let mut t = tuning();
        with_death_clip(&mut t);
        let mut s = State::new(&t);
        let start = s.pos;
        s.damage(t.base_health);
        run(&mut s, Input { stick: Vec2::Y, jump: true, ..Input::default() }, &t, 0.5);
        assert!(s.dead());
        assert_eq!(s.action_clip(&t).map(|(set, id, _)| (set, id)), Some(("DeathSet", "Death")));
        run(&mut s, Input { stick: Vec2::Y, ..Input::default() }, &t, 3.0);
        // He went 10.8 backwards (he faces +y) whatever the stick said, and nothing healed him.
        assert!((s.pos.y - start.y + 10.8).abs() < 0.3, "{:?}", s.pos);
        assert_eq!((s.health, s.vel.length()), (0.0, 0.0));
        s.damage(50.0);
        assert!(s.dead() && s.health == 0.0);
    }

    #[test]
    fn a_hit_that_leaves_health_does_not_kill() {
        let t = tuning();
        let mut s = State::new(&t);
        s.damage(t.base_health - 1.0);
        run(&mut s, Input::default(), &t, 0.2);
        assert!(!s.dead() && s.health > 0.0);
    }

    fn jump_apex(s: &mut State, held: Input, t: &Tuning) -> f32 {
        let arena = Arena::default();
        step(s, &Input { jump: true, ..held }, t, &arena, DT);
        for _ in 0..600 {
            step(s, &held, t, &arena, DT);
            if !matches!(s.mode, Mode::Jump(_)) {
                break;
            }
        }
        s.apex_z - s.launch_z
    }

    #[test]
    fn standing_jump_peaks_a_fifth_above_the_run_jump_height() {
        let t = tuning();
        let mut s = State::new(&t);
        let apex = jump_apex(&mut s, Input::default(), &t);
        assert!((apex - 12.0).abs() < 0.2, "apex {apex}");
    }

    #[test]
    fn running_jump_uses_the_run_jump_and_lands_at_walk_speed() {
        let t = tuning();
        let mut s = State::new(&t);
        let forward = Input { stick: Vec2::Y, ..Default::default() };
        run(&mut s, forward, &t, 1.0);
        assert_eq!(s.gait, Gait::Run);
        assert!((s.speed - t.locomotion.run_speed).abs() < 0.01);
        // Half stick is under the run rule's threshold: the walk clip's pace.
        run(&mut s, Input { stick: Vec2::Y * 0.5, ..Default::default() }, &t, 1.0);
        assert_eq!(s.gait, Gait::Walk);
        assert!((s.speed - t.locomotion.walk_speed).abs() < 0.01);
        run(&mut s, forward, &t, 1.0);
        let arena = Arena::default();
        step(&mut s, &Input { jump: true, ..forward }, &t, &arena, DT);
        assert_eq!(s.mode, Mode::Jump(JumpType::Moving));
        // Off at MoveMode's top speed; the jump's first update of air control is already in.
        assert!((Vec2::new(s.vel.x, s.vel.y).length() - t.moving.max_speed).abs() < 0.5);
        for _ in 0..600 {
            step(&mut s, &Input::default(), &t, &arena, DT);
            if !matches!(s.mode, Mode::Jump(_)) {
                break;
            }
        }
        assert_eq!(s.mode, Mode::Stand);
        assert!((s.apex_z - t.jump.run.height).abs() < 0.3, "apex {}", s.apex_z);
        assert!(s.speed <= t.moving.walk_run_speed + 1e-3);
    }

    #[test]
    fn robot_turns_at_the_move_turn_speed() {
        let t = tuning();
        let mut s = State::new(&t);
        // Stick hard left asks for a quarter turn; a fifth of a second at 360 degrees a second gets 72.
        run(&mut s, Input { stick: Vec2::NEG_X, ..Default::default() }, &t, 0.2);
        assert!((s.yaw.to_degrees() - 72.0).abs() < 0.5, "yaw {}", s.yaw.to_degrees());
    }

    #[test]
    fn car_tops_out_and_turbo_runs_its_time() {
        let t = tuning();
        let mut s = State::new(&t);
        let gas = Input { vehicle: 1.0, ..Default::default() };
        run(&mut s, gas, &t, 6.0);
        assert_eq!(s.mode, Mode::Drive);
        // The engine pushes for one update past the cap of 35: the game settles at 37.6.
        assert!((s.speed - 37.6).abs() < 0.3, "speed {}", s.speed);
        let arena = Arena::default();
        step(&mut s, &Input { turbo: true, ..gas }, &t, &arena, DT);
        run(&mut s, gas, &t, 2.0);
        // Cap 43 in turbo; the game holds 48.1.
        assert!((s.speed - 48.1).abs() < 0.4, "speed {}", s.speed);
        run(&mut s, gas, &t, 1.5);
        // 1.1 s after the turbo ran out the game was at 39.1 and still easing down.
        assert!((s.speed - 39.1).abs() < 0.8, "speed {}", s.speed);
        assert!(s.turbo_cooldown > 0.0);
    }

    /// The car's preset rumbles: the turbo as it starts, and a wall met at over 5.
    #[test]
    fn the_car_rumbles_the_pad_for_its_turbo_and_its_knocks() {
        use crate::rumble::{Preset, preset};
        let mut t = tuning();
        t.rumble_presets = (0..15).map(|n| Preset { kind: 1, duration: 0.25, strength: n as f32 }).collect();
        let started = |s: &State, n: usize| s.rumbles.iter().filter(|r| r.strength == n as f32).count();
        let mut s = State::new(&t);
        let gas = Input { vehicle: 1.0, ..Default::default() };
        let arena = Arena::default();
        let mut early = 0;
        for _ in 0..180 {
            step(&mut s, &gas, &t, &arena, DT);
            early += s.rumbles.len();
        }
        assert_eq!((s.mode, early), (Mode::Drive, 0));
        // The turbo: once, on the update it starts, and not again while it runs.
        let mut turbos = 0;
        for _ in 0..30 {
            step(&mut s, &Input { turbo: true, ..gas }, &t, &arena, DT);
            turbos += started(&s, preset::VEHICLE_TURBO);
        }
        assert_eq!(turbos, 1);
        // Into a wall at speed: the wall preset.
        let ahead = s.pos + s.facing().extend(0.0) * 60.0;
        let arena = Arena { boxes: vec![Box3 { min: ahead - Vec3::new(40.0, 5.0, 0.0), max: ahead + Vec3::new(40.0, 5.0, 20.0) }], ..Arena::default() };
        let mut knocks = 0;
        for _ in 0..240 {
            step(&mut s, &gas, &t, &arena, DT);
            knocks += started(&s, preset::VEHICLE_HIT_WALL);
        }
        assert!(knocks >= 1, "no knock; at {} speed {}", s.pos, s.speed);
    }

    #[test]
    fn car_pulls_away_smoothly_and_reaches_recorded_cap() {
        let t = tuning();
        let mut s = State::new(&t);
        let gas = Input { vehicle: 1.0, ..Default::default() };
        let arena = Arena::default();
        step(&mut s, &gas, &t, &arena, GAME_DT);
        // Flat arena start. Run1 t621.66 was on changing/sloped ground (support
        // normal xy .0087,-.0048 rising to .0528,-.0415), not this arena. The old
        // unseeded speed-curve comparison rewarded discarding horizontal spring
        // force. Recorded spring/velocity/omega replay now lives in vehicle.rs.
        let got = speeds(&mut s, gas, &t, 45);
        assert!(got[..36].windows(2).all(|v|v[1]>v[0]));
        assert!(got[0]>=0.0 && got[0]<1.0 && got[1]>0.0);
        assert!((got[44] - 37.0).abs() < 0.5, "{:.2}", got[44]);
    }

    #[test]
    fn car_brakes_in_a_straight_line_and_reverses() {
        let t = tuning();
        let mut s = State::new(&t);
        let gas = Input { vehicle: 1.0, ..Default::default() };
        run(&mut s, gas, &t, 4.0);
        let braking = Input { brake: 1.0, ..gas };
        let before = s.speed;
        run(&mut s, braking, &t, 1.0);
        // t 421.31 in run 1: 37.61, and 25.17 a second later. The cap comes down by
        // brakingDeceleration (15 a second) while the engine keeps pushing a little past it.
        assert!((before - s.speed - 12.4).abs() < 0.8, "lost {}", before - s.speed);
        run(&mut s, braking, &t, 4.0);
        assert!(s.speed < -10.0 && s.speed >= -t.drive.max_reverse_speed - 3.0, "reverse {}", s.speed);
    }

    #[test]
    fn car_turns_toward_the_stick_and_straightens() {
        let t = tuning();
        let mut s = State::new(&t);
        let gas = Input { vehicle: 1.0, ..Default::default() };
        run(&mut s, gas, &t, 4.0);
        let yaw = s.yaw;
        run(&mut s, Input { steer: 1.0, ..gas }, &t, 1.0);
        // Stick right turns him clockwise. At top speed the game turned at about 1 radian a
        // second with the stick hard over (t 644.9: 0.75 rising to 1.47, then settling).
        let turned = yaw - s.yaw;
        assert!((0.6..1.6).contains(&turned), "turned {turned}");
        run(&mut s, gas, &t, 1.5);
        assert!(s.car.yaw_rate.abs() < 0.05, "still turning at {}", s.car.yaw_rate);
        assert!(s.speed > 30.0);
    }

    #[test]
    fn jumping_out_of_the_car_at_speed_is_the_long_jump() {
        let t = tuning();
        let arena = Arena::default();
        let gas = Input { vehicle: 1.0, ..Default::default() };
        // Let go first and jump after: the unfold has no way into a jump. [game: the graph]
        let mut s = State::new(&t);
        run(&mut s, gas, &t, 4.0);
        step(&mut s, &Input::default(), &t, &arena, DT);
        assert_eq!(s.mode, Mode::Unfold);
        step(&mut s, &Input { jump: true, jump_held: true, ..Default::default() }, &t, &arena, DT);
        assert_eq!(s.mode, Mode::Unfold);
        // Jump held as the trigger goes up: the long jump.
        let mut s = State::new(&t);
        run(&mut s, gas, &t, 4.0);
        step(&mut s, &Input { jump: true, jump_held: true, ..Default::default() }, &t, &arena, DT);
        assert_eq!(s.mode, Mode::Jump(JumpType::Long));
        // With the stick centred the long jump still drives toward its own speed, and it
        // peaks at the long jump's height.
        let before = Vec2::new(s.vel.x, s.vel.y).length();
        for _ in 0..600 {
            step(&mut s, &Input::default(), &t, &arena, DT);
            if s.vel.z <= 0.0 {
                break;
            }
        }
        assert!(Vec2::new(s.vel.x, s.vel.y).length() > before);
        assert!((s.apex_z - t.jump.long.height).abs() < 0.3, "apex {}", s.apex_z);
    }

    #[test]
    fn walls_block_and_low_boxes_can_be_stood_on() {
        let t = tuning();
        let arena = Arena { boxes: vec![Box3 { min: Vec3::new(-5.0, 10.0, 0.0), max: Vec3::new(5.0, 20.0, 4.0) }], ..Default::default() };
        let mut s = State::new(&t);
        for _ in 0..240 {
            step(&mut s, &Input { stick: Vec2::Y, ..Default::default() }, &t, &arena, DT);
        }
        assert!(s.pos.y <= 10.0 - t.robot_radius + 0.05, "y {}", s.pos.y);
        assert_eq!(arena.floor(0.0, 15.0, 4.0), 4.0);
        assert_eq!(arena.floor(0.0, 15.0, 0.0), 0.0);
    }

    /// Coming down just under a ledge after the jump up off a wall, he is put on it; not
    /// while still rising, not from further below, not without a top set by the climb.
    #[test]
    fn the_jump_up_off_a_wall_catches_the_ledge_above() {
        let t = tuning();
        let arena = Arena { boxes: vec![Box3 { min: Vec3::new(-10.0, 10.0, 0.0), max: Vec3::new(10.0, 30.0, 20.0) }], ..Default::default() };
        let under = |z: f32, vz: f32| {
            let mut s = State::new(&t);
            s.pos = Vec3::new(0.0, 10.0 - t.robot_radius, z);
            s.vel = Vec3::new(0.0, 0.0, vz);
            s.climb_top = Vec3::new(0.0, s.pos.y, 20.0);
            s.climb_top_facing = Vec2::Y;
            s
        };
        let mut s = under(19.0, -3.0);
        assert!(ledge_catch(&mut s, &t, &arena));
        let on_top = Vec3::new(0.0, 10.0 - t.robot_radius + 2.25 * t.robot_radius, 20.0);
        assert!((s.pos - on_top).length() < 1e-4, "{:?}", s.pos);
        assert_eq!(s.vel, Vec3::ZERO);

        assert!(!ledge_catch(&mut under(19.0, 3.0), &t, &arena), "still rising");
        assert!(!ledge_catch(&mut under(18.4, -3.0), &t, &arena), "more than 1.5 below");
        assert!(!ledge_catch(&mut under(20.5, -3.0), &t, &arena), "above the top");
        let mut turned = under(19.0, -3.0);
        turned.yaw = 0.5;
        assert!(!ledge_catch(&mut turned, &t, &arena), "not facing as at the top");
        let mut unset = under(19.0, -3.0);
        unset.climb_top_facing = Vec2::ZERO;
        assert!(!ledge_catch(&mut unset, &t, &arena), "the climb set no top");
    }

    /// A small copy of the attack chain's shape: a press from standing starts a hit; a press
    /// inside the hit's button window chains a second when the window shuts; a hit that runs
    /// out goes back to standing.
    fn with_attack_chain(mut t: Tuning) -> Tuning {
        use crate::control::{Detector, Stage, StateDef, StickRange, Test, Transition};
        use crate::formats::anim::{Action as A, ActionEvent};
        let any = StickRange { amp_min: -1.0, amp_max: 100.0, angle_min: -7.0, angle_max: 7.0, camera_relative: true };
        let press = Stage { time: 0.0, charge: 0.0, include: vec![], exclude: vec![], hit: Some(button::ATTACK_FAST), left: any };
        let mut g = std::mem::take(&mut t.control);
        let attack = g.add_connection("attack", Test::Detect(Detector { stages: vec![press], stays_on: 0.5, hold_with_last_stage: false }));
        g.connections[attack].on_ground = true;
        let chain = g.add_connection("chain", Test::None);
        g.connections[chain].attack_can_branch = true;
        let done = g.add_connection("done", Test::None);
        (g.connections[done].branch_begin, g.connections[done].branch_end) = (100, 100);
        let state = |name: &str, class: &str, anim: &str| StateDef {
            name: name.into(),
            class: class.into(),
            anim: if anim.is_empty() { 0 } else { crc32(anim.as_bytes()) },
            motion_mode: ATTACK_MODE,
            ..Default::default()
        };
        let idle = g.find("stateIdle").unwrap();
        let one = g.add_state(state("stateAttack1", "StateAttack", "Hit1"));
        let two = g.add_state(state("stateAttack2", "StateAttack", "Hit2"));
        g.states[idle].transitions.push(Transition { connection: attack, output: Some(one), priority: 3 });
        g.states[one].transitions = vec![
            Transition { connection: chain, output: Some(two), priority: 2 },
            Transition { connection: done, output: Some(idle), priority: 0 },
        ];
        g.states[two].transitions = vec![Transition { connection: done, output: Some(idle), priority: 0 }];
        t.control = g;
        // 60 ticks carrying the root 6 forward; the window from tick 10 to 36.
        let event = |time, action| ActionEvent { time, action };
        let clip = |id: &str| ActionClip {
            set: "CombatSet".into(),
            id: id.into(),
            ticks: 60.0,
            speed: 1.0,
            crossfade: 0.0,
            looping: false,
            path: (0..=60).map(|tick| Vec3::Y * (tick as f32 / 10.0)).collect(),
            turn: Vec::new(),
            events: vec![
                event(10.0, A::ButtonWindow { open: true }),
                event(36.0, A::ButtonWindow { open: false }),
                event(38.0, A::BranchPoint),
            ],
            attack: None,
            bodies: Vec::new(),
            blasts: Vec::new(),
        };
        for id in ["Hit1", "Hit2"] {
            t.actions.clips.insert(crc32(id.as_bytes()), clip(id));
        }
        t
    }

    #[test]
    fn a_press_attacks_and_a_press_in_the_window_chains_the_next_hit() {
        let t = with_attack_chain(tuning());
        let arena = Arena::default();
        let tap = Input { buttons_down: 1 << button::ATTACK_FAST, buttons_hit: 1 << button::ATTACK_FAST, ..Default::default() };
        let attack_state = |s: &State| s.control_state(&t).map(str::to_owned);

        // One hit on its own: the clip's root carries him 6 forward, then he stands again.
        let mut s = State::new(&t);
        step(&mut s, &tap, &t, &arena, DT);
        assert_eq!(s.mode, Mode::Action);
        assert_eq!(attack_state(&s).as_deref(), Some("stateAttack1"));
        for _ in 0..70 {
            step(&mut s, &Input::default(), &t, &arena, DT);
        }
        assert_eq!(s.mode, Mode::Stand);
        assert!((s.pos.y - 6.0).abs() < 0.3, "travelled {}", s.pos.y);

        // A press inside the window: the second hit starts when the window shuts (tick 36).
        let mut s = State::new(&t);
        step(&mut s, &tap, &t, &arena, DT);
        for _ in 0..20 {
            step(&mut s, &Input::default(), &t, &arena, DT);
        }
        step(&mut s, &tap, &t, &arena, DT);
        let mut chained_at = None;
        for i in 0..40 {
            step(&mut s, &Input::default(), &t, &arena, DT);
            if chained_at.is_none() && attack_state(&s).as_deref() == Some("stateAttack2") {
                chained_at = Some(22 + i);
            }
        }
        assert_eq!(chained_at, Some(36), "chained at update {chained_at:?}");

        // A press before the window opens does not count.
        let mut s = State::new(&t);
        step(&mut s, &tap, &t, &arena, DT);
        step(&mut s, &tap, &t, &arena, DT);
        for _ in 0..70 {
            step(&mut s, &Input::default(), &t, &arena, DT);
        }
        assert_eq!(s.mode, Mode::Stand);
        assert!((s.pos.y - 6.0).abs() < 0.3, "travelled {}", s.pos.y);
    }

    /// The attack chain with what the first hit's clip carries in the install: the turn and
    /// the slide allowed from the start to tick 18, the hit live from tick 10 to 20.
    fn with_melee(t: Tuning) -> Tuning {
        use crate::formats::anim::{Action as A, ActionEvent, AttackRef};
        use crate::formats::scene::Shape;
        const HAND: u32 = crc32(b"Hand_R");
        let mut t = with_attack_chain(t);
        // The attack is taken before a move, so a press with the stick held attacks.
        let idle = t.control.find("stateIdle").unwrap();
        let first = t.control.find("stateAttack1");
        for transition in t.control.states[idle].transitions.iter_mut().filter(|tr| tr.output == first) {
            transition.priority = 100;
        }
        t.melee = crate::melee::MeleeTuning {
            ang_range: 90.0,
            best_dist: 1.5,
            angle_weight: 1.0,
            dist_weight: 0.5,
            match_weight: 0.75,
            distance: 25.0,
            height: 5.0,
            upgrades: [[1.25, 1.5, 2.0]; 3],
        };
        for clip in t.actions.clips.values_mut() {
            clip.attack = Some(AttackRef {
                damage: 30.0,
                mp_damage: 35.0,
                ai_damage: -1.0,
                slide_speed: 60.0,
                max_range: 20.0,
                auto_target_turn_speed: 900.0,
                turn_speed: 900.0,
                stick_choose_target_max_degrees: 180.0,
                auto_target_search_angle: 90.0,
                knock_back_speed: 24.0,
                knock_back_angle: 12.0,
                ..Default::default()
            });
            let event = |time, action| ActionEvent { time, action };
            clip.events.extend([
                event(0.0, A::FaceAndSlide { slide: true, face: true, stick: true }),
                event(10.0, A::Hit { on: true, node: HAND }),
                event(18.0, A::FaceAndSlide { slide: false, face: false, stick: false }),
                event(20.0, A::Hit { on: false, node: HAND }),
            ]);
            // A hand out in front at chest height all through, the size of his own.
            let hand = melee::Body::new(Shape::Sphere { radius: 1.83 }, Affine3A::from_translation(Vec3::new(0.0, 2.5, 3.5)));
            clip.bodies = vec![melee::AttackBody { node: HAND, foot: false, frames: vec![hand; clip.ticks as usize + 1] }];
            clip.events.sort_by(|a, b| a.time.total_cmp(&b.time));
        }
        t
    }

    /// Every clip also sets off the ground punch's blast at tick 8, with the numbers of
    /// Bumblebee's `genericgroundpound`, from a hand held out in front.
    fn with_blast(mut t: Tuning) -> Tuning {
        use crate::formats::anim::{Action as A, ActionEvent};
        use crate::formats::scene::ExplosionDef;
        const NAME: u32 = crc32(b"FX_groundpunch");
        (t.ground_pound_damage, t.ground_pound_damage_weak) = (100.0, 60.0);
        for clip in t.actions.clips.values_mut() {
            clip.events.push(ActionEvent { time: 8.0, action: A::Trigger { name: NAME } });
            clip.events.sort_by(|a, b| a.time.total_cmp(&b.time));
            clip.blasts = vec![melee::ClipBlast {
                ssd: Vec::new(),
                name: NAME,
                node: crc32(b"Hand_R"),
                attach: true,
                explosion: ExplosionDef {
                    radius: 18.0,
                    start_scale: 0.5,
                    time: 0.15,
                    delay: 0.0,
                    damage: 65.0,
                    min_damage: 0.0,
                    fall_off: -1.0,
                    pound_override: true,
                    damage_player: true,
                    can_damage_self: false,
                    damage_flags: melee::damage::BY_MELEE | melee::damage::BY_EXPLOSION | melee::damage::BY_STRONG_FLAIL,
                    knock_back_speed: 35.0,
                    knock_back_angle: 3.0,
                    camera_shake: crc32(b"MassiveCameraShake"),
                    stun_time: 0.0,
                },
                rumble: Some(crate::rumble::RumbleDef {
                    kind: 1,
                    duration: 0.7,
                    strength: 0.55,
                    scale_type: 1,
                    distance_near: 1.0,
                    distance_far: 25.0,
                }),
                centres: vec![Vec3::new(0.0, 2.5, 3.5); clip.ticks as usize + 1],
            }];
        }
        t
    }

    /// `stop_on_hit` (the slam out of the car): the clip's root stops moving him once the
    /// attack has hit a character.
    #[test]
    fn a_clip_that_stops_on_a_hit_carries_him_no_further() {
        let travel = |stop: bool| {
            let mut t = with_melee(tuning());
            for clip in t.actions.clips.values_mut() {
                let attack = clip.attack.as_mut().unwrap();
                (attack.stop_on_hit, attack.slide_speed) = (stop, 0.0);
            }
            let arena = Arena { targets: vec![enemy(1, 0.0, 7.0)], ..Default::default() };
            let tap = Input { buttons_down: 1 << button::ATTACK_FAST, buttons_hit: 1 << button::ATTACK_FAST, ..Default::default() };
            let mut s = State::new(&t);
            step(&mut s, &tap, &t, &arena, DT);
            let (mut hits, mut at_hit) = (0, 0.0);
            for _ in 0..70 {
                step(&mut s, &Input::default(), &t, &arena, DT);
                if !s.hits.is_empty() {
                    (hits, at_hit) = (hits + s.hits.len(), s.pos.y);
                }
            }
            assert_eq!(hits, 1);
            (s.pos.y, at_hit)
        };
        let (free, _) = travel(false);
        let (stopped, at_hit) = travel(true);
        assert!(free > stopped + 1.0, "{free} against {stopped}");
        assert!((stopped - at_hit).abs() < 0.25, "hit at {at_hit}, ended at {stopped}");
    }

    #[test]
    fn the_blast_grows_hits_each_thing_in_reach_once_and_goes() {
        let t = with_blast(with_melee(tuning()));
        // Both behind him, where no fist goes: one within the blast's 18, one beyond it.
        let arena = Arena { targets: vec![enemy(1, 0.0, -10.0), enemy(2, 0.0, -30.0)], ..Default::default() };
        let mut s = State::new(&t);
        let mut hits = Vec::new();
        let mut radii = Vec::new();
        step(&mut s, &TAP, &t, &arena, DT);
        for _ in 0..70 {
            step(&mut s, &Input::default(), &t, &arena, DT);
            hits.extend(s.hits.iter().copied());
            radii.extend(s.blasts.iter().filter(|b| b.live).map(|b| b.radius));
        }
        assert_eq!(hits.len(), 1, "{hits:?}");
        let hit = hits[0];
        // The data's 65 is over 5, so the damage is his `groundPoundDamage`.
        assert_eq!((hit.target, hit.damage, hit.knock_back_speed, hit.blast), (1, 100.0, 35.0, true));
        assert!((hit.knock_back_angle - 3f32.to_radians()).abs() < 1e-6);
        assert!(hit.direction.dot(Vec2::NEG_Y) > 0.99, "{:?}", hit.direction);
        // From half its size to all of it, then gone.
        assert_eq!(radii.first().copied(), Some(9.0));
        assert!(radii.windows(2).all(|w| w[1] >= w[0]) && radii.last().copied() == Some(18.0), "{radii:?}");
        assert!(s.blasts.is_empty());
        // 0.15 s of growing: the update that starts it, nine that grow it, one more at full size.
        assert_eq!(radii.len(), 11, "{radii:?}");
    }

    #[test]
    fn a_blast_marks_each_world_surface_once_without_hurting_a_target() {
        let mut t=with_blast(with_melee(tuning()));
        let info=crate::ssd::Info { radius:7.0,frames:4,noise_ratio:0.406,..Default::default() };
        for clip in t.actions.clips.values_mut() { for blast in &mut clip.blasts { blast.ssd=vec![info]; } }
        let arena=Arena::default(); let mut s=State::new(&t); let mut surfaces=Vec::new();
        step(&mut s,&TAP,&t,&arena,DT);
        for _ in 0..70 {
            step(&mut s,&Input::default(),&t,&arena,DT);
            surfaces.extend(s.surface_hits.iter().filter(|h|h.infos.is_some()).map(|h|h.contact.surface));
            assert!(s.hits.is_empty());
        }
        assert_eq!(surfaces,vec![0]);
    }

    #[test]
    fn a_hand_marks_a_wall_once_per_attack_even_without_an_attack_damage_record() {
        struct Wall;
        impl World for Wall {
            fn floor(&self,_:f32,_:f32,_:f32)->f32 { 0.0 }
            fn push_out(&self,_:&mut Vec3,_:f32,_:f32)->bool { false }
            fn surface_contacts(&self,_:&melee::Body)->Vec<crate::ssd::Contact> {
                vec![crate::ssd::Contact { surface:1,point:Vec3::Y*4.0,normal:-Vec3::Y }]
            }
        }
        let mut t=with_melee(tuning());
        for clip in t.actions.clips.values_mut() { clip.attack=None; }
        let mut s=State::new(&t);
        for _ in 0..2 {
            let mut hits=0;
            step(&mut s,&TAP,&t,&Wall,DT);
            for _ in 0..80 {
                step(&mut s,&Input::default(),&t,&Wall,DT);
                hits+=s.surface_hits.len();
            }
            assert_eq!(hits,1);
        }
    }

    #[test]
    fn the_blast_shakes_the_camera_and_the_pad_once_as_it_is_set_off() {
        let t = with_blast(with_melee(tuning()));
        let arena = Arena::default();
        let mut s = State::new(&t);
        let (mut shakes, mut rumbles) = (Vec::new(), Vec::new());
        step(&mut s, &TAP, &t, &arena, DT);
        for _ in 0..70 {
            step(&mut s, &Input::default(), &t, &arena, DT);
            shakes.extend(s.shakes.iter().copied());
            rumbles.extend(s.rumbles.iter().copied());
        }
        // The shake by its name, at the hand: 2.5 ahead of him and 3.5 up.
        assert_eq!(shakes.len(), 1, "{shakes:?}");
        assert_eq!(shakes[0].name, crc32(b"MassiveCameraShake"));
        assert!((shakes[0].at.z - 3.5).abs() < 1e-4 && shakes[0].at.distance(s.pos) < 6.0, "{:?}", shakes[0]);
        // The rumble weakened by the hand's distance from him: 0.55 * (25 - d) / 24.
        assert_eq!(rumbles.len(), 1, "{rumbles:?}");
        let d = Vec3::new(0.0, 2.5, 3.5).length();
        assert_eq!((rumbles[0].kind, rumbles[0].duration), (1, 0.7));
        assert!((rumbles[0].strength - 0.55 * (25.0 - d) / 24.0).abs() < 1e-4, "{:?}", rumbles[0]);
    }

    fn enemy(id: u32, x: f32, y: f32) -> Target {
        Target { id, pos: Vec3::new(x, y, 0.0), radius: 2.0, height: 5.4, character: true, large: false, surface: melee::surface::UPPER_BODY }
    }
    #[test]
    fn new_vehicle_solver_retains_arena_target_separation() {
        let t=tuning();let mut s=State::new(&t);
        let world=Arena {targets:vec![enemy(1,0.0,20.0)],..Default::default()};
        for _ in 0..300 {
            step(&mut s,&Input {vehicle:1.0,..Default::default()},&t,&world,DT);
            // Check separation throughout contact. Spring/tyre forces can steer past
            // the cylinder; a final y-only bound wrongly requires parking forever.
            assert!(s.pos.truncate().distance(Vec2::new(0.0,20.0))>=t.vehicle_radius+2.0-0.001,"{:?}",s.pos);
        }
    }

    const TAP: Input = Input {
        stick: Vec2::ZERO,
        steer: 0.0,
        pad_stick: Vec2::ZERO,
        jump: false,
        jump_held: false,
        vehicle: 0.0,
        brake: 0.0,
        turbo: false,
        drift: false,
        aim: None,
        buttons_down: 1 << button::ATTACK_FAST,
        buttons_hit: 1 << button::ATTACK_FAST,
    };

    #[test]
    fn an_attack_turns_to_its_target_slides_up_to_it_and_hits_it_once() {
        let t = with_melee(tuning());
        let arena = Arena { targets: vec![enemy(7, 6.0, 12.0)], ..Default::default() };
        let mut s = State::new(&t);
        let mut hits = Vec::new();
        step(&mut s, &TAP, &t, &arena, DT);
        for _ in 0..70 {
            step(&mut s, &Input::default(), &t, &arena, DT);
            if s.action.is_some() {
                assert_eq!(s.action.as_ref().unwrap().target, Some(7));
            }
            hits.extend(s.hits.iter().copied());
        }
        // He faced +y; the target is 26.6 degrees to his right.
        let to_target = Vec2::new(6.0, 12.0) - Vec2::new(s.pos.x, s.pos.y);
        assert!(s.facing().dot(to_target.normalize()) > 0.99, "facing {:?}", s.facing());
        // Up to it and no further: the two radii apart.
        assert!((to_target.length() - 4.0).abs() < 0.05, "{} from it", to_target.length());
        assert_eq!(hits.len(), 1, "{hits:?}");
        let hit = hits[0];
        assert_eq!((hit.target, hit.damage, hit.knock_back_speed), (7, 30.0, 24.0));
        assert!((hit.knock_back_angle - 12f32.to_radians()).abs() < 1e-6);
        // The blow goes his way (attack_dir 0), which by then is toward the target.
        assert!(hit.direction.dot(Vec2::new(6.0, 12.0).normalize()) > 0.95, "{:?}", hit.direction);
    }

    #[test]
    fn each_attack_of_a_chain_hits_and_a_target_out_of_range_is_left_alone() {
        let t = with_melee(tuning());
        let arena = Arena { targets: vec![enemy(1, 0.0, 9.0)], ..Default::default() };
        let mut s = State::new(&t);
        let mut hits = Vec::new();
        let run = |s: &mut State, input: &Input, updates: usize, hits: &mut Vec<Hit>| {
            for _ in 0..updates {
                step(s, input, &t, &arena, DT);
                hits.extend(s.hits.iter().copied());
            }
        };
        run(&mut s, &TAP, 1, &mut hits);
        run(&mut s, &Input::default(), 20, &mut hits);
        run(&mut s, &TAP, 1, &mut hits);
        run(&mut s, &Input::default(), 90, &mut hits);
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert_eq!(s.mode, Mode::Stand);

        // 40 away with a range of 20: no target, no turn, no slide, no hit.
        let far = Arena { targets: vec![enemy(1, 20.0, 35.0)], ..Default::default() };
        let mut s = State::new(&t);
        step(&mut s, &TAP, &t, &far, DT);
        for _ in 0..70 {
            step(&mut s, &Input::default(), &t, &far, DT);
            assert!(s.hits.is_empty());
        }
        assert!(s.yaw.abs() < 1e-6 && (s.pos.y - 6.0).abs() < 0.3 && s.pos.x.abs() < 1e-4, "{:?} {}", s.pos, s.yaw);
    }

    #[test]
    fn the_stick_picks_between_two_targets_and_turns_him_when_there_is_none() {
        let t = with_melee(tuning());
        // One ahead and one to the left, both in range; the stick held left picks the left.
        let arena = Arena { targets: vec![enemy(1, 0.0, 10.0), enemy(2, -10.0, 0.0)], ..Default::default() };
        let left = Input { stick: Vec2::NEG_X, ..Default::default() };
        let mut s = State::new(&t);
        step(&mut s, &TAP, &t, &arena, DT);
        step(&mut s, &Input::default(), &t, &arena, DT);
        assert_eq!(s.action.as_ref().unwrap().target, Some(1));
        let mut s = State::new(&t);
        step(&mut s, &Input { stick: Vec2::NEG_X, ..TAP }, &t, &arena, DT);
        step(&mut s, &left, &t, &arena, DT);
        assert_eq!(s.action.as_ref().unwrap().target, Some(2));

        // Nothing to fight: the stick turns him, at the clip's turn speed.
        let empty = Arena::default();
        let mut s = State::new(&t);
        step(&mut s, &Input { stick: Vec2::NEG_X, ..TAP }, &t, &empty, DT);
        for _ in 0..5 {
            step(&mut s, &left, &t, &empty, DT);
        }
        assert!((s.yaw - std::f32::consts::FRAC_PI_2).abs() < 1e-3, "yaw {}", s.yaw);
    }

    /// The ground half of Bumblebee's weapon set as the game's data has it
    /// (`cargo run -p tf2-core --example rules_dump -- WeaponSet`): the idle, the two
    /// turns on the spot, and the eight runs with their `Prev` twins, each 50 ticks and
    /// 16.67 along a root turned its own way; their exit rules and the set's entry rules.
    fn with_weapon_set(mut t: Tuning) -> Tuning {
        use crate::animrules::var;
        use crate::formats::anim::{ExitRule, IncomingRule, Rule};
        use crate::tuning::{RuleClip, RuleSet};
        let float = |variable, logic, float| Rule { variable, logic, float, ..Default::default() };
        let pushed = float(var::L_STICK_AMP, 3, 0.01);
        let centred = float(var::L_STICK_AMP, 4, 0.01);
        let strafing = Rule { variable: var::IS_STRAFING, logic: 0, int: 1, ..Default::default() };
        // Name, the degrees its root is turned to the left, and the angles it is picked in.
        let ways: [(&str, f32, &[(f32, f32)]); 8] = [
            ("Fwd", 0.0, &[(-22.5, 22.5)]),
            ("FwdL", 45.0, &[(22.5, 67.5)]),
            ("Left", 90.0, &[(67.5, 112.5)]),
            ("BackL", 135.0, &[(112.5, 157.5)]),
            ("Back", 180.0, &[(157.5, 1e9), (-1e9, -157.5)]),
            ("BackR", -135.0, &[(-157.5, -112.5)]),
            ("Right", -90.0, &[(-112.5, -67.5)]),
            ("FwdR", -45.0, &[(-67.5, -22.5)]),
        ];
        let within = |&(low, high): &(f32, f32)| {
            let mut rules = vec![pushed];
            if low > -1e8 {
                rules.push(float(var::STRAFE_ANG, 3, low));
            }
            if high < 1e8 {
                rules.push(float(var::STRAFE_ANG, 4, high));
            }
            rules
        };
        let id = |name: &str| crc32(name.as_bytes());
        let to = |to_id, rules| ExitRule { to_set: WEAPON_SET, to_id, rules, ..Default::default() };
        let mut clips = vec![RuleClip {
            id: id("Weapon_Idle"),
            name: "Weapon_Idle".into(),
            ticks: 84.0,
            speed: 1.0,
            crossfade: 0.25,
            looping: true,
            exit_crossfade: -1.0,
            path: vec![Vec3::ZERO; 85],
            turn: vec![0.0; 85],
            exits: vec![
                to(0, vec![pushed]),
                to(id("Weapon_IdleTurnL"), vec![float(var::STRAFE_ANG, 3, 45.0)]),
                to(id("Weapon_IdleTurnR"), vec![float(var::STRAFE_ANG, 4, -45.0)]),
            ],
            ..Default::default()
        }];
        for (name, sign) in [("Weapon_IdleTurnL", 1.0f32), ("Weapon_IdleTurnR", -1.0)] {
            clips.push(RuleClip {
                id: id(name),
                name: name.into(),
                ticks: 20.0,
                speed: 1.0,
                crossfade: 0.1,
                exit_crossfade: -1.0,
                path: vec![Vec3::ZERO; 21],
                turn: (0..=20).map(|tick| sign * 67.5f32.to_radians() * tick as f32 / 20.0).collect(),
                exits: vec![to(0, vec![pushed]), ExitRule { to_set: WEAPON_SET, finish: true, ..Default::default() }],
                ..Default::default()
            });
        }
        let mut incoming = vec![IncomingRule { to_set: WEAPON_SET, to_id: id("Weapon_Idle"), rules: vec![centred, strafing], ..Default::default() }];
        for (way, turned, ranges) in ways {
            let along = Vec2::from_angle(turned.to_radians()).rotate(Vec2::Y);
            for prefix in ["Weapon_Run", "Weapon_PrevRun"] {
                let name = format!("{prefix}{way}");
                let mut exits = vec![to(id("Weapon_Idle"), vec![centred])];
                for (other, _, ranges) in ways.iter().filter(|w| w.0 != way) {
                    exits.extend(ranges.iter().map(|range| to(id(&format!("Weapon_PrevRun{other}")), within(range))));
                }
                clips.push(RuleClip {
                    id: id(&name),
                    name,
                    ticks: 50.0,
                    speed: 1.0,
                    crossfade: 0.3,
                    looping: true,
                    keeps_time: prefix == "Weapon_PrevRun",
                    continue_time: false,
                    exit_crossfade: -1.0,
                    exit_crossfade_to: 0,
                    path: (0..=50).map(|tick| (along * (16.667 * tick as f32 / 50.0)).extend(0.0)).collect(),
                    turn: vec![0.0; 51],
                    branches: Vec::new(),
                    exits,
                });
            }
            for range in ranges {
                let to_id = id(&format!("Weapon_Run{way}"));
                incoming.push(IncomingRule { to_set: WEAPON_SET, to_id, rules: within(range), ..Default::default() });
            }
        }
        // The air half: three take-offs and their rises, the fall, and the landings out of
        // it: on the spot, or running on one of four ways, each of those left only once its
        // branch point has passed. The same again after a long fall.
        let int = |variable, int| Rule { variable, logic: 0, int, ..Default::default() };
        let not_strafing = int(var::IS_STRAFING, 0);
        let falling = vec![not_strafing, int(var::IN_AIR, 1), float(var::Z_VEL, 5, -0.01)];
        let still = |name: &str, ticks: usize, crossfade: f32, looping: bool, exits: Vec<ExitRule>| RuleClip {
            id: id(name),
            name: name.into(),
            ticks: ticks as f32,
            speed: 1.0,
            crossfade,
            looping,
            exit_crossfade: -1.0,
            path: vec![Vec3::ZERO; ticks + 1],
            turn: vec![0.0; ticks + 1],
            exits,
            ..Default::default()
        };
        let finished = |to_id| ExitRule { to_set: WEAPON_SET, to_id, finish: true, ..Default::default() };
        for side in ["", "_L", "_R"] {
            let rising = format!("Weapon_JumpRising{side}");
            let exits = vec![to(id("Weapon_Fall"), falling.clone()), finished(id(&rising))];
            clips.push(still(&format!("Weapon_JumpTakeOff{side}"), 10, 0.0, false, exits));
            clips.push(still(&rising, 30, 0.1, false, vec![to(id("Weapon_Fall"), falling.clone())]));
        }
        clips.push(still("Weapon_Fall", 56, 0.3, true, Vec::new()));
        let from_fall = |to_id, rules| IncomingRule { from_set: WEAPON_SET, from_id: id("Weapon_Fall"), to_set: WEAPON_SET, to_id, rules, ..Default::default() };
        let mut air = Vec::new();
        for (prefix, long) in [("Weapon_FallBigLand", true), ("Weapon_FallLand", false)] {
            let after = |mut rules: Vec<Rule>| {
                if long {
                    rules.insert(0, float(var::FALL_DUR, 3, 1.0));
                }
                rules
            };
            clips.push(still(prefix, 50, 0.03, false, vec![to(0, vec![pushed]), finished(0)]));
            air.push(from_fall(id(prefix), after(vec![centred, strafing])));
            // Way, the degrees its root is turned, its length and branch point, the angles
            // it is picked in, and the angles it is left in before its branch point.
            let ways: [(&str, f32, usize, f32, &[(f32, f32)], [(u8, f32); 2]); 4] = [
                ("Fwd", 0.0, 26, 20.0, &[(-45.0, 45.0)], [(3, 22.5), (4, -22.5)]),
                ("Back", 180.0, 26, 20.0, &[(-1e9, -100.0), (100.0, 1e9)], [(4, 157.5), (3, -157.5)]),
                ("Left", 90.0, 30, 24.0, &[(45.0, 135.0)], [(4, 67.5), (3, 112.5)]),
                ("Right", -90.0, 30, 24.0, &[(-135.0, -45.0)], [(3, -67.5), (4, -112.5)]),
            ];
            for (way, turned, ticks, branch, ranges, early) in ways {
                let name = format!("{prefix}2Run{way}");
                let along = Vec2::from_angle(turned.to_radians()).rotate(Vec2::Y);
                let mut exits: Vec<ExitRule> = early.iter().map(|&(logic, angle)| to(0, vec![pushed, float(var::STRAFE_ANG, logic, angle)])).collect();
                exits.push(ExitRule { to_set: WEAPON_SET, branch_point: 2, ..Default::default() });
                clips.push(RuleClip {
                    path: (0..=ticks).map(|tick| (along * (7.0 * tick as f32 / ticks as f32)).extend(0.0)).collect(),
                    exit_crossfade: 0.07,
                    exit_crossfade_to: id(&format!("Weapon_Run{way}")),
                    branches: vec![branch],
                    ..still(&name, ticks, 0.0, false, exits)
                });
                for range in ranges {
                    let mut rules = within(range);
                    rules.insert(0, strafing);
                    air.push(from_fall(id(&name), after(rules)));
                }
            }
        }
        for set in ["JumpSet", "FallSet"] {
            air.push(IncomingRule { from_set: id(set), to_set: WEAPON_SET, to_id: id("Weapon_Fall"), ..Default::default() });
        }
        air.push(IncomingRule { to_set: WEAPON_SET, to_id: id("Weapon_Fall"), rules: falling, ..Default::default() });
        let sideways = |low, high| vec![not_strafing, pushed, float(var::L_STICK_DIR, 3, low), float(var::L_STICK_DIR, 4, high)];
        let take_off = |name: &str, rules| IncomingRule { from_set: WEAPON_SET, to_set: WEAPON_SET, to_id: id(name), rules, ..Default::default() };
        air.push(take_off("Weapon_JumpTakeOff_L", sideways(45.0, 135.0)));
        air.push(take_off("Weapon_JumpTakeOff_R", sideways(-135.0, -45.0)));
        air.push(take_off("Weapon_JumpTakeOff", vec![not_strafing]));
        air.extend(incoming);
        t.strafe_speed = 20.0;
        t.weapon_set = RuleSet { name: WEAPON_SET, set_name: "WeaponSet".into(), clips, incoming: air };
        t
    }

    /// Steps at the game's rate and returns, after each update, his ground speed and the way
    /// he is going measured from his aim, in degrees to the left.
    fn strafes(s: &mut State, input: Input, t: &Tuning, updates: usize) -> Vec<(f32, f32)> {
        let arena = Arena::default();
        (0..updates)
            .map(|_| {
                step(s, &input, t, &arena, GAME_DT);
                let v = Vec2::new(s.vel.x, s.vel.y);
                (v.length(), wrapped(yaw_of(v) - s.strafe.aim_yaw).to_degrees())
            })
            .collect()
    }

    fn assert_strafes(got: &[(f32, f32)], recorded: &[(f32, f32)]) {
        assert_eq!(got.len(), recorded.len());
        for (i, (g, r)) in got.iter().zip(recorded).enumerate() {
            let off = wrapped((g.1 - r.1).to_radians()).to_degrees().abs();
            assert!((g.0 - r.0).abs() <= 0.5 && (off <= 2.5 || r.0 < 0.1), "update {i}: {g:.1?} here, {r:?} in the game\n{got:.1?}");
        }
    }

    /// Steps until the named control state is the one running; gives the weapon set's
    /// clips seen on the way, in order, and the number of updates.
    fn until_state(s: &mut State, input: Input, t: &Tuning, state: &str) -> (Vec<String>, usize) {
        let arena = Arena::default();
        let (mut seen, mut updates) = (Vec::<String>::new(), 0);
        while s.control_state(t) != Some(state) {
            step(s, &input, t, &arena, GAME_DT);
            updates += 1;
            assert!(updates < 400, "never reached {state}: {:?}, {seen:?}", s.control_state(t));
            if let Some((_, clip, _)) = s.action_clip(t).filter(|_| s.control_state(t) != Some(state)) {
                if seen.last().map(String::as_str) != Some(clip) {
                    seen.push(clip.to_owned());
                }
            }
        }
        (seen, updates)
    }

    #[test]
    fn a_jump_in_weapon_mode_goes_sideways_facing_the_aim_and_hangs_at_the_top() {
        let t = with_weapon_set(tuning());
        let arena = Arena::default();
        let aiming = Input { aim: Some(Vec2::Y), ..Default::default() };
        let left = Input { stick: Vec2::NEG_X, ..aiming };
        let mut s = State::new(&t);
        run(&mut s, left, &t, 1.0);
        assert_eq!(s.action_clip(&t).map(|c| c.1), Some("Weapon_RunLeft"));
        step(&mut s, &Input { jump: true, jump_held: true, ..left }, &t, &arena, GAME_DT);
        assert_eq!(s.control_state(&t), Some("stateWeaponModeJump"));
        // The stick is to the left of his aim: the take-off to the left, and no longer
        // strafing as the rules see it.
        assert_eq!(s.action_clip(&t).map(|c| c.1), Some("Weapon_JumpTakeOff_L"));
        assert!(!s.strafe.strafing && s.strafe.angle == 0.0);
        assert_eq!(s.mode, Mode::Jump(JumpType::Moving));
        // Launched with the running jump's speed (the body has had one update of gravity).
        let launch = (2.0 * t.gravity * t.jump.run.height).sqrt();
        assert!((s.vel.z - (launch - t.gravity * GAME_DT)).abs() < 0.01, "{}", s.vel.z);

        // Up and over: he never turns to the stick, the factor goes down to the data's
        // quarter near the top, and comes back before he lands.
        let (mut slowest, mut turned, mut airborne) = (1.0f32, 0.0f32, 1);
        let mut seen = vec!["Weapon_JumpTakeOff_L".to_owned()];
        while s.control_state(&t) == Some("stateWeaponModeJump") {
            step(&mut s, &left, &t, &arena, GAME_DT);
            airborne += 1;
            assert!(airborne < 400);
            slowest = slowest.min(s.dilation);
            turned = turned.max(s.yaw.abs());
            let clip = s.action_clip(&t).map(|c| c.1.to_owned()).unwrap();
            if seen.last() != Some(&clip) {
                seen.push(clip);
            }
        }
        assert!(turned < 1e-4, "he turned by {turned}");
        assert_eq!(slowest, t.jump.weapon_dilation);
        // The height is the plain jump's: slowing the time does not change the arc.
        assert!((s.apex_z - s.launch_z - t.jump.run.height).abs() < 0.2, "{}", s.apex_z - s.launch_z);
        // The plain jump is up and down in 1.51 s; the top of this one is stretched by a
        // quarter of a second.
        let plain = 2.0 * launch / t.gravity;
        let longer = airborne as f32 * GAME_DT - plain;
        assert!((0.2..0.35).contains(&longer), "{airborne} updates, {longer} s longer");
        assert_eq!(seen, ["Weapon_JumpTakeOff_L", "Weapon_JumpRising_L", "Weapon_Fall", "Weapon_FallLand2RunLeft"]);

        // Down in weapon mode again, strafing, on the landing that runs on to the left; it
        // is left once its branch point (tick 24 of 30) has passed, not at once.
        assert_eq!(s.mode, Mode::Strafe);
        assert!(s.strafe.strafing && s.dilation == 1.0);
        let mut landing = 1;
        while s.action_clip(&t).map(|c| c.1) == Some("Weapon_FallLand2RunLeft") {
            step(&mut s, &left, &t, &arena, GAME_DT);
            landing += 1;
            assert!(landing < 100);
        }
        let ticks = landing as f32 * GAME_DT * 60.0;
        assert!((24.0..28.0).contains(&ticks), "left after {ticks} ticks");
        assert_eq!(s.action_clip(&t).map(|c| c.1), Some("Weapon_RunLeft"));
        run(&mut s, left, &t, 0.3);
        let v = Vec2::new(s.vel.x, s.vel.y);
        assert!((v.length() - 20.0).abs() < 0.1 && (v.x + 20.0).abs() < 0.2, "{v:?}");
    }

    #[test]
    fn a_standing_jump_in_weapon_mode_lands_on_the_spot() {
        let t = with_weapon_set(tuning());
        let arena = Arena::default();
        let aiming = Input { aim: Some(Vec2::Y), ..Default::default() };
        let mut s = State::new(&t);
        run(&mut s, aiming, &t, 0.5);
        step(&mut s, &Input { jump: true, jump_held: true, ..aiming }, &t, &arena, GAME_DT);
        assert_eq!(s.mode, Mode::Jump(JumpType::Standing));
        assert_eq!(s.action_clip(&t).map(|c| c.1), Some("Weapon_JumpTakeOff"));
        let (seen, _) = until_state(&mut s, aiming, &t, "stateWeaponMode");
        assert_eq!(seen, ["Weapon_JumpTakeOff", "Weapon_JumpRising", "Weapon_Fall"]);
        // The hang stretches the way down from 0.83 s to 1.06 s, which is past the second
        // the set's rules count as a long fall, so he lands on the spot with the big
        // landing. Only two updates past: no recording says the original lands so.
        assert!((1.0..1.1).contains(&s.fall_time), "{}", s.fall_time);
        assert_eq!(s.action_clip(&t).map(|c| c.1), Some("Weapon_FallBigLand"));
        // It plays out (50 ticks), then the idle.
        run(&mut s, aiming, &t, 0.7);
        assert_eq!(s.action_clip(&t).map(|c| c.1), Some("Weapon_FallBigLand"));
        run(&mut s, aiming, &t, 0.3);
        assert_eq!(s.action_clip(&t).map(|c| c.1), Some("Weapon_Idle"));
    }

    #[test]
    fn the_firing_state_runs_on_top_of_weapon_mode_while_the_fire_button_is_down() {
        let t = with_weapon_set(tuning());
        let arena = Arena::default();
        let aiming = Input { aim: Some(Vec2::Y), ..Default::default() };
        let fire = Input { buttons_down: 1 << button::WEAPON_FIRE, ..aiming };
        let mut s = State::new(&t);
        // Out of weapon mode the button does nothing.
        run(&mut s, Input { buttons_down: fire.buttons_down, ..Default::default() }, &t, 0.2);
        assert!(!s.firing && s.overlay_state(&t).is_none());
        run(&mut s, aiming, &t, 0.5);
        assert_eq!(s.control_state(&t), Some("stateWeaponMode"));
        // The update the button goes down asks for the state; the next enters it and gives
        // the weapon the held trigger. Weapon mode goes on under it.
        step(&mut s, &fire, &t, &arena, GAME_DT);
        assert!(!s.firing && s.overlay_state(&t).is_none());
        step(&mut s, &fire, &t, &arena, GAME_DT);
        assert!(s.firing);
        assert_eq!(s.overlay_state(&t), Some("stateWeaponModeAttack"));
        assert_eq!(s.control_state(&t), Some("stateWeaponMode"));
        run(&mut s, fire, &t, 0.5);
        assert!(s.firing && s.mode == Mode::Strafe);
        // Let go: that update the state passes the release on and asks to end; the next
        // update it is gone.
        step(&mut s, &aiming, &t, &arena, GAME_DT);
        assert!(!s.firing);
        assert_eq!(s.overlay_state(&t), Some("stateWeaponModeAttack"));
        step(&mut s, &aiming, &t, &arena, GAME_DT);
        assert!(s.overlay_state(&t).is_none());
        // A jump while firing: the weapon state's exit ends the firing state, and the jump
        // state takes it up again while the button stays down.
        run(&mut s, fire, &t, 0.2);
        assert!(s.firing);
        step(&mut s, &Input { jump: true, jump_held: true, ..fire }, &t, &arena, GAME_DT);
        assert_eq!(s.control_state(&t), Some("stateWeaponModeJump"));
        assert!(s.overlay_state(&t).is_none());
        run(&mut s, fire, &t, 0.2);
        assert!(s.firing && s.control_state(&t) == Some("stateWeaponModeJump"));
        // Weapon mode let go while firing: no firing left.
        run(&mut s, Input { buttons_down: fire.buttons_down, ..Default::default() }, &t, 0.2);
        assert!(!s.firing && s.overlay_state(&t).is_none());
    }

    #[test]
    fn the_cars_gun_is_a_firing_state_on_top_of_the_drive_state() {
        let t = tuning();
        let arena = Arena::default();
        let driving = Input { vehicle: 1.0, ..Default::default() };
        let fire = Input { buttons_down: 1 << button::ATTACK_VEHICLE, ..driving };
        let mut s = State::new(&t);
        // On foot the button does not fire.
        run(&mut s, Input { buttons_down: fire.buttons_down, ..Default::default() }, &t, 0.2);
        assert!(!s.firing && !s.car_mode);
        // The control mode is the car's from the first update of the change.
        step(&mut s, &driving, &t, &arena, DT);
        step(&mut s, &driving, &t, &arena, DT);
        assert!(s.car_mode);
        assert_eq!(s.control_state(&t), Some("stateDriveEnterA"));
        run(&mut s, driving, &t, 2.0);
        assert_eq!(s.control_state(&t), Some("stateDrive"));
        // The button down asks for the state, the next update enters it.
        step(&mut s, &fire, &t, &arena, DT);
        assert!(!s.firing);
        step(&mut s, &fire, &t, &arena, DT);
        assert!(s.firing && s.is_vehicle());
        assert_eq!(s.overlay_state(&t), Some("stateWeaponModeVehicleAttack"));
        assert_eq!(s.control_state(&t), Some("stateDrive"));
        run(&mut s, fire, &t, 0.5);
        assert!(s.firing && s.mode == Mode::Drive);
        // Let go: it ends, and it is not taken up again with the button up (leaving the
        // state resets the connection it came by).
        step(&mut s, &driving, &t, &arena, DT);
        assert!(!s.firing);
        step(&mut s, &driving, &t, &arena, DT);
        assert!(s.overlay_state(&t).is_none());
        run(&mut s, driving, &t, 0.3);
        assert!(s.overlay_state(&t).is_none() && !s.firing);
        // Out of the car the control mode is his own again as the unfold starts.
        run(&mut s, Input::default(), &t, 0.2);
        assert!(!s.car_mode);
    }

    /// His special as the data has it: the script on his own object sets off a shockwave
    /// that stuns for 2.5 s and does no damage, a sphere of 15 from half size over 0.25 s.
    fn with_special(mut t: Tuning) -> Tuning {
        use crate::formats::scene::ExplosionDef;
        const SCRIPT: u32 = crc32(b"specialAttack");
        t.special = crate::tuning::SpecialTuning {
            cooldown: 23.5,
            duration: 5.0,
            kind: -1,
            icon: 0,
            script: SCRIPT,
            vehicle_script: crc32(b"specialAttackVehicle"),
        };
        t.actions.spawners = vec![melee::ClipBlast {
            ssd: Vec::new(),
            name: SCRIPT,
            node: 0,
            attach: true,
            explosion: ExplosionDef {
                radius: 15.0,
                start_scale: 0.5,
                time: 0.25,
                delay: 0.0,
                damage: 0.0,
                min_damage: 0.0,
                fall_off: -1.0,
                pound_override: false,
                damage_player: true,
                can_damage_self: false,
                damage_flags: 0,
                knock_back_speed: 0.0,
                knock_back_angle: 0.0,
                camera_shake: 0,
                stun_time: 2.5,
            },
            rumble: None,
            centres: vec![Vec3::new(0.0, 0.0, 2.7)],
        }];
        t
    }

    #[test]
    fn the_special_sets_off_his_script_stuns_what_is_near_and_then_waits_out_its_cooldown() {
        let t = with_special(with_weapon_set(tuning()));
        let arena = Arena { targets: vec![enemy(1, 0.0, 10.0), enemy(2, 0.0, 40.0)], ..Default::default() };
        let press = Input { buttons_down: 1 << button::ATTACK_SPECIAL, buttons_hit: 1 << button::ATTACK_SPECIAL, ..Default::default() };
        let mut s = State::new(&t);
        step(&mut s, &Input::default(), &t, &arena, DT);
        // The press asks for the state; the next update enters it, which starts the
        // ability; the one after, the state is gone again.
        step(&mut s, &press, &t, &arena, DT);
        assert!(s.triggers.is_empty() && s.special_cooldown == 0.0);
        step(&mut s, &Input::default(), &t, &arena, DT);
        assert_eq!(s.triggers, [crc32(b"specialAttack")]);
        assert_eq!(s.overlay_state(&t), Some("stateAttackSpecial"));
        assert!((s.special_cooldown - 23.5).abs() < 1e-4 && (s.special_left - 5.0).abs() < 1e-4);
        assert_eq!(s.control_state(&t), Some("stateIdle"), "he goes on with what he was doing");
        let mut hits: Vec<melee::Hit> = s.hits.clone();
        step(&mut s, &Input::default(), &t, &arena, DT);
        assert!(s.overlay_state(&t).is_none() && s.triggers.is_empty());
        hits.extend(s.hits.iter().copied());
        for _ in 0..20 {
            step(&mut s, &Input::default(), &t, &arena, DT);
            hits.extend(s.hits.iter().copied());
        }
        // The one 10 away is stunned once, with no damage and no throw; the one 40 away
        // is out of the sphere's 15.
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!((hits[0].target, hits[0].damage, hits[0].stun_time, hits[0].blast), (1, 0.0, 2.5, true));
        assert!(!hits[0].throws() && s.blasts.is_empty());
        // Pressed again inside the cooldown: nothing.
        step(&mut s, &press, &t, &arena, DT);
        for _ in 0..5 {
            step(&mut s, &Input::default(), &t, &arena, DT);
            assert!(s.triggers.is_empty() && s.overlay_state(&t).is_none());
        }
        // After it: again. The ability's own five seconds are long over.
        run(&mut s, Input::default(), &t, 23.5);
        assert_eq!((s.special_cooldown, s.special_left), (0.0, 0.0));
        step(&mut s, &press, &t, &arena, DT);
        step(&mut s, &Input::default(), &t, &arena, DT);
        assert_eq!(s.triggers, [crc32(b"specialAttack")]);
    }

    #[test]
    fn the_special_runs_on_top_of_the_firing_state_without_ending_it() {
        let t = with_special(with_weapon_set(tuning()));
        let arena = Arena::default();
        let fire = Input { aim: Some(Vec2::Y), buttons_down: 1 << button::WEAPON_FIRE, ..Default::default() };
        let press = Input { buttons_down: fire.buttons_down | 1 << button::ATTACK_SPECIAL, buttons_hit: 1 << button::ATTACK_SPECIAL, ..fire };
        let mut s = State::new(&t);
        run(&mut s, fire, &t, 1.0);
        assert!(s.firing && s.overlay_state(&t) == Some("stateWeaponModeAttack"));
        step(&mut s, &press, &t, &arena, DT);
        assert!(s.triggers.is_empty());
        step(&mut s, &fire, &t, &arena, DT);
        assert_eq!(s.triggers, [crc32(b"specialAttack")]);
        assert!(s.firing && s.overlay_state(&t) == Some("stateWeaponModeAttack"));
        for _ in 0..10 {
            step(&mut s, &fire, &t, &arena, DT);
            assert!(s.firing && s.triggers.is_empty());
        }
        assert!(s.runner.nested.is_none());
    }

    #[test]
    fn aiming_in_a_plain_jump_starts_the_weapon_fall_and_letting_go_ends_it() {
        let t = with_weapon_set(tuning());
        let arena = Arena::default();
        let mut s = State::new(&t);
        step(&mut s, &Input { jump: true, jump_held: true, ..Default::default() }, &t, &arena, GAME_DT);
        run(&mut s, Input::default(), &t, 0.3);
        assert_eq!(s.control_state(&t), Some("stateJump"));
        assert!(s.action_clip(&t).is_none() && !s.weapon_air());
        // The aim is to his left: the weapon set comes in on its fall clip, rising or not,
        // and the jump mode has him face the aim from the first update.
        let aiming = Input { aim: Some(Vec2::NEG_X), ..Default::default() };
        step(&mut s, &aiming, &t, &arena, GAME_DT);
        assert_eq!(s.control_state(&t), Some("stateWeaponModeFall"));
        assert_eq!(s.action_clip(&t).map(|c| c.1), Some("Weapon_Fall"));
        assert!(s.vel.z > 0.0 && (s.yaw.to_degrees() - 90.0).abs() < 0.01, "{}", s.yaw.to_degrees());
        // The trigger let go: the plain fall, the weapon set's clip gone, and the time
        // factor on its way back to 1.
        run(&mut s, aiming, &t, 0.4);
        assert!(s.dilation < 1.0);
        step(&mut s, &Input::default(), &t, &arena, GAME_DT);
        assert_eq!(s.control_state(&t), Some("stateFall"));
        assert!(s.action_clip(&t).is_none() && !s.weapon_air());
        run(&mut s, Input::default(), &t, 0.3);
        assert_eq!(s.dilation, 1.0);
    }

    #[test]
    fn strafing_from_a_stand_starts_ahead_for_an_update_as_recorded() {
        let t = with_weapon_set(tuning());
        let aiming = Input { aim: Some(Vec2::Y), ..Default::default() };
        let mut s = State::new(&t);
        run(&mut s, aiming, &t, 0.5);
        assert_eq!(s.mode, Mode::Strafe);
        assert_eq!(s.action_clip(&t).map(|c| c.1), Some("Weapon_Idle"));
        // Run 1, t 490.31 and 493.86 (the stick to the left) with 494.37 for the end: one
        // update of the forward clip, then the left one fading in over it.
        let left = Input { stick: Vec2::NEG_X, ..aiming };
        let got = strafes(&mut s, left, &t, 12);
        let recorded = [
            (2.13, 0.0),
            (4.39, 29.4),
            (6.64, 40.5),
            (8.71, 48.0),
            (10.57, 54.6),
            (12.32, 61.1),
            (14.01, 67.7),
            (15.61, 74.1),
            (17.38, 80.9),
            (19.34, 88.0),
            (20.0, 90.0),
            (20.0, 90.0),
        ];
        assert_strafes(&got, &recorded);
        assert!(s.yaw.abs() < 1e-4, "he keeps facing his aim: {}", s.yaw);
        assert_eq!(s.action_clip(&t).map(|c| c.1), Some("Weapon_PrevRunLeft"));
        // t 494.63: the stick through the middle to the other side. The idle fades in for
        // two updates, the forward clip for one, then the right one.
        let mut got = strafes(&mut s, aiming, &t, 2);
        got.extend(strafes(&mut s, Input { stick: Vec2::X, ..aiming }, &t, 4));
        assert_strafes(&got, &[(17.43, 90.0), (14.87, 90.0), (11.16, 78.8), (6.04, 50.5), (5.09, -5.5), (7.46, -38.7)]);
    }

    #[test]
    fn going_into_weapon_mode_at_a_run_carries_on_for_two_updates_as_recorded() {
        let t = with_weapon_set(tuning());
        let mut s = State::new(&t);
        run(&mut s, FORWARD, &t, 1.0);
        assert!((s.speed - 24.0).abs() < 0.1);
        // Run 1, t 489.79: running, the aim 88.6 degrees to his left, the stick as it was.
        let aim = Vec2::from_angle(88.6f32.to_radians()).rotate(Vec2::Y);
        let got = strafes(&mut s, Input { aim: Some(aim), ..FORWARD }, &t, 4);
        assert_eq!(s.mode, Mode::Strafe);
        assert_strafes(&got, &[(23.57, 0.0), (23.13, 0.0), (20.34, -6.2), (17.94, -14.1)]);
        assert!((s.yaw - 88.6f32.to_radians()).abs() < 1e-3, "he turned to the aim at once: {}", s.yaw);
    }

    #[test]
    fn standing_he_twists_to_the_aim_and_turns_on_the_spot_past_45_degrees() {
        let t = with_weapon_set(tuning());
        let ahead = Input { aim: Some(Vec2::Y), ..Default::default() };
        let mut s = State::new(&t);
        run(&mut s, ahead, &t, 0.5);
        // The aim 30 degrees to the left: his body stays, the spine twists by it.
        let aim = |degrees: f32| Input { aim: Some(Vec2::from_angle(degrees.to_radians()).rotate(Vec2::Y)), ..Default::default() };
        run(&mut s, aim(30.0), &t, 0.5);
        assert!(s.yaw.abs() < 1e-4 && (s.strafe.twist.to_degrees() - 30.0).abs() < 0.01, "{} {}", s.yaw, s.strafe.twist);
        assert!((s.strafe.angle - 30.0).abs() < 0.01);
        // 60 degrees: the turning clip takes him 67.5 round, and the twist comes back.
        let arena = Arena::default();
        step(&mut s, &aim(60.0), &t, &arena, GAME_DT);
        assert!((s.yaw.to_degrees() - 14.5).abs() < 0.01, "the body gives what is past 45.5 at once: {}", s.yaw.to_degrees());
        step(&mut s, &aim(60.0), &t, &arena, GAME_DT);
        assert_eq!(s.action_clip(&t).map(|c| c.1), Some("Weapon_IdleTurnL"));
        run(&mut s, aim(60.0), &t, 1.0);
        assert_eq!(s.action_clip(&t).map(|c| c.1), Some("Weapon_Idle"));
        // How much of the clip's 67.5 he gets while it fades in is our blend, not checked.
        let turned = s.yaw.to_degrees();
        assert!(turned > 60.0 && turned < 83.0, "{turned}");
        assert!((s.strafe.twist.to_degrees() - (60.0 - turned)).abs() < 0.01, "{}", s.strafe.twist.to_degrees());
        // Letting the aim go leaves the mode and takes the twist off.
        run(&mut s, Input::default(), &t, 0.2);
        assert_eq!(s.mode, Mode::Stand);
        assert!(s.strafe.twist == 0.0 && !s.strafe.strafing);
    }
}
