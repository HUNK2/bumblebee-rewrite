//! Plays scripted stretches of walking, running, stopping, jumping and falling through the
//! simulation on the install's real data, and compares, update by update, the clip the base
//! layer's own rules pick (`animrules::Base`: the sets' entry and exit rules) with the clip
//! the movement's pace chooser has leading on the ground.
//! `cargo run -p tf2-core --example base_check` (reads `C:\Games2`, or `TF2_GAME_DIR`).
//! Every disagreement is printed; the last line counts them. `VERBOSE=1` prints each change
//! of the rules' clip.

use std::path::Path;

use glam::{Vec2, Vec3};
use tf2_core::sim::{self, Arena, Input, Mode, Pace, State};
use tf2_core::tuning::{self, Actions, Locomotion, RuleSet};

const DT: f32 = 0.032;

/// The clip the pace chooser has leading, by name (`None` while it is standing, which the
/// original shows as the idle clip or a landing with no pace of its own).
fn pace_clip(s: &State) -> Option<&'static str> {
    match s.pace() {
        Pace::Idle => None,
        Pace::Walk => Some("Walk"),
        Pace::Run => Some("Run"),
        Pace::Stop => Some("Run2Idle"),
        Pace::LandRun => Some("Jump_LandRun"),
        Pace::BigLandRun => Some("BigJump_LandRun"),
    }
}

fn main() {
    let dir = std::env::var("TF2_GAME_DIR").unwrap_or_else(|_| "C:/Games2".into());
    let mut t = tuning::load(Path::new(&dir), "Bumblebee").expect("tuning");
    let data = tf2_core::character::load(Path::new(&dir), "bumblebee").expect("character");
    t.locomotion = Locomotion::from_animations(&data.animations, &mut t.missing);
    t.actions = Actions::from_animations(&data.animations, &data.robot, &mut t.missing);
    t.weapon_set = RuleSet::from_animations(&data.animations, "WeaponSet", &mut t.missing);
    t.rule_sets = RuleSet::all(&data.animations);
    for name in &t.missing {
        println!("missing: {name}");
    }
    let verbose = std::env::var("VERBOSE").is_ok();

    let run = Input { stick: Vec2::Y, ..Default::default() };
    let walk = Input { stick: Vec2::Y * 0.5, ..Default::default() };
    let jump = Input { jump: true, jump_held: true, ..Default::default() };
    let still = Input::default();
    let mut script: Vec<(&str, Vec<Input>)> = Vec::new();
    let rep = |input: Input, n: usize| std::iter::repeat_n(input, n);
    // Stand, run, stop, walk, stop, run, walk, run.
    let mut a = Vec::new();
    a.extend(rep(still, 40));
    a.extend(rep(run, 60));
    a.extend(rep(still, 60));
    a.extend(rep(walk, 40));
    a.extend(rep(still, 40));
    a.extend(rep(run, 30));
    a.extend(rep(walk, 30));
    a.extend(rep(run, 30));
    a.extend(rep(still, 60));
    script.push(("stand, run, stop, walk, stop, run, walk, run, stop", a));
    // A standing jump, landing on the spot; a running jump landing into a run; a jump with
    // the stick let go in the air.
    let mut b = Vec::new();
    b.extend(rep(still, 30));
    b.push(jump);
    b.extend(rep(Input { jump_held: true, ..still }, 5));
    b.extend(rep(still, 90));
    b.extend(rep(run, 40));
    b.push(Input { jump: true, jump_held: true, ..run });
    b.extend(rep(Input { jump_held: true, ..run }, 5));
    b.extend(rep(run, 90));
    b.extend(rep(still, 40));
    b.extend(rep(run, 30));
    b.push(Input { jump: true, jump_held: true, ..run });
    b.extend(rep(Input { jump_held: true, ..run }, 5));
    b.extend(rep(still, 100));
    script.push(("standing jump, running jump, jump and let go", b));
    // Over an edge: the arena below has a ledge at y = 30, 12 high.
    let mut c = Vec::new();
    c.extend(rep(run, 90));
    c.extend(rep(still, 100));
    script.push(("run off a ledge, stand", c));

    let mut total = 0usize;
    let mut disagreements = 0usize;
    for (name, inputs) in script {
        println!("== {name}");
        let mut s = State::new(&t);
        // The ledge case: start on a raised platform and run off its edge.
        let arena = if name.starts_with("run off") {
            s.pos = Vec3::new(0.0, 0.0, 12.0);
            Arena { boxes: vec![sim::Box3 { min: Vec3::new(-50.0, -50.0, -10.0), max: Vec3::new(50.0, 30.0, 12.0) }], ..Default::default() }
        } else {
            Arena::default()
        };
        let mut last: Option<(String, String)> = None;
        for (i, input) in inputs.iter().enumerate() {
            sim::step(&mut s, input, &t, &arena, DT);
            let base = s.base_clip(&t).map(|c| format!("{}/{}", c.set, c.id)).unwrap_or_else(|| "-".into());
            let mode = format!("{:?}", s.mode);
            if verbose && last.as_ref().is_none_or(|(b, m)| *b != base || *m != mode) {
                println!("  {:4} {:<16} rules: {:<28} pace: {:?} z {:.2} vz {:.1}", i, mode, base, s.pace(), s.pos.z, s.vel.z);
            }
            last = Some((base.clone(), mode.clone()));
            if matches!(s.mode, Mode::Stand | Mode::Move) {
                total += 1;
                let theirs = pace_clip(&s);
                // Standing with no pace of its own: the rules show the idle clip, or a
                // landing on the spot.
                let ok = match theirs {
                    None => base.starts_with("IdleStandSet/") || base == "-",
                    Some(clip) => base.ends_with(&format!("/{clip}")),
                };
                if !ok {
                    disagreements += 1;
                    println!("  DISAGREE at update {i} ({mode}): rules {base}, pace {:?}", s.pace());
                }
            }
        }
    }
    println!("{total} updates on foot compared, {disagreements} disagreements");
}
