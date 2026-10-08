//! The raw keys of a clip's channels at one tick: each node's own position and how far its
//! rotation is from none, in degrees. For telling an additive clip (small turns, positions
//! of zero) from one that holds whole poses.
//! `cargo run -p tf2-core --example layer_peek -- <Set> <Id> <tick>`.

use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = std::env::var("TF2_GAME_DIR").unwrap_or_else(|_| "C:/Games2".into());
    let data = tf2_core::character::load(Path::new(&dir), "bumblebee").expect("load");
    let r = data.animations.find(&args[0], &args[1]).expect("clip");
    let clip = r.clip.as_ref().unwrap();
    let tick: f32 = args[2].parse().unwrap();
    println!("{}/{}: {} ticks, loop {}, blend {}", args[0], args[1], clip.length, r.looping, r.crossfade);
    for n in &data.robot.nodes {
        let Some(channel) = clip.channel(n.name_hash) else { continue };
        let p = channel.position.sample(tick, clip.length, r.looping);
        let q = channel.rotation.sample(tick, clip.length, r.looping);
        let turn = q.map(|q| 2.0 * q[3].abs().min(1.0).acos().to_degrees());
        println!("  {:<26} position {:?}  turn {:?} quat {:?}", n.name, p, turn, q);
    }
}
