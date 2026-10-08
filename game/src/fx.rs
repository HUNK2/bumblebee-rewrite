//! The original's shaders for what glows: particles (`fx.wgsl`, from the shaders compiled
//! into the executable) and the unlit mesh techniques `emissive` and `dull_fx_emissive`
//! (`glow.wgsl`, from `shaders.lsa`). Each shader file says what in it is the game's.

use bevy::asset::uuid_handle;
use bevy::prelude::*;
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::render::render_resource::{AsBindGroup, Face, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError};
use bevy::shader::{Shader, ShaderRef};
use tf2_core::particles::{Blend, Fog, Renderer};

use crate::formats::hash::crc32;
use crate::formats::model;

const FX_SHADER: Handle<Shader> = uuid_handle!("0d6a3f1e-52c7-4b8e-8e1a-7c41f3a9d2b6");
const GLOW_SHADER: Handle<Shader> = uuid_handle!("b7e2c4a9-18f3-4d65-9c0b-5a2e6d7f8c13");
const WARP_SHADER: Handle<Shader> = uuid_handle!("5c1d9e72-3a4b-4f60-b8d1-2e7a9c0f4b35");

const ADD: u32 = 1;
const FEATHER: u32 = 2;
const LIT: u32 = 4;
const EDGE: u32 = 2;

/// [assumed] the original's near plane, which sets how deep the feathered particles'
/// fade against the scene is; not read. Ours.
const NEAR: f32 = 0.2;
/// [stand-in] `LightColor` for the shock rings' edge light (the level's in the original).
const LIGHT: Vec3 = Vec3::new(1.0, 0.97, 0.9);

#[derive(Clone, ShaderType)]
pub struct FxValues {
    flags: UVec4,
    values: Vec4,
    /// The lit shaders' constants: `DIR_TO_LIGHT`, `DIFFUSE_COLOR`, `AMBIENT_COLOR` and
    /// `INV_COLOR_COEFF` (its w the back-light).
    to_light: Vec4,
    diffuse: Vec4,
    ambient: Vec4,
    inverse: Vec4,
    fog: Vec4,
    fog_colour: Vec4,
}


/// A particle renderer's material.
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct Fx {
    #[uniform(0)]
    values: FxValues,
    #[texture(1)]
    #[sampler(2)]
    picture: Option<Handle<Image>>,
    alpha_mode: AlphaMode,
}

impl Material for Fx {
    fn vertex_shader() -> ShaderRef {
        FX_SHADER.into()
    }

    fn fragment_shader() -> ShaderRef {
        FX_SHADER.into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        self.alpha_mode
    }

    fn enable_prepass() -> bool {
        false
    }

    fn enable_shadows() -> bool {
        false
    }
}

impl Fx {
    /// [stand-in]: `Subtract` is drawn as `Alpha` and `Opaque` as `Alpha`; no effect of
    /// his has either.
    pub fn new(renderer: &Renderer, picture: Option<Handle<Image>>) -> Self {
        let add = renderer.blend() == Blend::Add;
        let flags = if add { ADD } else { 0 } | if renderer.z_feather > 0.0 { FEATHER } else { 0 } | if renderer.lit() { LIT } else { 0 };
        // `FUN_00973310`: diffuse = `DiffuseCoeff` x the light's colour, ambient =
        // `AmbientCoeff` x the ambient colour, and the "inverse" colour = 1 less both
        // coefficients (what of the particle no light changes), with `BackLitCoeff` as
        // its w. [game: 00973310 uses 005431f0 to copy RGB with an explicit zero w]
        let (diffuse, ambient) = (Vec3::from(renderer.diffuse), Vec3::from(renderer.ambient));
        // The level's light: its sun's colour and its ambient colour. [data: the level
        // the arena borrows its environment from] [assumed: the sun is the light the
        // particles take; the render node's `+0xc0` .. `+0xe0` are filled by code not read]
        let (level_diffuse, level_ambient) = crate::world::level_light();
        // [game] Fog is disabled for BlendMode 1/2. The arena retains its existing
        // [assumed] fogHotspot/range/falloff mapping until the Environment transfer
        // into shared game-state +194..1ac is verified (notes/status.md).
        let (fog, fog_colour) = crate::world::look().filter(|_| renderer.fogged()).map_or(
            (Fog::default(), Vec3::ZERO),
            |e| (Fog::new(e.fog_hotspot, e.fog_range, 0.0, e.fog_falloff * 0.01), e.fog_colour),
        );
        let [fade_slope, fade_intercept] = renderer.fade_constants();
        Fx {
            values: FxValues {
                flags: UVec4::new(flags, 0, 0, 0),
                values: Vec4::new(NEAR, fade_slope, fade_intercept, 0.0),
                to_light: crate::world::to_sun().extend(0.0),
                diffuse: (diffuse * level_diffuse).extend(0.0),
                ambient: (ambient * level_ambient).extend(0.0),
                inverse: (Vec3::ONE - diffuse - ambient).extend(renderer.back_lit),
                fog: Vec4::from_array(fog.0),
                fog_colour: fog_colour.extend(0.0),
            },
            picture,
            alpha_mode: if add { AlphaMode::Add } else { AlphaMode::Blend },
        }
    }
}

#[derive(Clone, ShaderType)]
pub struct GlowValues {
    colour: Vec4,
    uv_xy: Vec4,
    uv_zt: Vec4,
    edge: Vec4,
    falloff: Vec4,
    light: Vec4,
    flags: UVec4,
}

/// A mesh of the `emissive` or `dull_fx_emissive` technique.
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct Glow {
    #[uniform(0)]
    values: GlowValues,
    #[texture(1)]
    #[sampler(2)]
    diffuse: Option<Handle<Image>>,
    alpha_mode: AlphaMode,
    /// `Diffuse_Color`, kept so the object's colour can be laid over it again.
    diffuse_color: Vec4,
}

impl Material for Glow {
    fn fragment_shader() -> ShaderRef {
        GLOW_SHADER.into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        self.alpha_mode
    }

    fn enable_shadows() -> bool {
        false
    }

    /// As `Surface::specialize`: the depth prepass must drop the faces the main pass drops.
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

impl Glow {
    /// The object's own colour's alpha (`ObjectDiffuseColor`, which the vertex shader
    /// multiplies into the material's): what fades an effect's object out.
    pub fn set_object_alpha(&mut self, alpha: f32) {
        self.values.colour = self.diffuse_color * Vec4::new(1.0, 1.0, 1.0, alpha);
    }

    /// The colour texture a material of these techniques wants, or `None` for another
    /// technique.
    pub fn texture(source: &model::Material) -> Option<Option<u32>> {
        edge(source.technique).map(|_| source.texture("diffuse_texture1_texture"))
    }

    pub fn build(source: &model::Material, diffuse: Option<Handle<Image>>, alpha_mode: AlphaMode) -> Option<Self> {
        let edge = edge(source.technique)?;
        let pair = |name: &str, or: [f32; 2]| source.vector(name).map_or(or, |v| [v[0], v[1]]);
        let ([xx, xy], [yx, yy]) = (pair("diffuse_texture1_x", [1.0, 0.0]), pair("diffuse_texture1_y", [0.0, 1.0]));
        let ([zx, zy], [tx, ty]) = (pair("diffuse_texture1_z", [0.0; 2]), pair("diffuse_texture1_t", [0.0; 2]));
        let number = |name: &str, or: f32| source.number(name).unwrap_or(or);
        let diffuse_color = source.vector("diffuse_color").map_or(Vec4::ONE, Vec4::from);
        let add = alpha_mode == AlphaMode::Add;
        let values = GlowValues {
            colour: diffuse_color,
            uv_xy: Vec4::new(xx, xy, yx, yy),
            uv_zt: Vec4::new(zx, zy, tx, ty),
            edge: source.vector("edge_color").map_or(Vec3::ZERO, |v| Vec3::new(v[0], v[1], v[2])).extend(number("edge_power", 1.0)),
            falloff: Vec4::new(number("intransfalloff", 1.0), number("outtransfalloff", 1.0), number("diffuse_texture1_opacity", 1.0), 1.0),
            light: LIGHT.extend(0.0),
            flags: UVec4::new(if add { ADD } else { 0 } | if edge { EDGE } else { 0 }, 0, 0, 0),
        };
        Some(Glow { values, diffuse, alpha_mode, diffuse_color })
    }
}

/// Whether a technique is one of the two, and if so whether it has the edge light and the
/// facing fade.
fn edge(technique: u32) -> Option<bool> {
    const EMISSIVE: u32 = crc32(b"emissive");
    const DULL_FX: u32 = crc32(b"dull_fx_emissive");
    match technique {
        EMISSIVE => Some(false),
        DULL_FX => Some(true),
        _ => None,
    }
}

/// A particle renderer that bends the picture behind it (`Dstr`, `IsShimmer`):
/// `warp.wgsl`.
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct Warp {
    #[texture(0)]
    #[sampler(1)]
    pub map: Option<Handle<Image>>,
}

impl Material for Warp {
    fn fragment_shader() -> ShaderRef {
        WARP_SHADER.into()
    }

    /// Drawn after the solid scene with a copy of it at hand (Bevy's phase for glass).
    fn reads_view_transmission_texture(&self) -> bool {
        true
    }

    fn enable_prepass() -> bool {
        false
    }

    fn enable_shadows() -> bool {
        false
    }

    /// It hides nothing: what is drawn after it must not be cut out by its depth.
    fn specialize(
        _: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _: &MeshVertexBufferLayoutRef,
        _: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_write_enabled = Some(false);
        }
        descriptor.primitive.cull_mode = None;
        Ok(())
    }
}

pub fn plugin(app: &mut App) {
    app.add_plugins((MaterialPlugin::<Fx>::default(), MaterialPlugin::<Glow>::default(), MaterialPlugin::<Warp>::default()));
    app.world_mut()
        .resource_mut::<Assets<Shader>>()
        .insert(&WARP_SHADER, Shader::from_wgsl(include_str!("warp.wgsl"), "warp.wgsl"))
        .expect("the distortion shader's handle");
    let mut shaders = app.world_mut().resource_mut::<Assets<Shader>>();
    shaders.insert(&FX_SHADER, Shader::from_wgsl(include_str!("fx.wgsl"), "fx.wgsl")).expect("the particle shader's handle");
    shaders.insert(&GLOW_SHADER, Shader::from_wgsl(include_str!("glow.wgsl"), "glow.wgsl")).expect("the glow shader's handle");
}
