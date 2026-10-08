//! [stand-in] The test arena: blocks, ramps, slopes, suspension lanes and a climbing building.

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::light::GlobalAmbientLight;
use bevy::math::Affine2;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::camera::Hdr;
use bevy::pbr::{DistanceFog, FogFalloff};
use bevy::post_process::bloom::{Bloom, BloomCompositeMode, BloomPrefilter};
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::render::view::ColorGrading;
use std::sync::OnceLock;
use tf2_core::formats::environment::Environment;
use tf2_core::terrain::Ramp;

use crate::assets::game_to_bevy;
use crate::sim::{Arena, Box3, JumpType, jump_height};
use crate::tuning::Tuning;

const GROUND_SIZE: f32 = 1200.0;
/// One grid square on the ground, in game units.
const GRID: f32 = 10.0;

#[derive(Resource)]
pub struct ArenaRes(pub Arena);

/// Blocks to test against and to show the moves on: steps at the jump heights, a wall,
/// pillars to steer round, climbing walls, an avenue, ramps, a hill, a banked lane,
/// progressive rollers and alternating wheel bumps,
/// a stepped tower, a courtyard climbing building and a gate.
/// [stand-in] Authored dimensions for mechanic tests.
pub fn layout(tuning: &Tuning) -> Arena {
    let mut boxes = Vec::new();
    let mut block = |x: f32, y: f32, w: f32, d: f32, h: f32| {
        boxes.push(Box3 { min: Vec3::new(x - w / 2.0, y - d / 2.0, 0.0), max: Vec3::new(x + w / 2.0, y + d / 2.0, h) });
    };
    // A row of platforms just under each jump's height, so each jump can be told apart:
    // running, standing, then the jumps out of vehicle form.
    let jumps = [JumpType::Moving, JumpType::Standing, JumpType::Long].map(|kind| jump_height(tuning, kind));
    for (i, height) in jumps.into_iter().enumerate() {
        block(-45.0 + i as f32 * 30.0, 70.0, 24.0, 24.0, (height - 0.5).max(1.0));
    }
    // A low kerb that can be walked up, and a wall that cannot.
    block(60.0, 20.0, 30.0, 12.0, 0.4);
    block(-70.0, 10.0, 6.0, 50.0, 12.0);
    // Pillars for slalom in vehicle form.
    for i in 0..8 {
        block(40.0 + i as f32 * 28.0, -60.0 + if i % 2 == 0 { 9.0 } else { -9.0 }, 5.0, 5.0, 8.0);
    }
    // A climbing wall the height of the first one climbed in the recording (43.3), and two
    // blocks side by side in line, which the climb takes as one wall. Anything a jump high
    // (6) or more can be climbed: the kerb cannot, every other block can.
    block(60.0, 95.0, 60.0, 10.0, 43.3);
    block(105.0, 95.0, 30.0, 10.0, 25.0);
    // A few building-sized blocks for scale.
    block(-120.0, 120.0, 40.0, 60.0, 30.0);
    block(130.0, 140.0, 50.0, 40.0, 45.0);
    block(0.0, 220.0, 80.0, 30.0, 22.0);
    // An avenue of gate posts straight back from the start, every 100 out to 550: a long
    // straight for the turbo, and something standing at each distance through the level's
    // fog (164 to 700 in `us_west_town`).
    for i in 0..5 {
        for side in [-20.0, 20.0] {
            block(side, -150.0 - i as f32 * 100.0, 6.0, 6.0, 20.0);
        }
    }
    // The showcase pieces, behind and to the left of the start.
    // A real continuous gentle slope replaces the staircase; its deck still has a drop.
    block(-60.0, -100.0, 16.0, 20.0, 5.0);
    // A stepped tower, each tier a climb above the last: three climbs in a row to the top.
    for (side, height) in [(40.0, 12.0), (28.0, 24.0), (16.0, 36.0)] {
        block(-130.0, -70.0, side, side, height);
    }
    // A gate over the head of the avenue: two piers and a beam off the ground, to pass
    // under in either form.
    for side in [-20.0, 20.0] {
        block(side, -100.0, 6.0, 6.0, 16.0);
    }
    boxes.push(Box3 { min: Vec3::new(-23.0, -103.0, 16.0), max: Vec3::new(23.0, -97.0, 20.0) });
    boxes.extend(climbing_building());
    let mut ramps = vec![
        // Gentle slope to the existing deck, approached along -y from (-60, -40).
        Ramp { min: Vec2::new(-68.0, -90.0), max: Vec2::new(-52.0, -40.0),
            base: 0.0, height: 5.0, slope: Vec2::new(0.0, -0.1) },
        // Orange launch ramp next to it: 14 high, with an immediate open drop at the lip.
        Ramp { min: Vec2::new(-106.0, -90.0), max: Vec2::new(-84.0, -40.0),
            base: 0.0, height: 14.0, slope: Vec2::new(0.0, -0.28) },
        // An up-and-over hill to the right: matched seams, 8 high, no step at either foot.
        Ramp { min: Vec2::new(68.0, -75.0), max: Vec2::new(96.0, -35.0),
            base: 0.0, height: 8.0, slope: Vec2::new(0.0, -0.2) },
        Ramp { min: Vec2::new(68.0, -115.0), max: Vec2::new(96.0, -75.0),
            base: 0.0, height: 0.0, slope: Vec2::new(0.0, 0.2) },
        // A broad bank for testing body roll: enter from its low west edge, then turn -y.
        Ramp { min: Vec2::new(125.0, -130.0), max: Vec2::new(165.0, -35.0),
            base: 0.0, height: 0.0, slope: Vec2::new(0.15, 0.0) },
    ];
    // [stand-in] Suspension courses to the right of the spawn, approached along +y.
    // Broad, progressively taller rollers load both axles; the split lane loads the
    // left/right wheels at different times. Each bump meets the ground at both ends.
    for (y, height) in [(4.0, 0.4), (22.0, 0.7), (40.0, 1.1)] {
        suspension_bump(&mut ramps, Vec2::new(90.0, y), 16.0, 12.0, height);
    }
    for y in [4.0, 24.0, 44.0] {
        suspension_bump(&mut ramps, Vec2::new(114.0, y), 8.0, 12.0, 0.65);
        suspension_bump(&mut ramps, Vec2::new(122.0, y + 10.0), 8.0, 12.0, 0.65);
    }
    Arena { boxes, ramps, targets: Vec::new() }
}

/// [stand-in] "Forked Crown": an asymmetric U around an open courtyard, with a
/// switchback silhouette, usable roof terraces and an elevated front bridge.
/// These are authored fixtures, not inferred original-game building dimensions.
/// Keep the solids shared by rendering, collision, rays and Arena's ledge queries.
fn climbing_building() -> Vec<Box3> {
    let solid = |min, max| Box3 { min: Vec3::from_array(min), max: Vec3::from_array(max) };
    vec![
        // Four pull-ups along +y: 12, 26, 40, 54. Each setback leaves an 8m landing.
        solid([202.0, 100.0, 0.0], [240.0, 144.0, 12.0]),
        solid([206.0, 108.0, 0.0], [236.0, 144.0, 26.0]),
        solid([210.0, 116.0, 0.0], [232.0, 144.0, 40.0]),
        solid([184.0, 132.0, 0.0], [258.0, 144.0, 54.0]),
        // Unequal wings: long faces for up/down/shimmy, recessed inside corners
        // and exposed outside corners. The open court is 38m wide.
        solid([184.0, 78.0, 0.0], [202.0, 132.0, 26.0]),
        solid([240.0, 78.0, 0.0], [258.0, 132.0, 40.0]),
        // Projecting buttresses break up the outer facades, leaving short faces
        // and additional landings below and above the adjoining wing's roof.
        solid([178.0, 90.0, 0.0], [184.0, 100.0, 18.0]),
        solid([178.0, 112.0, 0.0], [184.0, 122.0, 34.0]),
        solid([258.0, 94.0, 0.0], [264.0, 104.0, 24.0]),
        // Skybridge: genuine empty space beneath, including for collision/rays.
        // Arena's ground-level ledge query must not offer its floating underside.
        solid([202.0, 80.0, 26.0], [240.0, 88.0, 30.0]),
        // A final narrow 8m wall on the crown, reached from the top terrace.
        solid([218.0, 136.0, 54.0], [224.0, 144.0, 62.0]),
    ]
}

/// [stand-in] Colour identifies each test surface; the 10m grid shows height/distance.
fn climbing_building_colour(index: usize) -> Color {
    let colours = [
        (0.28, 0.65, 0.72), (0.25, 0.52, 0.68), (0.33, 0.42, 0.64), (0.56, 0.43, 0.68),
        (0.32, 0.60, 0.62), (0.60, 0.45, 0.65),
        (0.92, 0.59, 0.26), (0.92, 0.59, 0.26), (0.92, 0.59, 0.26),
        (0.93, 0.73, 0.31), (0.93, 0.73, 0.31),
    ];
    let (r, g, b) = colours[index];
    Color::srgb(r, g, b)
}

#[cfg(test)]
mod climbing_building_tests {
    use super::*;
    use tf2_core::camera::Sight;
    use tf2_core::climb;

    fn building() -> Arena {
        Arena { boxes: climbing_building(), ..Default::default() }
    }

    fn wall(arena: &Arena, pos: Vec3, facing: Vec2) -> climb::Wall {
        // [data] Installed Bumblebee robot radius/height and standing jump height.
        climb::find(arena, pos, facing, 2.0, 5.4, 6.0).expect("fixture offers a climbable wall")
    }

    #[test]
    fn courtyard_route_has_four_climbs_from_supported_landings() {
        let arena = building();
        for (y, bottom, top) in [(97.5, 0.0, 12.0), (105.5, 12.0, 26.0),
            (113.5, 26.0, 40.0), (129.5, 40.0, 54.0)] {
            assert_eq!(arena.floor(221.0, y, bottom), bottom);
            let wall = wall(&arena, Vec3::new(221.0, y, bottom), Vec2::Y);
            assert_eq!(wall.origin.z, bottom);
            assert_eq!(wall.height, top - bottom);
            assert!(wall.width >= 22.0, "landing has room for a sideways approach");
        }
        let crown = wall(&arena, Vec3::new(221.0, 133.5, 54.0), Vec2::Y);
        assert_eq!((crown.height, crown.width), (8.0, 6.0));
    }

    #[test]
    fn recessed_wings_and_projecting_column_offer_distinct_faces() {
        let arena = building();
        let left = wall(&arena, Vec3::new(204.5, 94.0, 0.0), -Vec2::X);
        let right = wall(&arena, Vec3::new(237.5, 94.0, 0.0), Vec2::X);
        assert_eq!((left.height, right.height), (26.0, 40.0));
        assert_eq!(left.inward(), -right.inward());
        let column = wall(&arena, Vec3::new(175.5, 95.0, 0.0), Vec2::X);
        assert_eq!((column.height, column.width), (18.0, 10.0));
        // The front outer corner ends at x=202 rather than crossing the empty court.
        let front = wall(&arena, Vec3::new(199.5, 75.5, 0.0), Vec2::Y);
        assert_eq!(front.width, 18.0);
    }

    #[test]
    fn bridge_has_open_ground_below_and_a_solid_ceiling() {
        let arena = building();
        assert_eq!(arena.floor(221.0, 84.0, 0.0), 0.0);
        let mut pos = Vec3::new(221.0, 84.0, 0.0);
        assert!(!arena.push_out(&mut pos, 2.0, 5.4));
        let ceiling = arena.sight(pos, pos + Vec3::Z * 35.0).expect("bridge underside blocks rays");
        assert_eq!(ceiling.point.z, 26.0);
        assert_eq!(ceiling.normal, -Vec3::Z);
        assert_eq!(arena.floor(221.0, 84.0, 30.0), 30.0);
        assert!(climb::find(&arena, Vec3::new(221.0, 77.5, 0.0), Vec2::Y, 2.0, 5.4, 6.0).is_none(),
            "a floating bridge must not invent a climbable wall across the entrance");
    }
}

/// [stand-in] Two joined wedges: no vertical step at the entry, crest or exit.
fn suspension_bump(ramps: &mut Vec<Ramp>, min: Vec2, width: f32, length: f32, height: f32) {
    let half = length * 0.5;
    ramps.push(Ramp { min, max: min + Vec2::new(width, half),
        base: 0.0, height: 0.0, slope: Vec2::new(0.0, height / half) });
    let crest = min + Vec2::new(0.0, half);
    ramps.push(Ramp { min: crest, max: min + Vec2::new(width, length),
        base: 0.0, height, slope: Vec2::new(0.0, -height / half) });
}

/// The level whose `Environment` the arena borrows: its sun, ambient, fog, bloom and
/// colour filter. [data: that level's] [stand-in: that the arena is that level]
pub const LEVEL: &str = "us_west_town";

static LOOK: OnceLock<Option<Environment>> = OnceLock::new();

/// Read once at the start (`main`); `None` when the level's pack is not there.
pub fn set_look(look: Option<Environment>) {
    let _ = LOOK.set(look);
}

pub fn look() -> Option<&'static Environment> {
    LOOK.get().and_then(Option::as_ref)
}

/// The way to the sun, in Bevy's space: the level's `shadowLight`.
pub fn to_sun() -> Vec3 {
    look().map_or_else(|| Quat::from_euler(EulerRot::YXZ, -0.9, -0.85, 0.0) * Vec3::Z, |e| game_to_bevy(e.to_sun))
}

pub fn sun_turn() -> Quat {
    Transform::IDENTITY.looking_to(-to_sun(), Vec3::Y).rotation
}

/// The sun's colour (the light's `colour` x `range`) and the ambient colour, as the
/// level's data has them.
pub fn level_light() -> (Vec3, Vec3) {
    look().map_or((Vec3::new(0.85, 0.82, 0.76), Vec3::new(0.42, 0.45, 0.5)), |e| (e.sun_colour * e.sun_range, e.ambient))
}

/// What the camera gets of the level's environment.
/// [stand-in] Bevy's own bloom, fog and colour grading stand for the game's passes
/// (bloom extract 724450, fog 724890, colour filter 7210f0; `notes\shaders.md` "The
/// environment"), fed the level's numbers:
/// - fog: the level's colour, from `fogHotspot` to `fogRange`, at most `fogFalloff`
///   percent [assumed: what the three numbers mean];
/// - bloom: what is brighter than `bloomThreshold`, times `bloomBrightness`, added
///   [assumed: the threshold is on Bevy's lit values; the tint and saturation unused];
/// - colour grading: `colorFilterSaturation` and `Brightness` over 127.5
///   [assumed: 127.5 is "no change"]. `colorFilterContrast` is NOT applied (Bevy's
///   contrast at 150 / 127.5 turned every shadow black: it is not the same control).
///   The lookup table (`colorFilterLUT`), the iris
///   (exposure that adapts), depth of field, SSAO and the radial blur are not ported.
pub fn camera_look() -> Option<(Hdr, Bloom, DistanceFog, ColorGrading)> {
    let e = look()?;
    let colour = |c: Vec3, a: f32| Color::srgba(c.x, c.y, c.z, a);
    let mut grading = ColorGrading::default();
    grading.global.post_saturation = e.filter_saturation / 127.5;
    grading.global.exposure = (e.filter_brightness / 127.5).max(1e-3).log2();
    Some((
        Hdr,
        Bloom {
            intensity: e.bloom_brightness,
            composite_mode: BloomCompositeMode::Additive,
            prefilter: BloomPrefilter { threshold: e.bloom_threshold, threshold_softness: 0.0 },
            ..Bloom::NATURAL
        },
        DistanceFog {
            color: colour(e.fog_colour, (e.fog_falloff / 100.0).clamp(0.0, 1.0)),
            falloff: FogFalloff::Linear { start: e.fog_hotspot, end: e.fog_range.max(e.fog_hotspot + 1.0) },
            ..default()
        },
        grading,
    ))
}

fn grid_image(cell_shade: u8) -> Image {
    const SIZE: usize = 256;
    let mut data = Vec::with_capacity(SIZE * SIZE * 4);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let line = x < 3 || y < 3;
            let shade: u8 = if line { 150 } else { cell_shade + ((x / 32 + y / 32) % 2) as u8 * 6 };
            data.extend_from_slice(&[shade, shade, shade.saturating_add(4), 255]);
        }
    }
    let size = Extent3d { width: SIZE as u32, height: SIZE as u32, depth_or_array_layers: 1 };
    let mut image =
        Image::new(size, TextureDimension::D2, data, TextureFormat::Rgba8UnormSrgb, RenderAssetUsages::RENDER_WORLD);
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        anisotropy_clamp: 16,
        ..default()
    });
    image
}

/// [stand-in] Metric grid on every building face, including roofs, for climb tests.
fn building_mesh(size: Vec3) -> Mesh {
    use bevy::mesh::VertexAttributeValues;
    let mut mesh = Mesh::from(Cuboid::new(size.x, size.z, size.y));
    if let (Some(VertexAttributeValues::Float32x3(positions)),
        Some(VertexAttributeValues::Float32x3(normals))) =
        (mesh.attribute(Mesh::ATTRIBUTE_POSITION), mesh.attribute(Mesh::ATTRIBUTE_NORMAL)) {
        let uvs: Vec<[f32; 2]> = positions.iter().zip(normals).map(|(p, n)| {
            let (u, v) = if n[0].abs() > 0.5 { (p[2], p[1]) }
                else if n[1].abs() > 0.5 { (p[0], p[2]) }
                else { (p[0], p[1]) };
            [u / GRID, v / GRID]
        }).collect();
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    }
    mesh
}

/// Closed wedge with separate face normals; render exactly the collision top plane.
fn ramp_mesh(ramp: &Ramp) -> Mesh {
    let xy = [ramp.min, Vec2::new(ramp.max.x, ramp.min.y), ramp.max, Vec2::new(ramp.min.x, ramp.max.y)];
    let top = xy.map(|p| game_to_bevy(p.extend(ramp.top(p))));
    let bottom = xy.map(|p| game_to_bevy(p.extend(ramp.base)));
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut indices = Vec::new();
    let mut face = |corners: [Vec3; 4]| {
        let normal = (corners[1] - corners[0]).cross(corners[2] - corners[0]).normalize_or_zero();
        if normal == Vec3::ZERO { return; }
        let start = positions.len() as u32;
        let across = (corners[1] - corners[0]).normalize();
        let up = normal.cross(across);
        for p in corners {
            positions.push(p.to_array());
            normals.push(normal.to_array());
            uvs.push([p.dot(across) / GRID, p.dot(up) / GRID]);
        }
        indices.extend_from_slice(&[start, start + 1, start + 2, start, start + 2, start + 3]);
    };
    face(top);
    face([bottom[3], bottom[2], bottom[1], bottom[0]]);
    for i in 0..4 {
        let next = (i + 1) % 4;
        // At a zero-height corner the first triangle may collapse; keep the other one.
        let corners = [bottom[i], bottom[next], top[next], top[i]];
        if (corners[1] - corners[0]).cross(corners[2] - corners[0]).length_squared() < 1e-8 {
            face([corners[2], corners[3], corners[0], corners[1]]);
        } else {
            face(corners);
        }
    }
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD);
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

pub fn spawn(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<crate::ssd::Material>>,
    ssd: Res<crate::ssd::Gpu>,
    mut images: ResMut<Assets<Image>>,
    arena: Res<ArenaRes>,
) {
    commands.insert_resource(ClearColor(Color::srgb(0.55, 0.68, 0.82)));
    // The level's sun and ambient colours, each at its brightest channel's full, with
    // Bevy's own strengths. [data: the colours and the sun's direction] [stand-in: how
    // bright each is; the level's four other lights (bounce, fill, high, back) are not
    // put up]
    let tint = |c: Vec3| {
        let c = c / c.max_element().max(1e-5);
        Color::linear_rgb(c.x, c.y, c.z)
    };
    let (sun, ambient) = look().map_or((Color::WHITE, Color::srgb(0.8, 0.87, 1.0)), |e| (tint(e.sun_colour), tint(e.ambient)));
    commands.insert_resource(GlobalAmbientLight { color: ambient, brightness: 450.0, ..default() });
    commands.spawn((
        DirectionalLight { illuminance: 14_000.0, color: sun, shadow_maps_enabled: true, ..default() },
        Transform::from_rotation(sun_turn()),
    ));

    let grid = images.add(grid_image(92));
    // [stand-in] Pale wall grid keeps route colours readable in the courtyard shade.
    let building_grid = images.add(grid_image(225));
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(GROUND_SIZE, GROUND_SIZE))),
        MeshMaterial3d(materials.add(crate::ssd::Material { base:StandardMaterial {
            base_color_texture: Some(grid.clone()),
            uv_transform: Affine2::from_scale(Vec2::splat(GROUND_SIZE / GRID)),
            perceptual_roughness: 0.95,
            ..default()
        },extension:ssd.extension.clone() })),
    ));

    let palette = [Color::srgb(0.62, 0.6, 0.56), Color::srgb(0.5, 0.55, 0.6), Color::srgb(0.66, 0.56, 0.48)];
    if std::env::args().any(|a|a=="--camera-fade-test") {
        // [stand-in] Optional nonphysical camera-fade fixture, absent from the arena
        // unless requested. Its alpha and recovery are the original camera rules.
        let bounds=Box3 {min:Vec3::new(-4.0,-5.5,0.0),max:Vec3::new(4.0,-4.5,8.0)};
        commands.spawn((Mesh3d(meshes.add(Cuboid::new(8.0,8.0,1.0))),
            MeshMaterial3d(materials.add(crate::ssd::Material {base:StandardMaterial {
                base_color:Color::srgb(0.9,0.35,0.1),..default()
            },extension:ssd.extension.clone()})),
            Transform::from_translation(game_to_bevy((bounds.min+bounds.max)*0.5)),
            crate::camera::Fadeable {bounds,alpha:1.0,alpha_mode:AlphaMode::Opaque}));
    }
    let building = climbing_building();
    for (i, b) in arena.0.boxes.iter().enumerate() {
        let size = b.max - b.min;
        let centre = (b.min + b.max) / 2.0;
        let building_part = building.iter().position(|part| part.min == b.min && part.max == b.max);
        commands.spawn((
            Mesh3d(meshes.add(if building_part.is_some() { building_mesh(size) }
                else { Mesh::from(Cuboid::new(size.x, size.z, size.y)) })),
            MeshMaterial3d(materials.add(crate::ssd::Material { base:StandardMaterial {
                base_color: building_part.map(climbing_building_colour).unwrap_or(palette[i % palette.len()]),
                base_color_texture: building_part.map(|_| building_grid.clone()),
                perceptual_roughness: 0.9,
                ..default()
            },extension:ssd.extension.clone() })),
            Transform::from_translation(game_to_bevy(centre)),
        ));
    }
    let ramp_colours = [Color::srgb(0.35, 0.58, 0.75), Color::srgb(0.95, 0.5, 0.15),
        Color::srgb(0.4, 0.7, 0.45), Color::srgb(0.4, 0.7, 0.45), Color::srgb(0.72, 0.52, 0.78)];
    for (i, ramp) in arena.0.ramps.iter().enumerate() {
        commands.spawn((
            Mesh3d(meshes.add(ramp_mesh(ramp))),
            MeshMaterial3d(materials.add(crate::ssd::Material { base: StandardMaterial {
                base_color: ramp_colours.get(i).copied().unwrap_or_else(|| {
                    if ramp.min.x < 110.0 { Color::srgb(0.25, 0.75, 0.8) }
                    else { Color::srgb(0.9, 0.7, 0.25) }
                }),
                base_color_texture: Some(grid.clone()),
                perceptual_roughness: 0.9,
                ..default()
            }, extension: ssd.extension.clone() })),
        ));
    }
}
