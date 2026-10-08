//! The follow camera's separate radius-one contact proxy and near-plane correction.
//! The driving behaviour deliberately uses its uncorrected eye. [game]
use super::Hit;
use glam::{Vec2, Vec3};

/// Contact normals and points come from the camera proxy's physics callback. [game]
pub(super) fn adjust(
    eye: Vec3,
    forward: Vec3,
    fov: f32,
    near: f32,
    wide: bool,
    contacts: &[Hit],
) -> Vec3 {
    let right = forward.cross(Vec3::Z).normalize_or_zero();
    let up = right.cross(forward);
    let extent = fov.to_radians().tan() * near;
    let vertical = extent * if wide { 0.5625 } else { 0.75 };
    let mut lateral = Vec2::ZERO;
    let mut depth = Vec2::ZERO;
    let mut upright = Vec2::ZERO;
    let keep = |range: &mut Vec2, value: f32| {
        if value >= 0.0 {
            range.x = range.x.max(value);
        } else {
            range.y = range.y.min(value);
        }
    };
    for hit in contacts.iter().take(16) {
        let dot = forward.dot(hit.normal);
        if dot.abs() >= 0.999 || dot.abs() <= 0.0001 {
            continue;
        }
        let distance = (eye - hit.point).dot(hit.normal);
        let x = right.dot(hit.normal);
        let z = up.dot(hit.normal);
        let (across, limit, range) = if x.abs() > z.abs() {
            (x, extent, &mut lateral)
        } else {
            (z, vertical, &mut upright)
        };
        if across.abs() <= 0.0001 {
            continue;
        }
        let reach = (distance / across).abs();
        if reach >= limit {
            continue;
        }
        let shift = (limit - reach) * across.abs() / dot;
        let clipped = shift.clamp(-1.25, 1.25);
        keep(&mut depth, clipped);
        // The remaining displacement along the plane's dominant picture axis.
        keep(range, (shift - clipped) * dot / across);
    }
    let combine = |v: Vec2| {
        if v.x != 0.0 && v.y != 0.0 {
            (v.x + v.y) * 0.5
        } else {
            v.x + v.y
        }
    };
    eye + right * combine(lateral) + forward * combine(depth) + up * combine(upright)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frontal_planes_are_ignored_but_oblique_near_plane_corners_move_the_eye() {
        let eye = Vec3::ZERO;
        let front = Hit {
            point: Vec3::Y * 0.1,
            normal: Vec3::Y,
            ..Default::default()
        };
        assert_eq!(adjust(eye, Vec3::Y, 45.0, 0.5, true, &[front]), eye);
        let corner = Hit {
            point: Vec3::new(0.1, 0.1, 0.0),
            normal: Vec3::new(1.0, 1.0, 0.0).normalize(),
            ..Default::default()
        };
        let result = adjust(eye, Vec3::Y, 45.0, 0.5, true, &[corner]);
        assert!(
            result.abs_diff_eq(Vec3::new(0.0, 0.3, 0.0), 1e-6),
            "{result}"
        );
    }
    #[test]
    fn forward_correction_is_capped_and_excess_spills_sideways() {
        let hit = Hit {
            point: Vec3::ZERO,
            normal: Vec3::new(1.0, 0.01, 0.0).normalize(),
            ..Default::default()
        };
        let result = adjust(Vec3::ZERO, Vec3::Y, 45.0, 0.5, true, &[hit]);
        assert_eq!(result.y, 1.25);
        assert!(result.x > 0.4);
    }
}
