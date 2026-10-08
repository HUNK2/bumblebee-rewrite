//! Bumblebee's robot-form weapons: the trigger, the heat, what is fired and what it hits.
//! The rules and numbers are `tf2_core::weapons` (the game's code and data): the trigger
//! comes from the firing state of the control state machine, the aim point is the weapon
//! manager's, and a gun's round is traced from the camera as the original traces it. This
//! file traces them through the arena and scouts, plays their sounds and draws them.
//!
//! What is drawn is all a stand-in (lines for shots, a ball for a missile, a ring where
//! something lands): the game's own beam, missile, flash and impact effects are not decoded.
//! The gun on his arm follows `tf2_core::gun`: his weapon layer's clips and the weapon
//! object's own. The launcher's lock-on and its missile's guidance are `tf2_core::lockon`
//! and `weapons::Missile`; its explosion is `weapons::MissileBlast`. The car's gun is the
//! same rules with the car's own firing state, its wait after the change and its two
//! barrels (`Car`).

use std::path::Path;

use bevy::prelude::*;
use tf2_core::camera::Sight;
use tf2_core::character::CharacterData;
use tf2_core::formats::anim::Clip;
use tf2_core::gun::{Axe, Clips, Gun};
use tf2_core::lockon::{self, LockOn, View};
use tf2_core::melee::Target;
use tf2_core::reticle::{self, Reticle, Sprite};
use tf2_core::aim::AssistTarget;
use tf2_core::tuning::AimTuning;
use tf2_core::weapons::{self, Fired, Kind, MeleeWeapon, Missile, MissileBlast, Part, VehicleWait, Weapon, WeaponDef, WeaponTuning};

use crate::GameData;
use crate::animation::{AnimLibrary, ObjectBlend};
use crate::assets::{game_to_bevy, ObjectNodes};
use crate::effects::{WeaponFx, WeaponFxKind};
use crate::player::{Player, RobotModel, VehicleModel};
use crate::sound::SoundOut;

/// How long a shot's line and a hit's ring stay on screen, in seconds.
const TRACER_SECONDS: f32 = 0.08;
const IMPACT_SECONDS: f32 = 0.35;

struct Held {
    def: WeaponDef,
    tuning: WeaponTuning,
    state: Weapon,
}

struct Tracer {
    from: Vec3,
    to: Vec3,
    age: f32,
}

struct Impact {
    at: Vec3,
    age: f32,
    /// A missile's, drawn larger.
    explosive: bool,
}


/// The next number from 0 to 1 of a small generator.
fn roll(seed: &mut u32) -> f32 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 17;
    *seed ^= *seed << 5;
    (*seed >> 8) as f32 / (1u32 << 24) as f32
}

/// A round that landed on a target in the last tick, for whoever owns the targets.
#[derive(Clone, Copy)]
pub struct Struck {
    pub target: u32,
    pub packet: tf2_core::damage::Packet,
}

/// How far along the line from `from` to `to` (0 to 1) it enters a target's body.
/// [stand-in: the body is one upright cylinder, as for melee; the original traces its
/// collision bodies, each with its own surface (head, upper body...)]
fn enters(from: Vec3, to: Vec3, target: &Target) -> Option<f32> {
    let along = to - from;
    let (flat, offset) = (along.truncate(), (from - target.pos).truncate());
    let a = flat.length_squared();
    let mut best: Option<f32> = None;
    if a > 1e-9 {
        let b = offset.dot(flat);
        let c = offset.length_squared() - target.radius * target.radius;
        let root = b * b - a * c;
        if root >= 0.0 {
            let k = (-b - root.sqrt()) / a;
            let z = from.z + along.z * k - target.pos.z;
            if (0.0..=1.0).contains(&k) && (0.0..=target.height).contains(&z) {
                best = Some(k);
            }
        }
    }
    // Its top, from above.
    let top = target.pos.z + target.height;
    if from.z > top && to.z < top {
        let k = (from.z - top) / (from.z - to.z);
        if ((from + along * k) - target.pos).truncate().length() <= target.radius && best.is_none_or(|b| k < b) {
            best = Some(k);
        }
    }
    best
}

/// The weapon manager's aim point (`weapons::aim_point`) with scouts in the line's way
/// as well as the arena: the original's trace (mask 3) meets characters too. [game]
/// [stand-in: a scout target's body is one upright cylinder]
fn aim_point(eye: Vec3, forward: Vec3, body: Vec3, world: &dyn Sight, targets: &[Target]) -> Vec3 {
    let aim = weapons::aim_point(eye, forward, body, world);
    let from = eye + forward * (body - eye).dot(forward).clamp(0.0, weapons::AIM_REACH);
    let far = eye + forward * weapons::AIM_REACH;
    let mut nearest = (aim.point, aim.distance);
    for target in targets {
        let Some(k) = enters(from, far, target) else { continue };
        let distance = k * (far - from).length();
        if distance < nearest.1 {
            nearest = (from + (far - from) * k, distance);
        }
    }
    nearest.0
}

/// The car's side of a frame of the weapons.
pub struct Car<'a> {
    /// His control mode is the car's (from the start of the change into it), and he is in
    /// the car's form.
    pub mode: bool,
    pub formed: bool,
    /// `vehicleWeaponTimeTilCanFire`.
    pub wait: f32,
    /// Where the barrels of the car's weapon are, and the way the car points, in the
    /// game's space.
    pub barrels: &'a [Vec3],
    pub forward: Vec3,
}

#[derive(Resource)]
pub struct Arsenal {
    held: Vec<Held>,
    /// The car's weapons (`isVehicleModeWeapon`): in hand while his control mode is the
    /// car's, with the manager's wait before they fire.
    car: Vec<Held>,
    car_wait: VehicleWait,
    in_car: bool,
    current: usize,
    switch_was_down: bool,
    /// The missiles in flight: the weapon each left, a number of its own (what its
    /// trail is kept under) and the missile.
    missiles: Vec<(usize, u32, Missile)>,
    launched: u32,
    /// The explosions missiles have made, while their spheres last.
    pub blasts: Vec<MissileBlast>,
    /// The launcher's lock, the targets the aim assist has in its box, and what is drawn
    /// round a target being locked or locked.
    pub lock: LockOn,
    seen: [Option<u32>; 3],
    /// The reticle (`tf2_core::reticle`): its own state, the bracket and the lock's share
    /// found in this frame's search, the seconds since one of his shots hit, the weapons'
    /// icon clock, and the quads of this frame for the HUD to draw.
    reticle: Reticle,
    bracket: Option<reticle::Bracket>,
    lock_shown: f32,
    previous_lock_sound: f32,
    since_hit: f32,
    icon_clock: f32,
    pub sprites: Vec<Sprite>,
    /// A small generator for the scatter of shots.
    seed: u32,
    tracers: Vec<Tracer>,
    impacts: Vec<Impact>,
    pub rounds: u32,
    pub hits: u32,
    /// The targets the gun's rounds landed on in the last tick.
    pub struck: Vec<Struck>,
    /// Arena contacts of rounds, kept separately from character damage packets.
    pub surface_impacts: Vec<(u32,tf2_core::camera::Hit)>,
    /// The effects asked for since `effects::start` last took them: a flash where a
    /// round left, its impact, a missile's explosion.
    pub fx: Vec<WeaponFx>,
    /// His weapon layer and the weapon object's own clips.
    pub gun: Gun,
    /// The melee weapon in his hand, the clip whose events it last followed (by id, and
    /// the tick it had reached), and whether he was in an attack state.
    pub axe: Axe,
    axe_clip: Option<(u32, f32)>,
    was_attacking: bool,
}

/// The clips the gun's rules run on: his weapon layer's set with its entry rules, and the
/// clips the weapon object carries (one object for the cannon and the launcher).
pub fn gun_clips<'a>(library: &'a AnimLibrary, data: &'a CharacterData) -> Clips<'a> {
    Clips {
        arm: library.robot.sets.get(WEAPON_LAYER_SET).map_or(&[], Vec::as_slice),
        entries: library.robot.incoming.get(WEAPON_LAYER_SET).map_or(&[], Vec::as_slice),
        object: data.weapon_animations.first().map_or(&[], Vec::as_slice),
    }
}

pub const WEAPON_LAYER_SET: &str = "WeaponPartialSet";

impl Arsenal {
    pub fn receive_heat(&mut self, packets: impl IntoIterator<Item=(u32,f32)>) {
        for (flags,heat) in packets {
            if flags & tf2_core::damage::flags::OVERHEAT_CURRENT!=0 {
                if self.in_car {for held in &mut self.car {held.state.add_heat(&held.tuning,heat);}}
                else {let held=&mut self.held[self.current]; held.state.add_heat(&held.tuning,heat);}
            } else if flags & tf2_core::damage::flags::OVERHEAT_ALL!=0 {
                for held in &mut self.held {held.state.add_heat(&held.tuning,heat);}
                for held in &mut self.car {held.state.add_heat(&held.tuning,heat);}
            }
        }
    }
    /// His robot-form weapons, or `None` (said on the console) if the install did not give them.
    pub fn load(game_dir: &Path, defs: &[WeaponDef]) -> Option<Arsenal> {
        let tunings = match weapons::load(game_dir, defs) {
            Ok(tunings) => tunings,
            Err(e) => {
                eprintln!("weapons: not loaded ({e})");
                return None;
            }
        };
        for (def, tuning) in defs.iter().zip(&tunings) {
            eprintln!(
                "weapon {:#010x} ({:?}{}): a round every {} s, heat {} a round of {}, damage {}{}",
                def.name,
                def.kind,
                if tuning.vehicle_mode { ", car form" } else { "" },
                tuning.rate_of_fire,
                tuning.heat_per_round,
                tuning.heat_capacity,
                if def.kind == Kind::Gun { tuning.damage } else { tuning.explosive_damage },
                if tuning.missing.is_empty() { String::new() } else { format!("; missing {:?}", tuning.missing) }
            );
        }
        // The car's guns are in the same list (the manager's eight weapons): told apart by
        // `isVehicleModeWeapon`.
        let (car, held): (Vec<Held>, Vec<Held>) = defs
            .iter()
            .zip(tunings)
            .map(|(&def, tuning)| Held { def, tuning, state: Weapon::default() })
            .partition(|held| held.tuning.vehicle_mode);
        if held.is_empty() {
            eprintln!("weapons: his pack lists none for the robot");
            return None;
        }
        Some(Arsenal {
            held,
            car,
            car_wait: VehicleWait::default(),
            in_car: false,
            current: 0,
            switch_was_down: false,
            missiles: Vec::new(),
            launched: 0,
            blasts: Vec::new(),
            lock: LockOn::default(),
            seen: [None; 3],
            reticle: Reticle::default(),
            bracket: None,
            lock_shown: 0.0,
            previous_lock_sound: 0.0,
            since_hit: f32::MAX,
            icon_clock: 0.0,
            sprites: Vec::new(),
            seed: 0x2545_f491,
            tracers: Vec::new(),
            impacts: Vec::new(),
            rounds: 0,
            hits: 0,
            struck: Vec::new(),
            surface_impacts: Vec::new(),
            fx: Vec::new(),
            gun: Gun::default(),
            axe: Axe::default(),
            axe_clip: None,
            was_attacking: false,
        })
    }

    /// One frame of the melee weapon: the weapon events of the clip his control state
    /// plays (`playing`: the clip, its id and its tick), and the put-away when he leaves
    /// an attack state for a state that is not one (`FUN_00878be0`). Its attackRanged
    /// flag skips put-away for firing overlays. [game]
    pub fn tick_axe(&mut self, weapon: &MeleeWeapon, playing: Option<(&Clip, u32, f32)>, attacking: bool, dt: f32) {
        if let Some((clip, id, tick)) = playing {
            let from = self.axe_clip.filter(|&(last, at)| last == id && at <= tick).map(|(_, at)| at);
            self.axe.events(clip, from, tick, weapon.name);
        }
        self.axe_clip = playing.map(|(_, id, tick)| (id, tick));
        if std::mem::replace(&mut self.was_attacking, attacking) && !attacking {
            self.axe.put_away(weapon.exit_clip);
        }
        self.axe.step(&weapon.clips, dt);
    }

    /// The hash of the node the weapon in hand hangs from.
    pub fn bone(&self) -> u32 {
        self.held[self.current].def.bone
    }

    /// Where rounds leave the weapon in hand, in its own space (its `FireDummy` node).
    pub fn muzzle(&self) -> Vec3 {
        self.held[self.current].def.muzzle
    }

    fn random(&mut self) -> f32 {
        roll(&mut self.seed)
    }

    /// The car's weapon: the node of the car it hangs from and its barrels in its own
    /// space (`FireDummy`, `FireDummy01`).
    pub fn car_weapon(&self) -> Option<(u32, &[Vec3])> {
        self.car.first().map(|held| (held.def.bone, &held.def.barrels[..held.def.barrel_count]))
    }

    pub fn is_car_weapon(&self, name: u32) -> bool {
        self.car.iter().any(|held| held.def.name == name)
    }

    /// How far the aim assist looks with the weapon in hand (the aim-assist object's
    /// `+0x30`, set as a weapon is taken in hand: `FUN_007b2c60`).
    pub fn assist_range(&self, aim: &AimTuning) -> f32 {
        let range = self.held[self.current].tuning.aim_assist_max_range;
        if range > 0.0 { range } else { aim.default_max_range }
    }

    /// The target the aim assist has in its box (its best: the first of its three), as the
    /// assist's test wants it. [game] Speed comes from the simulated character body.
    /// [stand-in] The target point still uses cylinder height instead of a posed head.
    pub fn assist_target(&self, targets: &[Target], view: &tf2_core::camera::View,
        velocity: impl Fn(u32)->Vec3) -> Option<AssistTarget> {
        let target = targets.iter().find(|target| Some(target.id) == self.seen[0])?;
        let point = tf2_core::aim::assist_point(target);
        let picture = View::looking(view.eye, view.forward, view.fov, 16.0 / 9.0).project(point)?;
        Some(AssistTarget { point, place: target.pos, velocity: velocity(target.id), off_centre: (picture - lockon::PICTURE * 0.5).length_squared() })
    }

    /// A gun's round: traced from the muzzle towards the point it was sent at over the
    /// weapon's range, landing in the same update (`tracerFirePerRound` is over 0 for his
    /// guns). [game] What it lands on takes the round's damage.
    fn shoot(&mut self, weapon: u32, muzzle: Vec3, at: Vec3, tuning: &WeaponTuning, world: &dyn Sight, targets: &[Target], sound: Option<&SoundOut>) {
        let (from, end) = weapons::bullet_line(muzzle, at, tuning);
        let world_hit = world.sight(from, end);
        let mut landed = world_hit.map(|hit| (hit.point, None));
        for target in targets {
            let Some(k) = enters(from, end, target) else { continue };
            let point = from + (end - from) * k;
            if landed.is_none_or(|(nearest, _)| (point - from).length_squared() < (nearest - from).length_squared()) {
                landed = Some((point, Some(target.id)));
            }
        }
        self.fx.push(WeaponFx { kind: WeaponFxKind::Muzzle, weapon, at: muzzle, along: end - from });
        if let Some((point, target)) = landed {
            if target.is_none() && let Some(hit)=world_hit {
                self.surface_impacts.push((weapon,hit));
            }
            // [stand-in: the arena's walls and ground are the default material]
            let surface = target.and_then(|id| targets.iter().find(|t| t.id == id)).map_or(tf2_core::melee::surface::DEFAULT, |t| t.surface);
            // [assumed] Same synchronous impact as the existing bullet adapter,
            // in place of BulletTrace's original async probe.
            sound.inspect(|s| s.weapon_impact(weapon, surface, point));
            self.fx.push(WeaponFx { kind: WeaponFxKind::Impact(surface), weapon, at: point, along: end - from });
            self.hits += 1;
            self.impacts.push(Impact { at: point, age: 0.0, explosive: false });
            if let Some(target) = target {
                // [stand-in] every part of a scout target cylinder is an upper body.
                let damage = weapons::bullet_damage(tuning, 1.0, Part::UpperBody, (point - muzzle).length(), true);
                self.struck.push(Struck { target, packet: tf2_core::damage::Packet::bullet(tuning, damage, (end-from).normalize_or_zero()) });
            }
        }
        let endpoint=landed.map_or(end,|(point,_)|point);
        if self.is_car_weapon(weapon) {
            // [assumed] synchronous endpoint in place of BulletTrace's async probe.
            // Damage has already landed; only the installed visual emitter travels.
            self.fx.push(WeaponFx {kind:WeaponFxKind::BulletTrail {end:endpoint,speed:tuning.bullet_speed},
                weapon,at:muzzle,along:end-from});
        } else {
            self.tracers.push(Tracer { from: muzzle, to:endpoint, age: 0.0 });
        }
    }

    /// One frame. `aiming`: he is in a weapon state, or in the dodge with the gun out (the
    /// gun comes out on entering a weapon state from any state but those two kinds, and
    /// goes away on leaving one for any other: `FUN_0087f4a0`, `FUN_0087f6f0`); `from_car`:
    /// he was still in the change from the car the update before. `firing` is the firing
    /// state's word that the trigger is held (`State::firing`). `muzzle` is where shots leave him, `body` where he stands, `eye`,
    /// `forward` and `fov` (degrees) the camera, all in the game's space. Gives the pad's
    /// rumble for a round that left: seconds and strength, from the weapon's data.
    #[allow(clippy::too_many_arguments)]
    pub fn tick(
        &mut self,
        aiming: bool,
        from_car: bool,
        clips: Clips,
        firing: bool,
        switch: bool,
        muzzle: Vec3,
        body: Vec3,
        eye: Vec3,
        forward: Vec3,
        fov: f32,
        world: &dyn Sight,
        targets: &[Target],
        car: &Car,
        assist: &AimTuning,
        damage_effects: &tf2_core::damage::Tuning,
        slowed: bool,
        sound: Option<&SoundOut>,
        dt: f32,
    ) -> Option<(f32, f32)> {
        let mut rumble = None;
        self.struck.clear();
        self.surface_impacts.clear();
        let right = forward.cross(Vec3::Z).normalize_or(Vec3::X);
        let up = right.cross(forward);
        // The manager's wait for the car's weapon starts as his control mode becomes the
        // car's. [game] The firing state is the car's own while that lasts.
        self.car_wait.step(car.mode, car.wait, dt);
        self.in_car = car.mode;
        let car_firing = firing && car.mode;
        let firing = firing && !car.mode;
        // Taking aim pulls the gun out and letting go puts it away; while his arm does
        // either, or changes weapon, the trigger does not reach the weapon. [game]
        if aiming {
            self.gun.take_aim(clips, &self.held[self.current].def, from_car);
        } else {
            self.gun.put_away(clips, &self.held[self.current].def);
        }
        if switch && !self.switch_was_down && self.held.len() > 1 {
            self.current = (self.current + 1) % self.held.len();
            // The weapon now in hand starts its icon's clock from nothing.
            self.icon_clock = 0.0;
            self.gun.switched(clips, self.held[self.current].def.name);
        }
        self.switch_was_down = switch;
        let firing = firing && !self.gun.holds_trigger(clips, firing);
        // The aim-assist object's search: what is in the box in the middle of the picture
        // (the launcher's lock box, or for any other weapon the assist's own, 32 pixels
        // square), within the weapon's assist range. The aim assist slows and pulls the
        // aim by its best, and the manager locks the launcher on it while the trigger is
        // held. [game] [stand-in: only the arena hides a target; the picture is taken to
        // be 16:9]
        {
            let held = &self.held[self.current];
            let (t, state) = (&held.tuning, &held.state);
            let view = View { eye, right, forward, up, fov, aspect: 16.0 / 9.0 };
            let locks = aiming && held.def.kind == Kind::Launcher && t.guided;
            self.seen = if aiming {
                let range = if t.aim_assist_max_range > 0.0 { t.aim_assist_max_range } else { assist.default_max_range };
                let facing = (forward * Vec3::new(1.0, 1.0, 0.0)).normalize_or(forward);
                // `FUN_0070a220`, `FUN_0070a2f0`: a launcher with a lock time has its own box.
                let half = if held.def.kind == Kind::Launcher && t.lock_on_time > 0.0 {
                    lockon::lock_box(t)
                } else {
                    Vec2::new(assist.target_box[0] * 640.0, assist.target_box[1] * 360.0)
                };
                lockon::candidates(&view, body, facing, targets, range, lockon::PICTURE * 0.5, half, self.seen[0], |from, to, _| {
                    world.sight(from, to).is_none()
                })
            } else {
                [None; 3]
            };
            if let Some(sound) = sound { sound.combat_target(self.seen[0].is_some()); }
            let pulling = !t.hold_to_lock_on || (firing && state.may_lock(t));
            self.lock.step(t, locks, pulling, &self.seen, targets, &view, dt);
            // The bracket round the target being locked or locked, and how far the lock
            // is, for the reticle (`FUN_007b8420`).
            (self.bracket, self.lock_shown) = (None, 0.0);
            for target in targets {
                let locked = self.lock.locked == Some(target.id);
                if locked || (pulling && self.lock.candidate == Some(target.id)) {
                    self.bracket = reticle::bracket(&view, target.pos, target.height, target.radius, locked);
                    self.lock_shown = if locked { 1.0 } else { self.lock.progress(t) };
                }
            }
        }
        for index in 0..self.held.len() {
            let trigger = firing && index == self.current;
            let def = self.held[index].def;
            let was_locked = self.held[index].state.overheated_left > 0.0;
            let fired = {
                let held = &mut self.held[index];
                held.state.step_status(&held.tuning, trigger, slowed, damage_effects,dt)
            };
            if was_locked && self.held[index].state.overheated_left <= 0.0 {
                sound.inspect(|s| s.stop_event(def.overheat_sound));
                self.gun.cooled(&def);
            }
            let rounds = match fired {
                Fired::Nothing => 0,
                Fired::Refused => {
                    sound.inspect(|s| s.play_event(def.refused_sound));
                    0
                }
                Fired::Rounds(rounds) => rounds,
                Fired::RoundsAndOverheated(rounds) => {
                    sound.inspect(|s| s.play_event(def.overheat_sound));
                    rounds
                }
            };
            for _ in 0..rounds {
                self.gun.fired(clips, def.name);
            }
            if matches!(fired, Fired::RoundsAndOverheated(_)) {
                self.gun.overheated(clips, &def);
            }
            for _ in 0..rounds {
                sound.inspect(|s| s.play_event(def.fire_sound));
                let tuning = &self.held[index].tuning;
                let (noise, shots) = (tuning.player_noise_radius, tuning.projectiles_per_fire.max(1));
                // He shoots at the weapon manager's aim point: what the middle of the
                // picture is on, up to 500 away. [game]
                let target = aim_point(eye, forward, body, world, targets);
                rumble = Some((tuning.rumble_fire_duration, tuning.rumble_fire_strength));
                for _ in 0..shots {
                    let (a, b) = (self.random(), self.random());
                    // The round is sent at the aim point, scattered in the picture. [game]
                    let at = weapons::scattered_point(target, eye, right, up, fov, noise, a, b);
                    let direction = (at - muzzle).normalize_or(forward);
                    self.rounds += 1;
                    match def.kind {
                        // The round is traced from the muzzle towards the scattered point
                        // (the camera only starts a sniping shot's line). [game]
                        Kind::Gun => {
                            let tuning = self.held[index].tuning.clone();
                            self.shoot(def.name, muzzle, at, &tuning, world, targets, sound);
                        }
                        // A missile leaves along the muzzle, which in weapon mode is built
                        // from his aim (`FUN_007a7a40`), sent at the aim point and given
                        // the manager's lock (`FUN_007b36c0`). [game] [assumed: the
                        // camera's forward is his aim, which it follows]
                        Kind::Launcher => {
                            self.fx.push(WeaponFx { kind: WeaponFxKind::Muzzle, weapon: def.name, at: muzzle, along: direction });
                            let seed = &mut self.seed;
                            let missile = Missile::launch(muzzle, forward, at, self.lock.locked, &self.held[index].tuning, &mut || roll(seed));
                            self.launched += 1;
                            sound.inspect(|s| s.projectile_start(self.launched, def.name, missile.position));
                            self.fx.push(WeaponFx { kind: WeaponFxKind::Trail(self.launched), weapon: def.name, at: muzzle, along: forward });
                            self.missiles.push((index, self.launched, missile));
                        }
                    }
                }
            }
        }
        // The car's weapons: the trigger reaches one only in the car's form and once the
        // manager's wait has run out (`FUN_007b44e0`); they cool like the others meanwhile.
        // A firing's projectiles leave one from each barrel in turn, each sent at the point
        // 100 ahead of its own barrel along the way the car points (`useFacingAsTarget`),
        // scattered like any player's round, and traced from the barrel. [game] [assumed:
        // the round is made at the barrel it is aimed from]
        for index in 0..self.car.len() {
            let trigger = car_firing && car.formed && self.car_wait.ready() && index == 0;
            let def = self.car[index].def;
            let was_locked = self.car[index].state.overheated_left > 0.0;
            let fired = {
                let held = &mut self.car[index];
                held.state.step_status(&held.tuning, trigger, slowed, damage_effects,dt)
            };
            if was_locked && self.car[index].state.overheated_left <= 0.0 {
                sound.inspect(|s| s.stop_event(def.overheat_sound));
            }
            let firings = match fired {
                Fired::Nothing => 0,
                Fired::Refused => {
                    sound.inspect(|s| s.play_event(def.refused_sound));
                    0
                }
                Fired::Rounds(rounds) => rounds,
                Fired::RoundsAndOverheated(rounds) => {
                    sound.inspect(|s| s.play_event(def.overheat_sound));
                    rounds
                }
            };
            for _ in 0..firings {
                sound.inspect(|s| s.play_event(def.fire_sound));
                let tuning = self.car[index].tuning.clone();
                rumble = Some((tuning.rumble_fire_duration, tuning.rumble_fire_strength));
                for shot in 0..tuning.projectiles_per_fire.max(1) as usize {
                    // A projectile past the last barrel leaves the first (`FUN_007aab70`).
                    let Some(&barrel) = car.barrels.get(shot).or(car.barrels.first()) else { continue };
                    let target = if tuning.use_facing_as_target {
                        weapons::facing_target(barrel, car.forward)
                    } else {
                        aim_point(eye, forward, body, world, targets)
                    };
                    let (a, b) = (self.random(), self.random());
                    let at = weapons::scattered_point(target, eye, right, up, fov, tuning.player_noise_radius, a, b);
                    self.rounds += 1;
                    self.shoot(def.name, barrel, at, &tuning, world, targets, sound);
                }
            }
        }
        // Missiles in flight steer at the middle of their target, and end at the first
        // thing in their way or when their time is up; where one ends its explosion is
        // made (`weapons::MissileBlast`). [assumed: any touch ends it]
        let mut flying = std::mem::take(&mut self.missiles);
        let mut ended: Vec<(usize, u32, Vec3)> = Vec::new();
        flying.retain_mut(|(index, number, missile)| {
            let tuning = &self.held[*index].tuning;
            let middle = missile
                .target
                .and_then(|id| targets.iter().find(|target| target.id == id))
                .map(|target| target.pos + Vec3::Z * (target.height * 0.5));
            let seed = &mut self.seed;
            let Some((from, to)) = missile.step(tuning, middle, &mut || roll(seed), dt) else {
                // Its time is up: it is killed where it is (`FUN_00785c00`) and goes off.
                ended.push((*index, *number, missile.position));
                return false;
            };
            sound.inspect(|s| s.projectile_move(*number, missile.position, (to - from).length() / dt.max(f32::EPSILON)));
            let mut landed = world.sight(from, to).map(|hit| (hit.point, None));
            for target in targets {
                let Some(k) = enters(from, to, target) else { continue };
                let point = from + (to - from) * k;
                if landed.is_none_or(|(nearest, _)| (point - from).length_squared() < (nearest - from).length_squared()) {
                    landed = Some((point, Some(target.id)));
                }
            }
            let Some((point, target)) = landed else { return true };
            self.hits += u32::from(target.is_some());
            ended.push((*index, *number, point));
            false
        });
        self.missiles = flying;
        for (index, number, at) in ended {
            sound.inspect(|s| s.projectile_end(number, self.held[index].def.name, at));
            self.impacts.push(Impact { at, age: 0.0, explosive: true });
            self.fx.push(WeaponFx { kind: WeaponFxKind::Blast, weapon: self.held[index].def.name, at, along: Vec3::Z });
            let held = &self.held[index];
            // [stand-in] a launcher whose data has no explosion does nothing where it lands.
            if let Some(def) = held.def.explosion {
                let mut blast = MissileBlast::new(def, &held.tuning, at);
                let hits = blast.first(targets);
                self.explode(hits);
                self.blasts.push(blast);
            }
        }
        let mut blasts = std::mem::take(&mut self.blasts);
        blasts.retain_mut(|blast| {
            let (hits, alive) = blast.step(targets, dt);
            self.explode(hits);
            alive
        });
        self.blasts = blasts;
        for tracer in &mut self.tracers {
            tracer.age += dt;
        }
        for impact in &mut self.impacts {
            impact.age += dt;
        }
        self.tracers.retain(|tracer| tracer.age < TRACER_SECONDS);
        self.impacts.retain(|impact| impact.age < IMPACT_SECONDS);
        self.gun.step(clips, self.held[self.current].def.name, dt);
        // The reticle. [assumed: the hit mark's clock starts as one of his shots takes a
        // target's health]
        // [game] a weapon's icon clock (its word 0xf6) runs while it is the manager's
        // weapon in hand, and is 0 otherwise (slot `+0x50` = `FUN_007a7470`, asked through
        // `FUN_007b5030`): so only the weapon in hand has an icon, and it comes up afresh
        // on a switch and on leaving the car (in the car the weapon in hand is the car's).
        // [assumed] his weapons' definitions have `+0x5d` and `+0x9c` at 0, the two other
        // things `FUN_007a7470` wants.
        self.since_hit = if self.struck.is_empty() { self.since_hit + dt } else { 0.0 };
        self.icon_clock = if car.mode { 0.0 } else { self.icon_clock + dt };
        let side = |held: &Held, shown_for: f32| reticle::Side {
            kind: held.def.kind,
            heat: held.state.heat,
            capacity: held.tuning.heat_capacity,
            charged_shot: false,
            projectiles: held.tuning.projectiles_per_fire,
            shown_for,
        };
        let view = View { eye, right, forward, up, fov, aspect: 16.0 / 9.0 };
        let car_gun = self.car.first().filter(|_| car.mode && !car.barrels.is_empty());
        let frame = match car_gun {
            // The car's gun: its two bases lie 30 and 50 ahead of it. [stand-in: ahead
            // of the middle of its barrels; the original takes the node `Flipper`]
            Some(gun) => {
                let from = car.barrels.iter().sum::<Vec3>() / car.barrels.len() as f32;
                reticle::Frame {
                    primary: Some(side(gun, 0.0)),
                    current: Some(gun.def.kind),
                    since_hit: self.since_hit,
                    car: Some((view.project(from + car.forward * 30.0), view.project(from + car.forward * 50.0))),
                    ..Default::default()
                }
            }
            None => reticle::Frame {
                weapon_mode: aiming,
                primary: self.held.first().map(|held| side(held, if self.current == 0 { self.icon_clock } else { 0.0 })),
                secondary: self.held.get(1).map(|held| side(held, if self.current == 1 { self.icon_clock } else { 0.0 })),
                current: Some(self.held[self.current].def.kind),
                on_target: aiming && self.seen[0].is_some(),
                lock: self.lock_shown,
                bracket: self.bracket,
                since_hit: self.since_hit,
                car: None,
            },
        };
        if let Some(id) = reticle::lock_sound(self.previous_lock_sound, frame.lock) {
            sound.inspect(|s| s.play_event(id));
        }
        self.previous_lock_sound = frame.lock;
        self.sprites = self.reticle.draw(&frame, dt);
        rumble
    }

    /// What an explosion's sphere reached takes its damage (no throw: the flags his
    /// missile's explosion carries are not a throwing kind, `Hit::throws`).
    fn explode(&mut self, hits: Vec<weapons::BlastHit>) {
        for hit in hits {
            self.struck.push(Struck { target: hit.target, packet: tf2_core::damage::Packet {
                amount: hit.damage, flags: hit.flags, direction: hit.direction.extend(0.0),
                knockback_speed: hit.knock_back_speed, knockback_angle: hit.knock_back_angle,
                ..Default::default()
            }});
        }
    }

    /// Where the missile with this number is and the way it flies, while it flies.
    pub fn missile(&self, number: u32) -> Option<(Vec3, Vec3)> {
        self.missiles.iter().find(|(_, n, _)| *n == number).map(|(_, _, missile)| (missile.position, missile.direction))
    }

    /// For the readout: which weapon, its heat, and what has been fired.
    pub fn describe(&self) -> String {
        let held = &self.held[self.current];
        let heat = held.state.heat_fraction(&held.tuning);
        let mut line = format!("{:?} ({} of {}), heat {:3.0}%", held.def.kind, self.current + 1, self.held.len(), heat * 100.0);
        if held.state.overheated_left > 0.0 {
            line += &format!(" (overheated, locked {:.1} s)", held.state.overheated_left);
        }
        if let Some(car) = self.car.first().filter(|_| self.in_car) {
            line = format!("car gun, heat {:3.0}%", car.state.heat_fraction(&car.tuning) * 100.0);
            if self.car_wait.left() > 0.0 {
                line += &format!(" (ready in {:.2} s)", self.car_wait.left());
            }
            if car.state.overheated_left > 0.0 {
                line += &format!(" (overheated, locked {:.1} s)", car.state.overheated_left);
            }
        }
        line += &format!("; {} rounds fired, {} hit", self.rounds, self.hits);
        // The gun and the axe, for captures: shown or not, and the weapon layer's clip
        // (its place in `WeaponPartialSet`), tick and share.
        line += if self.gun.shown { "; gun out" } else { "; gun away" };
        if let Some(arm) = self.gun.arm {
            line += &format!(", arm clip {} at {:.0} ({:.2})", arm.clip, arm.tick, arm.weight());
        }
        if self.axe.shown {
            line += "; axe out";
        }
        if !self.missiles.is_empty() {
            line += &format!(", {} missiles flying", self.missiles.len());
        }
        if let Some(id) = self.lock.locked {
            line += &format!("; locked on {id}");
        } else if let Some(id) = self.lock.candidate {
            line += &format!("; locking on {id} ({:.2} s)", self.lock.held);
        }
        line
    }

    /// A reset: nothing in flight, nothing hot.
    pub fn reset(&mut self, sound: Option<&SoundOut>) {
        for held in self.held.iter_mut().chain(&mut self.car) {
            held.state = Weapon::default();
            sound.inspect(|s| s.stop_event(held.def.overheat_sound));
        }
        (self.car_wait, self.in_car) = (VehicleWait::default(), false);
        self.missiles.clear();
        self.blasts.clear();
        (self.lock, self.seen) = (LockOn::default(), [None; 3]);
        self.previous_lock_sound = 0.0;
        (self.reticle, self.bracket, self.lock_shown, self.since_hit, self.icon_clock) = (Reticle::default(), None, 0.0, f32::MAX, 0.0);
        self.sprites.clear();
        self.tracers.clear();
        self.impacts.clear();
        (self.rounds, self.hits) = (0, 0);
        self.gun = Gun::default();
        (self.axe, self.axe_clip, self.was_attacking) = (Axe::default(), None, false);
    }
}

/// A weapon's own model (the game's object, from his pack), spawned once for each weapon:
/// which weapon it is, and the node of the robot it hangs from (`startBoneAttachment`,
/// which `FUN_007a6380` looks up on the owner). [data]
#[derive(Component)]
pub struct WeaponModel {
    pub name: u32,
    pub bone: u32,
}

/// The node his hand weapons hang from while they are out. `startBoneAttachment` is only
/// where a weapon is linked when it is made (the cannon's is `ObjectAttachment_Hand_R`);
/// the clips that bring the gun out, change it and fire it (`Trans_R2W`, `Weapon_Switch`,
/// `Weapon_Recoil`) each carry a `WeaponAttachmentEvent` for the cannon and one for the
/// launcher on this node's channel, and the event's handler links the weapon's object to
/// the node the event sits on (`FUN_007b4f20`) and unhides it. [data] [game]
const WEAPON_NODE: &str = "WeaponAttachment_Arm_R";

/// Hangs each weapon's model on the robot's weapon node with the node's own axes: the
/// original's link (`lux::LinkConstraint`, update `FUN_0056c2f0`) gives the object the
/// node's matrix, and the manager makes it with no offset (`FUN_007b4f20` passes the
/// identity). [game] The node has a channel of its own in the weapon clips, lying along
/// the forearm, and its roll (11 degrees in `Weapon_Idle`) is the tilt the gun's parts are
/// modelled with, taken off again. A weapon the robot has no node for (the car's gun: its
/// `startBoneAttachment` is on the car) is left unattached and hidden.
pub fn attach_models(
    mut commands: Commands,
    robot: Single<&ObjectNodes, With<RobotModel>>,
    vehicle: Option<Single<&ObjectNodes, (With<VehicleModel>, Without<RobotModel>)>>,
    models: Query<(Entity, &WeaponModel)>,
) {
    let weapon_node = tf2_core::formats::hash::crc32(WEAPON_NODE.as_bytes());
    let node_of = |name: u32| robot.name_hashes.iter().position(|&hash| hash == name);
    for (entity, model) in &models {
        if node_of(model.bone).is_none() {
            // The car's gun hangs on a node of the car (`WheelWell_Front_R`), by the same
            // link. [game]
            let on_car = vehicle.as_ref().and_then(|car| Some(car.entities[car.name_hashes.iter().position(|&hash| hash == model.bone)?]));
            if let Some(node) = on_car {
                commands.entity(node).add_child(entity).insert(Transform::IDENTITY);
            }
            continue;
        }
        let Some(node) = node_of(weapon_node) else { continue };
        commands.entity(robot.entities[node]).add_child(entity).insert(Transform::IDENTITY);
    }
}

/// Shows the weapon's object when the gun's rules have it shown (the event of the arm's
/// pull-out clip unhides it; the end of its own put-away clip hides it), and poses it by
/// the clip of its own that is playing. His cannon and launcher are ONE object in the
/// original, so one model is drawn for both: the first weapon's. [game] [data]
pub fn show_models(
    time: Res<Time>,
    arsenal: Res<Arsenal>,
    data: Res<GameData>,
    player: Single<&Player>,
    mut models: Query<(&WeaponModel, &ObjectNodes, &mut Visibility)>,
    mut transforms: Query<&mut Transform, Without<WeaponModel>>,
    mut effects: ResMut<crate::effects::Effects>,
    mut last: Local<(u32, f32)>,
    mut blend: Local<ObjectBlend>,
) {
    let name = arsenal.held[0].def.name;
    let gun = &arsenal.gun;
    for (model, nodes, mut visibility) in &mut models {
        // The car's weapon is shown while it is in hand, which is while his control mode
        // is the car's (`FUN_007b43b0` -> `FUN_007b2c60` -> the weapon's slot `+0x24`);
        // it hangs on the car, which is drawn only in the car's form. [game]
        if arsenal.is_car_weapon(model.name) {
            *visibility = if player.state.car_mode { Visibility::Inherited } else { Visibility::Hidden };
            continue;
        }
        let mine = model.name == name;
        *visibility = if mine && gun.shown && !player.state.is_vehicle() { Visibility::Inherited } else { Visibility::Hidden };
        if !mine {
            continue;
        }
        let playing = gun.object.and_then(|object| {
            let clips = data.0.weapon_animations.first()?;
            let r = clips.iter().find(|r| tf2_core::formats::hash::crc32(r.id.as_bytes()) == object.id)?;
            Some((r, r.clip.as_ref()?, object.tick))
        });
        let pose = blend.pose(data.0.weapon_animations.first().map_or(&[], Vec::as_slice), gun.object.map_or(0, |o| o.id),
            gun.object.map_or(0.0, |o| o.tick), time.delta_secs(), nodes);
        // The clip's effect events (the overheat clip's smoke), on the gun's own nodes.
        let id = playing.as_ref().map_or(0, |_| gun.object.map_or(0, |object| object.id));
        if last.0 != id {
            effects.thing_clip_changed(nodes);
        }
        if let Some((_, clip, tick)) = playing {
            // Keep the previous phase across a loop: to < from processes its tail,
            // including the overheat smoke's stop key just before the loop boundary.
            let from = if last.0 == id { last.1 } else { 0.0 };
            effects.thing_clip_events(clip, from, tick, nodes);
            *last = (id, tick);
        } else {
            *last = (0, 0.0);
        }
        // Node 0 is the object itself, which hangs on his arm's node: a clip's root is
        // not put on it (the link overwrites the object's own matrix in the original).
        for (node, transform) in nodes.entities.iter().zip(&pose).skip(1) {
            if let Ok(mut placed) = transforms.get_mut(*node) {
                *placed = *transform;
            }
        }
    }
}

/// The melee weapon's own model (the game's object, from his pack), and the node of his
/// it hangs from now (CRC-32 of the node's name).
#[derive(Component)]
pub struct AxeModel {
    pub hung: u32,
}

/// Shows the axe when its rules have it shown, hangs it on the node its last event sat on
/// (the link is the one the guns have, `FUN_007b4f20`: the node's own axes, no offset)
/// and poses it by the clip of its own that is playing. [game] [data]
pub fn show_axe(
    time: Res<Time>,
    mut commands: Commands,
    arsenal: Res<Arsenal>,
    data: Res<GameData>,
    player: Single<&Player>,
    robot: Single<&ObjectNodes, With<RobotModel>>,
    mut models: Query<(Entity, &mut AxeModel, &ObjectNodes, &mut Visibility), Without<RobotModel>>,
    mut transforms: Query<&mut Transform, Without<AxeModel>>,
    mut blend: Local<ObjectBlend>,
) {
    let Some(weapon) = &data.0.melee_weapon else { return };
    let axe = &arsenal.axe;
    for (entity, mut model, nodes, mut visibility) in &mut models {
        if axe.node != 0 && model.hung != axe.node {
            if let Some(node) = robot.name_hashes.iter().position(|&hash| hash == axe.node) {
                commands.entity(robot.entities[node]).add_child(entity).insert(Transform::IDENTITY);
                model.hung = axe.node;
            }
        }
        let hung = model.hung == axe.node && axe.node != 0;
        *visibility = if axe.shown && hung && !player.state.is_vehicle() { Visibility::Inherited } else { Visibility::Hidden };
        let pose = blend.pose(&weapon.clips, axe.object.map_or(0, |o| o.id), axe.object.map_or(0.0, |o| o.tick), time.delta_secs(), nodes);
        // As for the gun: node 0 is the object itself, which the link places.
        for (node, transform) in nodes.entities.iter().zip(&pose).skip(1) {
            if let Ok(mut placed) = transforms.get_mut(*node) {
                *placed = *transform;
            }
        }
    }
}

/// Draws what is in flight and what has just landed. [stand-in]
pub fn draw(arsenal: Res<Arsenal>, mut gizmos: Gizmos) {
    for tracer in &arsenal.tracers {
        let fade = 1.0 - tracer.age / TRACER_SECONDS;
        gizmos.line(game_to_bevy(tracer.from), game_to_bevy(tracer.to), Color::srgba(1.0, 0.85, 0.3, fade));
    }
    for (_, _, missile) in &arsenal.missiles {
        let at = game_to_bevy(missile.position);
        gizmos.sphere(at, 0.35, Color::srgb(1.0, 0.55, 0.15));
        gizmos.line(at, at - game_to_bevy(missile.direction) * 3.0, Color::srgba(1.0, 0.8, 0.5, 0.6));
    }
    // An explosion's sphere as it grows. [stand-in: the original's is its particles, light
    // and the sound]
    for blast in &arsenal.blasts {
        gizmos.sphere(game_to_bevy(blast.centre), blast.radius, Color::srgb(1.0, 0.7, 0.2));
    }
    for impact in &arsenal.impacts {
        let grown = impact.age / IMPACT_SECONDS;
        let radius = if impact.explosive { 1.0 + 5.0 * grown } else { 0.2 + 0.6 * grown };
        gizmos.sphere(game_to_bevy(impact.at), radius, Color::srgba(1.0, 0.6, 0.2, 1.0 - grown));
    }
}
