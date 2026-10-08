// The original's unlit surface shaders for the meshes that glow, written again from the
// compiled ones in `shaders.lsa` (`work\shaders\asm`): `emissive` (pixel 094420: his eyes,
// the weapon's lights) and `dull_fx_emissive` (vertex 043aa0, pixel 043f90: the ground
// punches' shock rings).
//
// [game]: the texture's place is `X u + Y v + Z + T time`; the colour is the texel times
// `Diffuse_Color` times the object's own colour; `dull_fx_emissive` adds
// `(1 - |V.N|)^Edge_Power * LightColor * Edge_Color` and fades the alpha between
// `inTransFalloff` where the surface faces the camera and `outTransFalloff` where it is
// edge on; the alpha is the texel's times `Diffuse_Texture1_Opacity` times the colour's
// times `AlphaBoost`.
// [assumed]: arithmetic on stored texture values (as `surface.wgsl`); `AlphaBoost` 1.
// [stand-in]: `LightColor` (the level's). Not ported: fog, `IrisParams` (exposure).

#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    mesh_view_bindings as view_bindings,
}

const ADD: u32 = 1u;
const EDGE: u32 = 2u;

struct Glow {
    // `Diffuse_Color` times the object's colour
    colour: vec4<f32>,
    // xy `Diffuse_Texture1_X`, zw `Diffuse_Texture1_Y`
    uv_xy: vec4<f32>,
    // xy `Diffuse_Texture1_Z`, zw `Diffuse_Texture1_T`
    uv_zt: vec4<f32>,
    // rgb `Edge_Color`, w `Edge_Power`
    edge: vec4<f32>,
    // x `inTransFalloff`, y `outTransFalloff`, z `Diffuse_Texture1_Opacity`, w `AlphaBoost`
    falloff: vec4<f32>,
    // rgb `LightColor`
    light: vec4<f32>,
    flags: vec4<u32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> glow: Glow;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var diffuse_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var diffuse_sampler: sampler;

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    return pow(c, vec3<f32>(2.2));
}

@fragment
fn fragment(in: VertexOutput) -> FragmentOutput {
    var uv = vec2<f32>(0.0);
#ifdef VERTEX_UVS_A
    uv = glow.uv_xy.xy * in.uv.x + glow.uv_xy.zw * in.uv.y + glow.uv_zt.xy + glow.uv_zt.zw * view_bindings::globals.time;
#endif
    var texel = textureSample(diffuse_texture, diffuse_sampler, uv);
    texel.a *= glow.falloff.z;
    texel *= glow.colour;
    var colour = texel.rgb;
    var alpha = texel.a;

    if (glow.flags.x & EDGE) != 0u {
        let n = normalize(in.world_normal);
        let v = normalize(view_bindings::view.world_position - in.world_position.xyz);
        let rim = pow(saturate(1.0 - abs(dot(v, n))), glow.edge.w);
        colour += rim * glow.light.rgb * glow.edge.rgb;
        // How far the normal lies across the camera's line: 0 facing it, 1 edge on.
        let back = normalize(view_bindings::view.world_from_view[2].xyz);
        let along = dot(n, back);
        let across = sqrt(max(1.0 - along * along, 0.0));
        alpha *= saturate(across * (glow.falloff.y - glow.falloff.x) + glow.falloff.x);
    }
    alpha = saturate(alpha * glow.falloff.w);

    let lit = to_linear(saturate(colour));
    var out: FragmentOutput;
    if (glow.flags.x & ADD) != 0u {
        out.color = vec4<f32>(lit * alpha, 0.0);
    } else {
        out.color = vec4<f32>(lit, alpha);
    }
    return out;
}
