// The original's distortion shaders (vertex 731560, pixel 731810, compiled into the
// executable): the particle renderer `Dstr` and any renderer with `IsShimmer`. The heat
// haze over the turbo, a muzzle flash, an explosion.
//
// [game]: the renderer's picture is a map: red and green say which way to push (0.5 is
// no push), alpha how much. With f = the particle's alpha faded by depth,
//   push  = (0.5 - map.rg) x map.a x f
//   place = the pixel's place in the world + A x push.x + B x push.y,
//           A = left x n.y - up x n.x,  B = left x n.y + up x n.x
// (left and up the camera's, n two numbers on the vertex), and the pixel is the picture
// of the scene where that place lands on the screen, blended in with alpha map.a x f.
// [assumed]: the vertex's two numbers are both the particle's radius.
// [stand-in]: the scene behind is Bevy's copy of the picture taken before the see-through
// things are drawn (smoke and fire behind the haze are not bent), and the blend is done
// here against that copy.

#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    mesh_view_bindings as view_bindings,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var map: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var map_sampler: sampler;

fn screen(p: vec3<f32>) -> vec2<f32> {
    let clip = view_bindings::view.clip_from_world * vec4<f32>(p, 1.0);
    return clip.xy / clip.w * vec2<f32>(0.5, -0.5) + 0.5;
}

@fragment
fn fragment(in: VertexOutput) -> FragmentOutput {
    let d = textureSample(map, map_sampler, in.uv);
    let fade = in.color.a;
    let alpha = saturate(d.a * fade);
    let push = (vec2<f32>(0.5) - d.rg) * d.a * fade;
#ifdef VERTEX_TANGENTS
    let n = in.world_tangent.w;
#else
    let n = 1.0;
#endif
    let left = -view_bindings::view.world_from_view[0].xyz;
    let up = view_bindings::view.world_from_view[1].xyz;
    let place = in.world_position.xyz + (left - up) * n * push.x + (left + up) * n * push.y;
    let here = textureSampleLevel(view_bindings::view_transmission_texture, view_bindings::view_transmission_sampler, screen(in.world_position.xyz), 0.0);
    let bent = textureSampleLevel(view_bindings::view_transmission_texture, view_bindings::view_transmission_sampler, screen(place), 0.0);
    var out: FragmentOutput;
    out.color = vec4<f32>(mix(here.rgb, bent.rgb, alpha), 1.0);
    return out;
}
