//! Installed Decepticon scouts: shared character simulation, receiving damage and death.
//! [stand-in] Encounter placement and tactics in the test arena; see notes/damage.md.

use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
use bevy::prelude::*;

use crate::assets::{self, ObjectNodes, game_to_bevy};
use crate::player::{Player, TuningRes};
use crate::sim::World as _;
use crate::tuning::Tuning;
use crate::world::ArenaRes;
use tf2_core::formats::scene::Shape;
use tf2_core::melee::Target;

/// [stand-in] Arena cylinder has one surface; original contact bodies have individual ones.
const SURFACE: u32 = tf2_core::melee::surface::UPPER_BODY;

#[derive(Resource)]
pub struct Encounter {
    pub scout: tf2_core::combat::Scout,
    pub passive: bool,
    pub report: bool,
}
#[derive(Component)]
pub(crate) struct EnemyModel {
    owner: Entity,
    blend: crate::animation::ObjectBlend,
}
#[derive(Component)]
pub(crate) struct EnemyGun {
    owner: Entity,
    blend: crate::animation::ObjectBlend,
}
#[derive(Component)]
pub struct EnemyTarget {
    pub id: u32,
    pub combat: tf2_core::combat::Enemy,
    pub health: f32,
    /// Debug HUD: corpse age; original NPC completion removes it until encounter reset.
    pub down_for: f32,
    reset_serial: u32,
}

/// [stand-in] Three scouts to the left of the arena start.
pub fn layout(t: &Tuning) -> Vec<Target> {
    [(-25.0, 25.0), (-34.0, 31.0), (-25.0, 42.0)]
        .into_iter()
        .zip(1..)
        .map(|((x, y), id)| Target {
            id,
            pos: Vec3::new(x, y, 0.0),
            radius: t.robot_radius,
            height: t.robot_height,
            character: true,
            large: false,
            surface: SURFACE,
        })
        .collect()
}

pub fn spawn(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut surfaces: ResMut<Assets<crate::surface::Surface>>,
    mut glows: ResMut<Assets<crate::fx::Glow>>,
    mut images: ResMut<Assets<Image>>,
    mut bindposes: ResMut<Assets<SkinnedMeshInverseBindposes>>,
    encounter: Res<Encounter>,
    arena: Res<ArenaRes>,
) {
    let scout = &encounter.scout;
    let mut cache = assets::Cache::default();
    let mut builder = assets::Builder {
        library: &scout.character.library,
        meshes: &mut meshes,
        materials: &mut materials,
        surfaces: &mut surfaces,
        glows: &mut glows,
        images: &mut images,
        bindposes: &mut bindposes,
        cache: &mut cache,
    };
    for target in arena.0.targets() {
        let owner = commands
            .spawn((
                Transform::from_translation(game_to_bevy(target.pos)),
                Visibility::Inherited,
                EnemyTarget {
                    id: target.id,
                    combat: tf2_core::combat::Enemy::new(target.id, target.pos, scout),
                    health: scout.tuning.base_health,
                    down_for: 0.0,
                    reset_serial: 0,
                },
            ))
            .id();
        let (model, _) = builder.spawn(&mut commands, &scout.character.robot, "scout");
        commands.entity(model).insert(EnemyModel {
            owner,
            blend: Default::default(),
        });
        commands.entity(owner).add_child(model);
        if let Some(object) = scout
            .character
            .weapon_objects
            .get(scout.gun_index)
            .and_then(Option::as_ref)
        {
            let (gun, _) = builder.spawn(&mut commands, object, "scout gun");
            commands.entity(gun).insert((
                EnemyGun {
                    owner,
                    blend: Default::default(),
                },
                Transform::IDENTITY,
                Visibility::Hidden,
            ));
        }
    }
}

/// Whether his collision bodies that can hit are drawn (`--bodies`, or B in play).
#[derive(Resource, Default)]
pub struct ShowBodies(pub bool);

/// Draws the bodies an attack hits with while they are switched on, as the game's data
/// shapes them and the clip places them, and where each hit of this frame touched.
pub fn draw_bodies(
    keys: Res<ButtonInput<KeyCode>>,
    tuning: Res<TuningRes>,
    player: Single<&Player>,
    mut show: ResMut<ShowBodies>,
    mut gizmos: Gizmos,
) {
    if keys.just_pressed(KeyCode::KeyB) {
        show.0 = !show.0;
    }
    if !show.0 {
        return;
    }
    let colour = Color::srgb(1.0, 0.25, 0.9);
    for (_, body) in player.state.attack_bodies(&tuning.0) {
        let at = |local: Vec3| game_to_bevy(body.at.transform_point3(local));
        match body.shape {
            Shape::Sphere { radius } => {
                gizmos.sphere(at(Vec3::ZERO), radius, colour);
            }
            Shape::Cuboid { half } => {
                let corner = |i: usize| {
                    at(Vec3::from(half)
                        * Vec3::from(std::array::from_fn(|axis| {
                            if i >> axis & 1 == 1 { 1.0 } else { -1.0 }
                        })))
                };
                for i in 0..8usize {
                    for axis in 0..3 {
                        if i >> axis & 1 == 0 {
                            gizmos.line(corner(i), corner(i | 1 << axis), colour);
                        }
                    }
                }
            }
            Shape::Cylinder {
                radius,
                half_height,
            } => {
                const SIDES: usize = 24;
                let rim = |i: usize, z: f32| {
                    let (sin, cos) = (i as f32 / SIDES as f32 * std::f32::consts::TAU).sin_cos();
                    at(Vec3::new(cos * radius, sin * radius, z))
                };
                for i in 0..SIDES {
                    gizmos.line(rim(i, half_height), rim(i + 1, half_height), colour);
                    gizmos.line(rim(i, -half_height), rim(i + 1, -half_height), colour);
                    if i % 6 == 0 {
                        gizmos.line(rim(i, -half_height), rim(i, half_height), colour);
                    }
                }
            }
        }
    }
    // The ground punch's blast: its sphere as it grows.
    for blast in player.state.blasts.iter().filter(|blast| blast.live) {
        gizmos.sphere(
            game_to_bevy(blast.centre),
            blast.radius,
            Color::srgb(1.0, 0.6, 0.1),
        );
    }
    for hit in &player.state.hits {
        gizmos.sphere(game_to_bevy(hit.point), 0.3, Color::WHITE);
    }
}

/// Routes all hit packets through the same receiver and runs the scouts' combat.
pub fn update(
    time: Res<Time>,
    tuning: Res<TuningRes>,
    mut player: Single<&mut Player>,
    arsenal: Option<Res<crate::weapons::Arsenal>>,
    encounter: Res<Encounter>,
    sound: Option<Res<crate::sound::SoundOut>>,
    mut arena: ResMut<ArenaRes>,
    mut enemies: Query<(&mut EnemyTarget, &mut Transform, &mut Visibility)>,
    mut gizmos: Gizmos,
) {
    let scout = &encounter.scout;
    let dt = time.delta_secs().min(0.05);
    let mut outgoing = Vec::new();
    for (mut enemy, mut transform, mut visibility) in &mut enemies {
        if enemy.reset_serial != player.reset_serial {
            enemy.combat = tf2_core::combat::Enemy::new(enemy.id, enemy.combat.home, scout);
            enemy.reset_serial = player.reset_serial;
            *visibility = Visibility::Inherited;
            if encounter.report {
                eprintln!(
                    "combat: reset scout {}, player health {:.1}",
                    enemy.id, player.state.health
                );
            }
            if !arena.0.targets.iter().any(|t| t.id == enemy.id) {
                arena.0.targets.push(enemy.combat.target(&scout.tuning));
            }
        }
        if enemy.combat.state.restart_requested {
            continue;
        }
        let id = enemy.id;
        for hit in player.state.hits.iter().filter(|hit| hit.target == id) {
            let packet = tf2_core::damage::Packet::melee(hit, player.state.actor);
            let result = enemy.combat.receive(packet, scout);
            if encounter.report && result.accepted {
                eprintln!(
                    "combat: melee hit scout {}, damage {:.1}, health {:.1}, stun {:.2}",
                    id, result.amount, result.health, enemy.combat.state.damage_timers.stunned
                );
            }
        }
        for shot in arsenal
            .iter()
            .flat_map(|a| &a.struck)
            .filter(|s| s.target == id)
        {
            let result = enemy.combat.receive(
                tf2_core::damage::Packet {
                    source: Some(player.state.actor),
                    ..shot.packet
                },
                scout,
            );
            if encounter.report && result.accepted {
                eprintln!(
                    "combat: weapon hit scout {}, damage {:.1}, health {:.1}, flags {:x}",
                    id, result.amount, result.health, shot.packet.flags
                );
            }
        }
        for shot in enemy.combat.step(
            scout,
            &player.state,
            &tuning.0,
            &arena.0,
            encounter.passive,
            dt,
        ) {
            // [stand-in] Arena tracer drawing; original scout particle rendering is pending.
            gizmos.line(
                game_to_bevy(shot.from),
                game_to_bevy(shot.to),
                Color::srgb(1.0, 0.6, 0.12),
            );
            sound
                .as_deref()
                .inspect(|s| s.play_event_at(scout.character.weapons[scout.gun_index].fire_sound, shot.from));
            if let Some(id) = shot.target {
                outgoing.push((id, shot.packet));
            }
        }
        for hit in &enemy.combat.state.hits {
            outgoing.push((
                hit.target,
                tf2_core::damage::Packet::melee(hit, enemy.combat.state.actor),
            ));
        }
        enemy.health = enemy.combat.state.health;
        enemy.down_for = enemy.combat.state.death.as_ref().map_or(0.0, |d| d.age);
        transform.translation = game_to_bevy(enemy.combat.state.pos);
        transform.rotation = Quat::from_rotation_y(enemy.combat.state.yaw);
        if enemy.combat.state.dead() {
            arena.0.targets.retain(|t| t.id != enemy.id);
        } else if let Some(t) = arena.0.targets.iter_mut().find(|t| t.id == enemy.id) {
            *t = enemy.combat.target(&scout.tuning);
        }
        if enemy.combat.state.restart_requested {
            *visibility = Visibility::Hidden;
            if encounter.report {
                eprintln!(
                    "combat: scout {} removed after {:.2}s",
                    enemy.id, enemy.down_for
                );
            }
        }
    }
    for (id, packet) in outgoing {
        if id == player.state.actor.id {
            let result = player.state.receive_damage(packet, &tuning.0);
            if result.accepted && encounter.report {
                eprintln!(
                    "combat: scout {} hit player for {:.1}, health {:.1}",
                    packet.source.map_or(0, |s| s.id),
                    result.amount,
                    result.health
                );
            }
        } else if let Some((mut enemy, _, _)) = enemies.iter_mut().find(|(e, _, _)| e.id == id) {
            enemy.combat.receive(packet, scout);
        }
    }
}

/// Uses the scout's own clips on its model and weapon, including partial reactions.
pub fn animate(
    time: Res<Time>,
    encounter: Res<Encounter>,
    enemies: Query<&EnemyTarget>,
    mut models: Query<(&mut EnemyModel, &ObjectNodes)>,
    mut guns: Query<(Entity, &mut EnemyGun, &ObjectNodes, &mut Visibility)>,
    mut nodes: Query<&mut Transform>,
    mut commands: Commands,
) {
    let scout = &encounter.scout;
    for (mut model, object) in &mut models {
        let Ok(enemy) = enemies.get(model.owner) else {
            continue;
        };
        let s = &enemy.combat.state;
        let clip = s
            .action_clip(&scout.tuning)
            .map(|(set, id, tick)| (set, tf2_core::formats::hash::crc32(id.as_bytes()), tick))
            .or_else(|| {
                s.base_clip(&scout.tuning).map(|c| {
                    (
                        c.set,
                        tf2_core::formats::hash::crc32(c.id.as_bytes()),
                        c.tick,
                    )
                })
            });
        let mut pose = clip.map_or_else(
            || object.rest.clone(),
            |(set, id, tick)| {
                model.blend.pose(
                    scout
                        .character
                        .animations
                        .sets
                        .get(set)
                        .map_or(&[], Vec::as_slice),
                    id,
                    tick,
                    time.delta_secs(),
                    object,
                )
            },
        );
        if let Some(arm) = enemy.combat.weapon_pose.arm
            && let Some(r) = scout.gun_clips().arm.get(arm.clip)
            && let Some(clip) = r.clip.as_ref()
        {
            if r.additive {
                crate::animation::lay_offsets(
                    &mut pose,
                    clip,
                    arm.tick,
                    r.looping,
                    arm.weight(),
                    &object.name_hashes,
                );
            } else {
                let over =
                    crate::animation::sample(clip, r.looping, arm.tick, &object.name_hashes, &pose);
                crate::animation::cross_fade(&mut pose, &over, 1.0 - arm.weight());
            }
        }
        if let Some(r) = s
            .hit_partial
            .current(&scout.tuning.rule_sets)
            .and_then(|c| {
                scout
                    .character
                    .animations
                    .sets
                    .get("HitReactPartialSet")?
                    .iter()
                    .find(|r| tf2_core::formats::hash::crc32(r.id.as_bytes()) == c.id)
            })
            && let Some(clip) = r.clip.as_ref()
        {
            crate::animation::lay_offsets(
                &mut pose,
                clip,
                s.hit_partial.tick,
                r.looping,
                1.0,
                &object.name_hashes,
            );
        }
        for (&entity, transform) in object.entities.iter().zip(pose) {
            if let Ok(mut at) = nodes.get_mut(entity) {
                *at = transform;
            }
        }
        for (gun_entity, _, _, mut visibility) in guns
            .iter_mut()
            .filter(|(_, g, _, _)| g.owner == model.owner)
        {
            let def = &scout.character.weapons[scout.gun_index];
            let bone = if enemy.combat.weapon_pose.node != 0 {
                enemy.combat.weapon_pose.node
            } else {
                def.bone
            };
            if let Some(i) = object.name_hashes.iter().position(|&h| h == bone) {
                commands.entity(object.entities[i]).add_child(gun_entity);
            }
            *visibility = if enemy.combat.weapon_pose.shown && !s.dead() {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            };
        }
    }
    for (_, mut gun, object, _) in &mut guns {
        let Ok(enemy) = enemies.get(gun.owner) else {
            continue;
        };
        let pose = enemy.combat.weapon_pose.object.map_or_else(
            || object.rest.clone(),
            |c| {
                gun.blend.pose(
                    scout.gun_clips().object,
                    c.id,
                    c.tick,
                    time.delta_secs(),
                    object,
                )
            },
        );
        for (&entity, transform) in object.entities.iter().zip(pose) {
            if let Ok(mut at) = nodes.get_mut(entity) {
                *at = transform;
            }
        }
    }
}
