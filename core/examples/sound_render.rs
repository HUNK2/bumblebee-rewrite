//! Plays one sound event into a WAV file, to listen to what the mod would play without
//! launching anything.
//!
//!   cargo run -p tf2-core --example sound_render -- <out.wav> <EVENT_NAME> [seconds]
//!       [rpm=<from>..<to>] [load=<0..1>] [level=<level folder, default us_city_00>]
//!
//! With `rpm=` the event's `rpm` parameter sweeps over the given range across the file;
//! `speed=<from>..<to>` does the same for `speed` (the tyres' loops, in metres a second).
//! `<EVENT_NAME>` may be `list` to print every event that can be played. A further word
//! `describe` prints the event as the file has it instead; `drive` (with the engine event)
//! runs the game's gearbox, pulling away and then coasting.
//! `trace=<csv> start=<seconds>` replays recorded time,vehicle,rpm,load rows
//! (work/engine_audio_trace.py), preserving car entry/exit and recorded parameters.

use std::io::Write;
use std::path::PathBuf;

use tf2_core::sound::{Library, Mixer, event_id};

const RATE: u32 = 48000;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(out), Some(name)) = (args.first(), args.get(1)) else {
        eprintln!("usage: sound_render <out.wav> <EVENT_NAME | list> [seconds] [rpm=a..b] [load=x] [level=name]");
        return;
    };
    let option = |key: &str| args.iter().find_map(|a| a.strip_prefix(key).map(str::to_owned));
    let seconds: f32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(4.0);
    let game = PathBuf::from(std::env::var("TF2_GAME_DIR").unwrap_or_else(|_| r"C:\Games2".into()));
    let level = option("level=").unwrap_or_else(|| "us_city_00".into());
    let (mut library, problems) = Library::load(&[
        game.join(r"characters\eng\bumblebee.fev"),
        game.join("levels").join(&level).join(r"eng\mission.fev"),
        game.join(r"audio\eng\global.fev"),
        game.join(r"audio\eng\global_streaming.fev"),
    ]);
    for problem in problems {
        eprintln!("{problem}");
    }
    let data = tf2_core::character::load(&game, "bumblebee").expect("Bumblebee's chatter table");
    let chatter: Vec<_> = data.chatter.iter().map(|l| l.event).collect();
    library.add_programmer_bank(&game.join(r"audio\eng\CHATTER.fsb"), event_id("ProgrammerSoundPlayer"), &chatter)
        .expect("binding chatter samples");
    if name == "list" {
        let mut names = library.names();
        names.sort();
        println!("{}", names.join("\n"));
        return;
    }
    let id = if library.has(event_id(name)) { event_id(name) } else { event_id(&name.to_ascii_uppercase()) };
    if args.iter().any(|a| a == "describe") {
        println!("{}", library.describe(id).unwrap_or_else(|| format!("no event named {name}")));
        return;
    }
    println!("{name}: {} waveforms decoded", library.prepare(id));
    let mut mixer = Mixer::new(RATE);
    let Some(mut handle) = mixer.play(&library, id, 1.0) else {
        eprintln!("no event named {name}");
        return;
    };
    let trace: Option<Vec<[f32; 4]>> = option("trace=").map(|path| {
        std::fs::read_to_string(path).expect("reading trace CSV").lines().map(|line| {
            let row: Vec<f32> = line.split(',').map(|v| v.parse().expect("trace number")).collect();
            row.try_into().expect("time,vehicle,rpm,load columns")
        }).collect()
    });
    let start = option("start=").and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0);
    let mut trace_index = 0;
    let mut in_vehicle = true;
    mixer.set_parameter(handle, "2dPanLevel", 1.0);
    let range = |key: &str| {
        option(key).and_then(|s| {
            let (a, b) = s.split_once("..")?;
            Some((a.parse::<f32>().ok()?, b.parse::<f32>().ok()?))
        })
    };
    let sweep = range("rpm=");
    let speeds = range("speed=");
    if let Some(load) = option("load=").and_then(|s| s.parse().ok()) {
        mixer.set_parameter(handle, "load", load);
    }
    if let Some(force) = option("impactForce=").and_then(|s| s.parse().ok()) {
        mixer.set_parameter(handle, tf2_core::sound::IMPACT_FORCE, force);
    }
    let block = RATE as usize / 100;
    let blocks = (seconds * 100.0) as usize;
    let mut samples: Vec<i16> = Vec::new();
    let mut buffer = vec![0.0f32; block * 2];
    // `drive`: the game's gearbox with his numbers, pulling away flat out for two thirds of
    // the file (the speed rising to his 35 over four seconds) and then coasting down.
    let mut gearbox = args.iter().any(|a| a == "drive").then(|| {
        let gears = tf2_core::sound::load_gears(&game, "Bumblebee").expect("the gear table");
        tf2_core::sound::Gearbox::new(gears, 15000.0, 1.6, 5000.0, 35.0, 3.0)
    });
    let mut speed = 0.0f32;
    for i in 0..blocks {
        if let Some(rows) = trace.as_ref().filter(|rows| !rows.is_empty()) {
            let t = start + i as f32 / 100.0;
            while trace_index + 1 < rows.len() && rows[trace_index + 1][0] <= t {
                trace_index += 1;
            }
            let [_, vehicle, rpm, load] = rows[trace_index];
            if vehicle == 0.0 && in_vehicle {
                mixer.stop(handle);
            } else if vehicle != 0.0 && !in_vehicle {
                handle = mixer.play(&library, id, 1.0).unwrap();
                mixer.set_parameter(handle, "2dPanLevel", 1.0);
            }
            in_vehicle = vehicle != 0.0;
            if in_vehicle {
                mixer.set_parameter(handle, "rpm", rpm);
                mixer.set_parameter(handle, "load", load);
            }
        }
        // A block is 10 ms; the game updates every 32.
        if let Some(gearbox) = gearbox.as_mut().filter(|_| i * 10 / 32 != (i + 1) * 10 / 32) {
            let pulling = i < blocks * 2 / 3;
            speed = if pulling { (speed + 35.0 / 4.0 * 0.032).min(35.0) } else { (speed - 0.25).max(0.0) };
            let input = tf2_core::sound::GearboxInput {
                velocity: [0.0, speed, 0.0],
                trigger: if pulling { 1.0 } else { 0.0 },
                grounded: true,
                ..Default::default()
            };
            gearbox.update(&input, 0.032);
            mixer.set_parameter(handle, "rpm", gearbox.rpm);
            mixer.set_parameter(handle, "load", gearbox.load);
            if i % 25 == 0 {
                println!("{:5.2} s  gear {}  rpm {:6.0}  load {:.2}", i as f32 / 100.0, gearbox.gear + 1, gearbox.rpm, gearbox.load);
            }
        }
        if let Some((from, to)) = sweep {
            mixer.set_parameter(handle, "rpm", from + (to - from) * i as f32 / blocks as f32);
        }
        if let Some((from, to)) = speeds {
            mixer.set_parameter(handle, "speed", from + (to - from) * i as f32 / blocks as f32);
        }
        mixer.render(&library, &mut buffer);
        samples.extend(buffer.iter().map(|v| (v * 32767.0) as i16));
        if mixer.playing() == 0 && trace.is_none() {
            break;
        }
    }
    let bytes = samples.len() as u32 * 2;
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + bytes).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&RATE.to_le_bytes());
    wav.extend_from_slice(&(RATE * 4).to_le_bytes());
    wav.extend_from_slice(&4u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&bytes.to_le_bytes());
    for sample in &samples {
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    std::fs::File::create(out).and_then(|mut f| f.write_all(&wav)).expect("writing the WAV file");
    println!("{out}: {:.2} s", samples.len() as f32 / 2.0 / RATE as f32);
}
