//! Replays a stretch of the recorded game's camera inputs through `tf2_core::camera` and
//! prints how far its eye ends up from the recorded one.
//! `cargo run -p tf2-core --example camera_replay -- <file.csv> [drive level]`
//! The files are written from a trace by the snippet in `notes\live-trace.md`; columns:
//! t, target xyz, height, facing xy, stick xy, vehicle, eye xyz, distance, elevation, fov,
//! camera forward xyz.

use std::path::Path;

use glam::{Vec2, Vec3};
use tf2_core::camera::{Camera, Open, Target, View};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("a replay file");
    let level: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let dir = std::env::var("TF2_GAME_DIR").unwrap_or_else(|_| "C:/Games2".into());
    let mut tuning = tf2_core::tuning::load(Path::new(&dir), "Bumblebee").expect("tuning");
    if let Some(camera) = tuning.drive_camera_levels.get(level) {
        tuning.drive_camera = camera.clone();
    }
    let rows: Vec<Vec<f32>> = std::fs::read_to_string(&path)
        .expect("read")
        .lines()
        .map(|line| line.split(',').map(|v| v.parse().unwrap()).collect())
        .collect();
    let target_of = |r: &[f32], velocity: Vec3| {
        let vehicle = r[9] > 0.5;
        let settings = if vehicle { &tuning.drive_camera } else { &tuning.robot_camera };
        Target {
            // The recorded point is the one aimed at; the camera adds the height back.
            // In the car the recorded height is the point's over the car's own place, the
            // settings' offset already in it: the camera takes that off again itself.
            position: Vec3::new(r[1], r[2], r[3] - r[4] - if vehicle { 0.0 } else { settings.height_offset }),
            height: r[4] - if vehicle { settings.height_offset } else { 0.0 },
            facing: Vec2::new(r[5], r[6]),
            velocity,
            motion: Vec3::ZERO,
            vehicle,
            climbing: false,
            aim: None,
            radius: 0.0,
            pitch: 0.0,
            roll: 0.0,
            fighting: false,
            ..Default::default()
        }
    };
    let first = &rows[0];
    let view = View { eye: Vec3::new(first[10], first[11], first[12]), forward: Vec3::new(first[16], first[17], first[18]), fov: first[15], roll: 0.0 };
    let mut camera = Camera::from_view(view, &target_of(first, Vec3::ZERO), &tuning);
    // A stretch starts in the middle of play, where the drift to the default elevation is
    // long over (`+0xd9` reads 0 all through the recording): the smallest turn the camera
    // takes as the player's own ends it here too.
    if std::env::var("SETTLE").is_err() {
        camera.nudge(Vec2::new(0.0, 0.0015));
    }
    let (mut worst, mut sum, mut count) = (0.0f32, 0.0f32, 0u32);
    // The worst of each part of the error on its own: distance, heading (degrees), height.
    let (mut worst_away, mut worst_round, mut worst_up) = ((0.0f32, 0.0f32), (0.0f32, 0.0f32), (0.0f32, 0.0f32));
    for pair in rows.windows(2) {
        let (last, row) = (&pair[0], &pair[1]);
        let updates = ((row[0] - last[0]) / 0.032).round().max(1.0);
        let velocity = (Vec3::new(row[1], row[2], row[3]) - Vec3::new(last[1], last[2], last[3])) / (updates * 0.032);
        // The camera is handed the speed after the cap (35, or 43 in turbo), not the body's.
        let velocity = velocity.clamp_length_max(if velocity.length() > 41.0 { 43.0 } else { 35.0 });
        let target = target_of(row, velocity);
        let mut got = view;
        // Nothing was recorded while nothing changed: the updates in a gap ran on the last
        // row's inputs, and only the final one on this row's.
        let held = target_of(last, Vec3::ZERO);
        for update in 0..updates as usize {
            let (aim, r) = if update + 1 == updates as usize { (&target, row) } else { (&held, last) };
            camera.step(aim, Vec2::new(r[7], -r[8]), &tuning, &Open, 0.032);
            got = camera.latest();
        }
        let eye = Vec3::new(row[10], row[11], row[12]);
        let error = (got.eye - eye).length();
        worst = worst.max(error);
        sum += error;
        count += 1;
        {
            let at = Vec3::new(row[1], row[2], row[3]);
            let (theirs, mine) = (eye - at, got.eye - at);
            let away = (mine.length() - theirs.length()).abs();
            let mut round = ((-mine.x).atan2(mine.y) - (-theirs.x).atan2(theirs.y)).to_degrees().abs();
            if round > 180.0 {
                round = 360.0 - round;
            }
            let up = (mine.z - theirs.z).abs();
            if away > worst_away.0 {
                worst_away = (away, row[0]);
            }
            if round > worst_round.0 {
                worst_round = (round, row[0]);
            }
            if up > worst_up.0 {
                worst_up = (up, row[0]);
            }
        }
        let window = std::env::var("FROM").ok().and_then(|v| v.parse::<f32>().ok());
        let shown = match window {
            Some(from) => row[0] >= from && row[0] < from + 1.5,
            None => count % 40 == 0,
        };
        if shown {
            print!("stick ({:5.2},{:5.2}) ", row[7], row[8]);
            let offset = eye - Vec3::new(row[1], row[2], row[3]);
            let mine = got.eye - Vec3::new(row[1], row[2], row[3]);
            println!(
                "{:8.2}  off by {:5.2}  recorded: {:5.2} away, {:6.1} round, {:5.2} up, fov {:4.1} | here: {:5.2} away, {:6.1} round, {:5.2} up, fov {:4.1}",
                row[0],
                error,
                offset.length(),
                (-offset.x).atan2(offset.y).to_degrees(),
                offset.z,
                row[15],
                mine.length(),
                (-mine.x).atan2(mine.y).to_degrees(),
                mine.z,
                got.fov,
            );
        }
    }
    println!("{count} updates: eye off by {:.2} on average, {:.2} at worst", sum / count.max(1) as f32, worst);
    println!(
        "worst on its own: distance {:.2} at {:.2}, heading {:.1} degrees at {:.2}, height {:.2} at {:.2}",
        worst_away.0, worst_away.1, worst_round.0, worst_round.1, worst_up.0, worst_up.1
    );
}
