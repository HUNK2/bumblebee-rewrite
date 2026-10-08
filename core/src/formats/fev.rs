//! FMOD Designer event files (`.fev`, `FEV1`, version 0x00380000): which samples a named
//! sound event plays and how loud, and for the few events driven by a parameter (the engine by
//! its revolutions) how the samples are laid along it.
//!
//! Animation events and the character's sound table name an event by the CRC-32 of its name.
//!
//! Layout, worked out from the files (no code read) and checked by a test that walks every
//! `.fev` in the install to the end of its sound definitions. Little endian; a string is a
//! u32 length that counts its closing zero, then the bytes (length 0 for none).
//!
//!   'FEV1' | u32 version | u32 | u32 | string project
//!   u32 bank count, each: u32 flags | u32 max streams | string name   (name + ".fsb")
//!   category tree, root first: string name | f32 volume | f32 pitch | u32 | u32 |
//!       u32 child count | children
//!   u32 group count, each group: string name | user properties | u32 subgroup count |
//!       u32 event count | subgroups | events
//!   user properties: u32 count, each: string name | u32 kind | kind 0 i32, 1 f32, 2 string
//!   event: u32 kind | string name | 33 words | then by kind:
//!     kind 8:  u32 layer count | layers | u32 parameter count | parameters | user properties
//!     kind 16 (a simple event): u32 | one sound
//!     then: u32 category count | string category
//!     words: 0 volume, 1 pitch, 2 pitch randomisation, 3 volume randomisation, 4 priority,
//!       5 max playbacks, 7 mode bits, 8 min distance, 9 max distance,
//!       27 fade-in ms, 28 fade-out ms
//!     layer: u16 | i16 | i16 control parameter | u16 sound count | u16 envelope count |
//!       sounds | envelopes
//!     sound (58 bytes): u16 sound definition | f32 start | f32 length (both along the control
//!       parameter, 0 to 1) | u32 | u32 loop mode (0 loops) | i32 loop count | u32 auto pitch |
//!       f32 auto pitch reference | f32 auto pitch at minimum | f32 fine tune | f32 volume |
//!       f32 fade in | f32 fade out | u32 fade curve | u32 secondary curve
//!     envelope: i32 | string effect name | u32 | u32 flags | u32 point count |
//!       points {f32 x, f32 y, u32 curve} | u32 parameter | u32
//!     parameter: string name | f32 velocity | f32 min | f32 max | u32 flags | f32 seek speed |
//!       u32 envelopes | u32 sustain count | f32 sustain points
//!   u32 property block count, each 15 words: 0 play mode, 4 volume, 8 volume randomisation,
//!       9 pitch, 13 pitch randomisation
//!   u32 sound definition count, each: string name | u32 property block | u32 waveform count |
//!       waveforms
//!     waveform: u32 kind | u32 weight | kinds 2 and 3 are silence and end here | kind 0:
//!       string file | string bank | u32 index in the bank | u32 length in ms
//!   (three files carry more after this: music and reverb data, not read)
//!
//! [game] FUN_004c5230 measures fades as fractions of the SOUND's length, selects their
//! curve from the first curve word, and blends auto pitch with its value at minimum.

use super::hash::crc32;

/// A point of an envelope: where along the parameter (0 to 1) and the value there.
pub type Point = (f32, f32);

#[derive(Clone, Debug)]
pub struct Sound {
    /// Index into `Fev::definitions`.
    pub definition: usize,
    /// Where on the layer's parameter this sound plays, both as a share of its range.
    pub start: f32,
    pub length: f32,
    /// 0 loops for as long as the event lives; 1 plays once.
    pub one_shot: bool,
    pub auto_pitch: bool,
    pub auto_pitch_reference: f32,
    pub auto_pitch_at_min: f32,
    pub fine_tune: f32,
    pub volume: f32,
    /// Fractions of this sound's length; negative for none. [game: 004c5230]
    pub fade_in: f32,
    pub fade_out: f32,
    /// First curve word's low nibble drives both fades in this executable. [game]
    pub fade_curve: u32,
}

#[derive(Clone, Debug)]
pub struct Envelope {
    /// The effect this drives (`FMOD Lowpass Simple`), or empty for a property of the layer.
    pub effect: String,
    pub flags: u32,
    pub points: Vec<Point>,
    /// Index into the event's parameters.
    pub parameter: usize,
}

impl Envelope {
    /// Flag bits seen on the envelope that scales a layer's volume.
    pub const VOLUME: u32 = 0x0c;
    /// Channel::set3DLevel, dispatched by 004c5230 to 00415e20.
    pub const SPATIAL_BLEND: u32 = 0x404;
    /// [game: 004c5230] Normalized envelope maps to -4..4 octaves.
    pub const PITCH: u32 = 0x14;

    /// The value at `x` (0 to 1 along the parameter), straight lines between the points.
    pub fn at(&self, x: f32) -> f32 {
        let Some(&(first_x, first_y)) = self.points.first() else { return 1.0 };
        if x <= first_x {
            return first_y;
        }
        for pair in self.points.windows(2) {
            let ((x0, y0), (x1, y1)) = (pair[0], pair[1]);
            if x <= x1 {
                return if x1 > x0 { y0 + (y1 - y0) * (x - x0) / (x1 - x0) } else { y1 };
            }
        }
        self.points[self.points.len() - 1].1
    }
}

#[derive(Clone, Debug)]
pub struct Layer {
    /// The parameter its sounds are laid along, if any.
    pub parameter: Option<usize>,
    pub sounds: Vec<Sound>,
    pub envelopes: Vec<Envelope>,
}

#[derive(Clone, Debug)]
pub struct Parameter {
    pub name: String,
    pub min: f32,
    pub max: f32,
    /// Units per second the parameter moves by itself (the overheat loops run on this).
    pub velocity: f32,
    /// Fractions of the parameter range per second; 0 jumps. [game: 004cd990]
    pub seek_speed: f32,
}

#[derive(Clone, Debug)]
pub struct Event {
    pub name: String,
    /// CRC-32 of the name: what the game's data calls the event by.
    pub id: u32,
    pub volume: f32,
    /// [game: 004bf110/004c1330] Pitch and its symmetric random radius are in
    /// four-octave units. Zero is the sample's original pitch.
    pub pitch: f32,
    pub pitch_randomisation: f32,
    /// Share of volume that may be taken off.
    pub volume_randomisation: f32,
    pub max_playbacks: u32,
    /// FMOD's mode bits: 0x08 a sound with no place (the interface's), 0x10 one in the world.
    pub mode: u32,
    pub min_distance: f32,
    pub max_distance: f32,
    pub fade_in_ms: u32,
    pub fade_out_ms: u32,
    pub layers: Vec<Layer>,
    pub parameters: Vec<Parameter>,
    pub category: String,
}

#[derive(Clone, Debug)]
pub struct Wave {
    pub weight: u32,
    /// Bank name without `.fsb` and the sample's index in it; `None` plays nothing.
    pub sample: Option<(String, usize)>,
}

#[derive(Clone, Debug)]
pub struct Definition {
    pub name: String,
    /// [game: 0118aa60] 0/3 sequential, 1 weighted, 2 weighted without repeats,
    /// 4 shuffled per sound, 6 shuffled across the definition; 5 programmer.
    pub play_mode: u32,
    pub volume: f32,
    /// The quietest a play may be, as a share of `volume` (1 is no randomisation).
    pub volume_floor: f32,
    /// [game: 004df040] Four-octave units, as on the event.
    pub pitch: f32,
    pub pitch_randomisation: f32,
    pub waves: Vec<Wave>,
}

pub struct Fev {
    pub project: String,
    pub banks: Vec<String>,
    pub events: Vec<Event>,
    pub definitions: Vec<Definition>,
    pub categories: Vec<Category>,
}

#[derive(Clone, Debug)]
pub struct Category {
    /// Relative to master, matching the event's category path; master is empty.
    pub path: String,
    pub volume: f32,
    pub pitch: f32,
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn bytes(&mut self, count: usize) -> Result<&[u8], String> {
        let end = self.at.checked_add(count).filter(|&end| end <= self.data.len());
        let Some(end) = end else {
            return Err(format!("read of {count} bytes at {:#x} runs past the end ({:#x})", self.at, self.data.len()));
        };
        let slice = &self.data[self.at..end];
        self.at = end;
        Ok(slice)
    }

    fn u16(&mut self) -> Result<u16, String> {
        self.bytes(2).map(|b| u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, String> {
        self.bytes(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn f32(&mut self) -> Result<f32, String> {
        self.u32().map(f32::from_bits)
    }

    /// A count that is about to drive a loop: anything huge means the walk has gone wrong.
    fn count(&mut self, what: &str) -> Result<usize, String> {
        let at = self.at;
        let count = self.u32()? as usize;
        if count > 0x10000 {
            return Err(format!("{count} {what} at {at:#x}"));
        }
        Ok(count)
    }

    fn string(&mut self) -> Result<String, String> {
        let at = self.at;
        let len = self.u32()? as usize;
        if len > 4096 {
            return Err(format!("string of {len} bytes at {at:#x}"));
        }
        let bytes = self.bytes(len)?;
        let text = bytes.split(|&b| b == 0).next().unwrap_or(&[]);
        Ok(text.iter().map(|&b| b as char).collect())
    }

    fn words<const N: usize>(&mut self) -> Result<[u32; N], String> {
        let mut out = [0u32; N];
        for word in &mut out {
            *word = self.u32()?;
        }
        Ok(out)
    }
}

fn read_category(r: &mut Reader, parent: Option<&str>, out: &mut Vec<Category>) -> Result<(), String> {
    let name = r.string()?;
    let path = match parent {
        None => String::new(),
        Some("") => name,
        Some(parent) => format!("{parent}/{name}"),
    };
    let volume = r.f32()?;
    let pitch = r.f32()?;
    r.words::<2>()?;
    out.push(Category { path: path.clone(), volume, pitch });
    for _ in 0..r.count("categories")? {
        read_category(r, Some(&path), out)?;
    }
    Ok(())
}

fn read_properties(r: &mut Reader) -> Result<(), String> {
    for _ in 0..r.count("user properties")? {
        let name = r.string()?;
        match r.u32()? {
            0 | 1 => {
                r.u32()?;
            }
            2 => {
                r.string()?;
            }
            kind => return Err(format!("user property {name} of kind {kind} at {:#x}", r.at)),
        }
    }
    Ok(())
}

fn read_sound(r: &mut Reader) -> Result<Sound, String> {
    let definition = r.u16()? as usize;
    let (start, length) = (r.f32()?, r.f32()?);
    let [_start_mode, loop_mode, _loop_count, auto_pitch] = r.words()?;
    let (auto_pitch_reference, auto_pitch_at_min, fine_tune, volume) = (r.f32()?, r.f32()?, r.f32()?, r.f32()?);
    let (fade_in, fade_out) = (r.f32()?, r.f32()?);
    let [fade_curve, _secondary_curve] = r.words::<2>()?;
    Ok(Sound {
        definition,
        start,
        length,
        one_shot: loop_mode != 0,
        auto_pitch: auto_pitch != 0,
        auto_pitch_reference,
        auto_pitch_at_min,
        fine_tune,
        volume,
        fade_in,
        fade_out,
        fade_curve: fade_curve & 0xf,
    })
}

fn read_event(r: &mut Reader) -> Result<Event, String> {
    let at = r.at;
    let kind = r.u32()?;
    let name = r.string()?;
    if kind != 8 && kind != 16 {
        return Err(format!("event {name} of kind {kind} at {at:#x}"));
    }
    let head: [u32; 33] = r.words()?;
    let float = |i: usize| f32::from_bits(head[i]);
    let mut event = Event {
        id: crc32(name.as_bytes()),
        name,
        volume: float(0),
        pitch: float(1),
        pitch_randomisation: float(2),
        volume_randomisation: float(3),
        max_playbacks: head[5],
        mode: head[7],
        min_distance: float(8),
        max_distance: float(9),
        fade_in_ms: head[27],
        fade_out_ms: head[28],
        layers: Vec::new(),
        parameters: Vec::new(),
        category: String::new(),
    };
    if kind == 16 {
        // A simple event: one sound and nothing to drive it.
        r.u32()?;
        event.layers.push(Layer { parameter: None, sounds: vec![read_sound(r)?], envelopes: Vec::new() });
    } else {
        for _ in 0..r.count("layers")? {
            r.words::<1>()?;
            let parameter = r.u16()? as i16;
            let (sounds, envelopes) = (r.u16()?, r.u16()?);
            let mut layer = Layer { parameter: usize::try_from(parameter).ok(), sounds: Vec::new(), envelopes: Vec::new() };
            for _ in 0..sounds {
                layer.sounds.push(read_sound(r)?);
            }
            for _ in 0..envelopes {
                r.u32()?;
                let effect = r.string()?;
                r.u32()?;
                let flags = r.u32()?;
                let mut points = Vec::new();
                for _ in 0..r.count("envelope points")? {
                    points.push((r.f32()?, r.f32()?));
                    r.u32()?;
                }
                let parameter = r.u32()? as usize;
                r.u32()?;
                layer.envelopes.push(Envelope { effect, flags, points, parameter });
            }
            event.layers.push(layer);
        }
        for _ in 0..r.count("parameters")? {
            let name = r.string()?;
            let (velocity, min, max) = (r.f32()?, r.f32()?, r.f32()?);
            let [_flags, seek_speed, _envelopes] = r.words::<3>()?;
            for _ in 0..r.count("sustain points")? {
                r.u32()?;
            }
            event.parameters.push(Parameter { name, min, max, velocity, seek_speed: f32::from_bits(seek_speed) });
        }
        read_properties(r)?;
    }
    for _ in 0..r.count("categories")? {
        event.category = r.string()?;
    }
    Ok(event)
}

fn read_group(r: &mut Reader, events: &mut Vec<Event>) -> Result<(), String> {
    r.string()?;
    read_properties(r)?;
    let (groups, count) = (r.count("groups")?, r.count("events")?);
    for _ in 0..groups {
        read_group(r, events)?;
    }
    for _ in 0..count {
        events.push(read_event(r)?);
    }
    Ok(())
}

impl Fev {
    pub fn parse(data: &[u8]) -> Result<Self, String> {
        if data.get(..4) != Some(b"FEV1") {
            return Err("not an FEV1 file".into());
        }
        let mut r = Reader { data, at: 4 };
        r.words::<3>()?;
        let project = r.string()?;
        let mut banks = Vec::new();
        for _ in 0..r.count("banks")? {
            r.words::<2>()?;
            banks.push(r.string()?);
        }
        let mut categories = Vec::new();
        read_category(&mut r, None, &mut categories)?;
        let mut events = Vec::new();
        for _ in 0..r.count("groups")? {
            read_group(&mut r, &mut events)?;
        }
        let mut blocks = Vec::new();
        for _ in 0..r.count("property blocks")? {
            blocks.push(r.words::<15>()?);
        }
        let mut definitions = Vec::new();
        for _ in 0..r.count("sound definitions")? {
            let name = r.string()?;
            let block = r.u32()? as usize;
            let block = blocks.get(block).ok_or_else(|| format!("{name} uses property block {block} of {}", blocks.len()))?;
            let mut waves = Vec::new();
            for _ in 0..r.count("waveforms")? {
                let (kind, weight) = (r.u32()?, r.u32()?);
                let sample = match kind {
                    0 => {
                        r.string()?;
                        let bank = r.string()?;
                        let index = r.u32()? as usize;
                        r.u32()?;
                        Some((bank, index))
                    }
                    2 | 3 => None,
                    _ => return Err(format!("waveform of kind {kind} in {name} at {:#x}", r.at)),
                };
                waves.push(Wave { weight, sample });
            }
            definitions.push(Definition {
                name,
                play_mode: block[0],
                volume: f32::from_bits(block[4]),
                volume_floor: f32::from_bits(block[8]),
                pitch: f32::from_bits(block[9]),
                pitch_randomisation: f32::from_bits(block[13]),
                waves,
            });
        }
        for event in &events {
            for sound in event.layers.iter().flat_map(|layer| &layer.sounds) {
                if sound.definition >= definitions.len() {
                    return Err(format!("{} plays definition {} of {}", event.name, sound.definition, definitions.len()));
                }
            }
        }
        Ok(Self { project, banks, events, definitions, categories })
    }

    pub fn event(&self, id: u32) -> Option<&Event> {
        self.events.iter().find(|e| e.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_runs_straight_between_its_points() {
        let envelope = Envelope { effect: String::new(), flags: Envelope::VOLUME, points: vec![(0.0, 0.2), (0.5, 1.0)], parameter: 0 };
        assert_eq!(envelope.at(-1.0), 0.2);
        assert!((envelope.at(0.25) - 0.6).abs() < 1e-6);
        assert_eq!(envelope.at(0.9), 1.0);
    }

    fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect(&path, out);
            } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("fev")) {
                out.push(path);
            }
        }
    }

    #[test]
    fn every_event_file_in_the_game_parses() {
        let game = std::path::PathBuf::from(std::env::var("TF2_GAME_DIR").unwrap_or_else(|_| r"C:\Games2".into()));
        let mut files = Vec::new();
        collect(&game, &mut files);
        assert_eq!(files.len(), 172);
        let mut events = 0;
        for path in &files {
            let fev = Fev::parse(&std::fs::read(path).unwrap()).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            events += fev.events.len();
        }
        assert_eq!(events, 24085);
    }
}
