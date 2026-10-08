//! Every particle effect his pack starts and who starts it: the clips' effect events, the
//! footstep and melee presets, the effects on his objects and the weapons' effects, each
//! with what the port does not run of it.
//! `cargo run -p tf2-core --example effects_dump` (reads `C:\Games2`).

use std::path::Path;

use tf2_core::formats::anim::EffectEventKind;
use tf2_core::particles::Template;

fn about(template: &Template) -> String {
    // Each element's renderer, with an `l` when its particles are kept in the emitter's frame.
    let codes: Vec<String> = template
        .elements
        .iter()
        .map(|e| format!("{}{}", String::from_utf8_lossy(&e.renderer.id.to_be_bytes()), if e.local_space { "l" } else { "" }))
        .collect();
    let warped = template.elements.iter().filter(|e| e.renderer.distortion).count();
    format!("{:08x} {} elements [{}] warping {} unread {:?}", template.name, template.elements.len(), codes.join(" "), warped, template.unread())
}

fn main() {
    let dir = std::env::var("TF2_GAME_DIR").unwrap_or_else(|_| "C:/Games2".into());
    let data = tf2_core::character::load(Path::new(&dir), "bumblebee").expect("load");
    println!("== clip events");
    let mut sets: Vec<_> = data.animations.sets.iter().collect();
    sets.sort_by_key(|(name, _)| name.as_str());
    for (set, refs) in sets {
        for r in refs {
            let Some(clip) = &r.clip else { continue };
            for event in &clip.effect_events {
                match &event.kind {
                    EffectEventKind::Spawn { id, template, track, use_rotation, use_scale, .. } => println!(
                        "{set}/{} t={} node {:08x} id {id:08x} track {track} rotation {use_rotation} scale {use_scale}: {}",
                        r.id,
                        event.time,
                        event.node,
                        about(template)
                    ),
                    EffectEventKind::Stop { id } => println!("{set}/{} t={} stop {id:08x}", r.id, event.time),
                    EffectEventKind::FootStep { .. } | EffectEventKind::ClimbStep { .. } => {}
                }
            }
        }
    }
    println!("== presets");
    for (name, preset) in [("step", &data.step_impact), ("melee", &data.melee_impact)] {
        for info in &preset.infos {
            println!("{name} {:08x?} flags {:#x}: {}", info.materials, info.effect_flags, info.template.as_ref().map_or("none".into(), about));
        }
    }
    println!("== on his objects");
    for (name, object) in [("robot", Some(&data.robot)), ("vehicle", data.vehicle.as_ref())] {
        for def in object.into_iter().flat_map(|o| &o.effects) {
            println!("{name} node {} script {:08x} attach {} mesh {:?}: {}", def.node, def.script_name, def.attach, def.mesh, about(&def.template));
        }
    }
    println!("== weapons");
    for w in &data.weapon_effects {
        println!("weapon {:08x}", w.name);
        for (what, t) in [("muzzle", &w.muzzle_flash), ("trail", &w.trail), ("blast", &w.blast)] {
            println!("  {what}: {}", t.as_ref().map_or("none".into(), about));
        }
        for info in &w.impact.infos {
            println!("  impact {:08x?} flags {:#x}: {}", info.materials, info.effect_flags, info.template.as_ref().map_or("none".into(), about));
        }
    }
}
