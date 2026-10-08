//! The aim offset clips a clip carries (`additiveAnimFileList`, `Down` then `Up`): each
//! one's length and, for every node it has a channel for, its position and turn at the
//! first and the last tick. Without a clip id: which clips of the set carry any.
//! `cargo run -p tf2-core --example aim_peek -- <Set> [Id]`.

use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = std::env::var("TF2_GAME_DIR").unwrap_or_else(|_| "C:/Games2".into());
    let data = tf2_core::character::load(Path::new(&dir), "bumblebee").expect("load");
    let Some(id) = args.get(1) else {
        for (name, set) in &data.animations.sets {
            if !args.is_empty() && *name != args[0] {
                continue;
            }
            for r in set.iter().filter(|r| !r.aim_offsets.is_empty()) {
                let lengths: Vec<f32> = r.aim_offsets.iter().map(|c| c.length).collect();
                println!("{name}/{}: {} offset clips, lengths {lengths:?}", r.id, r.aim_offsets.len());
            }
        }
        return;
    };
    let r = data.animations.find(&args[0], id).expect("clip");
    // Where the gun's node points (its x lies along the forearm) with each offset clip
    // laid on by a share, as the engine lays it on.
    if let Some(base) = &r.clip {
        let rig = tf2_core::pose::Rig::new(&data.robot);
        let node = data.robot.nodes.iter().position(|n| n.name == "WeaponAttachment_Arm_R").expect("weapon node");
        let head = data.robot.nodes.iter().position(|n| n.name == "Head").expect("head");
        for (i, over) in r.aim_offsets.iter().enumerate() {
            for weight in [0.0, 0.5, 1.0] {
                let pose = rig.pose_offset(base, r.looping, 0.0, over, weight);
                let along = pose[node].matrix3.x_axis.normalize();
                let rise = along.z.atan2(along.truncate().length()).to_degrees();
                let look = pose[head].matrix3.y_axis.normalize();
                let head_rise = look.z.atan2(look.truncate().length()).to_degrees();
                println!(
                    "offset clip {i} at {weight}: gun along ({:.2}, {:.2}, {:.2}), {rise:.1} degrees up, at {:.2?}; head's y {head_rise:.1} degrees up",
                    along.x,
                    along.y,
                    along.z,
                    pose[node].translation
                );
            }
        }
    }
    for (i, clip) in r.aim_offsets.iter().enumerate() {
        println!("offset clip {i}: {} ticks, {} channels", clip.length, clip.names.len());
        for n in &data.robot.nodes {
            let Some(channel) = clip.channel(n.name_hash) else { continue };
            let at = |tick: f32| {
                let p = channel.position.sample(tick, clip.length, false);
                let q = channel.rotation.sample(tick, clip.length, false);
                let s = channel.scale.sample(tick, clip.length, false);
                (p, q.map(|q| 2.0 * q[3].abs().min(1.0).acos().to_degrees()), q, s)
            };
            let (first, last) = (at(0.0), at(clip.length));
            println!("  {:<26} {:?}", n.name, first);
            if first != last {
                println!("  {:<26} at the end {:?}", "", last);
            }
        }
    }
}
