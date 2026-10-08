//! Prints a character's weapons as read from the install:
//! `cargo run --release -p tf2-core --example weapons_check -- [game dir] [pack]`

use std::path::PathBuf;

use tf2_core::{character, weapons};

fn main() {
    let mut args = std::env::args().skip(1);
    let game_dir = PathBuf::from(args.next().unwrap_or_else(|| r"C:\Games2".to_owned()));
    let pack = args.next().unwrap_or_else(|| "bumblebee".to_owned());
    let data = character::load(&game_dir, &pack).expect("the character pack");
    let tunings = weapons::load(&game_dir, &data.weapons).expect("the weapon attributes");
    println!("{} weapons", data.weapons.len());
    for (def, tuning) in data.weapons.iter().zip(&tunings) {
        println!("{def:#010x?}\n{tuning:#?}");
    }
    // Each weapon's own object: its nodes (rest place in the object) and what it renders.
    for (def, object) in data.weapons.iter().zip(&data.weapon_objects) {
        let Some(object) = object else {
            println!("weapon {:#010x}: no object", def.name);
            continue;
        };
        println!("weapon {:#010x} object: {} nodes, {} rendered", def.name, object.nodes.len(), object.renders.len());
        for (i, node) in object.nodes.iter().enumerate() {
            println!("  node {i} {:<28} parent {:?} bind {:?}", node.name, node.parent, node.bind);
        }
        for render in &object.renders {
            let found = data.library.meshes.contains_key(&tf2_core::formats::hash::lower33(&render.model));
            println!("  renders {} on node {} (mesh in the pack: {found})", render.model, render.node);
        }
    }
}
