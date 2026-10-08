//! Turbo's three fullscreen passes: copy, Gaussian/stretch, masked composite.
//! The kernel, mask and sizes are original; Bevy ordering/colour domain are [assumed]
//! in notes/status.md. This pass never includes the HUD.

use bevy::{
    asset::uuid_handle,
    core_pipeline::{Core3dSystems, FullscreenShader, schedule::Core3d},
    prelude::*,
    render::{
        Render, RenderApp, RenderStartup, RenderSystems,
        extract_component::{
            ComponentUniforms, DynamicUniformIndex, ExtractComponent, ExtractComponentPlugin,
            UniformComponentPlugin,
        },
        extract_resource::{ExtractResource, ExtractResourcePlugin},
        render_asset::RenderAssets,
        render_resource::{
            binding_types::{sampler, texture_2d, uniform_buffer},
            *,
        },
        renderer::{RenderContext, RenderDevice, ViewQuery},
        texture::{CachedTexture, GpuImage, TextureCache},
        view::ViewTarget,
    },
    shader::Shader,
};
use tf2_core::speedblur::{self, Blend, Preset};

const SHADER: Handle<Shader> = uuid_handle!("466d630b-9051-4729-9df0-caa96fd44902");

#[derive(Resource)]
pub struct Loaded {
    preset: Preset,
    image: Option<Image>,
    disabled: bool,
    report: bool,
}

pub fn load(dir: &std::path::Path, disabled: bool, report: bool) -> Result<Loaded, String> {
    let (preset, texture) = speedblur::load(dir, crate::world::LEVEL)?;
    let image = crate::assets::image(&texture, false).ok_or("Cannot decode turbo speed mask")?;
    Ok(Loaded {
        preset,
        image: Some(image),
        disabled,
        report,
    })
}

#[derive(Resource, Clone, ExtractResource)]
struct Mask(Handle<Image>);

#[derive(Component, Clone, Copy, ExtractComponent, ShaderType)]
struct Settings {
    taps: [Vec4; 13],
    // stretch, environment weight, MaxBlur, diagnostic switch
    params: Vec4,
}

pub fn plugin(app: &mut App) {
    app.world_mut()
        .resource_mut::<Assets<Shader>>()
        .insert(
            &SHADER,
            Shader::from_wgsl(include_str!("speedblur.wgsl"), "speedblur.wgsl"),
        )
        .expect("Register turbo blur shader");
    app.add_plugins((
        ExtractComponentPlugin::<Settings>::default(),
        UniformComponentPlugin::<Settings>::default(),
        ExtractResourcePlugin::<Mask>::default(),
    ))
    .add_systems(Startup, setup.after(crate::spawn_player))
    .add_systems(Update, update.after(crate::player::tick));
    let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
        return;
    };
    render_app
        .init_resource::<SpecializedRenderPipelines<Pipeline>>()
        .add_systems(RenderStartup, init_pipeline)
        .add_systems(Render, prepare.in_set(RenderSystems::Prepare))
        // [assumed] Original global post-pass ordering/colour domain remain unread.
        .add_systems(Core3d, render.in_set(Core3dSystems::EarlyPostProcess));
}

fn setup(
    mut commands: Commands,
    mut loaded: ResMut<Loaded>,
    mut images: ResMut<Assets<Image>>,
    camera: Single<Entity, With<Camera3d>>,
) {
    commands.insert_resource(Mask(
        images.add(loaded.image.take().expect("Turbo mask loaded once")),
    ));
    commands.entity(*camera).insert(Settings {
        taps: speedblur::gaussian_taps(loaded.preset.blur_amount).map(Vec4::from_array),
        params: Vec4::new(
            loaded.preset.stretch,
            0.0,
            loaded.preset.max_blur,
            if loaded.report { 1.0 } else { 0.0 },
        ),
    });
}

fn update(
    time: Res<Time>,
    loaded: Res<Loaded>,
    player: Single<&crate::player::Player>,
    mut cameras: Query<&mut Settings>,
    mut blend: Local<Blend>,
) {
    // [assumed] Sample the final simulation latch once per render frame; timing debt
    // is logged in notes/status.md. Both turbo abilities send this independent latch.
    let weight = blend.step(player.state.turbo_fx_on, &loaded.preset, time.delta_secs());
    for mut settings in &mut cameras {
        let weight = if loaded.disabled { 0.0 } else { weight };
        if loaded.report && weight != settings.params.y {
            eprintln!(
                "turbo blur weight {weight:.3}, turbo FX {}",
                player.state.turbo_fx_on
            );
        }
        settings.params.y = weight;
    }
}

#[derive(Resource)]
struct Pipeline {
    layout: BindGroupLayoutDescriptor,
    sampler: Sampler,
    fullscreen: FullscreenShader,
}

impl SpecializedRenderPipeline for Pipeline {
    type Key = (TextureFormat, u8);
    fn specialize(&self, (format, pass): Self::Key) -> RenderPipelineDescriptor {
        RenderPipelineDescriptor {
            label: Some(format!("turbo blur pass {pass}").into()),
            layout: vec![self.layout.clone()],
            vertex: self.fullscreen.to_vertex_state(),
            fragment: Some(FragmentState {
                shader: SHADER,
                entry_point: Some(["copy", "blur", "composite"][pass as usize].into()),
                targets: vec![Some(ColorTargetState {
                    format,
                    blend: None,
                    write_mask: ColorWrites::ALL,
                })],
                ..default()
            }),
            ..default()
        }
    }
}

fn init_pipeline(
    mut commands: Commands,
    device: Res<RenderDevice>,
    fullscreen: Res<FullscreenShader>,
) {
    let layout = BindGroupLayoutDescriptor::new(
        "turbo blur bindings",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                texture_2d(TextureSampleType::Float { filterable: true }),
                sampler(SamplerBindingType::Filtering),
                texture_2d(TextureSampleType::Float { filterable: true }),
                texture_2d(TextureSampleType::Float { filterable: true }),
                uniform_buffer::<Settings>(true),
            ),
        ),
    );
    // [game: 00582d70, 005839a0, 00551670] Bilinear, clamp, point mip, anisotropy1.
    let sampler = device.create_sampler(&SamplerDescriptor {
        mag_filter: FilterMode::Linear,
        min_filter: FilterMode::Linear,
        ..default()
    });
    commands.insert_resource(Pipeline {
        layout,
        sampler,
        fullscreen: fullscreen.clone(),
    });
}

#[derive(Component)]
struct Prepared {
    pipelines: [CachedRenderPipelineId; 3],
    copy: CachedTexture,
    blur: CachedTexture,
}

fn prepare(
    mut commands: Commands,
    device: Res<RenderDevice>,
    mut textures: ResMut<TextureCache>,
    pipeline: Res<Pipeline>,
    cache: Res<PipelineCache>,
    mut specialized: ResMut<SpecializedRenderPipelines<Pipeline>>,
    views: Query<(Entity, &ViewTarget, &Settings)>,
) {
    for (entity, target, settings) in &views {
        if settings.params.y <= 0.0 {
            continue;
        }
        let size = target.main_texture().size();
        let [width, height] = speedblur::intermediate_size(size.width, size.height);
        let descriptor = TextureDescriptor {
            label: Some("turbo blur quarter target"),
            size: Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            // [assumed] Retain Bevy's current scene format/colour domain; status debt.
            format: target.main_texture_format(),
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        };
        commands.entity(entity).insert(Prepared {
            pipelines: [0, 1, 2]
                .map(|pass| specialized.specialize(&cache, &pipeline, (descriptor.format, pass))),
            copy: textures.get(&device, descriptor.clone()),
            blur: textures.get(&device, descriptor),
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn render(
    view: ViewQuery<(
        &ViewTarget,
        &Settings,
        &DynamicUniformIndex<Settings>,
        Option<&Prepared>,
    )>,
    pipeline: Res<Pipeline>,
    cache: Res<PipelineCache>,
    uniforms: Res<ComponentUniforms<Settings>>,
    mask: Option<Res<Mask>>,
    images: Res<RenderAssets<GpuImage>>,
    mut ctx: RenderContext,
    mut reported: Local<bool>,
) {
    let (target, settings, index, prepared) = view.into_inner();
    if settings.params.y <= 0.0 {
        *reported = false;
        return;
    }
    let Some(prepared) = prepared else {
        return;
    };
    let Some(mask) = mask.and_then(|m| images.get(&m.0)) else {
        return;
    };
    let [
        Some(copy_pipeline),
        Some(blur_pipeline),
        Some(composite_pipeline),
    ] = prepared.pipelines.map(|id| cache.get_render_pipeline(id))
    else {
        return;
    };
    let Some(binding) = uniforms.uniforms().binding() else {
        return;
    };
    // Flip only after every pipeline, asset and uniform is ready. Every pass writes
    // its entire destination; there is no scene read/write feedback within a pass.
    let post = target.post_process_write();
    let layout = cache.get_bind_group_layout(&pipeline.layout);
    let stages = [
        (
            post.source,
            &prepared.copy.default_view,
            &prepared.copy.default_view,
            copy_pipeline,
        ),
        (
            &prepared.copy.default_view,
            &prepared.copy.default_view,
            &prepared.blur.default_view,
            blur_pipeline,
        ),
        (
            post.source,
            &prepared.blur.default_view,
            post.destination,
            composite_pipeline,
        ),
    ];
    for (source, blur, destination, pass_pipeline) in stages {
        // Unused shader bindings still must not alias the render attachment.
        let blur = if blur.id() == destination.id() {
            source
        } else {
            blur
        };
        let group = ctx.render_device().create_bind_group(
            "turbo blur",
            &layout,
            &BindGroupEntries::sequential((
                source,
                &pipeline.sampler,
                blur,
                &mask.texture_view,
                binding.clone(),
            )),
        );
        let mut pass = ctx
            .command_encoder()
            .begin_render_pass(&RenderPassDescriptor {
                label: Some("turbo blur"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: destination,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations::default(),
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        pass.set_pipeline(pass_pipeline);
        pass.set_bind_group(0, &group, &[index.index()]);
        pass.draw(0..3, 0..1);
    }
    if settings.params.w > 0.0 && !*reported {
        let size = prepared.copy.texture.size();
        eprintln!(
            "turbo blur GPU: copy + gaussian13/stretch + mask composite, {}x{} intermediate, weight {:.3}",
            size.width, size.height, settings.params.y
        );
        *reported = true;
    }
}
