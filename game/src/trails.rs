//! Attack trails from RibbonTrail data, with timed and interrupted-clip messages.
//! [assumed] Character named trail messages also reach the attached axe object; the
//! original character listener/weapon-manager forwarding route is not fully read.

use crate::assets::{ObjectNodes, game_to_bevy, image};
use crate::camera::Rig;
use crate::fx::Glow;
use crate::player::RobotModel;
use crate::weapons::AxeModel;
use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::light::NotShadowCaster;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use std::collections::HashMap;
use tf2_core::formats::anim::Clip;
use tf2_core::ribbon::{self, Trail};

struct Shown {
    trail: Trail,
    mesh: Option<(Entity, Handle<Mesh>)>,
}

#[derive(Resource, Default)]
pub struct Trails {
    messages: Vec<(u32, u32)>,
    exits: Vec<(u32, u32)>,
    shown: HashMap<(Entity, usize), Shown>,
    materials: HashMap<u32, Handle<Glow>>,
}

impl Trails {
    /// [assumed] Exit keys fire when the leading clip changes, including during a fade.
    /// The registration is read; callback timing during BlendAnimation remains open.
    pub fn clip_changed(&mut self) {
        self.messages.append(&mut self.exits);
    }
    pub fn clip_events(&mut self, clip: &Clip, from: f32, to: f32) {
        self.exits = ribbon::exit_messages(clip)
            .map(|e| (e.name, e.message))
            .collect();
        self.messages.extend(
            clip.trigger_events
                .iter()
                .filter(|e| e.due(from, to))
                .map(|e| (e.name, e.message)),
        );
    }
}

/// Run after transform propagation so histories sample this frame's posed nodes.
pub fn run(
    time: Res<Time>,
    data: Res<crate::GameData>,
    rig: Res<Rig>,
    owners: Query<(
        Entity,
        &ObjectNodes,
        &Visibility,
        Option<&RobotModel>,
        Option<&AxeModel>,
    )>,
    placed: Query<&GlobalTransform>,
    mut trails: ResMut<Trails>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<Glow>>,
    mut images: ResMut<Assets<Image>>,
) {
    let trails = &mut *trails;
    let messages = std::mem::take(&mut trails.messages);
    for (owner, nodes, visible, robot, axe) in &owners {
        let object = if robot.is_some() {
            Some(&data.0.robot)
        } else if axe.is_some() {
            data.0.melee_weapon.as_ref().and_then(|w| w.object.as_ref())
        } else {
            None
        };
        let Some(object) = object else { continue };
        for (index, def) in object.ribbons.iter().enumerate() {
            let shown = trails.shown.entry((owner, index)).or_insert_with(|| Shown {
                trail: Trail::new(def),
                mesh: None,
            });
            for &(name, message) in &messages {
                if name == def.name {
                    shown.trail.message(message);
                }
            }
            let points = def
                .links
                .iter()
                .map(|name| {
                    let i = nodes.name_hashes.iter().position(|hash| hash == name)?;
                    Some(placed.get(nodes.entities[i]).ok()?.translation())
                })
                .collect::<Option<Vec<_>>>();
            let Some(points) = points else { continue };
            shown.trail.update(def, points, time.delta_secs());
            let (vertices, indices) = shown.trail.mesh(def, game_to_bevy(rig.view.eye));
            if vertices.is_empty() || *visible == Visibility::Hidden {
                if let Some((entity, _)) = &shown.mesh {
                    commands.entity(*entity).insert(Visibility::Hidden);
                }
                continue;
            }
            let mut mesh = Mesh::new(
                PrimitiveTopology::TriangleList,
                RenderAssetUsages::default(),
            );
            mesh.insert_attribute(
                Mesh::ATTRIBUTE_POSITION,
                vertices
                    .iter()
                    .map(|v| v.position.to_array())
                    .collect::<Vec<_>>(),
            );
            mesh.insert_attribute(
                Mesh::ATTRIBUTE_NORMAL,
                vertices
                    .iter()
                    .map(|v| v.normal.to_array())
                    .collect::<Vec<_>>(),
            );
            mesh.insert_attribute(
                Mesh::ATTRIBUTE_UV_0,
                vertices.iter().map(|v| v.uv).collect::<Vec<_>>(),
            );
            mesh.insert_indices(Indices::U32(indices));
            if let Some((entity, handle)) = &shown.mesh {
                let _ = meshes.insert(handle, mesh);
                commands.entity(*entity).insert(Visibility::Inherited);
            } else {
                let material = if let Some(material) = trails.materials.get(&def.material) {
                    material.clone()
                } else {
                    let Some(source) = data.0.library.materials.get(&def.material) else {
                        continue;
                    };
                    let texture = Glow::texture(source)
                        .flatten()
                        .and_then(|id| data.0.library.textures.get(&id))
                        .and_then(|t| image(t, false))
                        .map(|i| images.add(i));
                    let alpha = match source.word("blendmode") {
                        Some(v) if v == tf2_core::formats::hash::crc32(b"Additive") => {
                            AlphaMode::Add
                        }
                        Some(v) if v == tf2_core::formats::hash::crc32(b"Normal") => {
                            AlphaMode::Blend
                        }
                        _ => AlphaMode::Opaque,
                    };
                    let Some(material) = Glow::build(source, texture, alpha) else {
                        continue;
                    };
                    let material = materials.add(material);
                    trails.materials.insert(def.material, material.clone());
                    material
                };
                let handle = meshes.add(mesh);
                let entity = commands
                    .spawn((
                        Mesh3d(handle.clone()),
                        MeshMaterial3d(material),
                        Transform::IDENTITY,
                        NoFrustumCulling,
                        NotShadowCaster,
                    ))
                    .id();
                shown.mesh = Some((entity, handle));
            }
        }
    }
}
