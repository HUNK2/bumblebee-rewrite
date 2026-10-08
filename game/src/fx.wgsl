// The original's particle shaders, written again from the compiled ones that sit in the
// executable itself (not in `shaders.lsa`; `work\lsa_disasm.py` on the executable writes
// the listings to `work\shaders\exe`): vertex 728708 / 72a648 (frames / plain) and their
// feathered twins 728f30 / 72ac70, pixel 729968 / 72b4c8 and feathered 729db8 / 72b8d0.
//
// [game]: the picture is the frame's texel mixed with the next frame's by the frame's
// fraction, times the colour (tint times the particle's, alpha already faded by depth in
// the vertex shader), both cut off at 1; the feathered ones also fade the alpha where the
// scene behind is within 0.001 of the depth buffer, and less and less so for a scene
// past 0.99 of the depth buffer (no fade at all against one at 1).
// [assumed]: arithmetic on stored texture values (as `surface.wgsl`); the original's near
// plane, which the depth buffer's scale hangs on.
// [stand-in]: Bevy blends in linear light; the original blended the stored values.
// [stand-in]: the level's light for the lit variants (the arena's sun and two colours).
// Fog and depth alpha fade are evaluated at vertices, then interpolated as in the
// original. BlendMode 1/2 disables fog (00a9b620); RGB is fogged after lighting.

#import bevy_pbr::{
    forward_io::{Vertex, FragmentOutput},
    mesh_functions,
    view_transformations::position_world_to_clip,
    mesh_view_bindings as view_bindings,
}
#ifdef DEPTH_PREPASS
#import bevy_pbr::prepass_utils::prepass_depth
#endif

const ADD: u32 = 1u;
const FEATHER: u32 = 2u;
const LIT: u32 = 4u;

struct Fx {
    flags: vec4<u32>,
    // x the original's near plane, yz ALPHAFADE slope/intercept
    values: vec4<f32>,
    // DIR_TO_LIGHT, DIFFUSE_COLOR, AMBIENT_COLOR, INV_COLOR_COEFF (w: the back-light)
    to_light: vec4<f32>,
    diffuse: vec4<f32>,
    ambient: vec4<f32>,
    inverse: vec4<f32>,
    fog: vec4<f32>,
    fog_colour: vec4<f32>,
}

// These meshes contain world-space unskinned quads. Keep sheet-frame coordinates
// separate from lit normals, and carry the original vertex fog through location 8.
struct FxVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) uv_b: vec2<f32>,
#ifdef VERTEX_TANGENTS
    @location(4) world_tangent: vec4<f32>,
#endif
    @location(5) color: vec4<f32>,
    @location(8) fog: f32,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> fx: Fx;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var picture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var picture_sampler: sampler;

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    return pow(c, vec3<f32>(2.2));
}

@vertex
fn vertex(in: Vertex) -> FxVertexOutput {
    var out: FxVertexOutput;
    let world_from_local = mesh_functions::get_world_from_local(in.instance_index);
    out.world_position = mesh_functions::mesh_position_local_to_world(world_from_local, vec4<f32>(in.position, 1.0));
    out.position = position_world_to_clip(out.world_position.xyz);
    out.world_normal = mesh_functions::mesh_normal_local_to_world(in.normal, in.instance_index);
    out.uv = in.uv;
    out.uv_b = in.uv_b;
#ifdef VERTEX_TANGENTS
    out.world_tangent = mesh_functions::mesh_tangent_local_to_world(world_from_local, in.tangent, in.instance_index);
#endif
    // Perspective clip w is camera-forward depth, the original's r1.y.
    let depth = out.position.w;
    out.color = vec4<f32>(in.color.rgb, in.color.a * saturate(depth * fx.values.y + fx.values.z));
    out.fog = max(min(depth * fx.fog.x + fx.fog.y, fx.fog.w), fx.fog.z);
    return out;
}

@fragment
fn fragment(in: FxVertexOutput) -> FragmentOutput {
    // The quads carry the frame's fraction in their normal: (fraction, 1 - fraction, 0),
    // which the mesh's vertex shader made a unit vector.
    // A lit element's quads carry true normals, and the fraction in their tangent's w.
    var next = saturate(in.world_normal.x / max(in.world_normal.x + in.world_normal.y, 1e-6));
#ifdef VERTEX_TANGENTS
    if (fx.flags.x & LIT) != 0u {
        next = saturate(in.world_tangent.w);
    }
#endif
    let here = textureSample(picture, picture_sampler, in.uv);
    let there = textureSample(picture, picture_sampler, in.uv_b);
    let mixed = mix(here, there, next);
    var texel = mixed * in.color;
    var alpha = texel.a;

    // The lit pixel shaders (729ad8 / 72a020 with frames, 72b618 / 72bb18 without), used
    // by renderers of `BlendMode` 3. [game]: with a = the texel's alpha times the
    // colour's and d = N . the way to the light,
    //   light = saturate(max((1 - |d|) (1 - a) + back-light, max(d, 0)))
    //   rgb   = texel x colour x (diffuse x light + ambient + saturate(inverse colour))
    // so thin parts of the smoke let light through and its far side is not black.
    if (fx.flags.x & LIT) != 0u {
        let d = dot(normalize(in.world_normal), fx.to_light.xyz);
        let through = (1.0 - abs(d)) * (1.0 - mixed.a * in.color.a) + fx.inverse.w;
        let light = saturate(max(through, max(d, 0.0)));
        texel = vec4<f32>(texel.rgb * (fx.diffuse.rgb * light + fx.ambient.rgb + saturate(fx.inverse.rgb)), texel.a);
    }

#ifdef DEPTH_PREPASS
    if (fx.flags.x & FEATHER) != 0u {
        // Bevy's depth is near / distance; the original's was 1 - near / distance (its far
        // plane taken as far off), so a step between two depths is the same size in both
        // but for the two near planes.
        let behind = prepass_depth(in.position, 0u);
        let scale = fx.values.x / view_bindings::view.clip_from_view[3][2];
        let scene = 1.0 - behind * scale;
        let own = 1.0 - in.position.z * scale;
        let far_off = saturate((scene - 0.99) * 100.0);
        let near_it = saturate((scene - own - 0.0005) * 1000.0);
        alpha *= mix(near_it, 1.0, far_off);
    }
#endif

    // [game: 729968/729ad8/729db8/72a020] Fog changes RGB after lighting,
    // while particle/texture alpha (including depth feather) remains independent.
    let colour = to_linear(saturate(mix(texel.rgb, fx.fog_colour.rgb, in.fog)));
    alpha = saturate(alpha);
    var out: FragmentOutput;
    if (fx.flags.x & ADD) != 0u {
        // Source alpha and one: the pipeline's blend is premultiplied.
        out.color = vec4<f32>(colour * alpha, 0.0);
    } else {
        out.color = vec4<f32>(colour, alpha);
    }
    return out;
}
