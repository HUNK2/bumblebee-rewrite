//! Which way each clip of a set turns the root, the hips and the chest about the vertical,
//! measured from the rest pose, with the root's own turn left out of the pose (as the
//! drawing side has it). `cargo run -p tf2-core --example facing_dump -- <Set>`.

use std::path::Path;

use glam::{Quat, Vec3};
use tf2_core::formats::anim::ROOT_CHANNEL;
use tf2_core::pose::Rig;

fn main() {
    let set = std::env::args().nth(1).unwrap_or_else(|| "WeaponSet".into());
    let dir = std::env::var("TF2_GAME_DIR").unwrap_or_else(|_| "C:/Games2".into());
    let data = tf2_core::character::load(Path::new(&dir), "bumblebee").expect("load");
    let Some(refs) = data.animations.sets.get(&set) else {
        println!("no set {set}");
        return;
    };
    let rig = Rig::new(&data.robot);
    let rest = rig.rest();
    let node = |name: &str| data.robot.nodes.iter().position(|n| n.name == name);
    // Degrees to the left that a node's rest forward (y) has been turned.
    // (Taken from the matrices themselves: a mirrored node has no rotation to take out.)
    let yaw = |m: &glam::Affine3A, rest: &glam::Affine3A| {
        let forward = (m.matrix3 * rest.matrix3.inverse()) * glam::Vec3A::Y;
        (-forward.x).atan2(forward.y).to_degrees()
    };
    println!("{:<28} {:>9} {:>9} {:>9} {:>9}   (degrees to the left; root is its channel's turn)", "clip", "root", "Hips", "Spine_03", "Head");
    for r in refs {
        let Some(clip) = &r.clip else { continue };
        let tick = clip.length / 2.0;
        let pose = rig.pose(clip, r.looping, tick);
        let root = clip
            .channel(ROOT_CHANNEL)
            .and_then(|c| c.rotation.sample(tick, clip.length, r.looping))
            .map(|q| Quat::from_xyzw(-q[0], -q[1], -q[2], q[3]).normalize() * Vec3::Y)
            .map_or(0.0, |f| (-f.x).atan2(f.y).to_degrees());
        let of = |name: &str| node(name).map_or(f32::NAN, |i| yaw(&pose[i], &rest[i]));
        println!("{:<28} {root:9.1} {:9.1} {:9.1} {:9.1}", r.id, of("Hips"), of("Spine_03"), of("Head"));
        // What else the root's channel holds, and the nodes straight under the object.
        if std::env::args().nth(2).is_some() {
            let root = clip.channel(ROOT_CHANNEL);
            let scale = root.and_then(|c| c.scale.sample(tick, clip.length, r.looping));
            let turn = root.and_then(|c| c.rotation.sample(tick, clip.length, r.looping));
            println!("    root channel: scale {scale:?}, rotation {turn:?}");
            for (i, n) in data.robot.nodes.iter().enumerate().filter(|(_, n)| n.parent.is_none_or(|p| p == 0)) {
                println!("    node {i} {} (parent {:?}): turned {:.1}, has a channel: {}", n.name, n.parent, yaw(&pose[i], &rest[i]), clip.channel(n.name_hash).is_some());
            }
        }
    }
}
