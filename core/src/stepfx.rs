//! FootStep/ClimbStep contacts shared by their visual and sound cues.
//! [game: 0073f520,0073fa20,00718720] Probe the named component, then select its
//! impact preset by the contact material. [assumed] Synchronous final-pose probes
//! replace Lux's asynchronous callbacks (existing sound timing debt in status.md).

use glam::{Affine3A, Mat3, Quat, Vec3};
use crate::{formats::{anim::CLIMB_SIDES, scene::FootstepDef}, sim::World, vehicle::GroundHit};

pub fn footsteps(side: u32, pose: &[Affine3A], feet: &[FootstepDef], world: &impl World) -> Vec<GroundHit> {
    feet.iter().filter(|foot| foot.side == side).filter_map(|foot| {
        let point = Vec3::from(pose.get(foot.node)?.translation);
        world.wheel_ray(point + Vec3::Z * foot.height, point - Vec3::Z * foot.height)
    }).collect()
}

/// Bumblebee's installed four ClimbStep components have offsetFromWall=0 and
/// spawnSlideEffectOnThisBone=true. Slide therefore probes all four. [data]
pub fn climbing(limb: usize, pose: &[Affine3A], steps: &[(usize, u32)], forward: Vec3, world: &impl World) -> Vec<GroundHit> {
    let Some(&side) = CLIMB_SIDES.get(limb) else { return Vec::new() };
    let direction = forward.normalize_or_zero() * 1.5;
    steps.iter().filter(|(_, s)| limb == 4 || *s == side).filter_map(|&(node, _)| {
        let point = Vec3::from(pose.get(node)?.translation);
        world.wheel_ray(point - direction, point + direction)
    }).collect()
}

/// [game: 0077c180, flags1] +z follows the contact normal; +y uses projected
/// world up, falling back to +y when almost parallel (absolute dot >0.999).
/// Bumblebee's footstep flags0x100 instead preserve the hit object's axes; the
/// arena's static world-frame attachment is represented by identity.
pub fn rotation(flags: u32, normal: Vec3) -> Quat {
    if flags & 1 == 0 { return Quat::IDENTITY }
    let z = normal.normalize_or_zero();
    let reference = if z.z.abs() > 0.999 { Vec3::Y } else { Vec3::Z };
    let x = reference.cross(z).normalize_or_zero();
    let y = z.cross(x).normalize_or_zero();
    Quat::from_mat3(&Mat3::from_cols(x, y, z))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{Arena, Box3};

    #[test]
    fn foot_contact_uses_the_named_component_and_requires_ground_in_probe_range() {
        let side = 0x439eb7a4;
        let feet = [FootstepDef { node: 0, side, height: 1.5 }];
        let mut pose = [Affine3A::from_translation(Vec3::new(4.0, 5.0, 0.2))];
        let arena = Arena::default();
        let hit = footsteps(side, &pose, &feet, &arena);
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].point, Vec3::new(4.0, 5.0, 0.0));
        assert_eq!(hit[0].normal, Vec3::Z);
        assert!(footsteps(0, &pose, &feet, &arena).is_empty());
        pose[0].translation.z = 3.0;
        assert!(footsteps(side, &pose, &feet, &arena).is_empty());
    }

    #[test]
    fn climb_contact_hits_wall_surface_and_slide_reaches_each_component() {
        let steps: Vec<_> = CLIMB_SIDES[..4].iter().enumerate().map(|(i, &s)| (i, s)).collect();
        let pose = [Affine3A::from_translation(Vec3::new(0.0, 0.0, 3.0)); 4];
        let arena = Arena { boxes: vec![Box3 { min: Vec3::new(-2.0, 1.0, 0.0), max: Vec3::new(2.0, 2.0, 6.0) }], ..Default::default() };
        let hits = climbing(0, &pose, &steps, Vec3::Y, &arena);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].point, Vec3::new(0.0, 1.0, 3.0));
        assert_eq!(hits[0].normal, Vec3::NEG_Y);
        assert_eq!(climbing(4, &pose, &steps, Vec3::Y, &arena).len(), 4);
        assert!(climbing(0, &pose, &steps, Vec3::X, &arena).is_empty());
        assert!(climbing(5, &pose, &steps, Vec3::Y, &arena).is_empty());
    }

    #[test]
    fn climbing_dust_preserves_world_up_twist_instead_of_using_collision_fx_axes() {
        for normal in [Vec3::X, Vec3::NEG_Y, Vec3::Z] {
            let q = rotation(1, normal);
            assert!((q * Vec3::Z).abs_diff_eq(normal, 1e-6));
            assert!((q * Vec3::Y).abs_diff_eq(if normal == Vec3::Z { Vec3::Y } else { Vec3::Z }, 1e-6));
        }
        assert_eq!(rotation(0x100, Vec3::X), Quat::IDENTITY);
    }
}
