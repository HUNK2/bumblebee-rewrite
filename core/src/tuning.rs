//! Gameplay values read from the game's global pack at start-up.
//!
//! Nothing here is a constant of this program: every number comes out of `bnxglobal.str` in the
//! user's install. A value the pack does not have is reported and left at zero.

use std::collections::HashMap;
use std::path::Path;

use glam::Vec3;

use crate::control::Graph;
use crate::formats::anim::{Action, ActionEvent, AnimRef, AnimSet, AttackRef, Clip, ExitRule, IncomingRule, TICKS_PER_SECOND};
use crate::formats::hash::crc32;
use crate::formats::lxb::{DataFile, Node};
use crate::formats::pack::{self, Pack};
use crate::formats::scene::ObjectDef;
use crate::melee::{self, AttackBody, ClipBlast, MeleeTuning};
use crate::rumble::Preset;
use crate::shake::ShakeDef;
use crate::pose::Rig;

#[derive(Clone, Copy, Default, Debug)]
pub struct JumpKind {
    pub height: f32,
    pub speed: f32,
    pub control: f32,
    pub turn_speed: f32,
}

#[derive(Clone, Default, Debug)]
pub struct MoveTuning {
    pub max_speed: f32,
    pub turn_speed: f32,
    pub walk_run_speed: f32,
}

/// How the robot gets about on foot. None of this is a tuning number: the speeds are the walk
/// and run clips' own root motion and the thresholds are the animation rules', so it is filled
/// in from the character's animation set once that is loaded.
#[derive(Clone, Default, Debug)]
pub struct Locomotion {
    pub walk_speed: f32,
    pub run_speed: f32,
    /// Stick deflection above which he moves at all, and above which he runs.
    pub move_stick: f32,
    pub run_stick: f32,
    /// Seconds the idle, walk and run clips take to blend in.
    pub idle_blend: f32,
    pub walk_blend: f32,
    pub run_blend: f32,
    /// The one-off clips that set their own pace: run to idle, and the two landings into a run.
    pub stop: RootCurve,
    pub land_run: RootCurve,
    pub big_land_run: RootCurve,
    /// The jump's take-off clip, and the forward drift of the clip that follows it in the air.
    pub takeoff: RootCurve,
    pub in_air_speed: f32,
    /// Seconds of falling from which a landing is the big one (`animRuleLongFall`).
    pub long_fall: f32,
}

/// A one-off clip's root motion: how far forward the root is at each tick.
#[derive(Clone, Default, Debug)]
pub struct RootCurve {
    /// Seconds the clip takes to blend in.
    pub crossfade: f32,
    pub forward: Vec<f32>,
}

impl RootCurve {
    pub fn new(crossfade: f32, forward: Vec<f32>) -> Self {
        Self { crossfade, forward }
    }

    /// Seconds the clip lasts.
    pub fn duration(&self) -> f32 {
        self.forward.len().saturating_sub(1) as f32 / TICKS_PER_SECOND
    }

    fn at(&self, seconds: f32) -> f32 {
        let Some(last) = self.forward.len().checked_sub(1) else { return 0.0 };
        let tick = (seconds * TICKS_PER_SECOND).clamp(0.0, last as f32);
        let from = tick.floor() as usize;
        let to = (from + 1).min(last);
        self.forward[from] + (self.forward[to] - self.forward[from]) * (tick - from as f32)
    }

    /// Average forward speed of the root between two times in the clip.
    pub fn speed(&self, from: f32, to: f32) -> f32 {
        if to > from { (self.at(to) - self.at(from)) / (to - from) } else { 0.0 }
    }
}

/// A clip one of the control states plays, with what the movement needs of it. [data]
#[derive(Clone, Default, Debug)]
pub struct ActionClip {
    pub set: String,
    pub id: String,
    /// Length in ticks, playback speed, and seconds to blend in (negative: none given).
    pub ticks: f32,
    pub speed: f32,
    pub crossfade: f32,
    pub looping: bool,
    /// Where the root is at each tick, in its own frame at the start: x right, y forward, z up.
    pub path: Vec<Vec3>,
    /// How far the root has turned at each tick, radians toward -x. Only `Wall_JumpOff` turns.
    pub turn: Vec<f32>,
    pub events: Vec<ActionEvent>,
    pub attack: Option<AttackRef>,
    /// The collision bodies the clip's `AttackCollEvent`s switch on, tick by tick.
    pub bodies: Vec<AttackBody>,
    /// The explosions the clip's trigger events set off.
    pub blasts: Vec<ClipBlast>,
}

impl ActionClip {
    /// The root's place at a tick between two of the stored ones.
    pub fn root_at(&self, tick: f32) -> Vec3 {
        let Some(last) = self.path.len().checked_sub(1) else { return Vec3::ZERO };
        let tick = tick.clamp(0.0, last as f32);
        let from = tick.floor() as usize;
        self.path[from].lerp(self.path[(from + 1).min(last)], tick - from as f32)
    }

    /// The root's turn at a tick between two of the stored ones.
    pub fn turn_at(&self, tick: f32) -> f32 {
        let Some(last) = self.turn.len().checked_sub(1) else { return 0.0 };
        let tick = tick.clamp(0.0, last as f32);
        let from = tick.floor() as usize;
        let (a, b) = (self.turn[from], self.turn[(from + 1).min(last)]);
        a + (b - a) * (tick - from as f32)
    }
}

/// The clips of the attack, dodge, ground punch and climb sets, by the CRC-32 of their id
/// (which is how the control states name them), and the dodge's direction rules.
#[derive(Clone, Default, Debug)]
pub struct Actions {
    pub clips: HashMap<u32, ActionClip>,
    /// Degrees between stick and aim up to which a dodge goes forward, and from which it
    /// goes back; between the two it goes to the side. (`animRuleForwardMax`, `animRuleLeftMax`)
    pub dodge_forward: f32,
    pub dodge_back: f32,
    /// Every explosion spawner of his own object, at its node's rest place in his frame,
    /// for the trigger messages no clip sends: the special ability's script (his: the stun
    /// shockwave on node 8). And the same of the car's object. [data] [assumed: the node's
    /// rest place stands for where it is in whatever he is doing when it goes off]
    pub spawners: Vec<ClipBlast>,
    pub vehicle_spawners: Vec<ClipBlast>,
}

/// An object's explosion spawners, each at its node's rest place.
fn rest_spawners(object: &ObjectDef) -> Vec<ClipBlast> {
    object
        .spawners
        .iter()
        .map(|spawner| {
            let node = &object.nodes[spawner.node];
            ClipBlast {
                ssd: spawner.ssd.clone(),
                name: spawner.script_name,
                node: node.name_hash,
                attach: spawner.attach,
                explosion: spawner.explosion,
                rumble: spawner.rumble,
                centres: vec![Vec3::from(node.bind[3])],
            }
        })
        .collect()
}

impl Actions {
    /// Adds the car's own spawners (the special ability in the car's form).
    pub fn with_vehicle(mut self, vehicle: Option<&ObjectDef>) -> Self {
        self.vehicle_spawners = vehicle.map(rest_spawners).unwrap_or_default();
        self
    }

    pub const SETS: [&'static str; 7] =
        ["CombatSet", "DashSet", "GroundPunchSet", "ClimbSet", "DriveSet", "Vehicle2RobotSet", "DeathSet"];

    /// `robot` is his node tree, for the bodies his attacks hit with.
    pub fn from_animations(animations: &AnimSet, robot: &ObjectDef, missing: &mut Vec<String>) -> Self {
        let mut clips = HashMap::new();
        let rig = Rig::new(robot);
        for set in Self::SETS {
            let Some(refs) = animations.sets.get(set) else {
                missing.push(format!("animation set {set}"));
                continue;
            };
            for r in refs {
                let Some(clip) = &r.clip else { continue };
                clips.insert(
                    crc32(r.id.as_bytes()),
                    ActionClip {
                        set: set.to_owned(),
                        id: r.id.clone(),
                        ticks: clip.length,
                        speed: r.speed,
                        crossfade: r.crossfade,
                        looping: r.looping,
                        path: clip.root_path(),
                        turn: clip.root_turn(),
                        events: clip.action_events.clone(),
                        attack: r.attack,
                        bodies: melee::attack_bodies(&rig, robot, clip, r.looping),
                        blasts: melee::clip_blasts(&rig, robot, clip, r.looping),
                    },
                );
            }
        }
        let mut rule = |name: &str| {
            animations.rule_thresholds.get(name).copied().unwrap_or_else(|| {
                missing.push(format!("animation rule {name}"));
                0.0
            })
        };
        Self {
            clips,
            dodge_forward: rule("animRuleForwardMax"),
            dodge_back: rule("animRuleLeftMax"),
            spawners: rest_spawners(robot),
            vehicle_spawners: Vec::new(),
        }
    }

    pub fn named(&self, id: &str) -> Option<&ActionClip> {
        self.clips.get(&crc32(id.as_bytes()))
    }
}

/// A clip of a set the animation rules run, with what the movement needs of it. [data]
#[derive(Clone, Default, Debug)]
pub struct RuleClip {
    /// The CRC-32 of its id, and the id.
    pub id: u32,
    pub name: String,
    pub ticks: f32,
    pub speed: f32,
    /// Seconds to blend in (negative: none given).
    pub crossfade: f32,
    pub looping: bool,
    /// `usePrevAnimFrame`: it starts at the time the clip before had reached.
    pub keeps_time: bool,
    pub continue_time: bool,
    /// `xfadeTimeOnExit` (negative: none): the blend into the clip that follows it, when
    /// that is `exit_crossfade_to` (0: whichever).
    pub exit_crossfade: f32,
    pub exit_crossfade_to: u32,
    /// The root's place and turn at each tick, as `ActionClip`'s.
    pub path: Vec<Vec3>,
    pub turn: Vec<f32>,
    /// The ticks of its `AnimBranchPointEvent`s, which its exit rules can wait for.
    pub branches: Vec<f32>,
    pub exits: Vec<ExitRule>,
}

impl RuleClip {
    /// The branch count at a tick: 1 as the clip starts, one more for each branch point
    /// passed (the animation player's `+0xdc`). [game]
    pub fn branch(&self, tick: f32) -> i32 {
        1 + self.branches.iter().filter(|&&at| at <= tick).count() as i32
    }

    /// How far the root moves and turns between two ticks; a looping clip carries on from
    /// its start.
    pub fn travel(&self, from: f32, to: f32) -> (Vec3, f32) {
        let Some(last) = self.path.len().checked_sub(1) else { return (Vec3::ZERO, 0.0) };
        let at = |tick: f32| {
            let tick = tick.clamp(0.0, last as f32);
            let i = tick.floor() as usize;
            let j = (i + 1).min(last);
            let turn = |k: usize| self.turn.get(k).copied().unwrap_or(0.0);
            (self.path[i].lerp(self.path[j], tick - i as f32), turn(i) + (turn(j) - turn(i)) * (tick - i as f32))
        };
        let (a, b) = (at(from), at(to));
        if self.looping && to > self.ticks {
            let (end, start, wrapped) = (at(self.ticks), at(0.0), at(to - self.ticks));
            return (end.0 - a.0 + wrapped.0 - start.0, end.1 - a.1 + wrapped.1 - start.1);
        }
        (b.0 - a.0, b.1 - a.1)
    }
}

/// One animation set as the rules run it: its clips with their exit rules, and its entry
/// rules. [data]
#[derive(Clone, Default, Debug)]
pub struct RuleSet {
    /// The CRC-32 of the set's name, and the name.
    pub name: u32,
    pub set_name: String,
    pub clips: Vec<RuleClip>,
    pub incoming: Vec<IncomingRule>,
}

impl RuleSet {
    /// Every set of the character's animation set, by the CRC-32 of its name: what the base
    /// layer's controller (`animrules::Base`) walks.
    pub fn all(animations: &AnimSet) -> HashMap<u32, RuleSet> {
        animations
            .sets
            .keys()
            .map(|set| {
                let mut ignored = Vec::new();
                let rules = Self::from_animations(animations, set, &mut ignored);
                (rules.name, rules)
            })
            .collect()
    }

    pub fn from_animations(animations: &AnimSet, set: &str, missing: &mut Vec<String>) -> Self {
        let Some(refs) = animations.sets.get(set) else {
            missing.push(format!("animation set {set}"));
            return Self::default();
        };
        let clips = refs
            .iter()
            .filter_map(|r| {
                let clip = r.clip.as_ref()?;
                Some(RuleClip {
                    id: crc32(r.id.as_bytes()),
                    name: r.id.clone(),
                    ticks: clip.length,
                    speed: r.speed,
                    crossfade: r.crossfade,
                    looping: r.looping,
                    keeps_time: r.keeps_time,
                    continue_time: r.continue_time,
                    exit_crossfade: r.exit_crossfade,
                    exit_crossfade_to: r.exit_crossfade_to,
                    path: clip.root_path(),
                    turn: clip.root_turn(),
                    branches: clip.action_events.iter().filter(|e| matches!(e.action, Action::BranchPoint)).map(|e| e.time).collect(),
                    exits: r.exits.clone(),
                })
            })
            .collect();
        Self { name: crc32(set.as_bytes()), set_name: set.to_owned(), clips, incoming: animations.incoming.get(set).cloned().unwrap_or_default() }
    }

    pub fn clip(&self, id: u32) -> Option<&RuleClip> {
        self.clips.iter().find(|clip| clip.id == id)
    }
}

#[derive(Clone, Default, Debug)]
pub struct JumpTuning {
    pub standing: JumpKind,
    pub run: JumpKind,
    pub high_standing: JumpKind,
    pub long: JumpKind,
    /// The jumps off a wall: the one straight up it and the one away from it, and the
    /// latter's speed in the air.
    pub climb_up_height: f32,
    pub climb_off_height: f32,
    pub climb_off_speed: f32,
    /// The slow motion of a jump while the weapon set plays: it starts once he rises or
    /// falls slower than `weapon_dilation_start`, and the mode's time factor then goes to
    /// `weapon_dilation` at `weapon_dilation_rate` a second (`jumpWeaponZVelStartDilation`,
    /// `jumpWeaponDilation`, `jumpWeaponDilationAcceleration`).
    pub weapon_dilation_start: f32,
    pub weapon_dilation: f32,
    pub weapon_dilation_rate: f32,
}

/// The climb mode's tuning (`modeClimb`). [data]
#[derive(Clone, Default, Debug)]
pub struct ClimbTuning {
    /// Within this of the top he goes over it; and this close to a side he stops shimmying.
    pub pull_up_dist: f32,
    pub edge_limit_dist: f32,
    /// Not read by the climb mode's update. [game]
    pub wall_offset: f32,
    /// How fast the speed he arrives with dies away. Its name is not known (hash bbc71449);
    /// Bumblebee's data leaves it at the loader's default, 50. [game]
    pub arrival_deceleration: f32,
}

#[derive(Clone, Default, Debug)]
pub struct DriveTuning {
    pub max_speed: f32,
    /// DriveMode's per-second trigger filter strength (+0x100). [data]
    pub trigger_filter_strength: f32,
    /// How much of the top speed a full turn of the stick takes off, for the gearbox.
    pub turning_velocity_delta: f32,
    pub max_turbo_speed: f32,
    pub max_turbo_time: f32,
    pub max_turbo_cooldown_time: f32,
    /// Duration f3bd1a64 and cooldown95118079 upgrade levels1..3. [data/game]
    pub turbo_upgrades: [[f32; 3]; 2],
    pub throttle: f32,
    pub turbo_throttle_boost: f32,
    pub post_turbo_deceleration: f32,
    pub braking_deceleration: f32,
    pub max_reverse_speed: f32,
    pub post_reverse_throttle: f32,
    pub exit_to_run_deceleration: f32,
    pub render_wheel_radius: f32,
    /// Steering rate at top speed and at a standstill.
    pub turn_speed: f32,
    pub max_turn_speed: f32,
    pub engine_torque: f32,
    pub wheel_friction: f32,
    /// Distance between the two front wheels, the two rear wheels, and the two axles.
    pub front_axle_length: f32,
    pub rear_axle_length: f32,
    pub long_axle_length: f32,
    pub front_wheel_radius: f32,
    pub rear_wheel_radius: f32,
    pub max_drift_turbo_speed: f32,
    pub min_reverse_brake_time: f32,
    pub max_angular_vel_z: f32,
    /// How fast the body may tip and roll, radians a second.
    pub max_angular_vel_x: f32,
    pub max_angular_vel_y: f32,
    /// Leaving the car on the ground: with the stick held and with it centred.
    pub exit_to_run_min_speed: f32,
    pub exit_to_run_turn_speed: f32,
    pub exit_to_idle_deceleration: f32,
    pub exit_to_idle_min_speed: f32,
    pub exit_to_idle_turn_speed: f32,
    /// The slide: what is left of the tyres' sideways and forward grip, and how hard it slows.
    pub wheel_drift_factor: f32,
    pub drift_steering_factor: f32,
    pub skid_out_deceleration: f32,
    /// Drift turbo charges while absolute sideways velocity exceeds this speed.
    pub min_drift_time: f32,
    pub min_drift_turbo_speed: f32,
    pub drift_turbo_time: f32,
    pub drift_turbo_boost_factor: f32,
    pub suspension_length: f32,
    pub suspension_k: f32,
    pub suspension_damp: f32,
    pub suspension_wt_scale: f32,
    pub angular_vel_damp_x: f32,
    pub angular_vel_damp_y: f32,
    pub min_render_wheel_offset: f32,
    pub max_render_wheel_offset: f32,
    /// Vehicle avatar cuboid and physics centre relative to the rendered root. [data]
    pub body_half: glam::Vec3,
    pub body_offset_z: f32,
}

#[derive(Clone, Default, Debug)]
pub struct CameraTuning {
    pub fov: f32,
    pub min_dist: f32,
    pub max_dist: f32,
    pub min_elevation: f32,
    pub default_elevation: f32,
    pub max_elevation: f32,
    pub user_rot_speed_z: f32,
    pub user_rot_speed_x: f32,
    /// Field of view at top speed, as a percentage of the default. Driving cameras only.
    pub max_speed_fov_scale: f32,
    /// The rest as the data names them (`notes\camera.md`); angles in degrees, scales in percent.
    pub face_extra_dist: f32,
    pub face_extra_dist_gain: f32,
    /// The extra distance while he climbs (15 on Bumblebee's near camera, 0 on the others).
    pub climb_extra_dist: f32,
    pub scale_rot_speed_min: f32,
    pub scale_rot_speed_max: f32,
    pub min_height_gain: f32,
    pub interp_elev_gain: f32,
    pub interp_ang_gain: f32,
    /// Seconds over which the follow camera takes on a new look-at height.
    pub interp_target_height_time: f32,
    pub height_offset: f32,
    pub min_speed_align_move: f32,
    pub max_speed_align_move: f32,
    pub align_gain: f32,
    pub align_damp: f32,
    pub max_speed_dist_add: f32,
    pub user_rot_scale_max_speed: f32,
    pub user_rot_gain: f32,
    pub user_rot_damp: f32,
    pub tilt_gain: f32,
    /// How much of the car's nose-up tilt the driving camera's elevation takes, and how
    /// much of its lean the picture takes with what gain, in percent (100 and 25 for him).
    pub tilt_amount: f32,
    pub lean_amount: f32,
    pub lean_gain: f32,
    /// The weapon camera's own (loader `FUN_00927cd0`): past this many degrees a second
    /// off the aim's heading it swings round instead of snapping; where the point it turns
    /// about sits beside and ahead of him; and the gain its elevation follows the aim with.
    pub max_rot_speed: f32,
    pub side_offset: f32,
    pub ahead_offset: f32,
    pub elev_gain: f32,
}

/// How the stick turns his aim in weapon mode (`AimAssist` of the shared tuning, loader
/// `FUN_0079bea0`, read by `FUN_0071b990`). Angles in degrees as the data has them; the
/// loader turns every one of these to radians, the acceleration too.
#[derive(Clone, Copy, Default, Debug)]
pub struct AimTuning {
    /// Degrees a second at full stick with the aim sensitivity option at its lowest and
    /// at its highest.
    pub lowest_sensitivity: f32,
    pub highest_sensitivity: f32,
    /// How fast the stick's push is let through, per second (data 144: 2.51 a second).
    pub acceleration: f32,
    /// The aim's pitch limits (data -85 and 85).
    pub min_pitch: f32,
    pub max_pitch: f32,
    /// The aim assist: the share of its speed the aim keeps over a target
    /// (`cameraSpeedReduction`, 0.75), how fast that share changes a second
    /// (`cameraSpeedAcceleration`, 4), and how fast the aim is pulled toward a target,
    /// degrees a second (`lookAssistTurnRate`, 5).
    pub speed_reduction: f32,
    pub speed_acceleration: f32,
    pub look_assist_turn_rate: f32,
    /// `targetBoxPixelWidth` and `Height`: the half-size of the box a target must be in
    /// for a weapon that is not a launcher, as shares of 640 and 360 pixels (16 x 16).
    pub target_box: [f32; 2],
    /// `defaultMaxRange`: the assist's range for a weapon that gives none.
    pub default_max_range: f32,
    /// `r2wLockOnConeAngle` and `r2wLockOnConeElv`, degrees: how far to either side of
    /// the camera's line, and above or below it, a target may be for the aim to turn to
    /// it as weapon mode starts.
    pub snap_cone: f32,
    pub snap_cone_elevation: f32,
}

/// The special ability, from the character's attributes (`specialAbility*`; loader offsets
/// `+0xbc` cooldown, `+0xc0` type, `+0xc4` duration, `+0xc8` self trigger, `+0xcc` script,
/// `+0xec` HUD icon, `+0xdc` and `+0xe0` the same two for the car). Bumblebee's type is -1 (none of the six
/// coded ones): his ability is all in what the script name sets off on his own object.
#[derive(Clone, Copy, Default, Debug)]
pub struct SpecialTuning {
    pub cooldown: f32,
    pub duration: f32,
    pub kind: i32,
    /// `specialAbilityIcon`, an index in the HUD's shared icon list. [data]
    pub icon: u32,
    /// `specialAbilityLuxScriptNameToTrigger`, sent to himself when `specialAbilitySelfTrigger`
    /// is on; 0 for none. And the pair for the car's form.
    pub script: u32,
    pub vehicle_script: u32,
}

#[derive(Clone, Default, Debug)]
pub struct Tuning {
    pub name: String,
    pub drive_gears: Vec<crate::sound::Gear>,
    /// Collision registry material hardness (00757c00,011964c0), missing means soft.
    pub surface_hardness: HashMap<u32,bool>,
    pub damage: crate::damage::Tuning,
    pub base_health: f32,
    pub health_regen_rate: f32,
    pub health_regen_delay: f32,
    pub robot_height: f32,
    pub robot_radius: f32,
    pub vehicle_height: f32,
    pub vehicle_radius: f32,
    /// Distance between the axles. Measured from the vehicle model, not stored as a number.
    pub wheelbase: f32,
    pub gravity: f32,
    pub transform_delay: f32,
    pub aiming_height_percentage: f32,
    pub moving: MoveTuning,
    /// `modeStrafe`: StrafeMode's speed when the code moves him (`useCodeDrivenStrafing`;
    /// false for Bumblebee, whose weapon clips move him at the same 20), which is also what
    /// a landing into weapon mode caps his speed at.
    pub strafe_speed: f32,
    pub strafe_code_driven: bool,
    /// The weapon set (`WeaponSet`): the clips StrafeMode's variables pick between. Filled
    /// in from the character's animation set once that is loaded.
    pub weapon_set: RuleSet,
    /// All of the character's sets, which the base layer's controller walks: filled in from
    /// the animation set (`RuleSet::all`).
    pub rule_sets: HashMap<u32, RuleSet>,
    pub locomotion: Locomotion,
    pub jump: JumpTuning,
    pub drive: DriveTuning,
    pub climb: ClimbTuning,
    pub robot_camera: CameraTuning,
    pub drive_camera: CameraTuning,
    /// The camera of weapon mode (`weaponCamera`), and what the character adds to the
    /// height it looks at in that form (`cameraWeaponTargetHeightOffset`, 0.8).
    pub weapon_camera: CameraTuning,
    pub weapon_look_height: f32,
    pub aim: AimTuning,
    /// The distance levels of each, nearest first; the two above are level 0.
    pub robot_camera_levels: Vec<CameraTuning>,
    pub drive_camera_levels: Vec<CameraTuning>,
    pub weapon_camera_levels: Vec<CameraTuning>,
    /// Every camera shake the game has, by the CRC-32 of its name (`MassiveCameraShake`...).
    pub shakes: HashMap<u32, ShakeDef>,
    /// The pad's preset rumbles (`rumbleEffects`), by their number (`rumble::preset`).
    pub rumble_presets: Vec<Preset>,
    /// The control state machine (`stateMachineList`), and the clips its states play; the
    /// clips are filled in from the character's animation set once that is loaded.
    pub control: Graph,
    pub actions: Actions,
    /// Seconds after a dodge before the next (`modeDash.groundDashCooldown`).
    pub dash_cooldown: f32,
    /// How hard the ground punch pulls him down each update, and the most it may
    /// (`modeJump`'s values when it has them, else the shared `jumpGroundPunch*`).
    pub ground_punch_acceleration: f32,
    pub ground_punch_speed_limit: f32,
    /// What an explosion with `useGroundPoundDamageOverride` does when he owns it
    /// (`groundPoundDamage`, `groundPoundDamageWeak`; attributes `+0xf4`, `+0xf8`).
    pub ground_pound_damage: f32,
    pub ground_pound_damage_weak: f32,
    /// The special ability, and the seconds the car's weapon waits after the change into
    /// the car before it fires (`vehicleWeaponTimeTilCanFire`, attributes `+0xf0`).
    pub special: SpecialTuning,
    pub vehicle_weapon_wait: f32,
    /// What the attack mode's target search weighs by.
    pub melee: MeleeTuning,
    /// Values the pack did not have.
    pub missing: Vec<String>,
}

struct Reader<'a> {
    node: Option<Node<'a>>,
    prefix: &'static str,
    missing: &'a mut Vec<String>,
}

impl Reader<'_> {
    fn num(&mut self, name: &str) -> f32 {
        match self.node.and_then(|n| n.get(name)).and_then(Node::float) {
            Some(v) => v,
            None => {
                self.missing.push(format!("{}.{name}", self.prefix));
                0.0
            }
        }
    }
}

fn jump_kind(r: &mut Reader, stem: &str) -> JumpKind {
    JumpKind {
        height: r.num(&format!("{stem}Height")),
        speed: r.num(&format!("{stem}Speed")),
        control: r.num(&format!("{stem}Control")),
        turn_speed: r.num(&format!("{stem}TurnSpeed")),
    }
}

fn camera(node: Option<Node>, prefix: &'static str, missing: &mut Vec<String>) -> CameraTuning {
    // [game] Base/follow/drive/weapon loader defaults (0091f010/00920fb0/
    // 00913af0/00927cd0); installed fields override them.
    let opt = |name: &str, default: f32| node.and_then(|n| n.get(name)).and_then(Node::float).unwrap_or(default);
    if node.is_none() {missing.push(format!("{prefix} camera settings"));}
    CameraTuning {
        fov: opt("defaultFOV", 60.0),
        min_dist: opt("minDist", 12.0),
        max_dist: opt("maxDist", 15.0),
        min_elevation: opt("minElevation", 5.0),
        default_elevation: opt("defaultElev", 15.0),
        max_elevation: opt("maxElevation", 45.0),
        user_rot_speed_z: opt("userRotSpdZ", 180.0),
        user_rot_speed_x: opt("userRotSpdX", 180.0),
        // Only the driving cameras have one.
        max_speed_fov_scale: opt("maxSpdFOVScale", 75.0),
        face_extra_dist: opt("faceExtraDist", 0.0),
        face_extra_dist_gain: opt("faceExtraDistGain", 0.5).clamp(0.0,1.0),
        climb_extra_dist: opt("climbExtraDist", 0.0),
        scale_rot_speed_min: opt("scaleRotSpdMin", 80.0),
        scale_rot_speed_max: opt("scaleRotSpdMax", 120.0),
        min_height_gain: opt("minHeightGain", 0.02).clamp(0.0,1.0),
        interp_elev_gain: opt("interpElevGain", 0.05).clamp(0.0,1.0),
        interp_ang_gain: opt("interpAngGain", 0.075).clamp(0.0,1.0),
        interp_target_height_time: opt("interpTargetHeightTime", 0.0),
        height_offset: opt("heightOffset", 0.0),
        min_speed_align_move: opt("minSpdAlignMove", 0.5),
        max_speed_align_move: opt("maxSpdAlignMove", 10.0).max(opt("minSpdAlignMove",0.5)+2.0),
        align_gain: opt("alignGain", 0.02),
        align_damp: opt("alignDamp", 0.3),
        max_speed_dist_add: opt("maxSpdDistAdd", 5.0),
        user_rot_scale_max_speed: opt("userRotSclMaxSpeed", 100.0),
        user_rot_gain: opt("userRotGain", 0.02),
        user_rot_damp: opt("userRotDamp", 0.3),
        tilt_gain: opt("tiltGain", 0.25),
        tilt_amount: opt("tiltAmount", 85.0),
        lean_amount: opt("leanAmount", 50.0),
        lean_gain: opt("leanGain", 0.25),
        // [game] Weapon loader defaults and elevGain clamp.
        max_rot_speed: opt("maxRotSpd", 1080.0),
        side_offset: opt("sideOffset", 0.0),
        ahead_offset: opt("aheadOffset", 1.5),
        elev_gain: opt("elevGain", 0.2).clamp(0.001,1.0),
    }
}

/// A camera shake's settings (loader `FUN_0091f0f0`): the angles, and the angle chase's
/// limit, are turned to radians as the loader does.
fn shake(node: Node) -> ShakeDef {
    let defaults = ShakeDef::default(); // [game] 0091f0f0's omitted-field defaults.
    let num = |name: &str, default| node.get(name).and_then(Node::float).unwrap_or(default);
    ShakeDef {
        ang_horiz: num("shakeAngHoriz", 0.0).to_radians(),
        ang_vert: num("shakeAngVert", 0.0).to_radians(),
        tilt: num("shakeTilt", 0.0).to_radians(),
        pos_horiz: num("shakePosHoriz", defaults.pos_horiz),
        pos_vert: num("shakePosVert", defaults.pos_vert),
        full_dist: num("shakeFullDist", defaults.full_dist),
        max_dist: num("shakeMaxDist", defaults.max_dist),
        duration: num("shakeDuration", defaults.duration),
        half_duration: num("shakeHalfDuration", defaults.half_duration),
        max_ang_acc: num("shakeMaxAngAcc", 0.0).to_radians(),
        ang_gain: num("shakeAngGain", defaults.ang_gain),
        ang_damp: num("shakeAngDamp", defaults.ang_damp),
        max_offs_acc: num("shakeMaxOffsAcc", defaults.max_offs_acc),
        offs_gain: num("shakeOffsGain", defaults.offs_gain),
        offs_damp: num("shakeOffsDamp", defaults.offs_damp),
    }
}

impl Locomotion {
    /// Measures the on-foot speeds on the clips and takes the stick thresholds from the rules.
    pub fn from_animations(animations: &AnimSet, missing: &mut Vec<String>) -> Self {
        let mut clip = |set: &str, id: &str| {
            let found = animations.find(set, id);
            if found.is_none() {
                missing.push(format!("animation {set}/{id}"));
            }
            found.and_then(|r| r.clip.as_ref().map(|c| (r, c)))
        };
        let walk = clip("WalkSet", "Walk");
        let run = clip("WalkSet", "Run");
        let idle = clip("IdleStandSet", "Idle");
        let stop = clip("IdleStandSet", "Run2Idle");
        let land_run = clip("WalkSet", "Jump_LandRun");
        let big_land_run = clip("WalkSet", "BigJump_LandRun");
        let takeoff = clip("JumpSet", "Jump_Takeoff");
        let in_air = clip("JumpSet", "Jump_InAir");
        let fade = |c: Option<(&AnimRef, &Clip)>| c.map_or(0.0, |(r, _)| r.crossfade.max(0.0));
        let curve = |c: Option<(&AnimRef, &Clip)>| RootCurve::new(fade(c), c.map_or(Vec::new(), |(_, c)| c.root_forward()));
        let mut rule = |name: &str| {
            animations.rule_thresholds.get(name).copied().unwrap_or_else(|| {
                missing.push(format!("animation rule {name}"));
                0.0
            })
        };
        Self {
            walk_speed: walk.map_or(0.0, |(_, c)| c.forward_speed()),
            run_speed: run.map_or(0.0, |(_, c)| c.forward_speed()),
            move_stick: rule("animRuleMovement"),
            run_stick: rule("animRuleMovementRun"),
            idle_blend: fade(idle),
            walk_blend: fade(walk),
            run_blend: fade(run),
            stop: curve(stop),
            land_run: curve(land_run),
            big_land_run: curve(big_land_run),
            takeoff: curve(takeoff),
            in_air_speed: in_air.map_or(0.0, |(_, c)| c.forward_speed()),
            long_fall: rule("animRuleLongFall"),
        }
    }
}

/// Reads the tuning of one character, by the name its `characterAttributes` object goes by.
pub fn load(game_dir: &Path, character: &str) -> Result<Tuning, String> {
    let pack = Pack::open(&game_dir.join("bnxglobal.str"))?;
    let files: Vec<DataFile> =
        pack.of_type(pack::DATA).filter_map(|chunk| DataFile::parse(pack.data(chunk).to_vec()).ok()).collect();
    let root = |name: &str| files.iter().find_map(|file| file.root().get(name));
    // A root found by the hash the attributes store in place of its name.
    let root_by_hash = |hash: u32| files.iter().find_map(|file| file.root().field(hash));

    let attributes = root(&format!("characterAttributes{character}"))
        .ok_or_else(|| format!("bnxglobal.str has no characterAttributes{character}"))?;
    let shared = files.iter().find_map(|file| file.root().get("Character").filter(|c| c.get("gravity").is_some()));

    let mut missing = Vec::new();
    let mut t = Tuning { name: character.to_owned(), ..Default::default() };
    t.damage = crate::damage::Tuning::read(attributes, shared, root("Difficulty"));
    if let Some(multiplier)=root("OverDrive").and_then(|n|n.get("overDriveDamageMultiplier")).and_then(Node::float) {t.damage.overdrive_multiplier=multiplier;}
    {
        let mut r = Reader { node: Some(attributes), prefix: "attributes", missing: &mut missing };
        t.base_health = r.num("baseHealth");
        t.health_regen_rate = r.num("healthRegenRate");
        t.health_regen_delay = r.num("healthRegenDelay");
        t.aiming_height_percentage = r.num("aimingHeightPercentage");
    }
    // Two avatar descriptions: the robot's, then the vehicle's.
    for (index, prefix) in [(0, "avatar[robot]"), (1, "avatar[vehicle]")] {
        let node = attributes.get("avatarDescArray").and_then(|a| a.at(index));
        let mut r = Reader { node, prefix, missing: &mut missing };
        let (height, radius) = (r.num("height"), r.num("radius"));
        if index == 0 {
            (t.robot_height, t.robot_radius) = (height, radius);
        } else {
            (t.vehicle_height, t.vehicle_radius) = (height, radius);
            t.drive.body_half = glam::Vec3::new(r.num("width"), r.num("length"), height);
            t.drive.body_offset_z = r.num("offsetZ");
        }
    }
    {
        let mut r = Reader { node: shared, prefix: "Character", missing: &mut missing };
        t.gravity = r.num("gravity");
        t.transform_delay = r.num("advTransformDelay");
        t.melee = MeleeTuning {
            ang_range: r.num("autoTargetAngRange"),
            best_dist: r.num("autoTargetBestDist"),
            angle_weight: r.num("autoTrgAngleWeight"),
            dist_weight: r.num("autoTrgDistWeight"),
            match_weight: r.num("autoTrgMatchAtkWeight"),
            ..Default::default()
        };
    }
    // The three upgrades that scale a player's melee damage, by level. [data]
    match root("Upgrades").and_then(|u| u.get("upgradeParams")) {
        Some(params) => {
            for (i, kind) in melee::UPGRADES.into_iter().enumerate() {
                let Some(entry) = params.items().find(|p| p.get("characterUpgradeType").and_then(Node::int).map(|h| h as u32) == Some(kind)) else {
                    missing.push(format!("Upgrades.upgradeParams {kind:08x}"));
                    continue;
                };
                for info in entry.get("upgradeInfos").into_iter().flat_map(Node::items) {
                    let level = info.get("upgradeLevel").and_then(Node::int).unwrap_or(0);
                    if let Some(slot) = usize::try_from(level - 1).ok().and_then(|l| t.melee.upgrades[i].get_mut(l)) {
                        *slot = info.get("upgradeMultiplier").and_then(Node::float).unwrap_or(1.0);
                    }
                }
            }
        }
        None => missing.push("Upgrades.upgradeParams".into()),
    }
    {
        let mut r = Reader { node: Some(attributes), prefix: "attributes", missing: &mut missing };
        t.melee.distance = r.num("autoTargetMeleeDistance");
        t.melee.height = r.num("autoTargetMeleeHeight");
        t.ground_pound_damage = r.num("groundPoundDamage");
        t.ground_pound_damage_weak = r.num("groundPoundDamageWeak");
        t.vehicle_weapon_wait = r.num("vehicleWeaponTimeTilCanFire");
        // A script is sent only when its "self trigger" switch is on (`FUN_0072afd0`).
        let script = |switch: &str, name: &str| {
            let on = attributes.get(switch).is_some_and(|n| n.int() == Some(1) || n.is_true());
            attributes.get(name).and_then(Node::int).filter(|_| on).map_or(0, |h| h as u32)
        };
        t.special = SpecialTuning {
            cooldown: r.num("specialAbilityCooldown"),
            duration: r.num("specialAbilityDuration"),
            kind: attributes.get("specialAbilityType").and_then(Node::int).map_or(-1, |v| v as i32),
            icon: attributes.get("specialAbilityIcon").and_then(Node::int).unwrap_or(0) as u32,
            script: script("specialAbilitySelfTrigger", "specialAbilityLuxScriptNameToTrigger"),
            vehicle_script: script("specialAbilityVehicleSelfTrigger", "specialAbilityVehicleLuxScriptNameToTrigger"),
        };
    }
    {
        let mut r = Reader { node: attributes.get("modeMove"), prefix: "modeMove", missing: &mut missing };
        t.moving = MoveTuning {
            max_speed: r.num("maxSpeed"),
            turn_speed: r.num("turnSpeed"),
            walk_run_speed: r.num("walkRunSpeed"),
        };
    }
    {
        let node = attributes.get("modeStrafe");
        let mut r = Reader { node, prefix: "modeStrafe", missing: &mut missing };
        t.strafe_speed = r.num("strafeSpeed");
        t.strafe_code_driven = node.and_then(|n| n.get("useCodeDrivenStrafing")).is_some_and(Node::is_true);
    }
    {
        let mut r = Reader { node: attributes.get("modeJump"), prefix: "modeJump", missing: &mut missing };
        t.jump = JumpTuning {
            standing: jump_kind(&mut r, "jump"),
            run: jump_kind(&mut r, "jumpRun"),
            high_standing: jump_kind(&mut r, "jumpHighStanding"),
            long: jump_kind(&mut r, "jumpLong"),
            climb_up_height: r.num("climbJumpUpHeight"),
            climb_off_height: r.num("climbJumpOffHeight"),
            climb_off_speed: r.num("climbJumpOffSpeed"),
            weapon_dilation_start: r.num("jumpWeaponZVelStartDilation"),
            weapon_dilation: r.num("jumpWeaponDilation"),
            weapon_dilation_rate: r.num("jumpWeaponDilationAcceleration"),
        };
    }
    {
        let node = attributes.get("modeClimb");
        let mut r = Reader { node, prefix: "modeClimb", missing: &mut missing };
        t.climb = ClimbTuning {
            pull_up_dist: r.num("pullUpDist"),
            edge_limit_dist: r.num("edgeLimitDist"),
            wall_offset: r.num("wallOffset"),
            // Read by its hash; absent, the loader (`FUN_0085ad50`) gives 50. [game]
            arrival_deceleration: node.and_then(|n| n.field(0xbbc7_1449)).and_then(Node::float).unwrap_or(50.0),
        };
    }
    {
        let mut r = Reader { node: attributes.get("modeDrive"), prefix: "modeDrive", missing: &mut missing };
        t.drive = DriveTuning {
            max_speed: r.num("maxSpeed"),
            trigger_filter_strength: r.num("triggerFilterStrength"),
            turning_velocity_delta: r.num("turningVelocityDelta"),
            max_turbo_speed: r.num("maxTurboSpeed"),
            max_turbo_time: r.num("maxTurboTime"),
            max_turbo_cooldown_time: r.num("maxTurboCooldownTime"),
            turbo_upgrades: [[1.0; 3]; 2],
            throttle: r.num("throttle"),
            turbo_throttle_boost: r.num("turboThrottleBoost"),
            post_turbo_deceleration: r.num("postTurboDeceleration"),
            braking_deceleration: r.num("brakingDeceleration"),
            max_reverse_speed: r.num("maxReverseSpeed"),
            post_reverse_throttle: r.num("postReverseThrottle"),
            exit_to_run_deceleration: r.num("driveExitToRunDeceleration"),
            render_wheel_radius: r.num("renderWheelRadius"),
            turn_speed: r.num("turnSpeed"),
            max_turn_speed: r.num("maxTurnSpeed"),
            engine_torque: r.num("engineTorque"),
            wheel_friction: r.num("wheelFriction"),
            front_axle_length: r.num("frontAxleLength"),
            rear_axle_length: r.num("rearAxleLength"),
            long_axle_length: r.num("longAxleLength"),
            front_wheel_radius: r.num("frontWheelRadius"),
            rear_wheel_radius: r.num("rearWheelRadius"),
            max_drift_turbo_speed: r.num("maxDriftTurboSpeed"),
            min_reverse_brake_time: r.num("minReverseBrakeTime"),
            max_angular_vel_z: r.num("maxAngularVelZ"),
            max_angular_vel_x: r.num("maxAngularVelX"),
            max_angular_vel_y: r.num("maxAngularVelY"),
            exit_to_run_min_speed: r.num("driveExitToRunMinSpeed"),
            exit_to_run_turn_speed: r.num("driveExitToRunTurnSpeed"),
            exit_to_idle_deceleration: r.num("driveExitToIdleDeceleration"),
            exit_to_idle_min_speed: r.num("driveExitToIdleMinSpeed"),
            exit_to_idle_turn_speed: r.num("driveExitToIdleTurnSpeed"),
            wheel_drift_factor: r.num("wheelDriftFactor"),
            drift_steering_factor: r.num("driftSteeringFactor"),
            skid_out_deceleration: r.num("skidOutDeceleration"),
            min_drift_time: r.num("minDriftTime"),
            min_drift_turbo_speed: r.num("minDriftTurboSpeed"),
            drift_turbo_time: r.num("driftTurboTime"),
            drift_turbo_boost_factor: r.num("driftTurboBoostFactor"),
            suspension_length: r.num("suspensionLength"),
            suspension_k: r.num("suspensionK"),
            suspension_damp: r.num("suspensionDamp"),
            suspension_wt_scale: r.num("suspensionWTScale"),
            angular_vel_damp_x: r.num("angularVelDampX"),
            angular_vel_damp_y: r.num("angularVelDampY"),
            min_render_wheel_offset: r.num("minRenderWheelOffset"),
            max_render_wheel_offset: r.num("maxRenderWheelOffset"),
            body_half: t.drive.body_half,
            body_offset_z: t.drive.body_offset_z,
        };
        if let Some(params) = root("Upgrades").and_then(|u| u.get("upgradeParams")) {
            for (i, kind) in [0xf3bd_1a64u32, 0x9511_8079].into_iter().enumerate() {
                if let Some(entry) = params.items().find(|p| p.get("characterUpgradeType")
                    .and_then(Node::int).map(|v| v as u32) == Some(kind)) {
                    for info in entry.get("upgradeInfos").into_iter().flat_map(Node::items) {
                        let level = info.get("upgradeLevel").and_then(Node::int).unwrap_or(0);
                        if let Some(slot) = usize::try_from(level - 1).ok()
                            .and_then(|l| t.drive.turbo_upgrades[i].get_mut(l)) {
                            *slot = info.get("upgradeMultiplier").and_then(Node::float).unwrap_or(1.0);
                        }
                    }
                } else { missing.push(format!("Upgrades.upgradeParams {kind:08x}")); }
            }
        }
    }
    {
        let mut r = Reader { node: attributes.get("modeDash"), prefix: "modeDash", missing: &mut missing };
        t.dash_cooldown = r.num("groundDashCooldown");
    }
    {
        let tuning = files.iter().find_map(|file| {
            file.root().fields().into_iter().map(|(_, n)| n).find(|n| n.get("jumpGroundPunchAcceleration").is_some())
        });
        let mut r = Reader { node: tuning, prefix: "tuning", missing: &mut missing };
        t.ground_punch_acceleration = r.num("jumpGroundPunchAcceleration");
        t.ground_punch_speed_limit = r.num("jumpGroundPunchSpeedLimit");
    }
    // The state machines are in the same file as the attributes that name them.
    match files.iter().find(|file| file.root().get(&format!("characterAttributes{character}")).is_some()) {
        Some(file) => match attributes.get("stateMachineList") {
            Some(list) => t.control = Graph::read(file, list),
            None => missing.push("attributes.stateMachineList".into()),
        },
        None => missing.push("attributes.stateMachineList".into()),
    }
    let camera_named = |field: &str| attributes.get(field).and_then(Node::int).and_then(|hash| root_by_hash(hash as u32));
    t.robot_camera = camera(camera_named("normalCamera"), "normalCamera", &mut missing);
    t.drive_camera = camera(camera_named("drivingCamera"), "drivingCamera", &mut missing);
    t.weapon_camera = camera(camera_named("weaponCamera"), "weaponCamera", &mut missing);
    t.weapon_look_height = attributes.get("cameraWeaponTargetHeightOffset").and_then(Node::float).unwrap_or(0.0);
    {
        let assist = files.iter().find_map(|file| {
            file.root().fields().into_iter().map(|(_, n)| n).find(|n| n.get("aimWeaponFormAcceleration").is_some())
        });
        let mut r = Reader { node: assist, prefix: "AimAssist", missing: &mut missing };
        t.aim = AimTuning {
            lowest_sensitivity: r.num("aimWeaponFormLowestSensitivity"),
            highest_sensitivity: r.num("aimWeaponFormHighestSensitivity"),
            acceleration: r.num("aimWeaponFormAcceleration"),
            min_pitch: r.num("aimWeaponFormZDirMinAngle"),
            max_pitch: r.num("aimWeaponFormZDirMaxAngle"),
            speed_reduction: r.num("cameraSpeedReduction"),
            speed_acceleration: r.num("cameraSpeedAcceleration"),
            look_assist_turn_rate: r.num("lookAssistTurnRate"),
            target_box: [r.num("targetBoxPixelWidth"), r.num("targetBoxPixelHeight")],
            default_max_range: r.num("defaultMaxRange"),
            snap_cone: r.num("r2wLockOnConeAngle"),
            snap_cone_elevation: r.num("r2wLockOnConeElv"),
        };
    }
    // Each camera names the next distance level out (near, then two further), which the
    // player steps through in the original.
    let mut levels = |field: &'static str| {
        let mut found = Vec::new();
        let mut node = camera_named(field);
        while let Some(n) = node.filter(|_| found.len() < 3) {
            found.push(camera(Some(n), field, &mut missing));
            node = n.get("next").and_then(Node::int).and_then(|hash| root_by_hash(hash as u32));
        }
        found
    };
    t.robot_camera_levels = levels("normalCamera");
    t.drive_camera_levels = levels("drivingCamera");
    t.weapon_camera_levels = levels("weaponCamera");
    // The shakes are roots of the same file as the cameras; the game keeps them in a list
    // it searches by name (`DAT_00d434f4`, built by `FUN_0091ee00`), and a name not in it
    // starts nothing.
    for file in &files {
        for (name, node) in file.root().fields() {
            if node.get("shakeDuration").is_some() {
                t.shakes.insert(name, shake(node));
            }
        }
    }
    // The preset rumbles: each entry goes to the place its `id` says (`FUN_007dbed0`).
    match root("rumbleEffects") {
        Some(list) => {
            for entry in list.items() {
                let int = |name: &str| entry.get(name).and_then(Node::int).unwrap_or(0);
                let num = |name: &str| entry.get(name).and_then(Node::float).unwrap_or(0.0);
                let id = int("id") as usize;
                if t.rumble_presets.len() <= id {
                    t.rumble_presets.resize(id + 1, Preset::default());
                }
                t.rumble_presets[id] = Preset { kind: int("rumbleType") as u32, duration: num("duration"), strength: num("strength") };
            }
        }
        None => missing.push("rumbleEffects".into()),
    }
    t.drive_gears=crate::sound::load_gears(game_dir,character).unwrap_or_default();
    for file in &files {
        for (_,root) in file.root().fields() {
            for material in root.get("materials").into_iter().flat_map(Node::items) {
                if let Some(name)=material.get("name").and_then(Node::int) {
                    t.surface_hardness.insert(name as u32,material.field(0xb55f4fa0)
                        .and_then(Node::int).unwrap_or(0)!=0);
                }
            }
        }
    }
    t.missing = missing;
    Ok(t)
}
