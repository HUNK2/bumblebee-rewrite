//! Bumblebee's sounds on the default output device.
//!
//! What plays and when is the game's data: his clips carry sound cues (`AnimPlaySoundEvent`,
//! `AnimStopSoundEvent`, `FootStepEvent`), his `soundTable` names the engine and turbo
//! events, and the event files say which samples those are (`tf2_core::sound`). The samples
//! are decoded from the install when the test starts.
//!
//! The engine's revolutions and load are the game's gearbox (`tf2_core::sound::Gearbox`,
//! `FUN_007a48e0`). [game]
//! Footsteps probe the named animated foot and choose the material preset. Sources have
//! independent positions; stereo projection stands in for FMOD's full 3D renderer.
//! Tyre and suspension cues use their selected rear wheel's position. Shared
//! sounds (steps, jumps, landings, turbo) come from one level's event file.
//! A melee hit sounds by the surface it struck: his `meleeHitSparkPreset` names an event per
//! surface material (`FUN_0072d9c0` -> `FUN_0077cd60`). [game] [data] The surface is the
//! target's, and the dummies' is a stand-in.
//! The tyres' rolling and skid loops and the suspension's thump are the game's rules
//! (`tf2_core::sound::tyre_sounds`, `FUN_007347c0`). [game] Stand-ins: the ground is the
//! default surface everywhere. Actual rear-wheel spring compression drives the thump.
//! Collision reactions use his SoundPhysicsReactor presets; combat chatter uses his
//! installed radio lines, frequency, history and delay. Gameplay sounds only: the user
//! excluded music/ambience. `vehicleDriftStart` is in his data but no code
//! of the game's reads it.

use std::path::Path;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use bevy::prelude::*;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tf2_core::character::{CharacterData, ImpactPreset, Sounds};
use tf2_core::formats::anim::{Clip, SoundCue};
use tf2_core::sim::State;
use tf2_core::sound::{Handle, IMPACT_FORCE, Library, Mixer, TyreSounds, tyre_sounds};
use tf2_core::tuning::Tuning;
use tf2_core::{collisionfx, sim};
use tf2_core::chatter;

use crate::animation::AnimLibrary;

/// The level whose event file gives the shared sounds, unless `TF2_SOUND_LEVEL` names another.
const DEFAULT_LEVEL: &str = "us_city_00";

struct Playing {
    mixer: Mixer,
    /// Sounds a clip started that end with it.
    with_clip: Vec<Handle>,
    engine: Option<Handle>,
    sounds: Sounds,
    melee_impact: ImpactPreset,
    tyre_skid: ImpactPreset,
    tyre_road: ImpactPreset,
    /// The tyres' two loops, each with the surface it was started for.
    skid: Option<(Handle, u32)>,
    road: Option<(Handle, u32)>,
    was_vehicle: bool,
    /// His speed at the last tick, in metres per second.
    speed: f32,
    turbo_left: f32,
    turbo_cooldown: f32,
    /// Cues that named an event no loaded file has, so each is reported once.
    unknown: Vec<u32>,
    sources: Vec<(Handle, Source)>,
    position: Vec3,
    regenerating: bool,
    collision_defs: Vec<collisionfx::SoundDef>,
    collision_original: Vec<collisionfx::SoundDef>,
    collision_history: collisionfx::History,
    collision_pending: BTreeMap<u32, tf2_core::vehicle::Contact>,
    collision_clock: f32,
    collision_loops: BTreeMap<(usize, u32), Handle>,
    hardness: std::collections::HashMap<u32, bool>,
    chatter: Option<chatter::Speaker>,
    voice: Option<Handle>,
    pending_voice: Option<u32>,
    voice_delay: f32,
    low_health_spoken: bool,
    death_spoken: bool,
    chatter_clock: f32,
    random: u32,
    pending_steps: Vec<u32>,
    step_impact: ImpactPreset,
    listener: (Vec3, Vec3),
    combat_target: bool,
    trace: bool,
    triggered: BTreeMap<(bool, usize), Handle>,
    triggered_once: std::collections::BTreeSet<(bool, usize)>,
    trigger_definitions: [Vec<tf2_core::formats::scene::SoundPlayerDef>; 2],
    kept_sounds: std::collections::BTreeSet<(bool, usize)>,
    projectile_audio: BTreeMap<u32, (Vec<u32>, Vec<u32>)>,
    projectiles: BTreeMap<u32, Vec<Handle>>,
    weapon_impacts: BTreeMap<u32, ImpactPreset>,
    pending_climb_steps: Vec<usize>,
    pending_spawns: Vec<u32>,
    climb_impact: Vec<ImpactPreset>,
}

enum Source { Body(Vec3), Fixed(Vec3), Moving(Vec3, f32) }

#[derive(Resource, Clone)]
pub struct SoundOut {
    library: Arc<Library>,
    playing: Arc<Mutex<Playing>>,
}

/// Loads the event files, decodes what Bumblebee can ask for and starts the output. `None`
/// (with the reason printed) leaves the test silent.
pub fn start(game_dir: &Path, data: &CharacterData, animations: &AnimLibrary, tuning: &Tuning, scout: &tf2_core::combat::Scout) -> Option<SoundOut> {
    let volume = std::env::var("TF2_SOUND_VOLUME").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(1.0);
    if volume <= 0.0 {
        eprintln!("sound: off (TF2_SOUND_VOLUME is 0)");
        return None;
    }
    let level = std::env::var("TF2_SOUND_LEVEL").unwrap_or_else(|_| DEFAULT_LEVEL.to_owned());
    let began = Instant::now();
    let (mut library, problems) = Library::load(&[
        game_dir.join(r"characters\eng\bumblebee.fev"),
        game_dir.join("levels").join(&level).join(r"eng\mission.fev"),
        game_dir.join(r"audio\eng\global.fev"),
        game_dir.join(r"audio\eng\global_streaming.fev"),
        game_dir.join(r"characters\eng\dcomscout.fev"),
    ]);
    for problem in &problems {
        eprintln!("sound: {problem}");
    }
    let sounds = data.sounds;
    let chatter_ids: Vec<_> = data.chatter.iter().map(|l| l.event).collect();
    match library.add_programmer_bank(&game_dir.join(r"audio\eng\CHATTER.fsb"), tf2_core::sound::event_id("ProgrammerSoundPlayer"), &chatter_ids) {
        Ok(count) => eprintln!("sound: {count} of {} chatter table entries bound to his installed radio samples", data.chatter.len()),
        Err(e) => eprintln!("sound: chatter unavailable ({e})"),
    }
    let chatter = match chatter::Tuning::load(game_dir) {
        Ok(tuning) => Some(chatter::Speaker::new(data.chatter.clone(), tuning)),
        Err(e) => { eprintln!("sound: {e}"); None }
    };
    // Everything his clips and his sound table can ask for, decoded now rather than in play.
    let mut wanted = vec![sounds.engine, sounds.drift_start, sounds.suspension, sounds.turbo, sounds.turbo_ready];
    wanted.extend(data.step_impact.infos.iter().map(|info| info.sound));
    wanted.extend(data.climb_impact.iter().flat_map(|p| &p.infos).map(|info| info.sound));
    wanted.extend(std::iter::once(&data.robot).chain(data.vehicle.iter())
        .flat_map(|o| &o.sound_players).map(|p| p.id));
    wanted.extend(std::iter::once(&data.robot).chain(data.vehicle.iter())
        .flat_map(|o| &o.spawn_sound_players).flat_map(|s| &s.2).copied());
    wanted.extend(data.weapon_effects.iter().flat_map(|w| w.flight_sounds.iter().chain(&w.blast_sounds)).copied());
    wanted.extend(data.weapon_effects.iter().flat_map(|w| &w.impact.infos).map(|i| i.sound));
    let sets = animations.robot.sets.values().flatten();
    for clip in sets.chain(&animations.vehicle).filter_map(|reference| reference.clip.as_ref()) {
        wanted.extend(clip.sound_events.iter().filter_map(|event| match event.cue {
            SoundCue::Play { id, .. } => Some(id),
            _ => None,
        }));
    }
    for weapon in &data.weapons {
        wanted.extend([weapon.fire_sound, weapon.overheat_sound, weapon.refused_sound]);
    }
    for weapon in &scout.character.weapons {
        wanted.extend([weapon.fire_sound, weapon.overheat_sound, weapon.refused_sound]);
    }
    wanted.extend(data.melee_impact.infos.iter().map(|info| info.sound));
    wanted.extend(data.tyre_skid.infos.iter().chain(&data.tyre_road.infos).map(|info| info.sound));
    wanted.extend(data.vehicle.iter().flat_map(|o| &o.collision_sounds)
        .flat_map(|d| d.choices.iter().flatten()).map(|c| c.id));
    wanted.extend(["UI_LOCK_ON", "UI_LOCKED_ON", "UI_REGEN_START"].map(tf2_core::sound::event_id));
    wanted.extend(&chatter_ids);
    wanted.sort_unstable();
    wanted.dedup();
    wanted.retain(|&id| id != 0);
    let found = wanted.iter().filter(|&&id| library.has(id)).count();
    for &id in wanted.iter().filter(|&&id| !library.has(id)) { eprintln!("sound: unresolved authored cue {id:#010x}"); }
    let waves: usize = wanted.iter().map(|&id| library.prepare(id)).sum();
    eprintln!(
        "sound: {} events in the files; his data names {}, {found} of them found, {waves} samples decoded in {:.2} s (shared sounds from level {level})",
        library.event_count(),
        wanted.len(),
        began.elapsed().as_secs_f32(),
    );
    let out = SoundOut {
        library: Arc::new(library),
        playing: Arc::new(Mutex::new(Playing::new(data, tuning, chatter))),
    };
    // The stream lives on a thread of its own, which keeps it for as long as the test runs.
    let (library, playing) = (out.library.clone(), out.playing.clone());
    std::thread::spawn(move || {
        if let Err(e) = output(library, playing, volume) {
            eprintln!("sound: no output ({e})");
        }
    });
    Some(out)
}

/// Fills a device buffer of any channel count from the mixer's stereo.
fn render<T: cpal::SizedSample + cpal::FromSample<f32>>(
    out: &mut [T],
    channels: usize,
    mixed: &mut Vec<f32>,
    library: &Library,
    playing: &Mutex<Playing>,
) {
    let frames = out.len() / channels;
    mixed.clear();
    mixed.resize(frames * 2, 0.0);
    playing.lock().unwrap_or_else(|e| e.into_inner()).mixer.render(library, mixed);
    for (frame, pair) in out.chunks_mut(channels).zip(mixed.chunks(2)) {
        for (channel, sample) in frame.iter_mut().enumerate() {
            let value = match (channels, channel) {
                (1, _) => (pair[0] + pair[1]) * 0.5,
                (_, 0) => pair[0],
                (_, 1) => pair[1],
                _ => 0.0,
            };
            *sample = <T as cpal::Sample>::from_sample(value.clamp(-1.0, 1.0));
        }
    }
}

/// Opens the default device at its own rate and feeds it until the test ends.
fn output(library: Arc<Library>, playing: Arc<Mutex<Playing>>, volume: f32) -> Result<(), String> {
    let device = cpal::default_host().default_output_device().ok_or("no output device")?;
    let supported = device.default_output_config().map_err(|e| e.to_string())?;
    let channels = supported.channels() as usize;
    let rate = supported.sample_rate();
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    {
        let mut playing = playing.lock().unwrap_or_else(|e| e.into_inner());
        playing.mixer = Mixer::new(rate);
        playing.mixer.master = volume;
        for (option, categories) in [
            ("TF2_SOUND_FX_VOLUME", &["FX", "FX_LOUD"][..]),
            ("TF2_SOUND_UI_VOLUME", &["UI"][..]),
            ("TF2_SOUND_VOICE_VOLUME", &["VO", "VO_LOUD"][..]),
        ] {
            if let Some(gain) = std::env::var(option).ok().and_then(|v| v.parse::<f32>().ok()).filter(|v| v.is_finite()) {
                for category in categories { playing.mixer.set_category_volume(category, gain); }
            }
        }
        playing.engine = None;
        playing.with_clip.clear();
        playing.sources.clear();
        playing.skid = None;
        playing.road = None;
        playing.collision_loops.clear();
    }
    let problem = |e| eprintln!("sound: {e}");
    let mut mixed = Vec::new();
    let stream = match format {
        cpal::SampleFormat::F32 => device.build_output_stream(
            &config,
            move |out: &mut [f32], _: &cpal::OutputCallbackInfo| render(out, channels, &mut mixed, &library, &playing),
            problem,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_output_stream(
            &config,
            move |out: &mut [i16], _: &cpal::OutputCallbackInfo| render(out, channels, &mut mixed, &library, &playing),
            problem,
            None,
        ),
        other => return Err(format!("the device wants {other:?} samples")),
    }
    .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    eprintln!("sound: output started ({rate} Hz, {channels} channels)");
    loop {
        std::thread::park();
    }
}

impl Playing {
    fn new(data: &CharacterData, tuning: &Tuning, chatter: Option<chatter::Speaker>) -> Self {
        Self {
            mixer: Mixer::new(48000), with_clip: Vec::new(), engine: None, sounds: data.sounds,
            melee_impact: data.melee_impact.clone(), tyre_skid: data.tyre_skid.clone(), tyre_road: data.tyre_road.clone(),
            skid: None, road: None, was_vehicle: false, speed: 0.0, turbo_left: 0.0, turbo_cooldown: 0.0,
            unknown: Vec::new(), sources: Vec::new(), position: Vec3::ZERO, regenerating: false,
            collision_defs: data.vehicle.iter().flat_map(|o| &o.collision_sounds).cloned().collect(),
            collision_original: data.vehicle.iter().flat_map(|o| &o.collision_sounds).cloned().collect(),
            collision_history: collisionfx::History::default(), collision_pending: BTreeMap::new(),
            collision_clock: 0.0, collision_loops: BTreeMap::new(), hardness: tuning.surface_hardness.clone(),
            chatter, voice: None, pending_voice: None, voice_delay: 0.0, low_health_spoken: false,
            death_spoken: false, chatter_clock: 0.0, random: 0x2545_f491, pending_steps: Vec::new(),
            step_impact: data.step_impact.clone(), listener: (Vec3::ZERO, Vec3::X), combat_target: false,
            trace: std::env::var("TF2_SOUND_TRACE").is_ok_and(|v| v == "1"),
            triggered: BTreeMap::new(),
            triggered_once: std::collections::BTreeSet::new(),
            trigger_definitions: [data.robot.sound_players.clone(), data.vehicle.as_ref().map_or_else(Vec::new, |o| o.sound_players.clone())],
            kept_sounds: std::collections::BTreeSet::new(),
            projectile_audio: data.weapon_effects.iter().map(|w| (w.name, (w.flight_sounds.clone(), w.blast_sounds.clone()))).collect(),
            projectiles: BTreeMap::new(),
            weapon_impacts: data.weapon_effects.iter().map(|w| (w.name, w.impact.clone())).collect(),
            pending_climb_steps: Vec::new(), pending_spawns: Vec::new(), climb_impact: data.climb_impact.clone(),
        }
    }

    fn trigger(&mut self, library: &Library, key: (bool, usize), definition: &tf2_core::formats::scene::SoundPlayerDef) {
        if definition.once && self.triggered_once.contains(&key) { return; }
        if let Some(&old) = self.triggered.get(&key) {
            if self.mixer.is_playing(old) && !definition.retrigger { return; }
            self.mixer.stop(old);
        }
        if let Some(handle) = self.play(library, definition.id) {
            self.triggered.insert(key, handle);
            self.triggered_once.insert(key);
        }
    }

    /// [game] SoundPlayer receives the same persistent Trigger/UnTrigger messages
    /// as particles. In particular, Camaro boost audio follows turbo_fx_on, including
    /// independent drift/standard stops and DriveMode exit, rather than a new timer.
    fn kept(&mut self, library: &Library, state: &State, tuning: &Tuning) {
        let names = tf2_core::fxstate::kept(state, tuning, false); // No Bumblebee headlight sound players.
        let wanted = &names;
        let desired: std::collections::BTreeSet<_> = self.trigger_definitions.iter().enumerate()
            .flat_map(|(form, defs)| defs.iter().enumerate().filter_map(move |(index, d)| wanted.contains(&d.script).then_some((form != 0, index))))
            .collect();
        for key in self.kept_sounds.difference(&desired) {
            if let Some(handle) = self.triggered.remove(key) { self.mixer.stop(handle); }
        }
        let starts: Vec<_> = desired.difference(&self.kept_sounds).copied().collect();
        for key in starts {
            let definition = self.trigger_definitions[key.0 as usize][key.1].clone();
            self.trigger(library, key, &definition);
        }
        self.kept_sounds = desired;
    }
    fn chatter(&mut self, library: &Library, state: &State, tuning: &Tuning, dt: f32) {
        self.chatter_clock += dt;
        // Match the game's update cadence; probability is a per-update trial.
        while self.chatter_clock >= sim::GAME_UPDATE {
            self.chatter_clock -= sim::GAME_UPDATE;
            if let Some(id) = self.pending_voice {
                self.voice_delay -= sim::GAME_UPDATE;
                if self.voice_delay <= 0.0 {
                    self.pending_voice = None;
                    self.voice = self.play(library, id);
                }
                continue;
            }
            let Some(speaker) = &mut self.chatter else { return };
            speaker.step(sim::GAME_UPDATE);
            let low = state.health / tuning.base_health.max(1e-5) < speaker.tuning.low_health;
            if !low { self.low_health_spoken = false; }
            if self.voice.is_some_and(|h| self.mixer.is_playing(h)) || state.car_mode { continue; }
            // [game: 007cb180, raw007cbb44..77 /007cb887..b0]
            // Self-injury below the authored low-health threshold; weapon fire and
            // special attack use the generic attack category. Self death is once.
            let special = state.has_state_class(tuning, "StateAttackSpecial");
            let request = if state.dead() { (!self.death_spoken).then_some((chatter::DIED, chatter::CHARACTER, chatter::SELF)) }
                else if low && !self.low_health_spoken { Some((chatter::INJURED, chatter::CHARACTER, chatter::SELF)) }
                else if state.firing && state.state_class(tuning) == Some("StateWeapon") && self.combat_target { Some((chatter::ATTACK, chatter::GENERIC, chatter::ENEMY)) }
                else if special { Some((chatter::ATTACK, chatter::GENERIC, chatter::GENERIC_SUB)) }
                else { None };
            let Some((action, category, sub)) = request else { continue };
            // [assumed] Deterministic RNG replaces the original clock-mixed random
            // source; probabilities/ranges are original, individual choices differ.
            let seed = &mut self.random;
            let mut roll = || { *seed ^= *seed << 13; *seed ^= *seed >> 17; *seed ^= *seed << 5;
                (*seed >> 8) as f32 / (1u32 << 24) as f32 };
            let Some(line) = speaker.choose(action, category, [sub, 0], &mut roll) else { continue };
            let [min, max] = speaker.tuning.delay;
            self.voice_delay = min + (max - min) * roll();
            // [game: raw007cd794..be] Queue playback for game clock + speak delay.
            if library.has(line.event) {
                self.pending_voice = Some(line.event);
                self.low_health_spoken |= action == chatter::INJURED;
                self.death_spoken |= action == chatter::DIED;
            }
        }
    }
    fn play(&mut self, library: &Library, id: u32) -> Option<Handle> {
        let handle = self.mixer.play(library, id, 1.0);
        if let Some(handle) = handle {
            if self.trace { eprintln!("sound cue: {}", library.name(id).unwrap_or("unnamed")); }
            self.sources.push((handle, Source::Body(Vec3::ZERO)));
            // [data] the walking hydraulics are louder the faster he goes: their events
            // carry a `speed` parameter (0 to 20). Events without one ignore this.
            self.mixer.set_parameter(handle, "speed", self.speed);
            // [game] 00734630/007336f0/00733800 set5913246c to1 for
            // the human player's vehicle events; the FEV names it2dPanLevel.
            if [self.sounds.engine, self.sounds.turbo, self.sounds.turbo_ready].contains(&id) {
                self.mixer.set_parameter(handle, "2dPanLevel", 1.0);
            }
        }
        if handle.is_none() && id != 0 && !self.unknown.contains(&id) {
            self.unknown.push(id);
            eprintln!("sound: no loaded event file has event {id:#010x}");
        }
        handle
    }

    fn play_at(&mut self, library: &Library, id: u32, point: Vec3, attached: bool) -> Option<Handle> {
        let handle = self.play(library, id)?;
        self.place(handle, point, attached);
        Some(handle)
    }

    fn place(&mut self, handle: Handle, point: Vec3, attached: bool) {
        if let Some((_, source)) = self.sources.iter_mut().find(|(h, _)| *h == handle) {
            *source = if attached { Source::Body(point - self.position) } else { Source::Fixed(point) };
        }
    }

    fn spatial(&mut self, eye: Vec3, right: Vec3) {
        self.sources.retain(|(handle, _)| self.mixer.is_playing(*handle));
        for (handle, source) in &self.sources {
            let point = match source { Source::Body(offset) => self.position + *offset, Source::Fixed(point) | Source::Moving(point, _) => *point };
            let speed = match source { Source::Body(_) => Some(self.speed), Source::Moving(_, speed) => Some(*speed), _ => None };
            if let Some(speed) = speed {
                // [game: 0076e100/00595770] Attached events follow body speed every update.
                self.mixer.set_parameter(*handle, "speed", speed);
                self.mixer.set_parameter_hash(*handle, 0xd9a785a4, speed);
            }
            let offset = point - eye;
            self.mixer.set_spatial(*handle, offset.length(), offset.normalize_or_zero().dot(right));
        }
    }

    fn collisions(&mut self, library: &Library, state: &State, dt: f32) {
        let driving = state.mode == sim::Mode::Drive;
        let mut events = Vec::new();
        if !driving {
            events = self.collision_history.clear();
            self.collision_pending.clear();
            self.collision_clock = 0.0;
        } else {
            for contact in &state.vehicle_contacts { self.collision_pending.insert(contact.key, *contact); }
            self.collision_clock += dt;
        }
        while driving && self.collision_clock >= sim::GAME_UPDATE {
            self.collision_clock -= sim::GAME_UPDATE;
            for (_, contact) in std::mem::take(&mut self.collision_pending) {
                if self.collision_defs.iter().any(|d| d.filter.accepts(&contact)) {
                    // [assumed] Same arena contact adaptation as CollisionFX: first
                    // contact is push3, repeated contact slide5; Lux dispatcher unread.
                    let kind = if self.collision_history.contains(contact.key) { 5 } else { 3 };
                    self.collision_history.touch(contact, kind);
                }
            }
            events.extend(self.collision_history.step(sim::GAME_UPDATE));
        }
        let turn = tf2_core::vehicle::rotation(state.yaw, state.pitch, state.roll);
        for event in events {
            let keys: Vec<_> = self.collision_loops.keys().filter(|(_, key)| *key == event.key).copied().collect();
            for key in keys {
                if let Some(handle) = self.collision_loops.remove(&key) { self.mixer.stop(handle); }
            }
            if event.kind == collisionfx::Kind::End { continue; }
            let hard = self.hardness.get(&event.surface).copied().unwrap_or(false);
            let bottom = (turn.inverse() * event.normal).normalize_or_zero().z > 0.7;
            for index in 0..self.collision_defs.len() {
                if !self.collision_defs[index].filter.accepts_speed(event.speed) { continue; }
                let Some(choice) = self.collision_defs[index].select(event.kind, hard, bottom) else { continue; };
                if let Some(handle) = self.play_at(library, choice.id, event.point, choice.attached) {
                    if event.kind == collisionfx::Kind::Impact {
                        // [game: 0077f620] Case-sensitive impactForce hash, retained.
                        // [assumed] Arena contacts provide approach speed rather than
                        // Lux's contact impulse scalar at+5c; original contacts settle it.
                        self.mixer.set_parameter(handle, IMPACT_FORCE, event.speed);
                    } else {
                        self.collision_loops.insert((index, event.key), handle);
                    }
                }
            }
        }
        for (&(_, key), &handle) in &self.collision_loops {
            self.mixer.set_parameter(handle, "speed", state.vel.length());
            if let Some(point) = self.collision_history.point(key) {
                if let Some((_, source)) = self.sources.iter_mut().find(|(h, _)| *h == handle) {
                    if let Source::Body(offset) = source {
                        *offset += (point - state.pos - *offset).clamp_length_max(0.01);
                    }
                }
            }
        }
    }

    /// Keeps one of the tyres' loops (the skid's or the rolling one) playing for `surface`:
    /// started when wanted, started again when the surface changes, stopped when not.
    /// [game] `FUN_007347c0`
    fn tyre_loop(&mut self, library: &Library, skid: bool, surface: Option<u32>) {
        let current = if skid { self.skid } else { self.road };
        let next = match (current, surface) {
            (Some((handle, was)), Some(now)) if was == now && self.mixer.is_playing(handle) => current,
            (current, wanted) => {
                if let Some((handle, _)) = current {
                    self.mixer.stop(handle);
                }
                wanted.and_then(|surface| {
                    let preset = if skid { &self.tyre_skid } else { &self.tyre_road };
                    let id = preset.pick(surface).map_or(0, |info| info.sound);
                    self.play(library, id).map(|handle| (handle, surface))
                })
            }
        };
        if skid {
            self.skid = next;
        } else {
            self.road = next;
        }
    }
}

impl SoundOut {
    /// [assumed] The arena's best aim-assist target supplies the original chatter
    /// component's combat-target presence. Original target-query telemetry settles it.
    pub fn combat_target(&self, present: bool) { self.playing().combat_target = present; }
    fn playing(&self) -> MutexGuard<'_, Playing> {
        self.playing.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Plays one event by id, for things that are not clip cues (the weapons).
    pub fn play_event(&self, id: u32) {
        self.playing().play(&self.library, id);
    }

    /// [game: 0076d490/0076e500] Respect each component's retrigger/once flags.
    /// Other copies from clip cues have separate handles.
    pub fn trigger_event(&self, car: bool, index: usize, definition: &tf2_core::formats::scene::SoundPlayerDef) {
        let mut playing = self.playing();
        let key = (car, index);
        playing.trigger(&self.library, key, definition);
    }

    /// [game: 0076e100] Component sounds follow their attachment's world matrix,
    /// including bone motion/turning. [assumed] As with foot/spawn cues, use the
    /// frame's final animated pose; original callback order settles its timing.
    pub fn component_sources(&self, car: bool, world: &[bevy::math::Affine3A]) {
        let mut playing = self.playing();
        let updates: Vec<_> = playing.triggered.iter().filter(|(key, _)| key.0 == car)
            .filter_map(|(key, handle)| {
                let definition = playing.trigger_definitions[car as usize].get(key.1)?;
                let point = world.get(definition.node)?.translation.into();
                Some((*handle, point))
            }).collect();
        let speed = playing.speed;
        for (handle, point) in updates {
            if let Some((_, source)) = playing.sources.iter_mut().find(|(h, _)| *h == handle) {
                *source = Source::Moving(point, speed);
            }
        }
        let (eye, right) = playing.listener;
        playing.spatial(eye, right);
    }

    /// A one-shot at its world source, including the scout's gun rather than Bumblebee.
    pub fn play_event_at(&self, id: u32, point: Vec3) {
        let mut playing = self.playing();
        playing.play_at(&self.library, id, point, false);
        let (eye, right) = playing.listener;
        playing.spatial(eye, right);
    }

    /// [game: 0076dcf0] Anonymous projectile players start at object creation.
    /// [assumed] Source is the simulated missile centre: its model-node offset is
    /// absent from this projectile adapter. Original source telemetry settles it.
    pub fn projectile_start(&self, number: u32, weapon: u32, point: Vec3) {
        let mut playing = self.playing();
        let ids = playing.projectile_audio.get(&weapon).map(|p| p.0.clone()).unwrap_or_default();
        let handles = ids.into_iter().filter_map(|id| playing.play_at(&self.library, id, point, false)).collect();
        playing.projectiles.insert(number, handles);
        let (eye, right) = playing.listener;
        playing.spatial(eye, right);
    }

    /// Each missile has its own handles and position; other missiles keep playing.
    pub fn projectile_move(&self, number: u32, point: Vec3, speed: f32) {
        let mut playing = self.playing();
        let handles = playing.projectiles.get(&number).cloned().unwrap_or_default();
        for (handle, source) in &mut playing.sources {
            if handles.contains(handle) { *source = Source::Moving(point, speed); }
        }
        let (eye, right) = playing.listener;
        playing.spatial(eye, right);
    }

    /// Object destruction stops flight players and starts its detached players once.
    /// [assumed] Detached sound sources use the endpoint of the simulated missile;
    /// the original DetachLink matrices/source positions would settle their offsets.
    pub fn projectile_end(&self, number: u32, weapon: u32, point: Vec3) {
        let mut playing = self.playing();
        let Some(handles) = playing.projectiles.remove(&number) else { return };
        for handle in handles { playing.mixer.stop(handle); }
        let ids = playing.projectile_audio.get(&weapon).map(|p| p.1.clone()).unwrap_or_default();
        for id in ids { playing.play_at(&self.library, id, point, false); }
        let (eye, right) = playing.listener;
        playing.spatial(eye, right);
    }

    /// [game: 0077cd60] The bullet's material impact preset supplies its sound too.
    pub fn weapon_impact(&self, weapon: u32, surface: u32, point: Vec3) {
        let mut playing = self.playing();
        let id = playing.weapon_impacts.get(&weapon).and_then(|p| p.pick(surface)).map_or(0, |i| i.sound);
        playing.play_at(&self.library, id, point, false);
        let (eye, right) = playing.listener;
        playing.spatial(eye, right);
    }

    /// Stops every sound of one event.
    pub fn stop_event(&self, id: u32) {
        self.playing().mixer.stop_event(id);
    }

    /// The cues of `clip` between two positions in ticks. `to` below `from` means the clip
    /// went round; a clip that has just started has `from` at 0 and its cues at tick 0 play.
    /// `rerun`: the clip had gone round before this stretch; a cue that does not allow a
    /// rerun (`allowReRun` 0) then stays quiet (`FUN_0053ab90`). [game]
    pub fn clip_cues(&self, clip: &Clip, from: f32, to: f32, rerun: bool) {
        if clip.sound_events.is_empty() {
            return;
        }
        let mut playing = self.playing();
        for event in &clip.sound_events {
            let t = event.time;
            let (due, again) = if to >= from {
                ((t > from && t <= to) || (from == 0.0 && t == 0.0 && to > 0.0), rerun)
            } else if t > from {
                (true, rerun)
            } else {
                (t <= to, true)
            };
            if !due || (again && !event.rerun) {
                continue;
            }
            match event.cue {
                SoundCue::Play { id, auto_stop } => {
                    if let (Some(handle), true) = (playing.play(&self.library, id), auto_stop) {
                        playing.with_clip.push(handle);
                    }
                }
                SoundCue::Stop { id } => playing.mixer.stop_event(id),
                SoundCue::FootStep { side } => playing.pending_steps.push(side),
                SoundCue::ClimbStep { limb } => playing.pending_climb_steps.push(limb),
            }
        }
    }

    /// The robot's clip changed: sounds the old one asked to end with it stop.
    pub fn clip_changed(&self) {
        let mut playing = self.playing();
        for handle in std::mem::take(&mut playing.with_clip) {
            playing.mixer.stop(handle);
        }
    }

    /// Resolve each named foot's cue against its current animated world position.
    /// [assumed] Synchronous arena ray replaces the original async FootStep probe;
    /// it uses the installed attachment's height and chooses its material preset.
    pub fn footsteps(&self, world: &[bevy::math::Affine3A], definitions: &[tf2_core::formats::scene::FootstepDef], arena: &impl sim::World) {
        let mut playing = self.playing();
        for side in std::mem::take(&mut playing.pending_steps) {
            for hit in tf2_core::stepfx::footsteps(side, world, definitions, arena) {
                let Some(id) = playing.step_impact.pick(hit.surface).map(|i| i.sound) else { continue };
                playing.play_at(&self.library, id, hit.point, false);
            }
        }
        let (eye, right) = playing.listener;
        playing.spatial(eye, right);
    }

    /// [game: 0073fa20] ClimbStep probes 1.5m either side of the limb, along the
    /// object's forward row, and the contact's material selects its preset.
    /// [assumed] Like our footsteps, this is synchronous from the final pose;
    /// original asynchronous query/callback timing would settle the ordering.
    pub fn climb_steps(&self, world: &[bevy::math::Affine3A], steps: &[(usize, u32)], forward: Vec3, arena: &impl sim::World) {
        let mut playing = self.playing();
        let pending = std::mem::take(&mut playing.pending_climb_steps);
        for limb in pending {
            for hit in tf2_core::stepfx::climbing(limb, world, steps, forward, arena) {
                let id = playing.climb_impact.get(limb).and_then(|p| p.pick(hit.surface)).map_or(0, |i| i.sound);
                playing.play_at(&self.library, id, hit.point, false);
            }
        }
        let (eye, right) = playing.listener;
        playing.spatial(eye, right);
    }

    /// [game: 0076dcf0/007fc270] A spawner's anonymous SoundPlayer starts with its
    /// object at the named bone. [assumed] Use the frame's final animated bone pose,
    /// matching our foot/climb probe ordering; original callback timing settles this.
    pub fn spawn_cues(&self, world: &[bevy::math::Affine3A], definitions: &[(usize, u32, Vec<u32>)]) {
        let mut playing = self.playing();
        for name in std::mem::take(&mut playing.pending_spawns) {
            for (node, _, ids) in definitions.iter().filter(|(_, script, _)| *script == name) {
                let Some(point) = world.get(*node).map(|at| Vec3::from(at.translation)) else { continue };
                for &id in ids { playing.play_at(&self.library, id, point, false); }
            }
        }
        let (eye, right) = playing.listener;
        playing.spatial(eye, right);
    }

    /// Once a frame: the engine, the turbo, and where the listener (the camera, at `eye`
    /// looking along `forward`) is from him.
    pub fn tick(&self, state: &State, tuning: &Tuning, eye: Vec3, forward: Vec3, dt: f32) {
        let mut guard = self.playing();
        let playing = &mut *guard;
        let to_him = state.pos - eye;
        playing.mixer.distance = to_him.length();
        // The listener's right is across its look and the world's up (z).
        let right = forward.cross(Vec3::Z).normalize_or_zero();
        playing.listener = (eye, right);
        playing.mixer.side = to_him.normalize_or_zero().dot(right);
        playing.speed = state.vel.length();
        playing.position = state.pos;
        playing.kept(&self.library, state, tuning);
        if state.damage_timers.regenerating && !playing.regenerating {
            playing.play(&self.library, tf2_core::sound::event_id("UI_REGEN_START"));
        }
        playing.regenerating = state.damage_timers.regenerating;
        playing.collisions(&self.library, state, dt);
        playing.chatter(&self.library, state, tuning, dt);
        playing.pending_spawns.extend(&state.triggers);
        // [game] each hit of his own body plays his melee preset's sound for the surface
        // struck (`FUN_007189b0` -> `FUN_0072d9c0`), with his body's speed as the event's
        // `speed` (a second parameter, hash d9a785a4, gets the same; its name is not known).
        // An explosion's hits do not come this way. Not there: the preset's effect
        // (`r_hit_melee`, tied to the object hit) and the mark on the surface.
        for hit in state.hits.iter().filter(|hit| !hit.blast) {
            if let Some(id) = playing.melee_impact.pick(hit.surface).map(|info| info.sound) {
                playing.play_at(&self.library, id, hit.point, false);
            }
        }
        let vehicle = state.is_vehicle();
        if vehicle && playing.engine.is_none_or(|handle| !playing.mixer.is_playing(handle)) {
            let id = playing.sounds.engine;
            playing.engine = playing.play(&self.library, id);
        } else if !vehicle && let Some(handle) = playing.engine.take() {
            playing.mixer.stop(handle);
        }
        // [game] The sound reads the same gearbox that the wheel set updated.
        // A separate render-frame gearbox can miss the start of a transformation
        // and diverge when drive updates precede the visible vehicle form.
        if let (true, Some(handle), Some(gearbox)) = (vehicle, playing.engine, state.vehicle_gearbox.as_ref()) {
            playing.mixer.set_parameter(handle, "rpm", gearbox.rpm);
            playing.mixer.set_parameter(handle, "load", gearbox.load);
        }
        // [game] the tyres and the suspension (`FUN_007347c0`, `tyre_sounds`): a rolling
        // loop and, while they skid, a skid loop, each picked by the ground's surface from
        // his `vehicleTireRoadFX` / `vehicleTireSkidFX`, and `vehicleSuspension` when a
        // rear spring is knocked short. Out of the car both loops stop.
        let mut heard = state.tyre_inputs.last().map(tyre_sounds);
        if !vehicle {
            heard = Some(TyreSounds::default());
        }
        if let Some(heard) = heard {
            playing.tyre_loop(&self.library, true, heard.skid);
            playing.tyre_loop(&self.library, false, heard.road);
        }
        // [game] both loops follow the body's speed (`FUN_00595770` ties the event's
        // `speed` to the object it is on).
        for (handle, _) in playing.skid.into_iter().chain(playing.road) {
            playing.mixer.set_parameter(handle, "speed", playing.speed);
            if let Some(input)=state.tyre_inputs.last().filter(|input|input.per_wheel) {
                playing.place(handle, input.wheels[input.sound_wheel].point, true);
            }
        }
        for input in state.tyre_inputs.iter().filter(|_| vehicle) {
            let Some(force)=tyre_sounds(input).suspension else {continue;};
            let id = playing.sounds.suspension;
            // The force goes to a parameter the event does not have, as in the game
            // (`IMPACT_FORCE`): the mixer ignores a name it does not find.
            if let Some(handle) = playing.play(&self.library, id) {
                playing.mixer.set_parameter(handle, IMPACT_FORCE, force);
                if input.per_wheel {
                    playing.place(handle, input.wheels[input.suspension_wheel].point, true);
                }
            }
        }
        // [data] the sound table's turbo event when it starts, and its "ready" when the
        // cooldown runs out.
        // [game: 00732e60] Both standard and drift turbo play this event on start.
        if vehicle && state.turbo_started {
            let id = playing.sounds.turbo;
            playing.play(&self.library, id);
        }
        if vehicle && playing.was_vehicle && state.turbo_cooldown <= 0.0 && playing.turbo_cooldown > 0.0 {
            let id = playing.sounds.turbo_ready;
            playing.play(&self.library, id);
        }
        playing.turbo_left = state.turbo_left;
        playing.turbo_cooldown = state.turbo_cooldown;
        playing.was_vehicle = vehicle;
        playing.spatial(eye, right);
    }

    /// Everything fades out (a reset).
    pub fn silence(&self) {
        let mut playing = self.playing();
        playing.mixer.stop_all();
        playing.with_clip.clear();
        playing.engine = None;
        playing.skid = None;
        playing.road = None;
        playing.was_vehicle = false;
        playing.regenerating = false;
        playing.sources.clear();
        playing.collision_defs = playing.collision_original.clone();
        playing.collision_history.clear();
        playing.collision_pending.clear();
        playing.collision_loops.clear();
        playing.collision_clock = 0.0;
        playing.voice = None;
        playing.pending_voice = None;
        playing.combat_target = false;
        playing.voice_delay = 0.0;
        playing.low_health_spoken = false;
        playing.death_spoken = false;
        playing.chatter_clock = 0.0;
        playing.pending_steps.clear();
        playing.triggered.clear();
        playing.triggered_once.clear();
        playing.kept_sounds.clear();
        playing.projectiles.clear();
        playing.pending_climb_steps.clear();
        playing.pending_spawns.clear();
        if let Some(speaker) = &mut playing.chatter { speaker.clear(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offline() -> (SoundOut, Tuning, CharacterData) {
        let game = Path::new(r"C:\Games2");
        let data = tf2_core::character::load(game, "bumblebee").unwrap();
        let tuning = tf2_core::tuning::load(game, "Bumblebee").unwrap();
        let (mut library, errors) = Library::load(&[
            game.join(r"characters\eng\bumblebee.fev"), game.join(r"levels\us_city_00\eng\mission.fev"),
            game.join(r"audio\eng\global.fev"), game.join(r"audio\eng\global_streaming.fev"),
        ]);
        assert!(errors.is_empty(), "{errors:?}");
        let ids: Vec<_> = data.chatter.iter().map(|l| l.event).collect();
        library.add_programmer_bank(&game.join(r"audio\eng\CHATTER.fsb"), tf2_core::sound::event_id("ProgrammerSoundPlayer"), &ids).unwrap();
        let speaker = chatter::Speaker::new(data.chatter.clone(), chatter::Tuning::load(game).unwrap());
        let playing = Playing::new(&data, &tuning, Some(speaker));
        (SoundOut { library: Arc::new(library), playing: Arc::new(Mutex::new(playing)) }, tuning, data)
    }

    #[test]
    fn regeneration_is_an_edge_and_chatter_waits_before_playing_then_resets() {
        let (sound, tuning, _) = offline();
        let mut state = State::new(&tuning);
        let tick = |state: &State| sound.tick(state, &tuning, Vec3::Y * -10.0, Vec3::Y, sim::GAME_UPDATE);
        state.damage_timers.regenerating = true;
        tick(&state);
        assert_eq!(sound.playing().mixer.playing(), 1);
        tick(&state);
        assert_eq!(sound.playing().mixer.playing(), 1, "regeneration start must not repeat every frame");
        sound.silence();
        sound.playing().mixer.render(&sound.library, &mut [0.0; 9600]);
        state.damage_timers.regenerating = false;
        state.health = tuning.base_health * 0.1;
        for _ in 0..100 {
            tick(&state);
            if sound.playing().pending_voice.is_some() { break; }
        }
        let delay = sound.playing().voice_delay;
        assert!((0.3..=1.5).contains(&delay));
        assert!(sound.playing().pending_voice.is_some());
        assert_eq!(sound.playing().mixer.playing(), 0, "queued speech is silent until its delay");
        for _ in 0..(delay / sim::GAME_UPDATE).floor() as usize { tick(&state); }
        assert_eq!(sound.playing().mixer.playing(), 0);
        tick(&state);
        assert_eq!(sound.playing().mixer.playing(), 1);
        assert!(sound.playing().voice.is_some());
        sound.silence();
        let playing = sound.playing();
        assert!(playing.pending_voice.is_none() && playing.voice.is_none());
        assert!(!playing.low_health_spoken && !playing.death_spoken);
    }

    #[test]
    fn collision_scraping_loop_expires_and_unfolding_clears_contact_audio() {
        let (sound, tuning, _) = offline();
        sound.playing().chatter = None;
        let mut state = State::new(&tuning);
        state.mode = sim::Mode::Drive;
        state.car_mode = true;
        state.vehicle_contacts.push(tf2_core::vehicle::Contact { key: 77, fraction: 0.0, point: Vec3::Y,
            normal: Vec3::X, depth: 0.0, surface: tf2_core::melee::surface::DEFAULT, velocity: Vec3::Y * 20.0 });
        let tick = |state: &State| sound.tick(state, &tuning, Vec3::Y * -10.0, Vec3::Y, sim::GAME_UPDATE);
        for _ in 0..8 { tick(&state); }
        let handle = *sound.playing().collision_loops.values().next().expect("scrape starts after seven slide updates");
        assert!(sound.playing().mixer.is_playing(handle));
        tick(&state);
        assert_eq!(sound.playing().collision_loops.values().next(), Some(&handle), "steady scrape must retain the same loop");
        state.vehicle_contacts.clear();
        for _ in 0..10 { tick(&state); }
        assert!(sound.playing().collision_loops.is_empty());
        assert!(!sound.playing().mixer.is_playing(handle));
        state.mode = sim::Mode::Stand;
        state.car_mode = false;
        tick(&state);
        assert!(sound.playing().collision_pending.is_empty());
        assert!(sound.playing().collision_history.clear().is_empty());
    }

    #[test]
    fn a_footstep_requires_its_named_foot_to_probe_ground_and_consumes_the_cue() {
        let (sound, tuning, data) = offline();
        let arena = crate::world::layout(&tuning);
        let mut world = vec![bevy::math::Affine3A::IDENTITY; data.robot.nodes.len()];
        let foot = &data.robot.footsteps[0];
        sound.playing().pending_steps.push(foot.side);
        sound.footsteps(&world, &data.robot.footsteps, &arena);
        assert_eq!(sound.playing().mixer.playing(), 1);
        sound.footsteps(&world, &data.robot.footsteps, &arena);
        assert_eq!(sound.playing().mixer.playing(), 1, "the same cue is consumed once");
        world[foot.node].translation.z = 100.0;
        sound.playing().pending_steps.push(foot.side);
        sound.footsteps(&world, &data.robot.footsteps, &arena);
        assert_eq!(sound.playing().mixer.playing(), 1, "a midair probe must not invent a ground sound");
    }

    #[test]
    fn retrigger_replaces_the_components_handle_without_stopping_an_unrelated_cue() {
        let (sound, _, _) = offline();
        let id = tf2_core::sound::event_id("WPN_BUMBLEBEE_SPEC_FIRE");
        let mut definition = tf2_core::formats::scene::SoundPlayerDef { node: 0, script: 1, id, wait_for_trigger: true, once: false, retrigger: true };
        sound.trigger_event(false, 0, &definition);
        let first = sound.playing().triggered[&(false, 0)];
        let other = sound.playing().play(&sound.library, tf2_core::sound::event_id("UI_LOCK_ON")).unwrap();
        sound.trigger_event(false, 0, &definition);
        let next = sound.playing().triggered[&(false, 0)];
        definition.retrigger = false;
        sound.trigger_event(false, 0, &definition);
        let playing = sound.playing();
        assert_eq!(playing.triggered[&(false, 0)], next, "turbo's allowRetrigger=false retains its active event");
        assert_ne!(first, next);
        assert!(!playing.mixer.is_playing(first));
        assert!(playing.mixer.is_playing(next) && playing.mixer.is_playing(other));
    }

    #[test]
    fn camaro_boost_sound_follows_fx_messages_and_stops_on_mode_exit() {
        let (sound, tuning, data) = offline();
        sound.playing().chatter = None;
        let mut state = State::new(&tuning);
        state.mode = sim::Mode::Drive;
        state.car_mode = true;
        state.turbo_fx_on = true;
        let tick = |state: &State| sound.tick(state, &tuning, Vec3::Y * -10.0, Vec3::Y, sim::GAME_UPDATE);
        let index = data.vehicle.as_ref().unwrap().sound_players.iter().position(|p| p.script == tf2_core::fxstate::TURBO).unwrap();
        tick(&state);
        let handle = sound.playing().triggered[&(true, index)];
        assert!(sound.playing().mixer.is_playing(handle));
        tick(&state);
        assert_eq!(sound.playing().triggered[&(true, index)], handle);
        state.turbo_fx_on = false;
        tick(&state);
        assert!(!sound.playing().mixer.is_playing(handle));
        state.turbo_fx_on = true;
        tick(&state);
        let next = sound.playing().triggered[&(true, index)];
        assert_ne!(handle, next);
        state.mode = sim::Mode::Unfold;
        tick(&state);
        assert!(!sound.playing().mixer.is_playing(next));
    }

    #[test]
    fn component_sound_follows_its_bone_turn_without_moving_other_forms_or_cues() {
        let (sound, _, _) = offline();
        let definition = tf2_core::formats::scene::SoundPlayerDef { node: 1,
            script: 1, id: tf2_core::sound::event_id("WPN_BUMBLEBEE_SPEC_FIRE"),
            wait_for_trigger: true, once: false, retrigger: true };
        sound.playing().trigger_definitions[0][0] = definition.clone();
        sound.trigger_event(false, 0, &definition);
        let handle = sound.playing().triggered[&(false, 0)];
        let other = sound.playing().play(&sound.library, definition.id).unwrap();
        let root = bevy::math::Affine3A::from_rotation_translation(
            Quat::from_rotation_z(std::f32::consts::FRAC_PI_2), Vec3::new(10.0, 20.0, 3.0));
        let bone = root * bevy::math::Affine3A::from_translation(Vec3::new(2.0, 0.0, 1.0));
        sound.component_sources(false, &[root, bone]);
        let point = Vec3::new(10.0, 22.0, 4.0);
        {
            let playing = sound.playing();
            let source = &playing.sources.iter().find(|(h, _)| *h == handle).unwrap().1;
            assert!(matches!(source, Source::Moving(at, _) if at.distance(point) < 1e-5));
            assert!(matches!(playing.sources.iter().find(|(h, _)| *h == other).unwrap().1, Source::Body(_)),
                "an unrelated animation cue must keep its own source");
        }
        // Hidden/other-form node arrays cannot move this robot component.
        sound.component_sources(true, &[bevy::math::Affine3A::from_translation(Vec3::splat(300.0)); 2]);
        // Missing nodes cannot relocate a valid source to the character centre.
        sound.component_sources(false, &[root]);
        let playing = sound.playing();
        assert!(matches!(&playing.sources.iter().find(|(h, _)| *h == handle).unwrap().1,
            Source::Moving(at, _) if at.distance(point) < 1e-5));
    }

    #[test]
    fn missiles_keep_independent_flight_handles_and_explode_once_at_their_endpoint() {
        let (sound, _, data) = offline();
        let weapon = data.weapons.iter().find(|w| w.kind == tf2_core::weapons::Kind::Launcher).unwrap().name;
        let first_point = Vec3::new(1.0, 5.0, 3.0);
        let second_point = Vec3::new(-4.0, 10.0, 3.0);
        sound.projectile_start(1, weapon, first_point);
        sound.projectile_start(2, weapon, second_point);
        let first = sound.playing().projectiles[&1][0];
        let second = sound.playing().projectiles[&2][0];
        let endpoint = Vec3::new(20.0, 30.0, 3.0);
        sound.projectile_move(1, endpoint, 80.0);
        {
            let playing = sound.playing();
            assert!(matches!(playing.sources.iter().find(|(h, _)| *h == first).unwrap().1,
                Source::Moving(point, speed) if point == endpoint && speed == 80.0));
            assert!(matches!(playing.sources.iter().find(|(h, _)| *h == second).unwrap().1,
                Source::Fixed(point) if point == second_point));
        }
        sound.projectile_end(1, weapon, endpoint);
        let count = sound.playing().mixer.playing();
        assert_eq!(count, 3, "one remaining flight loop, explosion, and the authored stop fade of the ended loop");
        sound.projectile_end(1, weapon, endpoint);
        {
            let playing = sound.playing();
            assert_eq!(playing.mixer.playing(), count, "ending one missile again must not repeat the explosion");
            assert!(!playing.mixer.is_playing(first) && playing.mixer.is_playing(second));
            assert!(!playing.projectiles.contains_key(&1));
            assert!(playing.sources.iter().any(|(h, s)| *h != second && matches!(s, Source::Fixed(point) if *point == endpoint)));
        }
        sound.silence();
        assert!(sound.playing().projectiles.is_empty());
    }

    #[test]
    fn spawned_sound_uses_the_animated_bone_and_consumes_its_trigger() {
        let (sound, _, data) = offline();
        let (node, script, ids) = &data.robot.spawn_sound_players[0];
        let point = Vec3::new(3.0, 6.0, 2.0);
        let mut world = vec![bevy::math::Affine3A::IDENTITY; data.robot.nodes.len()];
        world[*node].translation = point.into();
        sound.playing().pending_spawns.push(*script);
        sound.spawn_cues(&world, &data.robot.spawn_sound_players);
        assert_eq!(sound.playing().mixer.playing(), ids.len());
        assert!(sound.playing().sources.iter().all(|(_, s)| matches!(s, Source::Fixed(at) if *at == point)));
        sound.spawn_cues(&world, &data.robot.spawn_sound_players);
        assert_eq!(sound.playing().mixer.playing(), ids.len(), "a spawner pulse plays once");
    }

    #[test]
    fn climbing_sound_requires_the_limbs_wall_probe_and_consumes_the_cue() {
        let (sound, _, data) = offline();
        let mut world = vec![bevy::math::Affine3A::IDENTITY; data.robot.nodes.len()];
        let side = tf2_core::formats::anim::CLIMB_SIDES[0];
        let node = data.robot.climb_steps.iter().find(|(_, s)| *s == side).unwrap().0;
        world[node].translation = Vec3::new(0.0, 0.0, 3.0).into();
        let arena = sim::Arena { boxes: vec![sim::Box3 { min: Vec3::new(-2.0, 1.0, 0.0), max: Vec3::new(2.0, 2.0, 6.0) }], ..Default::default() };
        sound.playing().pending_climb_steps.push(0);
        sound.climb_steps(&world, &data.robot.climb_steps, Vec3::Y, &arena);
        assert_eq!(sound.playing().mixer.playing(), 1);
        sound.climb_steps(&world, &data.robot.climb_steps, Vec3::Y, &arena);
        assert_eq!(sound.playing().mixer.playing(), 1, "a step cue is consumed once");
        world[node].translation.z = 100.0;
        sound.playing().pending_climb_steps.push(0);
        sound.climb_steps(&world, &data.robot.climb_steps, Vec3::Y, &arena);
        assert_eq!(sound.playing().mixer.playing(), 1, "without a wall contact no material cue plays");
    }
}
