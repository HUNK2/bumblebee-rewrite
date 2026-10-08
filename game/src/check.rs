//! `--check`: load a character pack without opening a window and report what was found.

use std::collections::BTreeMap;
use std::path::Path;

use bevy::math::{Mat4, Quat, Vec3};

use crate::character::{self, CharacterData};
use crate::formats::hash::lower33;
use crate::formats::scene::ObjectDef;

pub fn run(game_dir: &Path, pack_name: &str) -> Result<(), String> {
    let data = character::load(game_dir, pack_name)?;
    let library = &data.library;
    println!(
        "{pack_name}: {} meshes, {} materials, {} skeletons, {} textures",
        library.meshes.len(),
        library.materials.len(),
        library.skeletons.len(),
        library.textures.len()
    );
    for problem in &library.problems {
        println!("  problem: {problem}");
    }
    report("robot", &data.robot, &data);
    match &data.vehicle {
        Some(vehicle) => report("vehicle", vehicle, &data),
        None => println!("vehicle: none found"),
    }
    animations(&data);
    Ok(())
}

/// Rest transform of every node relative to its parent.
pub fn rest_locals(object: &ObjectDef) -> Vec<Mat4> {
    let world: Vec<Mat4> = object
        .nodes
        .iter()
        .map(|n| {
            let r = n.bind.map(Vec3::from);
            Mat4::from_cols(r[0].extend(0.0), r[1].extend(0.0), r[2].extend(0.0), r[3].extend(1.0))
        })
        .collect();
    object.nodes.iter().enumerate().map(|(i, n)| n.parent.map_or(world[i], |p| world[p].inverse() * world[i])).collect()
}

fn animations(data: &CharacterData) {
    let sets = &data.animations.sets;
    let clips: Vec<_> = sets.values().flatten().filter_map(|r| r.clip.as_ref()).collect();
    println!("animations: {} sets, {} clips", sets.len(), clips.len());
    let mut kinds: BTreeMap<(i64, &str), usize> = BTreeMap::new();
    for clip in &clips {
        for (stream_type, class) in &clip.stream_kinds {
            *kinds.entry((*stream_type, class)).or_default() += 1;
        }
    }
    for ((stream_type, class), count) in kinds {
        println!("  stream type {stream_type:#x} with {class}: {count} streams");
    }
    for (set, id) in [("WalkSet", "Run"), ("WalkSet", "Walk"), ("IdleStandSet", "Idle")] {
        match data.animations.find(set, id).and_then(|r| r.clip.as_ref().map(|c| (r, c))) {
            Some((r, clip)) => println!(
                "  {set}/{id}: {} ticks, loops {}, crossfade {} s, root moves {:.2} forward, {:.2} per second",
                clip.length,
                r.looping,
                r.crossfade,
                clip.end[3][1] - clip.start[3][1],
                clip.forward_speed()
            ),
            None => println!("  {set}/{id}: not found"),
        }
    }

    // How far is the idle clip's first frame from the rest pose? Small numbers on the bones an
    // idle stance does not bend mean positions and rotations are being read the right way.
    let Some(idle) = data.animations.find("IdleStandSet", "Idle").and_then(|r| r.clip.as_ref()) else { return };
    let rest = rest_locals(&data.robot);
    println!("  idle clip, first frame against the rest pose (distance, angle as stored, angle if conjugated):");
    for (i, node) in data.robot.nodes.iter().enumerate() {
        let Some(channel) = idle.channel(node.name_hash) else { continue };
        let (_, rest_rotation, rest_position) = rest[i].to_scale_rotation_translation();
        let position = channel.position.sample(0.0, idle.length, true).map(Vec3::from);
        let rotation = channel.rotation.sample(0.0, idle.length, true).map(Quat::from_array);
        let scale = channel.scale.sample(0.0, idle.length, true);
        println!(
            "    {:24} pos {:>7} rot {:>7} / {:>7}{}",
            node.name,
            position.map_or("-".into(), |p| format!("{:.3}", p.distance(rest_position))),
            rotation.map_or("-".into(), |q| format!("{:.1}", q.angle_between(rest_rotation).to_degrees())),
            rotation.map_or("-".into(), |q| format!("{:.1}", q.conjugate().angle_between(rest_rotation).to_degrees())),
            scale.map_or(String::new(), |s| format!("  scale {s:?}")),
        );
    }
}

fn report(label: &str, object: &ObjectDef, data: &CharacterData) {
    let library = &data.library;
    let (mut vertices, mut triangles, mut missing, mut untextured) = (0, 0, 0, 0);
    let (mut agree, mut disagree) = (0u32, 0u32);
    let mut height = (f32::MAX, f32::MIN);
    for render in &object.renders {
        let Some(mesh) = library.meshes.get(&lower33(&render.model)) else {
            missing += 1;
            println!("  {label}: no mesh for {}", render.model);
            continue;
        };
        let bind = object.nodes[render.node].bind;
        for part in &mesh.parts {
            vertices += part.positions.len();
            triangles += part.indices.len() / 3;
            let material = part.material.and_then(|id| library.materials.get(&id));
            let texture = material.and_then(|m| m.texture("diffuse_texture1_texture"));
            if texture.is_none_or(|id| !library.textures.contains_key(&id)) {
                untextured += 1;
            }
            if let Some(top) = part.blend_indices.iter().flatten().copied().reduce(f32::max) {
                println!(
                    "  {label}: {} skinned part, palette {:?}, highest bone number {top}, skeleton {}",
                    mesh.name,
                    part.palette,
                    mesh.skeleton.is_some_and(|id| library.skeletons.contains_key(&id)),
                );
            }
            for p in &part.positions {
                // Rigid parts are stored in their node's space; skinned ones in the object's.
                let z = if part.palette.is_empty() {
                    p[0] * bind[0][2] + p[1] * bind[1][2] + p[2] * bind[2][2] + bind[3][2]
                } else {
                    p[2]
                };
                height = (height.0.min(z), height.1.max(z));
            }
            // Which way round are triangles wound, judged against the stored normals?
            for t in part.indices.chunks_exact(3) {
                let [a, b, c] = [t[0], t[1], t[2]].map(|i| part.positions[i as usize]);
                let (u, v) = (sub(b, a), sub(c, a));
                let face = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
                let n = part.normals.get(t[0] as usize).copied().unwrap_or_default();
                if face[0] * n[0] + face[1] * n[1] + face[2] * n[2] >= 0.0 { agree += 1 } else { disagree += 1 }
            }
        }
    }
    println!(
        "{label}: {} nodes, {} rendered parts ({missing} without a mesh), {vertices} vertices, {triangles} triangles, \
         {untextured} parts without a colour texture",
        object.nodes.len(),
        object.renders.len(),
    );
    println!("  {label}: height range {:.2} to {:.2}; counter-clockwise triangles {agree}, clockwise {disagree}", height.0, height.1);
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
