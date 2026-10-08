//! The gun in his hand (`notes\weapon-mode.md`, "The weapon layer and the gun's clips"):
//! the clips of his weapon layer (`WeaponPartialSet`: the arm pulling the gun out, the
//! recoil, the change of weapon, the overheat, putting it away) and the clips the weapon's
//! own object plays, which the arm clips' events and the weapon's code start.
//!
//! Read from the original: which message starts which arm clip (`FUN_0084dfa0` and the
//! set's own entry rules on `C_WEAPON_ACTION`), the events (`FUN_007b4680`), the object's
//! clip player (`FUN_007c37e0`, `FUN_007c4170`), the weapon's own starts (its slots
//! `+0x10c` to `+0x120`), and the trigger's gate (`FUN_007b37f0`).
//!
//! The layer's blending is the animation controller's (`FUN_0074a180` starts a layer's
//! clip, `FUN_0074a970` updates the layer, `FUN_0074a610` takes it off): see `Arm::blend`.
//! Not ported: an event for a weapon that is not in hand but shares the object of the one
//! that is (the third branch of `FUN_007b4680`), and the object's own cross-fade.

use crate::animrules::var;
use crate::formats::anim::{AnimRef, Clip, IncomingRule, WeaponEventKind};
use crate::formats::hash::crc32;
use crate::weapons::WeaponDef;

/// Values of `C_WEAPON_ACTION`. [game]
pub mod action {
    use crate::formats::hash::crc32;
    pub const PULL_OUT: u32 = crc32(b"WEAPON_ACTION_PULL_OUT_GUN");
    pub const SWITCH: u32 = crc32(b"WEAPON_ACTION_SWITCH");
    pub const OVERHEAT: u32 = crc32(b"WEAPON_ACTION_OVERHEAT");
    /// What message ab304c20 (a round fired) sets; its name is not in the dictionary.
    pub const RECOIL: u32 = 0xb90a_70bd;
}

/// The two arm clips the trigger does not get past (`FUN_007b37f0`). [game]
const PULL_OUT_CLIP: u32 = crc32(b"Trans_R2W");
const SWITCH_CLIP: u32 = crc32(b"Weapon_Switch");
/// The trigger pulled during one of them ends it over this long (message 409f0e77 sets
/// the layer's blend to 0x3e083127), and the weapon fires once that is over. [game]
const CUT_SHORT: f32 = 0.132_999_99;
const TICKS_PER_SECOND: f32 = 60.0;

/// A clip of the weapon layer as it plays.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Arm {
    /// Which clip of `WeaponPartialSet`.
    pub clip: usize,
    pub tick: f32,
    /// How much of the clip is laid over his pose, 0 to 1 (for a clip of poses, 1 less
    /// the layer's `BlendAnimation` weight; for one of offsets, its `AdditiveAnimation`'s).
    weight: f32,
    /// The trigger ended it (the layer's `+0x94` is set).
    cut: bool,
    /// A looping clip that was left fades out over this long (the layer's `+0x9c`).
    out: Option<f32>,
    /// Seconds counted against either of the two (the layer's `+0xa4`).
    elapsed: f32,
}

impl Arm {
    pub fn weight(&self) -> f32 {
        self.weight
    }

    /// The layer's update (`FUN_0074a970`), with the clip `t` seconds in and `length`
    /// seconds long. Gives false when the layer is taken off. [game]
    ///
    /// - Cut by the trigger: gone after 0.133 s or when a clip that is not a loop ends;
    ///   until then its share drops with the time since the cut.
    /// - A loop that was left: gone after its `xfadeTimeOnExit`, dropping the same way.
    /// - A clip that is not a loop fades out INSIDE its own last `xfadeTimeOnExit`
    ///   seconds, so it is gone the moment it ends.
    /// - Otherwise, while it is less than `xfadeTime` in, its share is how far in it is.
    ///   Nothing sets the share to 1 when that is over: it stays at the last value the
    ///   fade gave (0.94 for 0.3 s at 60 updates a second).
    /// - A fade out never raises the share.
    fn blend(&mut self, r: &AnimRef, t: f32, length: f32, dt: f32) -> bool {
        let share = |part: f32, of: f32| (part / of).clamp(0.0, 1.0);
        if self.cut {
            self.elapsed += dt;
            if self.elapsed >= CUT_SHORT || (!r.looping && length - t <= 0.0) {
                return false;
            }
            self.weight = self.weight.min(1.0 - share(self.elapsed, CUT_SHORT));
        } else if let Some(out) = self.out {
            self.elapsed += dt;
            if self.elapsed >= out {
                return false;
            }
            self.weight = self.weight.min(1.0 - share(self.elapsed, out));
        } else if r.exit_crossfade > 0.0 && !r.looping && length - t < r.exit_crossfade {
            self.weight = self.weight.min(share(length - t, r.exit_crossfade));
        } else if r.crossfade > 0.0 && t < r.crossfade {
            self.weight = share(t, r.crossfade);
        }
        true
    }
}

/// The weapon object's own clip as it plays.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ObjectClip {
    /// Its id (CRC-32 of the name).
    pub id: u32,
    pub tick: f32,
    finished: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Gun {
    /// The gun is out (character flag 0x100000).
    pub out: bool,
    action: u32,
    pub arm: Option<Arm>,
    /// A `WeaponEnableFireEvent` has shut the trigger off.
    fire_shut: bool,
    /// The weapon's object is shown, the node of his it is linked to, and what it plays.
    pub shown: bool,
    pub node: u32,
    pub object: Option<ObjectClip>,
    /// When the object's clip ends: the clip to go on to, and whether to hide it.
    then: u32,
    hide_when_done: bool,
}

/// The clips the gun's rules run on, from his pack.
#[derive(Clone, Copy)]
pub struct Clips<'a> {
    /// `WeaponPartialSet` and its entry rules.
    pub arm: &'a [AnimRef],
    pub entries: &'a [IncomingRule],
    /// The weapon object's own.
    pub object: &'a [AnimRef],
}

fn id_of(r: &AnimRef) -> u32 {
    crc32(r.id.as_bytes())
}

/// One update of a weapon object's clip player (`FUN_007c4170`): a clip that is not a
/// loop and has ended hands over to the clip named for then, and hides the object if it
/// was asked to. Otherwise it holds its end. [game]
fn step_object(object: &mut Option<ObjectClip>, then: &mut u32, hide_when_done: &mut bool, shown: &mut bool, clips: &[AnimRef], dt: f32) {
    let Some(mut playing) = *object else { return };
    if let Some((r, clip)) = clips.iter().find(|r| id_of(r) == playing.id).and_then(|r| r.clip.as_ref().map(|c| (r, c))) {
        playing.tick += dt * TICKS_PER_SECOND * r.speed;
        if r.looping {
            playing.tick %= clip.length.max(1.0);
        } else if playing.tick >= clip.length {
            playing.tick = clip.length;
            playing.finished = true;
        }
    } else {
        playing.finished = true;
    }
    *object = Some(playing);
    if playing.finished {
        if *then != 0 {
            *object = Some(ObjectClip { id: std::mem::take(then), tick: 0.0, finished: false });
        }
        if std::mem::take(hide_when_done) {
            *shown = false;
        }
    }
}

/// The melee weapon in his hand (the axe). It is a weapon of the manager's list like the
/// guns, but one that is never "in hand" (its slot `+0x30` says no), so the events that
/// name it always count (`FUN_007b4680`): the attack clips link it to the node their
/// event sits on (`ObjectAttachment_Hand_L`), show it and start its object's clips
/// (`Trans_R2W` unfolding it, then `Idle`; `Trans_W2R` folding it, hidden when done).
/// Leaving an attack state for anything but another attack state puts it away
/// (`FUN_00878be0` -> `FUN_007b2d80` -> its slot `+0x28`, `FUN_007a67b0`). [game]
#[derive(Clone, Debug, Default)]
pub struct Axe {
    pub shown: bool,
    /// The node of his it is linked to (CRC-32 of its name).
    pub node: u32,
    pub object: Option<ObjectClip>,
    then: u32,
    hide_when_done: bool,
    /// An event has brought it out and nothing has put it away since (weapon `+0x38`).
    in_use: bool,
}

impl Axe {
    /// The weapon events of the clip he plays, from `from` (not counted; `None` when the
    /// clip has just started, and its events at the start count) to `to`, for the weapon
    /// called `name`. An event that names no weapon is for the one at its `weaponIndex`
    /// in the manager's list: the charge attack's put-away event names none and gives
    /// -1, so it does nothing, here as in the original. [game] [data]
    pub fn events(&mut self, clip: &Clip, from: Option<f32>, to: f32, name: u32) {
        for event in clip.weapon_events.iter().filter(|e| e.time <= to && from.is_none_or(|from| e.time > from)) {
            let WeaponEventKind::Attach { weapon, hide, unhide, clip, hide_when_done, then, .. } = event.kind else { continue };
            if weapon != name {
                continue;
            }
            self.node = event.node;
            if hide {
                (self.shown, self.in_use) = (false, false);
            } else if unhide {
                (self.shown, self.in_use) = (true, true);
            }
            if clip != 0 {
                self.object = Some(ObjectClip { id: clip, tick: 0.0, finished: false });
                (self.then, self.hide_when_done) = (0, false);
            }
            if then != 0 {
                self.then = then;
            }
            self.hide_when_done = hide_when_done;
        }
    }

    /// Put away (`FUN_007a67b0`): if it is in use, its object plays `exit_clip` unless
    /// that is the clip it already plays, and is hidden when the clip ends; with no such
    /// clip it is hidden at once. [game]
    pub fn put_away(&mut self, exit_clip: u32) {
        if !std::mem::take(&mut self.in_use) {
            return;
        }
        if exit_clip == 0 {
            self.shown = false;
        } else {
            if self.object.is_none_or(|o| o.id != exit_clip) {
                self.object = Some(ObjectClip { id: exit_clip, tick: 0.0, finished: false });
                self.then = 0;
            }
            self.hide_when_done = true;
        }
    }

    /// Advances its object's clip by `dt` seconds.
    pub fn step(&mut self, clips: &[AnimRef], dt: f32) {
        step_object(&mut self.object, &mut self.then, &mut self.hide_when_done, &mut self.shown, clips, dt);
    }
}

impl Gun {
    /// How much of the arm clip is laid over his pose, 0 to 1.
    pub fn arm_weight(&self) -> f32 {
        self.arm.map_or(0.0, |arm| arm.weight)
    }

    /// Walks the set's entry rules with the action as it stands and starts the clip the
    /// first fitting one names (`FUN_0084ddb0` with no clip, on layer 1). [game]
    fn start_by_rules(&mut self, clips: Clips) {
        let fits = |entry: &&IncomingRule| {
            entry.rules.iter().all(|rule| rule.variable == var::WEAPON_ACTION && (rule.hash == self.action) == (rule.logic == 0))
        };
        if let Some(entry) = clips.entries.iter().find(fits) {
            self.start_arm(clips, entry.to_id);
        }
    }

    fn start_arm(&mut self, clips: Clips, id: u32) {
        if let Some(clip) = clips.arm.iter().position(|r| id_of(r) == id && r.clip.is_some()) {
            // A layer's clip takes the layer over at once, whatever played there, and
            // starts at no share if it has a blend time and in full if not
            // (`FUN_0074a180`). [game]
            let weight = if clips.arm[clip].crossfade > 0.0 { 0.0 } else { 1.0 };
            self.arm = Some(Arm { clip, tick: 0.0, weight, cut: false, out: None, elapsed: 0.0 });
            self.arm_events(clips, clip, None, 0.0, 0);
        }
    }

    /// Starts a clip on the weapon's object (`FUN_007c37e0`): `unless_playing` leaves a
    /// clip that is already the one playing alone.
    fn play(&mut self, id: u32, then: u32, hide_when_done: bool, unless_playing: bool) {
        if unless_playing && self.object.is_some_and(|o| o.id == id) {
            return;
        }
        if id != 0 {
            self.object = Some(ObjectClip { id, tick: 0.0, finished: false });
            (self.then, self.hide_when_done) = (0, false);
        }
        if then != 0 {
            self.then = then;
        }
        self.hide_when_done = hide_when_done;
    }

    /// The arm clip's events from `from` (not counted, or counted when `None`: the clip
    /// has just started) to `to`, for the weapon in hand.
    fn arm_events(&mut self, clips: Clips, clip: usize, from: Option<f32>, to: f32, weapon: u32) {
        let Some(data) = clips.arm.get(clip).and_then(|r| r.clip.as_ref()) else { return };
        for event in data.weapon_events.iter().filter(|e| e.time <= to && from.is_none_or(|from| e.time > from)) {
            match event.kind {
                WeaponEventKind::EnableFire(allow) => self.fire_shut = !allow,
                WeaponEventKind::Attach { weapon: named, only_if_current, hide, unhide, clip, hide_when_done, then, .. } => {
                    if only_if_current && named != weapon {
                        continue;
                    }
                    self.node = event.node;
                    if hide {
                        self.shown = false;
                    } else if unhide {
                        self.shown = true;
                    }
                    self.play(clip, then, hide_when_done, false);
                }
            }
        }
    }

    /// He enters a weapon state from one that is neither a weapon state nor the dodge
    /// (`FUN_0087f4a0`): if the gun is not out, the arm pulls it out. `from_car`: the set
    /// his base layer played until now is `Vehicle2RobotSet` (he comes straight out of
    /// the change from the car). Then no arm clip plays: the weapon in hand is shown at
    /// once (its slot `+0x24` with 1, `FUN_007a7630`, which also starts its idle clip, as
    /// an unhiding event with no clip does), and the trigger works at once. [game]
    pub fn take_aim(&mut self, clips: Clips, weapon: &WeaponDef, from_car: bool) {
        if self.out {
            return;
        }
        self.out = true;
        if from_car {
            self.shown = true;
            self.play(weapon.idle_clip, 0, false, false);
        } else {
            self.action = action::PULL_OUT;
            self.start_by_rules(clips);
        }
    }

    /// He leaves the weapon states (`FUN_0087f6f0` -> the manager's `FUN_007b2eb0` -> the
    /// weapon's slot `+0x110`, `FUN_007ab1b0`): with an `exitAnimation`, his arm plays
    /// `exitCharacterAnimation` and the object the exit clip, hidden when that is done
    /// unless it always appears. [game]
    pub fn put_away(&mut self, clips: Clips, weapon: &WeaponDef) {
        if !std::mem::take(&mut self.out) {
            return;
        }
        self.action = 0;
        if weapon.exit_clip == 0 {
            return;
        }
        if weapon.exit_arm_clip != 0 {
            self.start_arm(clips, weapon.exit_arm_clip);
        }
        self.play(weapon.exit_clip, 0, !weapon.always_appear, true);
    }

    /// A round left the weapon (`FUN_007ae010` sends ab304c20): in weapon mode the arm
    /// recoils, and the recoil's event starts the object's firing clip. [game]
    pub fn fired(&mut self, clips: Clips, weapon: u32) {
        if self.out {
            self.action = action::RECOIL;
            self.start_by_rules(clips);
            self.run_start_events(clips, weapon);
        }
    }

    /// The weapon in hand was changed (message f800db01). [game]
    pub fn switched(&mut self, clips: Clips, weapon: u32) {
        if self.out {
            self.action = action::SWITCH;
            self.start_by_rules(clips);
            self.run_start_events(clips, weapon);
        }
    }

    /// The weapon overheated (`FUN_007a9ec0`: its slot `+0x120` starts the object's
    /// `overheatAnimation`; message 68479e46 starts the arm's). [game]
    pub fn overheated(&mut self, clips: Clips, weapon: &WeaponDef) {
        self.play(weapon.overheat_clip, 0, false, false);
        if self.out {
            self.action = action::OVERHEAT;
            self.start_by_rules(clips);
        }
    }

    /// The overheat is over (`FUN_007a9700`: slot `+0x118` starts `exitOverheatAnimation`;
    /// message c29eb8df clears the action, which the arm clip's exit rule waits for). [game]
    pub fn cooled(&mut self, weapon: &WeaponDef) {
        self.play(weapon.exit_overheat_clip, 0, false, false);
        if self.action == action::OVERHEAT {
            self.action = 0;
        }
    }

    /// The events at a clip's very start need the weapon in hand, which `start_arm` does
    /// not know: run them again with it.
    fn run_start_events(&mut self, clips: Clips, weapon: u32) {
        if let Some(arm) = self.arm.filter(|arm| arm.tick == 0.0) {
            self.arm_events(clips, arm.clip, None, 0.0, weapon);
        }
    }

    /// Whether the trigger is kept from the weapon this update (`FUN_007b37f0`): while
    /// the arm pulls the gun out or changes it, while such a clip that the trigger ended
    /// fades, and while an event has shut the trigger off. A trigger held during one of
    /// the two clips, with fire allowed, ends the clip. [game]
    pub fn holds_trigger(&mut self, clips: Clips, trigger: bool) -> bool {
        let busy = self.arm.filter(|arm| !arm.cut).and_then(|arm| clips.arm.get(arm.clip)).map(id_of);
        let busy = matches!(busy, Some(PULL_OUT_CLIP | SWITCH_CLIP));
        let cutting = self.arm.is_some_and(|arm| arm.cut);
        if trigger && busy && !self.fire_shut {
            if let Some(arm) = &mut self.arm {
                (arm.cut, arm.elapsed) = (true, 0.0);
            }
            return true;
        }
        busy || cutting || self.fire_shut
    }

    /// Advances both players by `dt` seconds; `weapon` is the name of the one in hand.
    pub fn step(&mut self, clips: Clips, weapon: u32, dt: f32) {
        if let Some(mut arm) = self.arm.take() {
            let keep = match clips.arm.get(arm.clip).and_then(|r| r.clip.as_ref().map(|clip| (r, clip))) {
                None => false,
                Some((r, clip)) => {
                    // The clip plays on while it fades, whatever ends it.
                    let before = arm.tick;
                    arm.tick += dt * TICKS_PER_SECOND * r.speed;
                    let finished = !r.looping && arm.tick >= clip.length;
                    arm.tick = if r.looping { arm.tick % clip.length.max(1.0) } else { arm.tick.min(clip.length) };
                    if arm.tick >= before {
                        self.arm_events(clips, arm.clip, Some(before), arm.tick, weapon);
                    }
                    // Its exit rules: once it has finished, or at any time, while the
                    // rules on the action hold. All of his lead to the empty set, which
                    // is `FUN_0074a180` with no clip: a LOOP with an `xfadeTimeOnExit`
                    // fades out over it, anything else is off at once. [game]
                    let leaves = arm.out.is_none()
                        && r.exits.iter().any(|exit| {
                            (finished || !exit.finish)
                                && exit.rules.iter().all(|rule| rule.variable == var::WEAPON_ACTION && (rule.hash == self.action) == (rule.logic == 0))
                        });
                    let fades = r.looping && r.exit_crossfade > 0.0;
                    if leaves && fades {
                        (arm.out, arm.elapsed) = (Some(r.exit_crossfade), 0.0);
                    }
                    (!leaves || fades) && arm.blend(r, arm.tick / TICKS_PER_SECOND, clip.length / TICKS_PER_SECOND, dt)
                }
            };
            self.arm = keep.then_some(arm);
        }
        step_object(&mut self.object, &mut self.then, &mut self.hide_when_done, &mut self.shown, clips.object, dt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::anim::{Clip, ExitRule, Rule, WeaponEvent};
    use crate::weapons::Kind;
    use glam::Vec3;

    const CANNON: u32 = 1;
    const NODE: u32 = 77;
    const DT: f32 = 1.0 / 60.0;

    fn id(name: &str) -> u32 {
        crc32(name.as_bytes())
    }

    fn on_action(hash: u32, equal: bool) -> Vec<Rule> {
        vec![Rule { variable: var::WEAPON_ACTION, logic: if equal { 0 } else { 1 }, hash, ..Default::default() }]
    }

    fn attach(time: f32, clip: &str, unhide: bool) -> WeaponEvent {
        let kind = WeaponEventKind::Attach {
            weapon: CANNON,
            index: -1,
            only_if_current: true,
            hide: false,
            unhide,
            clip: id(clip),
            hide_when_done: false,
            then: 0,
        };
        WeaponEvent { time, node: NODE, kind }
    }

    fn fire(time: f32, allow: bool) -> WeaponEvent {
        WeaponEvent { time, node: NODE, kind: WeaponEventKind::EnableFire(allow) }
    }

    /// His weapon layer's five clips with the lengths, fades, rules and events of his pack.
    fn arm_clips() -> (Vec<AnimRef>, Vec<IncomingRule>) {
        let clip = |name: &str, length: f32, fade: f32, looping: bool, exits: Vec<ExitRule>, events: Vec<WeaponEvent>| AnimRef {
            id: name.into(),
            looping,
            crossfade: fade,
            exit_crossfade: fade,
            speed: 1.0,
            exits,
            clip: Some(Clip { length, weapon_events: events, ..Default::default() }),
            ..Default::default()
        };
        let done = || vec![ExitRule { finish: true, ..Default::default() }];
        let not_overheat = vec![ExitRule { finish: false, rules: on_action(action::OVERHEAT, false), ..Default::default() }];
        let clips = vec![
            clip("Weapon_Recoil", 8.0, -1.0, false, done(), vec![attach(0.0, "FireA", false)]),
            clip("Weapon_Switch", 98.0, 0.3, false, done(), vec![fire(0.0, false), attach(2.0, "SwitchToWeaponA", true), fire(8.0, true)]),
            clip("Weapon_Overheat", 160.0, 0.3, true, not_overheat, vec![]),
            clip("Trans_R2W", 84.0, 0.3, false, done(), vec![fire(0.0, false), attach(4.0, "Trans_R2W_WeaponA", true), fire(8.0, true)]),
            clip("Trans_W2R", 48.0, 0.2, false, done(), vec![]),
        ];
        let entry = |to: &str, action: u32| IncomingRule { to_id: id(to), rules: on_action(action, true), ..Default::default() };
        let entries = vec![
            entry("Trans_R2W", action::PULL_OUT),
            entry("Weapon_Switch", action::SWITCH),
            entry("Weapon_Overheat", action::OVERHEAT),
            entry("Weapon_Recoil", action::RECOIL),
        ];
        (clips, entries)
    }

    fn object_clips() -> Vec<AnimRef> {
        let clip = |name: &str, length: f32, looping: bool| AnimRef {
            id: name.into(),
            looping,
            speed: 1.0,
            clip: Some(Clip { length, ..Default::default() }),
            ..Default::default()
        };
        vec![
            clip("Trans_R2W_WeaponA", 30.0, false),
            clip("Trans_W2R_WeaponA", 20.0, false),
            clip("FireA", 6.0, false),
            clip("Idle_WeaponA", 40.0, true),
            clip("Overheat_Weapon_A", 60.0, true),
            clip("SwitchToWeaponA", 30.0, false),
        ]
    }

    fn cannon() -> WeaponDef {
        WeaponDef {
            kind: Kind::Gun,
            name: CANNON,
            attribute: 0,
            bone: 0,
            fire_sound: 0,
            overheat_sound: 0,
            refused_sound: 0,
            muzzle: Vec3::ZERO,
            barrels: [Vec3::ZERO; 3],
            barrel_count: 1,
            idle_clip: id("Idle_WeaponA"),
            exit_clip: id("Trans_W2R_WeaponA"),
            exit_overheat_clip: id("Idle_WeaponA"),
            overheat_clip: id("Overheat_Weapon_A"),
            exit_arm_clip: id("Trans_W2R"),
            always_appear: false,
            explosion: None,
        }
    }

    fn run(gun: &mut Gun, clips: Clips, ticks: usize) {
        for _ in 0..ticks {
            gun.step(clips, CANNON, DT);
        }
    }

    #[test]
    fn taking_aim_pulls_the_gun_out_and_the_event_at_tick_4_shows_it() {
        let (arm, entries) = arm_clips();
        let object = object_clips();
        let clips = Clips { arm: &arm, entries: &entries, object: &object };
        let mut gun = Gun::default();
        gun.take_aim(clips, &cannon(), false);
        assert_eq!(gun.arm.map(|a| arm[a.clip].id.as_str()), Some("Trans_R2W"));
        assert!(!gun.shown);
        // The trigger is shut off from the clip's start.
        assert!(gun.holds_trigger(clips, false));
        run(&mut gun, clips, 3);
        assert!(!gun.shown);
        run(&mut gun, clips, 2);
        assert!(gun.shown, "shown by the event at tick 4");
        assert_eq!(gun.node, NODE);
        assert_eq!(gun.object.map(|o| o.id), Some(id("Trans_R2W_WeaponA")));
        // 5 ticks in of the 18 it fades in over.
        assert!((gun.arm_weight() - 5.0 / 18.0).abs() < 1e-3, "{}", gun.arm_weight());
        // The fade stops one update short of full and the share stays there.
        run(&mut gun, clips, 45);
        assert!((gun.arm_weight() - 17.0 / 18.0).abs() < 1e-3, "{}", gun.arm_weight());
        // It fades out inside its own last 0.3 s (ticks 66 to 84) and is off as it ends.
        run(&mut gun, clips, 25);
        assert!((gun.arm_weight() - 9.0 / 18.0).abs() < 1e-3, "{}", gun.arm_weight());
        run(&mut gun, clips, 8);
        assert!(gun.arm.is_some());
        run(&mut gun, clips, 2);
        assert!(gun.arm.is_none());
        // The gun stays out, holding the end of its own clip.
        assert!(gun.shown && gun.out);
        assert_eq!(gun.object.map(|o| o.tick), Some(30.0));
        assert!(!gun.holds_trigger(clips, true));
    }

    #[test]
    fn the_trigger_ends_the_pull_out_once_fire_is_allowed_and_fires_after_the_fade() {
        let (arm, entries) = arm_clips();
        let object = object_clips();
        let clips = Clips { arm: &arm, entries: &entries, object: &object };
        let mut gun = Gun::default();
        gun.take_aim(clips, &cannon(), false);
        // Before tick 8 the trigger does nothing to the clip.
        run(&mut gun, clips, 5);
        assert!(gun.holds_trigger(clips, true));
        assert!(gun.arm.is_some_and(|a| !a.cut));
        run(&mut gun, clips, 5);
        // After it, the trigger ends the clip over 0.133 s and is still held back.
        assert!(gun.holds_trigger(clips, true));
        assert!(gun.arm.is_some_and(|a| a.cut));
        run(&mut gun, clips, 4);
        assert!(gun.holds_trigger(clips, true));
        // The clip plays on while it goes, and its share only drops: half the cut's time
        // gone, from the 10 ticks of 18 it had faded in.
        assert!(gun.arm.is_some_and(|a| (a.tick - 14.0).abs() < 1e-3));
        assert!((gun.arm_weight() - 0.4987).abs() < 2e-3, "{}", gun.arm_weight());
        run(&mut gun, clips, 5);
        assert!(gun.arm.is_none());
        assert!(!gun.holds_trigger(clips, true));
        assert!(gun.shown);
    }

    #[test]
    fn a_round_recoils_the_arm_and_fires_the_guns_own_clip() {
        let (arm, entries) = arm_clips();
        let object = object_clips();
        let clips = Clips { arm: &arm, entries: &entries, object: &object };
        let mut gun = Gun { out: true, shown: true, ..Default::default() };
        gun.fired(clips, CANNON);
        assert_eq!(gun.arm.map(|a| arm[a.clip].id.as_str()), Some("Weapon_Recoil"));
        assert_eq!(gun.arm_weight(), 1.0);
        assert_eq!(gun.object.map(|o| o.id), Some(id("FireA")));
        // Another weapon in hand: the cannon's event is not for it.
        let mut other = Gun { out: true, ..Default::default() };
        other.fired(clips, CANNON + 1);
        assert_eq!(other.object, None);
        // The recoil is 8 ticks and has no fade.
        run(&mut gun, clips, 9);
        assert!(gun.arm.is_none());
    }

    #[test]
    fn overheating_holds_the_arm_clip_until_the_weapon_has_cooled() {
        let (arm, entries) = arm_clips();
        let object = object_clips();
        let clips = Clips { arm: &arm, entries: &entries, object: &object };
        let mut gun = Gun { out: true, shown: true, ..Default::default() };
        gun.overheated(clips, &cannon());
        assert_eq!(gun.object.map(|o| o.id), Some(id("Overheat_Weapon_A")));
        run(&mut gun, clips, 300);
        assert_eq!(gun.arm.map(|a| arm[a.clip].id.as_str()), Some("Weapon_Overheat"));
        gun.cooled(&cannon());
        assert_eq!(gun.object.map(|o| o.id), Some(id("Idle_WeaponA")));
        // A loop that is left fades out over its `xfadeTimeOnExit` (0.3 s), still playing.
        let before = gun.arm.map(|a| a.tick);
        run(&mut gun, clips, 1);
        assert!(gun.arm.is_some_and(|a| a.out.is_some()));
        run(&mut gun, clips, 9);
        assert!(gun.arm.map(|a| a.tick) != before);
        assert!((gun.arm_weight() - 8.0 / 18.0).abs() < 1e-3, "{}", gun.arm_weight());
        run(&mut gun, clips, 10);
        assert!(gun.arm.is_none());
    }

    /// The axe's events as his first attack and his charge attack carry them.
    #[test]
    fn the_attack_clips_bring_the_axe_out_and_leaving_the_attack_puts_it_away() {
        const AXE: u32 = 9;
        let event = |time: f32, weapon: u32, clip: &str, then: &str, hide_when_done: bool| WeaponEvent {
            time,
            node: NODE,
            kind: WeaponEventKind::Attach {
                weapon,
                index: -1,
                only_if_current: true,
                hide: false,
                unhide: weapon != 0,
                clip: id(clip),
                hide_when_done,
                then: if then.is_empty() { 0 } else { id(then) },
            },
        };
        let own = |name: &str, length: f32, looping: bool| AnimRef {
            id: name.into(),
            looping,
            speed: 1.0,
            clip: Some(Clip { length, ..Default::default() }),
            ..Default::default()
        };
        let clips = vec![own("Trans_R2W", 10.0, false), own("Idle", 30.0, true), own("Trans_W2R", 10.0, false)];
        let first = Clip {
            length: 60.0,
            weapon_events: vec![event(14.0, AXE, "Trans_R2W", "Idle", false), event(36.0, AXE, "Trans_W2R", "", true)],
            ..Default::default()
        };
        let mut axe = Axe::default();
        let play = |axe: &mut Axe, clip: &Clip, from: f32, to: f32| {
            let mut tick = from;
            while tick < to {
                axe.events(clip, Some(tick), tick + 1.0, AXE);
                axe.step(&clips, DT);
                tick += 1.0;
            }
        };
        play(&mut axe, &first, 0.0, 13.0);
        assert!(!axe.shown);
        play(&mut axe, &first, 13.0, 15.0);
        assert!(axe.shown && axe.node == NODE);
        assert_eq!(axe.object.map(|o| o.id), Some(id("Trans_R2W")));
        // Unfolded, it goes on to its idle clip.
        play(&mut axe, &first, 15.0, 30.0);
        assert_eq!(axe.object.map(|o| o.id), Some(id("Idle")));
        // The event at tick 36 folds it, and it is hidden when that clip of 10 ticks ends.
        play(&mut axe, &first, 30.0, 40.0);
        assert!(axe.shown);
        assert_eq!(axe.object.map(|o| o.id), Some(id("Trans_W2R")));
        play(&mut axe, &first, 40.0, 50.0);
        assert!(!axe.shown);
        // Leaving the attack now changes nothing: the exit clip is the one it played.
        axe.put_away(id("Trans_W2R"));
        assert!(!axe.shown && axe.object.is_some_and(|o| o.tick == 10.0));

        // The charge: brought out by the charging clip; the charge attack's own event
        // names no weapon and does nothing, so it is the state's exit that folds it.
        let charging = Clip { length: 56.0, weapon_events: vec![event(46.0, AXE, "Trans_R2W", "", false)], ..Default::default() };
        let attack = Clip { length: 92.0, weapon_events: vec![event(44.0, 0, "Trans_W2R", "", true)], ..Default::default() };
        play(&mut axe, &charging, 40.0, 56.0);
        assert!(axe.shown);
        play(&mut axe, &attack, 0.0, 92.0);
        assert!(axe.shown, "the event without a weapon's name is no one's");
        assert_eq!(axe.object.map(|o| o.id), Some(id("Trans_R2W")));
        axe.put_away(id("Trans_W2R"));
        assert!(axe.shown);
        for _ in 0..11 {
            axe.step(&clips, DT);
        }
        assert!(!axe.shown);
    }

    #[test]
    fn straight_out_of_the_car_the_gun_is_shown_at_once_and_no_arm_clip_plays() {
        let (arm, entries) = arm_clips();
        let object = object_clips();
        let clips = Clips { arm: &arm, entries: &entries, object: &object };
        let mut gun = Gun::default();
        gun.take_aim(clips, &cannon(), true);
        assert!(gun.out && gun.shown);
        assert!(gun.arm.is_none());
        assert_eq!(gun.object.map(|o| o.id), Some(id("Idle_WeaponA")));
        assert!(!gun.holds_trigger(clips, true), "the trigger works at once");
        // Put away and taken out again on foot, the arm pulls it out as usual.
        gun.put_away(clips, &cannon());
        gun.take_aim(clips, &cannon(), false);
        assert_eq!(gun.arm.map(|a| arm[a.clip].id.as_str()), Some("Trans_R2W"));
    }

    #[test]
    fn putting_it_away_plays_both_clips_and_hides_the_gun_when_its_own_is_done() {
        let (arm, entries) = arm_clips();
        let object = object_clips();
        let clips = Clips { arm: &arm, entries: &entries, object: &object };
        let mut gun = Gun { out: true, shown: true, ..Default::default() };
        gun.put_away(clips, &cannon());
        assert!(!gun.out);
        assert_eq!(gun.arm.map(|a| arm[a.clip].id.as_str()), Some("Trans_W2R"));
        assert_eq!(gun.object.map(|o| o.id), Some(id("Trans_W2R_WeaponA")));
        run(&mut gun, clips, 19);
        assert!(gun.shown);
        run(&mut gun, clips, 2);
        assert!(!gun.shown, "hidden when its clip of 20 ticks has ended");
        // Taking aim again pulls it out again.
        gun.take_aim(clips, &cannon(), false);
        assert_eq!(gun.arm.map(|a| arm[a.clip].id.as_str()), Some("Trans_R2W"));
    }
}
