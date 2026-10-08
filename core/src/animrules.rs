//! The animation rules: how the game picks the clip a set starts on and when a playing clip
//! hands over to another. The data is the character's own (`formats::anim`: each set's entry
//! rules, each clip's exit rules); the two walks below are the game's.
//!
//!   [game]     read from the game's code (`FUN_0070acc0` and `FUN_0070b710` for entering a
//!              set, `FUN_0070d540` for leaving a clip, `FUN_00710650` and its two siblings
//!              for the comparisons, `FUN_0084e3c0` and `FUN_0084ddb0` for the order)
//!   [assumed]  our reading of something not checked
//!
//! Each update the controller forgets the variables it worked out, then for each of its
//! three layers that has a clip walks that clip's exit rules; the first that fits names a set
//! and a clip. With no clip named, the set's entry rules are walked with the set and clip
//! being left; the first that fits names the clip (and can name another set, whose entry
//! rules are then walked in turn). Nothing fits: nothing changes. [game]

use std::collections::HashMap;

use crate::formats::anim::{ExitRule, IncomingRule, Rule, TICKS_PER_SECOND};
use crate::formats::hash::crc32;
use crate::tuning::{RuleClip, RuleSet};

/// The variables the rules of Bumblebee's sets ask for, by the CRC-32 of their names.
pub mod var {
    use super::crc32;

    pub const L_STICK_AMP: u32 = crc32(b"F_L_STICK_AMP");
    pub const L_STICK_DIR: u32 = crc32(b"F_L_STICK_DIR");
    pub const STRAFE_ANG: u32 = crc32(b"F_STRAFE_ANG");
    pub const FALL_DUR: u32 = crc32(b"F_FALL_DUR");
    pub const Z_VEL: u32 = crc32(b"F_Z_VEL");
    pub const TIME_IN_ANIM: u32 = crc32(b"F_TIME_IN_ANIM");
    pub const IS_STRAFING: u32 = crc32(b"I_IS_STRAFING");
    pub const IN_AIR: u32 = crc32(b"I_IN_AIR");
    pub const ON_GROUND: u32 = crc32(b"I_ON_GROUND");
    pub const WEAPON_INDEX: u32 = crc32(b"I_WEAPON_INDEX");
    pub const JUMP_TYPE: u32 = crc32(b"C_JUMP_TYPE");
    pub const WEAPON_ACTION: u32 = crc32(b"C_WEAPON_ACTION");
    pub const WEAPON_EQUIP: u32 = crc32(b"C_WEAPON_EQUIP");
    pub const CHAR_MESSAGE: u32 = crc32(b"C_CHAR_MESSAGE");
    pub const INTERACT_TYPE: u32 = crc32(b"C_INTERACT_TYPE");
    pub const SPECIAL_IDLE_ON: u32 = crc32(b"I_SPECIAL_IDLE_ON");
    pub const TIME_SINCE_DAMAGED: u32 = crc32(b"F_TIME_SINCE_DAMAGED");
    pub const CHAR_FORM: u32 = crc32(b"I_CHAR_FORM");
    pub const DAMAGE_TYPE: u32 = crc32(b"C_DAMAGE_TYPE");
}

/// A variable's value. Its kind decides which of a rule's three values it is compared with.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Value {
    Float(f32),
    Int(i32),
    Hash(u32),
}

fn compare<T: PartialOrd>(logic: u8, variable: T, value: T) -> bool {
    match logic {
        1 => variable != value,
        2 => variable > value,
        3 => variable >= value,
        4 => variable < value,
        5 => variable <= value,
        _ => variable == value,
    }
}

/// Whether one rule holds. A variable nobody has registered compares as the float 0. [game]
pub fn holds(rule: &Rule, value: Option<Value>) -> bool {
    match value.unwrap_or(Value::Float(0.0)) {
        Value::Float(v) => compare(rule.logic, v, rule.float),
        Value::Int(v) => compare(rule.logic, v, rule.int),
        Value::Hash(v) => compare(rule.logic, v, rule.hash),
    }
}

/// The first exit rule of a playing clip that fits: the clip has finished where the rule
/// asks for that, otherwise its branch count has reached the rule's branch point; and every
/// rule holds. `branch` is the count the animation player keeps for the layer (`+0xdc`): 1
/// as a clip starts, one more for each `AnimBranchPointEvent` passed; a finished clip
/// counts as 100. It is the count the control state machine's connections test too. [game]
pub fn exit<'a>(exits: &'a [ExitRule], branch: i32, finished: bool, get: &dyn Fn(u32) -> Option<Value>) -> Option<&'a ExitRule> {
    exits.iter().find(|exit| {
        let timed = if exit.finish { finished } else { exit.branch_point <= if finished { 100 } else { branch } };
        timed && exit.rules.iter().all(|rule| holds(rule, get(rule.variable)))
    })
}

/// The set and clip the first fitting entry rule of a set names, coming from `from_set` and
/// `from_id` on the same layer (an entry's filter that is 0 fits anything). [game]
pub fn entry(incoming: &[IncomingRule], from_set: u32, from_id: u32, get: &dyn Fn(u32) -> Option<Value>) -> Option<(u32, u32)> {
    entry_partial(incoming, (from_set, from_id), (0, 0), get)
}

/// Upper-layer entry walk (`FUN_0070b710`), including its current-base filters. [game]
pub fn entry_partial(incoming: &[IncomingRule], from: (u32, u32), base: (u32, u32), get: &dyn Fn(u32) -> Option<Value>) -> Option<(u32, u32)> {
    incoming
        .iter()
        .find(|inc| {
            (inc.from_set == 0 || inc.from_set == from.0)
                && (inc.from_id == 0 || inc.from_id == from.1)
                && (inc.base_set == 0 || inc.base_set == base.0)
                && (inc.base_id == 0 || inc.base_id == base.1)
                && inc.rules.iter().all(|rule| holds(rule, get(rule.variable)))
        })
        .map(|inc| (inc.to_set, inc.to_id))
}

/// Blend time for a clip whose own is negative, which the data uses for "none given"
/// (`FUN_00748f00` takes 0.1 for any blend time under 0). [game]
pub const DEFAULT_CROSSFADE: f32 = 0.1;
pub const EMPTY_SET: u32 = crc32(b"emptyAnimSet");

/// How many sets an entry walk may pass on through (a result that names no clip walks that
/// set's entries in turn). [assumed: the bound, which only guards against a loop in the data]
const NESTING: usize = 4;

/// The base layer's controller for the sets other than the weapon set (idle, walk, jump, fall
/// and what leads into them): the clip that plays, picked by the sets' own rules. A control
/// state that starts a set walks its entry rules coming from the clip that was playing
/// (`enter`, `FUN_0084ddb0` with no clip named); each update the playing clip's exit rules
/// are walked and the first that fits names the next clip (`update`, `FUN_0084e3c0`). [game]
/// The variables the rules read are the caller's.
#[derive(Clone, Debug, Default)]
pub struct Base {
    /// The set and clip playing, by the CRC-32 of their names (0: none yet), and the tick.
    pub set: u32,
    pub clip: u32,
    pub tick: f32,
    /// Player time before wrapping a looping clip's sampling phase. [game: player +e0]
    pub elapsed_ticks: f32,
    /// Time overshot beyond a non-looping clip's end (+e4), for continueTimeOnExit.
    pub overrun: f32,
    /// Seconds the clip fades in over: its own `xfadeTime`, or the one the clip before it asked
    /// for on its way out where that names it (`xfadeTimeOnExit`). [game]
    pub blend: f32,
    /// Counts the clips started, so a clip started again is told from one still playing.
    pub serial: u32,
}

impl Base {
    pub fn playing(&self) -> Option<(u32, u32)> {
        (self.set != 0 && self.clip != 0).then_some((self.set, self.clip))
    }

    /// The playing clip's time in seconds (`F_TIME_IN_ANIM`).
    pub fn seconds(&self) -> f32 {
        self.elapsed_ticks / TICKS_PER_SECOND
    }

    pub fn current<'a>(&self, sets: &'a HashMap<u32, RuleSet>) -> Option<&'a RuleClip> {
        sets.get(&self.set)?.clip(self.clip)
    }

    /// Starts `set`: its entry rules are walked, coming from the set and clip `from`, and the
    /// first that fits gives the clip (or another set, whose entries are walked in turn).
    /// Nothing fits: nothing changes. Says whether a clip started. [game]
    pub fn enter(&mut self, sets: &HashMap<u32, RuleSet>, set: u32, from: (u32, u32), get: &dyn Fn(u32) -> Option<Value>) -> bool {
        self.enter_partial(sets, set, from, (0, 0), get)
    }

    pub fn enter_partial(&mut self, sets: &HashMap<u32, RuleSet>, set: u32, from: (u32, u32), base: (u32, u32), get: &dyn Fn(u32) -> Option<Value>) -> bool {
        let mut set = set;
        for _ in 0..NESTING {
            if set == EMPTY_SET {
                *self = Self { serial: self.serial.wrapping_add(1), ..Self::default() };
                return true;
            }
            let Some(rules) = sets.get(&set) else { return false };
            let Some((to_set, to_id)) = entry_partial(&rules.incoming, from, base, get) else { return false };
            let to_set = if to_set == 0 { set } else { to_set };
            if to_id != 0 {
                // The clip already playing is not started again. [assumed: the weapon set's
                // entry walk does the same, and run 1 does not show it either way]
                if (to_set, to_id) == (self.set, self.clip) {
                    return false;
                }
                return self.start(sets, to_set, to_id, from);
            }
            set = to_set;
        }
        false
    }

    /// One update of the controller: the playing clip's exit rules. The first that fits names
    /// a clip, or none, which walks the named set's entry rules from this clip. [game]
    pub fn update(&mut self, sets: &HashMap<u32, RuleSet>, get: &dyn Fn(u32) -> Option<Value>) {
        self.update_partial(sets, (0, 0), get);
    }

    pub fn update_partial(&mut self, sets: &HashMap<u32, RuleSet>, base: (u32, u32), get: &dyn Fn(u32) -> Option<Value>) {
        let Some(clip) = self.current(sets) else { return };
        let finished = !clip.looping && self.tick >= clip.ticks;
        let Some(exit) = exit(&clip.exits, clip.branch(self.tick), finished, get) else { return };
        let to_set = if exit.to_set == 0 { self.set } else { exit.to_set };
        let from = (self.set, self.clip);
        if exit.to_id == 0 {
            self.enter_partial(sets, to_set, from, base, get);
        } else if (to_set, exit.to_id) != from {
            self.start(sets, to_set, exit.to_id, from);
        }
    }

    /// Plays the clip on by `dt` seconds at its own speed: a loop carries on round, any other
    /// stops at its end (`finished` for the exit rules).
    pub fn advance(&mut self, sets: &HashMap<u32, RuleSet>, dt: f32) {
        let Some(clip) = self.current(sets) else { return };
        let delta = dt * TICKS_PER_SECOND * clip.speed;
        let to = self.tick + delta;
        if !clip.looping {
            self.overrun += (to - clip.ticks).max(0.0);
        }
        self.elapsed_ticks = if clip.looping { self.elapsed_ticks + delta } else { (self.elapsed_ticks + delta).min(clip.ticks) };
        self.tick = if clip.looping { to % clip.ticks.max(1.0) } else { to.min(clip.ticks) };
    }

    /// Starts a clip: it fades in over its own blend time, or over the one the clip it comes
    /// from asked for where that names it; a clip with `usePrevAnimFrame` starts at the time
    /// the clip before had reached. [game: as the weapon set's clips start in `sim`]
    fn start(&mut self, sets: &HashMap<u32, RuleSet>, set: u32, id: u32, from: (u32, u32)) -> bool {
        let Some(clip) = sets.get(&set).and_then(|rules| rules.clip(id)) else { return false };
        let old = sets.get(&from.0).and_then(|rules| rules.clip(from.1));
        let handed = old.filter(|old| old.exit_crossfade >= 0.0 && (old.exit_crossfade_to == 0 || old.exit_crossfade_to == id));
        self.blend = handed.map_or(if clip.crossfade < 0.0 { DEFAULT_CROSSFADE } else { clip.crossfade }, |old| old.exit_crossfade);
        let elapsed = if clip.keeps_time && from == (self.set, self.clip) {
            self.elapsed_ticks
        } else if old.is_some_and(|old| old.continue_time) {
            self.overrun * clip.speed
        } else { 0.0 };
        let tick = if clip.looping { elapsed % clip.ticks.max(1.0) } else { elapsed.min(clip.ticks) };
        (self.set, self.clip, self.tick) = (set, id, tick);
        self.elapsed_ticks = if clip.looping { elapsed } else { elapsed.min(clip.ticks) };
        self.overrun = if clip.looping { 0.0 } else { (elapsed - clip.ticks).max(0.0) };
        self.serial = self.serial.wrapping_add(1);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_entries_filter_the_base_and_empty_set_removes_the_layer() {
        let data = crate::character::load(std::path::Path::new("C:/Games2"), "bumblebee").unwrap();
        let sets = RuleSet::all(&data.animations);
        let hit = crc32(b"HitReactPartialSet");
        let get = |v| (v == var::CHAR_FORM).then_some(Value::Int(0));
        let mut layer = Base::default();
        assert!(layer.enter_partial(&sets, hit, (EMPTY_SET, 0), (crc32(b"WalkSet"), crc32(b"Run")), &get));
        assert_eq!(layer.clip, crc32(b"HitFront"));
        layer.advance(&sets, 0.4);
        layer.update_partial(&sets, (crc32(b"WalkSet"), crc32(b"Run")), &get);
        assert!(layer.playing().is_none());
        // The second incoming rule suppresses this layer while a melee clip plays.
        layer.enter_partial(&sets, hit, (EMPTY_SET, 0), (crc32(b"CombatSet"), 0), &get);
        assert!(layer.playing().is_none());
        layer.enter_partial(&sets, hit, (EMPTY_SET, 0), (crc32(b"WalkSet"), 0), &|v| {
            (v == var::CHAR_FORM).then_some(Value::Int(3))
        });
        assert!(layer.playing().is_none());
    }

    #[test]
    fn idle_fidget_uses_elapsed_time_across_loop_boundaries() {
        let data = crate::character::load(std::path::Path::new("C:/Games2"), "bumblebee").unwrap();
        let sets = RuleSet::all(&data.animations);
        let mut base = Base::default();
        let values = |v, seconds| Some(match v {
            var::TIME_IN_ANIM => Value::Float(seconds),
            var::TIME_SINCE_DAMAGED => Value::Float(9.0),
            var::SPECIAL_IDLE_ON => Value::Int(1),
            var::INTERACT_TYPE => Value::Hash(0),
            _ => return None,
        });
        base.enter(&sets, crc32(b"IdleStandSet"), (EMPTY_SET, 0), &|v| values(v, 0.0));
        base.advance(&sets, 7.9);
        let seconds = base.seconds();
        base.update(&sets, &|v| values(v, seconds));
        assert_eq!(base.clip, crc32(b"Idle"));
        base.advance(&sets, 0.2);
        let seconds = base.seconds();
        assert!(seconds > 8.0 && base.tick < 68.0);
        base.update(&sets, &|v| values(v, seconds));
        assert_eq!(base.clip, crc32(b"Fidget1"));
        base.advance(&sets, 20.0);
        let seconds = base.seconds();
        base.update(&sets, &|v| values(v, seconds));
        assert_eq!(base.clip, crc32(b"Idle"));
        assert_eq!(base.seconds(), 0.0);
    }

    #[test]
    fn continue_time_carries_only_nonloop_overrun_into_the_next_player() {
        let mut sets = walking_sets();
        let set = crc32(b"S");
        sets.get_mut(&set).unwrap().clips.iter_mut().find(|c| c.name == "Land").unwrap().continue_time = true;
        let mut base = Base::default();
        base.enter(&sets, set, (99, 0), &|_| None);
        base.advance(&sets, 0.6); // length 30 ticks, overrun 6
        base.update(&sets, &|v| (v == var::L_STICK_AMP).then_some(Value::Float(1.0)));
        assert_eq!(base.clip, crc32(b"Run"));
        assert!((base.tick - 6.0).abs() < 1e-4);
    }

    fn rule(variable: u32, logic: u8, float: f32) -> Rule {
        Rule { variable, logic, float, ..Default::default() }
    }

    #[test]
    fn comparisons_are_the_games_six() {
        let at = |logic, v: f32| holds(&rule(var::STRAFE_ANG, logic, 45.0), Some(Value::Float(v)));
        assert!(at(0, 45.0) && !at(0, 44.0));
        assert!(at(1, 44.0) && !at(1, 45.0));
        assert!(at(2, 46.0) && !at(2, 45.0));
        assert!(at(3, 45.0) && !at(3, 44.9));
        assert!(at(4, 44.9) && !at(4, 45.0));
        assert!(at(5, 45.0) && !at(5, 45.1));
    }

    #[test]
    fn a_variable_is_compared_by_its_own_kind() {
        let strafing = Rule { variable: var::IS_STRAFING, logic: 0, int: 1, ..Default::default() };
        assert!(holds(&strafing, Some(Value::Int(1))));
        assert!(!holds(&strafing, Some(Value::Int(0))));
        // Unregistered: the float 0 against the rule's float value, which is 0 here.
        assert!(holds(&strafing, None));
        let jump = Rule { variable: var::JUMP_TYPE, logic: 0, hash: 0xaa18_5eeb, ..Default::default() };
        assert!(holds(&jump, Some(Value::Hash(0xaa18_5eeb))));
    }

    #[test]
    fn the_first_exit_that_fits_wins_and_waits_for_its_branch_point() {
        let moving = rule(var::L_STICK_AMP, 3, 0.01);
        let exits = vec![
            ExitRule { to_set: 1, to_id: 10, finish: true, ..Default::default() },
            ExitRule { to_set: 1, to_id: 20, branch_point: 2, rules: vec![moving], ..Default::default() },
            ExitRule { to_set: 1, to_id: 30, ..Default::default() },
        ];
        let stick = |amp: f32| move |v: u32| (v == var::L_STICK_AMP).then_some(Value::Float(amp));
        assert_eq!(exit(&exits, 1, false, &stick(1.0)).map(|e| e.to_id), Some(30));
        assert_eq!(exit(&exits, 2, false, &stick(1.0)).map(|e| e.to_id), Some(20));
        assert_eq!(exit(&exits, 2, false, &stick(0.0)).map(|e| e.to_id), Some(30));
        assert_eq!(exit(&exits, 1, true, &stick(0.0)).map(|e| e.to_id), Some(10));
    }

    #[test]
    fn an_entry_fits_where_it_comes_from() {
        let incoming = vec![
            IncomingRule { from_set: 7, from_id: 70, to_set: 1, to_id: 10, ..Default::default() },
            IncomingRule { from_set: 7, to_set: 1, to_id: 20, rules: vec![rule(var::STRAFE_ANG, 3, 45.0)], ..Default::default() },
            IncomingRule { to_set: 1, to_id: 30, ..Default::default() },
        ];
        let none = |_: u32| None;
        let left = |v: u32| (v == var::STRAFE_ANG).then_some(Value::Float(90.0));
        assert_eq!(entry(&incoming, 7, 70, &none), Some((1, 10)));
        assert_eq!(entry(&incoming, 7, 71, &none), Some((1, 30)));
        assert_eq!(entry(&incoming, 7, 71, &left), Some((1, 20)));
        assert_eq!(entry(&incoming, 8, 70, &left), Some((1, 30)));
        assert_eq!(entry(&incoming[..2], 8, 70, &left), None);
    }

    /// A set of three clips: a walk and a run (loops) that hand over to each other by the
    /// stick, and a landing that runs on into the run, with a blend of its own on the way out.
    fn walking_sets() -> HashMap<u32, RuleSet> {
        let set = crc32(b"S");
        let (walk, run, land) = (crc32(b"Walk"), crc32(b"Run"), crc32(b"Land"));
        let fast = rule(var::L_STICK_AMP, 3, 0.7);
        let slow = rule(var::L_STICK_AMP, 4, 0.7);
        let clip = |id: u32, name: &str, ticks: f32, looping: bool, crossfade: f32, exits: Vec<ExitRule>| RuleClip {
            id,
            name: name.into(),
            ticks,
            speed: 1.0,
            crossfade,
            looping,
            exit_crossfade: -1.0,
            exits,
            ..Default::default()
        };
        let mut rules = RuleSet {
            name: set,
            set_name: "S".into(),
            clips: vec![
                clip(walk, "Walk", 60.0, true, 0.25, vec![ExitRule { to_set: set, to_id: run, rules: vec![fast.clone()], ..Default::default() }]),
                clip(run, "Run", 40.0, true, 0.25, vec![ExitRule { to_set: set, to_id: walk, rules: vec![slow], ..Default::default() }]),
                clip(land, "Land", 30.0, false, 0.1, vec![ExitRule { to_set: set, to_id: 0, finish: true, ..Default::default() }]),
            ],
            incoming: vec![
                IncomingRule { from_set: 99, to_set: set, to_id: land, ..Default::default() },
                IncomingRule { to_set: set, to_id: run, rules: vec![fast], ..Default::default() },
                IncomingRule { to_set: set, to_id: walk, ..Default::default() },
            ],
        };
        // The landing hands over to the run with no blend at all.
        rules.clips[2].exit_crossfade = 0.0;
        rules.clips[2].exit_crossfade_to = run;
        HashMap::from([(set, rules)])
    }

    #[test]
    fn a_set_starts_on_the_clip_its_entry_rules_pick_from_what_was_playing() {
        let sets = walking_sets();
        let set = crc32(b"S");
        let stick = |amp: f32| move |v: u32| (v == var::L_STICK_AMP).then_some(Value::Float(amp));
        let mut base = Base::default();
        assert!(base.enter(&sets, set, (0, 0), &stick(0.2)));
        assert_eq!(base.playing(), Some((set, crc32(b"Walk"))));
        assert_eq!((base.blend, base.serial), (0.25, 1));
        // The rule on the way in: the run, from the same set's walk, with the stick over.
        assert!(base.enter(&sets, set, (set, crc32(b"Walk")), &stick(1.0)));
        assert_eq!(base.playing(), Some((set, crc32(b"Run"))));
        // Coming from set 99 the landing is picked, whatever the stick says.
        assert!(base.enter(&sets, set, (99, 5), &stick(1.0)));
        assert_eq!(base.playing(), Some((set, crc32(b"Land"))));
        assert_eq!(base.blend, 0.1);
        // A set nobody knows, or one with no entry that fits, changes nothing.
        let before = base.serial;
        assert!(!base.enter(&sets, 12345, (0, 0), &stick(1.0)));
        assert_eq!(base.serial, before);
    }

    #[test]
    fn the_playing_clips_exit_rules_hand_over_and_a_finished_one_runs_on_with_its_own_blend() {
        let sets = walking_sets();
        let set = crc32(b"S");
        let stick = |amp: f32| move |v: u32| (v == var::L_STICK_AMP).then_some(Value::Float(amp));
        let mut base = Base::default();
        base.enter(&sets, set, (0, 0), &stick(0.2));
        // The walk goes to the run at any time once the stick is over 0.7, and back.
        base.update(&sets, &stick(0.5));
        assert_eq!(base.clip, crc32(b"Walk"));
        base.update(&sets, &stick(0.9));
        assert_eq!(base.clip, crc32(b"Run"));
        base.update(&sets, &stick(0.2));
        assert_eq!(base.clip, crc32(b"Walk"));
        // The landing plays out, then its exit (no clip named) walks the set's entry rules
        // from itself: the run with the stick over, taken with the landing's own blend of 0.
        base.enter(&sets, set, (99, 1), &stick(0.9));
        assert_eq!(base.clip, crc32(b"Land"));
        base.advance(&sets, 29.0 / 60.0);
        base.update(&sets, &stick(0.9));
        assert_eq!(base.clip, crc32(b"Land"), "not finished yet");
        base.advance(&sets, 1.0);
        assert_eq!(base.tick, 30.0, "a one-off stops at its end");
        base.update(&sets, &stick(0.9));
        assert_eq!(base.clip, crc32(b"Run"));
        assert_eq!(base.blend, 0.0);
        // A clip started again is told apart by the count.
        let before = base.serial;
        base.enter(&sets, set, (set, crc32(b"Run")), &stick(0.9));
        assert_eq!(base.serial, before, "the same clip named again is not a new start");
    }

    /// On the install's own data: walking, running, stopping, jumping and falling through
    /// the simulation, the clip the base layer's rules pick is the one the movement's pace
    /// chooser leads with, update by update (`examples\base_check` prints the sequence).
    /// Skipped where the install is not.
    #[test]
    fn the_sets_own_rules_pick_the_clips_the_movement_runs_on_foot() {
        use crate::sim::{self, Arena, Box3, Input, Mode, Pace, State};
        use crate::tuning::{self, Actions, Locomotion};
        use glam::{Vec2, Vec3};

        let dir = std::path::Path::new(r"C:\Games2");
        if !dir.exists() {
            return;
        }
        let mut t = tuning::load(dir, "Bumblebee").unwrap();
        let data = crate::character::load(dir, "bumblebee").unwrap();
        t.locomotion = Locomotion::from_animations(&data.animations, &mut t.missing);
        t.actions = Actions::from_animations(&data.animations, &data.robot, &mut t.missing);
        t.weapon_set = RuleSet::from_animations(&data.animations, "WeaponSet", &mut t.missing);
        t.rule_sets = RuleSet::all(&data.animations);

        let run = Input { stick: Vec2::Y, ..Default::default() };
        let walk = Input { stick: Vec2::Y * 0.5, ..Default::default() };
        let jump = Input { jump: true, jump_held: true, ..Default::default() };
        let held = |input: Input| Input { jump: false, jump_held: true, ..input };
        let still = Input::default();
        let rep = |input: Input, n: usize| std::iter::repeat_n(input, n);
        let mut walking = Vec::new();
        for (input, n) in [(still, 40), (run, 60), (still, 60), (walk, 40), (still, 40), (run, 30), (walk, 30), (run, 30), (still, 60)] {
            walking.extend(rep(input, n));
        }
        let mut jumping = Vec::new();
        for (input, n) in [(still, 30), (jump, 1), (held(still), 5), (still, 90), (run, 40), (Input { stick: Vec2::Y, ..jump }, 1)] {
            jumping.extend(rep(input, n));
        }
        for (input, n) in [(held(run), 5), (run, 90), (still, 40), (run, 30), (Input { stick: Vec2::Y, ..jump }, 1), (held(run), 5), (still, 100)] {
            jumping.extend(rep(input, n));
        }
        // Running off the edge of a platform 12 high.
        let platform = Arena { boxes: vec![Box3 { min: Vec3::new(-50.0, -50.0, -10.0), max: Vec3::new(50.0, 30.0, 12.0) }], ..Default::default() };
        let flat = Arena::default();
        let mut off = Vec::new();
        off.extend(rep(run, 90));
        off.extend(rep(still, 100));
        let cases = [("walk and stop", walking, &flat, 0.0), ("jumps", jumping, &flat, 0.0), ("off a ledge", off, &platform, 12.0)];

        let mut compared = 0;
        for (name, inputs, arena, z) in cases {
            let mut s = State::new(&t);
            s.pos.z = z;
            for (i, input) in inputs.iter().enumerate() {
                sim::step(&mut s, input, &t, arena, 0.032);
                if !matches!(s.mode, Mode::Stand | Mode::Move) {
                    continue;
                }
                let base = s.base_clip(&t).unwrap_or_else(|| panic!("{name}, update {i}: the rules have picked nothing"));
                let wanted = match s.pace() {
                    Pace::Idle => None,
                    Pace::Walk => Some("Walk"),
                    Pace::Run => Some("Run"),
                    Pace::Stop => Some("Run2Idle"),
                    Pace::LandRun => Some("Jump_LandRun"),
                    Pace::BigLandRun => Some("BigJump_LandRun"),
                };
                match wanted {
                    // Standing: the idle clip, or a landing on the spot.
                    None => assert_eq!(base.set, "IdleStandSet", "{name}, update {i}: {}/{}", base.set, base.id),
                    Some(clip) => assert_eq!(base.id, clip, "{name}, update {i}: the pace leads with {clip}"),
                }
                compared += 1;
            }
        }
        assert!(compared > 800, "{compared}");
    }

    /// The dodge's direction is the dash set's entry rules on the stick against the aim, on the
    /// install's data: front from -45 up to 45, left from 45 up to 135, right from -135 up to
    /// -45, back beyond.
    #[test]
    fn the_dash_sets_entry_rules_pick_the_dodge_by_the_sticks_angle() {
        let dir = std::path::Path::new(r"C:\Games2");
        if !dir.exists() {
            return;
        }
        let data = crate::character::load(dir, "bumblebee").unwrap();
        let sets = RuleSet::all(&data.animations);
        let dash = sets.get(&crc32(b"DashSet")).expect("the dash set");
        for tenths in -1800..=1800 {
            let angle = tenths as f32 / 10.0;
            let get = |v: u32| (v == var::L_STICK_DIR).then_some(Value::Float(angle));
            let (_, id) = entry(&dash.incoming, 0, 0, &get).unwrap_or_else(|| panic!("no dodge at {angle}"));
            let wanted = if (-45.0..45.0).contains(&angle) {
                "DodgeFront"
            } else if (45.0..135.0).contains(&angle) {
                "DodgeLeft"
            } else if (-135.0..-45.0).contains(&angle) {
                "DodgeRight"
            } else {
                "DodgeBack"
            };
            assert_eq!(id, crc32(wanted.as_bytes()), "at {angle} degrees");
        }
    }

    /// Out of the car at a run: the walk set starts on the run with none of the blend the run
    /// has of its own (`Trans_V2R_Run_B` asks 0 on its way out to `Run`), and the idle set on
    /// the run-to-idle clip; a landing runs on into the run the same way. On the install's
    /// data; skipped where the install is not.
    #[test]
    fn the_change_back_from_the_car_runs_on_into_the_run_without_a_blend() {
        let dir = std::path::Path::new(r"C:\Games2");
        if !dir.exists() {
            return;
        }
        let data = crate::character::load(dir, "bumblebee").unwrap();
        let sets = RuleSet::all(&data.animations);
        let (idle, walk) = (crc32(b"IdleStandSet"), crc32(b"WalkSet"));
        let from = (crc32(b"Vehicle2RobotSet"), crc32(b"Trans_V2R_Run_B"));
        let stick = |amp: f32| move |v: u32| match v {
            v if v == var::L_STICK_AMP => Some(Value::Float(amp)),
            v if v == var::IS_STRAFING => Some(Value::Int(0)),
            v if v == var::INTERACT_TYPE => Some(Value::Hash(0)),
            _ => None,
        };
        let mut base = Base::default();
        assert!(base.enter(&sets, walk, from, &stick(1.0)));
        assert_eq!(base.playing(), Some((walk, crc32(b"Run"))));
        assert_eq!(base.blend, 0.0, "the run after the change has no blend");
        let mut base = Base::default();
        assert!(base.enter(&sets, idle, from, &stick(0.0)));
        assert_eq!(base.playing(), Some((idle, crc32(b"Run2Idle"))));
        // The same run started from rest has its own blend of a quarter second.
        let mut base = Base::default();
        assert!(base.enter(&sets, walk, (idle, crc32(b"Idle")), &stick(1.0)));
        assert_eq!(base.blend, 0.25);
        // Carried (the interaction type is the VIP's) the walk set picks its `Carry` clips.
        let carrying = |v: u32| (v == var::INTERACT_TYPE).then_some(Value::Hash(0xe081_c00e)).or_else(|| stick(1.0)(v));
        let mut base = Base::default();
        assert!(base.enter(&sets, walk, (idle, crc32(b"Idle")), &carrying));
        assert_eq!(base.clip, crc32(b"Run_Carry"));
    }

    #[test]
    fn a_loop_carries_on_round_at_its_own_speed() {
        let mut sets = walking_sets();
        let set = crc32(b"S");
        sets.get_mut(&set).unwrap().clips[1].speed = 2.0;
        let mut base = Base::default();
        base.enter(&sets, set, (0, 0), &|_| Some(Value::Float(1.0)));
        assert_eq!(base.clip, crc32(b"Run"));
        base.advance(&sets, 0.5);
        // 0.5 s is 30 ticks, at twice the speed 60: round a 40-tick loop once and 20 over.
        assert!((base.tick - 20.0).abs() < 1e-4, "{}", base.tick);
        assert!((base.seconds() - 1.0).abs() < 1e-4, "rule time is unwrapped player time");
    }
}
