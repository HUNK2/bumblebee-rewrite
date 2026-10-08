//! Persistent surface damage on the stand-in arena. Masks are the original's arithmetic;
//! Bevy's standard lighting and the arena's borrowed concrete textures are stand-ins.
use crate::{assets::image, effects::WeaponFxKind, player::Player, world::ArenaRes};
use bevy::{
    asset::uuid_handle,
    image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor},
    pbr::{ExtendedMaterial, MaterialExtension},
    prelude::*,
    render::{
        render_resource::{AsBindGroup, ShaderType},
        storage::ShaderBuffer,
    },
    shader::{Shader, ShaderRef},
};
use std::path::Path;
use tf2_core::{
    formats::{model::Library, pack::Pack, texture},
    melee::{Body, surface},
    ssd::{Damage, Info, Sphere},
};

const SHADER: Handle<Shader> = uuid_handle!("aaf94181-cb3e-46c3-b699-5a1ea6fb8106");
pub type Material = ExtendedMaterial<StandardMaterial, Extension>;
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct Extension {
    #[storage(100, read_only)]
    pub spheres: Handle<ShaderBuffer>,
    #[texture(101)]
    #[sampler(102)]
    pub noise: Handle<Image>,
    #[texture(103)]
    #[sampler(104)]
    pub cracks: Handle<Image>,
    #[texture(105)]
    #[sampler(106)]
    pub damaged: Handle<Image>,
}
impl MaterialExtension for Extension {
    fn fragment_shader() -> ShaderRef {
        SHADER.into()
    }
}
pub fn plugin(app: &mut App) {
    app.add_plugins(MaterialPlugin::<Material>::default());
    app.world_mut().resource_mut::<Assets<Shader>>().insert(
        &SHADER,
        Shader::from_wgsl(include_str!("ssd.wgsl"), "ssd.wgsl"),
    ).expect("Register SSD shader");
}

#[derive(Clone, Default, ShaderType)]
struct GpuSphere {
    sphere: Vec4,
    outer: Vec4,
    inner: Vec4,
    row0: Vec4,
    row1: Vec4,
    row2: Vec4,
    params: Vec4,
}
impl From<&Sphere> for GpuSphere {
    fn from(s: &Sphere) -> Self {
        let (outer, inner) = s.ramps();
        let m = s.noise_transform();
        let x: Vec3 = m.matrix3.x_axis.into();
        let y: Vec3 = m.matrix3.y_axis.into();
        let z: Vec3 = m.matrix3.z_axis.into();
        let p: Vec3 = m.translation.into();
        Self {
            sphere: s.centre.extend(s.radius),
            outer,
            inner,
            row0: Vec4::new(x.x, y.x, z.x, p.x),
            row1: Vec4::new(x.y, y.y, z.y, p.y),
            row2: Vec4::new(x.z, y.z, z.z, p.z),
            params: Vec4::new(s.noise_scale(), s.info.chaos, 0.0, 0.0),
        }
    }
}
#[derive(Resource)]
pub struct Textures {
    noise: Image,
    cracks: Image,
    damaged: Image,
}
fn picture(t: &texture::Texture) -> Result<Image, String> {
    let mut i = image(t, false).ok_or_else(|| format!("Cannot decode SSD texture {}", t.name))?;
    // [game] 0059f4d0 -> 01174460 -> 00546c20: wrap U/V, bilinear min/mag,
    // point mip filtering and anisotropy 1.
    i.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Nearest,
        ..default()
    });
    Ok(i)
}
pub fn load(dir: &Path) -> Result<Textures, String> {
    let bytes = std::fs::read(dir.join("Textures/NoiseVolume.apk")).map_err(|e| e.to_string())?;
    let noise = texture::parse(&bytes)?.ok_or("NoiseVolume2D is not an image")?;
    let library = Library::load(&Pack::open(
        &dir.join("levels")
            .join(crate::world::LEVEL)
            .join("zone.str"),
    )?);
    // [stand-in] This level's concrete material supplies the arena's two surface maps.
    // The arena has no authored SSD material; borrow these without shipping them.
    let material = library
        .materials
        .values()
        .find(|m| m.name == "us_valve_pipe_a.concrete2")
        .ok_or("No arena SSD concrete material")?;
    let map = |name| {
        material
            .texture(name)
            .and_then(|id| library.textures.get(&id))
            .ok_or_else(|| format!("Missing SSD {name}"))
    };
    Ok(Textures {
        noise: picture(&noise)?,
        cracks: picture(map("cracks_map_texture")?)?,
        damaged: picture(map("damage_map_texture")?)?,
    })
}
#[derive(Resource)]
pub struct Gpu {
    pub extension: Extension,
}
struct Blast {
    centre: Vec3,
    age: f32,
    def: tf2_core::formats::scene::ExplosionDef,
    infos: Vec<Info>,
    touched: Vec<usize>,
}
#[derive(Resource, Default)]
pub struct Marks {
    damage: Damage,
    blasts: Vec<Blast>,
}
/// Optional capture diagnostics, kept out of the HUD.
#[derive(Resource)]
pub struct Report(pub bool);
pub fn setup(
    mut commands: Commands,
    textures: Res<Textures>,
    mut images: ResMut<Assets<Image>>,
    mut buffers: ResMut<Assets<ShaderBuffer>>,
) {
    let extension = Extension {
        spheres: buffers.add(ShaderBuffer::from(vec![GpuSphere::default()])),
        noise: images.add(textures.noise.clone()),
        cracks: images.add(textures.cracks.clone()),
        damaged: images.add(textures.damaged.clone()),
    };
    commands.insert_resource(Gpu { extension });
}
fn put(damage: &mut Damage, infos: &[Info], point: Vec3, normal: Vec3, eye: Vec3) {
    // [data] Every one of Bumblebee's presets has exactly one SSDInfo.
    if let Some(&info) = infos.first()
        && let Some(sphere) = Sphere::contact(info, point, normal, surface::DEFAULT, 0, 0)
    {
        damage.put(sphere, eye);
    }
}
pub fn run(
    time: Res<Time>,
    player: Single<&Player>,
    data: Res<crate::GameData>,
    arena: Res<ArenaRes>,
    rig: Res<crate::camera::Rig>,
    arsenal: Res<crate::weapons::Arsenal>,
    gpu: Res<Gpu>,
    mut marks: ResMut<Marks>,
    mut buffers: ResMut<Assets<ShaderBuffer>>,
    report: Res<Report>,
) {
    let marks = &mut *marks;
    let eye = rig.view.eye;
    // Before submitting this frame, advance records previously submitted. New records
    // render at frame 1, as 01178c90 does before its increment.
    marks
        .damage
        .spheres
        .iter_mut()
        .for_each(Sphere::advance_frame);
    for hit in &player.state.surface_hits {
        if report.0 { eprintln!("SSD {} surface {} at {:?}",if hit.infos.is_some() {"blast"} else {"melee"},hit.contact.surface,hit.contact.point); }
        let infos = hit.infos.as_deref().unwrap_or(&data.0.melee_impact.ssd);
        put(
            &mut marks.damage,
            infos,
            hit.contact.point,
            hit.contact.normal,
            eye,
        );
    }
    for (weapon, hit) in &arsenal.surface_impacts {
        if let Some(effects) = data.0.weapon_effects.iter().find(|w| w.name == *weapon) {
            if report.0 { eprintln!("SSD gun at {:?}",hit.point); }
            put(
                &mut marks.damage,
                &effects.impact.ssd,
                hit.point,
                hit.normal,
                eye,
            );
        }
    }
    for event in arsenal
        .fx
        .iter()
        .filter(|e| matches!(e.kind, WeaponFxKind::Blast))
    {
        if let Some(effects) = data
            .0
            .weapon_effects
            .iter()
            .find(|w| w.name == event.weapon)
            && !effects.blast_ssd.is_empty()
            && let Some(def) = data
                .0
                .weapons
                .iter()
                .find(|w| w.name == event.weapon)
                .and_then(|w| w.explosion)
        {
            marks.blasts.push(Blast {
                centre: event.at,
                age: 0.0,
                def,
                infos: effects.blast_ssd.clone(),
                touched: Vec::new(),
            });
        }
    }
    // Same growing explosion sphere as the missiles' damage routine. [stand-in:
    // arena DETECT contacts, stepped per frame like Arsenal's missile explosions]
    let dt = time.delta_secs();
    marks.blasts.retain_mut(|blast| {
        let radius = blast.def.radius * blast.def.scale(blast.age, dt);
        let body = Body {
            shape: tf2_core::formats::scene::Shape::Sphere { radius },
            at: bevy::math::Affine3A::from_translation(blast.centre),
        };
        for contact in tf2_core::sim::World::surface_contacts(&arena.0, &body) {
            if !blast.touched.contains(&contact.surface) {
                if report.0 { eprintln!("SSD missile surface {} at {:?}",contact.surface,contact.point); }
                blast.touched.push(contact.surface);
                put(
                    &mut marks.damage,
                    &blast.infos,
                    contact.point,
                    contact.normal,
                    eye,
                );
            }
        }
        blast.age += dt;
        blast.age <= blast.def.time + dt
    });
    let mut values: Vec<GpuSphere> = marks.damage.spheres.iter().map(GpuSphere::from).collect();
    if values.is_empty() {
        values.push(GpuSphere::default());
    }
    if let Some(mut buffer) = buffers.get_mut(&gpu.extension.spheres) {
        buffer.set_data(values);
    }
}
