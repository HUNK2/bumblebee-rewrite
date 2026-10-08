#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

struct Settings {
    taps: array<vec4<f32>, 13>,
    params: vec4<f32>,
};
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var linear_clamp: sampler;
@group(0) @binding(2) var blurred: texture_2d<f32>;
@group(0) @binding(3) var mask: texture_2d<f32>;
@group(0) @binding(4) var<uniform> settings: Settings;

// [game: 00582d70] Linear copy into the quarter-size aligned target.
@fragment
fn copy(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    return textureSampleLevel(source, linear_clamp, in.uv, 0.0);
}

// [game: 005839a0, 71fdc0_ps.asm] Gaussian13 around the stretched centre.
@fragment
fn blur(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let centre = (in.uv - 0.5) * settings.params.x + 0.5;
    let texel = 1.0 / vec2<f32>(textureDimensions(source));
    var colour = vec4<f32>(0.0);
    for (var i = 0u; i < 13u; i += 1u) {
        let tap = settings.taps[i];
        colour += textureSampleLevel(source, linear_clamp, centre + tap.xy * texel, 0.0) * tap.z;
    }
    // DOF caller's stretch shader Blend is 1 (00551670): use the full filtered colour.
    return colour;
}

// [game: effects.lsa PS0032d0] scene + (blur-scene)*CoC*MaxBlur*mask.r*MaskTrans.
// The installed SpeedBlur has zero near/far focus/falloff: inverse farFalloff=10000,
// so far CoC saturates to1 for every visible depth (camera near=.2), including sky.
// The loader rejects other focus settings; this is not a general DOF approximation.
@fragment
fn composite(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let size = vec2<i32>(textureDimensions(source));
    let pixel = clamp(vec2<i32>(in.uv * vec2<f32>(size)), vec2<i32>(0), size - 1);
    // [game: 00551670] ColorBuffer point min/mag; BlurBuffer/Mask bilinear.
    let scene = textureLoad(source, pixel, 0).rgb;
    let soft = textureSampleLevel(blurred, linear_clamp, in.uv, 0.0).rgb;
    let amount = textureSampleLevel(mask, linear_clamp, in.uv, 0.0).r * settings.params.y * settings.params.z;
    return vec4<f32>(mix(scene, soft, amount), 1.0);
}
