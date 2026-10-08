//! Climbs through the real state machine with the real clips from the install: a wall like
//! the first one climbed in run 1 (43.3 high), run at, jumped at with Climb held, climbed
//! with the stick up, and pulled up over. Prints every change of control state and climb
//! phase, and the position against the wall. Then the shimmy, the climb down to the ground,
//! and the jump off. `cargo run -p tf2-core --example climb_check` (reads `C:\Games2`).

use std::path::Path;

use glam::{Vec2, Vec3};
use tf2_core::control::button;
use tf2_core::sim::{self, Arena, Box3, Input, Mode, State};
use tf2_core::tuning::{self, Actions, Locomotion};

const DT: f32 = 0.032;

fn main() {
    let dir = std::env::var("TF2_GAME_DIR").unwrap_or_else(|_| "C:/Games2".into());
    let mut t = tuning::load(Path::new(&dir), "Bumblebee").expect("tuning");
    let data = tf2_core::character::load(Path::new(&dir), "bumblebee").expect("character");
    t.locomotion = Locomotion::from_animations(&data.animations, &mut t.missing);
    t.actions = Actions::from_animations(&data.animations, &data.robot, &mut t.missing);
    for name in &t.missing {
        println!("missing: {name}");
    }
    println!(
        "modeClimb: pullUpDist {} edgeLimitDist {} wallOffset {} arrival deceleration {}; climb jumps {} / {} at {}",
        t.climb.pull_up_dist,
        t.climb.edge_limit_dist,
        t.climb.wall_offset,
        t.climb.arrival_deceleration,
        t.jump.climb_up_height,
        t.jump.climb_off_height,
        t.jump.climb_off_speed
    );
    // A wall facing -y, 107 wide, 43.3 high.
    let arena = Arena { boxes: vec![Box3 { min: Vec3::new(-50.0, 30.0, 0.0), max: Vec3::new(57.0, 70.0, 43.3) }], ..Default::default() };
    let climb_held = |stick: Vec2, pad: Vec2| Input {
        stick,
        pad_stick: pad,
        buttons_down: 1 << button::CLIMB,
        ..Default::default()
    };

    let mut s = State::new(&t);
    let mut log = Log::default();
    // Run at it and jump, Climb held from the start.
    for i in 0..30 {
        let mut input = climb_held(Vec2::Y, Vec2::Y);
        if i == 0 {
            input.buttons_hit = 1 << button::CLIMB;
        }
        input.jump = i == 20;
        input.jump_held = (20..24).contains(&i);
        step(&mut s, &input, &t, &arena, &mut log, "run and jump");
    }
    // Climb: stick up until over the top.
    for _ in 0..200 {
        step(&mut s, &climb_held(Vec2::ZERO, Vec2::Y), &t, &arena, &mut log, "stick up");
        if s.mode == Mode::Move {
            break;
        }
    }
    println!("on top at {:.2?}", s.pos);

    // Again from the ground: catch, shimmy right a second, down to the ground.
    let mut s = State::new(&t);
    s.pos = Vec3::new(0.0, 27.0, 0.0);
    let mut log = Log::default();
    step(&mut s, &Input { buttons_down: 1 << button::CLIMB, buttons_hit: 1 << button::CLIMB, ..Default::default() }, &t, &arena, &mut log, "press Climb");
    for _ in 0..30 {
        step(&mut s, &climb_held(Vec2::ZERO, Vec2::Y), &t, &arena, &mut log, "stick up");
    }
    let before = s.pos;
    for _ in 0..31 {
        step(&mut s, &climb_held(Vec2::ZERO, Vec2::X), &t, &arena, &mut log, "stick right");
    }
    println!("shimmied {:.2} in 0.99 s", (s.pos - before).length());
    for _ in 0..60 {
        step(&mut s, &climb_held(Vec2::ZERO, Vec2::NEG_Y), &t, &arena, &mut log, "stick down");
        if s.climb.is_none() {
            break;
        }
    }
    for _ in 0..10 {
        step(&mut s, &Input::default(), &t, &arena, &mut log, "let go");
    }

    // Up the wall a little, then jump off it with the stick pulled back.
    let mut s = State::new(&t);
    s.pos = Vec3::new(0.0, 27.0, 0.0);
    let mut log = Log::default();
    step(&mut s, &Input { buttons_down: 1 << button::CLIMB, buttons_hit: 1 << button::CLIMB, ..Default::default() }, &t, &arena, &mut log, "press Climb");
    for _ in 0..20 {
        step(&mut s, &climb_held(Vec2::ZERO, Vec2::Y), &t, &arena, &mut log, "stick up");
    }
    let back = Input { stick: Vec2::NEG_Y, pad_stick: Vec2::NEG_Y, jump: true, jump_held: true, ..Default::default() };
    step(&mut s, &back, &t, &arena, &mut log, "jump, stick back");
    for _ in 0..90 {
        step(&mut s, &Input { stick: Vec2::NEG_Y, pad_stick: Vec2::NEG_Y, ..Default::default() }, &t, &arena, &mut log, "stick back");
        if s.mode == Mode::Move {
            break;
        }
    }
    println!("landed at {:.2?} facing {:.0} degrees", s.pos, s.yaw.to_degrees());
}

#[derive(Default)]
struct Log {
    time: f32,
    last: String,
}

fn step(s: &mut State, input: &Input, t: &tuning::Tuning, arena: &Arena, log: &mut Log, what: &str) {
    sim::step(s, input, t, arena, DT);
    log.time += DT;
    let phase = s.climb.as_ref().map_or(String::new(), |c| format!(" {:?}{}", c.phase, if c.right_hand { " R" } else { " L" }));
    let clip = s.action_clip(t).map_or(String::new(), |(_, id, tick)| format!(" {id}@{tick:.0}"));
    let now = format!("{:?} {}{phase}", s.mode, s.control_state(t).unwrap_or("-"));
    if now != log.last {
        let wall = s.climb.as_ref().map_or(String::new(), |c| {
            let l = c.wall.local(s.pos);
            format!(" wall ({:.2} {:.2} {:.2}) of {:.1} x {:.1}", l.x, l.y, l.z, c.wall.width, c.wall.height)
        });
        println!(
            "{:6.3} {what:<18} {now:<40}{clip} pos ({:.2} {:.2} {:.2}) vel ({:.2} {:.2} {:.2}){wall}",
            log.time, s.pos.x, s.pos.y, s.pos.z, s.vel.x, s.vel.y, s.vel.z
        );
        log.last = now;
    }
}
