//! Sound: the game's event files and banks, and a mixer that plays events the way the data
//! describes them. No engine or audio device here; `Mixer::render` fills a buffer of floats.
//!
//! Tags as in `sim.rs`: `[data]` comes straight out of the event files, `[assumed]` is a
//! reading of the data that has not been checked against the game's player, `[stand-in]` is
//! ours. Remaining fidelity debts: FMOD's full 3D renderer and reverb.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::formats::fev::{Definition, Envelope, Event, Fev, Wave};
use crate::formats::fsb::{Bank, Sample};
use crate::formats::hash::crc32;
use crate::formats::lxb::{DataFile, Node};
use crate::formats::pack::{self, Pack};

/// The id the game's data uses for an event: the CRC-32 of its name.
pub fn event_id(name: &str) -> u32 {
    crc32(name.as_bytes())
}

struct File {
    dir: PathBuf,
    fev: Fev,
    events: Vec<Arc<Event>>,
}

type SampleKey = (usize, usize, usize);

/// Event files and their banks. Samples are decoded when first asked for and kept.
pub struct Library {
    files: Vec<File>,
    /// Event id to (file, event). The first file to define an event keeps it.
    index: HashMap<u32, (usize, usize)>,
    banks: Mutex<HashMap<PathBuf, Option<Arc<Bank>>>>,
    samples: Mutex<HashMap<SampleKey, Option<Arc<Sample>>>>,
}

impl Library {
    /// Reads the given event files; their banks are looked for beside each. Returns what
    /// could not be read as text for the log.
    pub fn load(paths: &[PathBuf]) -> (Self, Vec<String>) {
        let mut problems = Vec::new();
        let mut files = Vec::new();
        let mut index = HashMap::new();
        for path in paths {
            let parsed = std::fs::read(path).map_err(|e| e.to_string()).and_then(|data| Fev::parse(&data));
            match parsed {
                Ok(fev) => {
                    for (i, event) in fev.events.iter().enumerate() {
                        index.entry(event.id).or_insert((files.len(), i));
                    }
                    let events = fev.events.iter().cloned().map(Arc::new).collect();
                    files.push(File { dir: path.parent().unwrap_or(Path::new(".")).to_owned(), fev, events });
                }
                Err(e) => problems.push(format!("{}: {e}", path.display())),
            }
        }
        (Self { files, index, banks: Mutex::default(), samples: Mutex::default() }, problems)
    }

    pub fn event_count(&self) -> usize {
        self.index.len()
    }

    /// Bind programmer sounds to FSB samples named by the chatter table. The original
    /// registry keys are CRCs of uppercase sample stems (verified against his eventIDs).
    /// The player's programmer event supplies category, mode, volume and fade metadata.
    pub fn add_programmer_bank(&mut self, path: &Path, template: u32, wanted: &[u32]) -> Result<usize, String> {
        let (template_file, template) = self.event(template).ok_or("programmer event template missing")?;
        let bank = Bank::open(path)?;
        let name = path.file_stem().and_then(|s| s.to_str()).ok_or("bank has no name")?.to_owned();
        let mut fev = Fev { project: "chatter bindings".into(), banks: vec![name.clone()], events: Vec::new(),
            definitions: Vec::new(), categories: self.files[template_file].fev.categories.clone() };
        for (index, sample) in bank.entries.iter().enumerate() {
            let stem = sample.name.rsplit_once('.').map_or(sample.name.as_str(), |(stem, _)| stem);
            let id = event_id(&stem.to_ascii_uppercase());
            if !wanted.contains(&id) || self.index.contains_key(&id) { continue; }
            let mut event = (*template).clone();
            let mut sound = event.layers.first().and_then(|l| l.sounds.first()).cloned().ok_or("programmer event has no sound")?;
            sound.definition = fev.definitions.len();
            sound.one_shot = true;
            event.id = id;
            event.name = stem.to_owned();
            event.layers.truncate(1);
            event.layers[0].sounds = vec![sound];
            fev.definitions.push(Definition { name: stem.to_owned(), play_mode: 5,
                volume: 1.0, volume_floor: 1.0, pitch: 0.0,
                pitch_randomisation: 0.0, waves: vec![Wave { weight: 1, sample: Some((name.clone(), index)) }] });
            fev.events.push(event);
        }
        let file = self.files.len();
        let count = fev.events.len();
        for (index, event) in fev.events.iter().enumerate() { self.index.insert(event.id, (file, index)); }
        let events = fev.events.iter().cloned().map(Arc::new).collect();
        self.files.push(File { dir: path.parent().unwrap_or(Path::new(".")).to_owned(), fev, events });
        Ok(count)
    }

    /// Every event's name.
    pub fn names(&self) -> Vec<&str> {
        self.index.values().map(|&(file, event)| self.files[file].events[event].name.as_str()).collect()
    }

    pub fn has(&self, id: u32) -> bool {
        self.index.contains_key(&id)
    }

    pub fn name(&self, id: u32) -> Option<&str> {
        self.index.get(&id).map(|&(file, event)| self.files[file].events[event].name.as_str())
    }

    /// An event as the file has it, as text, for looking at.
    pub fn describe(&self, id: u32) -> Option<String> {
        self.event(id).map(|(_, event)| format!("{event:#?}"))
    }

    fn event(&self, id: u32) -> Option<(usize, Arc<Event>)> {
        self.index.get(&id).map(|&(file, event)| (file, self.files[file].events[event].clone()))
    }

    fn bank(&self, file: usize, name: &str) -> Option<Arc<Bank>> {
        let path = self.files[file].dir.join(format!("{name}.fsb"));
        let mut banks = self.banks.lock().ok()?;
        banks.entry(path.clone()).or_insert_with(|| Bank::open(&path).ok().map(Arc::new)).clone()
    }

    /// One waveform of a sound definition, decoded on first use. `None` for silence, or for
    /// a sample that does not decode (which then stays silent).
    fn wave(&self, file: usize, definition: usize, wave: usize, decode: bool) -> Option<Arc<Sample>> {
        let key = (file, definition, wave);
        if let Some(known) = self.samples.lock().ok()?.get(&key) {
            return known.clone();
        }
        if !decode {
            return None;
        }
        let (bank, index) = self.files[file].fev.definitions.get(definition)?.waves.get(wave)?.sample.clone()?;
        // Decoded outside the lock: it takes a few milliseconds per second of sound.
        let sample = self.bank(file, &bank).and_then(|bank| bank.sample(index).ok()).map(Arc::new);
        self.samples.lock().ok()?.insert(key, sample.clone());
        sample
    }

    /// Decodes everything an event can play, so that playing it later does not have to.
    /// Returns how many waveforms it has.
    pub fn prepare(&self, id: u32) -> usize {
        let Some((file, event)) = self.event(id) else { return 0 };
        let mut count = 0;
        for sound in event.layers.iter().flat_map(|layer| &layer.sounds) {
            let waves = self.files[file].fev.definitions[sound.definition].waves.len();
            for wave in 0..waves {
                count += self.wave(file, sound.definition, wave, true).is_some() as usize;
            }
        }
        count
    }
}

/// Names one playing event, for stopping it or setting its parameters.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Handle(u64);

struct Voice {
    sample: Arc<Sample>,
    /// Position in the sample, in its own frames.
    at: f64,
    /// Gain at the end of the last block; the next block ramps from it.
    gain: f32,
}

/// One sound of a playing event.
struct Part {
    layer: usize,
    sound: usize,
    voice: Option<Voice>,
    started: bool,
    /// This play's share of the randomised volume and pitch.
    gain: f32,
    pitch: f32,
}

struct Instance {
    handle: Handle,
    file: usize,
    event: Arc<Event>,
    parameters: Vec<f32>,
    /// What each parameter was last set to; one with a seek speed is still on its way.
    wanted: Vec<f32>,
    parts: Vec<Part>,
    volume: f32,
    /// [game: 004bf110] Event-level pitch is shared by every sound in the event.
    pitch: f32,
    elapsed: f32,
    stopped_at: Option<(f32, f32)>,
    /// Each layer's two cascaded low-pass stages, left then right. [game: 00469370]
    filters: Vec<[f32; 4]>,
    /// 1 while playing; runs down to 0 once stopped.
    fade: f32,
    stopping: bool,
    fresh: bool,
    spatial: Option<(f32,f32)>,
}

pub struct Mixer {
    rate: f32,
    instances: Vec<Instance>,
    next: u64,
    random: u32,
    /// How far the listener is from the sounds, in metres. Everything played is the
    /// character's own, so one distance serves.
    pub distance: f32,
    /// Where the sounds are across the listener: -1 hard left, 0 ahead or behind, 1 hard
    /// right. One value for the same reason as `distance`. [stand-in] for the game's 3D
    /// sound: two speakers, no front and back.
    pub side: f32,
    pub master: f32,
    category_volumes: HashMap<String, f32>,
    /// One layer's sounds, before its effect.
    gather: Vec<f32>,
    choices: HashMap<(usize, usize, u32, usize, usize), WaveChoice>,
}

#[derive(Default)]
struct WaveChoice {
    last: Option<usize>,
    shuffle: Vec<usize>,
}

impl WaveChoice {
    /// [game: 0118aa60] Random-no-repeat advances to the next index if the
    /// weighted pick repeats, rather than drawing again or reweighting the list.
    fn pick(&mut self, definition: &Definition, random: &mut u32) -> usize {
        let count = definition.waves.len();
        if count == 0 { return 0; }
        let draw = |random: &mut u32| {
            *random ^= *random << 13;
            *random ^= *random >> 17;
            *random ^= *random << 5;
            *random
        };
        let chosen = match definition.play_mode {
            0 | 3 => self.last.map_or(0, |last| (last + 1) % count),
            1 | 2 => {
                let total: u64 = definition.waves.iter().map(|w| w.weight as u64).sum();
                if total == 0 { self.last = Some(0); return 0; }
                let mut pick = draw(random) as u64 % total;
                let mut chosen = 0;
                for (i, wave) in definition.waves.iter().enumerate() {
                    chosen = i;
                    if pick < wave.weight as u64 { break; }
                    pick = pick.saturating_sub(wave.weight as u64);
                }
                if definition.play_mode == 2 && self.last == Some(chosen) && count > 1 {
                    chosen = (chosen + 1) % count;
                }
                chosen
            }
            4 | 6 => {
                if self.shuffle.is_empty() {
                    self.shuffle.extend(0..count);
                    for end in (1..count).rev() {
                        let chosen = draw(random) as usize % (end + 1);
                        self.shuffle.swap(end, chosen);
                    }
                    // [game: 004deeb0] Avoid repeating the last entry across bags.
                    if count > 1 && self.shuffle.last().copied() == self.last {
                        let chosen = draw(random) as usize % (count - 1);
                        self.shuffle.swap(count - 1, chosen);
                    }
                }
                self.shuffle.pop().unwrap()
            }
            _ => 0,
        };
        self.last = Some(chosen);
        chosen
    }
}

/// The effect the engine's layer carries, driven by `load`.
const LOWPASS: &str = "FMOD Lowpass Simple";

/// Where a layer's low-pass effect cuts, in hertz, if it has one.
/// [game] 004cc7c0 maps this DSP's envelope logarithmically between the descriptor's
/// 10 and 22000 Hz (00469150, 00bf8878).
fn cutoff(layer: &crate::formats::fev::Layer, share: &impl Fn(usize) -> f32) -> Option<f32> {
    let envelope = layer.envelopes.iter().find(|e| e.effect == LOWPASS)?;
    Some(10.0 * 2200f32.powf(envelope.at(share(envelope.parameter)).clamp(0.0, 1.0)))
}

/// [game: 004c5230] Pitch envelope centre .5 preserves pitch; its ends are
/// four octaves down/up. Disabled envelopes (flag bit0) are not evaluated.
fn layer_pitch(layer: &crate::formats::fev::Layer, share: &impl Fn(usize) -> f32) -> f32 {
    layer.envelopes.iter().filter(|e| e.effect.is_empty() && e.flags == Envelope::PITCH)
        .map(|e| 2f32.powf(e.at(share(e.parameter)) * 8.0 - 4.0)).product()
}

/// [game: 004c5230/00415e20] The 0x404 envelope sets the channel's 3D level.
/// [assumed] Blend the existing stereo projection/attenuation with unplaced gain;
/// FMOD's full speaker matrix and Doppler remain a [stand-in] debt. A synchronized
/// original output capture with listener movement would settle the gain law.
fn layer_sides(layer: &crate::formats::fev::Layer, share: &impl Fn(usize) -> f32,
    placed: bool, rolloff: f32, sides: (f32, f32)) -> (f32, f32) {
    if !placed { return (1.0, 1.0); }
    let blend: f32 = layer.envelopes.iter()
        .filter(|e| e.effect.is_empty() && e.flags == Envelope::SPATIAL_BLEND)
        .map(|e| e.at(share(e.parameter))).product();
    let blend = blend.clamp(0.0, 1.0);
    (1.0 + blend * (rolloff * sides.0 - 1.0), 1.0 + blend * (rolloff * sides.1 - 1.0))
}

/// [game: 004692a0] FMOD Lowpass Simple's coefficient, including its upper-frequency
/// transition and explicit 22000 Hz bypass.
fn lowpass_coefficient(hertz: f32, rate: f32) -> f32 {
    if hertz >= 22000.0 {
        1.0
    } else if hertz > rate / std::f32::consts::PI {
        (hertz - rate / std::f32::consts::PI) / ((22000.0 - rate / std::f32::consts::PI) * 3.0) + 2.0 / 3.0
    } else {
        let period = 1.0 / rate;
        period / (1.0 / (hertz * std::f32::consts::TAU) + period)
    }
}

/// Adds a layer through the original two cascaded stages (00469370), then its stereo
/// placement. Bypass copies the signal without advancing filter history, as in the game.
/// [assumed] Omit FMOD's alternating denormal-prevention bias: it is not an audible
/// component. Exact floating-point/DSP output comparison would settle this adapter detail.
fn settle(out: &mut [f32], gather: &mut [f32], filter: &mut [f32; 4], cutoff: Option<f32>, rate: f32, sides: (f32, f32)) {
    let pass = cutoff.map_or(1.0, |hertz| lowpass_coefficient(hertz, rate));
    for (to, from) in out.chunks_exact_mut(2).zip(gather.chunks_exact(2)) {
        for channel in 0..2 {
            let value = if pass == 1.0 {
                from[channel]
            } else {
                let first = channel * 2;
                filter[first] = (1.0 - pass) * filter[first] + pass * from[channel];
                filter[first + 1] = (1.0 - pass) * filter[first + 1] + pass * filter[first];
                filter[first + 1]
            };
            to[channel] += value * if channel == 0 { sides.0 } else { sides.1 };
        }
    }
}

/// [game: 004c5230] Fade widths are fractions of the sound's span, with fade-in
/// taking precedence if both fades overlap. Curve0 is the cubic Bezier default;
/// 1 is linear, 2 equal-power, and3..7 use the executable's exponential curves.
fn sound_span(sound: &crate::formats::fev::Sound, at: f32) -> f32 {
    let incoming = sound.length * sound.fade_in;
    let outgoing = sound.length * sound.fade_out;
    let curve = |progress: f32, fading_out: bool| {
        let t = progress.clamp(0.0, 1.0);
        match sound.fade_curve {
            1 => if fading_out { 1.0 - t } else { t },
            2 => (if fading_out { 1.0 - t } else { t }).sqrt(),
            3..=7 => {
                let base: f32 = match (sound.fade_curve, fading_out) {
                    (3, _) => 361.0,
                    (4, _) => 5.82843017578125,
                    (5, _) => 2.151270031929016,
                    (6, false) | (7, true) => 1.4514999985694885,
                    _ => 31.9960994720459,
                };
                1.0 - (base.powf(if fading_out { t } else { 1.0 - t }) - 1.0) / (base - 1.0)
            }
            _ => {
                let smooth = t * t * (3.0 - 2.0 * t);
                if fading_out { 1.0 - smooth } else { smooth }
            }
        }
    };
    if incoming > 0.0 && at < sound.start + incoming {
        curve((at - sound.start) / incoming, false)
    } else if outgoing > 0.0 && at > sound.start + sound.length - outgoing {
        curve((at - (sound.start + sound.length - outgoing)) / outgoing, true)
    } else {
        1.0
    }
}

/// [game: 004bf110/004c1330/004df040] Four-octave units, with a full symmetric
/// radius (2*random-1); the earlier adapter halved the authored random spread.
const PITCH_UNIT_OCTAVES: f32 = 4.0;

fn pitch_multiplier(pitch: f32, radius: f32, random: f32) -> f32 {
    2f32.powf((pitch + radius * (2.0 * random - 1.0)) * PITCH_UNIT_OCTAVES)
}

impl Mixer {
    /// `rate` is the output's frames per second.
    pub fn new(rate: u32) -> Self {
        Self { rate: rate as f32, instances: Vec::new(), next: 1, random: 0x2545_f491, distance: 0.0, side: 0.0, master: 1.0, category_volumes: HashMap::new(), gather: Vec::new(), choices: HashMap::new() }
    }

    /// Category options multiply the authored gain of the category and its parents.
    pub fn set_category_volume(&mut self, category: &str, volume: f32) {
        self.category_volumes.insert(category.to_owned(), volume.max(0.0));
    }

    /// Uniform in 0..1.
    fn random(&mut self) -> f32 {
        self.random ^= self.random << 13;
        self.random ^= self.random >> 17;
        self.random ^= self.random << 5;
        (self.random >> 8) as f32 / (1u32 << 24) as f32
    }

    pub fn playing(&self) -> usize {
        self.instances.len()
    }

    pub fn is_playing(&self, handle: Handle) -> bool {
        self.instances.iter().any(|i| i.handle == handle && !i.stopping)
    }

    /// Starts an event. Its samples should have been through `Library::prepare`; one that
    /// has not is decoded here, which is slow enough to be heard as a hitch.
    pub fn play(&mut self, library: &Library, id: u32, volume: f32) -> Option<Handle> {
        let (file, event) = library.event(id)?;
        // [game: 004bf110/004c1330] Random event gain/pitch are shared by its
        // layers. Definitions have their own independent random choices.
        let volume = volume * (1.0 - event.volume_randomisation.clamp(0.0, 1.0) * self.random());
        let pitch = pitch_multiplier(event.pitch, event.pitch_randomisation, self.random());
        // [data] an event allows only so many at once; the oldest makes room.
        if event.max_playbacks > 0 {
            let live = self.instances.iter().filter(|i| i.event.id == id && !i.stopping).count();
            if live >= event.max_playbacks as usize
                && let Some(oldest) = self.instances.iter_mut().find(|i| i.event.id == id && !i.stopping)
            {
                oldest.stopping = true;
            }
        }
        let mut parts = Vec::new();
        for (layer_index, layer) in event.layers.iter().enumerate() {
            for (sound_index, sound) in layer.sounds.iter().enumerate() {
                let definition = &library.files[file].fev.definitions[sound.definition];
                // [data] volume randomisation takes up to that share off; a definition
                // gives the quietest it may be.
                let floor = definition.volume_floor.clamp(0.0, 1.0);
                let gain = (floor + (1.0 - floor) * self.random()) * definition.volume;
                let pitch = pitch_multiplier(definition.pitch, definition.pitch_randomisation, self.random());
                parts.push(Part { layer: layer_index, sound: sound_index, voice: None, started: false, gain, pitch });
            }
        }
        let handle = Handle(self.next);
        self.next += 1;
        let parameters: Vec<f32> = event.parameters.iter().map(|p| p.min).collect();
        let filters = vec![[0.0; 4]; event.layers.len()];
        let wanted = parameters.clone();
        let fade = if event.fade_in_ms > 0 { 0.0 } else { 1.0 };
        self.instances.push(Instance { handle, file, event, parameters, wanted, parts, volume, pitch,
            elapsed: 0.0, stopped_at: None, filters, fade, stopping: false, fresh: true, spatial:None });
        Some(handle)
    }

    /// Lets an event fade out over its own fade time.
    pub fn stop(&mut self, handle: Handle) {
        if let Some(instance) = self.instances.iter_mut().find(|i| i.handle == handle) {
            instance.stopping = true;
        }
    }

    /// Independent source placement; the stereo projection remains a stand-in for FMOD 3D.
    pub fn set_spatial(&mut self, handle: Handle, distance: f32, side: f32) {
        if let Some(instance)=self.instances.iter_mut().find(|i|i.handle==handle) {
            instance.spatial=Some((distance.max(0.0),side.clamp(-1.0,1.0)));
        }
    }

    /// Stops every playing copy of an event.
    pub fn stop_event(&mut self, id: u32) {
        for instance in self.instances.iter_mut().filter(|i| i.event.id == id) {
            instance.stopping = true;
        }
    }

    pub fn stop_all(&mut self) {
        for instance in &mut self.instances {
            instance.stopping = true;
        }
    }

    pub fn set_parameter(&mut self, handle: Handle, name: &str, value: f32) {
        let Some(instance) = self.instances.iter_mut().find(|i| i.handle == handle) else { return };
        if let Some(index) = instance.event.parameters.iter().position(|p| p.name == name) {
            let p = &instance.event.parameters[index];
            let value = value.clamp(p.min.min(p.max), p.max.max(p.min));
            instance.wanted[index] = value;
            // [data] a parameter with a seek speed glides to what it is set to (in
            // `render`); one without, or an event not yet heard, is there at once.
            if p.seek_speed <= 0.0 || instance.fresh {
                instance.parameters[index] = value;
            }
        }
    }

    /// Original callers can name parameters only by their case-sensitive CRC.
    pub fn set_parameter_hash(&mut self, handle: Handle, hash: u32, value: f32) {
        let name = self.instances.iter().find(|i| i.handle == handle)
            .and_then(|i| i.event.parameters.iter().find(|p| event_id(&p.name) == hash))
            .map(|p| p.name.clone());
        if let Some(name) = name { self.set_parameter(handle, &name, value); }
    }

    pub fn set_volume(&mut self, handle: Handle, volume: f32) {
        if let Some(instance) = self.instances.iter_mut().find(|i| i.handle == handle) {
            instance.volume = volume;
        }
    }

    /// Adds the next stretch of sound to `out`: stereo, interleaved, `out.len() / 2` frames.
    /// `out` is overwritten.
    pub fn render(&mut self, library: &Library, out: &mut [f32]) {
        out.fill(0.0);
        let frames = out.len() / 2;
        if frames == 0 {
            return;
        }
        let seconds = frames as f32 / self.rate;
        let (distance, side, rate, master) = (self.distance, self.side, self.rate, self.master);
        let mut gather = std::mem::take(&mut self.gather);
        gather.resize(out.len(), 0.0);
        for instance in &mut self.instances {
            let (distance,side)=instance.spatial.unwrap_or((distance,side));
            let angle=(side+1.0)*std::f32::consts::FRAC_PI_4;
            let sides=(angle.cos()*std::f32::consts::SQRT_2,angle.sin()*std::f32::consts::SQRT_2);
            let event = instance.event.clone();
            for ((value, &wanted), parameter) in instance.parameters.iter_mut().zip(&instance.wanted).zip(&event.parameters) {
                if parameter.name == "(distance)" {
                    *value = distance.clamp(parameter.min, parameter.max);
                } else if parameter.seek_speed > 0.0 && *value != wanted {
                    // [game: 004ce900/004cd990] FMOD seeks its normalized value by
                    // seek_speed * elapsed_ms /1000, hence range units here.
                    let step = parameter.seek_speed * (parameter.max - parameter.min).abs() * seconds;
                    *value += (wanted - *value).clamp(-step, step);
                } else if parameter.velocity != 0.0 && !instance.fresh {
                    // [data] a parameter with a velocity runs by itself.
                    *value = (*value + parameter.velocity * seconds).clamp(parameter.min, parameter.max);
                }
            }
            let share = |index: usize| {
                let p = &event.parameters[index];
                if p.max > p.min { (instance.parameters[index] - p.min) / (p.max - p.min) } else { 0.0 }
            };
            // [game: 004c0ca0] Event fades are linear elapsed/fade-time ramps.
            // [assumed] Apply them on the device sample clock, rather than Lux's
            // EventSystem update clock. This preserves authored durations across
            // different callback sizes; original DSP timestamps settle the offset.
            if instance.stopping && instance.stopped_at.is_none() {
                instance.stopped_at = Some((instance.elapsed, instance.fade));
            }
            let stopped = instance.stopped_at;
            let elapsed = instance.elapsed;
            let fade_at = |time: f32| match stopped {
                Some((start, gain)) if event.fade_out_ms > 0 =>
                    gain * (1.0 - (time - start) * 1000.0 / event.fade_out_ms as f32).clamp(0.0, 1.0),
                Some(_) => 0.0,
                None if event.fade_in_ms > 0 => (time * 1000.0 / event.fade_in_ms as f32).clamp(0.0, 1.0),
                None => 1.0,
            };
            instance.elapsed += seconds;
            instance.fade = fade_at(instance.elapsed);
            // [data] the event's mode bits are FMOD's: 0x10 a sound placed in the world (0x08
            // one that is not: the interface's), 0x100000 the inverse roll-off, which is
            // full volume inside the minimum distance and falls with the distance outside.
            let placed = event.mode & 0x10 != 0;
            let inverse = placed && event.mode & 0x0010_0000 != 0;
            let rolloff = if inverse && event.min_distance > 0.0 && distance > event.min_distance { event.min_distance / distance } else { 1.0 };
            let parent_of = |parent: &str| parent.is_empty() || event.category == parent
                || event.category.strip_prefix(parent).is_some_and(|tail| tail.starts_with('/'));
            let authored: f32 = library.files[instance.file].fev.categories.iter()
                .filter(|c| parent_of(&c.path)).map(|c| c.volume).product();
            let options: f32 = self.category_volumes.iter().filter(|(c, _)| parent_of(c)).map(|(_, v)| *v).product();
            let level = event.volume * instance.volume * master * authored * options;
            // A layer's sounds are gathered apart, so that its effect works on them alone.
            let mut gathered: Option<usize> = None;
            for part in &mut instance.parts {
                if gathered != Some(part.layer) {
                    if let Some(done) = gathered {
                        let cutoff = cutoff(&event.layers[done], &share);
                        let placement = layer_sides(&event.layers[done], &share, placed, rolloff, sides);
                        settle(out, &mut gather, &mut instance.filters[done], cutoff, rate, placement);
                    }
                    gather.fill(0.0);
                    gathered = Some(part.layer);
                }
                let layer = &event.layers[part.layer];
                let sound = &layer.sounds[part.sound];
                let x = layer.parameter.filter(|&p| p < event.parameters.len()).map(share);
                // [data] a sound occupies a stretch of its layer's parameter.
                let at = x.unwrap_or(sound.start);
                let inside = at >= sound.start - 1e-4 && at <= sound.start + sound.length + 1e-4;
                let span = sound_span(sound, at);
                let shaped: f32 = layer
                    .envelopes
                    .iter()
                    .filter(|e| e.effect.is_empty() && e.flags == Envelope::VOLUME && e.parameter < event.parameters.len())
                    .map(|e| e.at(share(e.parameter)))
                    .product();
                let target = if inside { level * sound.volume * part.gain * span * shaped } else { 0.0 };
                if inside && !part.started && !instance.stopping {
                    part.started = true;
                    let definition = &library.files[instance.file].fev.definitions[sound.definition];
                    // [game: 0118aa60] No-repeat/shuffle mode6 share definition
                    // history. [assumed] The remaining modes reuse one history
                    // per authored sound in place of FMOD's EventSound pool.
                    let scope = if matches!(definition.play_mode, 2 | 6) { (0, 0, 0) }
                        else { (event.id, part.layer, part.sound) };
                    let key = (instance.file, sound.definition, scope.0, scope.1, scope.2);
                    let chosen = self.choices.entry(key).or_default().pick(definition, &mut self.random);
                    part.voice = library.wave(instance.file, sound.definition, chosen, true).map(|sample| Voice {
                        sample,
                        at: 0.0,
                        gain: if sound.one_shot { target } else { 0.0 },
                    });
                }
                let Some(voice) = &mut part.voice else { continue };
                // [game: 004c5230] Blend normalized parameter/reference with the
                // authored minimum multiplier, then add the sound's fine tune.
                let mut pitch = part.pitch;
                if let (true, Some(x)) = (sound.auto_pitch, x) {
                    let reference = if sound.auto_pitch_reference == 0.0 { 1e-8 } else { sound.auto_pitch_reference };
                    pitch *= sound.auto_pitch_at_min + (1.0 - sound.auto_pitch_at_min) * x / reference;
                }
                pitch += sound.fine_tune;
                pitch *= instance.pitch * layer_pitch(layer, &share);
                let step = voice.sample.rate as f64 / rate as f64 * pitch as f64;
                let (channels, total) = (voice.sample.channels.max(1), voice.sample.frames());
                let data = &voice.sample.data;
                let (from, ramp) = (voice.gain, (target - voice.gain) / frames as f32);
                let mut ended = total == 0;
                // [data] Keep every authored frame. The FSB loop endpoints, converted
                // to an exclusive end, define the period; no invented seam crossfade.
                let region = if sound.one_shot { 0..total } else { voice.sample.loop_range.clone() };
                let lap = region.len().max(1) as f64;
                if !ended && (from != 0.0 || target != 0.0) {
                    for frame in 0..frames {
                        if voice.at >= region.end as f64 {
                            if sound.one_shot {
                                ended = true;
                                break;
                            }
                            // Wrap before reading this frame; skipping it inserted a
                            // silent output frame at every audible loop boundary.
                            voice.at = region.start as f64 + (voice.at - region.end as f64) % lap;
                        }
                        let whole = voice.at as usize;
                        let t = (voice.at - whole as f64) as f32;
                        let gain = (from + ramp * frame as f32) * fade_at(elapsed + frame as f32 / rate);
                        let read = |frame: usize, channel: usize| {
                            let next = if frame + 1 < region.end { frame + 1 }
                                else if sound.one_shot { frame } else { region.start };
                            data[frame * channels + channel] * (1.0 - t) + data[next * channels + channel] * t
                        };
                        let left = read(whole, 0);
                        let right = if channels > 1 { read(whole, 1) } else { left };
                        gather[frame * 2] += left * gain;
                        gather[frame * 2 + 1] += right * gain;
                        voice.at += step;
                    }
                } else if !ended && !sound.one_shot {
                    // A silent loop keeps its place in step with the others.
                    voice.at += step * frames as f64;
                    if voice.at >= region.end as f64 {
                        voice.at = region.start as f64 + (voice.at - region.end as f64) % lap;
                    }
                }
                voice.gain = target;
                if ended {
                    part.voice = None;
                }
            }
            if let Some(done) = gathered {
                let cutoff = cutoff(&event.layers[done], &share);
                let placement = layer_sides(&event.layers[done], &share, placed, rolloff, sides);
                settle(out, &mut gather, &mut instance.filters[done], cutoff, rate, placement);
            }
            instance.fresh = false;
        }
        self.gather = gather;
        self.instances.retain(|instance| {
            if instance.stopping && instance.fade <= 0.0 {
                return false;
            }
            // Over when nothing sounds and nothing more can start: a sound not yet started
            // only will if a parameter is still running towards it.
            let sounding = instance.parts.iter().any(|p| p.voice.is_some());
            let running = instance
                .event
                .parameters
                .iter()
                .zip(&instance.parameters)
                .any(|(p, &value)| p.velocity > 0.0 && value < p.max);
            let waiting = instance.parts.iter().any(|p| !p.started) && running && !instance.stopping;
            sounding || waiting
        });
        for value in out.iter_mut() {
            *value = value.clamp(-1.0, 1.0);
        }
    }
}

/// One gear: the revolutions it runs between and its ratio. `<Name>Gears` in `bnxglobal.str`.
#[derive(Clone, Copy, Debug)]
pub struct Gear {
    pub min_rpm: f32,
    pub max_rpm: f32,
    pub ratio: f32,
}

/// Reads a character's gear table (`BumblebeeGears`) out of `bnxglobal.str`.
pub fn load_gears(game_dir: &Path, character: &str) -> Result<Vec<Gear>, String> {
    let pack = Pack::open(&game_dir.join("bnxglobal.str"))?;
    let name = format!("{character}Gears");
    for chunk in pack.of_type(pack::DATA) {
        let Ok(file) = DataFile::parse(pack.data(chunk).to_vec()) else { continue };
        let Some(table) = file.root().get(&name) else { continue };
        let number = |gear: Node, field: &str| gear.get(field).and_then(Node::float).unwrap_or(0.0);
        return Ok(table
            .items()
            .map(|gear| Gear { min_rpm: number(gear, "minRPM"), max_rpm: number(gear, "maxRPM"), ratio: number(gear, "ratio") })
            .filter(|gear| gear.ratio > 0.0 && gear.max_rpm > 0.0)
            .collect());
    }
    Err(format!("bnxglobal.str has no {name}"))
}

/// What the drive mode's wheel set shows the gearbox in one of the game's updates (the wheel
/// set's fields, `notes\driving.md`). [game]
#[derive(Clone, Copy, Debug, Default)]
pub struct GearboxInput {
    /// The body's velocity.
    pub velocity: [f32; 3],
    /// The filtered trigger, 0 to 1 (wheel set `+0x24`).
    pub trigger: f32,
    /// Stick left/right (`+0x1c`).
    pub stick_x: f32,
    /// The brake is slowing him (`+0x14` over 0).
    pub braking: bool,
    /// The slide is held (`+0x20`).
    pub skid: bool,
    pub reversing: bool,
    /// Braked to a stop and waiting to reverse (`+0x22`).
    pub stopped_by_brake: bool,
    /// Any wheel is on the ground.
    pub grounded: bool,
    /// A turbo is running: the throttle counts as full.
    pub turbo: bool,
}

/// The gearbox behind the engine event's `rpm` and `load` parameters, as the game runs it
/// once an update for a human player's car (`FUN_007a48e0`, called through `FUN_00731f80`
/// by the vehicle's sound update `FUN_007347c0`, which sends `+0x18` as `rpm` and `+0x30`
/// as `load`). [game]
///
/// The revolutions do not come from the speed: they are `trigger * revs * ratio * 600`, with
/// `revs` a number that climbs while he is not slowing down and falls while he is, and a
/// change of gear whenever that leaves the gear's range. The speed only says which of the
/// two it is.
#[derive(Clone, Debug)]
pub struct Gearbox {
    gears: Vec<Gear>,
    /// `engineTorque * 2 / (frontWheelRadius * mass)` and the same with 7 (`FUN_007a2300`).
    up_rate: f32,
    down_rate: f32,
    max_speed: f32,
    turning_velocity_delta: f32,
    pub gear: usize,
    pub rpm: f32,
    pub load: f32,
    /// `+0x58`.
    revs: f32,
    /// The revolutions when the wheels were last down (`+0x20`).
    grounded_rpm: f32,
    was_grounded: bool,
    was_reversing: bool,
    last_speed: f32,
    /// The stick, eased (`+0x54`).
    stick: f32,
    pub shifted_up: bool,
    pub shifted_down: bool,
}

impl Gearbox {
    /// 007a4fe0: rev progress within the current gear, used by suspension weight transfer. [game]
    pub fn progress(&self) -> f32 {
        let Some(current)=self.gears.get(self.gear) else {return 0.0;};
        if self.gear==0 {return self.rpm/current.max_rpm;}
        let below=self.gears[self.gear-1];
        let low=current.ratio*below.max_rpm/below.ratio;
        let span=current.max_rpm-low;
        if span>0.0 {(self.rpm-low)/span} else {0.0}
    }
    /// `mass` is the vehicle body's; the tuning values are the drive mode's.
    pub fn new(gears: Vec<Gear>, engine_torque: f32, front_wheel_radius: f32, mass: f32, max_speed: f32, turning_velocity_delta: f32) -> Self {
        let per = engine_torque / (front_wheel_radius * mass).max(1e-6);
        Self {
            gears,
            up_rate: per * 2.0,
            down_rate: per * 7.0,
            max_speed,
            turning_velocity_delta,
            gear: 0,
            rpm: 0.0,
            load: 0.5,
            revs: 0.0,
            grounded_rpm: 0.0,
            was_grounded: false,
            was_reversing: false,
            last_speed: 0.0,
            stick: 0.0,
            shifted_up: false,
            shifted_down: false,
        }
    }

    /// The same gearbox as the drive mode builds it on entry: first gear, nothing turning.
    pub fn fresh(self) -> Self {
        let (per, max_speed, delta) = (self.up_rate / 2.0, self.max_speed, self.turning_velocity_delta);
        Self { up_rate: per * 2.0, down_rate: per * 7.0, ..Self::new(self.gears, 1.0, 1.0, 1.0, max_speed, delta) }
    }

    /// One of the game's updates of `dt` seconds.
    pub fn update(&mut self, input: &GearboxInput, dt: f32) {
        if self.gears.is_empty() {
            return;
        }
        let [mut vx, mut vy, vz] = input.velocity;
        let throttle = if input.turbo { 1.0 } else { input.trigger };
        // At full throttle and not sliding, the speed it looks at is held under the top
        // speed less a share for the turn of the stick.
        if throttle > 0.9 && !input.skid {
            self.stick = if dt < 1.0 { self.stick + (input.stick_x - self.stick) * dt } else { input.stick_x };
            let limit = self.max_speed - self.stick.abs() * self.turning_velocity_delta;
            let flat = (vx * vx + vy * vy).sqrt();
            if limit < flat && flat > 1e-8 {
                (vx, vy) = (vx / flat * limit, vy / flat * limit);
            }
        }
        let speed = (vx * vx + vy * vy + vz * vz).sqrt();
        let slowing = speed - self.last_speed < -0.01;
        self.last_speed = speed;
        self.revs = self.revs.clamp(1.0, 1e9);
        let before = self.rpm;
        (self.shifted_up, self.shifted_down) = (false, false);
        let (grounded, was_grounded) = (input.grounded, std::mem::replace(&mut self.was_grounded, input.grounded));
        if grounded && !was_grounded {
            self.rpm = self.grounded_rpm;
        }
        let first = self.gears[0];
        if input.stopped_by_brake {
            (self.gear, self.rpm, self.load, self.revs) = (0, first.min_rpm, 0.5, 1.0);
            return;
        }
        let gear = self.gears[self.gear.min(self.gears.len() - 1)];
        let turning = |revs: f32| throttle * revs * gear.ratio * 600.0;
        if !input.turbo && input.braking && !input.skid && !input.reversing {
            self.revs = self.down_rate / self.revs.sqrt() * dt;
            self.rpm = turning(self.revs).max(first.min_rpm);
            self.load = 0.25;
            return;
        }
        if input.reversing {
            self.was_reversing = true;
            self.revs += self.up_rate / self.revs.sqrt() * dt;
            self.rpm = turning(self.revs).min(gear.max_rpm);
        } else if std::mem::take(&mut self.was_reversing) {
            (self.gear, self.rpm, self.revs) = (0, first.min_rpm, 1.0);
            return;
        }
        if (!slowing && !input.reversing) || !grounded {
            // In the air the engine runs to the top of its gear.
            self.rpm = if grounded {
                self.revs += self.up_rate / self.revs.sqrt() * dt;
                turning(self.revs)
            } else {
                gear.max_rpm
            };
            if self.rpm > gear.max_rpm {
                if !grounded || input.skid {
                    self.rpm = gear.max_rpm;
                } else {
                    self.shifted_up = true;
                    self.gear += 1;
                    if self.gear < self.gears.len() {
                        self.rpm = self.gears[self.gear].min_rpm;
                    } else {
                        // Out of gears: the revs fall back to where the gear before the
                        // last one ran out, and climb again.
                        self.gear = self.gears.len() - 1;
                        let below = self.gears[self.gears.len().saturating_sub(2)];
                        self.revs = below.max_rpm / (below.ratio * 600.0);
                        self.rpm = turning(self.revs);
                    }
                }
            }
        } else if !input.reversing {
            if !input.skid && self.gear > 0 {
                self.revs -= self.down_rate / self.revs.sqrt() * dt * 0.5;
            }
            self.rpm = turning(self.revs);
            if self.rpm < gear.min_rpm {
                self.shifted_down = true;
                self.rpm = match self.gear.checked_sub(1) {
                    Some(lower) => {
                        self.gear = lower;
                        self.gears[lower].max_rpm
                    }
                    None => first.min_rpm,
                };
            }
        }
        // The load is how fast the revolutions are changing, settling back to a half. A
        // division of nothing by nothing counts as 0, as the game's comparison has it.
        let moved = self.rpm / before - 1.0 + self.load;
        let load = if moved > 0.0 { moved.min(1.0) } else { 0.0 };
        self.load = (0.5 - load) * dt * 4.0 + load;
        if grounded {
            self.grounded_rpm = self.rpm;
        }
    }
}

/// What the vehicle's sound update (`FUN_007347c0`) reads of the car in one of the game's
/// updates, for the tyres and the suspension. [game]
#[derive(Clone, Copy, Debug, Default)]
pub struct TyreInput {
    /// The length of the body's velocity.
    pub speed: f32,
    /// The slide's button is held and the mode lets him slide (button 0x14, mode `+0x1f5`).
    pub drift: bool,
    /// The brake's amount, 0 to 1 (mode `+0x40`).
    pub brake: f32,
    pub reversing: bool,
    /// Braked to a stop and waiting to reverse (mode `+0x4e`).
    pub waiting_to_reverse: bool,
    /// Stick left / right.
    pub stick_x: f32,
    /// The speed the trigger allows right now (`FUN_0085f8b0`).
    pub cap: f32,
    /// Any wheel is on the ground.
    pub grounded: bool,
    /// The material of the ground under the wheel (wheel `+0x98`), 0 for none.
    pub surface: u32,
    /// The most a rear wheel's spring shortened in this update, as a share of
    /// `suspensionLength` (wheel `(+0x70 - +0x7c) / +0x80`).
    pub compression: f32,
    /// Independent original wheel contacts for effects/marks. [game]
    pub wheels: [TyreWheel; 4],
    pub per_wheel: bool,
    pub sound_wheel: usize,
    pub suspension_wheel: usize,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TyreWheel {
    pub grounded: bool,
    pub was_grounded: bool,
    pub point: glam::Vec3,
    pub surface: u32,
    pub compression: f32,
}

impl TyreInput {
    pub fn for_wheel(&self, index: usize) -> Self {
        if !self.per_wheel {return *self;}
        let w=self.wheels[index];
        Self {grounded:w.grounded,surface:w.surface,compression:w.compression,..*self}
    }
}

/// What the tyres and the suspension should sound like after an update.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TyreSounds {
    /// The surface the skid loop is for (`vehicleTireSkidFX` picks the event), if it plays.
    pub skid: Option<u32>,
    /// The same for the rolling loop (`vehicleTireRoadFX`).
    pub road: Option<u32>,
    /// A thump of the suspension, with the force the code works out for it.
    pub suspension: Option<f32>,
}

/// The parameter the code gives the suspension's force to, by the hash of this name. The
/// event's own parameter is spelt `ImpactForce` and the game's lookup compares CRC-32s of
/// the names as they are (`FUN_00594d90`), so in the game the force reaches nothing and the
/// thump always plays with the parameter where it starts. [game] The port does the same.
pub const IMPACT_FORCE: &str = "impactForce";

/// The skid test of `FUN_007347c0` (`00734a53` to `00734b42`). [game]
pub fn skidding(input: &TyreInput) -> bool {
    (input.drift && input.speed > 1.0)
        || (input.brake > 0.5 && !input.reversing && !input.waiting_to_reverse)
        || (input.stick_x.abs() >= 0.9 && input.speed >= input.cap * 0.85)
}

/// The tyres' part of `FUN_007347c0`. [game]
///
/// The tyres skid while the slide is held above 1 m/s, or the brake is over half down (not
/// reversing or waiting to), or the stick is hard over (0.9) at 0.85 of the speed the
/// trigger allows or more. With a wheel on the ground and the body faster than 1 m/s the
/// rolling loop for the surface plays, and the skid loop with it while skidding; a surface
/// of 0 plays neither. Both loops are started with the body's speed as `speed` and follow
/// it (`FUN_0077d890`, `FUN_00595770`). The game plays each at ONE rear wheel, the one on
/// the side the car is moving to, and starts it again when that wheel's surface changes.
///
/// A rear spring that shortened by more than 0.05 of its length in the update plays
/// `vehicleSuspension` and asks for its `impactForce` to be that share x 10 (see
/// `IMPACT_FORCE`: the event has no parameter of that spelling).
pub fn tyre_sounds(input: &TyreInput) -> TyreSounds {
    let moving = input.speed > 1.0;
    let skidding = skidding(input);
    let on_ground = input.grounded && input.surface != 0 && moving;
    TyreSounds {
        skid: (on_ground && skidding).then_some(input.surface),
        road: on_ground.then_some(input.surface),
        suspension: (input.grounded && input.compression > 0.05).then_some(input.compression * 10.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gearbox_replays_recorded_vehicle_updates() {
        let dir=std::env::var("TF2_GAME_DIR").unwrap_or_else(|_|"C:/Games2".into());
        let gears=load_gears(Path::new(&dir),"Bumblebee").expect("installed gears");
        let mut errors=Vec::new();
        // Offline comparison against a new user capture; default regression stays fixed.
        let capture=std::env::var("TF2_GEARBOX_CAPTURE").ok()
            .map(|path| std::fs::read_to_string(path).expect("gearbox capture fixture"));
        let fixture=capture.as_deref().unwrap_or(include_str!("../fixtures/vehicle_20261007_gearbox.csv"));
        let (mut gear_mismatches,mut load_error,mut revs_error)=(0usize,0.0f32,0.0f32);
        for line in fixture.lines().filter(|line| !line.trim().is_empty()) {
            let r:Vec<f32>=line.split(',').map(|v|v.parse().unwrap()).collect();
            let mut g=Gearbox::new(gears.clone(),15000.0,1.6,5000.0,35.0,3.0);
            (g.gear,g.rpm,g.load,g.revs)=(r[2] as usize,r[3],r[4],r[5]);
            g.grounded_rpm=r[6];g.was_grounded=r[7]!=0.0;g.was_reversing=r[8]!=0.0;
            g.last_speed=r[9];g.stick=r[10];
            g.update(&GearboxInput { velocity:[r[11],r[12],r[13]],trigger:r[14],stick_x:r[15],
                braking:r[16]!=0.0,skid:r[17]!=0.0,reversing:r[18]!=0.0,
                stopped_by_brake:r[19]!=0.0,grounded:r[20]!=0.0,turbo:r[21]!=0.0 },r[1]);
            errors.push((g.rpm-r[23]).abs());
            gear_mismatches+=usize::from(g.gear!=r[22] as usize);
            load_error=load_error.max((g.load-r[24]).abs());
            revs_error=revs_error.max((g.revs-r[25]).abs());
        }
        errors.sort_by(|a,b|a.total_cmp(b));
        let p95=errors[errors.len()*95/100];
        eprintln!("{} recorded gearbox updates: rpm median{},p95{},max{}",errors.len(),errors[errors.len()/2],p95,errors.last().unwrap());
        eprintln!("gear mismatches{gear_mismatches}; max load error{load_error}; max revs error{revs_error}");
        assert_eq!(gear_mismatches,0,"recorded gear mismatch");
        assert!(load_error<0.0001,"recorded load mismatch");
        assert!(revs_error<0.0001,"recorded revs mismatch");
        assert!(*errors.last().unwrap()<0.02,"recorded RPM mismatch: {errors:?}");
    }

    #[test]
    fn the_tyres_skid_on_the_slide_the_brake_and_a_hard_turn_at_speed() {
        const PAVE: u32 = 0x37f7_18bf;
        let rolling = TyreInput { speed: 20.0, cap: 40.0, grounded: true, surface: PAVE, ..Default::default() };
        let heard = tyre_sounds(&rolling);
        assert_eq!((heard.skid, heard.road, heard.suspension), (None, Some(PAVE), None));
        assert_eq!(tyre_sounds(&TyreInput { drift: true, ..rolling }).skid, Some(PAVE));
        assert_eq!(tyre_sounds(&TyreInput { brake: 0.6, ..rolling }).skid, Some(PAVE));
        assert_eq!(tyre_sounds(&TyreInput { brake: 0.5, ..rolling }).skid, None, "the brake has to be over half");
        assert_eq!(tyre_sounds(&TyreInput { brake: 1.0, reversing: true, ..rolling }).skid, None);
        assert_eq!(tyre_sounds(&TyreInput { brake: 1.0, waiting_to_reverse: true, ..rolling }).skid, None);
        // The stick hard over only skids near the speed the trigger allows.
        assert_eq!(tyre_sounds(&TyreInput { stick_x: -1.0, ..rolling }).skid, None);
        assert_eq!(tyre_sounds(&TyreInput { stick_x: -1.0, speed: 34.0, ..rolling }).skid, Some(PAVE));
        assert_eq!(tyre_sounds(&TyreInput { stick_x: 0.8, speed: 40.0, ..rolling }).skid, None);
        // Slow, in the air or on no surface: nothing.
        let quiet = TyreSounds::default();
        assert_eq!(tyre_sounds(&TyreInput { speed: 1.0, drift: true, ..rolling }), quiet);
        assert_eq!(tyre_sounds(&TyreInput { grounded: false, drift: true, ..rolling }), quiet);
        assert_eq!(tyre_sounds(&TyreInput { surface: 0, drift: true, ..rolling }), quiet);
        // The suspension's thump: over a twentieth of the spring in one update, times ten.
        assert_eq!(tyre_sounds(&TyreInput { compression: 0.05, ..rolling }).suspension, None);
        assert_eq!(tyre_sounds(&TyreInput { compression: 0.2, ..rolling }).suspension, Some(2.0));
        assert_eq!(crc32(IMPACT_FORCE.as_bytes()), 0x3283_b7ff);
        assert_ne!(crc32(b"ImpactForce"), 0x3283_b7ff, "the event's own spelling is not the one the code asks for");
    }

    fn game_dir() -> PathBuf {
        PathBuf::from(std::env::var("TF2_GAME_DIR").unwrap_or_else(|_| r"C:\Games2".into()))
    }

    fn bumblebee() -> Library {
        let (library, problems) = Library::load(&[game_dir().join(r"characters\eng\bumblebee.fev")]);
        assert!(problems.is_empty(), "{problems:?}");
        library
    }

    fn loudness(buffer: &[f32]) -> f32 {
        (buffer.iter().map(|v| v * v).sum::<f32>() / buffer.len().max(1) as f32).sqrt()
    }

    #[test]
    fn bumblebees_events_are_found_by_the_ids_his_data_uses() {
        let library = bumblebee();
        assert_eq!(library.event_count(), 38);
        // Ids as they appear in his animation events and sound table.
        assert_eq!(library.name(430740649), Some("ANIM_BUMB_TRANS_ROB_B"));
        assert_eq!(library.name(19899842), Some("VEH_BUMBLEBEE_CAMARO_RPM"));
        assert_eq!(library.name((-422524421i32) as u32), Some("VEH_BUMBLEBEE_CAMARO_REVS"));
    }

    #[test]
    fn installed_chatter_table_binds_decodable_radio_samples() {
        let game = game_dir();
        let data = crate::character::load(&game, "bumblebee").unwrap();
        let (mut library, errors) = Library::load(&[game.join(r"audio\eng\global_streaming.fev")]);
        assert!(errors.is_empty(), "{errors:?}");
        let wanted: Vec<_> = data.chatter.iter().map(|l| l.event).collect();
        assert_eq!(library.add_programmer_bank(&game.join(r"audio\eng\CHATTER.fsb"), event_id("ProgrammerSoundPlayer"), &wanted).unwrap(), 109);
        let missing: Vec<_> = wanted.iter().filter(|&&id| !library.has(id)).collect();
        assert!(missing.is_empty(), "unbound chatter IDs: {missing:x?}");
        for sub in [(crate::chatter::ATTACK, crate::chatter::GENERIC, crate::chatter::ENEMY),
            (crate::chatter::INJURED, crate::chatter::CHARACTER, crate::chatter::SELF),
            (crate::chatter::DIED, crate::chatter::CHARACTER, crate::chatter::SELF)] {
            let line = data.chatter.iter().find(|l| (l.action, l.category, l.subcategory) == sub).unwrap();
            assert_eq!(library.prepare(line.event), 1);
            let mut mixer = Mixer::new(48000);
            mixer.play(&library, line.event, 1.0).unwrap();
            let mut out = vec![0.0; 960];
            let mut peak = 0.0f32;
            for _ in 0..300 {
                mixer.render(&library, &mut out);
                peak = peak.max(loudness(&out));
                if mixer.playing() == 0 { break; }
            }
            assert!(peak > 0.001, "silent radio line {}", library.name(line.event).unwrap());
            assert_eq!(mixer.playing(), 0, "radio line must finish");
        }
    }

    #[test]
    fn category_tree_gains_and_runtime_options_multiply_without_touching_other_buses() {
        let mut library = bumblebee();
        let id = event_id("VEH_BUMBLEBEE_CAMARO_RPM");
        let (file, event) = library.event(id).unwrap();
        let category = event.category.clone();
        library.prepare(id);
        let render = |library: &Library, options: &[(&str, f32)]| {
            let mut mixer = Mixer::new(48000);
            for &(category, volume) in options { mixer.set_category_volume(category, volume); }
            let handle = mixer.play(library, id, 1.0).unwrap();
            mixer.set_parameter(handle, "rpm", 850.0);
            mixer.set_parameter(handle, "load", 1.0);
            let mut out = vec![0.0; 4800];
            mixer.render(library, &mut out);
            loudness(&out)
        };
        let baseline = render(&library, &[]);
        assert!(baseline > 0.001);
        assert!((render(&library, &[("", 0.5), (&category, 0.5)]) / baseline - 0.25).abs() < 1e-5);
        assert_eq!(render(&library, &[("UI", 0.0)]), baseline);
        let authored = library.files[file].fev.categories.iter_mut().find(|c| c.path == category).unwrap();
        authored.volume *= 0.5;
        assert!((render(&library, &[]) / baseline - 0.5).abs() < 1e-5);
    }

    #[test]
    fn player_engine_spatial_envelope_keeps_its_authored_2d_share() {
        let library = bumblebee();
        let (_, event) = library.event(event_id("VEH_BUMBLEBEE_CAMARO_RPM")).unwrap();
        let pan = event.parameters.iter().position(|p| p.name == "2dPanLevel").unwrap();
        let layer = &event.layers[0];
        let share = |index| if index == pan { 1.0 } else { 0.0 };
        let gains = layer_sides(layer, &share, true, 0.25, (0.0, std::f32::consts::SQRT_2));
        assert!((gains.0 - 0.6).abs() < 1e-5);
        assert!((gains.1 - (0.6 + 0.4 * 0.25 * std::f32::consts::SQRT_2)).abs() < 1e-5);
        assert_eq!(layer_sides(layer, &share, false, 0.0, (0.0, 0.0)), (1.0, 1.0));
    }

    #[test]
    fn authored_pitch_and_random_radius_preserve_four_octave_units() {
        // Original 004bf110/004c1330: .25 is one octave; randomness is a
        // symmetric radius, not the earlier half-width approximation.
        assert_eq!(pitch_multiplier(0.25, 0.0, 0.5), 2.0);
        assert_eq!(pitch_multiplier(-0.25, 0.0, 0.5), 0.5);
        assert_eq!(pitch_multiplier(0.0, 0.125, 0.5), 1.0);
        assert!((pitch_multiplier(0.0, 0.125, 0.0) - 0.5f32.sqrt()).abs() < 1e-6);
        assert!((pitch_multiplier(0.0, 0.125, 1.0) - 2.0f32.sqrt()).abs() < 1e-6);
        let (library, errors) = Library::load(&[game_dir().join(r"audio\eng\global.fev")]);
        assert!(errors.is_empty(), "{errors:?}");
        let id = event_id("UI_REGEN_START");
        let (_, event) = library.event(id).unwrap();
        assert!((event.pitch - 0.245).abs() < 1e-6, "the installed regeneration cue carries pitch");
        let mut mixer = Mixer::new(48000);
        let handle = mixer.play(&library, id, 1.0).unwrap();
        let instance = mixer.instances.iter().find(|i| i.handle == handle).unwrap();
        assert!((instance.pitch - 2f32.powf(0.98)).abs() < 1e-6);
    }

    #[test]
    fn pitch_envelope_is_neutral_at_its_centre_and_ignores_disabled_points() {
        let envelope = Envelope { effect: String::new(), flags: Envelope::PITCH,
            points: vec![(0.0, 0.0), (1.0, 1.0)], parameter: 0 };
        let mut layer = crate::formats::fev::Layer { parameter: None,
            sounds: vec![], envelopes: vec![envelope] };
        assert_eq!(layer_pitch(&layer, &|_| 0.5), 1.0);
        assert_eq!(layer_pitch(&layer, &|_| 0.0), 1.0 / 16.0);
        assert_eq!(layer_pitch(&layer, &|_| 1.0), 16.0);
        layer.envelopes[0].flags |= 1;
        assert_eq!(layer_pitch(&layer, &|_| 0.0), 1.0);
    }

    #[test]
    fn sample_choice_modes_preserve_no_repeat_sequence_and_shuffle_rules() {
        let waves = (0..3).map(|i| Wave { weight: if i == 0 { 1 } else { 0 }, sample: None }).collect();
        let mut definition = Definition { name: "choice fixture".into(), play_mode: 2,
            volume: 1.0, volume_floor: 1.0, pitch: 0.0, pitch_randomisation: 0.0, waves };
        let mut random = 0x2545_f491;
        let mut choice = WaveChoice::default();
        let picks: Vec<_> = (0..6).map(|_| choice.pick(&definition, &mut random)).collect();
        // Original advances a repeated weighted pick by one, even if that next
        // waveform's weight is zero. Redrawing would incorrectly repeat index0.
        assert_eq!(picks, [0, 1, 0, 1, 0, 1]);
        definition.play_mode = 3;
        let mut choice = WaveChoice::default();
        assert_eq!((0..6).map(|_| choice.pick(&definition, &mut random)).collect::<Vec<_>>(), [0, 1, 2, 0, 1, 2]);
        definition.play_mode = 4;
        let mut choice = WaveChoice::default();
        let picks: Vec<_> = (0..90).map(|_| choice.pick(&definition, &mut random)).collect();
        for bag in picks.chunks(3) {
            let mut bag = bag.to_vec();
            bag.sort_unstable();
            assert_eq!(bag, [0, 1, 2]);
        }
        assert!(picks.windows(2).all(|w| w[0] != w[1]), "shuffle cycle boundaries must not repeat");
    }

    #[test]
    fn event_fades_keep_their_duration_across_device_callback_sizes() {
        let mut library = bumblebee();
        let id = event_id("ANIM_BUMB_TRANS_ROB_B");
        let (file, index) = library.index[&id];
        let event = Arc::make_mut(&mut library.files[file].events[index]);
        event.fade_in_ms = 100;
        event.fade_out_ms = 200;
        library.prepare(id);
        let render = |block_frames: usize| {
            let mut mixer = Mixer::new(48000);
            mixer.master = 0.1;
            let handle = mixer.play(&library, id, 1.0).unwrap();
            let mut out = Vec::new();
            // Stop partway through fade-in: retain the current level without a pop.
            for (frames, stopping) in [(960, false), (12000, true)] {
                if stopping { mixer.stop(handle); }
                let mut left = frames;
                while left > 0 {
                    let count = left.min(block_frames);
                    let mut buffer = vec![0.0; count * 2];
                    mixer.render(&library, &mut buffer);
                    out.extend(buffer);
                    left -= count;
                }
            }
            assert_eq!(mixer.playing(), 0);
            out
        };
        let small = render(240);
        let large = render(2400);
        let max_error = small.iter().zip(&large).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(max_error < 2e-6, "callback-dependent fade: {max_error}");
        assert!(loudness(&small[..960 * 2]) > 0.00001);
        assert!(small[(960 + 9600) * 2..].iter().all(|v| v.abs() < 1e-7), "authored 200ms tail must finish");
    }

    #[test]
    fn a_one_shot_plays_its_sample_and_ends() {
        let library = bumblebee();
        let id = event_id("ANIM_BUMB_TRANS_ROB_B");
        assert_eq!(library.prepare(id), 1);
        let mut mixer = Mixer::new(48000);
        mixer.play(&library, id, 1.0).unwrap();
        let mut heard = 0.0f32;
        let mut blocks = 0;
        let mut buffer = vec![0.0; 960];
        while mixer.playing() > 0 && blocks < 1000 {
            mixer.render(&library, &mut buffer);
            heard = heard.max(loudness(&buffer));
            blocks += 1;
        }
        // The sample is 0.91 s long; a block is 10 ms. Pitch randomisation moves it a little.
        assert!((80..=100).contains(&blocks), "played for {blocks} blocks");
        assert!(heard > 0.01, "peak block loudness {heard}");
    }

    #[test]
    fn the_engine_moves_between_its_three_samples_with_the_revolutions() {
        let library = bumblebee();
        let id = event_id("VEH_BUMBLEBEE_CAMARO_RPM");
        assert_eq!(library.prepare(id), 3);
        let mut mixer = Mixer::new(48000);
        let handle = mixer.play(&library, id, 1.0).unwrap();
        mixer.set_parameter(handle, "load", 1.0);
        let mut buffer = vec![0.0; 9600];
        let mut sounding = |mixer: &mut Mixer, rpm: f32| {
            mixer.set_parameter(handle, "rpm", rpm);
            for _ in 0..3 {
                mixer.render(&library, &mut buffer);
            }
            let parts = &mixer.instances[0].parts;
            let gains: Vec<bool> = parts.iter().map(|p| p.voice.as_ref().is_some_and(|v| v.gain > 0.0)).collect();
            (gains, loudness(&buffer))
        };
        // Idle, medium and high in the order the event lists them.
        let (at_idle, loud) = sounding(&mut mixer, 850.0);
        assert_eq!(at_idle, [true, false, false]);
        assert!(loud > 0.005, "idle loudness {loud}");
        assert_eq!(sounding(&mut mixer, 3000.0).0, [false, true, false]);
        assert_eq!(sounding(&mut mixer, 4500.0).0, [false, true, true]);
        assert_eq!(sounding(&mut mixer, 8000.0).0, [false, false, true]);
        // It loops: still there after ten seconds, and gone soon after a stop.
        for _ in 0..50 {
            mixer.render(&library, &mut buffer);
        }
        assert_eq!(mixer.playing(), 1);
        mixer.stop(handle);
        for _ in 0..3 {
            mixer.render(&library, &mut buffer);
        }
        assert_eq!(mixer.playing(), 0);
    }

    #[test]
    fn the_engines_layer_is_duller_at_no_load() {
        let library = bumblebee();
        let id = event_id("VEH_BUMBLEBEE_CAMARO_RPM");
        let (_, event) = library.event(id).unwrap();
        let load = event.parameters.iter().position(|p| p.name == "load").unwrap();
        let at = |value: f32| cutoff(&event.layers[0], &|index| if index == load { value } else { 0.0 }).unwrap();
        // [data] the envelope: 0.894 at no load, 1 from half load up.
        assert!((at(0.0) - 10.0 * 2200f32.powf(0.893617)).abs() < 1.0);
        assert!(at(0.0) < 10000.0 && at(1.0) > 21999.0);
        // The filter: the fastest wobble there is goes through a 1 kHz cut much smaller, a
        // steady level whole, and an open filter changes nothing.
        let wobble: Vec<f32> = (0..2000).map(|i| if (i / 2) % 2 == 0 { 1.0 } else { -1.0 }).collect();
        let through = |cutoff: Option<f32>, from: &[f32]| {
            let (mut out, mut gather, mut filter) = (vec![0.0; from.len()], from.to_vec(), [0.0; 4]);
            settle(&mut out, &mut gather, &mut filter, cutoff, 48000.0, (1.0, 1.0));
            out
        };
        assert!(loudness(&through(Some(1000.0), &wobble)) < 0.1);
        assert_eq!(through(None, &wobble), wobble);
        let steady = vec![1.0; 2000];
        assert!((through(Some(1000.0), &steady)[1999] - 1.0).abs() < 1e-3);
    }

    #[test]
    fn engine_crossfades_use_the_authored_overlap_not_the_whole_rpm_range() {
        let library = bumblebee();
        let (_, event) = library.event(event_id("VEH_BUMBLEBEE_CAMARO_RPM")).unwrap();
        let sounds = &event.layers[0].sounds;
        assert_eq!(sounds.iter().map(|s| s.fade_curve).collect::<Vec<_>>(), [2, 0, 0]);
        // Original FEV: idle/medium overlap1438..1845, medium/high3847..5298.
        // Full medium at3000 was wrongly attenuated by the previous interpretation.
        assert_eq!(sound_span(&sounds[1], 3000.0 / 8800.0), 1.0);
        assert_eq!(sound_span(&sounds[0], 850.0 / 8800.0), 1.0);
        assert_eq!(sound_span(&sounds[2], 8000.0 / 8800.0), 1.0);
        for rpm in [4000.0, 4500.0, 5000.0] {
            let sum = sound_span(&sounds[1], rpm / 8800.0) + sound_span(&sounds[2], rpm / 8800.0);
            assert!((sum - 1.0).abs() < 2e-5, "overlap gain at{rpm}: {sum}");
        }
        // The idle's equal-power fade, checked at the middle of its authored overlap.
        let mid = sounds[0].start + sounds[0].length * (1.0 - sounds[0].fade_out * 0.5);
        assert!((sound_span(&sounds[0], mid) - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-5);
        library.prepare(event.id);
        let mut mixer = Mixer::new(48000);
        let handle = mixer.play(&library, event.id, 1.0).unwrap();
        mixer.set_parameter(handle, "rpm", 3000.0);
        mixer.set_parameter(handle, "load", 1.0);
        let mut buffer = vec![0.0; 960];
        mixer.render(&library, &mut buffer);
        let instance = &mixer.instances[0];
        let medium = &instance.parts[1];
        let expected = event.volume * 0.81 * medium.gain;
        assert!((medium.voice.as_ref().unwrap().gain - expected).abs() < 1e-6);
    }

    #[test]
    fn engine_lowpass_has_the_original_two_stage_impulse_and_upper_transition() {
        // Numerical reference from004692a0/00469370: at1000Hz/48000Hz alpha=.11574828.
        let alpha = lowpass_coefficient(1000.0, 48000.0);
        assert!((alpha - 0.11574828).abs() < 1e-7);
        let mut out = vec![0.0; 8];
        let mut gather = vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let mut filter = [0.0; 4];
        settle(&mut out, &mut gather, &mut filter, Some(1000.0), 48000.0, (1.0, 1.0));
        assert!((out[0] - 0.013397664).abs() < 1e-7);
        assert!((out[2] - 0.023693822).abs() < 1e-7);
        assert_eq!([out[1], out[3], out[5], out[7]], [0.0; 4]);
        assert!((lowpass_coefficient(48000.0 / std::f32::consts::PI, 48000.0) - 2.0 / 3.0).abs() < 1e-6);
        assert!(lowpass_coefficient(20000.0, 48000.0) > 0.89);
        assert_eq!(lowpass_coefficient(22000.0, 48000.0), 1.0);
        let history = filter;
        settle(&mut out, &mut gather, &mut filter, Some(22000.0), 48000.0, (1.0, 1.0));
        assert_eq!(filter, history, "the original bypass leaves filter memory alone");
    }

    #[test]
    fn an_audible_engine_loop_wrap_does_not_drop_an_output_frame() {
        let library = bumblebee();
        let id = event_id("VEH_BUMBLEBEE_CAMARO_RPM");
        library.prepare(id);
        let mut mixer = Mixer::new(48000);
        let handle = mixer.play(&library, id, 1.0).unwrap();
        mixer.set_parameter(handle, "rpm", 850.0); // Idle has constant1x pitch.
        mixer.set_parameter(handle, "load", 1.0);
        mixer.render(&library, &mut [0.0; 2]);
        let voice = mixer.instances[0].parts[0].voice.as_mut().unwrap();
        mixer.rate = voice.sample.rate as f32; // Exactly one source frame per output frame.
        let total = voice.sample.frames();
        voice.at = (total - 2) as f64;
        mixer.render(&library, &mut [0.0; 8]);
        assert_eq!(mixer.instances[0].parts[0].voice.as_ref().unwrap().at, 2.0);
    }

    #[test]
    fn a_placed_sound_leans_to_its_side_and_an_interface_sound_does_not() {
        let library = bumblebee();
        let id = event_id("ANIM_BUMB_TRANS_ROB_B");
        library.prepare(id);
        let heard = |side: f32| {
            let mut mixer = Mixer::new(48000);
            mixer.side = side;
            mixer.play(&library, id, 1.0).unwrap();
            let mut buffer = vec![0.0; 19200];
            mixer.render(&library, &mut buffer);
            let one = |channel: usize| loudness(&buffer.iter().skip(channel).step_by(2).copied().collect::<Vec<_>>());
            (one(0), one(1))
        };
        let (left, right) = heard(1.0);
        assert!(left < 1e-4 && right > 0.01, "{left} {right}");
        let (left, right) = heard(-1.0);
        assert!(right < 1e-4 && left > 0.01, "{left} {right}");
        // The interface's sounds have no place: the same on both sides, at any distance.
        let (shared, problems) = Library::load(&[game_dir().join(r"levels\us_city_00\eng\mission.fev")]);
        assert!(problems.is_empty(), "{problems:?}");
        let id = event_id("UI_BOOST_READY");
        assert!(shared.prepare(id) > 0);
        let mut mixer = Mixer::new(48000);
        (mixer.side, mixer.distance) = (1.0, 500.0);
        mixer.play(&shared, id, 1.0).unwrap();
        let mut buffer = vec![0.0; 19200];
        mixer.render(&shared, &mut buffer);
        let one = |channel: usize| loudness(&buffer.iter().skip(channel).step_by(2).copied().collect::<Vec<_>>());
        assert!(one(0) > 0.005 && (one(0) - one(1)).abs() < one(0) * 0.5, "{} {}", one(0), one(1));
    }

    #[test]
    fn a_change_of_gear_glides_at_the_events_seek_speed() {
        let library = bumblebee();
        let id = event_id("VEH_BUMBLEBEE_CAMARO_RPM");
        library.prepare(id);
        let mut mixer = Mixer::new(48000);
        let handle = mixer.play(&library, id, 1.0).unwrap();
        // Before it is first heard a value is taken at once.
        mixer.set_parameter(handle, "rpm", 6000.0);
        let mut buffer = vec![0.0; 960];
        mixer.render(&library, &mut buffer);
        assert_eq!(mixer.instances[0].parameters[0], 6000.0);
        // [data] seek speed 1: the whole 8800 in a second, so 88 in a block of 10 ms, and
        // the drop to second gear's 3000 takes a third of a second.
        mixer.set_parameter(handle, "rpm", 3000.0);
        mixer.render(&library, &mut buffer);
        assert!((mixer.instances[0].parameters[0] - 5912.0).abs() < 0.5);
        for _ in 0..40 {
            mixer.render(&library, &mut buffer);
        }
        assert_eq!(mixer.instances[0].parameters[0], 3000.0);
    }

    #[test]
    fn an_event_makes_room_for_itself() {
        let library = bumblebee();
        let id = event_id("VEH_BUMBLEBEE_CAMARO_REVS");
        let mut mixer = Mixer::new(48000);
        let first = mixer.play(&library, id, 1.0).unwrap();
        // This event allows one at a time.
        let second = mixer.play(&library, id, 1.0).unwrap();
        assert!(!mixer.is_playing(first));
        assert!(mixer.is_playing(second));
    }

    #[test]
    fn gears_carry_the_revolutions_up_through_the_speed_range() {
        let gears = load_gears(&game_dir(), "Bumblebee").unwrap();
        assert_eq!(gears.len(), 6);
        assert_eq!((gears[0].min_rpm, gears[0].max_rpm, gears[5].max_rpm), (1000.0, 6000.0, 8800.0));
        assert!((gears[0].ratio - 2.97).abs() < 1e-4);
        // His numbers: engineTorque 15000, front wheels 1.6, a body of 5000, maxSpeed 36.
        let mut engine = Gearbox::new(gears, 15000.0, 1.6, 5000.0, 36.0, 3.0);
        assert_eq!((engine.up_rate, engine.down_rate), (3.75, 13.125));
        let dt = 0.032;
        let pulling = |speed: f32| GearboxInput { velocity: [0.0, speed, 0.0], trigger: 1.0, grounded: true, ..Default::default() };
        // Pulling away: first gear from 1782 (revs 1 x 2.97 x 600) up to its 6000, which
        // takes (2/3)(3.367^1.5 - 1) / 3.75 = 0.92 s, then second from its 3000.
        engine.update(&pulling(0.0), dt);
        assert!((engine.rpm - 2.97 * 600.0 * (1.0 + 3.75 * dt)).abs() < 1.0, "{}", engine.rpm);
        let mut updates = 1;
        while !engine.shifted_up {
            // The load sits over a half while the revolutions rise.
            assert!(engine.rpm <= 6000.0 && engine.gear == 0 && engine.load > 0.5);
            engine.update(&pulling(updates as f32), dt);
            updates += 1;
        }
        assert!((27..=31).contains(&updates), "first gear lasted {updates} updates");
        assert_eq!((engine.gear, engine.rpm), (1, 3000.0));
        // The change up halves the revolutions, and the load drops with them.
        assert!(engine.load < 0.3, "{}", engine.load);
        // Held flat out it runs through every gear and then saws in the last: back to
        // 8250 / 0.8 x 0.63 = 6497 each time it passes 8800.
        let mut lowest = f32::MAX;
        for _ in 0..2000 {
            engine.update(&pulling(36.0), dt);
            assert!(engine.rpm <= 8800.0);
            if engine.gear == 5 && engine.shifted_up {
                lowest = lowest.min(engine.rpm);
            }
        }
        assert_eq!(engine.gear, 5);
        assert!((lowest - 6496.9).abs() < 1.0, "{lowest}");
        // The trigger let go while he slows: down through the gears to first's minimum.
        let coasting = |speed: f32| GearboxInput { velocity: [0.0, speed, 0.0], grounded: true, ..Default::default() };
        for i in 0..10 {
            engine.update(&coasting(36.0 - i as f32), dt);
        }
        assert_eq!((engine.gear, engine.rpm), (0, 1000.0));
        // Braking holds first's minimum at a quarter load; stopped by the brake, a half.
        engine.update(&GearboxInput { braking: true, ..pulling(20.0) }, dt);
        assert_eq!((engine.rpm, engine.load), (1000.0, 0.25));
        engine.update(&GearboxInput { stopped_by_brake: true, ..pulling(0.0) }, dt);
        assert_eq!((engine.gear, engine.rpm, engine.load), (0, 1000.0, 0.5));
        // Off the ground the engine runs to the top of its gear.
        engine.update(&GearboxInput { grounded: false, ..pulling(10.0) }, dt);
        assert_eq!(engine.rpm, 6000.0);
        // Standing with the trigger up: nothing turns, and nothing is not a number.
        for _ in 0..5 {
            engine.update(&coasting(0.0), dt);
        }
        assert_eq!(engine.rpm, 0.0);
        assert!(engine.load.is_finite());
    }
}
