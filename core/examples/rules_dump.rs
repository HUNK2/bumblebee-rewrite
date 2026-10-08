//! The entry rules of one animation set and the exit rules of each of its clips, with the
//! names resolved. `cargo run -p tf2-core --example rules_dump -- <Set>` (reads `C:\Games2`).

use std::collections::HashMap;
use std::path::Path;

use tf2_core::formats::anim::Rule;
use tf2_core::formats::hash::crc32;

const VARIABLES: [&str; 31] = [
    "F_STUN_TIME", "F_RANDOM", "I_IN_AIR", "I_SPECIAL_ON", "F_R_STICK_AMP", "F_TIME_SINCE_DAMAGED", "C_JUMP_TYPE",
    "F_Z_VEL", "I_IS_STRAFING", "C_VEHICLE_STUNT", "I_WEAPON_INDEX", "C_INTERACT_TYPE", "C_CHAR_MESSAGE",
    "C_DAMAGE_TYPE", "F_L_STICK_DIR", "I_ATTK_BTN_HELD", "I_SPECIAL_IDLE_ON", "I_ON_GROUND", "F_TIME_IN_ANIM",
    "F_FALL_DUR", "F_R_STICK_DIR", "I_ATTK_BRANCH", "I_IS_HOVERING", "I_PREV_CHAR_FORM", "I_CHAR_FORM",
    "C_INTERACT_ACTION_TYPE", "C_WEAPON_ACTION", "F_L_STICK_AMP", "F_STRAFE_ANG", "C_WEAPON_EQUIP", "F_HP_PERCENT",
];
const LOGIC: [&str; 6] = ["==", "!=", ">", ">=", "<", "<="];

fn main() {
    let set = std::env::args().nth(1).unwrap_or_else(|| "WeaponSet".into());
    let dir = std::env::var("TF2_GAME_DIR").unwrap_or_else(|_| "C:/Games2".into());
    let data = tf2_core::character::load(Path::new(&dir), "bumblebee").expect("load");
    let animations = &data.animations;
    let Some(refs) = animations.sets.get(&set) else {
        println!("no set {set}; sets: {:?}", animations.sets.keys().collect::<Vec<_>>());
        return;
    };
    let mut names: HashMap<u32, String> = VARIABLES.iter().map(|n| (crc32(n.as_bytes()), n.to_string())).collect();
    for (name, refs) in &animations.sets {
        names.insert(crc32(name.as_bytes()), name.clone());
        names.extend(refs.iter().map(|r| (crc32(r.id.as_bytes()), r.id.clone())));
    }
    let name = |hash: u32| match hash {
        0 => "any".to_owned(),
        hash => names.get(&hash).cloned().unwrap_or_else(|| format!("#{hash:08x}")),
    };
    let rules = |rules: &[Rule]| {
        let one = |r: &Rule| {
            let variable = name(r.variable);
            let value = match variable.as_bytes().first() {
                Some(b'I') => r.int.to_string(),
                Some(b'C') => name(r.hash),
                _ => r.float.to_string(),
            };
            format!("{variable} {} {value}", LOGIC.get(r.logic as usize).unwrap_or(&"?"))
        };
        if rules.is_empty() { "always".to_owned() } else { rules.iter().map(one).collect::<Vec<_>>().join(" and ") }
    };
    println!("entry rules of {set}:");
    for inc in animations.incoming.get(&set).into_iter().flatten() {
        println!(
            "  from {}/{} -> {}/{}  when {}",
            name(inc.from_set),
            name(inc.from_id),
            name(inc.to_set),
            name(inc.to_id),
            rules(&inc.rules)
        );
    }
    for r in refs {
        println!(
            "{}{}  (blend in {:.2}{})",
            r.id,
            if r.keeps_time { ", keeps the time of the clip before" } else { "" },
            r.crossfade,
            if r.exit_crossfade >= 0.0 { format!(", out {:.2} to {}", r.exit_crossfade, name(r.exit_crossfade_to)) } else { String::new() }
        );
        for exit in &r.exits {
            let when = match (exit.finish, exit.branch_point) {
                (true, _) => "finished".to_owned(),
                (false, 0) => "at any time".to_owned(),
                (false, count) => format!("from branch count {count}"),
            };
            println!("  -> {}/{}  {when}, {}", name(exit.to_set), name(exit.to_id), rules(&exit.rules));
        }
    }
}
