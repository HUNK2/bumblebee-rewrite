//! Inspect only the SSD resources needed by the port, from the user's install.
use std::path::Path;
use tf2_core::{
    character,
    formats::{hash::lower33, model::Library, pack::Pack},
};
fn main() -> Result<(), String> {
    let dir = std::env::var_os("TF2_GAME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "C:/Games2".into());
    let data = character::load(&dir, "bumblebee")?;
    let noise = tf2_core::formats::texture::parse(
        &std::fs::read(dir.join("Textures/NoiseVolume.apk")).map_err(|e| e.to_string())?,
    )?;
    if let Some(t) = noise {
        println!(
            "loose noise {} {:08x} {}x{} {:?}",
            t.name, t.id, t.width, t.height, t.format
        );
    }
    for s in &data.robot.spawners {
        println!("spawner {:08x} SSD {:?}", s.script_name, s.ssd);
    }
    println!("melee SSD {:?}", data.melee_impact.ssd);
    for w in &data.weapon_effects {
        println!(
            "weapon {:08x} impact {:?}, blast {:?}",
            w.name, w.impact.ssd, w.blast_ssd
        );
    }
    for path in [
        dir.join("bnxglobal.str"),
        dir.join("levels/us_west_town/zone.str"),
    ] {
        let pack = Pack::open(Path::new(&path))?;
        let lib = Library::load(&pack);
        for t in lib.textures.values().filter(|t| {
            t.id == lower33("NoiseVolume2D") || t.name.to_lowercase().contains("noisevolume")
        }) {
            println!(
                "noise {} {:08x} {}x{} {:?}",
                t.name, t.id, t.width, t.height, t.format
            );
        }
        let mut mats: Vec<_> = lib
            .materials
            .iter()
            .filter(|(_, m)| {
                m.texture("cracks_map_texture").is_some()
                    && m.texture("damage_map_texture").is_some()
            })
            .collect();
        mats.sort_by_key(|(id, _)| **id);
        for (_, m) in mats.into_iter().take(3) {
            println!(
                "material {} cracks {:?} damage {:?}",
                m.name,
                m.texture("cracks_map_texture")
                    .and_then(|id| lib.textures.get(&id))
                    .map(|t| (&t.name, t.width, t.height)),
                m.texture("damage_map_texture")
                    .and_then(|id| lib.textures.get(&id))
                    .map(|t| (&t.name, t.width, t.height))
            );
        }
    }
    Ok(())
}
