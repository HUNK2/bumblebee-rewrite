//! A character's guns and launchers: which he has, their numbers, and the rules for firing,
//! heat and what is fired. No engine types; space is the game's (x right, y forward, z up).
//!
//! The numbers are the game's own, read from the install: the character's pack names each
//! weapon and its sounds, and `bnxglobal.str` holds the attribute block each weapon points
//! at (`handoff\result-01.md` lists them). HOW the numbers are used is taken from their
//! names, except the heat meter, which is read from the weapon's code. Each rule says so:
//!
//!   [game]     read from the game's code
//!
//!   [data]     a value from the game's data, used as its name says
//!   [assumed]  our reading of what a value is for
//!   [stand-in] invented so the weapon can be used; to be replaced

use std::path::Path;

use glam::Vec3;

use crate::formats::lxb::{DataFile, Node};
use crate::formats::pack::{self, Pack};
use crate::formats::scene::ExplosionDef;
use crate::melee;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Fires bullets (for Bumblebee, the solar plasma cannon).
    Gun,
    /// Fires missiles.
    Launcher,
}

/// A weapon as the character's pack describes it. Names are ids: the CRC-32 of the name.
#[derive(Clone, Copy, Debug)]
pub struct WeaponDef {
    pub kind: Kind,
    pub name: u32,
    /// The root in `bnxglobal.str` that holds its numbers.
    pub attribute: u32,
    /// The node of the robot it is fixed to.
    pub bone: u32,
    pub fire_sound: u32,
    pub overheat_sound: u32,
    /// Played when the trigger is pulled while it is overheated.
    pub refused_sound: u32,
    /// Where rounds leave it, in the weapon object's own space: the rest place of its
    /// node `FireDummy`, `FireDummy01`... numbered `minFireDummyIndex` (the weapon's word
    /// 0xcc; `FUN_007a7a40` takes each barrel's place from these nodes). Zero when the
    /// object has no such node. [data] Only the first barrel: his weapons that fire from
    /// the robot have one.
    pub muzzle: Vec3,
    /// Every barrel the object has of those numbered `minFireDummyIndex` to
    /// `maxFireDummyIndex`, in order, and how many (`+0x324`): a firing's projectiles leave
    /// one from each in turn (`FUN_007ae010`; the car's gun has two, `FireDummy` and
    /// `FireDummy01`, though its data asks for three). [data]
    pub barrels: [Vec3; 3],
    pub barrel_count: usize,
    /// The clips of the weapon's own object that its code starts, by id, 0 for none
    /// (`idleAnimation`, `exitAnimation`, `exitOverheatAnimation`, `overheatAnimation`;
    /// weapon `+0x3ac`, `+0x3b0`, `+0x3b8`, `+0x3c0`), and the clip of his weapon layer
    /// that goes with putting it away (`exitCharacterAnimation`, `+0x3b4`). [data]
    pub idle_clip: u32,
    pub exit_clip: u32,
    pub exit_overheat_clip: u32,
    pub overheat_clip: u32,
    pub exit_arm_clip: u32,
    /// `alwaysAppear`: it is not hidden once it is put away.
    pub always_appear: bool,
    /// What a launcher's missile lets go of where it ends (`missileProjectileObj`). [data]
    pub explosion: Option<ExplosionDef>,
}

/// The names of a weapon's barrel nodes by number (the table at `00c0675c`).
const FIRE_DUMMIES: [u32; 3] = [0xeffc_a07e, 0xa82b_3c5a, 0x3122_6de0];

/// The rest place of the weapon object's barrel node numbered `index`.
fn fire_dummy(object: Option<Node>, index: usize) -> Option<Vec3> {
    let object = object?;
    let wanted = *FIRE_DUMMIES.get(index)?;
    let names = object.path(&["hierarchy", "names"])?.ints();
    let node = names.iter().position(|&name| name as u32 == wanted)?;
    if node == 0 {
        return Some(Vec3::ZERO);
    }
    let rest = object.get("sub_matrices")?.at(node - 1)?.floats();
    (rest.len() == 12).then(|| Vec3::new(rest[9], rest[10], rest[11]))
}

/// The weapons a `Character` object lists, in its own order (primary first).
pub fn read_defs(character: Node) -> Vec<WeaponDef> {
    let id = |node: Option<Node>| node.and_then(Node::int).map_or(0, |h| h as u32);
    let mut defs = Vec::new();
    for file in character.get("weapons").into_iter().flat_map(|weapons| weapons.items()) {
        let attached = file.path(&["obj", "attachments"]).into_iter().flat_map(|a| a.items());
        for thing in attached.filter_map(|a| a.get("thing")) {
            let (kind, attribute) = match thing.type_name() {
                Some("GunWeapon") => (Kind::Gun, "gunAttribute"),
                Some("MissileLauncher") => (Kind::Launcher, "missileLauncherAttribute"),
                _ => continue,
            };
            let sound = |field: &str| id(thing.path(&["genericWeaponSoundTable", field]));
            let (first, last) = (id(thing.get("minFireDummyIndex")) as usize, id(thing.get("maxFireDummyIndex")) as usize);
            let mut barrels = [Vec3::ZERO; 3];
            let mut barrel_count = 0;
            for index in first..=last.min(FIRE_DUMMIES.len() - 1) {
                if let Some(at) = fire_dummy(file.get("obj"), index) {
                    barrels[barrel_count] = at;
                    barrel_count += 1;
                }
            }
            defs.push(WeaponDef {
                barrels,
                barrel_count,
                kind,
                name: id(thing.get("weaponName")),
                attribute: id(thing.get(attribute)),
                bone: id(thing.get("startBoneAttachment")),
                fire_sound: sound("fireSound"),
                overheat_sound: sound("overHeatSound"),
                refused_sound: sound("fireOverHeatedSound"),
                muzzle: fire_dummy(file.get("obj"), id(thing.get("minFireDummyIndex")) as usize).unwrap_or(Vec3::ZERO),
                idle_clip: id(thing.get("idleAnimation")),
                exit_clip: id(thing.get("exitAnimation")),
                exit_overheat_clip: id(thing.get("exitOverheatAnimation")),
                overheat_clip: id(thing.get("overheatAnimation")),
                exit_arm_clip: id(thing.get("exitCharacterAnimation")),
                always_appear: thing.get("alwaysAppear").is_some_and(|f| f.int() == Some(1) || f.is_true()),
                explosion: thing.path(&["missileProjectileObj", "obj"]).and_then(crate::formats::scene::read_projectile_explosion),
            });
        }
    }
    defs
}

/// Each weapon's own object (its node tree and rendered parts), in the order of
/// `read_defs`; `None` where the weapon's file has none that reads.
pub fn read_objects<'a>(character: Node<'a>, name_of: impl Fn(u32) -> Option<&'a str> + Copy) -> Vec<Option<crate::formats::scene::ObjectDef>> {
    let mut objects = Vec::new();
    for file in character.get("weapons").into_iter().flat_map(|weapons| weapons.items()) {
        let attached = file.path(&["obj", "attachments"]).into_iter().flat_map(|a| a.items());
        for thing in attached.filter_map(|a| a.get("thing")) {
            if matches!(thing.type_name(), Some("GunWeapon" | "MissileLauncher")) {
                objects.push(file.get("obj").and_then(|object| crate::formats::scene::read_object(object, name_of)));
            }
        }
    }
    objects
}

/// Each weapon object's own clips (its `AnimRefBundle`: `Idle_WeaponA`, `Idle_WeaponB`,
/// `Overheat_Weapon_A`, `Overheat_Weapon_B` for his), in the order of `read_defs`. [data]
pub fn read_object_clips(data: &crate::formats::lxb::DataFile, character: Node) -> Vec<Vec<crate::formats::anim::AnimRef>> {
    let mut clips = Vec::new();
    for file in character.get("weapons").into_iter().flat_map(|weapons| weapons.items()) {
        let attached = || file.path(&["obj", "attachments"]).into_iter().flat_map(|a| a.items()).filter_map(|a| a.get("thing"));
        for thing in attached() {
            if matches!(thing.type_name(), Some("GunWeapon" | "MissileLauncher")) {
                let bundles = attached().filter(|t| t.type_name() == Some("AnimRefBundle"));
                clips.push(bundles.flat_map(|bundle| crate::formats::anim::read_bundle(data, bundle)).collect());
            }
        }
    }
    clips
}

/// His melee weapon (the axe): a weapon of the manager's list like the guns, with an object
/// of its own (11 nodes: six handle links and a blade in five parts), its clips
/// (`Trans_R2W` unfolding it, `Idle`, `Trans_W2R`) and no numbers. [data]
pub struct MeleeWeapon {
    /// `weaponName` (`MeleeWeapon`), which the attack clips' events name.
    pub name: u32,
    /// `exitAnimation`: what its object plays when it is put away (`Trans_W2R`).
    pub exit_clip: u32,
    pub object: Option<crate::formats::scene::ObjectDef>,
    pub clips: Vec<crate::formats::anim::AnimRef>,
}

/// The character's melee weapon, if one of its `weapons` is one.
pub fn read_melee<'a>(
    data: &crate::formats::lxb::DataFile,
    character: Node<'a>,
    name_of: impl Fn(u32) -> Option<&'a str> + Copy,
) -> Option<MeleeWeapon> {
    let id = |node: Option<Node>| node.and_then(Node::int).map_or(0, |h| h as u32);
    for file in character.get("weapons").into_iter().flat_map(|weapons| weapons.items()) {
        let attached = || file.path(&["obj", "attachments"]).into_iter().flat_map(|a| a.items()).filter_map(|a| a.get("thing"));
        let Some(thing) = attached().find(|thing| thing.type_name() == Some("MeleeWeapon")) else { continue };
        let bundles = attached().filter(|t| t.type_name() == Some("AnimRefBundle"));
        return Some(MeleeWeapon {
            name: id(thing.get("weaponName")),
            exit_clip: id(thing.get("exitAnimation")),
            object: file.get("obj").and_then(|object| crate::formats::scene::read_object(object, name_of)),
            clips: bundles.flat_map(|bundle| crate::formats::anim::read_bundle(data, bundle)).collect(),
        });
    }
    None
}

/// A weapon's numbers, named as the data names them.
#[derive(Clone, Default, Debug)]
pub struct WeaponTuning {
    /// Seconds between shots.
    pub rate_of_fire: f32,
    pub automatic_firing: bool,
    pub projectiles_per_fire: u32,
    pub heat_capacity: f32,
    pub heat_per_round: f32,
    pub heat_decrement_per_second: f32,
    pub heat_cooldown_til_decrement: f32,
    pub overheat_period: f32,
    /// How far a player's shots scatter about the aim point, as a share of half the
    /// picture's height at that distance (`playerNoiseRadius`; see `scattered_point`).
    pub player_noise_radius: f32,
    pub general_noise_radius: f32,
    /// The pad's rumble as a round leaves: seconds and strength (`rumbleFireDuration`,
    /// `rumbleFireStrength`; the weapon's fire calls `FUN_00717be0(1, ...)` with them).
    pub rumble_fire_duration: f32,
    pub rumble_fire_strength: f32,
    /// `mustHoldFireTriggerToLockOn` (launchers): the round leaves when the trigger is let
    /// go, not while it is held.
    pub hold_to_lock_on: bool,
    // Guns.
    pub bullet_speed: f32,
    pub bullet_range: f32,
    pub damage: f32,
    pub knock_back_speed: f32,
    pub damage_flags: u32,
    pub knock_back_angle: f32,
    pub stun_time: f32,
    pub slow_time: f32,
    pub dot_time: f32,
    pub heat_damage: f32,
    /// `bulletDamageFallOffRange`: from this far the damage falls to `minDamage` at the
    /// range; negative (his cannon's -1) for none.
    pub bullet_fall_off_range: f32,
    pub min_damage: f32,
    /// `tracerFirePerRound`: over 0 (all his guns), the round is traced over its whole
    /// range and lands in the update it is fired; else it flies at `bulletSpeed`.
    pub tracer_fire_per_round: i32,
    /// `positionalDamageMultiplier`: head, upper body, lower body, vehicle body.
    pub positional: [f32; 4],
    // Launchers.
    pub missile_start_speed: f32,
    pub missile_top_speed: f32,
    pub missile_acceleration: f32,
    pub missile_lifetime: f32,
    pub explosive_damage: f32,
    /// `overwriteExplosiveMinDamage`: -1 (his) leaves the explosion's own.
    pub explosive_min_damage: f32,
    pub lock_on_time: f32,
    /// `guided`: it steers at the target it was locked on.
    pub guided: bool,
    /// `lockOnBracketStayOnTime`: how long a lock outlasts its target leaving the box.
    pub lock_bracket_stay: f32,
    /// `missileLockOnBoxWidth` and `Height`, in pixels of a 1280 x 720 picture.
    pub lock_box: [f32; 2],
    /// `aimAssistMaxRange` (every kind of weapon has one: his cannon 300, his missiles
    /// 350, the car's gun 500): how far from the camera a target may be for the aim assist
    /// and the lock; 0 for the shared default.
    pub aim_assist_max_range: f32,
    /// `minGuidanceDelay`, `maxGuidanceDelay`: seconds before it steers at all.
    pub guidance_delay: [f32; 2],
    /// `minGuidanceOffDelay`, `maxGuidanceOffDelay`: seconds of steering before it gives
    /// up; negative (his) for never.
    pub guidance_off_delay: [f32; 2],
    /// The cosine of `guidedViewAngle` (the loader stores the cosine).
    pub guided_view_cos: f32,
    /// `outOfViewTurnSpeed`, in radians a second.
    pub out_of_view_turn: f32,
    /// `minAccelFactorDelay`, `maxAccelFactorDelay`, and `accelFactor`.
    pub accel_factor_delay: [f32; 2],
    pub accel_factor: f32,
    /// `minWobbleTimer`, `maxWobbleTimer`; `maxWobbleAngle` in radians; `minWobbleDuration`,
    /// `maxWobbleDuration`.
    pub wobble_timer: [f32; 2],
    pub wobble_angle: f32,
    pub wobble_duration: [f32; 2],
    /// The cosine of `pastTargetConeAngleActivated`.
    pub past_target_cos: f32,
    /// `isVehicleModeWeapon`: fitted to the car, not the robot.
    pub vehicle_mode: bool,
    /// `useFacingAsTarget` (attributes `+0x60`, the weapon's slot `+0xa8`): each round is
    /// sent at a point straight ahead of its own barrel, not at the manager's aim point
    /// (`FUN_007ae010`, `FUN_007aab70`). The car's gun.
    pub use_facing_as_target: bool,
    /// Values the data did not have.
    pub missing: Vec<String>,
}

fn tuning_from(node: Node, kind: Kind) -> WeaponTuning {
    let mut missing = Vec::new();
    let mut num = |name: &str| match node.get(name).and_then(Node::float) {
        Some(v) => v,
        None => {
            missing.push(name.to_owned());
            0.0
        }
    };
    let mut t = WeaponTuning {
        rate_of_fire: num("rateOfFire"),
        projectiles_per_fire: num("numProjectilesPerFire").max(1.0) as u32,
        heat_capacity: num("heatMeterCapacity"),
        heat_per_round: num("heatMeterIncrementPerRound"),
        heat_decrement_per_second: num("heatMeterDecrementPerSecond"),
        heat_cooldown_til_decrement: num("heatMeterCooldownTilDecrement"),
        overheat_period: num("heatMeterOverHeatPeriod"),
        rumble_fire_duration: num("rumbleFireDuration"),
        rumble_fire_strength: num("rumbleFireStrength"),
        aim_assist_max_range: num("aimAssistMaxRange"),
        ..Default::default()
    };
    match kind {
        Kind::Gun => {
            t.player_noise_radius = num("playerNoiseRadius");
            t.general_noise_radius = num("generalNoiseRadius");
            t.bullet_speed = num("bulletSpeed");
            t.bullet_range = num("bulletRange");
            t.damage = num("damage");
            t.bullet_fall_off_range = num("bulletDamageFallOffRange");
            t.min_damage = num("minDamage");
            t.tracer_fire_per_round = num("tracerFirePerRound") as i32;
            let part = |name: &str| node.path(&["positionalDamageMultiplier", name]).and_then(Node::float).unwrap_or(1.0);
            t.positional = [part("head"), part("upperBody"), part("lowerBody"), part("vehicleBody")];
        }
        Kind::Launcher => {
            t.missile_start_speed = num("missileStartSpeed");
            t.missile_top_speed = num("missileTopSpeed");
            t.missile_acceleration = num("missileAcceleration");
            t.missile_lifetime = num("missileLifetime");
            t.explosive_damage = num("overwriteExplosiveDamage");
            t.explosive_min_damage = num("overwriteExplosiveMinDamage");
            t.lock_on_time = num("missileCrosshairLockOnTime");
            t.guided = node.get("guided").is_some_and(Node::is_true);
            t.lock_bracket_stay = num("lockOnBracketStayOnTime");
            t.lock_box = [num("missileLockOnBoxWidth"), num("missileLockOnBoxHeight")];
            t.guidance_delay = [num("minGuidanceDelay"), num("maxGuidanceDelay")];
            t.guidance_off_delay = [num("minGuidanceOffDelay"), num("maxGuidanceOffDelay")];
            t.guided_view_cos = num("guidedViewAngle").to_radians().cos();
            t.out_of_view_turn = num("outOfViewTurnSpeed").to_radians();
            t.accel_factor_delay = [num("minAccelFactorDelay"), num("maxAccelFactorDelay")];
            t.accel_factor = num("accelFactor");
            t.wobble_timer = [num("minWobbleTimer"), num("maxWobbleTimer")];
            t.wobble_angle = num("maxWobbleAngle").to_radians();
            t.wobble_duration = [num("minWobbleDuration"), num("maxWobbleDuration")];
            t.past_target_cos = num("pastTargetConeAngleActivated").to_radians().cos();
            t.hold_to_lock_on = node.get("mustHoldFireTriggerToLockOn").is_some_and(Node::is_true);
        }
    }
    t.automatic_firing = node.get("automaticFiring").is_some_and(Node::is_true);
    t.vehicle_mode = node.get("isVehicleModeWeapon").is_some_and(Node::is_true);
    t.use_facing_as_target = node.get("useFacingAsTarget").is_some_and(Node::is_true);
    t.knock_back_speed = node.path(&["specialCaseDamageStuff", "knockBackSpeed"]).and_then(Node::float).unwrap_or(0.0);
    t.damage_flags = node.get("damageFlags").and_then(Node::int).unwrap_or(0) as u32;
    let special = |name| node.path(&["specialCaseDamageStuff", name]).and_then(Node::float).unwrap_or(0.0);
    t.knock_back_angle = special("knockBackAngle").to_radians();
    t.stun_time = special("stunTime");
    t.slow_time = special("slowTime");
    t.dot_time = special("dotTime");
    t.heat_damage = special("heatIncrement");
    t.missing = missing;
    t
}

/// The numbers of each weapon in `defs`, from the game's shared data.
pub fn load(game_dir: &Path, defs: &[WeaponDef]) -> Result<Vec<WeaponTuning>, String> {
    let pack = Pack::open(&game_dir.join("bnxglobal.str"))?;
    let files: Vec<DataFile> =
        pack.of_type(pack::DATA).filter_map(|chunk| DataFile::parse(pack.data(chunk).to_vec()).ok()).collect();
    defs.iter()
        .map(|def| {
            let node = files
                .iter()
                .find_map(|file| file.root().field(def.attribute))
                .ok_or_else(|| format!("bnxglobal.str has no weapon attributes {:#010x}", def.attribute))?;
            Ok(tuning_from(node, def.kind))
        })
        .collect()
}

/// What pulling the trigger did on one step.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fired {
    Nothing,
    /// This many rounds left the weapon (each of `projectiles_per_fire` projectiles).
    Rounds(u32),
    /// The last of them filled the heat meter: it is locked until it has cooled.
    RoundsAndOverheated(u32),
    /// The trigger was pulled while it is overheated.
    Refused,
}

/// One weapon's state: its heat and when it may fire again.
#[derive(Clone, Copy, Debug)]
pub struct Weapon {
    pub heat: f32,
    /// Seconds left of the lock after overheating.
    pub overheated_left: f32,
    /// Seconds before the heat starts to fall (the weapon's word 0xf3).
    heat_wait: f32,
    /// Seconds since the last round (0xfb); a new weapon starts at 100000.
    since_round: f32,
    /// Seconds the trigger has been held on (0x106), and whether this pull has fired
    /// (0x107).
    pub held_for: f32,
    fired_this_pull: bool,
    /// Seconds before the refusal may sound again (0xd8).
    refused_wait: f32,
    was_down: bool,
    /// Time not yet used up by whole updates of the game's.
    clock: f32,
}

impl Default for Weapon {
    fn default() -> Self {
        Self {
            heat: 0.0,
            overheated_left: 0.0,
            heat_wait: 0.0,
            since_round: 100_000.0,
            held_for: 0.0,
            fired_this_pull: false,
            refused_wait: 0.0,
            was_down: false,
            clock: 0.0,
        }
    }
}

impl Weapon {
    /// [game] 007a8f20 divides the shot interval by slowFireRate. The heat increment
    /// uses slowHeatMeterIncrementRate; the weapon's update clock remains 0.032s.
    pub fn step_status(&mut self, t: &WeaponTuning, trigger: bool, slow: bool, effects: &crate::damage::Tuning, dt: f32) -> Fired {
        if !slow {return self.step(t,trigger,dt);}
        let mut scaled=t.clone();
        scaled.rate_of_fire /= effects.slow_fire_rate.max(1e-6);
        scaled.heat_per_round *= effects.slow_heat_rate;
        self.step(&scaled,trigger,dt)
    }
    /// [game] Damage flags4/5 add heat through the same capacity/cooldown path as firing.
    pub fn add_heat(&mut self, t: &WeaponTuning, heat: f32) {
        if heat<=0.0 {return;}
        self.heat += heat;
        if self.heat >= t.heat_capacity { self.overheated_left = t.overheat_period; self.heat_wait=0.0; }
        else { self.heat_wait = t.heat_cooldown_til_decrement; }
    }
    /// The heat meter, 0 to 1; full while locked.
    pub fn heat_fraction(&self, t: &WeaponTuning) -> f32 {
        if self.overheated_left > 0.0 { 1.0 } else { (self.heat / t.heat_capacity.max(1e-3)).clamp(0.0, 1.0) }
    }

    /// Whether the lock-on may run for a weapon whose trigger is held: it is ready (more
    /// than the rate of fire since its last round, `FUN_007ad080`) and its heat is under
    /// the capacity (slot `+0xf4` under 1). [game]
    pub fn may_lock(&self, t: &WeaponTuning) -> bool {
        self.since_round > t.rate_of_fire && self.heat < t.heat_capacity
    }

    /// A step of any length with the trigger held or not: the game's rules run in whole
    /// updates of its own (0.032 s), which is what spaces the rounds. What the updates in
    /// the step did, added up.
    pub fn step(&mut self, t: &WeaponTuning, trigger: bool, dt: f32) -> Fired {
        self.clock += dt;
        let mut all = Fired::Nothing;
        while self.clock >= crate::sim::GAME_UPDATE {
            self.clock -= crate::sim::GAME_UPDATE;
            all = match (all, self.update(t, trigger, crate::sim::GAME_UPDATE)) {
                (Fired::Rounds(a), Fired::Rounds(b)) => Fired::Rounds(a + b),
                (Fired::Rounds(a), Fired::RoundsAndOverheated(b)) => Fired::RoundsAndOverheated(a + b),
                (was, Fired::Nothing) => was,
                (Fired::Nothing | Fired::Refused, now) => now,
                (was, Fired::Refused) => was,
                (was @ Fired::RoundsAndOverheated(_), _) => was,
            };
        }
        all
    }

    /// One update of the game's. [game]
    ///
    /// The trigger, as the weapon state's update gives it to the weapon manager each update
    /// (`FUN_00878740`: held, `FUN_007b37f0`; let go that update, `FUN_007b3a20`), which
    /// calls the weapon (`FUN_007aca50` held, `FUN_007acb90` let go):
    ///
    /// - Held: a round leaves if the weapon can fire (`FUN_007a8da0`: its heat is under
    ///   `heatMeterCapacity`, which it is not all through the overheat period) and is ready
    ///   (`FUN_007ad080`: MORE than `rateOfFire` since the last round; and, unless
    ///   `automaticFiring`, the trigger has not been held on since it fired). So at 0.032 s
    ///   an update his cannon's 0.15 comes to a round every fifth update, 0.16 s.
    /// - A launcher with `mustHoldFireTriggerToLockOn` (his missiles) does nothing while
    ///   held: its round leaves on the update the trigger is let go, if it had been held.
    /// - With the heat at the capacity the refusal sounds instead, and again every
    ///   `rateOfFire` while that goes on (`FUN_007a9dd0`, its wait in the weapon's 0xd8).
    ///
    /// Then the weapon's own update (`FUN_007ac370`): the time since the last round and the
    /// time held go up (the latter only once this pull has fired, or for the launcher above;
    /// it is zeroed with the trigger up), and the heat (`FUN_007a9700`; a round's is
    /// `FUN_007a90c0`, once a firing whatever the number of projectiles): a round adds
    /// `heatMeterIncrementPerRound`; under the capacity that restarts a wait of
    /// `heatMeterCooldownTilDecrement`, after which the heat falls by
    /// `heatMeterDecrementPerSecond`; the round that reaches the capacity starts
    /// `heatMeterOverHeatPeriod` instead, and when that has run the heat is put to zero,
    /// because `heatMeterOverHeatCooldownRate` is not positive for any of his weapons
    /// (where it is, the heat stays and falls at that rate).
    ///
    /// Not in: bursts (`burstPerFire` is 1 for all his weapons), the charged shot (none of
    /// his), alternating barrels, the upgrades that scale the rate, a round's heat and the
    /// period (levels all 0), and the weapon's slot `+0xc0` (its byte `+0x40c`, set
    /// while a charged shot is being charged: `FUN_007b01a0`), during which the heat does
    /// not fall: none of his weapons charges. [assumed: the state's update comes before the weapon's in an update; the
    /// launcher's release could not fire otherwise]
    fn update(&mut self, t: &WeaponTuning, trigger: bool, dt: f32) -> Fired {
        let let_go = !trigger && self.was_down;
        self.was_down = trigger;
        let on_release = t.hold_to_lock_on;
        let full = t.heat_capacity <= self.heat;
        let fresh = t.automatic_firing || self.held_for == 0.0;
        let asked = if on_release { let_go && self.held_for > 0.0 } else { trigger };
        let mut fired = Fired::Nothing;
        if asked && full {
            if (on_release || fresh) && self.refused_wait <= 0.0 {
                self.refused_wait = t.rate_of_fire;
                fired = Fired::Refused;
            }
        } else if asked && self.since_round > t.rate_of_fire && (on_release || fresh) {
            self.since_round = 0.0;
            self.fired_this_pull = true;
            self.heat += t.heat_per_round;
            if self.heat < t.heat_capacity {
                self.heat_wait = t.heat_cooldown_til_decrement;
                fired = Fired::Rounds(1);
            } else {
                self.overheated_left = t.overheat_period;
                self.heat_wait = 0.0;
                fired = Fired::RoundsAndOverheated(1);
            }
        }

        if self.refused_wait > 0.0 {
            self.refused_wait -= dt;
        }
        self.since_round += dt;
        if !trigger || !(self.fired_this_pull || on_release) {
            self.held_for = 0.0;
            self.fired_this_pull = false;
        } else {
            self.held_for += dt;
        }
        if self.overheated_left > 0.0 {
            self.overheated_left -= dt;
            if self.overheated_left <= 0.0 {
                self.overheated_left = 0.0;
                self.heat = 0.0;
            }
        } else if self.heat_wait > 0.0 {
            self.heat_wait -= dt;
        } else if self.heat > 0.0 {
            self.heat = (self.heat - t.heat_decrement_per_second * dt).max(0.0);
        }
        fired
    }
}

/// A number drawn evenly between two (`FUN_00551d60`), from a source of 0 to 1.
fn between(range: [f32; 2], random: &mut dyn FnMut() -> f32) -> f32 {
    range[0] + (range[1] - range[0]) * random()
}

/// A missile in flight: the launch (`FUN_00785de0`) and the update (`FUN_007847e0`). [game]
///
/// It leaves along the muzzle at `missileStartSpeed` and lasts `missileLifetime` (a timer
/// set at the launch). Each update of the game's:
///
/// - Once the guidance delay has run, it steers at a point: the middle of its target
///   (the target's place + its up axis * half its height) while it has one, the weapon is
///   `guided` and its own guidance is on; else the point it was launched at (the aim
///   point; for a weapon that is not guided, 10000 along the line to it). With c the
///   cosine between its way and the point: inside `guidedViewAngle` it points straight at
///   the point, at once; outside it turns toward it by `outOfViewTurnSpeed`.
/// - Once the point has been within `pastTargetConeAngleActivated` of its way, a point
///   that is no longer ahead (c <= 0) ends the guidance: it flies straight on.
/// - The wobble: after a wait drawn from `minWobbleTimer`..`maxWobbleTimer`, for a time
///   drawn from `minWobbleDuration`..`maxWobbleDuration`, it turns each update about an
///   axis drawn at random (each part -1 to 1, not made unit) by that axis's length *
///   a rate drawn from 0..`maxWobbleAngle`, either way * the update's time; a turn under
///   0.01 radians is not made. It does not steer while it wobbles.
/// - Speed: + `missileAcceleration` a second while the delay drawn from
///   `minAccelFactorDelay`..`maxAccelFactorDelay` runs, then `accelFactor` times that, up
///   to `missileTopSpeed`.
///
/// [assumed: it is moved by the new velocity after the update; the lifetime's end is
/// taken to remove it] Not in: the guidance ending when the target's damage object's
/// `+0x94` is over 0 (not read), `minGuidanceOffDelay` drawn anew (his is -1: never),
/// `twirl` (his is false), `stealthGuided`, the damage upgrade (`FUN_0079b770`).
#[derive(Clone, Copy, Debug)]
pub struct Missile {
    pub position: Vec3,
    pub direction: Vec3,
    pub speed: f32,
    pub age: f32,
    /// What it was locked on at the launch.
    pub target: Option<u32>,
    /// The point it steers at while it has no target to steer at (`+0x3c`).
    point: Vec3,
    /// Its guidance is on (`+0x80`), and the point has been inside the cone (`+0x81`).
    pub guided: bool,
    armed: bool,
    /// Seconds before it steers (`+0x74`), before it gives up steering (`+0x78`, negative
    /// for never) and before it gathers speed faster (`+0x7c`).
    guidance_wait: f32,
    off_wait: f32,
    accel_wait: f32,
    /// The wobble: seconds to the next (`+0x5c`), seconds left of this one (`+0x60`), and
    /// its axis times its rate (`+0x64`, `+0x70`).
    wobble_wait: f32,
    wobble_left: f32,
    wobble: Vec3,
    /// Time not yet used up by whole updates of the game's.
    clock: f32,
}

impl Missile {
    /// A missile leaving `position` along `forward`, sent at `point` and locked on
    /// `target`. `random` gives numbers from 0 to 1.
    pub fn launch(
        position: Vec3,
        forward: Vec3,
        point: Vec3,
        target: Option<u32>,
        t: &WeaponTuning,
        random: &mut dyn FnMut() -> f32,
    ) -> Self {
        let direction = forward.normalize_or(Vec3::Y);
        let point = if t.guided { point } else { position + (point - position).normalize_or(direction) * 10_000.0 };
        let guidance_wait = between(t.guidance_delay, random);
        let accel_wait = between(t.accel_factor_delay, random);
        Self {
            position,
            direction,
            speed: t.missile_start_speed,
            age: 0.0,
            target,
            point,
            guided: true,
            armed: false,
            guidance_wait,
            off_wait: -1.0,
            accel_wait,
            wobble_wait: 0.0,
            wobble_left: 0.0,
            wobble: Vec3::ZERO,
            clock: 0.0,
        }
    }

    /// Ends the guidance (`FUN_00785690`): it keeps the way it has (`straight`), or the
    /// way to its target's middle as it is now.
    fn guidance_off(&mut self, straight: bool, target_at: Option<Vec3>) {
        self.guided = false;
        let way = match target_at {
            Some(at) if !straight => (at - self.position).normalize_or(self.direction),
            _ => self.direction,
        };
        self.point = self.position + way * 10_000.0;
        self.target = None;
    }

    /// Moves it on by `dt` in whole updates of the game's and returns the stretch it
    /// covered, or `None` once its time is up. `target_at` is the middle of its target, if
    /// that is still there.
    pub fn step(
        &mut self,
        t: &WeaponTuning,
        target_at: Option<Vec3>,
        random: &mut dyn FnMut() -> f32,
        dt: f32,
    ) -> Option<(Vec3, Vec3)> {
        let from = self.position;
        self.clock += dt;
        while self.clock >= crate::sim::GAME_UPDATE {
            self.clock -= crate::sim::GAME_UPDATE;
            self.age += crate::sim::GAME_UPDATE;
            if self.age > t.missile_lifetime {
                return None;
            }
            self.update(t, target_at, random, crate::sim::GAME_UPDATE);
        }
        Some((from, self.position))
    }

    fn update(&mut self, t: &WeaponTuning, target_at: Option<Vec3>, random: &mut dyn FnMut() -> f32, dt: f32) {
        let target_at = target_at.filter(|_| self.target.is_some());
        self.wobble_left -= dt;
        if self.wobble_left <= 0.0 {
            self.wobble_left = 0.0;
            self.wobble_wait = (self.wobble_wait - dt).max(0.0);
        }
        self.guidance_wait -= dt;
        if self.guidance_wait <= 0.0 {
            self.guidance_wait = 0.0;
            if self.guided && self.off_wait < 0.0 && t.guidance_off_delay[0] >= 0.0 {
                self.off_wait = between(t.guidance_off_delay, random);
            }
        }
        if self.off_wait >= 0.0 && self.guided {
            self.off_wait -= dt;
            if self.off_wait <= 0.0 {
                self.off_wait = 0.0;
                self.guidance_off(false, target_at);
            }
        }
        self.accel_wait = (self.accel_wait - dt).max(0.0);

        let mut turned = Vec3::ZERO;
        if self.guidance_wait <= 0.0 {
            let point = match target_at {
                Some(at) if t.guided && self.guided => at,
                _ => self.point,
            };
            if let Some(to) = (point - self.position).try_normalize() {
                let c = to.dot(self.direction);
                if t.guided && !self.armed && c >= t.past_target_cos {
                    self.armed = true;
                }
                if self.armed && self.guided && c <= 0.0 {
                    self.guidance_off(true, target_at);
                }
                if self.wobble_left <= 0.0 {
                    if t.guided_view_cos < c {
                        self.direction = to;
                    } else if c > -0.999 && c < 0.999 {
                        let angle = c.acos().min(t.out_of_view_turn * dt);
                        let axis = self.direction.cross(to).normalize_or(Vec3::Z);
                        self.direction = (glam::Quat::from_axis_angle(axis, angle) * self.direction).normalize();
                    }
                }
            }
            if self.wobble_left <= 0.0 && self.wobble_wait <= 0.0 && t.wobble_timer[0] > 0.0 {
                // A new wobble (`FUN_00786440`).
                self.wobble_wait = between(t.wobble_timer, random);
                self.wobble_left = between(t.wobble_duration, random);
                let axis = Vec3::new(random() * 2.0 - 1.0, random() * 2.0 - 1.0, random() * 2.0 - 1.0);
                let side = if random() < 0.5 { -1.0 } else { 1.0 };
                self.wobble = axis * side * between([0.0, t.wobble_angle], random);
            }
            if self.wobble_left > 0.0 {
                turned = self.wobble * dt;
            }
        }
        if turned.length_squared() > 0.0001 {
            let angle = turned.length();
            self.direction = (glam::Quat::from_axis_angle(turned / angle, angle) * self.direction).normalize();
        }

        let gain = if self.accel_wait > 0.0 { t.missile_acceleration } else { t.accel_factor * t.missile_acceleration };
        self.speed = (self.speed + gain * dt).min(t.missile_top_speed);
        self.position += self.direction * self.speed * dt;
    }
}

/// What a missile lets go of where it ends: its `Explosion` (`FUN_007e5f00`, `FUN_007e60f0`,
/// the same as the ground punch's blast: a sphere that grows from `start_scale` to its full
/// size over `explosion_time`, and hits each thing it touches once, by the distance from its
/// middle). [game] The owner's routine (`FUN_007e6af0`, the branch for a missile) replaces
/// the explosion's damage with the launch's `overwriteExplosiveDamage` when that is 0 or
/// more, and its `minDamage` with `overwriteExplosiveMinDamage` likewise (his is -1: the
/// explosion's own 1 stays). The missile ends on the first thing it touches, or at the end
/// of its lifetime (`FUN_00785c00` sends it a kill of 100000 damage of the destroy kind the
/// data's `DamageData` names). [data] [assumed: the touch ends it at once; that it ends on
/// any contact and not only on some surfaces; that it is made in the update after the one
/// that ended the missile] Not in: the camera shake (his has none), the light, the sound
/// and the particles the object carries, the scorch on a surface (`SSDImpactPreset`).
#[derive(Clone, Debug)]
pub struct MissileBlast {
    pub centre: Vec3,
    /// The sphere's size now.
    pub radius: f32,
    pub age: f32,
    def: ExplosionDef,
    damage: f32,
    min_damage: f32,
    hit: Vec<u32>,
    /// Time not yet used up by whole updates of the game's.
    clock: f32,
}

/// A target an explosion reached: what it takes, and the way the blow went.
#[derive(Clone, Copy, Debug)]
pub struct BlastHit {
    pub target: u32,
    pub damage: f32,
    pub point: Vec3,
    /// Flat, from the explosion's middle to where the target stands.
    pub direction: glam::Vec2,
    pub knock_back_speed: f32,
    pub knock_back_angle: f32,
    pub flags: u32,
}

impl MissileBlast {
    /// A missile's explosion at `centre`, with the launcher's numbers.
    pub fn new(def: ExplosionDef, t: &WeaponTuning, centre: Vec3) -> Self {
        let damage = if t.explosive_damage >= 0.0 { t.explosive_damage } else { def.damage };
        let min_damage = if t.explosive_min_damage >= 0.0 { t.explosive_min_damage } else { def.min_damage };
        Self { centre, radius: def.radius * def.start_scale, age: 0.0, def, damage, min_damage, hit: Vec::new(), clock: 0.0 }
    }

    /// Runs it on by `dt` in whole updates of the game's, and says what its sphere touched
    /// and whether it is still there.
    pub fn step(&mut self, targets: &[melee::Target], dt: f32) -> (Vec<BlastHit>, bool) {
        let mut hits = Vec::new();
        self.clock += dt;
        while self.clock >= crate::sim::GAME_UPDATE {
            self.clock -= crate::sim::GAME_UPDATE;
            let step = crate::sim::GAME_UPDATE;
            let before = self.age;
            self.age += step;
            if self.age > self.def.time && before > self.def.time {
                return (hits, false);
            }
            self.radius = self.def.radius * self.def.scale(self.age, step);
            self.touch(targets, &mut hits);
        }
        (hits, true)
    }

    /// Its first update: the sphere at its starting size.
    pub fn first(&mut self, targets: &[melee::Target]) -> Vec<BlastHit> {
        let mut hits = Vec::new();
        self.touch(targets, &mut hits);
        hits
    }

    fn touch(&mut self, targets: &[melee::Target], hits: &mut Vec<BlastHit>) {
        let sphere = melee::Body { shape: crate::formats::scene::Shape::Sphere { radius: self.radius }, at: glam::Affine3A::from_translation(self.centre) };
        for target in targets {
            if self.hit.contains(&target.id) {
                continue;
            }
            let Some(point) = melee::touch(&sphere, &melee::Body::of_target(target)) else { continue };
            self.hit.push(target.id);
            hits.push(BlastHit {
                target: target.id,
                damage: self.def.damage_at(self.damage, self.min_damage, point.distance(self.centre), self.radius),
                point,
                direction: glam::Vec2::new(target.pos.x - self.centre.x, target.pos.y - self.centre.y).normalize_or_zero(),
                knock_back_speed: self.def.knock_back_speed,
                knock_back_angle: self.def.knock_back_angle.to_radians(),
                flags: self.def.damage_flags,
            });
        }
    }
}

/// The point a player's round is sent at: the aim point moved in the camera's picture
/// plane (`FUN_007aa210`). The most it moves is the distance from the camera to the aim
/// point times the tangent of half the camera's field of view times `playerNoiseRadius`,
/// so the scatter is the same size on screen at any range (his cannon's 0.01666 is a
/// sixtieth of half the picture's height). The way is an angle drawn evenly from the whole
/// turn along the camera's right and up, and how far a number drawn evenly from -1 to 1
/// times that most. [game] `a` and `b` are two numbers from 0 to 1, the caller's random
/// source; `fov` is in degrees. [assumed: the field of view is the camera's vertical one]
/// Not in: a non-player's scatter (`generalNoiseRadius`, a disc of that radius across the
/// line from the muzzle), and no scatter while sniping.
pub fn scattered_point(target: Vec3, eye: Vec3, right: Vec3, up: Vec3, fov: f32, radius: f32, a: f32, b: f32) -> Vec3 {
    if radius <= 0.0 {
        return target;
    }
    let most = (target - eye).length() * (fov.to_radians() * 0.5).tan() * radius;
    let (sin, cos) = ((a * 2.0 - 1.0) * std::f32::consts::PI).sin_cos();
    target + (up * sin + right * cos) * ((b * 2.0 - 1.0) * most)
}

/// How far along the camera's line the aim point is looked for (`DAT_00cad46c`). [game]
pub const AIM_REACH: f32 = 500.0;

/// What the middle of the picture is on: the weapon manager's aim point (`+0x94`), the
/// surface's normal there (`+0xa0`, zero when nothing was hit) and how far it is from where
/// the line began (`+0x90`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aim {
    pub point: Vec3,
    pub normal: Vec3,
    pub distance: f32,
}

/// The aim point of a player in weapon mode, as the weapon manager works it out every
/// update (`FUN_007b2310`). The line runs along the camera's forward from the point of it
/// nearest to where he stands (`FUN_006fa7b0`: the camera itself if he is behind it, so
/// nothing between the camera and him is aimed at) to `AIM_REACH` from the camera. The aim
/// point is the first thing the line meets, or its far end. [game]
///
/// `body` is the character's position (`FUN_0071a2e0`: his object's, at his feet).
/// [stand-in: `world` is the arena; the original's trace is its collision query with mask 3,
/// leaves out his own bodies, and passes things flagged 0x1000 that are not characters]
/// Not in: the aim out of weapon mode (the point keeps its distance along the line from
/// the muzzle), and `FUN_007b34a0`'s other answer while a game mode of type 768ce890,
/// d63697a6 or fc36eb23 runs (the muzzle plus the aim vector times 1000).
pub fn aim_point(eye: Vec3, forward: Vec3, body: Vec3, world: &dyn crate::camera::Sight) -> Aim {
    let far = eye + forward * AIM_REACH;
    let along = (body - eye).dot(forward).clamp(0.0, AIM_REACH);
    let from = eye + forward * along;
    match world.sight(from, far) {
        Some(hit) => Aim { point: hit.point, normal: hit.normal, distance: (from - hit.point).length() },
        None => Aim { point: far, normal: Vec3::ZERO, distance: (from - far).length() },
    }
}

/// The line a gun round is traced along (`FUN_007832b0`): from the round's own place, the
/// MUZZLE, towards the scattered aim point, `bulletRange` long. With `tracerFirePerRound`
/// over 0 the whole of it is traced in the update the round is fired and the hit lands at
/// once; otherwise only `bulletSpeed` times the update's length, the round flying on from
/// there (not done: none of his guns is so). [game]
///
/// Corrected 2026-10-04: this was traced from the camera, on a misreading of the test the
/// routine makes before taking the camera's place for the start. That test
/// (`FUN_0117e860`) is "the character's sniper camera (`+0x1e0`) is the one in use", not
/// "a player's": only a SNIPING shot starts at the camera, and Bumblebee has no sniper
/// camera (`sniperCamera: 0`).
pub fn bullet_line(muzzle: Vec3, at: Vec3, t: &WeaponTuning) -> (Vec3, Vec3) {
    let way = (at - muzzle).normalize_or_zero();
    (muzzle, muzzle + way * t.bullet_range)
}

/// The visible projectile continues to travel even when tracerFirePerRound already
/// applied damage instantly (00782310). The async endpoint query is supplied by the host.
#[derive(Clone,Copy,Debug)]
pub struct BulletVisual {pub position:Vec3,pub end:Vec3,pub velocity:Vec3}
impl BulletVisual {
    pub fn new(from:Vec3,end:Vec3,speed:f32)->Self {
        Self {position:from,end,velocity:(end-from).normalize_or_zero()*speed}
    }
    /// Original endpoint crossing: delete once advanced position is beyond the hit
    /// along its velocity, rather than assigning a fitted trail lifetime. [game]
    pub fn step(&mut self,dt:f32)->bool {
        let next=self.position+self.velocity*dt;
        if self.velocity==Vec3::ZERO || (next-self.end).dot(self.velocity)>0.0 {return false;}
        self.position=next;true
    }
}

/// How far ahead of its barrel a weapon with `useFacingAsTarget` sends a round
/// (`DAT_00b3487c`). [game]
pub const FACING_REACH: f32 = 100.0;

/// What a round of a weapon with `useFacingAsTarget` is sent at: the point `FACING_REACH`
/// straight ahead of the barrel it leaves (`FUN_007aab70`: the barrel's place plus its
/// forward row times 100). The barrel's axes are the character's own outside weapon mode
/// (`FUN_007a7a40` without `useFireDummyMatrix`), so the car's gun shoots where the car
/// points, each barrel along its own line; the scatter is then laid on that point as on
/// any other (`FUN_007aa210`). [game]
pub fn facing_target(barrel: Vec3, forward: Vec3) -> Vec3 {
    barrel + forward * FACING_REACH
}

/// The weapon manager's wait before a car weapon may fire (its `+0xd0`). Changing to the
/// car's control mode (3) takes the car's weapon in hand and starts the wait at the
/// character's `vehicleWeaponTimeTilCanFire` (`FUN_007b43b0`, called by the control mode's
/// setter `FUN_0071ab70`); the manager's update counts it down (`FUN_007b1850`); and a
/// weapon with `isVehicleModeWeapon` gets the trigger only once it has run out and he is
/// in the car's form (`FUN_007b44e0`, asked by the manager's "held" and "released").
/// Leaving the car's mode puts the weapon he held before back in hand and zeroes the wait.
/// [game]
#[derive(Clone, Copy, Debug, Default)]
pub struct VehicleWait {
    left: f32,
    in_car: bool,
}

impl VehicleWait {
    /// Each frame: whether his control mode is the car's, and the wait a change into it
    /// starts. True on the frame the mode changes either way.
    pub fn step(&mut self, in_car: bool, wait: f32, dt: f32) -> bool {
        let changed = in_car != self.in_car;
        if changed {
            self.in_car = in_car;
            self.left = if in_car { wait } else { 0.0 };
        } else if self.left > 0.0 {
            self.left = (self.left - dt).max(0.0);
        }
        changed
    }

    /// Whether a car weapon gets the trigger.
    pub fn ready(&self) -> bool {
        self.in_car && self.left <= 0.0
    }

    pub fn left(&self) -> f32 {
        self.left
    }
}

/// The part of a character a round struck, by the collision body's surface (the hit's
/// `+0x254`: `Head`, `UpperBody`, `LowerBody`, `VehicleBody`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Part {
    Head,
    UpperBody,
    LowerBody,
    VehicleBody,
    /// Anything else: no multiplier.
    Other,
}

/// A gun round's damage where it lands (`FUN_00782b30`): `damage` times the charge
/// (`charge`, 1 for a weapon without a charged shot), times `positionalDamageMultiplier`
/// for the part struck, and past `bulletDamageFallOffRange` (when that is not negative)
/// falling in a straight line to `minDamage` at `bulletRange`. `travelled` is from where
/// the round was made (the muzzle) to the hit. A head counts as a head only when the one
/// who fired is a player or of the second controller kind (`FUN_00711210`); for anyone
/// else it is an upper body. [game] The blow pushes along the round's way with
/// `damage * 10`, and carries the data's `damageFlags` and `specialCaseDamageStuff`.
pub fn bullet_damage(t: &WeaponTuning, charge: f32, part: Part, travelled: f32, by_player: bool) -> f32 {
    let mut damage = t.damage * charge;
    damage *= match part {
        Part::Head if by_player => t.positional[0],
        Part::Head | Part::UpperBody => t.positional[1],
        Part::LowerBody => t.positional[2],
        Part::VehicleBody => t.positional[3],
        Part::Other => 1.0,
    };
    let fall_off = t.bullet_fall_off_range;
    if fall_off >= 0.0 && travelled >= fall_off {
        damage = if travelled < t.bullet_range {
            t.min_damage + (1.0 - (travelled - fall_off) / (t.bullet_range - fall_off)) * (damage - t.min_damage)
        } else {
            t.min_damage
        };
    }
    damage
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn visible_tracer_moves_at_data_speed_and_ends_only_after_crossing() {
        let mut v=BulletVisual::new(Vec3::ZERO,Vec3::Y*64.0,1000.0);
        assert!(v.step(0.032));assert_eq!(v.position,Vec3::Y*32.0);
        assert!(v.step(0.032));assert_eq!(v.position,Vec3::Y*64.0);
        assert!(!v.step(0.032));
    }

    /// Bumblebee's two robot weapons, as the game's data has them.
    fn cannon() -> WeaponTuning {
        WeaponTuning {
            rate_of_fire: 0.15,
            automatic_firing: true,
            projectiles_per_fire: 1,
            heat_capacity: 100.0,
            heat_per_round: 4.2,
            heat_decrement_per_second: 38.0,
            heat_cooldown_til_decrement: 0.4,
            overheat_period: 5.0,
            player_noise_radius: 0.01666,
            bullet_speed: 1000.0,
            bullet_range: 300.0,
            damage: 15.0,
            bullet_fall_off_range: -1.0,
            min_damage: 1.0,
            tracer_fire_per_round: 1,
            positional: [2.0, 1.0, 1.0, 1.0],
            ..Default::default()
        }
    }

    /// A wall across y = 40, for the aim's line.
    struct Wall;

    impl crate::camera::Sight for Wall {
        fn sight(&self, from: Vec3, to: Vec3) -> Option<crate::camera::Hit> {
            (from.y < 40.0 && to.y >= 40.0)
                .then(|| crate::camera::Hit { point: from + (to - from) * ((40.0 - from.y) / (to.y - from.y)), normal: Vec3::NEG_Y, ..Default::default() })
        }
        fn sweep(&self,from:Vec3,to:Vec3,radius:f32)->Option<crate::camera::Hit> {
            let plane=40.0-radius;
            (from.y<plane && to.y>=plane).then(||crate::camera::Hit {
                point:from+(to-from)*((plane-from.y)/(to.y-from.y)),normal:Vec3::NEG_Y,..Default::default()
            })
        }
    }

    #[test]
    fn the_aim_point_is_what_the_cameras_line_meets_beyond_him() {
        // The camera 10 behind and 4 above him, looking level along +y.
        let (eye, forward, body) = (Vec3::new(0.0, -10.0, 4.0), Vec3::Y, Vec3::ZERO);
        // Nothing in the way: 500 from the camera, measured from beside him.
        let open = aim_point(eye, forward, body, &crate::camera::Open);
        assert_eq!(open.point, Vec3::new(0.0, 490.0, 4.0));
        assert_eq!((open.normal, open.distance), (Vec3::ZERO, 490.0));
        // A wall 40 ahead of him.
        let wall = aim_point(eye, forward, body, &Wall);
        assert_eq!(wall.point, Vec3::new(0.0, 40.0, 4.0));
        assert_eq!((wall.normal, wall.distance), (Vec3::NEG_Y, 40.0));
        // The line starts beside him, not at the camera: a wall between the two is not
        // aimed at.
        let behind = aim_point(Vec3::new(0.0, 30.0, 4.0), forward, Vec3::new(0.0, 45.0, 0.0), &Wall);
        assert_eq!(behind.point, Vec3::new(0.0, 530.0, 4.0));
        // With him behind the camera it starts at the camera.
        let ahead = aim_point(Vec3::new(0.0, 30.0, 4.0), forward, Vec3::new(0.0, 20.0, 0.0), &Wall);
        assert_eq!(ahead.point, Vec3::new(0.0, 40.0, 4.0));
    }

    #[test]
    fn a_round_is_traced_from_the_muzzle_and_hurts_by_the_part_and_the_range() {
        let mut t = cannon();
        // From the muzzle towards the point it was sent at, over the range.
        let (from, to) = bullet_line(Vec3::new(0.0, -10.0, 4.0), Vec3::new(0.0, 40.0, 4.0), &t);
        assert_eq!((from, to), (Vec3::new(0.0, -10.0, 4.0), Vec3::new(0.0, 290.0, 4.0)));
        // His cannon: 15, twice that on a head, with no fall-off.
        assert_eq!(bullet_damage(&t, 1.0, Part::UpperBody, 250.0, true), 15.0);
        assert_eq!(bullet_damage(&t, 1.0, Part::Head, 250.0, true), 30.0);
        // A head is an upper body to a round that is not a player's.
        assert_eq!(bullet_damage(&t, 1.0, Part::Head, 250.0, false), 15.0);
        // With a fall-off from 100: half way to the range, half way down to `minDamage`.
        t.bullet_fall_off_range = 100.0;
        assert_eq!(bullet_damage(&t, 1.0, Part::Other, 99.0, true), 15.0);
        assert_eq!(bullet_damage(&t, 1.0, Part::Other, 200.0, true), 8.0);
        assert_eq!(bullet_damage(&t, 1.0, Part::Other, 300.0, true), 1.0);
    }

    #[test]
    fn the_cars_gun_waits_after_the_change_and_shoots_where_each_barrel_points() {
        // The wait starts as the car's mode starts, and runs out in 0.75 s.
        let mut wait = VehicleWait::default();
        assert!(!wait.ready(), "no car weapon on foot");
        assert!(wait.step(true, 0.75, DT));
        assert!(!wait.ready());
        let mut updates = 0;
        while !wait.ready() {
            assert!(!wait.step(true, 0.75, DT));
            updates += 1;
        }
        assert_eq!(updates, 24, "0.75 s is 24 updates of 0.032");
        // Out of the car it is not ready, and the next change starts the wait again.
        assert!(wait.step(false, 0.75, DT));
        assert!(!wait.ready());
        wait.step(true, 0.75, DT);
        assert_eq!(wait.left(), 0.75);
        // Each barrel's round goes at the point 100 ahead of that barrel: two parallel
        // lines the barrels' distance apart, not two lines meeting at one aim point.
        let (left, right) = (Vec3::new(-1.78, 1.14, 0.19), Vec3::new(-0.14, 1.14, 0.19));
        let (a, b) = (facing_target(left, Vec3::Y), facing_target(right, Vec3::Y));
        assert_eq!(a, Vec3::new(-1.78, 101.14, 0.19));
        assert!(((b - a) - (right - left)).length() < 1e-5);
    }

    fn launcher() -> WeaponTuning {
        WeaponTuning {
            rate_of_fire: 0.6,
            automatic_firing: false,
            projectiles_per_fire: 1,
            heat_capacity: 100.0,
            heat_per_round: 23.0,
            heat_decrement_per_second: 25.0,
            heat_cooldown_til_decrement: 0.6,
            overheat_period: 6.25,
            missile_start_speed: 70.0,
            missile_top_speed: 140.0,
            missile_acceleration: 70.0,
            missile_lifetime: 4.0,
            explosive_damage: 75.0,
            hold_to_lock_on: true,
            lock_on_time: 0.4,
            guided: true,
            lock_bracket_stay: 1.0,
            lock_box: [128.0, 128.0],
            aim_assist_max_range: 350.0,
            guidance_off_delay: [-1.0, -1.0],
            guided_view_cos: 15f32.to_radians().cos(),
            out_of_view_turn: 135f32.to_radians(),
            accel_factor_delay: [0.2, 0.2],
            accel_factor: 2.0,
            wobble_timer: [0.2, 0.8],
            wobble_angle: 20f32.to_radians(),
            wobble_duration: [0.02, 0.2],
            past_target_cos: 30f32.to_radians().cos(),
            ..Default::default()
        }
    }

    fn micro_missile_explosion() -> ExplosionDef {
        ExplosionDef {
            radius: 5.0,
            start_scale: 0.5,
            time: 0.1,
            delay: 0.0,
            damage: 8.0,
            min_damage: 1.0,
            fall_off: 0.75,
            pound_override: false,
            damage_player: true,
            can_damage_self: false,
            damage_flags: 0,
            knock_back_speed: 30.0,
            knock_back_angle: 15.0,
            camera_shake: 0,
            stun_time: 0.0,
        }
    }

    fn dummy(id: u32, at: Vec3) -> melee::Target {
        melee::Target { id, pos: at, radius: 1.0, height: 5.0, character: true, large: false, surface: 0 }
    }

    #[test]
    fn a_missiles_explosion_does_the_launchers_damage_by_distance_from_its_middle() {
        let t = WeaponTuning { explosive_min_damage: -1.0, ..launcher() };
        // Beside the middle, 4 away (a body 1 wide, so touched from 3 on) and 5.5 away.
        let targets = [dummy(1, Vec3::new(0.0, 0.5, 0.0)), dummy(2, Vec3::new(4.0, 0.0, 0.0)), dummy(3, Vec3::new(5.5, 0.0, 0.0))];
        let mut blast = MissileBlast::new(micro_missile_explosion(), &t, Vec3::new(0.0, 0.0, 2.0));
        // `overwriteExplosiveDamage` 75 replaces the 8; `minDamage` stays at 1.
        assert_eq!((blast.damage, blast.min_damage), (75.0, 1.0));
        let mut hits = blast.first(&targets);
        for _ in 0..8 {
            hits.extend(blast.step(&targets, DT).0);
        }
        // The sphere starts at 2.5 and is 5 by the end of 0.1 s.
        assert!((blast.radius - 5.0).abs() < 1e-4, "{}", blast.radius);
        let by = |id: u32| hits.iter().filter(|h| h.target == id).collect::<Vec<_>>();
        assert_eq!(by(1).len(), 1);
        assert_eq!(by(1)[0].damage, 75.0);
        // Reached once the sphere is big enough, near its edge, so far down the fall-off.
        assert_eq!(by(2).len(), 1);
        assert!(by(2)[0].damage < 75.0 && by(2)[0].damage >= 1.0, "{}", by(2)[0].damage);
        // Out of reach: 4.5 from the middle to its nearest side, past 5 only from the middle.
        assert!(by(3).len() <= 1);
        assert_eq!(by(1)[0].knock_back_speed, 30.0);
    }

    #[test]
    fn an_explosion_is_gone_once_its_time_is_over() {
        let t = WeaponTuning { explosive_min_damage: -1.0, ..launcher() };
        let mut blast = MissileBlast::new(micro_missile_explosion(), &t, Vec3::ZERO);
        let mut alive = true;
        let mut updates = 0;
        while alive && updates < 100 {
            alive = blast.step(&[], DT).1;
            updates += 1;
        }
        // 0.1 s is 4 updates of 0.032; it stays one update past the time, then goes.
        assert!((4..=6).contains(&updates), "{updates}");
    }

    /// A source of numbers from 0 to 1 for the tests.
    fn dice() -> impl FnMut() -> f32 {
        let mut seed = 0x1234_5678u32;
        move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed >> 8) as f32 / (1u32 << 24) as f32
        }
    }

    /// The game's update.
    const DT: f32 = crate::sim::GAME_UPDATE;

    fn rounds(fired: Fired) -> u32 {
        match fired {
            Fired::Rounds(n) | Fired::RoundsAndOverheated(n) => n,
            _ => 0,
        }
    }

    #[test]
    fn the_cannon_fires_at_its_rate_and_overheats_on_the_24th_round() {
        let t = cannon();
        let mut w = Weapon::default();
        let (mut total, mut time, mut locked_at) = (0, 0.0, None);
        while locked_at.is_none() {
            let fired = w.step(&t, true, DT);
            total += rounds(fired);
            time += DT;
            if matches!(fired, Fired::RoundsAndOverheated(_)) {
                locked_at = Some(time);
            }
        }
        // 100 / 4.2 is 23.8, so the 24th round fills the meter. A round needs more than
        // 0.15 s since the last, looked at once an update of 0.032 s: every fifth, 0.16 s.
        assert_eq!(total, 24);
        assert!((locked_at.unwrap() - (23.0 * 0.16 + DT)).abs() < 1e-3, "{locked_at:?}");
        // Locked for five seconds. The trigger held on is refused at once and again every
        // 0.15 s (five updates); then the meter is empty and it fires again.
        assert_eq!(w.step(&t, false, DT), Fired::Nothing);
        let refusals: Vec<bool> = (0..11).map(|_| w.step(&t, true, DT) == Fired::Refused).collect();
        assert_eq!(refusals, [true, false, false, false, false, true, false, false, false, false, true]);
        let mut waited = 12.0 * DT;
        while w.overheated_left > 0.0 {
            w.step(&t, false, DT);
            waited += DT;
        }
        assert!((waited - 5.0).abs() < 0.05 && w.heat == 0.0, "{waited}");
        assert_eq!(rounds(w.step(&t, true, DT)), 1);
    }

    #[test]
    fn the_rounds_are_spaced_by_the_games_update_whatever_the_frame_rate() {
        let t = cannon();
        for frame in [1.0 / 144.0, 1.0 / 60.0, 1.0 / 30.0, 0.1] {
            let mut w = Weapon::default();
            let (mut total, mut time) = (0, 0.0);
            while time < 2.0 {
                total += rounds(w.step(&t, true, frame));
                time += frame;
            }
            // Updates 0, 5, 10 ... of the 62 in two seconds.
            assert!((12..=13).contains(&total), "{total} rounds at {frame} a frame");
        }
    }

    #[test]
    fn heat_falls_after_a_pause() {
        let t = cannon();
        let mut w = Weapon::default();
        for _ in 0..31 {
            w.step(&t, true, DT);
        }
        let hot = w.heat;
        assert!((hot - 7.0 * 4.2).abs() < 0.01, "{hot}");
        // Nothing until 0.4 s after the last round (which left on the last of those
        // updates), then 38 a second.
        for _ in 0..12 {
            w.step(&t, false, DT);
        }
        assert_eq!(w.heat, hot);
        for _ in 0..16 {
            w.step(&t, false, DT);
        }
        assert!((hot - w.heat - 38.0 * 16.0 * DT).abs() < 0.01, "{}", w.heat);
    }

    #[test]
    fn the_launcher_fires_when_the_trigger_is_let_go() {
        let t = launcher();
        let mut w = Weapon::default();
        // Held for a second: nothing leaves, the time held runs.
        let mut total = 0;
        for _ in 0..31 {
            total += rounds(w.step(&t, true, DT));
        }
        assert_eq!(total, 0);
        assert!((w.held_for - 31.0 * DT).abs() < 1e-4);
        // Let go: one missile, on that update.
        assert_eq!(rounds(w.step(&t, false, DT)), 1);
        assert_eq!(w.held_for, 0.0);
        assert_eq!(rounds(w.step(&t, false, DT)), 0);
        // A tap inside the 0.6 s since that one does nothing; after it, another.
        w.step(&t, true, DT);
        assert_eq!(rounds(w.step(&t, false, DT)), 0);
        for _ in 0..20 {
            w.step(&t, false, DT);
        }
        w.step(&t, true, DT);
        assert_eq!(rounds(w.step(&t, false, DT)), 1);
    }

    #[test]
    fn a_weapon_that_is_not_automatic_fires_once_a_pull() {
        let t = WeaponTuning { hold_to_lock_on: false, ..launcher() };
        let mut w = Weapon::default();
        let mut total = 0;
        for _ in 0..60 {
            total += rounds(w.step(&t, true, DT));
        }
        assert_eq!(total, 1);
        w.step(&t, false, DT);
        assert_eq!(rounds(w.step(&t, true, DT)), 1);
    }

    #[test]
    fn a_missile_gathers_speed_and_runs_out() {
        // No wobble, so that it flies straight.
        let t = WeaponTuning { wobble_timer: [0.0, 0.0], ..launcher() };
        let mut random = dice();
        let mut m = Missile::launch(Vec3::ZERO, Vec3::Y, Vec3::Y * 1000.0, None, &t, &mut random);
        let mut steps = 0;
        while m.step(&t, None, &mut random, DT).is_some() {
            steps += 1;
        }
        // 70 rising 70 a second for 0.2 s (to 84), then 140 a second to 140 (0.4 s more),
        // then 140 to the end of its 4 s: about 15 + 45 + 476.
        assert!((steps as f32 * DT - 4.0).abs() < 0.05);
        assert!((m.position.y - 536.0).abs() < 6.0 && m.speed == 140.0, "{} at {}", m.position.y, m.speed);
        assert!(m.position.x.abs() < 1e-3 && m.position.z.abs() < 1e-3);
    }

    #[test]
    fn a_missile_turns_to_its_target_and_flies_on_once_past_it() {
        let t = WeaponTuning { wobble_timer: [0.0, 0.0], ..launcher() };
        let mut random = dice();
        // The target is 60 degrees off its way: outside the 15 degree cone, so it turns at
        // 135 degrees a second, 4.32 an update.
        let target = Vec3::new(60f32.to_radians().sin(), 60f32.to_radians().cos(), 0.0) * 400.0;
        let mut m = Missile::launch(Vec3::ZERO, Vec3::Y, Vec3::Y * 500.0, Some(1), &t, &mut random);
        m.step(&t, Some(target), &mut random, DT);
        let turned = m.direction.angle_between(Vec3::Y).to_degrees();
        assert!((turned - 4.32).abs() < 0.05, "{turned}");
        // Inside the cone it points straight at the target, and so reaches it.
        let mut nearest = f32::MAX;
        for _ in 0..125 {
            if m.step(&t, Some(target), &mut random, DT).is_none() {
                break;
            }
            nearest = nearest.min((m.position - target).length());
        }
        assert!(nearest < 5.0, "{nearest}");
        // Past the target its guidance is off: it keeps its way.
        assert!(!m.guided && m.target.is_none());
        let way = m.direction;
        m.step(&t, Some(target), &mut random, DT);
        assert!(m.direction.angle_between(way) < 1e-3);
    }

    #[test]
    fn a_missile_wobbles_off_its_line_but_comes_back_to_the_target() {
        let t = launcher();
        let mut random = dice();
        let target = Vec3::Y * 300.0;
        let mut m = Missile::launch(Vec3::ZERO, Vec3::Y, target, Some(1), &t, &mut random);
        let (mut off, mut nearest) = (0.0f32, f32::MAX);
        for _ in 0..125 {
            if m.step(&t, Some(target), &mut random, DT).is_none() || !m.guided {
                break;
            }
            off = off.max(m.direction.angle_between((target - m.position).normalize()));
            nearest = nearest.min((m.position - target).length());
        }
        assert!(off > 0.005, "it never wobbled: {off}");
        assert!(nearest < 6.0, "{nearest}");
    }

    #[test]
    fn scatter_is_a_disc_in_the_picture_that_grows_with_the_distance() {
        let (eye, right, up) = (Vec3::ZERO, Vec3::X, Vec3::Z);
        for distance in [10.0f32, 100.0] {
            // A 45 degree view: half the picture is 0.414 of the distance high.
            let most = distance * 22.5f32.to_radians().tan() * 0.01666;
            let target = Vec3::Y * distance;
            let mut widest = 0.0f32;
            for i in 0..200 {
                let p = scattered_point(target, eye, right, up, 45.0, 0.01666, i as f32 / 200.0, (i * 37 % 200) as f32 / 199.0);
                assert!((p.y - distance).abs() < 1e-3, "it stays in the picture plane");
                widest = widest.max((p - target).length());
            }
            assert!(widest <= most + 1e-4 && widest > most * 0.95, "{widest} of {most}");
        }
        assert_eq!(scattered_point(Vec3::Y, eye, right, up, 45.0, 0.0, 0.3, 0.9), Vec3::Y);
    }
}
