//! A weapon object's own clips: which nodes each moves, and where its nodes sit (in the
//! object's space) at rest and in a clip. For working out how the gun hangs on his arm.
//! `cargo run -p tf2-core --example weapon_pose -- [weapon index] [clip id] [tick]`

use std::path::Path;

use tf2_core::pose::Rig;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = std::env::var("TF2_GAME_DIR").unwrap_or_else(|_| "C:/Games2".into());
    let data = tf2_core::character::load(Path::new(&dir), "bumblebee").expect("load");
    // `axe` in place of the index looks at the melee weapon.
    let (object, clips) = match args.first().map(String::as_str) {
        Some("axe") => {
            let weapon = data.melee_weapon.as_ref().expect("a melee weapon");
            (weapon.object.as_ref().expect("its object"), &weapon.clips)
        }
        index => {
            let index: usize = index.map_or(0, |a| a.parse().unwrap());
            (data.weapon_objects[index].as_ref().expect("the weapon object"), &data.weapon_animations[index])
        }
    };
    for r in clips {
        let Some(clip) = &r.clip else { continue };
        let moved: Vec<&str> = object.nodes.iter().filter(|n| clip.channel(n.name_hash).is_some()).map(|n| n.name.as_str()).collect();
        println!("{} loop {} {} ticks, start {:?} end {:?}", r.id, r.looping, clip.length, clip.start, clip.end);
        println!("    moves {} of {} nodes: {}", moved.len(), object.nodes.len(), moved.join(" "));
    }
    let rig = Rig::new(object);
    let rest = rig.rest();
    let posed = args.get(1).and_then(|id| clips.iter().find(|r| &r.id == id)).and_then(|r| {
        let tick = args.get(2).map_or(0.0, |t| t.parse().unwrap());
        r.clip.as_ref().map(|clip| rig.pose(clip, r.looping, tick))
    });
    for (i, n) in object.nodes.iter().enumerate() {
        let line = |label: &str, m: glam::Affine3A| {
            let (x, y, z, t) = (m.matrix3.x_axis, m.matrix3.y_axis, m.matrix3.z_axis, m.translation);
            println!(
                "    {label}  x ({:5.2} {:5.2} {:5.2})  y ({:5.2} {:5.2} {:5.2})  z ({:5.2} {:5.2} {:5.2})  at ({:5.2} {:5.2} {:5.2})",
                x.x, x.y, x.z, y.x, y.y, y.z, z.x, z.y, z.z, t.x, t.y, t.z
            );
        };
        println!("{i:3} {} (parent {:?})", n.name, n.parent);
        line("rest", rest[i]);
        if let Some(posed) = &posed {
            line("clip", posed[i]);
        }
    }
}
