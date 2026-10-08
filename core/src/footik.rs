//! Foot support applied after animation layers, in game space (z up).
//! Original constants/solver evidence: notes/foot-ik.md. Character binding,
//! sole proxy, activation and plane adaptation are [assumed], documented there.
use glam::{Affine3A, Quat, Vec3};

use crate::camera::Sight;
use crate::formats::{
    hash::{crc32, lower33},
    model::Library,
    scene::ObjectDef,
};
use crate::pose::affine;

/// [game: 0073db50] Probe extent, contact bias, reach margin and angular rate.
const PROBE: f32 = 1.5;
const BIAS: f32 = 0.03;
const REACH_MARGIN: f32 = 0.001;
const TURN_RATE: f32 = 24.0;
const UPDATE: f32 = 0.032; // [game] Simulation cadence.

#[derive(Clone, Debug)]
pub struct Leg {
    pub hip: usize,
    pub knee: usize,
    pub foot: usize,
    /// [assumed] Rest-space rectangular sole proxy, expressed in ankle space.
    pub sole: [Vec3; 4],
}

#[derive(Clone, Debug)]
pub struct FootRig {
    pub legs: [Leg; 2],
    pub body: usize,
}

impl FootRig {
    /// [data] Hierarchy/rigid mesh coordinates. [assumed] Named ancestor chains,
    /// rectangular sole and Main body compensation: Bumblebee has no FootIK links.
    pub fn read(object: &ObjectDef, library: &Library) -> Option<Self> {
        let node = |name: &str| {
            object
                .nodes
                .iter()
                .position(|n| n.name_hash == crc32(name.as_bytes()))
        };
        let leg = |side: &str| -> Option<Leg> {
            let foot = node(&format!("Foot_{side}"))?;
            let knee = object.nodes[foot].parent?;
            let hip = object.nodes[knee].parent?;
            let toe = node(&format!("Midfoot_{side}"))?;
            let mut min = Vec3::splat(f32::INFINITY);
            let mut max = Vec3::splat(f32::NEG_INFINITY);
            for render in object
                .renders
                .iter()
                .filter(|r| r.node == foot || r.node == toe)
            {
                let mesh = library.meshes.get(&lower33(&render.model))?;
                // Skinned meshes are in model space and need a different bounds path.
                if mesh.skeleton.is_some() {
                    continue;
                }
                let bind = affine(&object.nodes[render.node].bind);
                for p in mesh.parts.iter().flat_map(|p| &p.positions) {
                    let p = bind.transform_point3(Vec3::from(*p));
                    min = min.min(p);
                    max = max.max(p);
                }
            }
            if !min.is_finite() || !max.is_finite() {
                return None;
            }
            let inverse = affine(&object.nodes[foot].bind).inverse();
            let sole = [
                Vec3::new(min.x, min.y, min.z),
                Vec3::new(max.x, min.y, min.z),
                Vec3::new(min.x, max.y, min.z),
                Vec3::new(max.x, max.y, min.z),
            ]
            .map(|p| inverse.transform_point3(p));
            Some(Leg {
                hip,
                knee,
                foot,
                sole,
            })
        };
        Some(Self {
            legs: [leg("L")?, leg("R")?],
            body: node("Main")?,
        })
    }
}

/// Parent accumulation shared by the pose pass and engine-independent tests.
pub fn accumulate(local: &[Affine3A], parents: &[Option<usize>]) -> Vec<Affine3A> {
    let mut out = Vec::with_capacity(local.len());
    for (i, &at) in local.iter().enumerate() {
        out.push(parents[i].map_or(at, |p| out[p] * at));
    }
    out
}

/// Solve a two-segment chain without stretching; retain the animated bend plane.
/// [game] Cosine-law geometry and sum-minus-.001 cap (0073db50).
/// [assumed] Bend plane follows the incoming animation; singular fallback is geometric.
pub fn solve_knee(hip: Vec3, knee: Vec3, ankle: Vec3, target: Vec3) -> Option<(Vec3, Vec3)> {
    let a = hip.distance(knee);
    let b = knee.distance(ankle);
    if a <= REACH_MARGIN || b <= REACH_MARGIN || !target.is_finite() {
        return None;
    }
    let direction = (target - hip)
        .try_normalize()
        .or_else(|| (ankle - hip).try_normalize())?;
    let distance = hip
        .distance(target)
        .clamp((a - b).abs() + REACH_MARGIN, a + b - REACH_MARGIN);
    let along = (a * a - b * b + distance * distance) / (2.0 * distance);
    let height = (a * a - along * along).max(0.0).sqrt();
    let bend = knee - hip - direction * (knee - hip).dot(direction);
    let bend = bend.try_normalize().unwrap_or_else(|| {
        let axis = if direction.y.abs() < 0.9 {
            Vec3::Y
        } else {
            Vec3::X
        };
        (axis - direction * axis.dot(direction)).normalize()
    });
    Some((
        hip + direction * along + bend * height,
        hip + direction * distance,
    ))
}

/// Rotate a joint in object space, converting back to its parent's local frame.
fn rotate_joint(local: &mut [Affine3A], parents: &[Option<usize>], joint: usize, turn: Quat) {
    let world = accumulate(local, parents);
    let (scale, rotation, position) = world[joint].to_scale_rotation_translation();
    let at =
        Affine3A::from_scale_rotation_translation(scale, (turn * rotation).normalize(), position);
    local[joint] = parents[joint].map_or(at, |p| world[p].inverse() * at);
}

fn apply_leg(
    local: &mut [Affine3A],
    parents: &[Option<usize>],
    leg: &Leg,
    target: Vec3,
    rotation: Quat,
) {
    let world = accumulate(local, parents);
    let h = world[leg.hip].translation.into();
    let k = world[leg.knee].translation.into();
    let f = world[leg.foot].translation.into();
    let Some((new_knee, end)) = solve_knee(h, k, f, target) else {
        return;
    };
    rotate_joint(
        local,
        parents,
        leg.hip,
        Quat::from_rotation_arc((k - h).normalize(), (new_knee - h).normalize()),
    );
    let world = accumulate(local, parents);
    let k: Vec3 = world[leg.knee].translation.into();
    let f: Vec3 = world[leg.foot].translation.into();
    rotate_joint(
        local,
        parents,
        leg.knee,
        Quat::from_rotation_arc((f - k).normalize(), (end - k).normalize()),
    );
    let world = accumulate(local, parents);
    let (scale, _, position) = world[leg.foot].to_scale_rotation_translation();
    let at = Affine3A::from_scale_rotation_translation(scale, rotation, position);
    local[leg.foot] = parents[leg.foot].map_or(at, |p| world[p].inverse() * at);
}

#[derive(Default)]
pub struct FootIk {
    offsets: [f32; 2],
    turns: [Quat; 2],
    remainder: f32,
    previous: Option<Vec3>,
}

impl FootIk {
    pub fn reset(&mut self) {
        self.offsets = [0.0; 2];
        self.turns = [Quat::IDENTITY; 2];
        self.remainder = 0.0;
        self.previous = None;
    }

    /// [assumed] Preserve animated lift and adapt it to local terrain; whole-body
    /// visual lowering prevents extending the downhill leg. Never changes movement.
    /// `root` maps the object's z-up space to the world's z-up space.
    pub fn apply(
        &mut self,
        rig: &FootRig,
        local: &mut [Affine3A],
        parents: &[Option<usize>],
        root: Affine3A,
        active: bool,
        dt: f32,
        ground: &impl Sight,
    ) {
        if !active || !dt.is_finite() || dt <= 0.0 {
            self.reset();
            return;
        }
        let origin: Vec3 = root.translation.into();
        // [assumed] A move larger than the probe diameter in one frame is a teleport.
        if self
            .previous
            .is_some_and(|p| p.distance(origin) > PROBE * 2.0)
        {
            self.reset();
        }
        self.previous = Some(origin);
        let inverse = root.inverse();
        let world = accumulate(local, parents);
        let mut desired = [0.0; 2];
        let mut turns = [Quat::IDENTITY; 2];
        let mut supported = [false; 2];
        for (i, leg) in rig.legs.iter().enumerate() {
            let ankle = root * world[leg.foot];
            let (_, rotation, p) = ankle.to_scale_rotation_translation();
            let centre = leg.sole.iter().copied().sum::<Vec3>() / 4.0;
            // [game] Vertical probe +/-1.5. [assumed] Centre and corners of the
            // mesh-derived sole instead of the original linked endpoint. Sampling
            // corners prevents a toe sinking into a higher surface at a join.
            let contacts: Vec<_> = std::iter::once(centre)
                .chain(leg.sole)
                .filter_map(|s| {
                    let probe = ankle.transform_point3(s);
                    ground.sight(probe + Vec3::Z * PROBE, probe - Vec3::Z * PROBE)
                })
                .filter(|h| h.normal.z > 1e-6)
                .collect();
            let before = leg
                .sole
                .iter()
                .map(|s| (rotation * *s).z)
                .fold(f32::INFINITY, f32::min);
            let correction = |hit: &crate::camera::Hit, rotated: Quat| {
                let after = leg
                    .sole
                    .iter()
                    .map(|s| hit.normal.dot(rotated * *s) / hit.normal.z)
                    .fold(f32::INFINITY, f32::min);
                let height = hit.point.z
                    - hit.normal.truncate().dot((p - hit.point).truncate()) / hit.normal.z;
                height - origin.z + before - after + BIAS
            };
            let candidate = |hit: &crate::camera::Hit| {
                correction(
                    hit,
                    Quat::from_rotation_arc(Vec3::Z, hit.normal.normalize()) * rotation,
                )
            };
            let Some(hit) = contacts
                .iter()
                .max_by(|a, b| candidate(a).total_cmp(&candidate(b)))
            else {
                continue;
            };
            let turn = Quat::from_rotation_arc(Vec3::Z, hit.normal.normalize());
            // [assumed] Respect every sampled plane after selecting the foot turn.
            desired[i] = contacts
                .iter()
                .map(|h| correction(h, turn * rotation))
                .fold(f32::NEG_INFINITY, f32::max);
            turns[i] = inverse.to_scale_rotation_translation().1
                * turn
                * root.to_scale_rotation_translation().1;
            supported[i] = true;
        }
        // [assumed] Apply the game's half-per-update filter at the simulation cadence,
        // independent of display refresh. Angular dt*24 is capped to avoid overshoot.
        self.remainder += dt;
        while self.remainder >= UPDATE {
            self.remainder -= UPDATE;
            for i in 0..2 {
                self.offsets[i] += (desired[i] - self.offsets[i]) * 0.5;
            }
        }
        for (i, turn) in turns.iter().enumerate() {
            self.turns[i] = self.turns[i].slerp(*turn, (dt * TURN_RATE).min(1.0));
            if !supported[i] {
                // [assumed] A lost probe releases its foot immediately.
                self.offsets[i] = 0.0;
                self.turns[i] = Quat::IDENTITY;
            }
        }
        if !supported.iter().any(|&s| s) {
            self.reset();
            return;
        }
        let lower = self.offsets.iter().copied().fold(0.0_f32, f32::min);
        let body_parent = parents[rig.body].map_or(Affine3A::IDENTITY, |p| world[p]);
        local[rig.body].translation +=
            glam::Vec3A::from(body_parent.inverse().transform_vector3(Vec3::Z * lower));
        for (i, leg) in rig.legs.iter().enumerate() {
            if !supported[i] {
                continue;
            }
            let (_, rotation, position) = world[leg.foot].to_scale_rotation_translation();
            apply_leg(
                local,
                parents,
                leg,
                position + Vec3::Z * self.offsets[i],
                self.turns[i] * rotation,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        character,
        pose::Rig,
        sim::{Arena, Box3},
        terrain::Ramp,
    };
    use glam::Vec2;
    use std::{path::Path, sync::OnceLock};

    fn data() -> &'static character::CharacterData {
        static DATA: OnceLock<character::CharacterData> = OnceLock::new();
        DATA.get_or_init(|| character::load(Path::new(r"C:\Games2"), "bumblebee").unwrap())
    }
    fn fixture() -> (FootRig, Vec<Affine3A>, Vec<Option<usize>>) {
        let data = data();
        let rig =
            FootRig::read(&data.robot, &data.library).expect("both rigid foot meshes and chains");
        let parents: Vec<_> = data.robot.nodes.iter().map(|n| n.parent).collect();
        let rest = Rig::new(&data.robot).rest();
        let local = rest
            .iter()
            .enumerate()
            .map(|(i, &w)| parents[i].map_or(w, |p| rest[p].inverse() * w))
            .collect();
        (rig, local, parents)
    }
    fn settled(
        rig: &FootRig,
        base: &[Affine3A],
        parents: &[Option<usize>],
        root: Affine3A,
        arena: &Arena,
        dt: f32,
    ) -> Vec<Affine3A> {
        let mut ik = FootIk::default();
        let mut pose = base.to_vec();
        for _ in 0..(1.6 / dt) as usize {
            pose.clone_from_slice(base);
            ik.apply(rig, &mut pose, parents, root, true, dt, arena);
        }
        pose
    }
    fn assert_lengths(
        rig: &FootRig,
        base: &[Affine3A],
        actual: &[Affine3A],
        parents: &[Option<usize>],
    ) {
        let before = accumulate(base, parents);
        let after = accumulate(actual, parents);
        for leg in &rig.legs {
            for (a, b) in [(leg.hip, leg.knee), (leg.knee, leg.foot)] {
                let len =
                    |w: &[Affine3A]| Vec3::from(w[a].translation).distance(w[b].translation.into());
                assert!((len(&before) - len(&after)).abs() < 1e-4);
            }
        }
        assert!(after.iter().all(|p| p.is_finite()));
    }
    #[test]
    fn cosine_solver_preserves_lengths_and_handles_unreachable_and_folded_targets() {
        let (h, k, f) = (
            Vec3::ZERO,
            Vec3::new(0.0, 1.0, -1.0),
            Vec3::new(0.0, 0.0, -2.0),
        );
        for target in [Vec3::new(0.0, 0.0, -1.0), Vec3::new(0.0, 0.0, -20.0), h] {
            let (k2, f2) = solve_knee(h, k, f, target).unwrap();
            assert!((h.distance(k2) - h.distance(k)).abs() < 1e-4);
            assert!((k2.distance(f2) - k.distance(f)).abs() < 1e-4);
            assert!(k2.is_finite() && f2.is_finite());
        }
        assert!(solve_knee(h, h, f, f).is_none());
    }
    #[test]
    fn real_feet_clear_slopes_and_banks_at_different_character_yaws() {
        let (rig, base, parents) = fixture();
        for slope in [
            Vec2::new(0.0, 0.28),
            Vec2::new(0.35, 0.0),
            Vec2::new(-0.2, 0.2),
        ] {
            let ramp = Ramp {
                min: Vec2::splat(-10.0),
                max: Vec2::splat(10.0),
                base: 0.0,
                height: 8.0,
                slope,
            };
            let arena = Arena {
                ramps: vec![ramp],
                ..Default::default()
            };
            for yaw in [0.0, 0.8, 2.4] {
                let root = Affine3A::from_rotation_translation(
                    Quat::from_rotation_z(yaw),
                    Vec3::new(0.0, 0.0, ramp.top(Vec2::ZERO)),
                );
                let actual = settled(&rig, &base, &parents, root, &arena, 0.016);
                let world = accumulate(&actual, &parents);
                for leg in &rig.legs {
                    let lowest = leg
                        .sole
                        .iter()
                        .map(|p| (root * world[leg.foot]).transform_point3(*p))
                        .map(|p| p.z - ramp.top(p.truncate()))
                        .fold(f32::INFINITY, f32::min);
                    assert!(
                        (lowest - BIAS).abs() < 0.06,
                        "slope={slope:?}, yaw={yaw}, clearance={lowest}"
                    );
                }
                assert_lengths(&rig, &base, &actual, &parents);
            }
        }
    }
    #[test]
    fn uneven_support_solves_each_leg_without_stretching() {
        let (rig, base, parents) = fixture();
        let arena = Arena {
            boxes: vec![Box3 {
                min: Vec3::new(0.0, -5.0, 0.0),
                max: Vec3::new(5.0, 5.0, 0.4),
            }],
            ..Default::default()
        };
        let actual = settled(&rig, &base, &parents, Affine3A::IDENTITY, &arena, 0.016);
        let world = accumulate(&actual, &parents);
        for (i, leg) in rig.legs.iter().enumerate() {
            let z = leg
                .sole
                .iter()
                .map(|p| world[leg.foot].transform_point3(*p).z)
                .fold(f32::INFINITY, f32::min);
            let expected = if i == 0 { 0.0 } else { 0.4 };
            assert!((z - expected - BIAS).abs() < 0.04, "foot={i}, z={z}");
        }
        assert_lengths(&rig, &base, &actual, &parents);
    }
    #[test]
    fn inactive_and_missing_support_leave_animation_unchanged() {
        let (rig, base, parents) = fixture();
        let mut ik = FootIk::default();
        for active in [true, false] {
            let mut pose = base.clone();
            ik.apply(
                &rig,
                &mut pose,
                &parents,
                Affine3A::IDENTITY,
                active,
                0.032,
                &crate::camera::Open,
            );
            assert_eq!(pose, base);
        }
        let mut pose = base.clone();
        ik.apply(
            &rig,
            &mut pose,
            &parents,
            Affine3A::IDENTITY,
            true,
            0.032,
            &Arena::default(),
        );
        pose.clone_from_slice(&base);
        ik.apply(
            &rig,
            &mut pose,
            &parents,
            Affine3A::IDENTITY,
            false,
            0.032,
            &Arena::default(),
        );
        assert_eq!(pose, base);
    }
    #[test]
    fn flat_support_preserves_pose_except_contact_bias_and_render_cadence_agrees() {
        let (rig, base, parents) = fixture();
        let slow = settled(
            &rig,
            &base,
            &parents,
            Affine3A::IDENTITY,
            &Arena::default(),
            0.032,
        );
        let fast = settled(
            &rig,
            &base,
            &parents,
            Affine3A::IDENTITY,
            &Arena::default(),
            0.008,
        );
        let before = accumulate(&base, &parents);
        let a = accumulate(&slow, &parents);
        let b = accumulate(&fast, &parents);
        for leg in &rig.legs {
            assert!(
                Vec3::from(a[leg.foot].translation).distance(b[leg.foot].translation.into()) < 1e-4
            );
            assert!(
                (a[leg.foot].translation.z - before[leg.foot].translation.z - BIAS).abs() < 1e-4
            );
        }
    }
    #[test]
    fn raised_toe_corner_at_a_ramp_join_is_not_missed_by_the_centre_probe() {
        let (rig, base, parents) = fixture();
        let rest = accumulate(&base, &parents);
        let points = rig.legs[0]
            .sole
            .map(|p| rest[rig.legs[0].foot].transform_point3(p));
        let low = points.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
        let high = points.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max);
        let seam = low + (high - low) * 0.75;
        let ramp = Ramp {
            min: Vec2::new(-5.0, seam),
            max: Vec2::new(5.0, 10.0),
            base: 0.0,
            height: 0.0,
            slope: Vec2::new(0.0, 0.28),
        };
        let arena = Arena {
            ramps: vec![ramp],
            ..Default::default()
        };
        let actual = settled(&rig, &base, &parents, Affine3A::IDENTITY, &arena, 0.016);
        let world = accumulate(&actual, &parents);
        for leg in &rig.legs {
            for p in leg.sole {
                let p = world[leg.foot].transform_point3(p);
                let height = if ramp.contains(p.truncate()) {
                    ramp.top(p.truncate())
                } else {
                    0.0
                };
                assert!(
                    p.z - height >= -0.001,
                    "sole corner penetrates join: {p:?}, surface={height}"
                );
            }
        }
        assert_lengths(&rig, &base, &actual, &parents);
    }
    #[test]
    fn walking_retains_animated_swing_height_and_toe_keys() {
        let (rig, _, parents) = fixture();
        let data = data();
        let clip = data
            .animations
            .find("WalkSet", "Walk")
            .unwrap()
            .clip
            .as_ref()
            .unwrap();
        let pose_rig = Rig::new(&data.robot);
        let toe = data
            .robot
            .nodes
            .iter()
            .position(|n| n.name == "Midfoot_L")
            .unwrap();
        for tick in [0.0, 10.0, 25.0, 40.0, 55.0] {
            let world = pose_rig.pose(clip, true, tick);
            let base: Vec<_> = world
                .iter()
                .enumerate()
                .map(|(i, &w)| parents[i].map_or(w, |p| world[p].inverse() * w))
                .collect();
            let actual = settled(
                &rig,
                &base,
                &parents,
                Affine3A::IDENTITY,
                &Arena::default(),
                0.016,
            );
            let result = accumulate(&actual, &parents);
            for leg in &rig.legs {
                assert!(
                    (result[leg.foot].translation.z - world[leg.foot].translation.z - BIAS).abs()
                        < 0.005,
                    "tick={tick}, foot={}",
                    leg.foot
                );
            }
            assert_eq!(actual[toe], base[toe]);
            assert_lengths(&rig, &base, &actual, &parents);
        }
    }
    #[test]
    fn teleport_and_lost_probe_do_not_carry_previous_slope_offsets() {
        let (rig, base, parents) = fixture();
        let ramp = Ramp {
            min: Vec2::splat(-10.0),
            max: Vec2::splat(10.0),
            base: 0.0,
            height: 8.0,
            slope: Vec2::new(0.3, 0.0),
        };
        let arena = Arena {
            ramps: vec![ramp],
            ..Default::default()
        };
        let mut ik = FootIk::default();
        let root = Affine3A::from_translation(Vec3::new(0.0, 0.0, 11.0));
        let mut pose = base.clone();
        for _ in 0..20 {
            pose.clone_from_slice(&base);
            ik.apply(&rig, &mut pose, &parents, root, true, 0.032, &arena);
        }
        assert!(ik.offsets[0] < 0.0);
        pose.clone_from_slice(&base);
        ik.apply(
            &rig,
            &mut pose,
            &parents,
            Affine3A::from_translation(Vec3::new(100.0, 0.0, 0.0)),
            true,
            0.032,
            &arena,
        );
        assert!((ik.offsets[0] - BIAS * 0.5).abs() < 1e-5);
        pose.clone_from_slice(&base);
        ik.apply(
            &rig,
            &mut pose,
            &parents,
            root,
            true,
            0.032,
            &crate::camera::Open,
        );
        assert_eq!(pose, base);
        assert_eq!(ik.offsets, [0.0; 2]);
    }
}
