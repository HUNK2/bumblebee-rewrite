//! Where an additive clip of the weapon layer puts his arm over a pose, both ways its
//! turns could be meant (in the node's own frame, or in its parent's), to see which one
//! makes sense. `cargo run -p tf2-core --example offsets_check -- <Set> <Id> <tick>
//! <OverSet> <OverId> <tick> [node ...]`.

use std::path::Path;

use tf2_core::pose::Rig;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = std::env::var("TF2_GAME_DIR").unwrap_or_else(|_| "C:/Games2".into());
    let data = tf2_core::character::load(Path::new(&dir), "bumblebee").expect("load");
    let base = data.animations.find(&args[0], &args[1]).expect("clip");
    let over = data.animations.find(&args[3], &args[4]).expect("clip over");
    let (clip, over_clip) = (base.clip.as_ref().unwrap(), over.clip.as_ref().unwrap());
    let (tick, over_tick): (f32, f32) = (args[2].parse().unwrap(), args[5].parse().unwrap());
    let rig = Rig::new(&data.robot);
    let plain = rig.pose(clip, base.looping, tick);
    let own = rig.pose_with_offsets(clip, base.looping, tick, over_clip, over_tick, true);
    let parent = rig.pose_with_offsets(clip, base.looping, tick, over_clip, over_tick, false);
    for (i, n) in data.robot.nodes.iter().enumerate() {
        if args.len() > 6 && !args[6..].contains(&n.name) {
            continue;
        }
        let show = |m: &glam::Affine3A| {
            format!(
                "at ({:6.2} {:6.2} {:6.2}) x axis ({:5.2} {:5.2} {:5.2})",
                m.translation.x, m.translation.y, m.translation.z, m.matrix3.x_axis.x, m.matrix3.x_axis.y, m.matrix3.x_axis.z
            )
        };
        println!("{:<24} pose {}\n{:24} own frame {}\n{:24} parent's  {}", n.name, show(&plain[i]), "", show(&own[i]), "", show(&parent[i]));
    }
}
