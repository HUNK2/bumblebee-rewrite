//! Scaleform `.gfx` movies (the game's HUD, `UI\Flash\hud.gfx`): a Flash 8 file with the
//! signature `GFX`, read far enough to draw its vector shapes as the timelines place them.
//!
//! Read: shapes (solid, linear and radial fills), sprites with their frames (placements,
//! removals, labels, whether frame 1 stops), masks (clip depths), colour transforms, the
//! exported names. Bitmap fill IDs and matrices are retained, but the CPU movie renderer
//! does not sample them yet. Not read: strokes, text, morph shapes, filters, and scripts:
//! whoever draws a movie plays the part of its classes through
//! `Pose` (`work\gfx_dump.py` lists and disassembles what is in a file).

use std::collections::HashMap;

use super::Bytes;

/// x' = a x + c y + tx, y' = b x + d y + ty, in pixels (a twip is 1 / 20).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Affine {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub tx: f32,
    pub ty: f32,
}

impl Affine {
    pub const IDENTITY: Affine = Affine { a: 1.0, b: 0.0, c: 0.0, d: 1.0, tx: 0.0, ty: 0.0 };

    pub fn scale_then_move(scale: f32, tx: f32, ty: f32) -> Self {
        Affine { a: scale, b: 0.0, c: 0.0, d: scale, tx, ty }
    }

    pub fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        (self.a * x + self.c * y + self.tx, self.b * x + self.d * y + self.ty)
    }

    /// `self` after `inner`.
    pub fn then(&self, inner: &Affine) -> Affine {
        let (tx, ty) = self.apply(inner.tx, inner.ty);
        Affine {
            a: self.a * inner.a + self.c * inner.b,
            b: self.b * inner.a + self.d * inner.b,
            c: self.a * inner.c + self.c * inner.d,
            d: self.b * inner.c + self.d * inner.d,
            tx,
            ty,
        }
    }

    pub fn inverse(&self) -> Option<Affine> {
        let det = self.a * self.d - self.b * self.c;
        if det.abs() < 1e-12 {
            return None;
        }
        let (a, b, c, d) = (self.d / det, -self.b / det, -self.c / det, self.a / det);
        Some(Affine { a, b, c, d, tx: -(a * self.tx + c * self.ty), ty: -(b * self.tx + d * self.ty) })
    }
}

/// A colour transform: each of red, green, blue, alpha (0 to 1) times `mul` plus `add`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Tint {
    pub mul: [f32; 4],
    pub add: [f32; 4],
}

impl Tint {
    pub const NONE: Tint = Tint { mul: [1.0; 4], add: [0.0; 4] };

    pub fn apply(&self, colour: [f32; 4]) -> [f32; 4] {
        std::array::from_fn(|i| (colour[i] * self.mul[i] + self.add[i]).clamp(0.0, 1.0))
    }

    /// `self` after `inner`.
    pub fn then(&self, inner: &Tint) -> Tint {
        Tint { mul: std::array::from_fn(|i| self.mul[i] * inner.mul[i]), add: std::array::from_fn(|i| inner.add[i] * self.mul[i] + self.add[i]) }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Fill {
    Solid([f32; 4]),
    /// The gradient's square (819.2 pixels either way) into the shape's space, and the
    /// stops: where (0 to 1) and the colour.
    Linear(Affine, Vec<(f32, [f32; 4])>),
    Radial(Affine, Vec<(f32, [f32; 4])>),
    /// An external bitmap fill, referenced by its image character id in the paired APK.
    /// The general movie renderer does not sample these yet; icon consumers can resolve
    /// the reference and use the original texture directly.
    Bitmap { id: u16, matrix: Affine, repeat: bool, smooth: bool },
    /// A non-bitmap fill kind this reader does not render.
    Other,
}

/// One edge: a line, or a curve by its control point. `fill0` is the fill on its left,
/// `fill1` the one on its right, as places in the shape's `fills` + 1; 0 is none.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Edge {
    pub from: (f32, f32),
    pub control: Option<(f32, f32)>,
    pub to: (f32, f32),
    pub fill0: u32,
    pub fill1: u32,
}

#[derive(Clone, Default, PartialEq, Debug)]
pub struct Shape {
    /// Left, top, right, bottom.
    pub bounds: [f32; 4],
    pub fills: Vec<Fill>,
    pub edges: Vec<Edge>,
}

/// A `PlaceObject`: what goes at a depth, or what changes about what is there.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct Place {
    pub depth: u16,
    /// It changes the object at the depth; else it puts a new one there.
    pub moves: bool,
    pub id: Option<u16>,
    pub matrix: Option<Affine>,
    pub tint: Option<Tint>,
    pub name: Option<String>,
    /// It is a mask for the depths above it up to this one.
    pub clip: Option<u16>,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Step {
    Place(Place),
    Remove(u16),
}

#[derive(Clone, Default, PartialEq, Debug)]
pub struct Sprite {
    pub frames: Vec<Vec<Step>>,
    /// A label and its frame, counted from 1.
    pub labels: Vec<(String, usize)>,
    /// Frame 1's script has a `stop`: the timeline waits for a script to move it.
    pub stops: bool,
}

impl Sprite {
    pub fn label(&self, name: &str) -> Option<usize> {
        self.labels.iter().find(|(label, _)| label == name).map(|(_, frame)| *frame)
    }

    /// What is shown once frames 1 to `frame` have run, by depth.
    pub fn shown(&self, frame: usize) -> Vec<Place> {
        let mut shown: Vec<Place> = Vec::new();
        for steps in self.frames.iter().take(frame.max(1)) {
            for step in steps {
                match step {
                    Step::Remove(depth) => shown.retain(|p| p.depth != *depth),
                    Step::Place(place) => {
                        let old = shown.iter().position(|p| p.depth == place.depth);
                        match (old, place.moves) {
                            (Some(at), true) => {
                                let item = &mut shown[at];
                                item.id = place.id.or(item.id);
                                item.matrix = place.matrix.or(item.matrix);
                                item.tint = place.tint.or(item.tint);
                                item.name = place.name.clone().or(item.name.take());
                                item.clip = place.clip.or(item.clip);
                            }
                            (Some(at), false) => shown[at] = place.clone(),
                            (None, _) => shown.push(place.clone()),
                        }
                    }
                }
            }
        }
        shown.sort_by_key(|p| p.depth);
        shown
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Character {
    Shape(Shape),
    Sprite(Sprite),
    Other,
}

pub struct Movie {
    /// The stage in pixels and the frames a second.
    pub stage: [f32; 2],
    pub rate: f32,
    pub characters: HashMap<u16, Character>,
    pub exports: HashMap<String, u16>,
}

struct Bits<'a> {
    data: &'a [u8],
    at: usize,
    bit: u32,
    short: bool,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8], at: usize) -> Self {
        Self { data, at, bit: 0, short: false }
    }

    fn unsigned(&mut self, count: u32) -> u32 {
        let mut value = 0;
        for _ in 0..count {
            let Some(byte) = self.data.get(self.at) else {
                self.short = true;
                return 0;
            };
            value = (value << 1) | u32::from((byte >> (7 - self.bit)) & 1);
            self.bit += 1;
            if self.bit == 8 {
                (self.bit, self.at) = (0, self.at + 1);
            }
        }
        value
    }

    fn signed(&mut self, count: u32) -> i32 {
        let value = self.unsigned(count);
        if count > 0 && count < 32 && value >> (count - 1) != 0 { value as i32 - (1 << count) } else { value as i32 }
    }

    fn align(&mut self) -> usize {
        if self.bit != 0 {
            (self.bit, self.at) = (0, self.at + 1);
        }
        self.at
    }

    fn byte(&mut self) -> u8 {
        self.align();
        let value = self.data.get(self.at).copied();
        self.short |= value.is_none();
        self.at += 1;
        value.unwrap_or(0)
    }

    fn word(&mut self) -> u16 {
        u16::from(self.byte()) | (u16::from(self.byte()) << 8)
    }

    fn rect(&mut self) -> [f32; 4] {
        self.align();
        let count = self.unsigned(5);
        let v: [f32; 4] = std::array::from_fn(|_| self.signed(count) as f32 / 20.0);
        self.align();
        [v[0], v[2], v[1], v[3]]
    }

    fn matrix(&mut self) -> Affine {
        self.align();
        let mut m = Affine::IDENTITY;
        if self.unsigned(1) != 0 {
            let count = self.unsigned(5);
            m.a = self.signed(count) as f32 / 65536.0;
            m.d = self.signed(count) as f32 / 65536.0;
        }
        if self.unsigned(1) != 0 {
            let count = self.unsigned(5);
            m.b = self.signed(count) as f32 / 65536.0;
            m.c = self.signed(count) as f32 / 65536.0;
        }
        let count = self.unsigned(5);
        m.tx = self.signed(count) as f32 / 20.0;
        m.ty = self.signed(count) as f32 / 20.0;
        self.align();
        m
    }

    fn tint(&mut self) -> Tint {
        self.align();
        let (has_add, has_mul) = (self.unsigned(1) != 0, self.unsigned(1) != 0);
        let count = self.unsigned(4);
        let mut tint = Tint::NONE;
        if has_mul {
            tint.mul = std::array::from_fn(|_| self.signed(count) as f32 / 256.0);
        }
        if has_add {
            tint.add = std::array::from_fn(|_| self.signed(count) as f32 / 255.0);
        }
        self.align();
        tint
    }

    fn colour(&mut self, alpha: bool) -> [f32; 4] {
        let rgb: [f32; 3] = std::array::from_fn(|_| f32::from(self.byte()) / 255.0);
        [rgb[0], rgb[1], rgb[2], if alpha { f32::from(self.byte()) / 255.0 } else { 1.0 }]
    }

    fn string(&mut self) -> String {
        let mut text = Vec::new();
        loop {
            match self.byte() {
                0 => break,
                _ if self.short => break,
                c => text.push(c),
            }
        }
        String::from_utf8_lossy(&text).into_owned()
    }
}

/// The tags from `at` to `end`: the code, where its body starts and the body's length.
fn tags(data: &[u8], mut at: usize, end: usize) -> Vec<(u16, usize, usize)> {
    let mut out = Vec::new();
    while at + 2 <= end {
        let Ok(head) = data.u16_at(at) else { break };
        at += 2;
        let (code, mut size) = (head >> 6, usize::from(head & 63));
        if size == 63 {
            let Ok(long) = data.u32_at(at) else { break };
            (size, at) = (long as usize, at + 4);
        }
        if at + size > end {
            break;
        }
        out.push((code, at, size));
        at += size;
        if code == 0 {
            break;
        }
    }
    out
}

fn fill_style(bits: &mut Bits, code: u16) -> Fill {
    let alpha = code >= 32;
    match bits.byte() {
        0 => Fill::Solid(bits.colour(alpha)),
        kind @ (0x10 | 0x12 | 0x13) => {
            let matrix = bits.matrix();
            let count = bits.byte() & 15;
            let stops = (0..count).map(|_| (f32::from(bits.byte()) / 255.0, bits.colour(alpha))).collect();
            if kind == 0x13 {
                bits.word();
            }
            if kind == 0x10 { Fill::Linear(matrix, stops) } else { Fill::Radial(matrix, stops) }
        }
        kind @ (0x40..=0x43) => {
            let id = bits.word();
            let matrix = bits.matrix();
            Fill::Bitmap { id, matrix, repeat: kind == 0x40 || kind == 0x42, smooth: kind == 0x40 || kind == 0x41 }
        }
        _ => {
            bits.word();
            bits.matrix();
            Fill::Other
        }
    }
}

fn styles(bits: &mut Bits, code: u16, fills: &mut Vec<Fill>) {
    let mut count = usize::from(bits.byte());
    if count == 255 {
        count = usize::from(bits.word());
    }
    for _ in 0..count {
        let fill = fill_style(bits, code);
        fills.push(fill);
    }
    let mut count = usize::from(bits.byte());
    if count == 255 {
        count = usize::from(bits.word());
    }
    for _ in 0..count {
        bits.word();
        if code == 83 {
            // A `LINESTYLE2`: the join, whether it has a fill of its own, the caps.
            let flags = bits.byte();
            bits.byte();
            if (flags >> 4) & 3 == 2 {
                bits.word();
            }
            if flags & 8 != 0 {
                fill_style(bits, code);
            } else {
                bits.colour(true);
            }
        } else {
            bits.colour(code >= 32);
        }
    }
}

fn shape(data: &[u8], at: usize, code: u16) -> Result<Shape, String> {
    let mut bits = Bits::new(data, at + 2);
    let mut shape = Shape { bounds: bits.rect(), ..Default::default() };
    if code == 83 {
        bits.rect();
        bits.byte();
    }
    styles(&mut bits, code, &mut shape.fills);
    bits.align();
    let (mut fill_bits, mut line_bits) = (bits.unsigned(4), bits.unsigned(4));
    // A record may bring new styles: its fills are counted from where the old ones end.
    let mut base = 0;
    let (mut x, mut y) = (0.0f32, 0.0f32);
    let (mut fill0, mut fill1) = (0, 0);
    loop {
        if bits.short {
            return Err("a shape runs past its tag".into());
        }
        if bits.unsigned(1) == 0 {
            let flags = bits.unsigned(5);
            if flags == 0 {
                break;
            }
            if flags & 1 != 0 {
                let count = bits.unsigned(5);
                x = bits.signed(count) as f32 / 20.0;
                y = bits.signed(count) as f32 / 20.0;
            }
            if flags & 2 != 0 {
                let style = bits.unsigned(fill_bits);
                fill0 = if style == 0 { 0 } else { base + style };
            }
            if flags & 4 != 0 {
                let style = bits.unsigned(fill_bits);
                fill1 = if style == 0 { 0 } else { base + style };
            }
            if flags & 8 != 0 {
                bits.unsigned(line_bits);
            }
            if flags & 16 != 0 {
                base = shape.fills.len() as u32;
                bits.align();
                styles(&mut bits, code, &mut shape.fills);
                bits.align();
                (fill_bits, line_bits) = (bits.unsigned(4), bits.unsigned(4));
                (fill0, fill1) = (0, 0);
            }
        } else if bits.unsigned(1) != 0 {
            let count = bits.unsigned(4) + 2;
            let from = (x, y);
            if bits.unsigned(1) != 0 {
                x += bits.signed(count) as f32 / 20.0;
                y += bits.signed(count) as f32 / 20.0;
            } else if bits.unsigned(1) != 0 {
                y += bits.signed(count) as f32 / 20.0;
            } else {
                x += bits.signed(count) as f32 / 20.0;
            }
            shape.edges.push(Edge { from, control: None, to: (x, y), fill0, fill1 });
        } else {
            let count = bits.unsigned(4) + 2;
            let from = (x, y);
            let control = (x + bits.signed(count) as f32 / 20.0, y + bits.signed(count) as f32 / 20.0);
            x = control.0 + bits.signed(count) as f32 / 20.0;
            y = control.1 + bits.signed(count) as f32 / 20.0;
            shape.edges.push(Edge { from, control: Some(control), to: (x, y), fill0, fill1 });
        }
    }
    Ok(shape)
}

fn place(data: &[u8], at: usize, code: u16) -> Place {
    let mut bits = Bits::new(data, at);
    let flags = bits.byte();
    let more = if code == 70 { bits.byte() } else { 0 };
    let mut place = Place { depth: bits.word(), moves: flags & 1 != 0, ..Default::default() };
    if more & 8 != 0 {
        bits.string();
    }
    if flags & 2 != 0 {
        place.id = Some(bits.word());
    }
    if flags & 4 != 0 {
        place.matrix = Some(bits.matrix());
    }
    if flags & 8 != 0 {
        place.tint = Some(bits.tint());
    }
    if flags & 16 != 0 {
        bits.word();
    }
    if flags & 32 != 0 {
        place.name = Some(bits.string());
    }
    if flags & 64 != 0 {
        place.clip = Some(bits.word());
    }
    place
}

/// Whether a frame script's own actions (not its functions') have a `stop`.
fn has_stop(data: &[u8]) -> bool {
    let mut at = 0;
    while let Some(&op) = data.get(at) {
        at += 1;
        if op == 0 {
            break;
        }
        if op == 0x07 {
            return true;
        }
        if op >= 0x80 {
            let Ok(size) = data.u16_at(at) else { break };
            let body = at + 2;
            at = body + usize::from(size);
            // A function's code follows its header: step over it.
            if op == 0x9b || op == 0x8e {
                if let Ok(code) = data.u16_at(at.saturating_sub(2)) {
                    at += usize::from(code);
                }
            }
        }
    }
    false
}

fn sprite(data: &[u8], at: usize, size: usize) -> Sprite {
    let mut sprite = Sprite::default();
    let mut frame = Vec::new();
    for (code, body, length) in tags(data, at + 4, at + size) {
        match code {
            26 | 70 => frame.push(Step::Place(place(data, body, code))),
            28 => frame.push(Step::Remove(data.u16_at(body).unwrap_or(0))),
            43 => sprite.labels.push((Bits::new(data, body).string(), sprite.frames.len() + 1)),
            12 if sprite.frames.is_empty() => sprite.stops |= has_stop(&data[body..body + length]),
            1 => sprite.frames.push(std::mem::take(&mut frame)),
            _ => {}
        }
    }
    sprite
}

impl Movie {
    pub fn parse(file: &[u8]) -> Result<Movie, String> {
        if file.get(..3) != Some(b"GFX") && file.get(..3) != Some(b"FWS") {
            return Err("not an uncompressed .gfx or .swf movie".into());
        }
        let data = file.get(8..).ok_or("a movie with no body")?;
        let mut bits = Bits::new(data, 0);
        let stage = bits.rect();
        let rate = f32::from(data.u16_at(bits.at)?) / 256.0;
        let mut movie = Movie { stage: [stage[2] - stage[0], stage[3] - stage[1]], rate, characters: HashMap::new(), exports: HashMap::new() };
        for (code, at, size) in tags(data, bits.at + 4, data.len()) {
            match code {
                2 | 22 | 32 | 83 => {
                    movie.characters.insert(data.u16_at(at)?, Character::Shape(shape(data, at, code)?));
                }
                39 => {
                    movie.characters.insert(data.u16_at(at)?, Character::Sprite(sprite(data, at, size)));
                }
                37 | 46 | 11 | 33 | 7 | 34 | 1001 => {
                    movie.characters.insert(data.u16_at(at)?, Character::Other);
                }
                56 => {
                    let mut bits = Bits::new(data, at);
                    for _ in 0..bits.word() {
                        let id = bits.word();
                        movie.exports.insert(bits.string(), id);
                    }
                }
                _ => {}
            }
        }
        Ok(movie)
    }

    pub fn sprite(&self, id: u16) -> Option<&Sprite> {
        match self.characters.get(&id)? {
            Character::Sprite(sprite) => Some(sprite),
            _ => None,
        }
    }
}

/// What the movie's scripts would have done to an instance, by its path of instance
/// names from the sprite drawn (`Meter_Animation/Meter/Mask`).
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Tweak {
    /// The frame it is on, counted from 1.
    pub frame: Option<usize>,
    /// Its `_x` and `_y`, in place of its placement's.
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub hidden: bool,
}

pub type Pose = HashMap<String, Tweak>;

/// A picture being drawn: red, green, blue and alpha of each pixel, the colours already
/// times the alpha.
#[derive(Clone)]
pub struct Canvas {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<[f32; 4]>,
}

/// Sub-scanlines to a pixel row.
const ROWS: usize = 4;

impl Canvas {
    pub fn new(width: usize, height: usize) -> Self {
        Self { width, height, pixels: vec![[0.0; 4]; width * height] }
    }

    fn over(&mut self, at: usize, colour: [f32; 4], coverage: f32) {
        let alpha = colour[3] * coverage;
        if alpha <= 0.0 {
            return;
        }
        let pixel = &mut self.pixels[at];
        for i in 0..3 {
            pixel[i] = colour[i] * alpha + pixel[i] * (1.0 - alpha);
        }
        pixel[3] = alpha + pixel[3] * (1.0 - alpha);
    }

    /// Red, green, blue, alpha bytes with the colours not multiplied by the alpha.
    pub fn rgba8(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pixels.len() * 4);
        for pixel in &self.pixels {
            let alpha = pixel[3];
            for value in &pixel[..3] {
                out.push(if alpha > 0.0 { (value / alpha * 255.0).round().clamp(0.0, 255.0) as u8 } else { 0 });
            }
            out.push((alpha * 255.0).round().clamp(0.0, 255.0) as u8);
        }
        out
    }

    /// One fill of a shape: the edges with it on one side only, filled between crossings.
    fn fill(&mut self, segments: &[[f32; 4]], mut paint: impl FnMut(f32, f32) -> [f32; 4]) {
        if segments.is_empty() || self.width == 0 {
            return;
        }
        let top = segments.iter().map(|s| s[1].min(s[3])).fold(f32::MAX, f32::min).floor().max(0.0) as usize;
        let bottom = (segments.iter().map(|s| s[1].max(s[3])).fold(f32::MIN, f32::max).ceil().max(0.0) as usize).min(self.height);
        let mut coverage = vec![0.0f32; self.width];
        let mut crossings = Vec::new();
        for row in top..bottom {
            coverage.fill(0.0);
            let mut any = false;
            for sub in 0..ROWS {
                let y = row as f32 + (sub as f32 + 0.5) / ROWS as f32;
                crossings.clear();
                for s in segments {
                    let (y0, y1) = (s[1].min(s[3]), s[1].max(s[3]));
                    if y >= y0 && y < y1 {
                        crossings.push(s[0] + (s[2] - s[0]) * (y - s[1]) / (s[3] - s[1]));
                    }
                }
                crossings.sort_by(f32::total_cmp);
                for pair in crossings.chunks_exact(2) {
                    let (x0, x1) = (pair[0].clamp(0.0, self.width as f32), pair[1].clamp(0.0, self.width as f32));
                    if x1 <= x0 {
                        continue;
                    }
                    any = true;
                    for x in x0.floor() as usize..(x1.ceil() as usize).min(self.width) {
                        coverage[x] += (x1.min(x as f32 + 1.0) - x0.max(x as f32)).max(0.0) / ROWS as f32;
                    }
                }
            }
            if !any {
                continue;
            }
            for (x, &covered) in coverage.iter().enumerate() {
                if covered > 0.0 {
                    self.over(row * self.width + x, paint(x as f32 + 0.5, row as f32 + 0.5), covered.min(1.0));
                }
            }
        }
    }
}

fn gradient(stops: &[(f32, [f32; 4])], t: f32) -> [f32; 4] {
    let Some(first) = stops.first() else { return [0.0; 4] };
    if t <= first.0 {
        return first.1;
    }
    for pair in stops.windows(2) {
        if t <= pair[1].0 {
            let k = (t - pair[0].0) / (pair[1].0 - pair[0].0).max(1e-6);
            return std::array::from_fn(|i| pair[0].1[i] + (pair[1].1[i] - pair[0].1[i]) * k);
        }
    }
    stops[stops.len() - 1].1
}

/// Half the side of the square a gradient is defined on: 16384 twips.
const GRADIENT: f32 = 819.2;

impl Movie {
    fn draw_shape(&self, shape: &Shape, to_canvas: &Affine, tint: &Tint, canvas: &mut Canvas) {
        for (index, fill) in shape.fills.iter().enumerate() {
            let style = index as u32 + 1;
            let mut segments = Vec::new();
            for edge in shape.edges.iter().filter(|e| (e.fill0 == style) != (e.fill1 == style)) {
                let mut from = to_canvas.apply(edge.from.0, edge.from.1);
                let parts = if edge.control.is_some() { 8 } else { 1 };
                for part in 1..=parts {
                    let t = part as f32 / parts as f32;
                    let (x, y) = match edge.control {
                        Some(c) => {
                            let u = 1.0 - t;
                            (u * u * edge.from.0 + 2.0 * u * t * c.0 + t * t * edge.to.0, u * u * edge.from.1 + 2.0 * u * t * c.1 + t * t * edge.to.1)
                        }
                        None => edge.to,
                    };
                    let to = to_canvas.apply(x, y);
                    if from.1 != to.1 {
                        segments.push([from.0, from.1, to.0, to.1]);
                    }
                    from = to;
                }
            }
            match fill {
                Fill::Solid(colour) => {
                    let colour = tint.apply(*colour);
                    canvas.fill(&segments, |_, _| colour);
                }
                Fill::Linear(matrix, stops) | Fill::Radial(matrix, stops) => {
                    let Some(back) = to_canvas.then(matrix).inverse() else { continue };
                    let radial = matches!(fill, Fill::Radial(..));
                    canvas.fill(&segments, |x, y| {
                        let (gx, gy) = back.apply(x, y);
                        let t = if radial { (gx * gx + gy * gy).sqrt() / GRADIENT } else { (gx + GRADIENT) / (2.0 * GRADIENT) };
                        tint.apply(gradient(stops, t.clamp(0.0, 1.0)))
                    });
                }
                Fill::Bitmap { .. } | Fill::Other => {}
            }
        }
    }

    /// Draws a character as `to_canvas` places it. `clock` is the seconds a timeline that
    /// plays by itself has been playing (they loop); `path` is the instance's own path,
    /// which `pose` is looked up by.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(&self, id: u16, to_canvas: &Affine, tint: &Tint, pose: &Pose, path: &str, clock: f32, canvas: &mut Canvas) {
        match self.characters.get(&id) {
            Some(Character::Shape(shape)) => self.draw_shape(shape, to_canvas, tint, canvas),
            Some(Character::Sprite(sprite)) => {
                let frame = match pose.get(path).and_then(|tweak| tweak.frame) {
                    Some(frame) => frame,
                    None if sprite.stops || sprite.frames.len() < 2 => 1,
                    None => (clock * self.rate) as usize % sprite.frames.len() + 1,
                };
                let shown = sprite.shown(frame);
                let mut at = 0;
                while at < shown.len() {
                    let item = &shown[at];
                    at += 1;
                    let Some(clip) = item.clip else {
                        self.draw_placed(item, to_canvas, tint, pose, path, clock, canvas);
                        continue;
                    };
                    // A mask: what lies above it up to its clip depth shows only where
                    // the mask has a shape, whatever the shape's colour or opacity.
                    let mut mask = Canvas::new(canvas.width, canvas.height);
                    let solid = Tint { mul: [0.0; 4], add: [1.0; 4] };
                    self.draw_placed(item, to_canvas, &solid, pose, path, clock, &mut mask);
                    let mut layer = Canvas::new(canvas.width, canvas.height);
                    while at < shown.len() && shown[at].depth <= clip {
                        self.draw_placed(&shown[at], to_canvas, tint, pose, path, clock, &mut layer);
                        at += 1;
                    }
                    for ((pixel, layer), mask) in canvas.pixels.iter_mut().zip(&layer.pixels).zip(&mask.pixels) {
                        let alpha = layer[3] * mask[3];
                        if alpha > 0.0 {
                            for i in 0..3 {
                                pixel[i] = layer[i] * mask[3] + pixel[i] * (1.0 - alpha);
                            }
                            pixel[3] = alpha + pixel[3] * (1.0 - alpha);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_placed(&self, item: &Place, to_canvas: &Affine, tint: &Tint, pose: &Pose, path: &str, clock: f32, canvas: &mut Canvas) {
        let Some(id) = item.id else { return };
        let name = item.name.clone().unwrap_or_else(|| format!("#{}", item.depth));
        let child = if path.is_empty() { name } else { format!("{path}/{name}") };
        let tweak = pose.get(&child).copied().unwrap_or_default();
        if tweak.hidden {
            return;
        }
        let mut matrix = item.matrix.unwrap_or(Affine::IDENTITY);
        matrix.tx = tweak.x.unwrap_or(matrix.tx);
        matrix.ty = tweak.y.unwrap_or(matrix.ty);
        let tint = tint.then(&item.tint.unwrap_or(Tint::NONE));
        self.draw(id, &to_canvas.then(&matrix), &tint, pose, &child, clock, canvas);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(left: f32, top: f32, right: f32, bottom: f32, fill: Fill) -> Shape {
        let corners = [(left, top), (right, top), (right, bottom), (left, bottom)];
        let edges = (0..4).map(|i| Edge { from: corners[i], control: None, to: corners[(i + 1) % 4], fill0: 0, fill1: 1 }).collect();
        Shape { bounds: [left, top, right, bottom], fills: vec![fill], edges }
    }

    fn movie(characters: Vec<(u16, Character)>) -> Movie {
        Movie { stage: [16.0, 16.0], rate: 30.0, characters: characters.into_iter().collect(), exports: HashMap::new() }
    }

    #[test]
    fn a_square_covers_its_pixels_and_half_covers_its_edges() {
        let movie = movie(vec![(1, Character::Shape(square(2.0, 2.0, 6.5, 6.0, Fill::Solid([1.0, 0.0, 0.0, 1.0]))))]);
        let mut canvas = Canvas::new(8, 8);
        movie.draw(1, &Affine::IDENTITY, &Tint::NONE, &Pose::new(), "", 0.0, &mut canvas);
        assert_eq!(canvas.pixels[3 * 8 + 3], [1.0, 0.0, 0.0, 1.0]);
        assert!((canvas.pixels[3 * 8 + 6][3] - 0.5).abs() < 1e-5);
        assert_eq!(canvas.pixels[3 * 8 + 7][3], 0.0);
        assert_eq!(canvas.pixels[8 + 3][3], 0.0);
        assert_eq!(&canvas.rgba8()[(3 * 8 + 6) * 4..][..4], &[255, 0, 0, 128]);
    }

    #[test]
    fn a_mask_and_a_pose_cut_what_is_above_them() {
        // Like the game's meters: a bar under a mask that a script slides to the left.
        let bar = Character::Shape(square(0.0, 0.0, 8.0, 4.0, Fill::Solid([0.0, 1.0, 0.0, 1.0])));
        let cover = Character::Shape(square(0.0, 0.0, 8.0, 4.0, Fill::Solid([1.0, 0.0, 0.0, 1.0])));
        let mask = Character::Sprite(Sprite { frames: vec![vec![Step::Place(Place { depth: 1, id: Some(2), ..Default::default() })]], ..Default::default() });
        let meter = Character::Sprite(Sprite {
            frames: vec![vec![
                Step::Place(Place { depth: 1, id: Some(3), name: Some("Mask".into()), clip: Some(4), ..Default::default() }),
                Step::Place(Place { depth: 3, id: Some(1), ..Default::default() }),
            ]],
            ..Default::default()
        });
        let movie = movie(vec![(1, bar), (2, cover), (3, mask), (4, meter)]);
        let mut pose = Pose::new();
        pose.insert("Mask".into(), Tweak { x: Some(-4.0), ..Default::default() });
        let mut canvas = Canvas::new(8, 4);
        movie.draw(4, &Affine::IDENTITY, &Tint::NONE, &pose, "", 0.0, &mut canvas);
        assert_eq!(canvas.pixels[8 + 2], [0.0, 1.0, 0.0, 1.0]);
        assert_eq!(canvas.pixels[8 + 5], [0.0; 4]);
    }

    #[test]
    fn a_gradient_runs_across_its_square_and_a_tint_changes_it() {
        // The square scaled to 8 pixels wide, its middle at x = 4.
        let matrix = Affine { a: 8.0 / (2.0 * GRADIENT), d: 1.0, tx: 4.0, ..Affine::IDENTITY };
        let stops = vec![(0.0, [0.0, 0.0, 0.0, 1.0]), (1.0, [1.0, 1.0, 1.0, 1.0])];
        let movie = movie(vec![(1, Character::Shape(square(0.0, 0.0, 8.0, 2.0, Fill::Linear(matrix, stops))))]);
        let mut canvas = Canvas::new(8, 2);
        movie.draw(1, &Affine::IDENTITY, &Tint::NONE, &Pose::new(), "", 0.0, &mut canvas);
        assert!((canvas.pixels[2][0] - 2.5 / 8.0).abs() < 1e-4);
        let mut tinted = Canvas::new(8, 2);
        let tint = Tint { mul: [0.0, 0.0, 0.0, 0.5], add: [1.0, 0.0, 0.0, 0.0] };
        movie.draw(1, &Affine::IDENTITY, &tint, &Pose::new(), "", 0.0, &mut tinted);
        assert_eq!(tinted.pixels[2], [0.5, 0.0, 0.0, 0.5]);
    }

    #[test]
    fn a_timeline_keeps_what_a_move_does_not_change() {
        let sprite = Sprite {
            frames: vec![
                vec![Step::Place(Place { depth: 4, id: Some(9), name: Some("a".into()), ..Default::default() })],
                vec![Step::Place(Place { depth: 4, moves: true, matrix: Some(Affine { tx: 5.0, ..Affine::IDENTITY }), ..Default::default() })],
                vec![Step::Remove(4)],
            ],
            ..Default::default()
        };
        let second = sprite.shown(2);
        assert_eq!((second[0].id, second[0].name.as_deref(), second[0].matrix.unwrap().tx), (Some(9), Some("a"), 5.0));
        assert!(sprite.shown(3).is_empty());
    }

    #[test]
    fn the_games_hud_reads_as_the_dump_has_it() {
        // Needs the install.
        let Ok(file) = std::fs::read(r"C:\Games2\UI\Flash\hud.gfx") else { return };
        let movie = Movie::parse(&file).unwrap();
        assert_eq!((movie.stage, movie.rate), ([1280.0, 720.0], 30.0));
        let health = movie.sprite(movie.exports["Health_Meter Class"]).unwrap();
        assert_eq!((health.frames.len(), health.label("main")), (166, Some(59)));
        // The bar's mask is 111 x 23, and the bar under it plays `low_loop` from frame 2.
        let Character::Shape(mask) = &movie.characters[&55] else { panic!() };
        assert_eq!((mask.bounds, mask.edges.len()), ([0.0, 0.0, 111.0, 23.0], 4));
        let meter = movie.sprite(59).unwrap();
        assert!(meter.stops && meter.label("low_loop") == Some(2));
        assert!(!movie.sprite(66).unwrap().stops);
        // Drawn whole, the bar's middle is its colour; with the mask slid away, nothing.
        let mut canvas = Canvas::new(111, 23);
        movie.draw(59, &Affine::IDENTITY, &Tint::NONE, &Pose::new(), "", 0.0, &mut canvas);
        let middle = canvas.pixels[11 * 111 + 55];
        assert!((middle[0] - 187.0 / 255.0).abs() < 1e-3 && middle[3] == 1.0, "{middle:?}");
        let mut pose = Pose::new();
        pose.insert("Mask".into(), Tweak { x: Some(-111.0), ..Default::default() });
        let mut empty = Canvas::new(111, 23);
        movie.draw(59, &Affine::IDENTITY, &Tint::NONE, &pose, "", 0.0, &mut empty);
        assert_eq!(empty.pixels[11 * 111 + 55][3], 0.0);
    }
}
