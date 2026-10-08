//! FMOD sound banks (`.fsb`, `FSB4`). The layout is in the docstring of `tools\tf2_fsb.py`.
//!
//! Every sample in the game's banks is mono or stereo MPEG audio whose frames are padded to
//! two bytes; the padding is dropped here and the rest decoded to floats.

use std::fs::File;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use super::Bytes;

const HEADER: usize = 48;
const MODE_LOOP: u32 = 0x2;

pub struct Entry {
    pub name: String,
    /// Length in sample frames once decoded.
    pub frames: usize,
    pub bytes: usize,
    /// Where the data starts, from the start of the bank's sample data.
    pub offset: usize,
    pub looping: bool,
    /// Authored loop, start inclusive and end exclusive (FSB stores an inclusive end).
    pub loop_range: std::ops::Range<usize>,
    pub rate: u32,
    pub channels: usize,
}

pub struct Bank {
    path: PathBuf,
    data_start: usize,
    pub entries: Vec<Entry>,
}

/// Decoded audio: `channels` floats per frame, interleaved.
pub struct Sample {
    pub rate: u32,
    pub channels: usize,
    pub data: Vec<f32>,
    pub looping: bool,
    pub loop_range: std::ops::Range<usize>,
}

impl Sample {
    pub fn frames(&self) -> usize {
        self.data.len() / self.channels.max(1)
    }
}

/// Size in bytes of the MPEG frame whose header is at `at`, if there is one.
fn frame_size(data: &[u8], at: usize) -> Option<usize> {
    let h = u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?);
    if h >> 21 != 0x7ff {
        return None;
    }
    let (version, layer) = ((h >> 19) & 3, 4 - ((h >> 17) & 3));
    let (bitrate, rate, padding) = (((h >> 12) & 15) as usize, ((h >> 10) & 3) as usize, ((h >> 9) & 1) as usize);
    if version == 1 || layer == 4 || bitrate == 0 || bitrate == 15 || rate == 3 {
        return None;
    }
    let first = version == 3;
    let kbps: [usize; 15] = match (first, layer) {
        (true, 1) => [0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448],
        (true, 2) => [0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384],
        (true, _) => [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320],
        (false, 1) => [0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256],
        (false, _) => [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
    };
    let hz = match version {
        3 => [44100, 48000, 32000],
        2 => [22050, 24000, 16000],
        _ => [11025, 12000, 8000],
    }[rate];
    let bits = kbps[bitrate] * 1000;
    Some(match (layer, first) {
        (1, _) => (12 * bits / hz + padding) * 4,
        (3, false) => 72 * bits / hz + padding,
        _ => 144 * bits / hz + padding,
    })
}

/// The sample's MPEG frames back to back, as an ordinary MP3 stream.
fn unpad(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(data.len());
    let mut at = 0;
    while at < data.len() {
        if data[at..].iter().take(4).all(|&b| b == 0) {
            at += 2;
            continue;
        }
        let size = frame_size(data, at).ok_or_else(|| format!("no MPEG frame at byte {at} of {}", data.len()))?;
        out.extend_from_slice(&data[at..(at + size).min(data.len())]);
        at = (at + size + 1) & !1;
    }
    Ok(out)
}

fn decode(mp3: Vec<u8>) -> Result<(Vec<f32>, usize, u32), String> {
    let stream = MediaSourceStream::new(Box::new(Cursor::new(mp3)), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("mp3");
    let probed = symphonia::default::get_probe()
        .format(&hint, stream, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| format!("not readable as MPEG audio: {e}"))?;
    let mut format = probed.format;
    let track = format.default_track().ok_or("no audio track")?;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("no decoder: {e}"))?;
    let (mut data, mut channels, mut rate) = (Vec::new(), 0, 0);
    while let Ok(packet) = format.next_packet() {
        // A frame that does not decode is skipped; the ones around it still play.
        let Ok(decoded) = decoder.decode(&packet) else { continue };
        let spec = *decoded.spec();
        channels = spec.channels.count();
        rate = spec.rate;
        let mut buffer = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buffer.copy_interleaved_ref(decoded);
        data.extend_from_slice(buffer.samples());
    }
    if channels == 0 || data.is_empty() {
        return Err("no audio decoded".into());
    }
    Ok((data, channels, rate))
}

impl Bank {
    pub fn open(path: &Path) -> Result<Self, String> {
        let mut file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut head = [0u8; HEADER];
        file.read_exact(&mut head).map_err(|e| format!("{}: {e}", path.display()))?;
        if &head[..4] != b"FSB4" {
            return Err(format!("{}: not an FSB4 bank", path.display()));
        }
        let (count, table_bytes, data_bytes) = (head.u32_at(4)? as usize, head.u32_at(8)? as usize, head.u32_at(12)? as usize);
        if table_bytes > 64 << 20 {
            return Err(format!("{}: a header table of {table_bytes} bytes", path.display()));
        }
        let mut table = vec![0u8; table_bytes];
        file.read_exact(&mut table).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut entries = Vec::with_capacity(count);
        let (mut at, mut offset) = (0, 0);
        for _ in 0..count {
            let size = table.u16_at(at)? as usize;
            let name = table.get(at + 2..at + 32).ok_or("sample header runs past the table")?;
            let name: String = name.iter().take_while(|&&b| b != 0).map(|&b| b as char).collect();
            let bytes = table.u32_at(at + 36)? as usize;
            entries.push(Entry {
                name,
                frames: table.u32_at(at + 32)? as usize,
                bytes,
                offset,
                looping: table.u32_at(at + 48)? & MODE_LOOP != 0,
                loop_range: table.u32_at(at + 40)? as usize..table.u32_at(at + 44)? as usize + 1,
                rate: table.i32_at(at + 52)?.max(0) as u32,
                channels: table.u16_at(at + 62)? as usize,
            });
            offset += bytes;
            at += size.max(1);
        }
        if offset != data_bytes {
            return Err(format!("{}: sample sizes add up to {offset}, header says {data_bytes}", path.display()));
        }
        Ok(Self { path: path.to_owned(), data_start: HEADER + table_bytes, entries })
    }

    /// Reads and decodes one sample.
    pub fn sample(&self, index: usize) -> Result<Sample, String> {
        let entry = self.entries.get(index).ok_or_else(|| format!("{} has no sample {index}", self.path.display()))?;
        let fail = |e: std::io::Error| format!("{}: {e}", self.path.display());
        let mut file = File::open(&self.path).map_err(fail)?;
        file.seek(SeekFrom::Start((self.data_start + entry.offset) as u64)).map_err(fail)?;
        let mut raw = vec![0u8; entry.bytes];
        file.read_exact(&mut raw).map_err(fail)?;
        let (mut data, channels, rate) = decode(unpad(&raw)?).map_err(|e| format!("{} in {}: {e}", entry.name, self.path.display()))?;
        // The decoder returns whole frames; the header says how much of that is the sound.
        if entry.frames > 0 {
            data.truncate(entry.frames * channels);
        }
        let frames = data.len() / channels.max(1);
        let loop_range = if entry.loop_range.start < entry.loop_range.end && entry.loop_range.end <= frames {
            entry.loop_range.clone()
        } else {
            0..frames
        };
        Ok(Sample { rate, channels, data, looping: entry.looping, loop_range })
    }
}
