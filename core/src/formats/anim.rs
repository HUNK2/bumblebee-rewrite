//! Animation clips (`AnimFile`) and the sets that group them.
//!
//! A clip names its channels (one per node, by the CRC-32 of the node's name) and holds several
//! streams. Each stream animates one property for some channels: its `frame` keys give channels
//! that never change, and its `keys` give the rest as `(track, time, value)`, where the track
//! number indexes `keyIndexMap` to find the channel. Times count sixtieths of a second.
//!
//! Key storage, by the class of the key list:
//!   AnimKeysVector3         three floats (position or scale)
//!   AnimKeysUnsignedShort3  three half floats (position or scale)
//!   AnimKeysVector4         a quaternion as four floats, x y z w
//!   AnimKeysUnsigned        a quaternion in 32 bits: three 10-bit components and, in the top
//!                           two bits, which component was left out because it is the largest
//!
//! The original plays keys as a stream and blends linearly between the two keys around the
//! current time (quaternions the short way round, then normalised). Sampling here gives the
//! same values at any time without the stream state.

use std::collections::HashMap;

use super::lxb::{DataFile, Node};
use crate::particles::Template;

/// Clip times are in these per second.
pub const TICKS_PER_SECOND: f32 = 60.0;

/// CRC-32 of `Root`, the channel that carries a clip's root motion tick by tick.
pub const ROOT_CHANNEL: u32 = 0xb6c6_5665;

/// Scale and offset of a packed quaternion component: ten bits spanning -1/sqrt(2) to 1/sqrt(2).
const PACKED_QUAT_SCALE: f32 = 0.001_383_77;
const PACKED_QUAT_OFFSET: f32 = 0.707_106_5;

/// Stream types whose three-component keys are positions; the others seen are scales.
const STREAM_POSITION: [i64; 2] = [0x00, 0x40];

#[derive(Default, Clone)]
pub struct Track<T> {
    times: Vec<f32>,
    values: Vec<T>,
}

#[derive(Default, Clone)]
pub struct Channel {
    pub position: Track<[f32; 3]>,
    pub rotation: Track<[f32; 4]>,
    pub scale: Track<[f32; 3]>,
}

#[derive(Default)]
pub struct Clip {
    /// Length in ticks.
    pub length: f32,
    /// Where the root starts and ends: three axis rows and a position row.
    pub start: [[f32; 3]; 4],
    pub end: [[f32; 3]; 4],
    /// CRC-32 of each channel's node name.
    pub names: Vec<u32>,
    pub channels: Vec<Channel>,
    /// Stream type and key class of every stream, for looking around.
    pub stream_kinds: Vec<(i64, String)>,
    /// When the clip shows or hides the character's two models, in time order.
    pub model_events: Vec<ModelEvent>,
    /// The sounds the clip asks for, in time order.
    pub sound_events: Vec<SoundEvent>,
    /// What the clip tells the control state machine and the attack mode, in time order.
    pub action_events: Vec<ActionEvent>,
    /// The camera shakes the clip starts, in time order.
    pub shake_events: Vec<ShakeEvent>,
    /// What the clip does to a weapon's object, and when the trigger counts, in time order.
    pub weapon_events: Vec<WeaponEvent>,
    /// The particle effects the clip starts and stops, in time order.
    pub effect_events: Vec<EffectEvent>,
    /// Named trigger messages, including timed and clip-exit stops for ribbon trails.
    pub trigger_events: Vec<crate::ribbon::TriggerEvent>,
}

/// An `AnimSpawnEffectEvent` or an `AnimStopEffectEvent` (loader `FUN_00747410`). The
/// event sits on a node's channel: the effect is started at that node. With neither an
/// `eventId` nor `trackObject` it is put into the world there once and left; otherwise it
/// is tied to the node and follows it, and with an `eventId` a later stop event of the
/// same id ends it. `useRotation` (on when the data does not give it) hands the node's
/// turn to it, `useScale` its scale. [game]
///
/// `autoStop` (off when absent): the clip's reference keeps the event's id in a list of its
/// own (`FUN_0070ccd0`, reference `+0x50`), [assumed] to stop those effects when the clip
/// is left (who walks the list is not read). `allowReRun`: see `EffectEvent::rerun`.
#[derive(Clone, Debug, PartialEq)]
pub struct EffectEvent {
    pub time: f32,
    /// The CRC-32 of the node's name.
    pub node: u32,
    pub kind: EffectEventKind,
    /// `allowReRun`, on when absent: off, the event runs only in a looping clip's first
    /// time round (`FUN_0053ab90`: an event is run if this is on or the loop's offset is
    /// 0). [game]
    pub rerun: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EffectEventKind {
    Spawn { id: u32, template: Template, track: bool, use_rotation: bool, use_scale: bool, auto_stop: bool },
    Stop { id: u32 },
    /// A `FootStepEvent` (handler `FUN_0073f780`): it tells his own object a foot came
    /// down. Its named FootStep attachment probes the ground before material FX.
    /// [game: 0073f520; data: side, heightFromGround, component node]
    FootStep { side: u32 },
    /// A `ClimbStepEvent`, with one of the five climbing `side`s: the `ClimbStep` thing of
    /// his with that side (`scene::ObjectDef::climb_steps`) finds the wall along its
    /// node (`FUN_0073fa20`) and the character's handler (`FUN_00718720`) starts the
    /// effect of the climb preset for that limb (`CLIMB_SIDES`) there. [game]
    ClimbStep { limb: usize },
}

/// The `side` names of a climbing step, in the order of the character's presets
/// `climbLeftHandFX`, `climbRightHandFX`, `climbLeftFootFX`, `climbRightFootFX`,
/// `climbSlideFX` (`+0x348` to `+0x358`; `FUN_00718720`). The names themselves are not
/// in the game's files. [game]
pub const CLIMB_SIDES: [u32; 5] = [0x85e6_0aab, 0x6509_1969, 0x6b0b_71b7, 0x8be4_6275, 0x85bb_2cd2];

/// What a clip tells the weapon manager. [game]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WeaponEvent {
    pub time: f32,
    /// The node whose channel the event sits on (CRC-32 of its name).
    pub node: u32,
    pub kind: WeaponEventKind,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WeaponEventKind {
    /// A `WeaponAttachmentEvent` (handler `FUN_007b4680`): the weapon of that name (or,
    /// with no name, at that place in the manager's list) is linked to the node the event
    /// sits on, hidden or shown, and its object starts `clip`; when that ends the object
    /// starts `then`, or is hidden.
    Attach {
        weapon: u32,
        index: i32,
        /// Only if the weapon is the one in hand.
        only_if_current: bool,
        hide: bool,
        unhide: bool,
        clip: u32,
        hide_when_done: bool,
        then: u32,
    },
    /// A `WeaponEnableFireEvent`: whether the trigger reaches the weapon from here on.
    EnableFire(bool),
}

/// An `AnimCameraShakeEvent` (handler `FUN_007d0ff0`): starts the camera shake of that
/// name, at the object's place if `positional` (the default) and wherever the camera is
/// if not. A name the game has no shake for starts nothing. [game]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShakeEvent {
    pub time: f32,
    /// CRC-32 of the shake's name.
    pub name: u32,
    pub positional: bool,
}

impl Clip {
    /// The shake events between two positions in ticks. `to` below `from` means the clip
    /// went round; a clip that has just started has `from` at 0 and its events at tick 0
    /// count.
    pub fn shakes_between(&self, from: f32, to: f32) -> impl Iterator<Item = &ShakeEvent> {
        self.shake_events.iter().filter(move |event| {
            let t = event.time;
            if to >= from { (t > from && t <= to) || (from == 0.0 && t == 0.0 && to > 0.0) } else { t > from || t <= to }
        })
    }
}

/// An event the control state machine or the attack mode listens for. [game]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    /// `AnimBranchPointEvent`: from here the clip may be left for another state.
    BranchPoint,
    /// `AttackButtonWindowEvent`: a press of the attack button counts toward the next hit
    /// while the window is open.
    ButtonWindow { open: bool },
    /// `AttackFaceAndSlideEvent`: what the attack mode may do from here on.
    FaceAndSlide { slide: bool, face: bool, stick: bool },
    /// `AttackCollEvent`: the hit lands while this is on. `node` is the CRC-32 of the name
    /// of the node whose channel the event sits on: the event's handler is handed that
    /// node (`FUN_0053adb0`), and it is the collision body on it that gets to hit.
    Hit { on: bool, node: u32 },
    /// `AnimSendTriggerEvent`: a trigger message to whatever on his object goes by this
    /// `LuxScriptName`: a spawner, an effect, a sound.
    Trigger { name: u32 },
}

#[derive(Clone, Copy, Debug)]
pub struct ActionEvent {
    pub time: f32,
    pub action: Action,
}

/// The numbers an attack clip's reference carries. Angles in degrees, as stored.
#[derive(Clone, Copy, Debug, Default)]
pub struct AttackRef {
    pub damage: f32,
    /// The damage in multiplayer and from a computer-run attacker; negative for "use `damage`".
    pub mp_damage: f32,
    pub ai_damage: f32,
    /// Which way the blow goes (`melee::attack_direction`).
    pub attack_dir: i32,
    /// Once the attack has hit a character, the stick no longer turns him.
    pub stop_on_hit: bool,
    /// `specialCaseDamageStuff`: the throw a hit gives, speed and degrees above the ground.
    pub knock_back_speed: f32,
    pub knock_back_angle: f32,
    pub slide_speed: f32,
    pub min_range: f32,
    pub max_range: f32,
    pub auto_target_turn_speed: f32,
    pub turn_speed: f32,
    pub stick_choose_target_max_degrees: f32,
    pub auto_target_search_angle: f32,
    /// The damage type flags a hit carries (`melee::damage`): `includeDamage`, the sets
    /// for small and for large characters, and `excludeDamage`. [data]
    pub include_damage: u32,
    pub include_damage_small: u32,
    pub include_damage_large: u32,
    pub exclude_damage: u32,
}

impl AttackRef {
    /// The type flags of a hit on a target (`FUN_007189b0`): for a character of size 0
    /// the small characters' set, of size 1 the large characters', for anything else
    /// `includeDamage`; less `excludeDamage`. [game]
    pub fn flags_for(&self, size: Option<u8>) -> u32 {
        let include = match size {
            Some(0) => self.include_damage_small,
            Some(1) => self.include_damage_large,
            _ => self.include_damage,
        };
        include & !self.exclude_damage
    }

    /// The damage a hit of this clip does (`FUN_00717c80`): the computer's value for an
    /// attacker no player runs, the multiplayer value in multiplayer, each only if it is not
    /// negative; otherwise `damage`. [game]
    pub fn damage_for(&self, player: bool, multiplayer: bool) -> f32 {
        if self.ai_damage >= 0.0 && !player {
            self.ai_damage
        } else if self.mp_damage >= 0.0 && multiplayer {
            self.mp_damage
        } else {
            self.damage
        }
    }
}

/// What a clip asks of the sound at one of its ticks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SoundCue {
    /// `AnimPlaySoundEvent`: a sound event by id; `auto_stop` ends it with the clip. It is
    /// on when the data does not give it (`FUN_0070ccd0`). [game]
    Play { id: u32, auto_stop: bool },
    /// `AnimStopSoundEvent`.
    Stop { id: u32 },
    /// `FootStepEvent`: named foot, with a material probe before its sound.
    FootStep { side: u32 },
    /// `ClimbStepEvent`: the limb's wall contact supplies the material sound.
    ClimbStep { limb: usize },
}

#[derive(Clone, Copy, Debug)]
pub struct SoundEvent {
    pub time: f32,
    pub cue: SoundCue,
    /// `allowReRun`, as `EffectEvent::rerun`.
    pub rerun: bool,
}

/// An `AlternateModelEvent`: a character's "true" model is the robot and its "alternate" model
/// the vehicle. Transform clips show both for a while and start a clip on the vehicle.
#[derive(Clone, Debug)]
pub struct ModelEvent {
    pub time: f32,
    pub show_alternate: bool,
    pub show_true: bool,
    /// Clip to start on the alternate model, and the one to follow it, by id.
    pub alternate_clip: Option<String>,
    pub alternate_then: Option<String>,
}

/// One test of an animation rule (`AnimGenericRule`): a variable by name hash, a comparison
/// (0 equal, 1 not equal, 2 greater, 3 greater or equal, 4 less, 5 less or equal) and the
/// value to compare with, of which the one of the variable's own type counts.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Rule {
    pub variable: u32,
    pub logic: u8,
    pub float: f32,
    pub hash: u32,
    pub int: i32,
}

/// When a clip hands over, and to which clip (0: whichever the set's entry rules pick) of
/// which set: once it has finished, or once its branch count (1 as it starts, one more for
/// each `AnimBranchPointEvent` passed, 100 when finished) has reached `branch_point`, and
/// only while every rule holds. The data stores the count as an enum of 0 or 2.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct ExitRule {
    pub to_set: u32,
    pub to_id: u32,
    pub finish: bool,
    pub branch_point: i32,
    pub rules: Vec<Rule>,
}

/// One of a set's entry rules: coming from this set and clip (0: any) and going to this
/// set and clip (0: any), while every rule holds, start on `to_id`.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct IncomingRule {
    pub from_set: u32,
    pub from_id: u32,
    /// Upper-layer entry filters for the currently playing base layer (0: any).
    pub base_set: u32,
    pub base_id: u32,
    pub to_set: u32,
    pub to_id: u32,
    pub rules: Vec<Rule>,
}

#[derive(Default)]
pub struct AnimRef {
    pub id: String,
    pub looping: bool,
    /// Seconds to blend in from whatever played before.
    pub crossfade: f32,
    pub speed: f32,
    pub clip: Option<Clip>,
    /// Present on clips that carry attack numbers (the combat set's).
    pub attack: Option<AttackRef>,
    /// Its exit rules, in stored order.
    pub exits: Vec<ExitRule>,
    /// `usePrevAnimFrame`: it starts at the time the clip before had reached.
    pub keeps_time: bool,
    /// Carry time overshot beyond this clip's end into the next clip.
    pub continue_time: bool,
    /// Let this clip's cues run while it is the outgoing side of a cross-fade.
    pub exit_events: bool,
    /// `xfadeTimeOnExit` (negative: none) and the clip it is for (0: any).
    pub exit_crossfade: f32,
    pub exit_crossfade_to: u32,
    /// `animPlayType` 1: the clip's keys are offsets laid over the pose under it (no
    /// change at rest: positions of zero, no turn), not poses of their own. [data: the
    /// recoil is no turn at tick 0 and a few degrees after; the hit reaction likewise]
    pub additive: bool,
    /// `additiveAnimFileList`: the offset clips laid over this one by how far the aim is
    /// from level, `Down` first and `Up` second (`lux::MetaAdditiveAnimation`, built by
    /// `FUN_00748f00`). Only the weapon set's clips have them. [data] [game]
    pub aim_offsets: Vec<Clip>,
}

#[derive(Default)]
pub struct AnimSet {
    pub sets: HashMap<String, Vec<AnimRef>>,
    /// The threshold each named rule compares its variable against (`animRuleMovementRun`...).
    pub rule_thresholds: HashMap<String, f32>,
    /// Each set's entry rules, in stored order.
    pub incoming: HashMap<String, Vec<IncomingRule>>,
}

fn half(bits: u16) -> f32 {
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exponent = ((bits >> 10) & 0x1f) as i32;
    let mantissa = (bits & 0x3ff) as f32;
    sign * match exponent {
        0 => mantissa * 2f32.powi(-24),
        31 => f32::INFINITY,
        _ => (1.0 + mantissa / 1024.0) * 2f32.powi(exponent - 15),
    }
}

fn unpack_quat(packed: u32) -> [f32; 4] {
    let part = |shift: u32| ((packed >> shift) & 0x3ff) as f32 * PACKED_QUAT_SCALE - PACKED_QUAT_OFFSET;
    let (a, b, c) = (part(20), part(10), part(0));
    let largest = (1.0 - (a * a + b * b + c * c)).max(0.0).sqrt();
    match packed >> 30 {
        0 => [largest, a, b, c],
        1 => [a, largest, b, c],
        2 => [a, b, largest, c],
        _ => [a, b, c, largest],
    }
}

pub trait Blend: Copy {
    fn blend(self, other: Self, t: f32) -> Self;
}

impl Blend for [f32; 3] {
    fn blend(self, other: Self, t: f32) -> Self {
        std::array::from_fn(|i| self[i] + (other[i] - self[i]) * t)
    }
}

impl Blend for [f32; 4] {
    fn blend(self, other: Self, t: f32) -> Self {
        let dot: f32 = (0..4).map(|i| self[i] * other[i]).sum();
        let other = if dot < 0.0 { other.map(|v| -v) } else { other };
        let mixed: [f32; 4] = std::array::from_fn(|i| self[i] + (other[i] - self[i]) * t);
        let length = mixed.iter().map(|v| v * v).sum::<f32>().sqrt();
        if length > 0.0 { mixed.map(|v| v / length) } else { mixed }
    }
}

impl<T: Blend> Track<T> {
    fn push(&mut self, time: f32, value: T) {
        // Keys arrive in the order the stream needs them, which is by time within one track.
        let at = self.times.partition_point(|&t| t <= time);
        self.times.insert(at, time);
        self.values.insert(at, value);
    }

    /// Value at `time` ticks. A looping clip blends from its last key round to its first.
    pub fn sample(&self, time: f32, length: f32, looping: bool) -> Option<T> {
        let last = self.times.len().checked_sub(1)?;
        let next = self.times.partition_point(|&t| t <= time);
        if next == 0 {
            return Some(self.values[0]);
        }
        let (from_time, from) = (self.times[next - 1], self.values[next - 1]);
        let (to_time, to) = if next <= last {
            (self.times[next], self.values[next])
        } else if looping && last > 0 {
            (length + self.times[0], self.values[0])
        } else {
            return Some(from);
        };
        let span = to_time - from_time;
        Some(if span > 0.0 { from.blend(to, ((time - from_time) / span).clamp(0.0, 1.0)) } else { to })
    }
}

fn matrix(node: Option<Node>) -> [[f32; 3]; 4] {
    let v = node.map(Node::floats).unwrap_or_default();
    std::array::from_fn(|row| std::array::from_fn(|col| v.get(row * 3 + col).copied().unwrap_or(0.0)))
}

fn read_model_events(file: &DataFile, keys: Node, out: &mut Vec<ModelEvent>) {
    for key in keys.get("array").into_iter().flat_map(Node::items) {
        let Some(event) = key.get("value").filter(|v| v.type_name() == Some("AlternateModelEvent")) else { continue };
        let clip = |field: &str| {
            let hash = event.get(field).and_then(Node::int).filter(|&h| h != 0)?;
            file.name(hash as u32).map(str::to_owned)
        };
        out.push(ModelEvent {
            time: key.get("time").and_then(Node::float).unwrap_or(0.0),
            show_alternate: event.get("showAlternateModel").and_then(Node::int) == Some(1),
            show_true: event.get("showTrueModel").and_then(Node::int) == Some(1),
            alternate_clip: clip("alternateAnimID"),
            alternate_then: clip("animToPlayOnAnimDoneID"),
        });
    }
}

fn read_sound_events(keys: Node, out: &mut Vec<SoundEvent>) {
    for key in keys.get("array").into_iter().flat_map(Node::items) {
        let Some(event) = key.get("value") else { continue };
        let id = || event.get("id").and_then(Node::int).map(|h| h as u32).filter(|&h| h != 0);
        let cue = match event.type_name() {
            Some("AnimPlaySoundEvent") => id().map(|id| SoundCue::Play { id, auto_stop: event.get("autoStop").and_then(Node::int) != Some(0) }),
            Some("AnimStopSoundEvent") => id().map(|id| SoundCue::Stop { id }),
            Some("FootStepEvent") => event.get("side").and_then(|n| n.enum_hash().or_else(|| n.int().map(|v| v as u32)))
                .map(|side| SoundCue::FootStep { side }),
            Some("ClimbStepEvent") => event.get("side").and_then(|n| n.enum_hash().or_else(|| n.int().map(|v| v as u32)))
                .and_then(|side| CLIMB_SIDES.iter().position(|&s| s == side))
                .map(|limb| SoundCue::ClimbStep { limb }),
            _ => None,
        };
        if let Some(cue) = cue {
            let rerun = event.get("allowReRun").and_then(Node::int) != Some(0);
            out.push(SoundEvent { time: key.get("time").and_then(Node::float).unwrap_or(0.0), cue, rerun });
        }
    }
}

fn read_shake_events(keys: Node, out: &mut Vec<ShakeEvent>) {
    for key in keys.get("array").into_iter().flat_map(Node::items) {
        let Some(event) = key.get("value").filter(|v| v.type_name() == Some("AnimCameraShakeEvent")) else { continue };
        out.push(ShakeEvent {
            time: key.get("time").and_then(Node::float).unwrap_or(0.0),
            name: event.get("name").and_then(Node::int).unwrap_or(0) as u32,
            // Absent, the handler takes it as on.
            positional: event.get("positional").and_then(Node::int).is_none_or(|v| v != 0),
        });
    }
}

fn read_weapon_events(keys: Node, node_of: impl Fn(usize) -> u32, out: &mut Vec<WeaponEvent>) {
    for key in keys.get("array").into_iter().flat_map(Node::items) {
        let Some(event) = key.get("value") else { continue };
        let on = |field: &str| event.get(field).and_then(Node::int).is_some_and(|v| v != 0);
        let kind = match event.type_name() {
            Some("WeaponAttachmentEvent") => WeaponEventKind::Attach {
                weapon: hash_of(event.get("weaponID")),
                index: event.get("weaponIndex").and_then(Node::int).unwrap_or(-1) as i32,
                only_if_current: on("useEventOnlyIfWeaponIsCurrentlyUsed"),
                hide: on("hide"),
                unhide: on("unhide"),
                clip: hash_of(event.get("animID")),
                hide_when_done: on("hideOnAnimDone"),
                then: hash_of(event.get("animToPlayOnAnimDoneID")),
            },
            Some("WeaponEnableFireEvent") => WeaponEventKind::EnableFire(on("allowFire")),
            _ => continue,
        };
        out.push(WeaponEvent {
            time: key.get("time").and_then(Node::float).unwrap_or(0.0),
            node: key.get("index").and_then(Node::int).map_or(0, |i| node_of(i as usize)),
            kind,
        });
    }
}

fn read_effect_events(keys: Node, node_of: impl Fn(usize) -> u32, out: &mut Vec<EffectEvent>) {
    for key in keys.get("array").into_iter().flat_map(Node::items) {
        let Some(event) = key.get("value") else { continue };
        let on = |field: &str, or: bool| event.get(field).and_then(Node::int).map_or(or, |v| v != 0);
        let id = hash_of(event.get("eventId"));
        let kind = match event.type_name() {
            Some("AnimSpawnEffectEvent") => {
                let Some(template) = event.get("effect").and_then(Template::read) else { continue };
                EffectEventKind::Spawn { id, template, track: on("trackObject", false), use_rotation: on("useRotation", true), use_scale: on("useScale", false), auto_stop: on("autoStop", false) }
            }
            Some("AnimStopEffectEvent") => EffectEventKind::Stop { id },
            Some("FootStepEvent") => {
                let Some(side) = event.get("side").and_then(|n| n.enum_hash().or_else(|| n.int().map(|v| v as u32))) else { continue };
                EffectEventKind::FootStep { side }
            }
            Some("ClimbStepEvent") => {
                let side = event.get("side").and_then(|side| side.enum_hash().or_else(|| side.int().map(|v| v as u32))).unwrap_or(0);
                match CLIMB_SIDES.iter().position(|&name| name == side) {
                    Some(limb) => EffectEventKind::ClimbStep { limb },
                    None => continue,
                }
            }
            _ => continue,
        };
        out.push(EffectEvent {
            time: key.get("time").and_then(Node::float).unwrap_or(0.0),
            node: key.get("index").and_then(Node::int).map_or(0, |i| node_of(i as usize)),
            kind,
            rerun: on("allowReRun", true),
        });
    }
}

/// `node_of` turns a key's index into the name hash of the node its channel belongs to.
fn read_action_events(keys: Node, node_of: impl Fn(usize) -> u32, out: &mut Vec<ActionEvent>) {
    for key in keys.get("array").into_iter().flat_map(Node::items) {
        let Some(event) = key.get("value") else { continue };
        let on = |field: &str| event.get(field).and_then(Node::int) == Some(1);
        let node = || key.get("index").and_then(Node::int).map_or(0, |i| node_of(i as usize));
        let action = match event.type_name() {
            Some("AnimBranchPointEvent") if event.get("animChannel").and_then(Node::int).unwrap_or(0) == 0 => Action::BranchPoint,
            Some("AttackButtonWindowEvent") => Action::ButtonWindow { open: on("start") },
            Some("AttackFaceAndSlideEvent") => {
                Action::FaceAndSlide { slide: on("allowSlide"), face: on("allowFace"), stick: on("allowStickMovement") }
            }
            Some("AttackCollEvent") => Action::Hit { on: on("attack"), node: node() },
            Some("AnimSendTriggerEvent") => Action::Trigger { name: event.get("LuxScriptName").and_then(Node::int).unwrap_or(0) as u32 },
            _ => continue,
        };
        out.push(ActionEvent { time: key.get("time").and_then(Node::float).unwrap_or(0.0), action });
    }
}

/// Reads one key list into the channels. `channel_of` turns a key's index into a channel.
fn read_keys(keys: Node, stream_type: i64, channels: &mut [Channel], channel_of: impl Fn(usize) -> Option<usize>) {
    let Some(class) = keys.type_name() else { return };
    if class == "AnimKeysEvent" {
        return;
    }
    let Some(array) = keys.get("array") else { return };
    let positions = STREAM_POSITION.contains(&stream_type);
    for key in array.items() {
        let fields = key.fields();
        let field = |name: &str| {
            let hash = super::hash::crc32(name.as_bytes());
            fields.iter().find(|f| f.0 == hash).map(|f| f.1)
        };
        let (Some(index), Some(time), Some(value)) = (field("index"), field("time"), field("value")) else { continue };
        let Some(channel) = index.int().and_then(|i| channel_of(i as usize)).and_then(|c| channels.get_mut(c)) else {
            continue;
        };
        let time = time.float().unwrap_or(0.0);
        match class {
            "AnimKeysVector3" | "AnimKeysUnsignedShort3" => {
                let v: Vec<f32> = if class == "AnimKeysVector3" {
                    value.floats()
                } else {
                    value.ints().into_iter().map(|bits| half(bits as u16)).collect()
                };
                let Ok(v) = <[f32; 3]>::try_from(v) else { continue };
                if positions { channel.position.push(time, v) } else { channel.scale.push(time, v) }
            }
            "AnimKeysVector4" => {
                if let Ok(q) = <[f32; 4]>::try_from(value.floats()) {
                    channel.rotation.push(time, q);
                }
            }
            "AnimKeysUnsigned" => {
                if let Some(packed) = value.int() {
                    channel.rotation.push(time, unpack_quat(packed as u32));
                }
            }
            // Events and float parameters are not motion.
            _ => {}
        }
    }
}

pub fn read_clip(data: &DataFile, file: Node) -> Option<Clip> {
    let names: Vec<u32> = file.get("names")?.ints().into_iter().map(|h| h as u32).collect();
    let mut channels = vec![Channel::default(); names.len()];
    let mut stream_kinds = Vec::new();
    let mut model_events = Vec::new();
    let mut sound_events = Vec::new();
    let mut action_events = Vec::new();
    let mut shake_events = Vec::new();
    let mut weapon_events = Vec::new();
    let mut effect_events = Vec::new();
    let mut trigger_events = Vec::new();
    for stream in file.get("streams")?.items() {
        let stream_type = stream.get("streamType").and_then(Node::int).unwrap_or(-1);
        let map: Vec<usize> = stream.get("keyIndexMap").map(Node::ints).unwrap_or_default().into_iter().map(|i| i as usize).collect();
        if let Some(constant) = stream.get("frame") {
            read_keys(constant, stream_type, &mut channels, Some);
        }
        if let Some(keys) = stream.get("keys") {
            // A stream with no key list only holds channels that never change.
            stream_kinds.push((stream_type, keys.type_name().unwrap_or("constants only").to_owned()));
            read_keys(keys, stream_type, &mut channels, |i| map.get(i).copied());
            if keys.type_name() == Some("AnimKeysEvent") {
                read_model_events(data, keys, &mut model_events);
                read_sound_events(keys, &mut sound_events);
                read_shake_events(keys, &mut shake_events);
                let node_of = |i: usize| map.get(i).and_then(|&channel| names.get(channel)).copied().unwrap_or(0);
                read_action_events(keys, node_of, &mut action_events);
                read_weapon_events(keys, node_of, &mut weapon_events);
                read_effect_events(keys, node_of, &mut effect_events);
                crate::ribbon::read_events(keys, &mut trigger_events);
            }
        }
    }
    model_events.sort_by(|a, b| a.time.total_cmp(&b.time));
    sound_events.sort_by(|a, b| a.time.total_cmp(&b.time));
    action_events.sort_by(|a, b| a.time.total_cmp(&b.time));
    shake_events.sort_by(|a, b| a.time.total_cmp(&b.time));
    weapon_events.sort_by(|a, b| a.time.total_cmp(&b.time));
    effect_events.sort_by(|a, b| a.time.total_cmp(&b.time));
    trigger_events.sort_by(|a, b| a.time.total_cmp(&b.time));
    Some(Clip {
        length: file.get("length")?.float()?,
        start: matrix(file.get("startMat")),
        end: matrix(file.get("endMat")),
        names,
        channels,
        stream_kinds,
        model_events,
        sound_events,
        action_events,
        shake_events,
        weapon_events,
        effect_events,
        trigger_events,
    })
}

fn hash_of(node: Option<Node>) -> u32 {
    node.and_then(Node::int).map_or(0, |h| h as u32)
}

/// The rules an entry or exit rule holds: its own `rules` and those of its `ruleList`.
fn read_rules(holder: Node) -> Vec<Rule> {
    let own = holder.get("rules").into_iter().flat_map(Node::items);
    let listed = holder.path(&["ruleList", "rules"]).into_iter().flat_map(Node::items);
    own.chain(listed)
        .map(|rule| Rule {
            variable: hash_of(rule.get("rule")),
            logic: rule.get("logic").and_then(Node::int).unwrap_or(0) as u8,
            float: rule.get("floatValue").and_then(Node::float).unwrap_or(0.0),
            hash: hash_of(rule.get("crcValue")),
            int: rule.get("intValue").and_then(Node::int).unwrap_or(0) as i32,
        })
        .collect()
}

fn read_exits(r: Node) -> Vec<ExitRule> {
    let entries = r.path(&["animExitRules", "exitRules"]).into_iter().flat_map(Node::items);
    entries
        .filter_map(|entry| {
            let exit = entry.get("exitRule")?;
            Some(ExitRule {
                to_set: hash_of(entry.get("toAnimSet")),
                to_id: hash_of(entry.get("toAnimID")),
                finish: exit.get("finishAnim").is_some_and(|f| f.int() == Some(1) || f.is_true()),
                branch_point: exit.get("animBranchPoint").and_then(Node::int).unwrap_or(0) as i32,
                rules: read_rules(exit),
            })
        })
        .collect()
}

fn read_incoming(set: Node) -> Vec<IncomingRule> {
    // A set of a layer above the base one keeps its entries under another name, with the
    // from-filters named for that layer and a second pair for the base layer. [data]
    let partial = set.path(&["animPartialIncomingSetRules", "incRules"]);
    let (entries, from_set, from_id) = match partial {
        Some(entries) => (Some(entries), "fromAnimPartialSet", "fromAnimPartialID"),
        None => (set.path(&["animIncomingSetRules", "incRules"]), "fromAnimSet", "fromAnimID"),
    };
    entries
        .into_iter()
        .flat_map(Node::items)
        .filter_map(|entry| {
            let inc = entry.get("incRule")?;
            Some(IncomingRule {
                from_set: hash_of(inc.get(from_set)),
                from_id: hash_of(inc.get(from_id)),
                base_set: hash_of(inc.get("currAnimBaseSet")),
                base_id: hash_of(inc.get("currAnimBaseID")),
                to_set: hash_of(entry.get("toAnimSet")),
                to_id: hash_of(entry.get("toAnimID")),
                rules: read_rules(inc),
            })
        })
        .collect()
}

fn read_ref(file: &DataFile, r: Node) -> AnimRef {
    AnimRef {
        exits: read_exits(r),
        keeps_time: r.get("usePrevAnimFrame").is_some_and(|f| f.int() == Some(1) || f.is_true()),
        continue_time: r.get("continueTimeOnExit").is_some_and(|f| f.int() == Some(1) || f.is_true()),
        exit_events: r.get("allowEventsOnBlendFromThisAnim").is_some_and(|f| f.int() == Some(1) || f.is_true()),
        exit_crossfade: r.get("xfadeTimeOnExit").and_then(Node::float).unwrap_or(-1.0),
        exit_crossfade_to: hash_of(r.get("xfadeTimeOnExitToAnimID")),
        additive: r.get("animPlayType").and_then(Node::int) == Some(1),
        aim_offsets: r.get("additiveAnimFileList").into_iter().flat_map(Node::items).filter_map(|clip| read_clip(file, clip)).collect(),
        id: r.get("animID").and_then(Node::int).and_then(|h| file.name(h as u32)).unwrap_or("?").to_owned(),
        // Sets store these flags as 0 or 1; a vehicle's bundle stores them as true or false.
        looping: r.get("loop").is_some_and(|l| l.int() == Some(1) || l.is_true()),
        crossfade: r.get("xfadeTime").and_then(Node::float).unwrap_or(0.0),
        speed: r.get("animSpeed").and_then(Node::float).unwrap_or(1.0),
        clip: r.get("animFile").and_then(|clip| read_clip(file, clip)),
        attack: r.get("slideSpeed").map(|_| {
            let num = |name: &str| r.get(name).and_then(Node::float).unwrap_or(0.0);
            let mask = |name: &str| r.get(name).and_then(Node::int).unwrap_or(0) as u32;
            let special = |name: &str| {
                r.get("specialCaseDamageStuff").and_then(|s| s.get(name)).and_then(Node::float).unwrap_or(0.0)
            };
            AttackRef {
                damage: num("damage"),
                mp_damage: num("MP_damage"),
                ai_damage: num("AI_damage"),
                attack_dir: r.get("attack_dir").and_then(Node::int).unwrap_or(0) as i32,
                stop_on_hit: r.get("stop_on_hit").and_then(Node::int) == Some(1),
                knock_back_speed: special("knockBackSpeed"),
                knock_back_angle: special("knockBackAngle"),
                slide_speed: num("slideSpeed"),
                min_range: num("minRange"),
                max_range: num("maxRange"),
                auto_target_turn_speed: num("auto_target_turn_speed"),
                turn_speed: num("turn_speed"),
                stick_choose_target_max_degrees: num("stick_choose_target_max_degrees"),
                auto_target_search_angle: num("auto_target_search_angle"),
                include_damage: mask("includeDamage"),
                include_damage_small: mask("includeDamageOnSmallChars"),
                include_damage_large: mask("includeDamageOnLargeChars"),
                exclude_damage: mask("excludeDamage"),
            }
        }),
    }
}

/// Reads an `AnimRefBundle`, the clip list an object such as the vehicle carries for itself.
pub fn read_bundle(file: &DataFile, bundle: Node) -> Vec<AnimRef> {
    bundle.get("anims").into_iter().flat_map(Node::items).map(|r| read_ref(file, r)).collect()
}

/// Reads every set of an animation set (the embedded file a `Character` calls `animSet`).
pub fn read_set(file: &DataFile, anim_set: Node) -> AnimSet {
    let mut sets = HashMap::new();
    let mut rule_thresholds = HashMap::new();
    let mut incoming = HashMap::new();
    for (hash, member) in anim_set.fields() {
        let Some(name) = file.name(hash) else { continue };
        if let (Some(_), Some(threshold)) = (member.get("rule"), member.get("floatValue").and_then(Node::float)) {
            rule_thresholds.insert(name.to_owned(), threshold);
            continue;
        }
        let Some(refs) = member.get("animRef") else { continue };
        sets.insert(name.to_owned(), refs.items().map(|r| read_ref(file, r)).collect());
        incoming.insert(name.to_owned(), read_incoming(member));
    }
    AnimSet { sets, rule_thresholds, incoming }
}

impl AnimSet {
    pub fn find(&self, set: &str, id: &str) -> Option<&AnimRef> {
        self.sets.get(set)?.iter().find(|r| r.id == id && r.clip.is_some())
    }
}

impl Clip {
    /// Average speed of the root over the clip, forward (y) per second.
    pub fn forward_speed(&self) -> f32 {
        (self.end[3][1] - self.start[3][1]) / (self.length / TICKS_PER_SECOND)
    }

    /// How far forward the root has got at each tick, from 0 to the clip's length, starting
    /// at zero. Clips that change pace carry it as the `Root` channel's position; the others
    /// move evenly from `start` to `end`.
    pub fn root_forward(&self) -> Vec<f32> {
        let ticks = self.length.round().max(1.0) as usize;
        let keyed = self.channel(ROOT_CHANNEL).filter(|c| c.position.sample(0.0, self.length, false).is_some());
        let first = keyed.and_then(|c| c.position.sample(0.0, self.length, false)).map_or(0.0, |p| p[1]);
        (0..=ticks)
            .map(|tick| match keyed.and_then(|c| c.position.sample(tick as f32, self.length, false)) {
                Some(p) => p[1] - first,
                None => (self.end[3][1] - self.start[3][1]) * tick as f32 / ticks as f32,
            })
            .collect()
    }

    /// Where the root has got to at each tick, from 0 to the clip's length, starting at zero
    /// and measured in the root's own frame at the start: x right, y forward, z up. The
    /// sideways dodges are made travelling along y with the root turned a quarter turn, so
    /// taking the turn out is what makes them go sideways. [data]
    pub fn root_path(&self) -> Vec<glam::Vec3> {
        let ticks = self.length.round().max(1.0) as usize;
        let root = self.channel(ROOT_CHANNEL);
        let keyed = root.filter(|c| c.position.sample(0.0, self.length, false).is_some());
        let first = keyed.and_then(|c| c.position.sample(0.0, self.length, false)).map_or(glam::Vec3::ZERO, glam::Vec3::from);
        // Stored for row vectors, so used as it is here it undoes the root's turn.
        let undo = root
            .and_then(|c| c.rotation.sample(0.0, self.length, false))
            .map_or(glam::Quat::IDENTITY, |q| glam::Quat::from_xyzw(q[0], q[1], q[2], q[3]).normalize());
        let (start, end) = (glam::Vec3::from(self.start[3]), glam::Vec3::from(self.end[3]));
        (0..=ticks)
            .map(|tick| match keyed.and_then(|c| c.position.sample(tick as f32, self.length, false)) {
                Some(p) => undo * (glam::Vec3::from(p) - first),
                None => undo * ((end - start) * tick as f32 / ticks as f32),
            })
            .collect()
    }

    /// How far the root has turned about the vertical at each tick, from 0 to the clip's
    /// length, in radians from where it started, positive toward -x (as `sim`'s yaw). Most
    /// clips do not turn; `Wall_JumpOff` turns 124 degrees. [data]
    pub fn root_turn(&self) -> Vec<f32> {
        let ticks = self.length.round().max(1.0) as usize;
        let Some(root) = self.channel(ROOT_CHANNEL) else { return vec![0.0; ticks + 1] };
        // Stored for row vectors: the conjugate is the turn for column vectors.
        let at = |tick: f32| {
            root.rotation
                .sample(tick, self.length, false)
                .map(|q| glam::Quat::from_xyzw(q[0], q[1], q[2], q[3]).normalize().conjugate())
        };
        let Some(first) = at(0.0) else { return vec![0.0; ticks + 1] };
        (0..=ticks)
            .map(|tick| {
                let turned = first.inverse() * at(tick as f32).unwrap_or(first);
                let forward = turned * glam::Vec3::Y;
                (-forward.x).atan2(forward.y)
            })
            .collect()
    }

    pub fn channel(&self, name_hash: u32) -> Option<&Channel> {
        self.names.iter().position(|&n| n == name_hash).map(|i| &self.channels[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats_decode() {
        assert_eq!(half(0x3c00), 1.0);
        assert_eq!(half(0xc000), -2.0);
        assert_eq!(half(0), 0.0);
    }

    #[test]
    fn packed_quaternions_are_unit_length() {
        for packed in [0xDFF1_15FFu32, 0x5EF5_E90B, 0xE00F_5DFE] {
            let q = unpack_quat(packed);
            let length: f32 = q.iter().map(|v| v * v).sum::<f32>().sqrt();
            assert!((length - 1.0).abs() < 1e-3, "{packed:#x} gives {q:?}");
        }
    }

    #[test]
    fn looping_track_blends_back_to_its_first_key() {
        let mut track = Track::default();
        track.push(0.0, [0.0, 0.0, 0.0]);
        track.push(10.0, [10.0, 0.0, 0.0]);
        assert_eq!(track.sample(5.0, 20.0, true), Some([5.0, 0.0, 0.0]));
        assert_eq!(track.sample(15.0, 20.0, true), Some([5.0, 0.0, 0.0]));
        assert_eq!(track.sample(15.0, 20.0, false), Some([10.0, 0.0, 0.0]));
    }
}
