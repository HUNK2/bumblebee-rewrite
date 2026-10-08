//! Loads Bumblebee's control state machine and action clips from the install and plays
//! scripted button sequences through the simulation, printing every change of control state.
//! `cargo run -p tf2-core --example control_check` (reads `C:\Games2`, or `TF2_GAME_DIR`).

use std::path::Path;

use glam::{Vec2, Vec3};
use tf2_core::control::button;
use tf2_core::melee::{self, Target};
use tf2_core::sim::{self, Arena, Input, State};
use tf2_core::tuning::{self, Actions, Locomotion, RuleSet};

const DT: f32 = 0.032;

fn main() {
    let dir = std::env::var("TF2_GAME_DIR").unwrap_or_else(|_| "C:/Games2".into());
    let mut t = tuning::load(Path::new(&dir), "Bumblebee").expect("tuning");
    let data = tf2_core::character::load(Path::new(&dir), "bumblebee").expect("character");
    t.locomotion = Locomotion::from_animations(&data.animations, &mut t.missing);
    t.actions = Actions::from_animations(&data.animations, &data.robot, &mut t.missing);
    t.weapon_set = RuleSet::from_animations(&data.animations, "WeaponSet", &mut t.missing);
    for name in &t.missing {
        println!("missing: {name}");
    }
    let transitions: usize = t.control.states.iter().map(|s| s.transitions.len()).sum();
    println!(
        "{} states, {} connections, {} transitions; {} action clips; dash cooldown {}, dodge rules {} / {}",
        t.control.states.len(),
        t.control.connections.len(),
        transitions,
        t.actions.clips.len(),
        t.dash_cooldown,
        t.actions.dodge_forward,
        t.actions.dodge_back
    );
    for id in ["AttackFast_1", "AttackFast_3", "AttackFast_3_ChargeAttk", "DodgeLeft", "DodgeRight", "DodgeBack", "GroundPunchWeak"] {
        match t.actions.named(id) {
            Some(c) => println!(
                "  {id}: {} ticks at {}, root ends {:.2?}, {} events, attack {:?}",
                c.ticks,
                c.speed,
                c.path.last().copied().unwrap_or_default(),
                c.events.len(),
                c.attack.map(|a| (a.damage, a.turn_speed))
            ),
            None => println!("  {id}: MISSING"),
        }
    }

    // Clips whose root is turned: their turn is part of the movement, not of the pose.
    for (set, refs) in &data.animations.sets {
        for r in refs {
            let Some(clip) = &r.clip else { continue };
            let root = clip.channel(tf2_core::formats::anim::ROOT_CHANNEL);
            let turned = |at: f32| root.and_then(|c| c.rotation.sample(at, clip.length, false)).is_some_and(|q| q[3].abs() < 0.9999);
            if turned(0.0) || turned(clip.length) {
                println!("  root turned in {set}/{}", r.id);
            }
        }
    }

    let attack = |down: bool, hit: bool| Input {
        buttons_down: (down as u64) << button::ATTACK_FAST,
        buttons_hit: (hit as u64) << button::ATTACK_FAST,
        ..Default::default()
    };
    let mut script: Vec<(&str, Vec<Input>)> = Vec::new();
    // Three taps, each a little after the last hit's button window opens.
    let mut taps = Vec::new();
    for _ in 0..3 {
        taps.push(attack(true, true));
        taps.push(attack(true, false));
        taps.push(attack(false, false));
        taps.extend(std::iter::repeat_n(Input::default(), 12));
    }
    taps.extend(std::iter::repeat_n(Input::default(), 60));
    script.push(("three taps", taps));
    // A hold of one second, then let go.
    let mut hold = vec![attack(true, true)];
    hold.extend(std::iter::repeat_n(attack(true, false), 31));
    hold.extend(std::iter::repeat_n(Input::default(), 90));
    script.push(("hold", hold));
    // Weapon mode, stick to the left, dodge; then again at once (cooldown), then later.
    let aim = Input { aim: Some(Vec2::Y), stick: Vec2::NEG_X, ..Default::default() };
    let dodge = Input { buttons_down: 1 << button::DODGE, buttons_hit: 1 << button::DODGE, ..aim };
    let mut dodges = vec![aim, aim, dodge];
    dodges.extend(std::iter::repeat_n(aim, 50));
    dodges.push(dodge);
    dodges.extend(std::iter::repeat_n(aim, 20));
    dodges.push(dodge);
    dodges.extend(std::iter::repeat_n(aim, 50));
    script.push(("dodges", dodges));
    // A jump, then the attack held in the air: the ground punch.
    let mut punch = vec![Input { jump: true, jump_held: true, ..Default::default() }];
    punch.extend(std::iter::repeat_n(Input::default(), 6));
    punch.extend(std::iter::repeat_n(attack(true, false), 40));
    punch.extend(std::iter::repeat_n(Input::default(), 80));
    script.push(("ground punch", punch));
    // Into the car from standing, drive, let go with the stick held: the unfold to a run.
    let gas = Input { vehicle: 1.0, ..Default::default() };
    let mut car = vec![gas; 100];
    car.extend(std::iter::repeat_n(Input { stick: Vec2::Y, ..Default::default() }, 50));
    script.push(("car, out to a run", car));
    // Running, then the trigger with the stick held to the side: MoveMode turns him until
    // the stick lines up; drive, then let go with jump held: the leap out.
    let mut run_in = vec![Input { stick: Vec2::Y, ..Default::default() }; 30];
    run_in.extend(std::iter::repeat_n(Input { stick: Vec2::NEG_X, ..gas }, 60));
    run_in.extend(std::iter::repeat_n(Input { jump: true, jump_held: true, ..Default::default() }, 1));
    run_in.extend(std::iter::repeat_n(Input { jump_held: true, ..Default::default() }, 80));
    script.push(("car from a run, leap out", run_in));
    // Driving, then the trigger let go with melee pressed: the slam out of the car.
    let mut slam = vec![gas; 100];
    slam.extend(std::iter::repeat_n(attack(true, true), 2));
    slam.extend(std::iter::repeat_n(attack(true, false), 60));
    slam.extend(std::iter::repeat_n(Input::default(), 90));
    script.push(("car, slam out", slam.clone()));
    // The same with the action button: the punch out of the car, which ends in the strong
    // ground punch.
    let action = Input { buttons_down: 1 << button::CLIMB, ..Default::default() };
    let mut air_punch = vec![gas; 100];
    air_punch.extend(std::iter::repeat_n(action, 60));
    air_punch.extend(std::iter::repeat_n(Input::default(), 120));
    script.push(("car, punch out", air_punch));

    // Something to hit, 16 ahead and 6 to the right: the turn, the slide and the hits of the
    // three taps, and of the charged finisher.
    let m = &t.melee;
    println!(
        "melee search: best distance {}, weights angle {} distance {}, reach {} wide and {} high",
        m.best_dist, m.angle_weight, m.dist_weight, m.distance, m.height
    );
    let (taps, hold) = (script[0].1.clone(), script[1].1.clone());
    let body_name = |hash: u32| data.robot.nodes.iter().find(|n| n.name_hash == hash).map_or("?", |n| n.name.as_str());
    let aside = Vec3::new(6.0, 16.0, 0.0);
    let ahead = Vec3::new(0.0, 16.0, 0.0);
    let cases = [
        ("three taps at a target", taps, aside),
        ("hold at a target to the side", hold.clone(), aside),
        ("hold at a target straight ahead", hold, ahead),
        // The car reaches about 91 along y when the trigger is let go.
        ("slam out of the car at a target", slam, Vec3::new(0.0, 104.0, 0.0)),
    ];
    for (name, inputs, at) in cases {
        println!("== {name}");
        let dummy = Target { id: 1, pos: at, radius: t.robot_radius, height: t.robot_height, character: true, large: false, surface: melee::surface::UPPER_BODY };
        let arena = Arena { targets: vec![dummy], ..Default::default() };
        let mut s = State::new(&t);
        let mut last = String::new();
        for (i, input) in inputs.iter().enumerate() {
            sim::step(&mut s, input, &t, &arena, DT);
            let apart = (dummy.pos - s.pos).length();
            let (state, clip) = (s.control_state(&t).unwrap_or("-").to_owned(), s.action_clip(&t).map(|(_, id, tick)| (id, tick)));
            if state != last {
                println!("  {:5.2}s {state:<24} facing {:6.1} deg, {apart:5.2} from it", i as f32 * DT, s.yaw.to_degrees());
                last = state;
            }
            for hit in &s.hits {
                println!(
                    "  {:5.2}s HIT at {:?} by {} at ({:.2}, {:.2}, {:.2}): damage {}, thrown at {} and {:.0} deg up, going ({:.2}, {:.2}), flags {:#x} ({}); {apart:.2} apart",
                    i as f32 * DT,
                    clip,
                    body_name(hit.body),
                    hit.point.x,
                    hit.point.y,
                    hit.point.z,
                    hit.damage,
                    hit.knock_back_speed,
                    hit.knock_back_angle.to_degrees(),
                    hit.direction.x,
                    hit.direction.y,
                    hit.flags,
                    if hit.throws() { "throws" } else { "no throw" }
                );
            }
        }
        println!("  end pos ({:.2}, {:.2}), facing {:.1} deg, {:.2} from it", s.pos.x, s.pos.y, s.yaw.to_degrees(), (dummy.pos - s.pos).length());
    }

    // The ground punch among three targets: 8 ahead, 14 to the right, and 30 behind (out
    // of the blast's reach).
    for id in ["GroundPunch", "GroundPunchWeak"] {
        for b in t.actions.named(id).map(|c| c.blasts.as_slice()).unwrap_or_default() {
            let e = &b.explosion;
            println!(
                "  {id} sets off at {}: radius {} from {} of it over {} s, damage {} -> {:?} (pound {} / {}), throw {} at {} deg, node ends at {:.2?}",
                body_name(b.node),
                e.radius,
                e.start_scale,
                e.time,
                e.damage,
                e.owned_damage(t.ground_pound_damage, t.ground_pound_damage_weak),
                t.ground_pound_damage,
                t.ground_pound_damage_weak,
                e.knock_back_speed,
                e.knock_back_angle,
                b.centres.last().copied().unwrap_or_default()
            );
        }
    }
    println!("== ground punch among targets");
    let stand = |id, x, y| Target { id, pos: Vec3::new(x, y, 0.0), radius: t.robot_radius, height: t.robot_height, character: true, large: false, surface: melee::surface::UPPER_BODY };
    let arena = Arena { targets: vec![stand(1, 0.0, 8.0), stand(2, 14.0, 0.0), stand(3, 0.0, -30.0)], ..Default::default() };
    let mut s = State::new(&t);
    let mut last = String::new();
    for (i, input) in script[3].1.iter().enumerate() {
        sim::step(&mut s, input, &t, &arena, DT);
        let state = s.control_state(&t).unwrap_or("-").to_owned();
        let tick = s.action_clip(&t).map_or(0.0, |(_, _, tick)| tick);
        if state != last {
            println!("  {:5.2}s {state:<24} pos ({:.2}, {:.2}, {:.2})", i as f32 * DT, s.pos.x, s.pos.y, s.pos.z);
            last = state;
        }
        for b in s.blasts.iter().filter(|b| b.live) {
            println!("  {:5.2}s blast at ({:.2}, {:.2}, {:.2}), radius {:.2} (clip tick {tick:.1})", i as f32 * DT, b.centre.x, b.centre.y, b.centre.z, b.radius);
        }
        for shake in &s.shakes {
            match t.shakes.get(&shake.name) {
                Some(def) => println!(
                    "  {:5.2}s camera shake {:08x} at ({:.2}, {:.2}, {:.2}): {} s, whole within {} and gone at {}",
                    i as f32 * DT, shake.name, shake.at.x, shake.at.y, shake.at.z, def.duration, def.full_dist, def.max_dist
                ),
                None => println!("  {:5.2}s camera shake {:08x}: not in the game's list, starts nothing", i as f32 * DT, shake.name),
            }
        }
        for rumble in &s.rumbles {
            println!("  {:5.2}s pad rumble type {}: strength {:.3} for {} s", i as f32 * DT, rumble.kind, rumble.strength, rumble.duration);
        }
        for hit in &s.hits {
            println!(
                "  {:5.2}s {} target {} at ({:.2}, {:.2}, {:.2}): damage {}, thrown at {} and {:.0} deg up, going ({:.2}, {:.2}), flags {:#x} ({})",
                i as f32 * DT,
                if hit.blast { "BLAST hits" } else { "FIST hits" },
                hit.target,
                hit.point.x,
                hit.point.y,
                hit.point.z,
                hit.damage,
                hit.knock_back_speed,
                hit.knock_back_angle.to_degrees(),
                hit.direction.x,
                hit.direction.y,
                hit.flags,
                if hit.throws() { "throws" } else { "no throw" }
            );
        }
    }

    // Weapon mode on foot with the game's own weapon set: standing, the stick left, through
    // the middle to the right, round to the back, let go; then the aim swung 60 degrees.
    // Every update: speed, the way he goes measured from the aim, and the leading clip.
    println!("== strafing (speed, degrees left of the aim, clip), strafe speed {}", t.strafe_speed);
    let aim_at = |degrees: f32| Vec2::from_angle(degrees.to_radians()).rotate(Vec2::Y);
    let held = |stick: Vec2, aim: f32| Input { aim: Some(aim_at(aim)), stick, ..Default::default() };
    let mut strafe = vec![held(Vec2::ZERO, 0.0); 4];
    strafe.extend(std::iter::repeat_n(held(Vec2::NEG_X, 0.0), 13));
    strafe.extend(std::iter::repeat_n(held(Vec2::ZERO, 0.0), 2));
    strafe.extend(std::iter::repeat_n(held(Vec2::X, 0.0), 13));
    strafe.extend(std::iter::repeat_n(held(Vec2::new(1.0, -1.0).normalize(), 0.0), 4));
    strafe.extend(std::iter::repeat_n(held(Vec2::NEG_Y, 0.0), 12));
    strafe.extend(std::iter::repeat_n(held(Vec2::ZERO, 0.0), 10));
    strafe.extend(std::iter::repeat_n(held(Vec2::ZERO, 60.0), 30));
    let arena = Arena::default();
    let mut s = State::new(&t);
    for (i, input) in strafe.iter().enumerate() {
        sim::step(&mut s, input, &t, &arena, DT);
        let v = Vec2::new(s.vel.x, s.vel.y);
        let way = if v.length() > 0.05 { (v.angle_to(aim_at(0.0)) * -1.0).to_degrees() } else { 0.0 };
        println!(
            "  {:5.2}s {:5.2} {:7.1}  {:<22} facing {:6.1}, twist {:5.1}, F_STRAFE_ANG {:6.1}",
            i as f32 * DT,
            v.length(),
            way,
            s.action_clip(&t).map_or("-", |(_, id, _)| id),
            s.yaw.to_degrees(),
            s.strafe.twist.to_degrees(),
            s.strafe.angle
        );
    }

    // Weapon mode in the air with the game's own weapon set and jump values: strafing left,
    // a jump, the stick held; then standing, a jump on the spot; then a plain running jump
    // with the aim taken up half way. Every update in the air and the landing after it.
    println!(
        "== weapon jumps (height, vertical speed, ground speed, time factor, clip); slow motion under {} to {} at {} a second",
        t.jump.weapon_dilation_start, t.jump.weapon_dilation, t.jump.weapon_dilation_rate
    );
    let jumping = |input: Input| Input { jump: true, jump_held: true, ..input };
    let mut air = vec![held(Vec2::NEG_X, 0.0); 20];
    air.push(jumping(held(Vec2::NEG_X, 0.0)));
    air.extend(std::iter::repeat_n(held(Vec2::NEG_X, 0.0), 80));
    air.extend(std::iter::repeat_n(held(Vec2::ZERO, 0.0), 20));
    air.push(jumping(held(Vec2::ZERO, 0.0)));
    air.extend(std::iter::repeat_n(held(Vec2::ZERO, 0.0), 90));
    let forward = Input { stick: Vec2::Y, ..Default::default() };
    air.extend(std::iter::repeat_n(Input::default(), 10));
    air.extend(std::iter::repeat_n(forward, 20));
    air.push(jumping(forward));
    air.extend(std::iter::repeat_n(forward, 15));
    air.extend(std::iter::repeat_n(held(Vec2::Y, 90.0), 50));
    let mut s = State::new(&t);
    let mut quiet = String::new();
    for (i, input) in air.iter().enumerate() {
        sim::step(&mut s, input, &t, &arena, DT);
        let state = s.control_state(&t).unwrap_or("-").to_owned();
        let clip = s.action_clip(&t).map_or("-", |(_, id, _)| id).to_owned();
        let on_the_ground = !matches!(s.mode, sim::Mode::Jump(_));
        let now = format!("{state} {clip}");
        if on_the_ground && now == quiet {
            continue;
        }
        quiet = now;
        println!(
            "  {:5.2}s {:<20} z {:5.2} vz {:6.2} speed {:5.2} factor {:.3}  {:<26} facing {:6.1}",
            i as f32 * DT,
            state,
            s.pos.z,
            s.vel.z,
            Vec2::new(s.vel.x, s.vel.y).length(),
            s.dilation,
            clip,
            s.yaw.to_degrees()
        );
    }

    for (name, inputs) in script {
        println!("== {name}");
        let mut s = State::new(&t);
        let mut last = String::new();
        for (i, input) in inputs.iter().enumerate() {
            sim::step(&mut s, input, &t, &arena, DT);
            let clip = s.action_clip(&t).map(|(set, id, tick)| format!("{set}/{id}@{tick:.0}")).unwrap_or_default();
            let now = format!("{:?} {} {}", s.mode, s.control_state(&t).unwrap_or("-"), clip.split('@').next().unwrap_or(""));
            if now != last {
                println!(
                    "  {:5.2}s {:<60} pos ({:6.2}, {:6.2}, {:5.2}) speed {:5.1}",
                    i as f32 * DT,
                    format!("{:?} {} {}", s.mode, s.control_state(&t).unwrap_or("-"), clip),
                    s.pos.x,
                    s.pos.y,
                    s.pos.z,
                    s.speed
                );
                last = now;
            }
        }
        println!("  end pos ({:.2}, {:.2}, {:.2})", s.pos.x, s.pos.y, s.pos.z);
    }
}
