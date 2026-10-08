//! Every clip of one animation set: length, loop, blend, speed, root travel and events.
//! `cargo run -p tf2-core --example set_dump -- <Set>` (reads `C:\Games2`).

use std::path::Path;

fn main() {
    let set = std::env::args().nth(1).unwrap_or_else(|| "ClimbSet".into());
    let dir = std::env::var("TF2_GAME_DIR").unwrap_or_else(|_| "C:/Games2".into());
    let data = tf2_core::character::load(Path::new(&dir), "bumblebee").expect("load");
    let Some(refs) = data.animations.sets.get(&set) else {
        println!("no set {set}; sets: {:?}", data.animations.sets.keys().collect::<Vec<_>>());
        return;
    };
    for r in refs {
        print!("{:<24} loop {:<5} xfade {:.2} speed {:.2}", r.id, r.looping, r.crossfade, r.speed);
        let Some(clip) = &r.clip else {
            println!("  (no clip)");
            continue;
        };
        let (s, e) = (clip.start[3], clip.end[3]);
        println!(
            "  {:>4} ticks  root start ({:.2} {:.2} {:.2}) end ({:.2} {:.2} {:.2}) travel ({:.2} {:.2} {:.2})",
            clip.length, s[0], s[1], s[2], e[0], e[1], e[2], e[0] - s[0], e[1] - s[1], e[2] - s[2]
        );
        // How far the root turns about the vertical over the clip, from its forward rows.
        let turn = |m: &[[f32; 3]; 4]| (-m[1][0]).atan2(m[1][1]).to_degrees();
        let turned = clip.root_turn();
        let last = turned.last().copied().unwrap_or(0.0).to_degrees();
        if (turn(&clip.end) - turn(&clip.start)).abs() > 1.0 || last.abs() > 1.0 {
            println!(
                "    root turns {:.1} -> {:.1} degrees (matrices); {:.1} by the root channel",
                turn(&clip.start),
                turn(&clip.end),
                last
            );
        }
        let path = clip.root_path();
        if path.len() > 1 {
            let step = (path.len() / 8).max(1);
            let pts: Vec<String> = path
                .iter()
                .step_by(step)
                .map(|p| format!("({:.1} {:.1} {:.1})", p.x, p.y, p.z))
                .collect();
            println!("    root path: {}", pts.join(" "));
        }
        for ev in &clip.action_events {
            println!("    action @{:.0}: {:?}", ev.time, ev.action);
        }
        for ev in &clip.sound_events {
            println!("    sound  @{:.0}: {:?}", ev.time, ev);
        }
        for ev in &clip.shake_events {
            println!("    shake  @{:.0}: {:08x}{}", ev.time, ev.name, if ev.positional { " at his place" } else { " everywhere" });
        }
    }
}
