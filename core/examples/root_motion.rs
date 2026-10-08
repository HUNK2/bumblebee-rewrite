//! Prints which channels of a clip move forward over time, to find where root motion is kept.
//! `cargo run -p tf2-core --example root_motion -- <Set> <Id>` (reads `C:\Games2`).

use std::path::Path;

fn main() {
    let mut args = std::env::args().skip(1);
    let set = args.next().unwrap_or_else(|| "IdleStandSet".into());
    let id = args.next().unwrap_or_else(|| "Run2Idle".into());
    let dir = std::env::var("TF2_GAME_DIR").unwrap_or_else(|_| "C:/Games2".into());
    let data = tf2_core::character::load(Path::new(&dir), "bumblebee").expect("load");
    let anim = data.animations.find(&set, &id).expect("no such clip");
    let clip = anim.clip.as_ref().unwrap();
    println!("{set}/{id}: {} ticks, start {:?}, end {:?}", clip.length, clip.start[3], clip.end[3]);
    for (i, channel) in clip.channels.iter().enumerate() {
        let at = |t: f32| channel.position.sample(t, clip.length, anim.looping);
        let (Some(a), Some(b)) = (at(0.0), at(clip.length)) else { continue };
        let moved = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
        if moved > 0.5 || i == 0 {
            print!("channel {i} ({:08x}) moves {moved:.2}:", clip.names[i]);
            let mut t = 0.0;
            while t <= clip.length {
                let p = at(t).unwrap();
                print!(" {:.2}", p[1]);
                t += 2.0;
            }
            println!();
        }
    }
}
