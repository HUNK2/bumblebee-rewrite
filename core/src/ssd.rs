//! Surface damage volumes (`lux::SSD`), from the executable and SSDInfo data.
//! See notes/surface-damage.md for the function addresses and qualifications.
use crate::formats::{lxb::Node, scene::Shape};
use crate::melee::Body;
use glam::{Affine3A, Mat3, Vec2, Vec3, Vec4};

pub const LIMIT: usize = 512; // [game] 0059ecd0
const WATER: u32 = 0x3af2_3bde;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Info {
    pub radius: f32,
    pub frames: u32,
    pub noise_ratio: f32,
    pub chaos: f32,
    /// outer cracks, outer damage, inner cracks, inner damage.
    pub thresholds: [f32; 4],
    pub softness: [f32; 4],
}
impl Info {
    pub fn read(node: Node) -> Self {
        let num = |name| node.get(name).and_then(Node::float).unwrap_or(0.0);
        Self {
            radius: num("Radius"),
            frames: node
                .get("NumFrames")
                .and_then(Node::int)
                .unwrap_or(1)
                .max(1) as u32,
            noise_ratio: num("NoiseMapSizeRatio"),
            chaos: num("Chaos"),
            thresholds: [
                num("CracksExtThreshold"),
                num("DamageExtThreshold"),
                num("CracksIntThreshold"),
                num("DamageIntThreshold"),
            ],
            softness: [
                num("CracksExtSoftness"),
                num("DamageExtSoftness"),
                num("CracksIntSoftness"),
                num("DamageIntSoftness"),
            ],
        }
    }
}
pub fn read(preset: Option<Node>) -> Vec<Info> {
    preset
        .and_then(|p| p.get("ssdInfos"))
        .into_iter()
        .flat_map(|n| n.items())
        .map(Info::read)
        .collect()
}

#[derive(Clone, Debug)]
pub struct Sphere {
    pub centre: Vec3,
    pub radius: f32,
    pub info: Info,
    pub frame: u32,
    pub seed: u32,
    pub max_radius: f32,
    noise_scale: f32,
}
impl Sphere {
    /// [game] Water and object bit 25 suppress the mark, independently of materialInfos.
    pub fn contact(
        info: Info,
        point: Vec3,
        normal: Vec3,
        surface: u32,
        object_flags: u32,
        seed: u32,
    ) -> Option<Self> {
        if surface == WATER
            || object_flags & (1 << 25) != 0
            || info.radius <= 0.0
            || info.noise_ratio <= 0.0
        {
            return None;
        }
        let depth = if normal.z.abs() >= 0.1 { 0.2 } else { 0.5 };
        Some(Self {
            centre: point - normal * depth,
            radius: info.radius,
            info,
            frame: 1,
            seed,
            max_radius: info.radius * 5.0,
            noise_scale: (info.radius * info.noise_ratio).recip(),
        })
    }
    pub fn advance_frame(&mut self) {
        self.frame = (self.frame + 1).min(self.info.frames.max(1));
    }
    /// [game] 0059e190: finite hard thresholds and the 0.133 frame ramp.
    pub fn ramps(&self) -> (Vec4, Vec4) {
        let ramp = |i: usize| {
            let softness = self.info.softness[i];
            let scale = 0.5 / softness.max(1e-7);
            let threshold = self.info.thresholds[i]
                + 0.133 * (1.0 - self.frame as f32 / self.info.frames.max(1) as f32);
            (scale, -(threshold - softness) * scale)
        };
        let (a, b, c, d) = (ramp(0), ramp(1), ramp(2), ramp(3));
        (Vec4::new(a.0, a.1, b.0, b.1), Vec4::new(c.0, c.1, d.0, d.1))
    }
    /// Same mask arithmetic as effects.lsa 00efa0, with the sampled noise handed in.
    pub fn mask(&self, point: Vec3, noise: f32) -> Vec2 {
        let edge = (1.0 - point.distance(self.centre) / self.radius).clamp(0.0, 1.0);
        let feather = (5.0 * edge).clamp(0.0, 1.0);
        let outer = edge + 0.5 + (noise - 0.5) * self.info.chaos;
        let (a, b) = self.ramps();
        Vec2::new(
            feather
                * feather
                * (a.x * outer + a.y).clamp(0.0, 1.0)
                * (b.x * noise + b.y).clamp(0.0, 1.0),
            feather * (a.z * outer + a.w).clamp(0.0, 1.0) * (b.z * noise + b.w).clamp(0.0, 1.0),
        )
    }
    /// [game] 0059e190 / 00570470: Rz * Rx * Ry, applied to world point + offset.
    /// The shader calls this matrix "Inv", but 00530590 composes R * T, without inversion.
    pub fn noise_transform(&self) -> Affine3A {
        let mut seed = self.seed;
        let mut roll = || {
            seed = seed.wrapping_mul(0x343fd).wrapping_add(0x269ec3);
            f32::from_bits(0x3f800000 | ((seed >> 8) & 0x7fff00)) - 1.0
        };
        let radians_per_degree = f32::from_bits(0x3c8efa35);
        let x = (roll() * 360.0) * radians_per_degree;
        let y = (roll() * 180.0 - 90.0) * radians_per_degree;
        let z = (roll() * 180.0 - 90.0) * radians_per_degree;
        let rotation = Mat3::from_rotation_z(z)
            * (Mat3::from_rotation_x(x) * Mat3::from_rotation_y(y));
        // 0059e47d..0059e51f stores samples 4, 5, 6 in Z, Y, X respectively.
        let offset_z = roll() * 1000.0;
        let offset_y = roll() * 1000.0;
        let offset_x = roll() * 1000.0;
        let offset = Vec3::new(offset_x, offset_y, offset_z);
        Affine3A::from_mat3_translation(rotation, rotation * offset)
    }
    pub fn noise_scale(&self) -> f32 {
        self.noise_scale
    }
}

#[derive(Default, Debug)]
pub struct Damage {
    pub spheres: Vec<Sphere>,
    next_seed: u32,
}
impl Damage {
    pub fn put(&mut self, mut sphere: Sphere, eye: Vec3) {
        sphere.seed = self.next_seed;
        self.next_seed = self.next_seed.wrapping_add(1);
        // [game] Greatest positive squared overlap, newest against the existing records.
        loop {
            let best = self
                .spheres
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    (
                        i,
                        s.radius * s.radius + sphere.radius * sphere.radius
                            - s.centre.distance_squared(sphere.centre),
                    )
                })
                .filter(|(_, score)| *score > 0.0)
                .max_by(|a, b| a.1.total_cmp(&b.1));
            let Some((index, _)) = best else { break };
            let older = self.spheres.remove(index);
            let distance = sphere.centre.distance(older.centre);
            let (centre, radius) = if sphere.radius >= distance + older.radius {
                (sphere.centre, sphere.radius)
            } else if older.radius >= distance + sphere.radius {
                (older.centre, older.radius)
            } else {
                let radius = (sphere.radius + older.radius + distance) * 0.5;
                (
                    sphere.centre
                        + (older.centre - sphere.centre) * ((radius - sphere.radius) / distance),
                    radius,
                )
            };
            let fraction = (sphere.frame as f32 / sphere.info.frames.max(1) as f32)
                .max(older.frame as f32 / older.info.frames.max(1) as f32);
            let frames = older.info.frames.max(1);
            if sphere.radius <= older.radius {
                sphere = older;
            }
            sphere.centre = centre;
            sphere.radius = radius.min(sphere.max_radius);
            sphere.info.frames = frames;
            sphere.frame = ((fraction * frames as f32 + 0.5) as u32).min(frames);
        }
        self.spheres.insert(0, sphere);
        if self.spheres.len() > LIMIT {
            let far = self
                .spheres
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| {
                    a.centre
                        .distance_squared(eye)
                        .total_cmp(&b.centre.distance_squared(eye))
                })
                .unwrap()
                .0;
            self.spheres.remove(far);
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Contact {
    pub surface: usize,
    pub point: Vec3,
    pub normal: Vec3,
}

#[derive(Clone, Debug)]
pub struct Hit {
    pub contact: Contact,
    /// None means the character's melee preset; Some is an explosion's own preset.
    pub infos: Option<Vec<Info>>,
}

/// [stand-in] Arena boxes/ground, using the existing primitive contact solver.
/// Ground has id 0; boxes 1.. . No character contact is included here.
pub fn contacts(body: &Body, boxes: &[crate::sim::Box3]) -> Vec<Contact> {
    let mut out = Vec::new();
    let centre: Vec3 = body.at.translation.into();
    let bottom = body.nearest(centre - Vec3::Z * 1e6);
    if bottom.z <= 0.01 {
        out.push(Contact {
            surface: 0,
            point: Vec3::new(bottom.x, bottom.y, 0.0),
            normal: Vec3::Z,
        });
    }
    for (index, b) in boxes.iter().enumerate() {
        let theirs = Body {
            shape: Shape::Cuboid {
                half: ((b.max - b.min) * 0.5).to_array(),
            },
            at: Affine3A::from_translation((b.max + b.min) * 0.5),
        };
        let Some(mut point) = crate::melee::touch(body, &theirs) else {
            continue;
        };
        // Contact must be on the exterior (the solver can return an interior point).
        let mut best = (f32::INFINITY, Vec3::Z, 2, b.max.z);
        for axis in 0..3 {
            for (side, sign) in [(b.min[axis], -1.0), (b.max[axis], 1.0)] {
                let distance = (point[axis] - side).abs();
                if distance < best.0 {
                    let mut normal = Vec3::ZERO;
                    normal[axis] = sign;
                    best = (distance, normal, axis, side);
                }
            }
        }
        point[best.2] = best.3;
        out.push(Contact {
            surface: index + 1,
            point,
            normal: best.1,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn info() -> Info {
        Info {
            radius: 2.0,
            frames: 10,
            noise_ratio: 0.553,
            chaos: 0.0,
            thresholds: [1.032, 2.0, 0.118, 0.0],
            softness: [0.5, 0.0, 0.5, 0.0],
        }
    }
    fn sphere(point: Vec3) -> Sphere {
        Sphere::contact(info(), point, Vec3::Z, 0, 0, 0).unwrap()
    }
    #[test]
    fn seeded_noise_matches_game_matrix_arithmetic() {
        // Independent expanded Rz*Rx*Ry coefficients from 00570470, with the
        // caller's float32 degree conversion and R*T translation (00530590).
        // Two seeds catch axis/sample order, handedness and accidental inversion.
        for (seed, rows, translation) in [
            (
                0,
                [
                    [0.60502817, -0.44874285, -0.65770117],
                    [0.29779814, 0.89363124, -0.33576706],
                    [0.73841538, 0.00728634, 0.67430687],
                ],
                Vec3::new(47.64043, 323.7294, 317.95938),
            ),
            (
                0xbeef,
                [
                    [0.60077121, 0.67658817, 0.42579614],
                    [-0.68393329, 0.15924085, 0.71195331],
                    [0.41389506, -0.71893728, 0.55840851],
                ],
                Vec3::new(819.74054, -71.41698, 480.14645),
            ),
        ] {
            let mut s = sphere(Vec3::ZERO);
            s.seed = seed;
            let m = s.noise_transform();
            for (axis, column) in [Vec3::X, Vec3::Y, Vec3::Z].into_iter().zip(0..3) {
                let expected = Vec3::new(rows[0][column], rows[1][column], rows[2][column]);
                assert!(m.transform_vector3(axis).abs_diff_eq(expected, 1e-6));
            }
            assert!(m.transform_point3(Vec3::ZERO).abs_diff_eq(translation, 1e-3));
            let point = Vec3::new(13.0, -27.0, 41.0);
            let expected = Vec3::new(
                Vec3::from_array(rows[0]).dot(point),
                Vec3::from_array(rows[1]).dot(point),
                Vec3::from_array(rows[2]).dot(point),
            ) + translation;
            assert!(m.transform_point3(point).abs_diff_eq(expected, 1e-3));
        }
    }
    #[test]
    fn water_and_suppressed_objects_leave_no_mark() {
        assert!(Sphere::contact(info(), Vec3::ZERO, Vec3::Z, WATER, 0, 0).is_none());
        assert!(Sphere::contact(info(), Vec3::ZERO, Vec3::Z, 0, 1 << 25, 0).is_none());
        assert_eq!(sphere(Vec3::ZERO).centre.z, -0.2);
        assert_eq!(
            Sphere::contact(info(), Vec3::ZERO, Vec3::X, 0, 0, 0)
                .unwrap()
                .centre
                .x,
            -0.5
        );
    }
    #[test]
    fn frames_deepen_then_persist_with_finite_hard_thresholds() {
        let mut s = sphere(Vec3::ZERO);
        let first = s.mask(Vec3::ZERO, 0.5);
        for _ in 0..100 {
            s.advance_frame();
        }
        assert_eq!(s.frame, 10);
        let last = s.mask(Vec3::ZERO, 0.5);
        assert!(last.x >= first.x && last.is_finite());
        assert_eq!(last.y, 0.0);
        assert_eq!(s.mask(Vec3::splat(10.0), 0.5), Vec2::ZERO);
    }
    #[test]
    fn repeated_hits_merge_and_cap_the_volume() {
        let mut damage = Damage::default();
        for x in 0..40 {
            damage.put(sphere(Vec3::new(x as f32 * 0.1, 0.0, 0.0)), Vec3::ZERO);
        }
        assert_eq!(damage.spheres.len(), 1);
        assert!(damage.spheres[0].radius > 2.0);
        assert!(damage.spheres[0].radius <= 10.0);
    }
    #[test]
    fn capacity_evicts_farthest_from_the_eye() {
        let mut damage = Damage::default();
        for i in 0..=LIMIT {
            damage.put(sphere(Vec3::X * (i as f32 * 10.0)), Vec3::ZERO);
        }
        assert_eq!(damage.spheres.len(), LIMIT);
        assert!(
            damage
                .spheres
                .iter()
                .all(|s| s.centre.x < (LIMIT as f32 * 10.0))
        );
    }
    #[test]
    fn wall_and_ground_contacts_stay_on_the_exterior() {
        let b = crate::sim::Box3 {
            min: Vec3::new(2.0, -3.0, 0.0),
            max: Vec3::new(4.0, 3.0, 10.0),
        };
        let body = Body {
            shape: Shape::Sphere { radius: 1.0 },
            at: Affine3A::from_translation(Vec3::new(1.5, 0.0, 0.5)),
        };
        let hit = contacts(&body, &[b]);
        assert_eq!(hit.len(), 2);
        assert_eq!(hit[0].normal, Vec3::Z);
        assert_eq!(hit[1].normal, -Vec3::X);
        assert_eq!(hit[1].point.x, 2.0);
    }
    #[test]
    fn bumblebees_presets_are_read_from_his_pack() {
        let dir = std::env::var_os("TF2_GAME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| "C:/Games2".into());
        let data = crate::character::load(&dir, "bumblebee").unwrap();
        assert_eq!(data.melee_impact.ssd.len(), 1);
        assert_eq!(
            (
                data.melee_impact.ssd[0].radius,
                data.melee_impact.ssd[0].frames
            ),
            (2.0, 10)
        );
        assert_eq!(
            (
                data.weapon_effects[0].impact.ssd[0].radius,
                data.weapon_effects[0].impact.ssd[0].frames
            ),
            (2.0, 6)
        );
        let blast = &data.weapon_effects[1].blast_ssd;
        assert_eq!((blast[0].radius, blast[0].frames), (7.0, 4));
        for script in [
            crate::formats::hash::crc32(b"FX_groundpunch"),
            crate::formats::hash::crc32(b"FX_groundpunchweak"),
        ] {
            let spawner = data
                .robot
                .spawners
                .iter()
                .find(|s| s.script_name == script)
                .unwrap();
            assert_eq!(&spawner.ssd, blast);
        }
        let bytes = std::fs::read(dir.join("Textures/NoiseVolume.apk")).unwrap();
        let noise = crate::formats::texture::parse(&bytes).unwrap().unwrap();
        assert_eq!(
            (noise.id, noise.width, noise.height),
            (0x1881f96c, 128, 128)
        );
    }
}
