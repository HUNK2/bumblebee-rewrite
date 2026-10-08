//! Replays the WHOLE of a recorded run's camera inputs through `tf2_core::camera` (the
//! follow camera, the weapon camera and the driving camera, with every take-over between
//! them) and says how far the view ends up from the recorded one, by camera and by what
//! he was doing.
//! `cargo run -p tf2-core --example camera_run -- [table.csv]`
//! The table is written by `work\cam_export.py` (its docstring has the columns).
//!
//! The level's walls are not here, so updates where the original's camera was pulled in or
//! held on a corner are not scored, and the camera is put back on the recording after them.
//! It is also put back whenever it has strayed more than `RESYNC` (default 2) from the
//! recorded eye, and each such time is counted and listed: those are the places to look at.
//! `RESYNC=0` never puts it back (errors then pile up for the rest of the run).
//! `FROM=<t> [SPAN=<s>]` prints every update in a window.

use std::collections::BTreeMap;
use std::path::Path;

use glam::{Vec2, Vec3};
use tf2_core::camera::{Camera, Mode, Open, Target, View};

const MODES: [&str; 8] = [
    "First",
    "Move",
    "Jump",
    "Climb",
    "Attack",
    "DriveCompanion",
    "Drive",
    "Strafe",
];
const KINDS: [&str; 3] = ["follow", "weapon", "driving"];
/// Updates after one camera takes over from another that are scored on their own.
const TAKE_OVER: u32 = 45;

#[derive(Default)]
struct Score {
    count: u32,
    eye: f32,
    eye_worst: (f32, f32),
    turn: f32,
    turn_worst: (f32, f32),
    fov_worst: (f32, f32),
    all: Vec<f32>,
}

impl Score {
    fn add(&mut self, t: f32, eye: f32, turn: f32, fov: f32) {
        self.count += 1;
        self.eye += eye;
        self.turn += turn;
        self.all.push(eye);
        if eye > self.eye_worst.0 {
            self.eye_worst = (eye, t);
        }
        if turn > self.turn_worst.0 {
            self.turn_worst = (turn, t);
        }
        if fov > self.fov_worst.0 {
            self.fov_worst = (fov, t);
        }
    }

    fn print(&mut self, name: &str) {
        if self.count == 0 {
            return;
        }
        self.all.sort_by(|a, b| a.total_cmp(b));
        let at = |part: f32| self.all[((self.all.len() - 1) as f32 * part) as usize];
        println!(
            "{name:28} {:6} updates | eye off {:5.2} mean, {:5.2} median, {:5.2} at 95%, {:5.2} worst (t {:7.2}) | view off {:5.2} deg mean, {:5.1} worst (t {:7.2}) | fov {:4.1} worst (t {:7.2})",
            self.count,
            self.eye / self.count as f32,
            at(0.5),
            at(0.95),
            self.eye_worst.0,
            self.eye_worst.1,
            self.turn / self.count as f32,
            self.turn_worst.0,
            self.turn_worst.1,
            self.fov_worst.0,
            self.fov_worst.1,
        );
    }
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "C:/Bumblebee/work/traces/run1_camera_full.csv".into());
    let dir = std::env::var("TF2_GAME_DIR").unwrap_or_else(|_| "C:/Games2".into());
    let number = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<f32>().ok());
    let resync = number("RESYNC").unwrap_or(2.0);
    let from = number("FROM");
    let span = number("SPAN").unwrap_or(1.5);
    let mut tuning = tf2_core::tuning::load(Path::new(&dir), "Bumblebee").expect("tuning");
    println!(
        "heights: robot {}, vehicle {}, weapon look {}; radius {}",
        tuning.robot_height, tuning.vehicle_height, tuning.weapon_look_height, tuning.robot_radius
    );
    let rows: Vec<Vec<f32>> = std::fs::read_to_string(&path)
        .expect("read")
        .lines()
        .map(|line| line.split(',').map(|v| v.parse().unwrap()).collect())
        .collect();
    let v3 = |r: &[f32], i: usize| Vec3::new(r[i], r[i + 1], r[i + 2]);
    let view_of = |r: &[f32]| View {
        eye: v3(r, 13),
        forward: v3(r, 16),
        fov: r[19],
        roll: -r[33].clamp(-1.0, 1.0).asin(),
    };
    let radius = tuning.robot_radius;
    let target_of = |r: &[f32]| {
        let kind = r[2] as usize;
        Target {
            // The recorded point is the one on him the camera looks at; the camera adds the
            // height back.
            position: v3(r, 5) - Vec3::Z * r[8],
            height: r[8],
            facing: Vec2::new(r[9], r[10]),
            velocity: if kind == 2 { v3(r, 26) } else { Vec3::ZERO },
            // Legacy44/45-column exports omit raw velocity; their capped value
            // is only a replay approximation, not the runtime camera's input.
            motion: if r.len() >= 48 {
                v3(r, 45)
            } else if kind == 2 {
                v3(r, 26)
            } else {
                Vec3::ZERO
            },
            vehicle: kind == 2,
            climbing: r[4] as usize == 3,
            aim: (kind == 1).then(|| v3(r, 26)),
            radius,
            pitch: if kind == 2 {
                r[43].clamp(-1.0, 1.0).asin()
            } else {
                0.0
            },
            roll: if kind == 2 {
                (-r[42]).atan2((1.0 - r[42] * r[42]).max(0.0).sqrt())
            } else {
                0.0
            },
            // The legacy recorder omitted the character's attack-controller +108
            // timer used by 0071a5c0. Do not feed back the camera's recorded +13c
            // stand-off state as though it were an independent character input.
            fighting: false,
            ground_distance: r.get(44).copied().unwrap_or(0.0),
            body_height: (kind == 2 && r.len() >= 49).then(|| r[48]),
            mode: match r.get(49).copied().unwrap_or(0.0) as u32 {
                1 => Mode::Climb,
                2 => Mode::Fast,
                3 => Mode::GroundPunch,
                4 => Mode::Transition,
                _ => Mode::Normal,
            },
            ..Default::default()
        }
    };

    let mut scores: BTreeMap<String, Score> = BTreeMap::new();
    let mut camera: Option<Camera> = None;
    let (mut last_kind, mut last_level, mut last_stretch) = (usize::MAX, usize::MAX, -1.0f32);
    let mut since_change = 0u32;
    let mut put_back: Vec<(f32, usize, usize, f32)> = Vec::new();
    let (mut walled, mut scored) = (0u32, 0u32);
    let mut was_walled = false;
    for (i, row) in rows.iter().enumerate() {
        // Two rows a few milliseconds apart are one update caught half written: the later
        // one is the whole of it.
        if rows
            .get(i + 1)
            .is_some_and(|next| next[0] - row[0] < 0.012 && next[0] >= row[0])
            && i > 0
        {
            continue;
        }
        let (kind, level, mode) = (row[2] as usize, row[3] as usize, row[4] as usize);
        let levels = [
            &tuning.robot_camera_levels,
            &tuning.weapon_camera_levels,
            &tuning.drive_camera_levels,
        ];
        let settings = levels[kind].get(level).cloned().expect("level");
        // The settings' heightOffset is in the recorded height already.
        match kind {
            0 => tuning.robot_camera = settings,
            1 => tuning.weapon_camera = settings,
            _ => tuning.drive_camera = settings,
        }
        if kind == 2 {
            tuning.drive_camera.height_offset = 0.0;
        }
        let target = target_of(row);
        let walls = row[30] > 0.5 || row[31] > 0.5;
        let fresh = camera.is_none()
            || row[1] != last_stretch
            || (level != last_level && kind == last_kind);
        if kind != last_kind || level != last_level {
            since_change = 0;
        }
        // Clear of the level's walls again: back on the recording, its springs kept.
        if !fresh && was_walled && !walls && kind == last_kind {
            camera.as_mut().unwrap().reseat(view_of(row));
            (last_kind, last_level, last_stretch) = (kind, level, row[1]);
            was_walled = walls;
            continue;
        }
        (last_kind, last_level, last_stretch) = (kind, level, row[1]);
        was_walled = walls;
        if fresh {
            // Put on the recording: this row is where it starts from, the next the first scored.
            let mut started = Camera::from_view(view_of(row), &target, &tuning);
            if row[29] < 0.5 {
                // Not drifting to its default elevation in the recording (`+0xd9` clear).
                started.nudge(Vec2::new(0.0, 0.0015));
            }
            if kind != 1 && row[38] > 0.5 {
                started.recentre(&target);
            }
            camera = Some(started);
            continue;
        }
        let cam = camera.as_mut().unwrap();
        let last = if row[0] - rows[i - 1][0] < 0.012 && i > 1 {
            &rows[i - 2]
        } else {
            &rows[i - 1]
        };
        // [trace] Follow/drive+d7 is recentre in progress. A rising edge is an
        // original request, including recorded R3 presses outside Climb. Weapon+d7
        // is its swing flag instead. Never reinterpret that as a recentre request.
        if kind != 1 && row[38] > 0.5 && (last[38] < 0.5 || last[2] != row[2]) {
            cam.recentre(&target);
        }
        let updates = if row[0] - last[0] > 0.2 {
            1
        } else {
            ((row[0] - last[0]) / 0.032).round().max(1.0) as usize
        };
        for _ in 0..updates {
            cam.step(&target, Vec2::new(row[11], -row[12]), &tuning, &Open, 0.032);
        }
        let got = cam.latest();
        since_change += 1;
        let eye = (got.eye - v3(row, 13)).length();
        let turn = got.forward.angle_between(v3(row, 16)).to_degrees();
        let fov = (got.fov - row[19]).abs();
        if from.is_some_and(|f| row[0] >= f && row[0] < f + span) {
            let (distance, elevation, offset, extra) = cam.internals();
            println!(
                "{:8.3} {:7} {:14} L{level} stick ({:5.2},{:5.2}) | eye off {:5.2} view off {:5.2} | dist {:6.2} / {:6.2}  elev {:6.1} / {:6.1}  offset ({:5.2},{:5.2},{:5.2}) / ({:5.2},{:5.2},{:5.2})  extra {:5.2} / {:5.2}  fov {:4.1} / {:4.1}  eye.z {:6.2} / {:6.2}{}",
                row[0],
                KINDS[kind],
                MODES[mode],
                row[11],
                row[12],
                eye,
                turn,
                row[20],
                distance,
                row[21].to_degrees(),
                elevation.to_degrees(),
                row[22],
                row[23],
                row[24],
                offset.x,
                offset.y,
                offset.z,
                row[25],
                extra,
                row[19],
                got.fov,
                row[15],
                got.eye.z,
                if walls { "  [walls]" } else { "" },
            );
        }
        if walls {
            walled += 1;
            continue;
        }
        scored += 1;
        let taking_over = since_change <= TAKE_OVER;
        let name = if taking_over {
            format!("{} (first {TAKE_OVER} updates)", KINDS[kind])
        } else {
            format!("{} / {}", KINDS[kind], MODES[mode])
        };
        scores.entry(name).or_default().add(row[0], eye, turn, fov);
        scores
            .entry(format!("{} ALL", KINDS[kind]))
            .or_default()
            .add(row[0], eye, turn, fov);
        if resync > 0.0 && eye > resync {
            put_back.push((row[0], kind, mode, eye));
            cam.reseat(view_of(row));
        }
    }
    println!("recorded on / here (in the windows above)");
    for (name, score) in scores.iter_mut() {
        score.print(name);
    }
    println!(
        "{scored} updates scored; {walled} not scored (the original's camera was against the level's walls)"
    );
    println!(
        "put back on the recording {} times (strayed more than {resync}):",
        put_back.len()
    );
    for (t, kind, mode, eye) in &put_back {
        println!(
            "  t {t:7.2}  {:7} {:14} off by {eye:.2}",
            KINDS[*kind], MODES[*mode]
        );
    }
}
