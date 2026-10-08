//! Melee: who an attack goes for, and what a hit carries. No engine types.
//!
//! Read from the original (`notes\melee.md`): the target search of the attack mode
//! (`FUN_0071efb0`, its scoring `FUN_007996c0` and choice `FUN_0079a000`), and the hit
//! (`FUN_00719540` -> `FUN_007189b0`). Tags as in `sim.rs`: [game], [data], [assumed],
//! [stand-in].
//!
//! In the original a hit is a physics contact: `AttackCollEvent` lets the collision body on
//! the node it names take `PUNCH` (his hands' spheres, his feet's boxes, the drum round
//! him), and a touch between such a body and one that sends `PUNCH` is the hit. His bodies
//! are here, posed by the clip (`attack_bodies`); a target is still one upright cylinder,
//! and the touch is found by a search of our own (`touch`).

use std::f32::consts::{PI, TAU};

use glam::{Affine3A, Vec2, Vec3};

use crate::formats::anim::{Action, Clip};
use crate::formats::hash::crc32;
use crate::formats::scene::{CollisionDef, ExplosionDef, ObjectDef, Shape};
use crate::pose::{self, Rig};
use crate::rumble::RumbleDef;

/// Something an attack can go for and hit. `pos` is where it stands (its feet).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Target {
    pub id: u32,
    pub pos: Vec3,
    pub radius: f32,
    pub height: f32,
    /// A character, as against an object that can be broken: the search puts objects
    /// behind characters, and only a character counts as "the attack has hit". [game]
    pub character: bool,
    /// A character's attribute `size`, 1 for the large ones (Optimus, Blackout...): it
    /// picks which of an attack's damage type sets a hit carries. [data]
    pub large: bool,
    /// The surface material of its body (`surface::*`), which picks the hit's sound and
    /// effect from the attacker's impact preset. [game]
    pub surface: u32,
}

/// Damage type flags, as the bits of the game's flag set (`DAMAGE_BY_MELEE`...). [data]
pub mod damage {
    pub const BY_MELEE: u32 = 1 << 8;
    pub const BY_EXPLOSION: u32 = 1 << 11;
    /// `DAMAGE_DOT`: `FUN_00719830` takes no health for a hit with it. [game]
    pub const DOT: u32 = 1 << 15;
    pub const BY_FAST_FLAIL: u32 = 1 << 16;
    pub const BY_STRONG_FLAIL: u32 = 1 << 17;
    pub const BY_KNOCKBACK: u32 = 1 << 18;
    pub const BY_PARTIAL_KNOCKBACK: u32 = 1 << 19;
    /// The four that make the target's damage routine throw it (`FUN_00719830` tests
    /// `0xf0000`). [game]
    pub const THROWS: u32 = BY_FAST_FLAIL | BY_STRONG_FLAIL | BY_KNOCKBACK | BY_PARTIAL_KNOCKBACK;
}

/// Surface materials, by the CRC-32 of their names. [data]
pub mod surface {
    /// `0 - Default`: the entry an impact preset falls back on (`FUN_0077cd60`).
    pub const DEFAULT: u32 = 0x37f7_18bf;
    pub const UPPER_BODY: u32 = 0x1f01_07fa;
    pub const WATER: u32 = 0x3af2_3bde;
}

/// The shared `Character` values the search weighs by, and the character's own reach. [data]
#[derive(Clone, Copy, Default, Debug)]
pub struct MeleeTuning {
    /// `autoTargetAngRange`, degrees: the search angle for a clip that gives none.
    pub ang_range: f32,
    /// `autoTargetBestDist`: the distance a target is best at.
    pub best_dist: f32,
    /// `autoTrgAngleWeight`, `autoTrgDistWeight`.
    pub angle_weight: f32,
    pub dist_weight: f32,
    /// `autoTrgMatchAtkWeight`: scales the score of a target whose kind matches the one
    /// asked for. The attack mode asks for none, so it never applies there. [game]
    pub match_weight: f32,
    /// `autoTargetMeleeDistance` and `autoTargetMeleeHeight` of the character: the range
    /// for a clip with no `maxRange`, and how far above or below a target may be.
    pub distance: f32,
    pub height: f32,
    /// `Upgrades.upgradeParams`: what each of the three melee upgrades (`UPGRADES`)
    /// multiplies the damage by at levels 1, 2 and 3.
    pub upgrades: [[f32; 3]; 3],
}

/// The upgrades that scale a human player's melee damage in single player, by the hash the
/// data keys them with (their names are not in the game's files): the first for the air
/// attack and the three hits of the chain, the second for the charged hit, the third for
/// the slam out of the car (`FUN_007189b0`). [game]
pub const UPGRADES: [u32; 3] = [0x62ff_fc0c, 0x0841_17aa, 0x49ab_1fe8];

impl MeleeTuning {
    /// What a hit of this clip is multiplied by with the upgrades at these levels
    /// (`FUN_0079d060`): the table's value for the level, 1 at level 0, for a level the
    /// table lacks and for any other clip. [game]
    pub fn upgrade_scale(&self, clip: u32, levels: [u32; 3]) -> f32 {
        const CHAIN: [u32; 4] = [crc32(b"AttackJump"), crc32(b"AttackFast_1"), crc32(b"AttackFast_2"), crc32(b"AttackFast_3")];
        let upgrade = if CHAIN.contains(&clip) {
            0
        } else if clip == crc32(b"AttackFast_3_ChargeAttk") {
            1
        } else if clip == crc32(b"Trans_Vehicle2RobotSlam") {
            2
        } else {
            return 1.0;
        };
        let scale = (levels[upgrade] as usize).checked_sub(1).and_then(|level| self.upgrades[upgrade].get(level)).copied();
        scale.filter(|&s| s > 0.0).unwrap_or(1.0)
    }
}

/// An angle brought into -pi..pi (`FUN_0056f290`).
pub fn wrap(angle: f32) -> f32 {
    let a = (angle + PI).rem_euclid(TAU) - PI;
    if a <= -PI { a + TAU } else { a }
}

fn yaw_of(direction: Vec2) -> f32 {
    (-direction.x).atan2(direction.y)
}

/// One search (`FUN_0071efb0` fills it in). Angles in radians.
#[derive(Clone, Copy, Debug)]
pub struct Query {
    pub from: Vec3,
    /// The direction looked along, as a yaw (0 along +y, positive toward -x) and a pitch.
    pub yaw: f32,
    pub pitch: f32,
    pub range: f32,
    pub height: f32,
    pub angle: f32,
}

impl Query {
    /// The attack mode's search: along `direction`, within the clip's `maxRange` (the
    /// character's `autoTargetMeleeDistance` if the clip has none) and its
    /// `auto_target_search_angle` (the shared `autoTargetAngRange` if negative). [game]
    pub fn new(from: Vec3, direction: Vec3, max_range: f32, angle: f32, m: &MeleeTuning) -> Self {
        let flat = Vec2::new(direction.x, direction.y);
        Self {
            from,
            yaw: yaw_of(flat),
            pitch: (-direction.z).atan2(flat.length()),
            range: if max_range > 0.0 { max_range } else { m.distance },
            height: m.height,
            angle: if angle < 0.0 { m.ang_range.to_radians() } else { angle },
        }
    }

    /// How good a target is, lower being better; `None` if it is out of range, too far
    /// above or below, or outside the angle (`FUN_007996c0`). [game]
    pub fn score(&self, target: &Target, m: &MeleeTuning) -> Option<f32> {
        self.score_with_angles(target, m, self.angle, m.angle_weight)
    }

    /// [game: 007996c0] Yaw and pitch have separate limits and weights. Weapon-entry
    /// targeting uses yaw weight2.5 and pitch weight1 (`00715c50`'s query).
    pub fn score_with_angles(&self, target: &Target, m: &MeleeTuning, elevation: f32, pitch_weight: f32) -> Option<f32> {
        let d = target.pos - self.from;
        let flat = Vec2::new(d.x, d.y);
        // Distance to its surface, not its middle.
        let distance = (flat.length() - target.radius).abs();
        if !(distance > 0.0 && distance < self.range && d.z.abs() < self.height) {
            return None;
        }
        let yaw = wrap(yaw_of(flat) - self.yaw).abs();
        let length = d.length();
        let unit = if length > 1e-8 { d / length } else { d };
        let pitch = wrap((-unit.z).atan2(Vec2::new(unit.x, unit.y).length()) - self.pitch).abs();
        if !(yaw < self.angle && pitch < elevation) {
            return None;
        }
        // Each part is a share of its own span. The spans' lower ends are zero for the
        // angles and the best distance for the distance.
        let span = |top: f32, low: f32| if top - low > low { top - low } else { low };
        let by_distance = (distance - m.best_dist).abs() / span(self.range, m.best_dist);
        let mut score = m.dist_weight * by_distance + m.angle_weight * yaw / span(self.angle, 0.0)
            + pitch_weight * pitch / span(elevation, 0.0);
        if !target.character {
            // Objects come after characters (`FUN_0079a000`). [game]
            score += m.dist_weight + m.angle_weight;
        }
        Some(score)
    }

    /// The target the search settles on: the lowest score, the first listed on a tie
    /// (`FUN_0079a000`). [game] [stand-in: every target listed can be targeted; the original
    /// also skips its own team, the dead and the untargetable]
    pub fn best(&self, targets: &[Target], m: &MeleeTuning) -> Option<u32> {
        let mut best: Option<(f32, u32)> = None;
        for target in targets {
            let Some(score) = self.score(target, m) else { continue };
            if best.is_none_or(|(least, _)| score < least) {
                best = Some((score, target.id));
            }
        }
        best.map(|(_, id)| id)
    }
}

/// What a hit hands to the one it lands on (the damage record `FUN_007189b0` builds).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub target: u32,
    /// The clip that hit, by the CRC-32 of its id.
    pub clip: u32,
    pub damage: f32,
    /// Where his body touched it.
    pub point: Vec3,
    /// The body of his that hit, by the CRC-32 of its node's name (`Hand_R`...).
    pub body: u32,
    /// The way the blow goes, flat and of length 1 (or zero).
    pub direction: Vec2,
    /// `specialCaseDamageStuff`: how fast and how steeply (radians above the ground) the
    /// target is thrown. What the target does with them (`FUN_00718480`) is not read.
    pub knock_back_speed: f32,
    pub knock_back_angle: f32,
    /// The damage type flags (`damage::*`).
    pub flags: u32,
    /// The surface material of the body that was hit.
    pub surface: u32,
    /// From an explosion he set off (the ground punch's blast) and not from a touch of his
    /// own body; `body` is then the node the explosion was put at.
    pub blast: bool,
    /// `specialCaseDamageStuff.stunTime`: seconds the target is stunned for; 0 for none.
    /// What a stunned target does (`stateStun`) belongs to the target's side, not read.
    pub stun_time: f32,
}

impl Hit {
    /// Whether the target's damage routine throws it (`FUN_00719830`): only for a hit
    /// whose flags have one of the flail or knock-back kinds. [game] Left out: the same
    /// for any hit on a target whose `+0x148` has bit 3 (not read what sets it), and the
    /// test of the throw's angle against the blow's own pitch.
    pub fn throws(&self) -> bool {
        self.flags & damage::THROWS != 0
    }
}

/// An explosion a clip sets off: an `AnimSendTriggerEvent` names an `ObjectSpawnerOnBone`
/// of his object, which puts its object, an `Explosion`, at its node. [game] [data]
#[derive(Clone, Debug)]
pub struct ClipBlast {
    pub ssd: Vec<crate::ssd::Info>,
    /// The spawner's `LuxScriptName`, which the event names, and its node's name hash.
    pub name: u32,
    pub node: u32,
    pub attach: bool,
    pub explosion: ExplosionDef,
    /// The pad rumble the spawned object starts with.
    pub rumble: Option<RumbleDef>,
    /// Where the node is at each tick of the clip, in his own frame.
    pub centres: Vec<Vec3>,
}

impl ClipBlast {
    /// The node's place at the whole tick nearest to `tick`.
    pub fn centre(&self, tick: f32) -> Vec3 {
        let last = self.centres.len().saturating_sub(1);
        self.centres.get((tick.max(0.0).round() as usize).min(last)).copied().unwrap_or(Vec3::ZERO)
    }
}

/// The explosions the clip's trigger events set off, with their node's path.
pub fn clip_blasts(rig: &Rig, object: &ObjectDef, clip: &Clip, looping: bool) -> Vec<ClipBlast> {
    let mut blasts: Vec<(usize, ClipBlast)> = Vec::new();
    for event in &clip.action_events {
        let Action::Trigger { name } = event.action else { continue };
        for spawner in object.spawners.iter().filter(|s| s.script_name == name) {
            if blasts.iter().any(|(_, b)| b.name == name && b.explosion == spawner.explosion) {
                continue;
            }
            let node = object.nodes[spawner.node].name_hash;
            blasts.push((spawner.node, ClipBlast { ssd:spawner.ssd.clone(), name, node, attach: spawner.attach, explosion: spawner.explosion, rumble: spawner.rumble, centres: Vec::new() }));
        }
    }
    if blasts.is_empty() {
        return Vec::new();
    }
    for tick in 0..=clip.length.round().max(1.0) as usize {
        let pose = rig.pose(clip, looping, tick as f32);
        for (index, blast) in &mut blasts {
            blast.centres.push(pose[*index].translation.into());
        }
    }
    blasts.into_iter().map(|(_, blast)| blast).collect()
}

/// The damage above which `useGroundPoundDamageOverride` picks the strong value
/// (`00b3476c`, read in `FUN_007e6af0`). [game]
const POUND_STRONG_OVER: f32 = 5.0;

impl ExplosionDef {
    /// The damage and the damage at the edge once the explosion knows its owner
    /// (`FUN_007e6af0`): with `useGroundPoundDamageOverride` and a character for an owner,
    /// both become the owner's `groundPoundDamage` if the data's damage is over 5, else its
    /// `groundPoundDamageWeak`. Both of Bumblebee's punch blasts (65 and 40) are over 5, so
    /// both do the strong value; the weak one is what his `genericsplat` (0) gets. [game]
    pub fn owned_damage(&self, pound: f32, pound_weak: f32) -> (f32, f32) {
        if !self.pound_override {
            return (self.damage, self.min_damage);
        }
        let damage = if self.damage > POUND_STRONG_OVER { pound } else { pound_weak };
        (damage, damage)
    }

    /// The sphere's share of its full size `age` seconds in, the update taking `dt`
    /// (`FUN_007e5f00`): from `start_scale` to 1 over `explosion_time`, and 1 at once when
    /// that time is not longer than an update. [game]
    pub fn scale(&self, age: f32, dt: f32) -> f32 {
        if self.time > dt && self.time > 1e-6 { ((1.0 - self.start_scale) / self.time * age + self.start_scale).min(1.0) } else { 1.0 }
    }

    /// The damage for a touch `distance` from the middle of a sphere now `radius` big
    /// (`FUN_007e60f0`): whole within `fall_off` of the radius, falling evenly to
    /// `min_damage` at the edge; whole everywhere when `fall_off` is outside 0..=1. [game]
    pub fn damage_at(&self, damage: f32, min_damage: f32, distance: f32, radius: f32) -> f32 {
        if !(0.0..=1.0).contains(&self.fall_off) {
            return damage;
        }
        let inner = self.fall_off * radius;
        if distance < inner {
            damage
        } else if distance < radius {
            (damage - min_damage) * (1.0 - (distance - inner) / (radius - inner)) + min_damage
        } else {
            min_damage
        }
    }
}

/// The way a blow goes, from the clip's `attack_dir` (`FUN_00718230`): 0 the attacker's
/// forward, 1 his left, 2 his right, 5 from him to the target; 3 and 4 are straight down and
/// up, which flattened is nothing, and anything past 6 counts as 6, nothing. [game]
pub fn attack_direction(attack_dir: i32, facing: Vec2, from: Vec3, target: Vec3) -> Vec2 {
    let right = Vec2::new(facing.y, -facing.x);
    let flat = match attack_dir {
        0 => facing,
        1 => -right,
        2 => right,
        5 => Vec2::new(target.x - from.x, target.y - from.y),
        _ => Vec2::ZERO,
    };
    flat.normalize_or_zero()
}

/// A collision body in place: its shape at its true size, and where it is (no scale).
#[derive(Clone, Copy, Debug)]
pub struct Body {
    pub shape: Shape,
    pub at: Affine3A,
}

impl Body {
    /// A shape under a transform that may scale it: the scale goes into the shape, as the
    /// original's loader keeps a primitive's scale apart from its squared-up axes. [game]
    /// [assumed: a sphere and a cylinder take the scale of their x]
    pub fn new(shape: Shape, at: Affine3A) -> Self {
        let (scale, rotation, position) = at.to_scale_rotation_translation();
        let scale = scale.abs();
        let shape = match shape {
            Shape::Sphere { radius } => Shape::Sphere { radius: radius * scale.x },
            Shape::Cuboid { half } => Shape::Cuboid { half: (Vec3::from(half) * scale).into() },
            Shape::Cylinder { radius, half_height } => Shape::Cylinder { radius: radius * scale.x, half_height: half_height * scale.z },
        };
        Self { shape, at: Affine3A::from_rotation_translation(rotation, position) }
    }

    /// A target as a body: an upright cylinder. [stand-in: the original's target is hit on
    /// whichever of its own bodies sends `PUNCH`, a character's being the boxes on its limbs]
    pub fn of_target(target: &Target) -> Self {
        let half_height = target.height * 0.5;
        Self {
            shape: Shape::Cylinder { radius: target.radius, half_height },
            at: Affine3A::from_translation(target.pos + Vec3::Z * half_height),
        }
    }

    /// The same body seen from outside the frame it was given in.
    pub fn moved(&self, by: &Affine3A) -> Self {
        Self { shape: self.shape, at: *by * self.at }
    }

    /// The point of the body nearest to `p` (which is `p` itself inside it).
    pub fn nearest(&self, p: Vec3) -> Vec3 {
        let local = self.at.inverse().transform_point3(p);
        let local = match self.shape {
            Shape::Sphere { radius } => local.clamp_length_max(radius),
            Shape::Cuboid { half } => local.clamp(-Vec3::from(half), Vec3::from(half)),
            Shape::Cylinder { radius, half_height } => {
                Vec2::new(local.x, local.y).clamp_length_max(radius).extend(local.z.clamp(-half_height, half_height))
            }
        };
        self.at.transform_point3(local)
    }
}

/// Two bodies no further apart than this touch.
const TOUCH: f32 = 0.01;

/// Where two bodies touch, if they do: a point of `b` that is also in `a`. Found by going
/// back and forth between the nearest points of the two, which for shapes without dents
/// closes in on the nearest pair. [stand-in for the original's contact search; the shapes
/// are its own]
pub fn touch(a: &Body, b: &Body) -> Option<Vec3> {
    let mut on_b = b.nearest(a.at.translation.into());
    for _ in 0..32 {
        let on_a = a.nearest(on_b);
        let next = b.nearest(on_a);
        if on_a.distance_squared(next) <= TOUCH * TOUCH {
            return Some(next);
        }
        if next.distance_squared(on_b) < 1e-8 {
            break;
        }
        on_b = next;
    }
    None
}

/// One of his collision bodies that an attack clip switches on, with where the clip puts it
/// at each of its ticks, in his own frame (x right, y forward, z up, feet at the origin).
#[derive(Clone, Debug)]
pub struct AttackBody {
    /// CRC-32 of the name of the node it rides: what the clip's `AttackCollEvent`s name.
    pub node: u32,
    /// On a foot: a foot's touch counts only against a character (`FUN_007189b0`). [game]
    pub foot: bool,
    pub frames: Vec<Body>,
}

impl AttackBody {
    /// The body at the whole tick nearest to `tick`.
    pub fn at(&self, tick: f32) -> Option<&Body> {
        let last = self.frames.len().checked_sub(1)?;
        self.frames.get((tick.max(0.0).round() as usize).min(last))
    }
}

const FEET: [u32; 2] = [crc32(b"Foot_L"), crc32(b"Foot_R")];

/// The bodies a clip's `AttackCollEvent`s name, posed tick by tick. The event sets the
/// `PUNCH` bit in what the body on its node takes (`FUN_00747e60` -> `FUN_006dcda0`), and a
/// touch between such a body and one that sends `PUNCH` is the hit (`FUN_00719540`). The
/// events also name the node the axe hangs from; it has no body, so nothing comes of that.
/// [game] [assumed: the pose is the clip's alone, without the blend out of the clip before]
pub fn attack_bodies(rig: &Rig, object: &ObjectDef, clip: &Clip, looping: bool) -> Vec<AttackBody> {
    let mut named: Vec<u32> = Vec::new();
    for event in &clip.action_events {
        if let Action::Hit { node, .. } = event.action {
            if !named.contains(&node) {
                named.push(node);
            }
        }
    }
    let mut bodies: Vec<(usize, &CollisionDef, AttackBody)> = Vec::new();
    for node in named {
        let Some(index) = object.nodes.iter().position(|n| n.name_hash == node) else { continue };
        for collision in object.collisions.iter().filter(|c| c.node == index) {
            bodies.push((index, collision, AttackBody { node, foot: FEET.contains(&node), frames: Vec::new() }));
        }
    }
    if bodies.is_empty() {
        return Vec::new();
    }
    for tick in 0..=clip.length.round().max(1.0) as usize {
        let pose = rig.pose(clip, looping, tick as f32);
        for (index, collision, body) in &mut bodies {
            body.frames.push(Body::new(collision.shape, pose[*index] * pose::affine(&collision.matrix)));
        }
    }
    bodies.into_iter().map(|(_, _, body)| body).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tuning() -> MeleeTuning {
        // The shared values of the install, and Bumblebee's own two.
        MeleeTuning {
            ang_range: 90.0,
            best_dist: 1.5,
            angle_weight: 1.0,
            dist_weight: 0.5,
            match_weight: 0.75,
            distance: 25.0,
            height: 5.0,
            upgrades: [[1.25, 1.5, 2.0], [1.25, 1.5, 2.5], [1.25, 1.5, 2.0]],
        }
    }

    fn at(id: u32, x: f32, y: f32) -> Target {
        Target { id, pos: Vec3::new(x, y, 0.0), radius: 2.0, height: 5.5, character: true, large: false, surface: surface::UPPER_BODY }
    }

    fn query() -> Query {
        Query::new(Vec3::ZERO, Vec3::Y, 20.0, 90f32.to_radians(), &tuning())
    }

    #[test]
    fn a_target_straight_ahead_at_the_best_distance_scores_nothing() {
        let m = tuning();
        // Surface 1.5 away: middle at 3.5 with a radius of 2.
        let score = query().score(&at(1, 0.0, 3.5), &m).unwrap();
        assert!(score.abs() < 1e-5, "{score}");
    }

    #[test]
    fn the_score_is_the_weighted_shares_of_distance_and_angle() {
        let m = tuning();
        // 45 degrees to the right, surface 10 away: distance share (10 - 1.5) / 18.5,
        // angle share 45 / 90.
        let d = 12.0 * std::f32::consts::FRAC_1_SQRT_2;
        let score = query().score(&at(1, d, d), &m).unwrap();
        let expected = 0.5 * (8.5 / 18.5) + 1.0 * 0.5;
        assert!((score - expected).abs() < 1e-4, "{score} against {expected}");
    }

    #[test]
    fn range_height_and_angle_rule_targets_out() {
        let m = tuning();
        let q = query();
        assert!(q.score(&at(1, 0.0, 22.5), &m).is_none(), "surface 20.5 away, range 20");
        assert!(q.score(&at(1, 0.0, -6.0), &m).is_none(), "behind him");
        let mut high = at(1, 0.0, 6.0);
        high.pos.z = 5.0;
        assert!(q.score(&high, &m).is_none(), "5 above, the limit is under 5");
        high.pos.z = 3.0;
        assert!(q.score(&high, &m).is_some());
    }

    #[test]
    fn a_hit_carries_the_flags_for_the_targets_size_and_throws_by_them() {
        use crate::formats::anim::AttackRef;
        let attack = AttackRef {
            include_damage: damage::BY_MELEE,
            include_damage_small: damage::BY_MELEE | damage::BY_STRONG_FLAIL,
            include_damage_large: damage::BY_MELEE | damage::BY_FAST_FLAIL,
            exclude_damage: damage::BY_FAST_FLAIL,
            ..Default::default()
        };
        assert_eq!(attack.flags_for(None), 0x100);
        assert_eq!(attack.flags_for(Some(0)), 0x20100);
        assert_eq!(attack.flags_for(Some(1)), 0x100, "less what the clip excludes");
        assert_eq!(attack.flags_for(Some(2)), 0x100);
        let hit = |flags| Hit {
            target: 1,
            clip: 0,
            damage: 30.0,
            point: Vec3::ZERO,
            body: 0,
            direction: Vec2::Y,
            knock_back_speed: 24.0,
            knock_back_angle: 0.2,
            flags,
            surface: surface::UPPER_BODY,
            blast: false,
            stun_time: 0.0,
        };
        assert!(!hit(damage::BY_MELEE).throws(), "a throw speed alone throws nothing");
        assert!(hit(damage::BY_MELEE | damage::BY_KNOCKBACK).throws());
        assert!(hit(damage::BY_PARTIAL_KNOCKBACK).throws());
        assert!(!hit(damage::BY_EXPLOSION | damage::DOT).throws());
    }

    #[test]
    fn the_nearer_to_the_line_wins_and_objects_come_after_characters() {
        let m = tuning();
        let q = query();
        let ahead = at(1, 0.0, 12.0);
        let aside = at(2, 6.0, 6.0);
        assert_eq!(q.best(&[aside, ahead], &m), Some(1));
        let mut crate_ahead = ahead;
        crate_ahead.character = false;
        assert_eq!(q.best(&[crate_ahead, aside], &m), Some(2));
        assert_eq!(q.best(&[], &m), None);
    }

    #[test]
    fn a_clip_without_a_range_or_an_angle_uses_the_shared_ones() {
        let m = tuning();
        let q = Query::new(Vec3::ZERO, Vec3::Y, 0.0, -1.0, &m);
        assert_eq!(q.range, 25.0);
        assert!((q.angle - 90f32.to_radians()).abs() < 1e-6);
    }

    #[test]
    fn attack_directions_follow_the_table() {
        let facing = Vec2::Y;
        let (from, target) = (Vec3::ZERO, Vec3::new(3.0, 4.0, 2.0));
        assert_eq!(attack_direction(0, facing, from, target), Vec2::Y);
        assert_eq!(attack_direction(1, facing, from, target), Vec2::NEG_X);
        assert_eq!(attack_direction(2, facing, from, target), Vec2::X);
        assert_eq!(attack_direction(3, facing, from, target), Vec2::ZERO);
        assert!((attack_direction(5, facing, from, target) - Vec2::new(0.6, 0.8)).length() < 1e-6);
    }

    #[test]
    fn a_hand_touches_a_standing_target_only_when_it_reaches_it() {
        // The target's near side is at y 4 and its top at 5.5. The hand's sphere is stored
        // at 0.93 under a scale of 2: 1.86 across.
        let target = Body::of_target(&at(1, 0.0, 6.0));
        let hand = |y: f32, z: f32| {
            let at = Affine3A::from_scale_rotation_translation(Vec3::splat(2.0), glam::Quat::IDENTITY, Vec3::new(0.0, y, z));
            Body::new(Shape::Sphere { radius: 0.93 }, at)
        };
        assert!(touch(&hand(2.0, 3.0), &target).is_none(), "reaches to 3.86");
        let point = touch(&hand(2.5, 3.0), &target).expect("reaches to 4.36");
        assert!((point - Vec3::new(0.0, 4.0, 3.0)).length() < 0.05, "{point:?}");
        assert!(touch(&hand(4.0, 7.5), &target).is_none(), "over its head");
        assert!(touch(&hand(4.0, 7.0), &target).is_some());
    }

    #[test]
    fn a_turned_box_touches_by_its_corner() {
        // Half a turn of a quarter about the vertical: its corner is 0.85 further along y.
        let target = Body::of_target(&at(1, 0.0, 6.0));
        let foot = |y: f32| {
            let turn = glam::Quat::from_rotation_z(std::f32::consts::FRAC_PI_4);
            Body::new(Shape::Cuboid { half: [1.0, 0.2, 0.2] }, Affine3A::from_rotation_translation(turn, Vec3::new(0.0, y, 1.0)))
        };
        assert!(touch(&foot(3.0), &target).is_none());
        assert!(touch(&foot(3.5), &target).is_some());
    }

    #[test]
    fn upgrades_scale_the_clips_they_belong_to() {
        let m = tuning();
        let hit = |id: &str, levels| m.upgrade_scale(crc32(id.as_bytes()), levels);
        assert_eq!(hit("AttackFast_2", [0, 3, 3]), 1.0);
        assert_eq!(hit("AttackFast_2", [2, 0, 0]), 1.5);
        assert_eq!(hit("AttackFast_3_ChargeAttk", [1, 3, 1]), 2.5);
        assert_eq!(hit("Trans_Vehicle2RobotSlam", [0, 0, 1]), 1.25);
        assert_eq!(hit("Trans_Vehicle2Robot2GroundPunch", [3, 3, 3]), 1.0);
        assert_eq!(hit("AttackJump", [4, 0, 0]), 1.0, "a level the table lacks");
    }

    fn explosion() -> ExplosionDef {
        ExplosionDef {
            radius: 4.18,
            start_scale: 0.5,
            time: 0.15,
            delay: 0.0,
            damage: 10.0,
            min_damage: 1.0,
            fall_off: 0.75,
            pound_override: false,
            damage_player: false,
            can_damage_self: false,
            damage_flags: 0,
            knock_back_speed: 25.0,
            knock_back_angle: 75.0,
            camera_shake: 0,
            stun_time: 0.0,
        }
    }

    #[test]
    fn an_explosion_grows_from_its_start_scale_to_full_size_over_its_time() {
        let e = explosion();
        assert!((e.scale(0.032, 0.032) - (0.5 + 0.5 * 0.032 / 0.15)).abs() < 1e-6);
        assert_eq!(e.scale(0.15, 0.032), 1.0);
        assert_eq!(e.scale(0.4, 0.032), 1.0);
        // An explosion no longer than an update is full size at once.
        assert_eq!(ExplosionDef { time: 0.02, ..e }.scale(0.01, 0.032), 1.0);
    }

    #[test]
    fn an_explosions_damage_falls_off_past_the_inner_share_of_its_radius() {
        let e = explosion();
        // Whole within 0.75 of the radius, the least at the edge, even in between.
        assert_eq!(e.damage_at(10.0, 1.0, 2.0, 4.0), 10.0);
        assert!((e.damage_at(10.0, 1.0, 3.5, 4.0) - 5.5).abs() < 1e-5);
        assert_eq!(e.damage_at(10.0, 1.0, 4.5, 4.0), 1.0);
        // A share outside 0..=1 (the ground punch's -1): whole everywhere.
        assert_eq!(ExplosionDef { fall_off: -1.0, ..e }.damage_at(10.0, 1.0, 3.9, 4.0), 10.0);
    }

    #[test]
    fn the_ground_pound_override_picks_the_owners_damage_by_the_datas() {
        let e = explosion();
        assert_eq!(e.owned_damage(100.0, 60.0), (10.0, 1.0));
        let pound = ExplosionDef { pound_override: true, ..e };
        assert_eq!(ExplosionDef { damage: 65.0, ..pound }.owned_damage(100.0, 60.0), (100.0, 100.0));
        assert_eq!(ExplosionDef { damage: 40.0, ..pound }.owned_damage(100.0, 60.0), (100.0, 100.0));
        assert_eq!(ExplosionDef { damage: 0.0, ..pound }.owned_damage(100.0, 60.0), (60.0, 60.0));
    }

    #[test]
    fn wrap_keeps_angles_within_half_a_turn() {
        assert!((wrap(3.0 * PI) - PI).abs() < 1e-5);
        assert!((wrap(-PI / 2.0) + PI / 2.0).abs() < 1e-6);
        assert!((wrap(TAU + 0.25) - 0.25).abs() < 1e-5);
    }
}
