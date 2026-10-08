//! The single-player receiving side of every hit. [game] See notes/damage.md.
use crate::formats::{hash::crc32, lxb::Node};
use crate::melee;
use glam::Vec3;

pub mod flags {
    pub use crate::melee::damage::*;
    pub const OVERHEAT_CURRENT: u32 = 1 << 4;
    pub const OVERHEAT_ALL: u32 = 1 << 5;
    pub const FLASH_BANG: u32 = 1 << 6;
    pub const PROJECTILE: u32 = 1 << 9;
    pub const DISABLE_VEHICLE: u32 = 1 << 12;
    pub const STUN: u32 = 1 << 13;
    pub const SLOW: u32 = 1 << 14;
    pub const NO_HIT_REACT: u32 = 1 << 21;
    pub const DISABLE_TRANSFORM: u32 = 1 << 22;
    pub const PREDAMAGE: u32 = 1 << 23;
    pub const FRIENDS_ONLY: u32 = 1 << 25;
    pub const SPIRIT_LINK: u32 = 1 << 27;
    pub const INSTANT_KILL: u32 = 1 << 28;
    pub const NO_TEAM_FILTER: u32 = 1 << 30;
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Difficulty {
    pub enemy_health: f32,
    pub enemy_damage: f32,
}
impl Default for Difficulty {
    fn default() -> Self {
        Self {
            enemy_health: 1.0,
            enemy_damage: 1.0,
        }
    }
}
impl Difficulty {
    pub fn read(node: Node<'_>) -> Self {
        Self {
            enemy_health: node
                .get("scaleEnemyHealth")
                .and_then(Node::float)
                .unwrap_or(100.0)
                * 0.01,
            enemy_damage: node
                .get("scaleEnemyDamage")
                .and_then(Node::float)
                .unwrap_or(100.0)
                * 0.01,
        }
    }
    /// [game] 0079ac30 returns the reciprocal; NPC max health stays at baseHealth.
    pub fn received(self) -> f32 {
        if self.enemy_health > 0.01 {
            self.enemy_health.recip()
        } else {
            0.0
        }
    }
}

#[derive(Clone, Debug)]
pub struct Tuning {
    pub team: u32,
    pub invincible: bool,
    pub immovable: bool,
    pub large: bool,
    pub reaction_immunity: u32,
    pub vehicle_multiplier: f32,
    pub knockback_multiplier: f32,
    pub regen_duration: f32,
    pub regen_interval: f32,
    pub shield_multiplier: f32,
    pub flight_shield_multiplier: f32,
    pub bumblebee_stun_break: f32,
    pub other_stun_break: f32,
    pub slow_multiplier: f32,
    pub slow_fire_rate: f32,
    pub slow_heat_rate: f32,
    pub overdrive_multiplier: f32,
    pub difficulties: [Difficulty; 4],
}
impl Default for Tuning {
    fn default() -> Self {
        Self {
            team: 0,
            invincible: false,
            immovable: false,
            large: false,
            reaction_immunity: 0,
            vehicle_multiplier: 1.0,
            knockback_multiplier: 1.0,
            regen_duration: 6.0,
            regen_interval: -1.0,
            shield_multiplier: 0.5,
            flight_shield_multiplier: 0.5,
            bumblebee_stun_break: 80.0,
            other_stun_break: 100.0,
            slow_multiplier: 0.5,
            slow_fire_rate: 0.65,
            slow_heat_rate: 0.65,
            overdrive_multiplier: 4.0,
            difficulties: [Difficulty::default(); 4],
        }
    }
}
impl Tuning {
    pub fn read(
        attributes: Node<'_>,
        shared: Option<Node<'_>>,
        difficulty: Option<Node<'_>>,
    ) -> Self {
        let num = |name, default| {
            attributes
                .get(name)
                .and_then(Node::float)
                .unwrap_or(default)
        };
        let shared_num = |name, default| {
            shared
                .and_then(|n| n.get(name))
                .and_then(Node::float)
                .unwrap_or(default)
        };
        Self {
            team: attributes.get("team").and_then(Node::int).unwrap_or(0) as u32,
            invincible: attributes.get("invincible").is_some_and(Node::is_true),
            immovable: attributes.get("immovable").is_some_and(Node::is_true),
            large: attributes.get("size").and_then(Node::int).unwrap_or(0) != 0,
            reaction_immunity: attributes
                .get("invulnFlagsBase")
                .and_then(Node::int)
                .unwrap_or(0) as u32
                & 0xf27f_ffff,
            vehicle_multiplier: num("damageMultiplierInVehicle", 1.0),
            knockback_multiplier: num("knockBackMultiplier", 1.0),
            regen_duration: num("healthRegenDuration", 6.0),
            regen_interval: num("healthRegenSecsPerTic", -1.0),
            shield_multiplier: shared_num("optimusSpecialDamageMultiplier", 0.5),
            flight_shield_multiplier: shared_num("optimusFlightSpecialDamageMultiplier", 0.5),
            bumblebee_stun_break: shared_num("bumbleBeeStunBreakThreshold", 80.0),
            other_stun_break: shared_num("dComWarriorStunBreakThreshold", 100.0),
            slow_multiplier: shared_num("slowMultiplier", 0.5),
            slow_fire_rate: shared_num("slowFireRate", 0.65),
            slow_heat_rate: shared_num("slowHeatMeterIncrementRate", 0.65),
            overdrive_multiplier: 4.0, // Original OverDrive defaults, overwritten from its root.
            difficulties: ["Easy", "Medium", "Hard", "Expert"].map(|name| {
                difficulty
                    .and_then(|n| n.get(name))
                    .map_or(Difficulty::default(), Difficulty::read)
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Actor {
    pub id: u32,
    pub team: u32,
    pub player: bool,
    pub character: u32,
    pub overdrive: bool,
    pub reduced_damage: bool,
}

/// Common record for melee, bullets, explosions, status ticks and scripted hits.
#[derive(Clone, Copy, Debug, Default)]
pub struct Packet {
    pub source: Option<Actor>,
    pub amount: f32,
    pub flags: u32,
    pub direction: Vec3,
    pub knockback_speed: f32,
    pub knockback_angle: f32,
    pub stun_time: f32,
    pub slow_time: f32,
    pub dot_time: f32,
    pub heat: f32,
    pub headshot: bool,
}
impl Packet {
    pub fn bullet(t: &crate::weapons::WeaponTuning, amount: f32, direction: Vec3) -> Self {
        Self {
            amount,
            direction,
            flags: t.damage_flags,
            knockback_speed: t.knock_back_speed,
            knockback_angle: t.knock_back_angle,
            stun_time: t.stun_time,
            slow_time: t.slow_time,
            dot_time: t.dot_time,
            heat: t.heat_damage,
            ..Self::default()
        }
    }
    pub fn melee(hit: &melee::Hit, source: Actor) -> Self {
        Self {
            source: Some(source),
            amount: hit.damage,
            flags: hit.flags,
            direction: hit.direction.extend(0.0),
            knockback_speed: hit.knock_back_speed,
            knockback_angle: hit.knock_back_angle,
            stun_time: hit.stun_time,
            ..Self::default()
        }
    }
    /// Keep a steeper incoming direction; never lower it to the packet's minimum pitch.
    /// [game: 00719830, 006f8950]
    pub fn velocity(self, multiplier: f32) -> Vec3 {
        let mut direction = self.direction;
        if self.knockback_angle >= direction.z.atan2(direction.truncate().length()) {
            let flat = direction.truncate().normalize_or(glam::Vec2::Y);
            let (up, along) = self.knockback_angle.sin_cos();
            direction = (flat * along).extend(up);
        }
        direction * self.knockback_speed * multiplier
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Context {
    pub actor: Actor,
    pub health: f32,
    pub vehicle: bool,
    pub shield: Option<f32>,
    pub enabled: bool,
    pub reject: bool,
    pub invincible: bool,
    pub controller_accepts: bool,
    pub external_health: bool,
    pub difficulty: Difficulty,
    pub immune_to_overdrive: bool,
    pub double_damage: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Resolved {
    pub accepted: bool,
    pub amount: f32,
    pub health: f32,
    pub flags: u32,
    pub velocity: Option<Vec3>,
    pub death_kind: u32,
}

/// [game] 00719830 + 00717f90 + 0071f650, single-player; source health scaling
/// is reciprocal difficulty, outgoing NPC damage scaling belongs to the attacker.
pub fn resolve(packet: Packet, context: Context, t: &Tuning) -> Resolved {
    let mut out = Resolved {
        health: context.health,
        ..Resolved::default()
    };
    let source = packet.source;
    let self_hit = source.is_some_and(|s| s.id == context.actor.id);
    if !context.enabled || context.reject || !context.controller_accepts {
        return out;
    }
    if context.invincible && !(self_hit && packet.flags & 0x40f0f0 != 0) {
        return out;
    }
    if let Some(s) = source {
        if self_hit && packet.flags & 0x40f0f0 == 0 {
            return out;
        }
        if !self_hit && s.team == context.actor.team && packet.flags & 0x4200_0000 == 0 {
            return out;
        }
        if !self_hit && s.team != context.actor.team && packet.flags & flags::FRIENDS_ONLY != 0 {
            return out;
        }
    }
    out.accepted = true;
    out.flags = packet.flags & !7 | source.map_or(4, |s| if s.player { 1 } else { 2 });
    let mut amount = packet.amount;
    if source.is_some_and(|s| s.overdrive) && !context.immune_to_overdrive {
        amount *= t.overdrive_multiplier;
    }
    if !context.actor.player && source.is_some_and(|s| s.player) {
        amount *= context.difficulty.received();
    }
    if packet.flags & flags::PREDAMAGE == 0 {
        if packet.flags & flags::INSTANT_KILL == 0
            && let Some(shield) = context.shield
        {
            amount *= shield;
        }
        if context.vehicle {
            amount *= t.vehicle_multiplier;
        }
    }
    if context.double_damage {
        amount *= 2.0;
    }
    if context.shield.is_some() || packet.flags & flags::SPIRIT_LINK != 0 {
        amount = amount.min(context.health - 1.0);
    }
    if source.is_some_and(|s| s.reduced_damage) {
        amount *= 0.1;
    }
    out.amount = amount;
    if !context.external_health && packet.flags & flags::DOT == 0 {
        out.health = (context.health - amount).max(0.0);
        if out.health < 0.1 || packet.flags & flags::INSTANT_KILL != 0 {
            out.health = 0.0;
            out.death_kind = if packet.headshot {
                0x4782_c9ce
            } else if packet.flags & flags::BY_EXPLOSION != 0 {
                0xf712_8ccc
            } else {
                0x2ae1_e7e9
            };
        }
    }
    if packet.flags & flags::THROWS != 0 {
        out.velocity = Some(packet.velocity(t.knockback_multiplier));
    }
    out
}

#[derive(Clone, Debug, Default)]
pub struct Runtime {
    pub disabled_vehicle: f32,
    pub disabled_transform: f32,
    pub stunned: f32,
    pub stun_budget: f32,
    pub flashbang: f32,
    pub slowed: f32,
    pub dot_left: f32,
    dot: Option<Packet>,
    pub regen_left: f32,
    regen_tick: f32,
    pub regenerating: bool,
}
impl Runtime {
    pub fn stop_regen(&mut self, delay: f32) {
        self.regen_left = delay;
        self.regen_tick = 0.0;
        self.regenerating = false;
    }
    pub fn receive(&mut self, packet: Packet, shield: bool, delay: f32, t: &Tuning) {
        self.stop_regen(delay);
        if shield {
            return;
        }
        if packet.flags & flags::STUN != 0 && packet.stun_time > 0.0 {
            self.stunned = packet.stun_time;
            self.stun_budget = if packet
                .source
                .is_some_and(|s| s.character == crc32(b"Bumblebee"))
            {
                t.bumblebee_stun_break
            } else {
                t.other_stun_break
            };
        } else if self.stunned > 0.0 {
            self.stun_budget -= packet.amount.max(0.0);
            if self.stun_budget < 0.0 {
                self.stunned = 0.0;
            }
        }
        if packet.stun_time > 0.0 {
            if packet.flags & flags::DISABLE_VEHICLE != 0 {
                self.disabled_vehicle = packet.stun_time;
            }
            if packet.flags & flags::DISABLE_TRANSFORM != 0 {
                self.disabled_transform = packet.stun_time;
            }
            if packet.flags & flags::FLASH_BANG != 0 {
                self.flashbang = packet.stun_time;
            }
        }
        if packet.flags & flags::SLOW != 0 && packet.slow_time > 0.0 {
            self.slowed = packet.slow_time;
        }
        if packet.flags & flags::DOT != 0 && packet.dot_time > 0.0 {
            self.dot_left = packet.dot_time.ceil() - 0.01;
            self.dot = Some(Packet {
                flags: flags::NO_HIT_REACT,
                dot_time: 0.0,
                ..packet
            });
        }
    }
    /// [game] 0072a550: status clocks and integer-boundary DOT ticks.
    pub fn step(&mut self, dt: f32) -> Option<Packet> {
        for time in [
            &mut self.disabled_vehicle,
            &mut self.disabled_transform,
            &mut self.stunned,
            &mut self.flashbang,
            &mut self.slowed,
        ] {
            *time = (*time - dt).max(0.0);
        }
        if self.dot_left <= 0.0 {
            return None;
        }
        let before = self.dot_left.trunc() as i32;
        self.dot_left -= dt;
        let ticks = if self.dot_left > 0.0 {
            before - self.dot_left.trunc() as i32
        } else {
            (before - self.dot_left.trunc() as i32).max(1)
        };
        if self.dot_left <= 0.0 {
            self.dot_left = 0.0;
        }
        if ticks <= 0 {
            return None;
        }
        self.dot.map(|p| Packet {
            amount: p.amount * ticks as f32,
            ..p
        })
    }
    /// [game] 0071ffb0: midpoint cubic increment; rate is an enable gate only.
    pub fn regenerate(
        &mut self,
        health: &mut f32,
        maximum: f32,
        enabled: bool,
        delay: f32,
        t: &Tuning,
        dt: f32,
    ) {
        if *health <= 0.0 {
            self.regenerating = false;
            return;
        }
        if *health >= maximum || self.dot_left > 0.0 {
            self.stop_regen(delay);
            return;
        }
        self.regen_left -= dt;
        if self.regen_left > 0.0 || !enabled || t.regen_duration <= 0.0 {
            return;
        }
        self.regenerating = true;
        let interval = if t.regen_interval > 0.0 {
            self.regen_tick += dt;
            if self.regen_tick < t.regen_interval {
                return;
            }
            self.regen_tick -= t.regen_interval;
            t.regen_interval
        } else {
            dt
        };
        let midpoint = interval * 0.5 - self.regen_left;
        *health = (*health
            + 3.0 * maximum / t.regen_duration.powi(3) * midpoint.powi(2) * interval)
            .min(maximum);
    }
}

/// [game] KnockBackMode contact classification uses body's FORWARD, not velocity.
pub fn wall_splat(
    normal: Vec3,
    forward: Vec3,
    finished: bool,
    character: bool,
    excluded: bool,
) -> bool {
    let normal = normal.normalize_or_zero();
    !finished
        && !character
        && !excluded
        && normal.length_squared() > 0.0
        && normal.z.abs() < 0.5
        && forward.dot(normal) >= -0.5
}

/// [game] StateDeath clocks, with single-player / keep-corpse exclusions supplied.
pub fn death_finished(
    age: f32,
    grounded: bool,
    finished: bool,
    multiplayer_player: bool,
    keep: bool,
) -> bool {
    !multiplayer_player && !keep && (age >= 10.0 || (age >= 3.0 && grounded && finished))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context() -> Context {
        Context {
            actor: Actor {
                id: 1,
                team: 1,
                player: true,
                ..Default::default()
            },
            health: 320.0,
            vehicle: false,
            shield: None,
            enabled: true,
            reject: false,
            invincible: false,
            controller_accepts: true,
            external_health: false,
            difficulty: Difficulty::default(),
            immune_to_overdrive: false,
            double_damage: false,
        }
    }
    fn bullet() -> Packet {
        Packet {
            source: Some(Actor {
                id: 2,
                team: 2,
                ..Default::default()
            }),
            amount: 50.0,
            ..Default::default()
        }
    }
    #[test]
    fn rejection_gates_and_source_bits() {
        let t = Tuning::default();
        let c = context();
        assert_eq!(resolve(bullet(), c, &t).flags & 7, 2);
        assert!(
            !resolve(
                bullet(),
                Context {
                    invincible: true,
                    ..c
                },
                &t
            )
            .accepted
        );
        assert!(
            !resolve(
                bullet(),
                Context {
                    controller_accepts: false,
                    ..c
                },
                &t
            )
            .accepted
        );
        let friend = Packet {
            source: Some(c.actor),
            ..bullet()
        };
        assert!(!resolve(friend, c, &t).accepted);
        assert!(
            resolve(
                Packet {
                    flags: flags::DISABLE_TRANSFORM,
                    ..friend
                },
                Context {
                    invincible: true,
                    ..c
                },
                &t
            )
            .accepted
        );
        let friend = Packet {
            source: Some(Actor { id: 3, ..c.actor }),
            ..bullet()
        };
        assert!(!resolve(friend, c, &t).accepted);
        assert!(
            resolve(
                Packet {
                    flags: flags::NO_TEAM_FILTER,
                    ..friend
                },
                c,
                &t
            )
            .accepted
        );
        assert!(
            !resolve(
                Packet {
                    flags: flags::FRIENDS_ONLY,
                    ..bullet()
                },
                c,
                &t
            )
            .accepted
        );
    }
    #[test]
    fn difficulty_is_reciprocal_and_vehicle_predamage_and_shield_are_separate() {
        let t = Tuning {
            vehicle_multiplier: 0.25,
            ..Tuning::default()
        };
        let c = Context {
            actor: Actor {
                player: false,
                ..context().actor
            },
            vehicle: true,
            difficulty: Difficulty {
                enemy_health: 2.0,
                enemy_damage: 2.0,
            },
            ..context()
        };
        let p = Packet {
            source: Some(Actor {
                player: true,
                ..bullet().source.unwrap()
            }),
            ..bullet()
        };
        assert_eq!(resolve(p, c, &t).amount, 6.25);
        assert_eq!(
            resolve(
                Packet {
                    flags: flags::PREDAMAGE,
                    ..p
                },
                c,
                &t
            )
            .amount,
            25.0
        );
        let c = Context {
            health: 20.0,
            shield: Some(0.5),
            ..context()
        };
        assert_eq!(resolve(bullet(), c, &t).health, 1.0);
        assert_eq!(
            resolve(bullet(), Context { shield: None, ..c }, &t).health,
            0.0
        );
    }
    #[test]
    fn throws_preserve_pitch_and_use_receiver_scale() {
        let p = Packet {
            direction: Vec3::Y,
            knockback_speed: 20.0,
            knockback_angle: 0.3,
            ..bullet()
        };
        assert!((p.velocity(1.25).length() - 25.0).abs() < 1e-5);
        let p = Packet {
            direction: Vec3::new(0.0, 0.5, 0.8660254),
            ..p
        };
        assert!((p.velocity(1.25).z - 21.650635).abs() < 1e-4);
    }
    #[test]
    fn stun_break_is_strict_and_disable_timers_replace() {
        let t = Tuning::default();
        let mut r = Runtime::default();
        let p = Packet {
            flags: flags::STUN | flags::DISABLE_VEHICLE,
            stun_time: 2.5,
            source: Some(Actor {
                character: crc32(b"Bumblebee"),
                ..Default::default()
            }),
            amount: 0.0,
            ..bullet()
        };
        r.receive(p, false, 1.5, &t);
        assert_eq!(r.stun_budget, 80.0);
        r.receive(
            Packet {
                amount: 80.0,
                ..bullet()
            },
            false,
            1.5,
            &t,
        );
        assert!(r.stunned > 0.0);
        r.receive(
            Packet {
                amount: 0.1,
                ..bullet()
            },
            false,
            1.5,
            &t,
        );
        assert_eq!(r.stunned, 0.0);
        r.receive(
            Packet {
                stun_time: 0.5,
                flags: flags::DISABLE_VEHICLE,
                ..bullet()
            },
            false,
            1.5,
            &t,
        );
        r.step(0.5);
        assert_eq!(r.disabled_vehicle, 0.0);
    }
    #[test]
    fn dot_delays_damage_and_ticks_once_per_second_including_final() {
        let p = Packet {
            flags: flags::DOT,
            dot_time: 2.5,
            ..bullet()
        };
        assert_eq!(resolve(p, context(), &Tuning::default()).health, 320.0);
        let mut r = Runtime::default();
        r.receive(p, false, 1.5, &Tuning::default());
        let mut damage = 0.0;
        for _ in 0..100 {
            if let Some(p) = r.step(0.032) {
                damage += p.amount;
                assert_eq!(p.flags, flags::NO_HIT_REACT);
            }
        }
        assert_eq!(damage, 150.0);
    }
    #[test]
    fn regeneration_uses_the_midpoint_cubic_curve_and_delay_reset() {
        let t = Tuning::default();
        let mut r = Runtime::default();
        r.stop_regen(1.5);
        let mut health = 100.0;
        r.regenerate(&mut health, 320.0, true, 1.5, &t, 1.0);
        assert_eq!(health, 100.0);
        r.regenerate(&mut health, 320.0, true, 1.5, &t, 0.5);
        assert!((health - (100.0 + 3.0 * 320.0 / 216.0 * 0.25f32.powi(2) * 0.5)).abs() < 1e-5);
        r.stop_regen(1.5);
        assert!(!r.regenerating && r.regen_left == 1.5);
        let mut health = 0.0;
        r.regenerate(&mut health, 320.0, true, 1.5, &t, 10.0);
        assert_eq!(health, 0.0);
    }
    #[test]
    fn wall_splat_has_orientation_and_surface_gates() {
        assert!(wall_splat(Vec3::Y, Vec3::Y, false, false, false));
        assert!(!wall_splat(Vec3::Y, -Vec3::Y, false, false, false));
        assert!(!wall_splat(Vec3::Z, Vec3::Y, false, false, false));
        assert!(!wall_splat(Vec3::Y, Vec3::Y, false, true, false));
    }
    #[test]
    fn death_completion_clocks_and_exclusions() {
        assert!(!death_finished(2.9, true, true, false, false));
        assert!(death_finished(3.0, true, true, false, false));
        assert!(!death_finished(3.0, false, true, false, false));
        assert!(death_finished(10.0, false, false, false, false));
        assert!(!death_finished(10.0, true, true, true, false));
    }
    #[test]
    fn overdrive_immunity_cheat_and_reduced_attacker_follow_receiver_order() {
        let t = Tuning::default();
        let c = context();
        let p = Packet {
            source: Some(Actor {
                overdrive: true,
                ..bullet().source.unwrap()
            }),
            ..bullet()
        };
        assert_eq!(resolve(p, c, &t).amount, 200.0);
        assert_eq!(
            resolve(
                p,
                Context {
                    immune_to_overdrive: true,
                    ..c
                },
                &t
            )
            .amount,
            50.0
        );
        assert_eq!(
            resolve(
                p,
                Context {
                    double_damage: true,
                    ..c
                },
                &t
            )
            .health,
            0.0
        );
        let p = Packet {
            source: Some(Actor {
                reduced_damage: true,
                ..p.source.unwrap()
            }),
            ..p
        };
        assert_eq!(
            resolve(
                p,
                Context {
                    health: 20.0,
                    shield: Some(0.5),
                    ..c
                },
                &t
            )
            .amount,
            1.9
        );
    }
}
