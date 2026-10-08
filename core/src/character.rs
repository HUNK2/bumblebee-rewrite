//! Loads one character pack from the game install into plain data.

use std::path::Path;

use crate::formats::anim::{self, AnimRef, AnimSet};
use crate::formats::lxb::{DataFile, Node};
use crate::formats::model::Library;
use crate::formats::pack::{self, Pack};
use crate::formats::scene::{self, ObjectDef};
use crate::particles::Template;
use crate::weapons::{self, WeaponDef};

pub struct CharacterData {
    pub robot: ObjectDef,
    pub vehicle: Option<ObjectDef>,
    pub animations: AnimSet,
    /// The clips the vehicle model carries for itself, for the change of form.
    pub vehicle_animations: Vec<AnimRef>,
    pub library: Library,
    pub sounds: Sounds,
    pub chatter: Vec<crate::chatter::Line>,
    /// `particleHitTable.meleeHitSparkPreset`: the sound and the effect of a melee hit,
    /// by the surface struck.
    pub melee_impact: ImpactPreset,
    /// `footStepFX`: the same for a step, by the ground's surface.
    pub step_impact: ImpactPreset,
    /// `climbData`'s five presets, in `anim::CLIMB_SIDES`' order: the dust of a hand or a
    /// foot on the wall, and of sliding down it. [data]
    pub climb_impact: Vec<ImpactPreset>,
    /// `vehicleTireSkidFX` and `vehicleTireRoadFX` (character `+0x35c`, `+0x360`): the loop
    /// and the effect of a skidding and of a rolling tyre, by the ground's surface.
    pub tyre_skid: ImpactPreset,
    pub tyre_road: ImpactPreset,
    /// His weapons, primary first; the car's guns come last.
    pub weapons: Vec<WeaponDef>,
    /// Each weapon's own object, in the same order.
    pub weapon_objects: Vec<Option<ObjectDef>>,
    /// Each weapon object's own clips, in the same order.
    pub weapon_animations: Vec<Vec<AnimRef>>,
    /// His melee weapon (the axe), with its object and clips.
    pub melee_weapon: Option<weapons::MeleeWeapon>,
    /// What each weapon shows, by the weapon's name.
    pub weapon_effects: Vec<WeaponEffects>,
}

/// Sound events the character names outside its clips, by id (the CRC-32 of the event's
/// name); 0 where the data gives none.
#[derive(Default, Clone, Copy, Debug)]
pub struct Sounds {
    /// `soundTable.vehicleTransmission`: the engine, driven by `rpm` and `load`.
    pub engine: u32,
    /// `soundTable.vehicleDriftStart`: a burst of revs.
    pub drift_start: u32,
    pub suspension: u32,
    pub turbo: u32,
    pub turbo_ready: u32,
    /// `footStepFX`: the sound for the default ground material.
    pub footstep: u32,
}

/// One entry of a `MaterialImpactPreset`'s `materialInfos`. [data]
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImpactInfo {
    /// The surface materials this entry is for, by the CRC-32 of their names.
    pub materials: Vec<u32>,
    /// `soundOnImpact`: a sound event's id, 0 for none.
    pub sound: u32,
    /// `effectOnImpact`: the particle template, by the CRC-32 of its name (`r_hit_melee`
    /// for every entry of his melee preset), and the template itself.
    pub effect: Option<u32>,
    pub template: Option<Template>,
    /// `effectOnImpactFlags`: how the effect is turned (bit 0 z along the normal, 1 z up,
    /// 2 y along the normal, 3 y up, 4 and 5 a random y or z, 6 and 7 along the forward
    /// handed in) and bit 8, tied to the object that was hit (`FUN_0077c180`). [game]
    pub effect_flags: u32,
}

/// A `MaterialImpactPreset`: what an impact sounds and looks like on each surface. The game
/// keeps them by name (`thunk_FUN_01184300`) and asks with a surface (`FUN_0077cd60`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImpactPreset {
    pub infos: Vec<ImpactInfo>,
    /// Persistent surface damage, selected independently of materialInfos. [data/game]
    pub ssd: Vec<crate::ssd::Info>,
}

impl ImpactPreset {
    /// The entry for a surface (`FUN_0077bff0`): the first that lists it, else the first
    /// that lists `0 - Default`, else none. [game]
    pub fn pick(&self, surface: u32) -> Option<&ImpactInfo> {
        let listing = |material: u32| self.infos.iter().find(|info| info.materials.contains(&material));
        listing(surface).or_else(|| listing(crate::melee::surface::DEFAULT))
    }
}

fn read_impact(preset: Option<Node>) -> ImpactPreset {
    let infos = preset.and_then(|preset| preset.get("materialInfos")).into_iter().flat_map(|infos| infos.items());
    ImpactPreset {
        ssd: crate::ssd::read(preset),
        infos: infos
            .map(|info| ImpactInfo {
                materials: info.get("material").map(|m| m.ints().into_iter().map(|h| h as u32).collect()).unwrap_or_default(),
                sound: info.get("soundOnImpact").and_then(Node::int).map_or(0, |h| h as u32),
                effect: info.get("effectOnImpact").and_then(Node::label),
                template: info.get("effectOnImpact").and_then(Template::read),
                effect_flags: info.get("effectOnImpactFlags").and_then(Node::int).map_or(0, |v| v as u32),
            })
            .collect(),
    }
}

/// What a weapon shows as it fires and where its rounds land. [data]
/// Who starts each and how it is turned is NOT read: see `game\src\effects.rs`.
#[derive(Clone, Debug, Default)]
pub struct WeaponEffects {
    /// The weapon's name, as in its `WeaponDef`.
    pub name: u32,
    /// `muzzleFlashEffect`.
    pub muzzle_flash: Option<Template>,
    /// A gun's `bulletProjectileObj`'s `BulletTrace.impactFX`: by the surface struck.
    pub impact: ImpactPreset,
    /// The projectile object's own `ParticleEmitter`: what it trails.
    pub trail: Option<Template>,
    /// The `ParticleEmitter` of the object the projectile's `DamageLink` lets go of
    /// where it ends (a missile's explosion).
    pub blast: Option<Template>,
    pub blast_ssd: Vec<crate::ssd::Info>,
    /// Anonymous SoundPlayers of the projectile and its released objects. [data]
    pub flight_sounds: Vec<u32>,
    pub blast_sounds: Vec<u32>,
}

fn read_weapon_effects(character: Node) -> Vec<WeaponEffects> {
    let mut out = Vec::new();
    for file in character.get("weapons").into_iter().flat_map(|weapons| weapons.items()) {
        let attached = file.path(&["obj", "attachments"]).into_iter().flat_map(|a| a.items());
        for thing in attached.filter_map(|a| a.get("thing")) {
            if !matches!(thing.type_name(), Some("GunWeapon" | "MissileLauncher")) {
                continue;
            }
            fn things(object: Option<Node<'_>>) -> Vec<Node<'_>> {
                object.and_then(|o| o.get("attachments")).into_iter().flat_map(|a| a.items()).filter_map(|a| a.get("thing")).collect()
            }
            let emitter = |things: &[Node]| things.iter().find(|t| t.type_name() == Some("ParticleEmitter")).and_then(|e| e.get("id")).and_then(Template::read);
            let projectile = things(thing.path(&["bulletProjectileObj", "obj"]).or_else(|| thing.path(&["missileProjectileObj", "obj"])));
            let let_go = things(projectile.iter().find(|t| t.type_name() == Some("DamageLink")).and_then(|link| link.get("thing")));
            out.push(WeaponEffects {
                name: thing.get("weaponName").and_then(Node::int).map_or(0, |h| h as u32),
                muzzle_flash: thing.get("muzzleFlashEffect").and_then(Template::read),
                impact: read_impact(projectile.iter().find(|t| t.type_name() == Some("BulletTrace")).and_then(|trace| trace.get("impactFX"))),
                trail: emitter(&projectile),
                blast: emitter(&let_go),
                blast_ssd: thing.path(&["missileProjectileObj", "obj"]).map(crate::formats::scene::read_explosion_ssd).unwrap_or_default(),
                flight_sounds: thing.path(&["missileProjectileObj", "obj"]).map(scene::read_spawn_sounds).unwrap_or_default(),
                blast_sounds: thing.path(&["missileProjectileObj", "obj"]).map(scene::read_projectile_end_sounds).unwrap_or_default(),
            });
        }
    }
    out
}

fn read_sounds(character: Node) -> Sounds {
    let id = |node: Option<Node>| node.and_then(Node::int).map_or(0, |h| h as u32);
    let table = |field: &str| id(character.path(&["soundTable", field]));
    Sounds {
        engine: table("vehicleTransmission"),
        drift_start: table("vehicleDriftStart"),
        suspension: table("vehicleSuspension"),
        turbo: table("vehicleTurbo"),
        turbo_ready: table("vehicleTurboReady"),
        footstep: id(character.path(&["footStepFX", "materialInfos"]).and_then(|infos| infos.at(0)).and_then(|info| info.get("soundOnImpact"))),
    }
}

pub fn load(game_dir: &Path, pack_name: &str) -> Result<CharacterData, String> {
    let path = game_dir.join("characters").join(format!("{pack_name}.str"));
    let pack = Pack::open(&path)?;
    let chunk = pack.of_type(pack::DATA).next().ok_or("pack has no data object")?;
    let file = DataFile::parse(pack.data(chunk).to_vec())?;
    let name_of = |hash| file.name(hash);

    let mut robot = None;
    let mut vehicle = None;
    let mut animations = AnimSet::default();
    let mut vehicle_animations = Vec::new();
    let mut sounds = Sounds::default();
    let mut chatter = Vec::new();
    let (mut melee_impact, mut step_impact) = (ImpactPreset::default(), ImpactPreset::default());
    let mut climb_impact = Vec::new();
    let (mut tyre_skid, mut tyre_road) = (ImpactPreset::default(), ImpactPreset::default());
    let mut weapon_defs = Vec::new();
    let mut weapon_objects = Vec::new();
    let mut weapon_animations = Vec::new();
    let mut melee_weapon = None;
    let mut weapon_effects = Vec::new();
    for (_, root) in file.root().fields() {
        for thing in root.get("things").into_iter().flat_map(|things| things.items()) {
            match thing.type_name() {
                Some("Object") if robot.is_none() => robot = scene::read_object(thing, name_of),
                Some("Character") => {
                    sounds = read_sounds(thing);
                    chatter = thing.get("battleChatter").map(crate::chatter::read).unwrap_or_default();
                    melee_impact = read_impact(thing.path(&["particleHitTable", "meleeHitSparkPreset"]));
                    step_impact = read_impact(thing.get("footStepFX"));
                    climb_impact = ["climbLeftHandFX", "climbRightHandFX", "climbLeftFootFX", "climbRightFootFX", "climbSlideFX"]
                        .iter()
                        .map(|name| read_impact(thing.path(&["climbData", name])))
                        .collect();
                    tyre_skid = read_impact(thing.get("vehicleTireSkidFX"));
                    tyre_road = read_impact(thing.get("vehicleTireRoadFX"));
                    weapon_defs = weapons::read_defs(thing);
                    weapon_effects = read_weapon_effects(thing);
                    weapon_objects = weapons::read_objects(thing, name_of);
                    weapon_animations = weapons::read_object_clips(&file, thing);
                    melee_weapon = weapons::read_melee(&file, thing, name_of);
                    if let Some(object) = thing.path(&["transformModel", "vehicleProxy", "obj"]) {
                        vehicle = scene::read_object(object, name_of);
                        let attached = object.get("attachments").into_iter().flat_map(|a| a.items());
                        for bundle in attached.filter_map(|a| a.get("thing")).filter(|t| t.type_name() == Some("AnimRefBundle")) {
                            vehicle_animations.extend(anim::read_bundle(&file, bundle));
                        }
                    }
                    if let Some(set) = thing.get("animSet") {
                        animations = anim::read_set(&file, set);
                    }
                }
                _ => {}
            }
        }
    }
    Ok(CharacterData {
        robot: robot.ok_or("pack has no robot object")?,
        vehicle,
        animations,
        vehicle_animations,
        library: Library::load(&pack),
        sounds,
        chatter,
        melee_impact,
        step_impact,
        climb_impact,
        tyre_skid,
        tyre_road,
        weapons: weapon_defs,
        weapon_objects,
        weapon_animations,
        melee_weapon,
        weapon_effects,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::hash::crc32;
    use crate::melee::surface;

    #[test]
    fn anonymous_spawn_players_supply_the_punch_and_missile_sounds() {
        let data = load(Path::new(r"C:\Games2"), "bumblebee").unwrap();
        let launcher = data.weapons.iter().find(|w| w.kind == weapons::Kind::Launcher).unwrap();
        let audio = data.weapon_effects.iter().find(|w| w.name == launcher.name).unwrap();
        assert_eq!(audio.flight_sounds, [crc32(b"WPN_OPTIMUS_MISSLE_LOOP")]);
        assert_eq!(audio.blast_sounds, [crc32(b"EXP_OPTIMUS_MISSLE")]);
        let sounds: Vec<_> = data.robot.spawn_sound_players.iter().flat_map(|s| &s.2).copied().collect();
        assert_eq!(sounds, [crc32(b"IMP_CONC_LAND"); 3]);
        for object in std::iter::once(&data.robot).chain(&data.vehicle) {
            assert!(object.sound_players.iter().all(|p| p.node < object.nodes.len()),
                "installed SoundPlayer attachments must address their object's nodes");
        }
    }

    #[test]
    fn an_impact_preset_falls_back_on_the_default_surface() {
        let info = |materials: &[u32], sound| ImpactInfo { materials: materials.to_vec(), sound, effect: None, template: None, effect_flags: 0 };
        let preset = ImpactPreset { infos: vec![info(&[surface::DEFAULT, 7], 1), info(&[8, 9], 2)], ..Default::default() };
        assert_eq!(preset.pick(9).map(|i| i.sound), Some(2));
        assert_eq!(preset.pick(7).map(|i| i.sound), Some(1));
        assert_eq!(preset.pick(1234).map(|i| i.sound), Some(1), "an unlisted surface is the default one");
        let bare = ImpactPreset { infos: vec![info(&[8], 2)], ..Default::default() };
        assert!(bare.pick(1234).is_none());
    }

    /// Needs the game install, as the other data tests do.
    #[test]
    fn his_tyres_sound_by_the_ground_under_them() {
        let data = load(Path::new(r"C:\Games2"), "bumblebee").unwrap();
        let event = |name: &str| Some(crc32(name.as_bytes()));
        let skid = |name: &str| data.tyre_skid.pick(crc32(name.as_bytes())).map(|info| info.sound);
        let road = |name: &str| data.tyre_road.pick(crc32(name.as_bytes())).map(|info| info.sound);
        assert_eq!(skid("0 - Default"), event("VEH_SPORTS_SKID_PAVE"));
        assert_eq!(skid("Dirt"), event("VEH_SPORTS_SKID_DIRT"));
        assert_eq!(skid("Sand"), event("VEH_SPORTS_SKID_DIRT"));
        assert_eq!(road("0 - Default"), event("VEH_ROAD_PAVE"));
        assert_eq!(road("Dirt"), event("VEH_ROAD_GRASS"));
        assert_eq!(road("Mud"), event("VEH_ROAD_MUD"));
        assert_eq!(road("Sand"), event("VEH_ROAD_SAND"));
        assert_eq!(road("a surface no entry lists"), event("VEH_ROAD_PAVE"));
        assert_eq!(Some(data.sounds.suspension), event("VEH_SUSPENSION_SM"));
    }

    /// Needs the game install.
    #[test]
    fn his_car_has_a_skid_mark_and_smoke_for_each_tyre() {
        let data = load(Path::new(r"C:\Games2"), "bumblebee").unwrap();
        let car = data.vehicle.as_ref().unwrap();
        // Each sits on an effect node of its own; `tireName` names the tyre's node, where
        // the points are put (`FUN_0080bc30`).
        let tyre = |m: &crate::skidmark::SkidMarkDef| car.nodes.iter().find(|n| n.name_hash == m.tyre).map(|n| n.name.as_str());
        let tyres: Vec<_> = car.skid_marks.iter().filter_map(tyre).collect();
        assert_eq!(tyres, ["Tire_Rear_L", "Tire_Rear_R", "Tire_Front_R", "Tire_Front_L"]);
        for mark in &car.skid_marks {
            assert_eq!((mark.point_length, mark.point_width, mark.lifetime), (2.0, 0.3, 10.0));
            assert_eq!(mark.colour, [191.0 / 255.0; 4]);
            let material = data.library.materials.get(&mark.material).expect("the material is in his pack");
            assert_eq!(material.name, "bumblebeevehicle.fx_tireskid01");
            assert!(material.texture("diffuse_texture1_texture").is_some_and(|t| data.library.textures.contains_key(&t)));
        }
        // The tyres' effects by the ground: smoke on the road, dirt on dirt.
        let effect = |preset: &ImpactPreset, name: &str| preset.pick(crc32(name.as_bytes())).and_then(|info| info.template.as_ref()).map(|t| t.name);
        assert_eq!(effect(&data.tyre_skid, "0 - Default"), Some(crc32(b"v_smoke_exhaust_skid_bumblebee")));
        assert_eq!(effect(&data.tyre_skid, "Dirt"), Some(crc32(b"v_dirt_skid")));
        assert_eq!(effect(&data.tyre_road, "0 - Default"), Some(crc32(b"v_asphalt_constant")));
        assert!(data.tyre_skid.infos.iter().chain(&data.tyre_road.infos).all(|info| info.effect_flags == 0x100), "tied to the car");
        assert_eq!(car.collision_fx.len(),1);
        let collision=&car.collision_fx[0];
        assert_eq!((collision.mask,collision.types,collision.min_speed,collision.max_speed),
            (1,7,0.05,1e7));
        assert!(collision.align);
        assert_eq!(collision.sounds,[[0;3];2],"Bumblebee has no CollisionFX SFX overrides");
        assert!(collision.particles.iter().flatten().all(Option::is_some),
            "hard/soft impact, roll and scrape templates resolve from the install");
    }

    /// Needs the game install, as the other data tests do.
    #[test]
    fn installed_chatter_feet_and_collision_sounds_resolve_the_original_bindings() {
        let data = load(Path::new(r"C:\Games2"), "bumblebee").unwrap();
        assert_eq!(data.chatter.len(), 115);
        let generic_attack: Vec<_> = data.chatter.iter().filter(|l| l.action == crate::chatter::ATTACK
            && l.category == crate::chatter::GENERIC && l.subcategory == crate::chatter::ENEMY).collect();
        assert_eq!(generic_attack.len(), 4);
        assert!(generic_attack.iter().all(|l| l.frequency == 0.333));
        let feet = &data.robot.footsteps;
        assert_eq!(feet.len(), 2);
        assert_eq!(feet.iter().map(|f| data.robot.nodes[f.node].name.as_str()).collect::<Vec<_>>(), ["Midfoot_L", "Midfoot_R"]);
        assert!(feet.iter().all(|f| f.height == 1.5));
        assert_ne!(feet[0].side, feet[1].side);
        let car = data.vehicle.as_ref().unwrap();
        assert_eq!(car.collision_sounds.len(), 1);
        let mut sounds = car.collision_sounds[0].clone();
        assert_eq!((sounds.filter.mask, sounds.filter.types, sounds.filter.min_speed), (1, 5, 0.08));
        let impact = sounds.select(crate::collisionfx::Kind::Impact, false, false).unwrap();
        assert_eq!(impact.id, crc32(b"VEH_CAR_IMP_OBJ"));
        assert!(impact.attached && !impact.once);
        let scrape = sounds.select(crate::collisionfx::Kind::Scrape, true, false).unwrap();
        assert_eq!(scrape.id, crc32(b"VEH_IMP_SKID_WALL"));
        assert!(scrape.attached);
        assert_ne!(sounds.select(crate::collisionfx::Kind::Impact, true, true).unwrap().id, impact.id);
        assert_eq!(data.robot.sound_players.iter().map(|p| (p.script, p.id)).collect::<Vec<_>>(), [
            (crc32(b"allspark1"), crc32(b"UI_MP_ALLSPARK_PICKUP")),
            (crc32(b"allspark1"), crc32(b"UI_MP_ALLSPARK_ENERGY_LP")),
            (crc32(b"jet_dodge"), crc32(b"ANIM_AUTOBOT_DODGE_THRUST")),
            (crc32(b"specialAttack"), crc32(b"WPN_BUMBLEBEE_SPEC_FIRE")),
        ]);
        assert!(data.robot.sound_players.iter().chain(&car.sound_players).all(|p| p.wait_for_trigger && !p.once));
        assert!(data.robot.sound_players.iter().all(|p| p.retrigger));
        assert!(!car.sound_players.iter().find(|p| p.script == crc32(b"FX_turbocharged")).unwrap().retrigger);
    }

    /// Needs the game install, as the other data tests do.
    #[test]
    fn his_punch_sounds_by_what_it_strikes() {
        let data = load(Path::new(r"C:\Games2"), "bumblebee").unwrap();
        let sound = |name: &str| data.melee_impact.pick(crc32(name.as_bytes())).map(|info| info.sound);
        let event = |name: &str| Some(crc32(name.as_bytes()));
        assert_eq!(data.melee_impact.infos.len(), 7);
        assert_eq!(sound("UpperBody"), event("H2H_PUNCH_SM"));
        assert_eq!(sound("Concrete"), event("H2H_PUNCH_SM_CONC"));
        assert_eq!(sound("MetalSoft"), event("H2H_PUNCH_SM_METAL"));
        assert_eq!(sound("Glass"), event("H2H_PUNCH_SM_GLASS"));
        assert_eq!(sound("no such surface"), event("H2H_PUNCH_SM"));
        // Every entry's effect is tied to the object hit, and is the same template.
        assert!(data.melee_impact.infos.iter().all(|info| info.effect_flags == 0x100));
        assert!(data.melee_impact.infos.iter().all(|info| info.effect == Some(crc32(b"r_hit_melee"))));
        // The step preset's first entry is the one the footsteps have used so far.
        assert_eq!(data.step_impact.pick(surface::DEFAULT).map(|info| info.sound), Some(data.sounds.footstep));
    }

    /// Needs the game install. The blasts of his two ground punches name a camera shake the
    /// game has, and carry a pad rumble.
    #[test]
    fn his_ground_punches_shake_the_camera_and_the_pad() {
        let game = Path::new(r"C:\Games2");
        let data = load(game, "bumblebee").unwrap();
        let tuning = crate::tuning::load(game, "Bumblebee").unwrap();
        let spawner = |name: &str| data.robot.spawners.iter().find(|s| s.script_name == crc32(name.as_bytes())).unwrap();
        let (strong, weak) = (spawner("FX_groundpunch"), spawner("FX_groundpunchweak"));
        assert_eq!(strong.explosion.camera_shake, crc32(b"MassiveCameraShake"));
        assert_eq!(weak.explosion.camera_shake, crc32(b"MedHardCameraShake"));
        let massive = tuning.shakes[&strong.explosion.camera_shake];
        assert_eq!((massive.duration, massive.half_duration, massive.full_dist, massive.max_dist), (2.5, 1.25, 20.0, 90.0));
        assert!((massive.ang_horiz - 1.2_f32.to_radians()).abs() < 1e-6 && massive.pos_vert == 0.35);
        let med_hard = tuning.shakes[&weak.explosion.camera_shake];
        assert_eq!((med_hard.duration, med_hard.half_duration, med_hard.full_dist, med_hard.max_dist), (0.65, 0.15, 25.0, 30.0));
        // The shakes his punch CLIPS name are in no file of the game: they start nothing.
        assert!(!tuning.shakes.contains_key(&crc32(b"MediumShake1_16f")));
        assert!(!tuning.shakes.contains_key(&crc32(b"StrongCameraShake")));
        assert_eq!(tuning.shakes.len(), 14);
        let rumble = |s: &crate::formats::scene::SpawnerDef| {
            let r = s.rumble.unwrap();
            (r.kind, r.duration, (r.strength * 100.0).round(), r.scale_type, r.distance_near, r.distance_far)
        };
        assert_eq!(rumble(strong), (1, 0.7, 55.0, 1, 1.0, 25.0));
        assert_eq!(rumble(weak), (1, 0.3, 55.0, 1, 1.0, 25.0));
        // The preset rumbles, by number: a melee hit (strong, fast), the climb (moved,
        // sliding), the car (wall, ground, turbo).
        use crate::rumble::{Preset, preset};
        let p = |n: usize| tuning.rumble_presets[n];
        assert_eq!(tuning.rumble_presets.len(), 15);
        assert_eq!(p(preset::STRONG_HIT_ATTACKER), Preset { kind: 1, duration: 0.45, strength: 1.0 });
        assert_eq!(p(preset::FAST_HIT_ATTACKER), Preset { kind: 1, duration: 0.25, strength: 1.0 });
        assert_eq!(p(preset::CLIMB_MOVED), Preset { kind: 3, duration: 0.0, strength: 0.4 });
        assert_eq!(p(preset::CLIMB_SLIDING), Preset { kind: 3, duration: 0.2, strength: 0.2 });
        assert_eq!(p(preset::VEHICLE_HIT_WALL), Preset { kind: 1, duration: 0.25, strength: 1.0 });
        assert_eq!(p(preset::VEHICLE_HIT_GROUND), Preset { kind: 1, duration: 0.3, strength: 0.75 });
        assert_eq!(p(preset::VEHICLE_TURBO), Preset { kind: 1, duration: 0.35, strength: 0.75 });
    }

    /// Needs the game install. Who starts his other effects, as his pack has it: the
    /// clips' events, the step preset and the weapons.
    #[test]
    fn his_clips_steps_and_weapons_name_their_effects() {
        use crate::formats::anim::EffectEventKind;
        let data = load(Path::new(r"C:\Games2"), "bumblebee").unwrap();
        let clip = |set: &str, id: &str| data.animations.sets[set].iter().find(|r| r.id == id).and_then(|r| r.clip.as_ref()).unwrap();
        // A landing: dust at a foot at tick 8, left where it was made.
        let land: Vec<_> = clip("IdleStandSet", "Jump_Land")
            .effect_events
            .iter()
            .filter_map(|e| match &e.kind {
                EffectEventKind::Spawn { id, template, track, use_rotation, .. } => Some((e.time, *id, template.name, *track, *use_rotation, template.unread())),
                _ => None,
            })
            .collect();
        assert_eq!(land, [(8.0, 0, crc32(b"r_contactdust_landing"), false, false, Vec::new())]);
        // Climbing: steps of hands and feet with the climbing sides, and a preset for each
        // of the five whose effect for a plain wall is the climbing dust.
        let climbing = data.animations.sets.iter().flat_map(|(_, clips)| clips).filter_map(|r| r.clip.as_ref()).flat_map(|c| &c.effect_events);
        let limbs: std::collections::BTreeSet<usize> =
            climbing.filter_map(|e| if let EffectEventKind::ClimbStep { limb } = e.kind { Some(limb) } else { None }).collect();
        assert!(limbs.len() >= 4, "{limbs:?}");
        assert_eq!(data.climb_impact.len(), 5);
        let dust = data.climb_impact[0].pick(surface::DEFAULT).and_then(|info| info.template.as_ref()).unwrap();
        assert_eq!(dust.name, crc32(b"r_contactdust_climb"));
        // A walk: a step for each foot, each on its foot's node.
        let steps: Vec<_> = clip("WalkSet", "Walk").effect_events.iter().filter(|e| matches!(e.kind, EffectEventKind::FootStep { .. })).map(|e| (e.time, e.node)).collect();
        assert_eq!(steps, [(18.0, crc32(b"Foot_L")), (50.0, crc32(b"Foot_R"))]);
        for event in clip("WalkSet", "Walk").effect_events.iter() {
            if let EffectEventKind::FootStep { side } = event.kind {
                assert!(data.robot.footsteps.iter().any(|foot| foot.side == side), "step side must name a component");
            }
        }
        let orientation_flags: Vec<_> = data.step_impact.infos.iter().chain(data.climb_impact.iter().flat_map(|p| &p.infos))
            .filter(|info| info.template.is_some()).map(|info| info.effect_flags).collect();
        assert!(orientation_flags.iter().all(|flags| matches!(flags, 1 | 0x100)), "step/climb orientation flags {orientation_flags:x?}");
        let step = data.step_impact.pick(surface::DEFAULT).and_then(|info| info.template.as_ref()).unwrap();
        assert_eq!((step.name, step.unread()), (crc32(b"r_footstep_small_concrete"), Vec::new()));
        // The cannon: a flash, and an impact effect by the surface; the launcher: a
        // flash, a trail and an explosion; the car's gun: a flash and a trail.
        let names: Vec<_> = data.weapon_effects.iter().map(|w| (w.muzzle_flash.as_ref().map(|t| t.name), w.impact.infos.len(), w.trail.as_ref().map(|t| t.name), w.blast.as_ref().map(|t| t.name))).collect();
        assert_eq!(
            names,
            [
                (Some(crc32(b"w_muzzle_solar_plasma_cannon")), 15, None, None),
                (Some(crc32(b"w_muzzle_flash_micro_missiles")), 0, Some(crc32(b"w_bullet_trail_micro_missiles2")), Some(crc32(b"w_exp_micro_missiles2"))),
                (Some(crc32(b"w_muzzle_flash_vehicle_autobot")), 15, Some(crc32(b"w_bullet_trail_vehicle_autobot")), None),
            ]
        );
        // Every action and domain of the effects his clips start is one the port runs
        // (the values in slots 13 to 15 are kept by nothing here).
        for r in data.animations.sets.values().flatten() {
            for event in r.clip.iter().flat_map(|clip| &clip.effect_events) {
                if let EffectEventKind::Spawn { template, .. } = &event.kind {
                    assert!(template.unread().iter().all(|what| what.starts_with("slot")), "{}: {:?}", r.id, template.unread());
                }
            }
        }
    }

    /// Needs the game install. The effect of a melee hit and of the two ground punches,
    /// as his pack has them, and that everything in them is something the port runs.
    #[test]
    fn his_hit_and_punch_effects_are_read_whole() {
        use crate::particles::{Domain, Kind};
        use glam::Vec3;
        let data = load(Path::new(r"C:\Games2"), "bumblebee").unwrap();
        let hit = data.melee_impact.pick(surface::UPPER_BODY).unwrap();
        assert_eq!(hit.effect, Some(crc32(b"r_hit_melee")));
        let t = hit.template.as_ref().unwrap();
        assert_eq!(t.unread(), Vec::<String>::new());
        // The flash, the electric flicker, the sparks.
        let e: Vec<_> = t.elements.iter().map(|e| (e.max_particles, e.end, e.local_space, e.actions.len())).collect();
        assert_eq!(e, [(1, 0.3, true, 4), (2, 0.6, true, 3), (3, 0.6, false, 5)]);
        let flash = &t.elements[0];
        assert_eq!(flash.renderer.texture, 0x47b1_03e3);
        assert!(flash.renderer.texture_path.ends_with("fx_blueflash.dds") && flash.renderer.blend_mode == 1);
        assert!(data.library.textures.contains_key(&flash.renderer.texture), "the texture is in his pack");
        let Kind::Burst { count, slots } = &flash.actions[0].kind else { panic!("{:?}", flash.actions[0]) };
        assert_eq!((*count, slots[1], slots[9]), (2.0, Some(Domain::Point(Vec3::new(5.0, 0.0, 0.0))), Some(Domain::Point(Vec3::new(0.2, 0.0, 0.0)))));
        assert_eq!(flash.actions[1].kind, Kind::LinearScale { delta: 7.0 });
        assert_eq!(flash.actions[2].kind, Kind::AlphaFade { begin_alpha: 1.0, fade_begin: 0.5 });
        let sparks = &t.elements[2];
        assert!(sparks.renderer.rectangle() && sparks.renderer.velocity_tracked);
        assert_eq!(t.elements[1].renderer.frames, (2, 2));
        // The punches: both on `Hand_R`, upright and tied to the hand; each texture there.
        let effect = |name: &str| data.robot.effects.iter().filter(|e| e.script_name == crc32(name.as_bytes())).collect::<Vec<_>>();
        let (strong, weak) = (effect("FX_groundpunch"), effect("FX_groundpunchweak"));
        assert_eq!(weak.len(), 1, "{:?}", weak.iter().map(|e| e.template.name).collect::<Vec<_>>());
        assert_eq!(weak[0].template.name, crc32(b"r_attack_radial_small"));
        assert_eq!(weak[0].mesh.as_deref(), Some("R_RadialAttack_small.RadialAttack_small"));
        assert_eq!(weak[0].timeout, Some((0.5, 0.2)));
        // The strong one: a spawner whose object is a mesh alone, and the particles as a
        // `TriggerableParticle`.
        assert_eq!(strong.len(), 2);
        assert_eq!((strong[0].mesh.as_deref(), strong[0].timeout), (Some("R_RadialAttack_Optimus.RadialAttack_Optimus_a"), Some((0.7, 0.5))));
        assert!(strong[0].template.elements.is_empty() && !strong[0].attach);
        assert_eq!((strong[1].template.name, strong[1].attach), (crc32(b"r_attack_radial_optimus"), true));
        assert!(!weak[0].attach, "the data says the weak punch's effect stays where it was put");
        // Each spawner's object: one mesh the pack has, and a clip that scales it.
        for (e, ticks) in [(weak[0], 20.0), (strong[0], 70.0)] {
            let proxy = e.proxy.as_ref().unwrap();
            assert!(proxy.renders.iter().all(|r| data.library.meshes.contains_key(&crate::formats::hash::lower33(&r.model))), "{:?}", e.mesh);
            let clip = e.clip.as_ref().unwrap();
            assert_eq!(clip.length, ticks);
            let grown = |tick: f32| clip.channels.iter().find_map(|c| c.scale.sample(tick, clip.length, false)).unwrap();
            println!("{:?}: {} renders, scale {:?} -> {:?} -> {:?}", e.mesh, proxy.renders.len(), grown(0.0), grown(ticks / 2.0), grown(ticks));
        }
        for e in strong.iter().chain(&weak) {
            assert_eq!(data.robot.nodes[e.node].name, "Hand_R");
            assert!(e.upright, "{:08x}", e.template.name);
            assert_eq!(e.template.unread(), Vec::<String>::new());
            for element in &e.template.elements {
                let r = &element.renderer;
                let texture = data.library.textures.get(&r.texture).unwrap_or_else(|| panic!("{}", r.texture_path));
                let name = r.texture_path.rsplit('\\').next().unwrap();
                println!("{name}: {:?} {}x{}, BlendMode {}", texture.format, texture.width, texture.height, r.blend_mode);
            }
        }
        println!("strong: {:?}", strong.iter().map(|e| (e.template.name, e.template.elements.len(), e.mesh.clone())).collect::<Vec<_>>());
    }
}
