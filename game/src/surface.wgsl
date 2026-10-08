// The original's surface shaders for his five techniques, written again from the compiled
// pixel shaders in `shaders.lsa` (`work\lsa_disasm.py`; the listings are in
// `work\shaders\asm`): `aniso_paint_x` 0ab3b0, `aniso_paint` 0a83d0, `glass_plus` 09a0d0,
// `cheap_plus` 05cb30, `glass_cheap` 098a20. One shader here, the differences as flags.
//
// [game]: the normal from the map, the paint ramp, the mix with the colour texture by its
// alpha, the diffuse and specular terms, the edge light, the reflection and its falloff.
// [assumed]: the arithmetic is on the textures' stored values (no sRGB decode), as a 2009
// Direct3D 9 title did it; the picture's yellow needs it.
// [stand-in]: the light rig (one sun from Bevy's light and its shadow, a two-colour
// ambient for the vertex shader's nine irradiance terms), and the paint's
// `RealtimeSphereMap`, which the original renders from the level each frame.
// Not ported: the two omni and two spot lights, fog, `IrisParams` (exposure).

#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    mesh_view_bindings as view_bindings,
    shadows,
}

const PAINT: u32 = 1u;
const EDGE: u32 = 2u;
const REFLECT_CUBE: u32 = 4u;
const REFLECT_SPHERE: u32 = 8u;
const FALLOFF: u32 = 16u;
const NORMAL_MAP: u32 = 32u;
const VERTEX_COLOR: u32 = 64u;

struct Surface {
    diffuse_color: vec4<f32>,
    // rgb `Specular_Color`, w `Specular_Power`
    specular: vec4<f32>,
    // rgb `Reflection_Color`, w `Reflection_Falloff`
    reflection: vec4<f32>,
    // rgb `Edge_Color`, w `Edge_Power`
    edge: vec4<f32>,
    // rgb `Paint_Color0`, w `Paint_Pow1`
    paint0: vec4<f32>,
    // rgb `Paint_Color1`, w `Paint_Pow2`
    paint1: vec4<f32>,
    // rgb `Paint_Color2`, w `Normal_Map_HeightScale`
    paint2: vec4<f32>,
    // rgb the sun's colour, w `Diffuse_Texture1_Opacity`
    light: vec4<f32>,
    // rgb the ambient from above, w `Reflection_Falloff_Power`
    sky: vec4<f32>,
    // rgb the ambient from below, w `AlphaBoost`
    ground: vec4<f32>,
    // rgb what the paint reflects overhead
    sky_seen: vec4<f32>,
    uv_x: vec4<f32>,
    uv_y: vec4<f32>,
    uv_z: vec4<f32>,
    uv_t: vec4<f32>,
    flags: vec4<u32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> surface: Surface;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var diffuse_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var diffuse_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(3) var normal_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(4) var normal_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(5) var specular_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(6) var specular_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(7) var reflection_texture: texture_cube<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(8) var reflection_sampler: sampler;

// Bevy's space to the game's (z up, y forward), which the cube map is laid out in.
fn game_space(v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(v.x, -v.z, v.y);
}

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}

@fragment
fn fragment(in: VertexOutput) -> FragmentOutput {
    let flags = surface.flags.x;
#ifdef VERTEX_UVS_A
    var uv = in.uv;
#else
    var uv = vec2<f32>(0.0);
#endif
    if (flags & VERTEX_COLOR) != 0u {
        // [game] 044aa0_vs: X*u + Y*v + Z + T*time.
        uv = surface.uv_x.xy*uv.x + surface.uv_y.xy*uv.y + surface.uv_z.xy
            + surface.uv_t.xy*view_bindings::globals.time;
    }

    var n = normalize(in.world_normal);
#ifdef VERTEX_TANGENTS
    if (flags & NORMAL_MAP) != 0u {
        // The map's x goes along the mesh's binormal and its y along its tangent, each
        // times the height scale; z is rebuilt from them.
        let tangent = normalize(in.world_tangent.xyz);
        let binormal = normalize(cross(n, tangent) * in.world_tangent.w);
        let m = textureSample(normal_texture, normal_sampler, uv).xy * 2.0 - 1.0;
        let z = sqrt(abs(dot(m, m) - 1.0));
        let scale = surface.paint2.w;
        n = normalize(n * z + tangent * (m.y * scale) + binormal * (m.x * scale));
    }
#endif

    let to_eye = view_bindings::view.world_position - in.world_position.xyz;
    let v = normalize(to_eye);

    // [stand-in] the sun and its shadow are Bevy's; the original has two shadow maps
    // and a four-tap filter.
    var l = vec3<f32>(0.0, 1.0, 0.0);
    var shadow = 1.0;
    if view_bindings::lights.n_directional_lights > 0u {
        let sun = &view_bindings::lights.directional_lights[0];
        l = (*sun).direction_to_light;
        if ((*sun).flags & 1u) != 0u {
            let view_z = dot(vec4<f32>(
                view_bindings::view.view_from_world[0].z,
                view_bindings::view.view_from_world[1].z,
                view_bindings::view.view_from_world[2].z,
                view_bindings::view.view_from_world[3].z
            ), in.world_position);
            shadow = shadows::fetch_directional_shadow(0u, in.world_position, in.world_normal, view_z, in.position.xy);
        }
    }
    let light_color = surface.light.rgb;
    // [stand-in] for `AmbientIrradiance` and `AmbientColor`, which the vertex shader adds.
    let ambient = mix(surface.ground.rgb, surface.sky.rgb, n.y * 0.5 + 0.5);

    let half_way = normalize(v + l);
    let n_dot_h = max(saturate(dot(n, half_way)), 0.000001);
    let specular_map = textureSample(specular_texture, specular_sampler, uv).rgb;
    let texel = textureSample(diffuse_texture, diffuse_sampler, uv);

    var albedo: vec3<f32>;
    var alpha: f32;
    if (flags & PAINT) != 0u {
        // The paint: dark to its colour to its highlight by how nearly the surface faces
        // half way between eye and sun; the colour texture covers it by its alpha.
        var paint = mix(surface.paint0.rgb, surface.paint1.rgb, pow(n_dot_h, surface.paint0.w));
        paint = mix(paint, surface.paint2.rgb, pow(n_dot_h, surface.paint1.w));
        albedo = mix(paint, texel.rgb, texel.a) * surface.diffuse_color.rgb;
        alpha = surface.ground.w * surface.diffuse_color.a;
    } else {
        albedo = texel.rgb * surface.diffuse_color.rgb;
        alpha = texel.a * surface.light.w * surface.diffuse_color.a * surface.ground.w;
    }
#ifdef VERTEX_COLORS
    // [game] dull_vc: 044aa0_vs / 038900_ps multiply the material, object and
    // vertex colours. The installed skid UV transform is identity [data].
    // [assumed] White object tint; the original strip renderer's object tint is unread.
    if (flags & VERTEX_COLOR) != 0u {
        albedo *= in.color.rgb;
        alpha *= in.color.a;
    }
#endif

    let power = surface.specular.w;
    let diffuse_light = saturate(dot(n, l)) * light_color * shadow + ambient;
    let specular_light = (power / 30.0 + 1.0) * pow(n_dot_h, power) * shadow * light_color;
    var colour = albedo * diffuse_light;
    // [game] dull_vc has diffuse lighting only, without the other techniques' specular.
    if (flags & VERTEX_COLOR) == 0u {
        colour += specular_map * surface.specular.rgb * specular_light;
    }

    let facing = abs(dot(v, n));
    if (flags & EDGE) != 0u {
        colour += pow(saturate(1.0 - facing), surface.edge.w) * light_color * surface.edge.rgb;
    }

    if (flags & (REFLECT_CUBE | REFLECT_SPHERE)) != 0u {
        let r = reflect(-to_eye, n);
        var seen: vec3<f32>;
        if (flags & REFLECT_CUBE) != 0u {
            seen = textureSample(reflection_texture, reflection_sampler, game_space(r)).rgb;
        } else {
            // [stand-in] for `RealtimeSphereMap`: sky above, ground below.
            seen = mix(surface.ground.rgb, surface.sky_seen.rgb, smoothstep(-0.15, 0.25, normalize(r).y));
        }
        var reflected = seen * surface.reflection.rgb * specular_map;
        if (flags & FALLOFF) != 0u {
            reflected *= pow(facing * (surface.reflection.w - 1.0) + 1.0, surface.sky.w);
        }
        colour += saturate(reflected);
    }

    var out: FragmentOutput;
    out.color = vec4<f32>(to_linear(saturate(colour)), alpha);
    return out;
}
