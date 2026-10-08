//! Camera query geometry for the authored arena. Sphere sweeps use the actual faces,
//! rounded edges and vertices; expanding a box gives the wrong answer at its corners.
use super::{Hit, Shape};
use glam::{Vec2, Vec3};

pub(super) fn box_vertices(min: Vec3, max: Vec3) -> [Vec3; 8] {
    std::array::from_fn(|i| {
        Vec3::new(
            if i & 1 == 0 { min.x } else { max.x },
            if i & 2 == 0 { min.y } else { max.y },
            if i & 4 == 0 { min.z } else { max.z },
        )
    })
}

pub(super) fn edges(vertices: &[Vec3; 8]) -> [(Vec3, Vec3); 12] {
    let mut out = [(Vec3::ZERO, Vec3::ZERO); 12];
    let mut at = 0;
    for i in 0..8 {
        for bit in [1, 2, 4] {
            if i & bit == 0 {
                out[at] = (vertices[i], vertices[i | bit]);
                at += 1;
            }
        }
    }
    out
}

fn root(offset: Vec3, travel: Vec3, radius: f32) -> Option<f32> {
    let a = travel.length_squared();
    let b = offset.dot(travel);
    let c = offset.length_squared() - radius * radius;
    if c < 0.0 {
        return Some(0.0);
    }
    if a < 1e-12 || b >= 0.0 {
        return None;
    }
    let d = b * b - a * c;
    if d < 0.0 {
        return None;
    }
    let t = (-b - d.sqrt()) / a;
    (0.0..=1.0).contains(&t).then_some(t)
}

/// Exact sphere contact. Lux's convex cast returns the obstacle contact, while its
/// distance field stores centre travel (005d6930 /005e24c0). [game]
pub(super) fn sweep(
    from: Vec3,
    to: Vec3,
    radius: f32,
    planes: &[(Vec3, f32)],
    vertices: &[Vec3; 8],
    shape: Option<Shape>,
) -> Option<Hit> {
    let travel = to - from;
    let mut nearest: Option<(f32, Vec3)> = None;
    let mut keep = |t: f32, n: Vec3| {
        if (0.0..=1.0).contains(&t) && nearest.is_none_or(|(old, _)| t < old) {
            nearest = Some((t, n));
        }
    };
    // A camera starting inside the solid, or overlapping its rounded surface, reports
    // distance zero. The driving behaviour's own too-near rule places its eye at P.
    if planes.iter().all(|(n, d)| n.dot(from) <= *d) {
        let (n, _) = planes
            .iter()
            .min_by(|(a, da), (b, db)| (da - a.dot(from)).total_cmp(&(db - b.dot(from))))?;
        return Some(Hit {
            point: from,
            normal: *n,
            shape,
            distance: Some(0.0),
        });
    }
    for &(n, d) in planes {
        let gap = n.dot(from) - d;
        let speed = n.dot(travel);
        if speed >= -1e-9 {
            continue;
        }
        let t = ((radius - gap) / speed).max(0.0);
        if t > 1.0 {
            continue;
        }
        let contact = from + travel * t - n * radius;
        if planes.iter().all(|(a, b)| a.dot(contact) <= *b + 1e-5) {
            keep(t, n);
        }
    }
    for (a, b) in edges(vertices) {
        let edge = b - a;
        let squared = edge.length_squared();
        if squared < 1e-10 {
            continue;
        }
        let offset = from - a;
        let perpendicular = offset - edge * (offset.dot(edge) / squared);
        let movement = travel - edge * (travel.dot(edge) / squared);
        if let Some(t) = root(perpendicular, movement, radius) {
            let centre = from + travel * t;
            let u = (centre - a).dot(edge) / squared;
            if (0.0..=1.0).contains(&u) {
                keep(t, (centre - (a + edge * u)).normalize_or_zero());
            }
        }
    }
    for &v in vertices {
        if let Some(t) = root(from - v, travel, radius) {
            keep(t, (from + travel * t - v).normalize_or_zero());
        }
    }
    nearest.map(|(t, normal)| Hit {
        point: from + travel * t - normal * radius,
        normal,
        shape,
        distance: Some(t * travel.length()),
    })
}

pub(super) fn box_sweep(b: &crate::sim::Box3, from: Vec3, to: Vec3, radius: f32) -> Option<Hit> {
    let planes = [
        (-Vec3::X, -b.min.x),
        (Vec3::X, b.max.x),
        (-Vec3::Y, -b.min.y),
        (Vec3::Y, b.max.y),
        (-Vec3::Z, -b.min.z),
        (Vec3::Z, b.max.z),
    ];
    sweep(
        from,
        to,
        radius,
        &planes,
        &box_vertices(b.min, b.max),
        Some(Shape::Box {
            min: b.min,
            max: b.max,
        }),
    )
}

pub(super) fn arena_sweep(
    arena: &crate::sim::Arena,
    from: Vec3,
    to: Vec3,
    radius: f32,
) -> Option<Hit> {
    let mut nearest = None;
    let mut keep = |hit: Hit| {
        if nearest
            .as_ref()
            .is_none_or(|old: &Hit| hit.distance < old.distance)
        {
            nearest = Some(hit);
        }
    };
    if from.z < radius {
        keep(Hit {
            point: from,
            normal: Vec3::Z,
            shape: None,
            distance: Some(0.0),
        });
    } else if to.z < radius {
        let t = (from.z - radius) / (from.z - to.z);
        keep(Hit {
            point: from + (to - from) * t - Vec3::Z * radius,
            normal: Vec3::Z,
            shape: None,
            distance: Some(t * (to - from).length()),
        });
    }
    for b in &arena.boxes {
        if let Some(hit) = box_sweep(b, from, to, radius) {
            keep(hit);
        }
    }
    for ramp in &arena.ramps {
        let vertices = std::array::from_fn(|i| {
            let xy = Vec2::new(
                if i & 1 == 0 { ramp.min.x } else { ramp.max.x },
                if i & 2 == 0 { ramp.min.y } else { ramp.max.y },
            );
            xy.extend(if i & 4 == 0 { ramp.base } else { ramp.top(xy) })
        });
        // Wedges are not Lux's type-4 boxes. As for any other shape the original can
        // take a corner without retaining an edge; do not invent one for a slope.
        if let Some(hit) = sweep(from, to, radius, &ramp.planes(), &vertices, None) {
            keep(hit);
        }
    }
    nearest
}

pub(super) fn arena_contacts(arena: &crate::sim::Arena, centre: Vec3, radius: f32) -> Vec<Hit> {
    let mut contacts = Vec::new();
    if centre.z < radius {
        contacts.push(Hit {
            point: Vec3::new(centre.x, centre.y, 0.0),
            normal: Vec3::Z,
            ..Default::default()
        });
    }
    for b in &arena.boxes {
        let point = centre.clamp(b.min, b.max);
        let offset = centre - point;
        if offset.length_squared() > radius * radius {
            continue;
        }
        if offset.length_squared() > 1e-10 {
            contacts.push(Hit {
                point,
                normal: offset.normalize(),
                ..Default::default()
            });
        } else {
            let (normal, distance) = [
                (-Vec3::X, centre.x - b.min.x),
                (Vec3::X, b.max.x - centre.x),
                (-Vec3::Y, centre.y - b.min.y),
                (Vec3::Y, b.max.y - centre.y),
                (-Vec3::Z, centre.z - b.min.z),
                (Vec3::Z, b.max.z - centre.z),
            ]
            .into_iter()
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap();
            contacts.push(Hit {
                point: centre + normal * distance,
                normal,
                ..Default::default()
            });
        }
    }
    for r in &arena.ramps {
        let planes = r.planes();
        let inside = planes.iter().all(|(n, d)| n.dot(centre) <= *d);
        let mut nearest: Option<(f32, Hit)> = None;
        let mut keep = |point: Vec3, normal: Vec3| {
            let squared = centre.distance_squared(point);
            if (inside || squared <= radius * radius)
                && nearest.as_ref().is_none_or(|(old, _)| squared < *old)
            {
                nearest = Some((
                    squared,
                    Hit {
                        point,
                        normal,
                        ..Default::default()
                    },
                ));
            }
        };
        // Closest finite face/edge/vertex, including overlap at a wedge's round corner.
        for &(normal, d) in &planes {
            let distance = normal.dot(centre) - d;
            let point = centre - normal * distance;
            if planes.iter().all(|(n, p)| n.dot(point) <= *p + 1e-5) {
                keep(point, normal);
            }
        }
        if !inside {
            let vertices = std::array::from_fn(|i| {
                let xy = Vec2::new(
                    if i & 1 == 0 { r.min.x } else { r.max.x },
                    if i & 2 == 0 { r.min.y } else { r.max.y },
                );
                xy.extend(if i & 4 == 0 { r.base } else { r.top(xy) })
            });
            for (a, b) in edges(&vertices) {
                let edge = b - a;
                let u = if edge.length_squared() > 1e-10 {
                    ((centre - a).dot(edge) / edge.length_squared()).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let point = a + edge * u;
                keep(point, (centre - point).normalize_or_zero());
            }
        }
        if let Some((_, hit)) = nearest {
            contacts.push(hit);
        }
    }
    contacts.truncate(16);
    contacts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::Sight;
    fn arena() -> crate::sim::Arena {
        crate::sim::Arena {
            boxes: vec![crate::sim::Box3 {
                min: Vec3::new(0.0, 0.0, 0.0),
                max: Vec3::new(2.0, 2.0, 2.0),
            }],
            ..Default::default()
        }
    }
    #[test]
    fn sphere_hits_off_axis_wall_that_a_ray_misses() {
        let a = arena();
        let from = Vec3::new(-3.0, -0.4, 1.0);
        let to = Vec3::new(3.0, -0.4, 1.0);
        assert!(a.sight(from, to).is_none());
        let hit = a.sweep(from, to, 0.65).unwrap();
        let centre = hit.point + hit.normal * 0.65;
        assert!((centre.x + (0.65_f32.powi(2) - 0.4_f32.powi(2)).sqrt()).abs() < 1e-5);
        assert!(hit.normal.x < 0.0 && hit.normal.y < 0.0);
    }
    #[test]
    fn sphere_corner_is_round_and_is_not_an_expanded_box() {
        let a = arena();
        let from = Vec3::new(-3.0, -3.0, 1.0);
        let to = Vec3::new(1.0, 1.0, 1.0);
        let hit = a.sweep(from, to, 0.65).unwrap();
        assert!(((hit.point + hit.normal * 0.65).x + 0.65 / 2.0_f32.sqrt()).abs() < 1e-5);
        let clear_from = Vec3::new(-0.6, -0.6, 1.0);
        assert!(
            a.sweep(clear_from, clear_from + Vec3::Z * 0.1, 0.65)
                .is_none()
        );
    }
    #[test]
    fn sphere_respects_sloped_top_and_reports_initial_overlap() {
        let r = crate::terrain::Ramp {
            min: Vec2::ZERO,
            max: Vec2::splat(5.0),
            base: 0.0,
            height: 1.0,
            slope: Vec2::new(0.4, 0.0),
        };
        let a = crate::sim::Arena {
            ramps: vec![r],
            ..Default::default()
        };
        let hit = a
            .sweep(Vec3::new(2.0, 2.0, 6.0), Vec3::new(2.0, 2.0, 1.0), 0.65)
            .unwrap();
        let centre = hit.point + hit.normal * 0.65;
        assert!((centre.z - (r.top(Vec2::splat(2.0)) + 0.65 / r.normal().z)).abs() < 1e-5);
        assert_eq!(
            arena()
                .sweep(Vec3::ONE, Vec3::new(4.0, 1.0, 1.0), 0.65)
                .unwrap()
                .point,
            Vec3::ONE
        );
    }
    #[test]
    fn proxy_detects_wedge_edge_overlap_when_both_face_projections_miss() {
        let r = crate::terrain::Ramp {
            min: Vec2::ZERO,
            max: Vec2::splat(5.0),
            base: 0.0,
            height: 2.0,
            slope: Vec2::new(0.4, 0.0),
        };
        let a = crate::sim::Arena {
            ramps: vec![r],
            ..Default::default()
        };
        let hits = arena_contacts(&a, Vec3::new(-0.4, -0.4, 1.0), 0.65);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].point.abs_diff_eq(Vec3::Z, 1e-6));
        assert!(
            hits[0]
                .normal
                .abs_diff_eq(Vec3::new(-1.0, -1.0, 0.0).normalize(), 1e-6)
        );
    }
}
