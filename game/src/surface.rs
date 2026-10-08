//! The original's surface shaders for his materials: the car paint, the two metals, the
//! rubber and the glass. `surface.wgsl` has the arithmetic and what in it is the game's;
//! this builds a material from a pack's `MAT` entry.
//!
//! [stand-in]: the light's colour, the ambient and what the paint reflects are ours: the
//! original takes them from the level, which is not read.

use bevy::asset::uuid_handle;
use bevy::prelude::*;
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::render::render_resource::{AsBindGroup, Face, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError};
use bevy::shader::{Shader, ShaderRef};

use crate::formats::hash::crc32;
use crate::formats::model;

const SHADER: Handle<Shader> = uuid_handle!("6b1f0c52-3d0e-4c1b-9a53-2f6d1e7a90b4");

const PAINT: u32 = 1;
const EDGE: u32 = 2;
const REFLECT_CUBE: u32 = 4;
const REFLECT_SPHERE: u32 = 8;
const FALLOFF: u32 = 16;
const NORMAL_MAP: u32 = 32;
const VERTEX_COLOR: u32 = 64;

/// [stand-in] the sun's colour, in the original's own scale (a lit texel is its value
/// times this plus the ambient, cut off at 1).
const LIGHT: Vec3 = Vec3::new(1.0, 0.97, 0.9);
/// [stand-in] the ambient on a surface facing up, and on one facing down.
const AMBIENT_SKY: Vec3 = Vec3::new(0.42, 0.46, 0.52);
const AMBIENT_GROUND: Vec3 = Vec3::new(0.26, 0.25, 0.23);
/// The id of his reflection cube map (`outputcube.dds`) in his pack.
const HIS_CUBE: u32 = 0x224f_6431;
/// [stand-in] the sky the paint reflects where that cube is not found: the arena's clear
/// colour.
const SKY_SEEN: Vec3 = Vec3::new(0.55, 0.68, 0.82);

#[derive(Clone, ShaderType)]
pub struct SurfaceValues {
    diffuse_color: Vec4,
    specular: Vec4,
    reflection: Vec4,
    edge: Vec4,
    paint0: Vec4,
    paint1: Vec4,
    paint2: Vec4,
    light: Vec4,
    sky: Vec4,
    ground: Vec4,
    sky_seen: Vec4,
    uv_x: Vec4,
    uv_y: Vec4,
    uv_z: Vec4,
    uv_t: Vec4,
    flags: UVec4,
}

#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct Surface {
    #[uniform(0)]
    values: SurfaceValues,
    #[texture(1)]
    #[sampler(2)]
    diffuse: Option<Handle<Image>>,
    #[texture(3)]
    #[sampler(4)]
    normal: Option<Handle<Image>>,
    #[texture(5)]
    #[sampler(6)]
    specular: Option<Handle<Image>>,
    #[texture(7, dimension = "cube")]
    #[sampler(8)]
    reflection: Option<Handle<Image>>,
    alpha_mode: AlphaMode,
}

impl Material for Surface {
    fn fragment_shader() -> ShaderRef {
        SHADER.into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        self.alpha_mode
    }

    /// Bevy's depth prepass draws both faces of a custom material's mesh while the main
    /// pass draws the front ones: the back of a panel then left its depth with no colour,
    /// a hole in the sky's colour over whatever was behind (the "bits of blue" the user
    /// saw). Both passes drop the back faces now. [assumed: the original does not draw
    /// his panels from behind either; its cull state is not read]
    fn specialize(
        _: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _: &MeshVertexBufferLayoutRef,
        _: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        descriptor.primitive.cull_mode = Some(Face::Back);
        Ok(())
    }
}

pub fn plugin(app: &mut App) {
    app.add_plugins(MaterialPlugin::<Surface>::default());
    let shader = Shader::from_wgsl(include_str!("surface.wgsl"), "surface.wgsl");
    app.world_mut().resource_mut::<Assets<Shader>>().insert(&SHADER, shader).expect("the surface shader's handle");
}

/// What a technique of the original's does, or `None` for one that is not ported.
fn flags(technique: u32) -> Option<u32> {
    const PAINT_X: u32 = crc32(b"aniso_paint_x");
    const PAINT_CUBE: u32 = crc32(b"aniso_paint");
    const GLASS_PLUS: u32 = crc32(b"glass_plus");
    const CHEAP_PLUS: u32 = crc32(b"cheap_plus");
    const GLASS_CHEAP: u32 = crc32(b"glass_cheap");
    const DULL_VC: u32 = crc32(b"dull_vc");
    match technique {
        PAINT_X => Some(PAINT | EDGE | REFLECT_SPHERE | FALLOFF | NORMAL_MAP),
        PAINT_CUBE => Some(PAINT | REFLECT_CUBE | FALLOFF | NORMAL_MAP),
        GLASS_PLUS => Some(REFLECT_CUBE | NORMAL_MAP),
        CHEAP_PLUS => Some(NORMAL_MAP),
        GLASS_CHEAP => Some(REFLECT_CUBE),
        DULL_VC => Some(VERTEX_COLOR),
        _ => None,
    }
}

/// The ids of the textures a material of a ported technique wants: colour, normal,
/// specular and reflection.
pub fn textures(source: &model::Material) -> Option<[Option<u32>; 4]> {
    flags(source.technique)?;
    Some([
        source.texture("diffuse_texture1_texture"),
        source.texture("normal_map_texture"),
        source.texture("specular_map_texture"),
        // [stand-in] `aniso_paint_x` reflects `RealtimeSphereMap`, the level drawn as the
        // game runs. There is no level here: his own cube map (`outputcube.dds`, which his
        // other materials reflect) stands for it. In another pack the id finds nothing
        // and the flat sky and ground below are used.
        source.texture("reflectionmap_texture").or((flags(source.technique)? & REFLECT_SPHERE != 0).then_some(HIS_CUBE)),
    ])
}

/// The material for a pack's `MAT` entry, its textures already made (in the order of
/// `textures`), or `None` if its technique is not one of the ported ones.
pub fn build(source: &model::Material, images: [Option<Handle<Image>>; 4], alpha_mode: AlphaMode) -> Option<Surface> {
    let mut flags = flags(source.technique)?;
    let [diffuse, normal, specular, reflection] = images;
    if normal.is_none() {
        flags &= !NORMAL_MAP;
    }
    if reflection.is_none() {
        flags &= !REFLECT_CUBE;
    } else if flags & REFLECT_SPHERE != 0 {
        // The stand-in for the sphere map (see `textures`).
        flags = flags & !REFLECT_SPHERE | REFLECT_CUBE;
    }
    let colour = |name: &str, or: f32| source.vector(name).map_or(Vec3::splat(or), |v| Vec3::new(v[0], v[1], v[2]));
    let number = |name: &str, or: f32| source.number(name).unwrap_or(or);
    let values = SurfaceValues {
        diffuse_color: source.vector("diffuse_color").map_or(Vec4::ONE, Vec4::from),
        specular: colour("specular_color", 0.0).extend(number("specular_power", 1.0)),
        reflection: colour("reflection_color", 0.0).extend(number("reflection_falloff", 1.0)),
        edge: colour("edge_color", 0.0).extend(number("edge_power", 1.0)),
        paint0: colour("paint_color0", 0.0).extend(number("paint_pow1", 1.0)),
        paint1: colour("paint_color1", 0.0).extend(number("paint_pow2", 1.0)),
        paint2: colour("paint_color2", 0.0).extend(number("normal_map_heightscale", 1.0)),
        light: LIGHT.extend(number("diffuse_texture1_opacity", 1.0)),
        sky: AMBIENT_SKY.extend(number("reflection_falloffpower", 1.0)),
        ground: AMBIENT_GROUND.extend(1.0),
        sky_seen: SKY_SEEN.extend(0.0),
        uv_x: source.vector("diffuse_texture1_x").map_or(Vec4::X,Vec4::from),
        uv_y: source.vector("diffuse_texture1_y").map_or(Vec4::Y,Vec4::from),
        uv_z: source.vector("diffuse_texture1_z").map_or(Vec4::ZERO,Vec4::from),
        uv_t: source.vector("diffuse_texture1_t").map_or(Vec4::ZERO,Vec4::from),
        flags: UVec4::new(flags, 0, 0, 0),
    };
    Some(Surface { values, diffuse, normal, specular, reflection, alpha_mode })
}
