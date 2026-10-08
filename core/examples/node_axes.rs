//! A node's three axes and place in a clip's pose and in the rest pose, in his own space
//! (x right, y forward, z up). For working out how something hangs on a node.
//! `cargo run -p tf2-core --example node_axes -- <Set> <Id> <tick> <node>...`

use std::path::Path;

use tf2_core::pose::Rig;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = std::env::var("TF2_GAME_DIR").unwrap_or_else(|_| "C:/Games2".into());
    let data = tf2_core::character::load(Path::new(&dir), "bumblebee").expect("load");
    let r = data.animations.find(&args[0], &args[1]).expect("clip");
    let clip = r.clip.as_ref().unwrap();
    let tick: f32 = args[2].parse().unwrap();
    println!("root at the clip's start: {:?}", clip.start);
    let rig = Rig::new(&data.robot);
    let (pose, rest) = (rig.pose(clip, r.looping, tick), rig.rest());
    for (i, n) in data.robot.nodes.iter().enumerate() {
        if !args[3..].contains(&n.name) {
            continue;
        }
        println!("{i:3} {}", n.name);
        for (label, m) in [("pose", pose[i]), ("rest", rest[i])] {
            let (x, y, z, t) = (m.matrix3.x_axis, m.matrix3.y_axis, m.matrix3.z_axis, m.translation);
            println!(
                "    {label}  x ({:5.2} {:5.2} {:5.2})  y ({:5.2} {:5.2} {:5.2})  z ({:5.2} {:5.2} {:5.2})  at ({:5.2} {:5.2} {:5.2})",
                x.x, x.y, x.z, y.x, y.y, y.z, z.x, z.y, z.z, t.x, t.y, t.z
            );
        }
    }
}
