// [game] effects.lsa destruction_sphere 00efa0; shaders.lsa surface mix 037e60.
// [stand-in] Masks evaluated directly on arena surfaces, then lit by Bevy PBR.
#import bevy_pbr::{forward_io::{VertexOutput,FragmentOutput},pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{apply_pbr_lighting,main_pass_post_lighting_processing}}
struct Sphere { sphere:vec4<f32>,outer:vec4<f32>,inner:vec4<f32>,row0:vec4<f32>,row1:vec4<f32>,row2:vec4<f32>,params:vec4<f32> }
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<storage,read> spheres:array<Sphere>;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var noise_tex:texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var noise_sampler:sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var cracks_tex:texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(104) var cracks_sampler:sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(105) var damage_tex:texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(106) var damage_sampler:sampler;
@fragment
fn fragment(in:VertexOutput,@builtin(front_facing) front:bool)->FragmentOutput {
    var pbr=pbr_input_from_standard_material(in,front);
    let point=vec3<f32>(in.world_position.x,-in.world_position.z,in.world_position.y);
    let dx=dpdx(point);
    let dy=dpdy(point);
    var mask=vec2<f32>(0.0);
    for (var i=0u;i<arrayLength(&spheres);i+=1u) {
        let s=spheres[i];
        if (s.sphere.w<=0.0) { continue; }
        let edge=clamp(1.0-distance(point,s.sphere.xyz)/s.sphere.w,0.0,1.0);
        if (edge<=0.0) { continue; }
        let p=vec4<f32>(point,1.0);
        let n=vec3<f32>(dot(s.row0,p),dot(s.row1,p),dot(s.row2,p))*s.params.x;
        // The original texld selects from the noise texture's eight mips. Explicit
        // gradients preserve that selection inside the per-volume branches.
        let gx=vec3<f32>(dot(s.row0.xyz,dx),dot(s.row1.xyz,dx),dot(s.row2.xyz,dx))*s.params.x;
        let gy=vec3<f32>(dot(s.row0.xyz,dy),dot(s.row1.xyz,dy),dot(s.row2.xyz,dy))*s.params.x;
        let noise=(textureSampleGrad(noise_tex,noise_sampler,n.xy,gx.xy,gy.xy).r+
            textureSampleGrad(noise_tex,noise_sampler,n.xz,gx.xz,gy.xz).r+
            textureSampleGrad(noise_tex,noise_sampler,n.yz,gx.yz,gy.yz).r)*0.33333;
        let outer=edge+0.5+(noise-0.5)*s.params.y;
        let ext=clamp(s.outer.xz*outer+s.outer.yw,vec2<f32>(0.0),vec2<f32>(1.0));
        let int_mask=clamp(s.inner.xz*noise+s.inner.yw,vec2<f32>(0.0),vec2<f32>(1.0));
        let feather=clamp(5.0*edge,0.0,1.0);
        mask+=vec2<f32>(feather*feather,feather)*ext*int_mask;
    }
    mask=clamp(mask,vec2<f32>(0.0),vec2<f32>(1.0));
    // [stand-in] Arena's own UVs for the borrowed material's crack and damage maps.
    let cracks=textureSample(cracks_tex,cracks_sampler,in.uv).r;
    let damaged=textureSample(damage_tex,damage_sampler,in.uv);
    let remaining=(1.0-mask.x*cracks)*(1.0-mask.y);
    pbr.material.base_color=vec4<f32>(mix(damaged.rgb,pbr.material.base_color.rgb,remaining),pbr.material.base_color.a);
    var out:FragmentOutput;
    out.color=apply_pbr_lighting(pbr);
    out.color=main_pass_post_lighting_processing(pbr,out.color);
    return out;
}
