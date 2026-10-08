//! Installed Decepticon scout and shared character combat. See notes/damage.md.
use crate::camera::Sight;
use crate::formats::{
    hash::crc32,
    lxb::{DataFile, Node},
    pack::{self, Pack},
};
use crate::melee::{Target, surface};
use crate::{
    character,
    damage::{Actor, Packet},
    sim::{self, Arena, Input, State},
    tuning::{self, Tuning},
    weapons::{self, Fired, Part, Weapon, WeaponTuning},
};
use glam::{Vec2, Vec3};
use std::path::Path;

/// Shared collision space, but auto-target searches consider the opposing character.
struct FightWorld<'a> {
    space: &'a Arena,
    foes: &'a [Target],
}
impl sim::World for FightWorld<'_> {
    fn floor(&self, x: f32, y: f32, z: f32) -> f32 {
        self.space.floor(x, y, z)
    }
    fn push_out(&self, pos: &mut Vec3, radius: f32, height: f32) -> bool {
        self.space.push_out(pos, radius, height)
    }
    fn follow_floor(&self, from: Vec3, to: Vec2) -> Option<f32> {
        self.space.follow_floor(from, to)
    }
    fn targets(&self) -> &[Target] {
        self.foes
    }
    fn ledge(&self, from: Vec3, toward: Vec2, reach: f32) -> Option<crate::climb::Ledge> {
        self.space.ledge(from, toward, reach)
    }
    fn drop_onto(&self, from: Vec3, to_z: f32, radius: f32) -> Option<Vec3> {
        sim::World::drop_onto(self.space, from, to_z, radius)
    }
    fn surface_contacts(&self, body: &crate::melee::Body) -> Vec<crate::ssd::Contact> {
        sim::World::surface_contacts(self.space, body)
    }
}

pub struct Scout {
    pub character: character::CharacterData,
    pub tuning: Tuning,
    pub gun_index: usize,
    pub gun: WeaponTuning,
    pub brain: Brain,
    pub rig: crate::pose::Rig,
}
#[derive(Clone, Debug)]
pub struct Brain {
    pub sight: f32,
    pub melee_distance: f32,
    pub attack_height: f32,
    pub attack_delay: f32,
    pub ranged_distance: f32,
    pub backpedal_distance: f32,
}
impl Scout {
    pub fn gun_clips(&self) -> crate::gun::Clips<'_> {
        crate::gun::Clips {
            arm: self
                .character
                .animations
                .sets
                .get("WeaponPartialSet")
                .map_or(&[], Vec::as_slice),
            entries: self
                .character
                .animations
                .incoming
                .get("WeaponPartialSet")
                .map_or(&[], Vec::as_slice),
            object: self
                .character
                .weapon_animations
                .get(self.gun_index)
                .map_or(&[], Vec::as_slice),
        }
    }
    pub fn load(dir: &Path) -> Result<Self, String> {
        let character = character::load(dir, "dcomscout")?;
        let mut tuning = tuning::load(dir, "DComScout")?;
        tuning.locomotion =
            tuning::Locomotion::from_animations(&character.animations, &mut tuning.missing);
        tuning.actions = tuning::Actions::from_animations(
            &character.animations,
            &character.robot,
            &mut tuning.missing,
        )
        .with_vehicle(character.vehicle.as_ref());
        tuning.weapon_set = tuning::RuleSet::from_animations(
            &character.animations,
            "WeaponSet",
            &mut tuning.missing,
        );
        tuning.rule_sets = tuning::RuleSet::all(&character.animations);
        let guns = weapons::load(dir, &character.weapons)?;
        let gun_index = guns
            .iter()
            .position(|g| !g.vehicle_mode)
            .ok_or("Scout has no robot gun")?;
        let pack = Pack::open(&dir.join("bnxglobal.str"))?;
        let files: Vec<_> = pack
            .of_type(pack::DATA)
            .filter_map(|c| DataFile::parse(pack.data(c).to_vec()).ok())
            .collect();
        let attrs = files
            .iter()
            .find_map(|f| f.root().get("characterAttributesDComScout"))
            .ok_or("Scout attributes missing")?;
        let number = |name| {
            attrs
                .get(name)
                .and_then(Node::float)
                .ok_or_else(|| format!("Scout missing {name}"))
        };
        let brain = Brain {
            sight: number("sightRange")?,
            melee_distance: number("attackMeleeDistance")?,
            attack_height: number("attackHeight")?,
            attack_delay: number("attackDelay")?,
            ranged_distance: number("attackRangeDistance")?,
            backpedal_distance: number("backPedalDistance")?,
        };
        let rig = crate::pose::Rig::new(&character.robot);
        Ok(Self {
            character,
            tuning,
            gun_index,
            gun: guns[gun_index].clone(),
            brain,
            rig,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Enemy {
    pub state: State,
    pub gun: Weapon,
    pub weapon_pose: crate::gun::Gun,
    pub home: Vec3,
    melee_wait: f32,
    engaged: bool,
    seed: u32,
}
#[derive(Clone, Copy, Debug)]
pub struct Shot {
    pub from: Vec3,
    pub to: Vec3,
    pub target: Option<u32>,
    pub packet: Packet,
}
impl Enemy {
    pub fn new(id: u32, home: Vec3, scout: &Scout) -> Self {
        let mut state = State::new(&scout.tuning);
        state.pos = home;
        state.actor = Actor {
            id,
            team: scout.tuning.damage.team,
            player: false,
            character: crc32(b"Swindle"),
            ..Default::default()
        };
        Self {
            state,
            gun: Weapon::default(),
            weapon_pose: Default::default(),
            home,
            melee_wait: 0.0,
            engaged: false,
            seed: id.wrapping_mul(0x9e37_79b9).max(1),
        }
    }
    pub fn target(&self, t: &Tuning) -> Target {
        Target {
            id: self.state.actor.id,
            pos: self.state.pos,
            radius: t.robot_radius,
            height: t.robot_height,
            character: true,
            large: t.damage.large,
            surface: surface::UPPER_BODY,
        }
    }
    pub fn receive(&mut self, packet: Packet, scout: &Scout) -> crate::damage::Resolved {
        let result = self.state.receive_damage(packet, &scout.tuning);
        if result.accepted {
            self.engaged = true;
            for (_, heat) in self.state.heat_damage.drain(..) {
                self.gun.add_heat(&scout.gun, heat);
            }
        }
        result
    }
    /// [game] Attack behaviours request the controller buttons (008297e0/00822360).
    /// [data] Scout melee has priority .92 over ranged .6, range7 and height10.
    /// [stand-in] Arena steering/engagement and individual attack-delay bookkeeping;
    /// original goals, tactics-point navigation and shared kung-fu slots are not ported.
    pub fn step(
        &mut self,
        scout: &Scout,
        player: &State,
        player_t: &Tuning,
        arena: &Arena,
        passive: bool,
        dt: f32,
    ) -> Vec<Shot> {
        self.melee_wait = (self.melee_wait - dt).max(0.0);
        self.state.difficulty = player.difficulty;
        let mut world = arena.clone();
        world.targets.retain(|t| t.id != self.state.actor.id);
        if !player.dead() {
            world.targets.push(Target {
                id: player.actor.id,
                pos: player.pos,
                radius: if player.is_vehicle() {
                    player_t.vehicle_radius
                } else {
                    player_t.robot_radius
                },
                height: if player.is_vehicle() {
                    player_t.vehicle_height
                } else {
                    player_t.robot_height
                },
                character: true,
                large: player_t.damage.large,
                surface: surface::UPPER_BODY,
            });
        }
        let offset = player.pos - self.state.pos;
        let distance = offset.truncate().length();
        let direction = offset.truncate().normalize_or(Vec2::Y);
        let eye = self.state.pos
            + Vec3::Z * scout.tuning.robot_height * scout.tuning.aiming_height_percentage * 0.01;
        let aim = player.pos
            + Vec3::Z
                * (if player.is_vehicle() {
                    player_t.vehicle_height
                } else {
                    player_t.robot_height
                })
                * player_t.aiming_height_percentage
                * 0.01;
        let visible = arena.sight(eye, aim).is_none();
        self.engaged |= distance <= scout.brain.sight && visible;
        let active = !passive && self.engaged && !player.dead() && !self.state.dead();
        let melee = active
            && visible
            && !player.is_vehicle()
            && player.ground_flag()
            && distance <= scout.brain.melee_distance
            && offset.z.abs() <= scout.brain.attack_height;
        let start_melee = melee && self.melee_wait <= 0.0;
        if start_melee {
            self.melee_wait = scout.brain.attack_delay;
        }
        let ranged = active && !melee && visible && distance <= scout.brain.ranged_distance;
        let movement = if active && !melee && !ranged {
            direction
        } else if ranged && distance < scout.brain.backpedal_distance {
            -direction
        } else {
            Vec2::ZERO
        };
        if active && !self.state.dead() && self.state.mode != sim::Mode::Action {
            self.state.yaw = (-direction.x).atan2(direction.y);
        }
        let input = Input {
            stick: movement,
            aim: ranged.then_some(direction),
            buttons_down: if ranged {
                1 << crate::control::button::WEAPON_FIRE
            } else if start_melee {
                1 << crate::control::button::ATTACK_FAST
            } else {
                0
            },
            buttons_hit: if start_melee {
                1 << crate::control::button::ATTACK_FAST
            } else {
                0
            },
            ..Input::default()
        };
        let foes: Vec<_> = world
            .targets
            .iter()
            .filter(|t| t.id == player.actor.id)
            .copied()
            .collect();
        sim::step(
            &mut self.state,
            &input,
            &scout.tuning,
            &FightWorld {
                space: &world,
                foes: &foes,
            },
            dt,
        );
        let clips = scout.gun_clips();
        let def = &scout.character.weapons[scout.gun_index];
        if self.state.state_class(&scout.tuning) == Some("StateWeapon") && !self.state.dead() {
            self.weapon_pose.take_aim(clips, def, false);
        } else {
            self.weapon_pose.put_away(clips, def);
        }
        let asked = self.state.firing && ranged && !self.state.dead();
        let trigger = asked && !self.weapon_pose.holds_trigger(clips, asked);
        self.weapon_pose.step(clips, def.name, dt);
        let fired = self.gun.step_status(
            &scout.gun,
            trigger,
            self.state.damage_timers.slowed > 0.0,
            &scout.tuning.damage,
            dt,
        );
        let count = match fired {
            Fired::Rounds(n) | Fired::RoundsAndOverheated(n) => n,
            _ => 0,
        };
        let mut shots = Vec::new();
        let muzzle = self.muzzle(scout);
        for _ in 0..count {
            self.weapon_pose.fired(clips, def.name);
            let forward = (aim - muzzle).normalize_or(Vec3::Y);
            let right = forward.cross(Vec3::Z).normalize_or(Vec3::X);
            let up = right.cross(forward);
            let angle = (self.random() * 2.0 - 1.0) * std::f32::consts::PI;
            let radius = (self.random() * 2.0 - 1.0) * scout.gun.general_noise_radius;
            // [game] 007aa210 non-player noise: fixed-radius disc across muzzle-to-aim.
            let at = aim + (right * angle.cos() + up * angle.sin()) * radius;
            let (from, to) = weapons::bullet_line(muzzle, at, &scout.gun);
            let mut landed = arena.sight(from, to).map(|h| (h.point, None));
            for target in &world.targets {
                if let Some(k) = intersect_cylinder(from, to, target) {
                    let point = from + (to - from) * k;
                    if landed.is_none_or(|(p, _)| {
                        p.distance_squared(from) > point.distance_squared(from)
                    }) {
                        landed = Some((point, Some(target.id)));
                    }
                }
            }
            let end = landed.map_or(to, |(p, _)| p);
            let amount = weapons::bullet_damage(
                &scout.gun,
                1.0,
                if player.is_vehicle() {
                    Part::VehicleBody
                } else {
                    Part::UpperBody
                },
                end.distance(muzzle),
                false,
            );
            let packet = Packet {
                source: Some(self.state.actor),
                ..Packet::bullet(&scout.gun, amount, (to - from).normalize_or(Vec3::Y))
            };
            shots.push(Shot {
                from: muzzle,
                to: end,
                target: landed.and_then(|(_, id)| id),
                packet,
            });
        }
        shots
    }
    pub fn muzzle(&self, scout: &Scout) -> Vec3 {
        let def = &scout.character.weapons[scout.gun_index];
        let playing = self.state.action_clip(&scout.tuning).or_else(|| {
            self.state
                .base_clip(&scout.tuning)
                .map(|c| (c.set, c.id, c.tick))
        });
        let clip = playing.and_then(|(set, id, tick)| {
            scout
                .character
                .animations
                .sets
                .get(set)?
                .iter()
                .find(|r| r.id == id)
                .and_then(|r| r.clip.as_ref().map(|c| (c, tick, r.looping)))
        });
        let nodes = clip.map_or_else(
            || scout.rig.rest(),
            |(c, tick, looping)| scout.rig.pose(c, looping, tick),
        );
        let nodes = match (clip, self.weapon_pose.arm) {
            (_, Some(arm)) => scout
                .gun_clips()
                .arm
                .get(arm.clip)
                .and_then(|r| r.clip.as_ref().map(|over| (r, over)))
                .map_or(nodes.clone(), |(r, over)| {
                    scout.rig.pose_layer(
                        clip.map(|(c, tick, looping)| (c, looping, tick)),
                        over,
                        r.looping,
                        arm.tick,
                        r.additive,
                        arm.weight(),
                    )
                }),
            _ => nodes,
        };
        let arm = scout
            .character
            .robot
            .nodes
            .iter()
            .position(|n| n.name_hash == def.bone)
            .and_then(|i| nodes.get(i));
        let local = arm.map_or(
            Vec3::Z * scout.tuning.robot_height * scout.tuning.aiming_height_percentage * 0.01,
            |a| a.transform_point3(def.muzzle),
        );
        self.state.pos + glam::Quat::from_rotation_z(self.state.yaw) * local
    }
    fn random(&mut self) -> f32 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        (self.seed >> 8) as f32 / (1u32 << 24) as f32
    }
}

/// [stand-in] Arena character collision is one cylinder, rather than per-body surfaces.
pub fn intersect_cylinder(from: Vec3, to: Vec3, target: &Target) -> Option<f32> {
    let along = to - from;
    let (flat, offset) = (along.truncate(), (from - target.pos).truncate());
    let a = flat.length_squared();
    let b = offset.dot(flat);
    let c = offset.length_squared() - target.radius * target.radius;
    if c <= 0.0 && (0.0..=target.height).contains(&(from.z - target.pos.z)) {
        return Some(0.0);
    }
    let mut best = None;
    if a > 1e-9 && b * b - a * c >= 0.0 {
        let k = (-b - (b * b - a * c).sqrt()) / a;
        if (0.0..=1.0).contains(&k)
            && (0.0..=target.height).contains(&(from.z + along.z * k - target.pos.z))
        {
            best = Some(k);
        }
    }
    for z in [target.pos.z, target.pos.z + target.height] {
        if along.z.abs() > 1e-9 {
            let k = (z - from.z) / along.z;
            if (0.0..=1.0).contains(&k)
                && (from + along * k - target.pos).truncate().length_squared()
                    <= target.radius * target.radius
                && best.is_none_or(|v| k < v)
            {
                best = Some(k);
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::damage::flags;
    fn installed() -> &'static Scout {
        static DATA: std::sync::OnceLock<Scout> = std::sync::OnceLock::new();
        DATA.get_or_init(|| Scout::load(Path::new("C:/Games2")).expect("installed scout"))
    }
    fn player_tuning() -> Tuning {
        let dir = Path::new("C:/Games2");
        let data = character::load(dir, "bumblebee").unwrap();
        let mut t = tuning::load(dir, "Bumblebee").unwrap();
        t.locomotion = tuning::Locomotion::from_animations(&data.animations, &mut t.missing);
        t.actions = tuning::Actions::from_animations(&data.animations, &data.robot, &mut t.missing)
            .with_vehicle(data.vehicle.as_ref());
        t.weapon_set =
            tuning::RuleSet::from_animations(&data.animations, "WeaponSet", &mut t.missing);
        t.rule_sets = tuning::RuleSet::all(&data.animations);
        t
    }
    #[test]
    fn installed_scout_fires_original_gun_and_can_kill_player() {
        let scout = installed();
        let t = player_tuning();
        let mut player = State::new(&t);
        player.health = 4.0; // Two actual gun rounds must kill before an overheat recovery.
        let mut enemy = Enemy::new(1, Vec3::new(0.0, 35.0, 0.0), scout);
        let arena = Arena::default();
        let mut hits = 0;
        for _ in 0..3000 {
            for shot in enemy.step(scout, &player, &t, &arena, false, sim::GAME_UPDATE) {
                assert_eq!(shot.packet.flags, 0x200200);
                assert_eq!(shot.packet.amount, 2.0);
                if shot.target == Some(0) {
                    player.receive_damage(shot.packet, &t);
                    hits += 1;
                }
            }
            sim::step(&mut player, &Input::default(), &t, &arena, sim::GAME_UPDATE);
            if player.dead() {
                break;
            }
        }
        assert!(
            hits > 0,
            "state {:?}, gun {:?}",
            enemy.state.control_state(&scout.tuning),
            enemy.weapon_pose
        );
        assert!(player.dead(), "health {} after {hits} hits", player.health);
        for _ in 0..340 {
            sim::step(&mut player, &Input::default(), &t, &arena, sim::GAME_UPDATE);
        }
        assert!(player.restart_requested);
    }
    #[test]
    fn installed_scout_melee_is_a_real_clip_contact() {
        let scout = installed();
        let t = player_tuning();
        let player = State::new(&t);
        let mut enemy = Enemy::new(1, Vec3::new(0.0, 5.0, 0.0), scout);
        let mut hits = Vec::new();
        for _ in 0..160 {
            enemy.step(
                scout,
                &player,
                &t,
                &Arena::default(),
                false,
                sim::GAME_UPDATE,
            );
            hits.extend(enemy.state.hits.iter().filter(|h| h.target == 0).cloned());
        }
        assert!(
            !hits.is_empty(),
            "state {:?}, pos {:?}",
            enemy.state.control_state(&scout.tuning),
            enemy.state.pos
        );
        assert!(
            hits.iter()
                .all(|h| h.damage > 0.0 && h.flags & flags::BY_MELEE != 0)
        );
    }
    #[test]
    fn emp_stuns_scout_break_budget_and_death_removes_after_clip() {
        let scout = installed();
        let t = player_tuning();
        let player = State::new(&t);
        let mut enemy = Enemy::new(1, Vec3::new(0.0, 35.0, 0.0), scout);
        let emp = Packet {
            source: Some(player.actor),
            flags: flags::STUN,
            stun_time: 2.5,
            ..Default::default()
        };
        enemy.receive(emp, scout);
        for _ in 0..35 {
            assert!(
                enemy
                    .step(
                        scout,
                        &player,
                        &t,
                        &Arena::default(),
                        false,
                        sim::GAME_UPDATE
                    )
                    .is_empty()
            );
        }
        assert_eq!(enemy.state.control_state(&scout.tuning), Some("stateStun"));
        enemy.receive(
            Packet {
                amount: 80.0,
                flags: flags::NO_HIT_REACT,
                ..emp
            },
            scout,
        );
        assert!(enemy.state.damage_timers.stunned > 0.0);
        enemy.receive(
            Packet {
                amount: 0.1,
                flags: flags::NO_HIT_REACT,
                ..emp
            },
            scout,
        );
        assert_eq!(enemy.state.damage_timers.stunned, 0.0);
        enemy.receive(
            Packet {
                amount: 500.0,
                flags: flags::BY_EXPLOSION,
                ..emp
            },
            scout,
        );
        for _ in 0..340 {
            enemy.step(
                scout,
                &player,
                &t,
                &Arena::default(),
                true,
                sim::GAME_UPDATE,
            );
        }
        assert!(enemy.state.dead());
        assert!(enemy.state.restart_requested);
        assert_eq!(enemy.state.health, 0.0);
        assert_eq!(scout.tuning.base_health, 200.0);
        // No invented timed resurrection.
        for _ in 0..400 {
            enemy.step(
                scout,
                &player,
                &t,
                &Arena::default(),
                true,
                sim::GAME_UPDATE,
            );
        }
        assert_eq!(enemy.state.health, 0.0);
    }
    #[test]
    fn cylinder_trace_handles_vertical_and_inside_starts() {
        let target = Enemy::new(1, Vec3::ZERO, installed()).target(&installed().tuning);
        assert_eq!(
            intersect_cylinder(Vec3::Z, Vec3::X * 100.0, &target),
            Some(0.0)
        );
        assert!(intersect_cylinder(Vec3::Z * 100.0, Vec3::ZERO, &target).is_some());
        assert!(
            intersect_cylinder(
                Vec3::new(100.0, 0.0, 100.0),
                Vec3::new(100.0, 0.0, -100.0),
                &target
            )
            .is_none()
        );
    }
    #[test]
    fn bumblebee_emp_does_not_shield_and_disable_blocks_transform() {
        let scout = installed();
        let t = player_tuning();
        let mut player = State::new(&t);
        player.special_left = t.special.duration;
        let attacker = Enemy::new(1, Vec3::Y * 35.0, scout).state.actor;
        let result = player.receive_damage(
            Packet {
                source: Some(attacker),
                amount: t.base_health,
                ..Default::default()
            },
            &t,
        );
        assert_eq!(
            result.health, 0.0,
            "EMP must not grant Optimus's last-health-point protection"
        );
        let mut player = State::new(&t);
        player.receive_damage(
            Packet {
                source: Some(attacker),
                flags: flags::DISABLE_VEHICLE | flags::DISABLE_TRANSFORM,
                stun_time: 0.6,
                ..Default::default()
            },
            &t,
        );
        for _ in 0..15 {
            sim::step(
                &mut player,
                &Input {
                    vehicle: 1.0,
                    ..Default::default()
                },
                &t,
                &Arena::default(),
                0.032,
            );
            assert!(!player.is_vehicle());
        }
        for _ in 0..80 {
            sim::step(
                &mut player,
                &Input {
                    vehicle: 1.0,
                    ..Default::default()
                },
                &t,
                &Arena::default(),
                0.032,
            );
        }
        assert!(player.is_vehicle());
    }
    #[test]
    fn wall_contact_plays_installed_splat_recovery() {
        let t = player_tuning();
        let mut player = State::new(&t);
        player.pos = Vec3::new(0.0, 7.5, 1.0);
        let arena = Arena {
            boxes: vec![sim::Box3 {
                min: Vec3::new(-10.0, 10.0, 0.0),
                max: Vec3::new(10.0, 12.0, 10.0),
            }],
            ..Default::default()
        };
        for _ in 0..4 {
            sim::step(&mut player, &Input::default(), &t, &arena, 0.032);
        }
        player.receive_damage(
            Packet {
                source: Some(Actor {
                    id: 1,
                    team: installed().tuning.damage.team,
                    ..Default::default()
                }),
                amount: 10.0,
                flags: flags::BY_FAST_FLAIL,
                direction: Vec3::Y,
                knockback_speed: 30.0,
                knockback_angle: 0.1,
                ..Default::default()
            },
            &t,
        );
        let mut splat = false;
        let mut recovery = false;
        for _ in 0..250 {
            sim::step(&mut player, &Input::default(), &t, &arena, 0.032);
            splat |= player.damage_type == 0x5060153e;
            recovery |= player
                .action_clip(&t)
                .is_some_and(|(_, id, _)| id == "Recover_WallSplat")
                || player
                    .base_clip(&t)
                    .is_some_and(|c| c.id == "Recover_WallSplat");
        }
        assert!(
            splat && recovery,
            "wall must select its real recovery clip; splat{splat} recovery{recovery}, pos {:?}, kind {:x}, state {:?}, base {:?}",
            player.pos,
            player.damage_type,
            player.control_state(&t),
            player.base
        );
        assert_ne!(player.control_state(&t), Some("stateHitReact"));
    }
    #[test]
    fn slow_changes_motion_and_weapon_cadence_and_heat_then_expires() {
        let scout = installed();
        let t = player_tuning();
        let mut a = State::new(&t);
        let mut b = a.clone();
        b.receive_damage(
            Packet {
                source: Some(Actor {
                    id: 1,
                    team: scout.tuning.damage.team,
                    ..Default::default()
                }),
                flags: flags::SLOW | flags::NO_HIT_REACT,
                slow_time: 2.0,
                ..Default::default()
            },
            &t,
        );
        for _ in 0..30 {
            for s in [&mut a, &mut b] {
                sim::step(
                    s,
                    &Input {
                        stick: Vec2::Y,
                        ..Default::default()
                    },
                    &t,
                    &Arena::default(),
                    0.032,
                );
            }
        }
        assert!(
            b.pos.y < a.pos.y * 0.65 && b.pos.y > a.pos.y * 0.3,
            "slow {} vs {}",
            b.pos.y,
            a.pos.y
        );
        let mut gun_a = Weapon::default();
        let mut gun_b = Weapon::default();
        let (mut n, mut slowed) = (0, 0);
        for _ in 0..50 {
            if matches!(
                gun_a.step_status(&scout.gun, true, false, &t.damage, 0.032),
                Fired::Rounds(_)
            ) {
                n += 1;
            }
            if matches!(
                gun_b.step_status(&scout.gun, true, true, &t.damage, 0.032),
                Fired::Rounds(_)
            ) {
                slowed += 1;
            }
        }
        assert!(slowed < n && gun_b.heat < gun_a.heat);
        for _ in 0..65 {
            sim::step(&mut b, &Input::default(), &t, &Arena::default(), 0.032);
        }
        assert_eq!(b.damage_timers.slowed, 0.0);
        b.receive_damage(
            Packet {
                source: Some(Actor {
                    id: 1,
                    team: scout.tuning.damage.team,
                    ..Default::default()
                }),
                flags: flags::OVERHEAT_ALL,
                heat: 100.0,
                ..Default::default()
            },
            &t,
        );
        assert_eq!(b.heat_damage, vec![(flags::OVERHEAT_ALL, 100.0)]);
    }
    #[test]
    fn difficulty_data_is_used_and_explosion_death_keeps_vertical_root_motion() {
        let t = player_tuning();
        assert_eq!(
            t.damage.difficulties.map(|d| d.enemy_health),
            [0.5, 1.0, 1.5, 2.0]
        );
        let mut player = State::new(&t);
        player.receive_damage(
            Packet {
                source: Some(Actor {
                    id: 1,
                    team: installed().tuning.damage.team,
                    ..Default::default()
                }),
                amount: 500.0,
                flags: flags::BY_EXPLOSION,
                ..Default::default()
            },
            &t,
        );
        let arena = Arena::default();
        sim::step(&mut player, &Input::default(), &t, &arena, 0.032);
        assert!(player.death.as_ref().unwrap().root_z);
        let mut z = 0.0f32;
        for _ in 0..8 {
            let death = player.death.as_ref().unwrap();
            let old = death.tick;
            let clip = death.clip;
            sim::step(&mut player, &Input::default(), &t, &arena, 0.032);
            let death = player.death.as_ref().unwrap();
            assert_eq!(death.clip, clip);
            let moved =
                t.actions.clips[&clip].root_at(death.tick) - t.actions.clips[&clip].root_at(old);
            z = moved.z.max(0.0).max(z);
            assert!(
                (player.vel.z - moved.z / 0.032).abs() < 1e-4,
                "explosion death must use animated Z, not gravity"
            );
        }
        assert!(z > 0.0, "installed explosion clip has vertical travel");
    }
}
