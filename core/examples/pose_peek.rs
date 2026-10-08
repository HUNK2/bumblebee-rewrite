//! A few nodes' places and axes in two clips at one tick, for comparing poses by eye.
//! `cargo run -p tf2-core --example pose_peek -- <Set> <Id> <tick> [node ...]`.

use std::path::Path;

use tf2_core::pose::Rig;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = std::env::var("TF2_GAME_DIR").unwrap_or_else(|_| "C:/Games2".into());
    let data = tf2_core::character::load(Path::new(&dir), "bumblebee").expect("load");
    let r = data.animations.find(&args[0], &args[1]).expect("clip");
    let clip = r.clip.as_ref().unwrap();
    let tick: f32 = args[2].parse().unwrap();
    let rig = Rig::new(&data.robot);
    let (pose, rest) = (rig.pose(clip, r.looping, tick), rig.rest());
    for (i, n) in data.robot.nodes.iter().enumerate() {
        if args.len() > 3 && !args[3..].contains(&n.name) {
            continue;
        }
        let (m, b) = (pose[i], rest[i]);
        println!(
            "{i:3} {:<26} parent {:<4} channel {:<5} at ({:6.2} {:6.2} {:6.2}) rest ({:6.2} {:6.2} {:6.2})  x axis ({:5.2} {:5.2} {:5.2}) rest ({:5.2} {:5.2} {:5.2})",
            n.name,
            n.parent.map_or("-".into(), |p| p.to_string()),
            clip.channel(n.name_hash).is_some(),
            m.translation.x, m.translation.y, m.translation.z,
            b.translation.x, b.translation.y, b.translation.z,
            m.matrix3.x_axis.x, m.matrix3.x_axis.y, m.matrix3.x_axis.z,
            b.matrix3.x_axis.x, b.matrix3.x_axis.y, b.matrix3.x_axis.z,
        );
    }
}
