//! The game's particle system, the library its classes call `aps` (`notes\particles.md`).
//!
//! A `ParticleTemplate` is a list of elements; an element is a renderer and a list of
//! actions. A running effect keeps a group of particles for each element. Each update of a
//! group (`FUN_00a9ff00`): the source actions at the head of the list add particles, the
//! rest are applied to every particle, every age goes up, and what the lifetime action
//! marked is removed. An action runs only while the EFFECT's clock is between its
//! `StartTime` and `EndTime`. [game]
//!
//! Read from disassembly and ported: the update's order, the sources (`Brst`, `SrcA`,
//! `Ejct`, `RSpn`) and what each of their domain slots sets, the actions `LnSc`, `LScW`,
//! `LScH`, `ExSc`, `AlFd`, `AFIO`, `RFIO`, `ClSh`, `Move`, `Forc`, `VlDr`, `AVDr`, `PAtr`,
//! `UVFr`, `AngT`, `Life`, the domains `1dPt`, `3dPt`, `1dBx`, `3dBx`, `1dBu`, `3dBu`,
//! `Crcl`, `Disc`, `Sphe`, `SpSu`, `Line`, and the random number generator. Anything else
//! in a template (`Intt`, the damage smoke's source, in his pack) is kept by its code and
//! does nothing (`Template::unread`). How a particle is drawn is read too: the quad
//! builders and the particle shaders that are compiled into the executable (see `Sprite`,
//! `Renderer` and `notes\particles.md` "Drawing").

use glam::{Affine3A, Vec3};

use crate::formats::lxb::Node;

/// The library's random numbers (`DAT_00d44940`): a 31-bit congruential generator whose
/// top 23 bits make a float in 0..1. [game] What the seed is when the game starts is not
/// read; it is shared by every effect, so no effect's numbers are repeatable there either.
#[derive(Clone, Copy, Debug)]
pub struct Rng(pub u32);

impl Rng {
    pub fn unit(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(0xc1c6_4e6d).wrapping_add(0x3039) & 0x7fff_ffff;
        f32::from_bits(self.0 >> 8 | 0x3f80_0000) - 1.0
    }
}

const fn code(text: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*text)
}

/// Where a source takes a value from. One number uses `x` only.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Domain {
    /// `1dPt`, `3dPt`: the value itself.
    Point(Vec3),
    /// `1dBx`, `3dBx`: evenly between the two, each axis on its own (`FUN_0095ccb0`).
    Box { min: Vec3, max: Vec3 },
    /// `1dBu`, `3dBu`: between the two by `u * 2 * (1 - u)` for u under a half and
    /// `u * 2 * (u - 1) + 1` over it (`FUN_0095d750`).
    Bump { min: Vec3, max: Vec3 },
    /// `Crcl`: on the rim of a circle, at an angle drawn evenly (`FUN_00a9bd00`).
    /// [assumed: which way the angle's zero points; it makes no difference to an even draw]
    Circle { center: Vec3, normal: Vec3, radius: f32 },
    /// `Disc`: inside a circle (`FUN_00a9bfa0`): two numbers drawn in the square round it,
    /// drawn again while they fall outside, a hundred times at most. [game]
    /// [assumed: which two directions across the normal the numbers go along; an even
    /// draw looks the same either way]
    Disc { center: Vec3, normal: Vec3, radius: f32 },
    /// `Sphe` (`FUN_00a9c710`): x and y drawn between minus and plus the radius, z between
    /// NOTHING and the radius, drawn again while outside the sphere, thirty times at
    /// most. So it is the upper half of a ball, as the code has it. [game]
    Sphere { center: Vec3, radius: f32 },
    /// `SpSu` (`FUN_00a9c310`): three numbers between -1 and 1, made a unit vector, times
    /// the radius. (Not even over the sphere: it leans to a cube's corners.) [game]
    SphereSurface { center: Vec3, radius: f32 },
    /// `Line` (`FUN_00a9bb40`): evenly between the two ends, one number for all three.
    Line { min: Vec3, max: Vec3 },
    /// A kind not read (the hemispheres; none in his pack): gives the slot's default.
    Unread(u32),
}

impl Domain {
    fn read(node: Node) -> Option<Self> {
        let id = node.get("id").and_then(Node::int)? as u32;
        let vec = |name: &str| {
            let v = node.get(name).map(Node::floats).unwrap_or_default();
            Vec3::new(v.first().copied().unwrap_or(0.0), v.get(1).copied().unwrap_or(0.0), v.get(2).copied().unwrap_or(0.0))
        };
        Some(match &id.to_be_bytes() {
            b"NULL" => return None,
            b"1dPt" | b"3dPt" => Domain::Point(vec("Val")),
            b"1dBx" | b"3dBx" => Domain::Box { min: vec("Min"), max: vec("Max") },
            b"1dBu" | b"3dBu" => Domain::Bump { min: vec("Min"), max: vec("Max") },
            b"Crcl" => Domain::Circle { center: vec("Center"), normal: vec("Normal"), radius: vec("Radius").x },
            b"Disc" => Domain::Disc { center: vec("Center"), normal: vec("Normal"), radius: vec("Radius").x },
            b"Sphe" => Domain::Sphere { center: vec("Center"), radius: vec("Radius").x },
            b"SpSu" => Domain::SphereSurface { center: vec("Center"), radius: vec("Radius").x },
            b"Line" => Domain::Line { min: vec("Min"), max: vec("Max") },
            _ => Domain::Unread(id),
        })
    }

    /// One number. Draws one random number for a box or a bump.
    fn number(&self, rng: &mut Rng) -> Option<f32> {
        match *self {
            Domain::Point(v) => Some(v.x),
            Domain::Box { min, max } => Some(min.x + (max.x - min.x) * rng.unit()),
            Domain::Bump { min, max } => Some(min.x + (max.x - min.x) * bump(rng.unit())),
            _ => None,
        }
    }

    /// Three numbers, drawn x then y then z.
    fn vector(&self, rng: &mut Rng) -> Option<Vec3> {
        match *self {
            Domain::Point(v) => Some(v),
            Domain::Box { min, max } => {
                let u = Vec3::new(rng.unit(), rng.unit(), rng.unit());
                Some(min + (max - min) * u)
            }
            Domain::Bump { min, max } => {
                let u = Vec3::new(bump(rng.unit()), bump(rng.unit()), bump(rng.unit()));
                Some(min + (max - min) * u)
            }
            Domain::Circle { center, normal, radius } => {
                let angle = rng.unit() * std::f32::consts::TAU;
                let normal = normal.try_normalize().unwrap_or(Vec3::Z);
                let across = normal.any_orthonormal_vector();
                let (sin, cos) = angle.sin_cos();
                Some(center + (across * cos + normal.cross(across) * sin) * radius)
            }
            Domain::Disc { center, normal, radius } => {
                let (mut a, mut b) = (0.0, 0.0);
                for _ in 0..100 {
                    a = rng.unit() * (radius + radius) - radius;
                    b = rng.unit() * (radius + radius) - radius;
                    if radius * radius >= a * a + b * b {
                        break;
                    }
                }
                let normal = normal.try_normalize().unwrap_or(Vec3::Z);
                let across = normal.any_orthonormal_vector();
                Some(center + across * b + normal.cross(across) * a)
            }
            Domain::Sphere { center, radius } => {
                let mut v = Vec3::ZERO;
                for _ in 0..30 {
                    let x = rng.unit() * (radius + radius) - radius;
                    let y = rng.unit() * (radius + radius) - radius;
                    let z = rng.unit() * radius;
                    v = Vec3::new(x, y, z);
                    if radius * radius > v.length_squared() {
                        break;
                    }
                }
                Some(center + v)
            }
            Domain::SphereSurface { center, radius } => {
                let mut draw = || rng.unit() * 2.0 - 1.0;
                let v = Vec3::new(draw(), draw(), draw());
                Some(center + v.try_normalize().unwrap_or(v) * radius)
            }
            Domain::Line { min, max } => Some(min + (max - min) * rng.unit()),
            Domain::Unread(_) => None,
        }
    }
}

fn bump(u: f32) -> f32 {
    if u >= 0.5 { u * 2.0 * (u - 1.0) + 1.0 } else { u * 2.0 * (1.0 - u) }
}

/// The 16 domain slots of a source, by what each sets on a new particle (`FUN_00ac5c20`):
/// 0 the place, 1 `radius`, 2 `width`, 3 the colour, 4 the alpha, 5 a facing, 6 the turn,
/// 7 `height`, 8 the picture's frame, 9 the lifetime, 10 the most alpha a random fade
/// reaches, 11 the velocity, 12 the turning rate, 13 to 15 values nothing ported uses.
/// [game]
pub type Slots = [Option<Domain>; 16];

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// `Brst`: all at once, the first update it runs, and never again (`FUN_00ac5910`).
    Burst { count: f32, slots: Box<Slots> },
    /// `SrcA`: so many a second, the fraction carried over (`FUN_00ac3470`).
    Source { rate: f32, slots: Box<Slots> },
    /// `Intt` (the game's own; count `FUN_0083ab60`, emit `FUN_00839a00`): `SrcA` that
    /// goes on and off. A clock on the group runs down; each time it is out a time is
    /// drawn from the first domain (going on) or the second (going off) and it changes
    /// over. It starts off with the clock at 0, so it goes on at the first update. The
    /// other domains are `SrcA`'s slots, two further on (`FUN_00839830`). [game]
    /// [assumed: the clock starts at 0 and off; the constructor remains unresolved]
    Intermittent { rate: f32, on: Option<Domain>, off: Option<Domain>, slots: Box<Slots> },
    /// `LnSc`, `LScW`, `LScH`: the radius, width or height grows by this much a second
    /// (`FUN_00ac0b60`, `FUN_00ac6f00`). [assumed for `LScW`: its code, `FUN_00ac70d0`,
    /// is not read; it is taken to be `LScH` for the width]
    LinearScale { delta: f32 },
    LinearScaleWidth { delta: f32 },
    LinearScaleHeight { delta: f32 },
    /// `AlFd`: `begin_alpha` until the age is `fade_begin` of the lifetime, then evenly
    /// down to nothing at the lifetime (`FUN_00ac3310`).
    AlphaFade { begin_alpha: f32, fade_begin: f32 },
    /// `AFIO`: up to `max_alpha` over the first `end_fade_in` of the lifetime, down from
    /// `begin_fade_out` on (`FUN_00ac3090`).
    AlphaFadeInOut { end_fade_in: f32, max_alpha: f32, begin_fade_out: f32 },
    /// `RFIO`: the same with the particle's own most (slot 10) (`FUN_00ac74c0`).
    RandomAlphaFadeInOut { end_fade_in: f32, begin_fade_out: f32 },
    /// `Move`: the place by the velocity, the turn by the turning rate (`FUN_00ac0ab0`).
    Move,
    /// `Life`: a particle older than its lifetime is removed (`FUN_00ac3240`).
    Life,
    /// `Ejct` (the game's own, not the library's; count `FUN_008394e0`, emit
    /// `FUN_00838740`): a steady source for an emitter that moves. Nothing is made in an
    /// update the emitter moved less than `min_speed` in (a distance in ONE update, for
    /// all its name); what is made is spread evenly from the emitter's last place to this
    /// one and leaves with the emitter's velocity times `speed_coeff`. Its domain slots
    /// are 13 and have no facing and no velocity; they are kept here in a `Brst`'s
    /// places. [game]
    Eject { rate: f32, speed_coeff: f32, min_speed: f32, slots: Box<Slots> },
    /// `RSpn` (count `FUN_00ac4390`): one particle each time a clock runs out, the next
    /// wait drawn from its seventeenth domain; ten in an update at most. `EmissionRate`
    /// is not used. [game] [assumed: a particle is made as `SrcA` makes one; its own
    /// routine, `FUN_00ac46b0`, is the same size and was not gone through]
    RandomSpawn { wait: Option<Domain>, slots: Box<Slots> },
    /// `ExSc` (`FUN_00ac73c0`): the radius times `1 + x + x * x / 2`, x the step times
    /// the logarithm of `Exponent`.
    ExponentialScale { exponent: f32 },
    /// `ClSh` (`FUN_00ac6910`): while the age is between `begin` and `end` of the
    /// lifetime, the colour is that far from the one to the other.
    ColorShift { begin: f32, from: Vec3, end: f32, to: Vec3 },
    /// `Forc` (`FUN_00acc500`): the velocity gains this much a second; with `local` the
    /// push is turned by the emitter's frame first.
    Force { acc: Vec3, local: bool },
    /// `VlDr`, `AVDr` (`FUN_00ac6af0`, `FUN_00ac6820`): the velocity, or the turning rate,
    /// times `1 + x + x * x / 2 + x * x * x / 6`, x minus `Exponent` times the step.
    VelocityDrag { exponent: f32 },
    AngularVelocityDrag { exponent: f32 },
    /// `PAtr` (`FUN_00acb5f0`): pulled to a point by `force` over the distance squared
    /// (no nearer than a squared distance of 0.001).
    PointAttractor { center: Vec3, force: f32 },
    /// `UVFr` (`FUN_00ac71a0`): the frame goes on by `fps` a second, or with `cycles`
    /// over 0 by `cycles` times the range over the lifetime, and wraps inside
    /// `begin` .. `end`.
    FrameAnim { begin: f32, end: f32, fps: f32, cycles: f32 },
    /// `AngT` (`FUN_00acbde0`): the turn is set from the way the velocity points in the
    /// picture. Done where the quads are made (`Sprite::along`). [assumed: the picture's
    /// top is what points along it; the routine's angle arithmetic was not followed]
    AngleTrackVelocity,
    /// Not read: does nothing here.
    Unread(u32),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub start: f32,
    pub end: f32,
    pub kind: Kind,
}

impl Action {
    fn read(node: Node) -> Option<Self> {
        let id = node.get("id").and_then(Node::int)? as u32;
        let num = |name: &str| node.get(name).and_then(Node::float).unwrap_or(0.0);
        let slots = || {
            let mut slots: Slots = [None; 16];
            for (slot, domain) in slots.iter_mut().zip(node.get("domains").into_iter().flat_map(|d| d.items())) {
                *slot = Domain::read(domain);
            }
            Box::new(slots)
        };
        // `Ejct`'s thirteen slots, each in the place a `Brst` keeps the same value
        // (`FUN_00838590` against `FUN_00ac59a0`).
        const EJECT: [usize; 13] = [0, 1, 2, 3, 4, 6, 7, 8, 9, 10, 12, 14, 15];
        let kind = match &id.to_be_bytes() {
            b"Brst" => Kind::Burst { count: num("NumParticles"), slots: slots() },
            b"SrcA" => Kind::Source { rate: num("EmissionRate"), slots: slots() },
            b"Intt" => {
                let mut domains = node.get("domains").into_iter().flat_map(|d| d.items());
                let (on, off) = (domains.next().and_then(Domain::read), domains.next().and_then(Domain::read));
                let mut slots: Slots = [None; 16];
                for (slot, domain) in slots.iter_mut().zip(domains) {
                    *slot = Domain::read(domain);
                }
                Kind::Intermittent { rate: num("EmissionRate"), on, off, slots: Box::new(slots) }
            }
            b"Ejct" => {
                let mut slots: Slots = [None; 16];
                for (n, domain) in node.get("domains").into_iter().flat_map(|d| d.items()).enumerate().take(EJECT.len()) {
                    slots[EJECT[n]] = Domain::read(domain);
                }
                Kind::Eject { rate: num("EmissionRate"), speed_coeff: num("SpeedCoeff"), min_speed: num("MinSpeedToSpawn"), slots: Box::new(slots) }
            }
            b"RSpn" => Kind::RandomSpawn { wait: node.get("domains").and_then(|d| d.items().nth(16)).and_then(Domain::read), slots: slots() },
            b"ExSc" => Kind::ExponentialScale { exponent: num("Exponent") },
            b"ClSh" => Kind::ColorShift {
                begin: num("BeginShiftTime"),
                from: Vec3::new(num("BeginRed"), num("BeginGreen"), num("BeginBlue")),
                end: num("EndShiftTime"),
                to: Vec3::new(num("EndRed"), num("EndGreen"), num("EndBlue")),
            },
            b"Forc" => Kind::Force { acc: Vec3::new(num("AccX"), num("AccY"), num("AccZ")), local: num("LocalSpace") != 0.0 },
            b"VlDr" => Kind::VelocityDrag { exponent: num("Exponent") },
            b"AVDr" => Kind::AngularVelocityDrag { exponent: num("Exponent") },
            b"PAtr" => Kind::PointAttractor { center: Vec3::new(num("CenterX"), num("CenterY"), num("CenterZ")), force: num("Force") },
            b"UVFr" => Kind::FrameAnim { begin: num("BeginFrame"), end: num("EndFrame"), fps: num("FPS"), cycles: num("FrameCycles") },
            b"AngT" => Kind::AngleTrackVelocity,
            b"LnSc" => Kind::LinearScale { delta: num("DeltaRadius") },
            b"LScW" => Kind::LinearScaleWidth { delta: num("DeltaWidth") },
            b"LScH" => Kind::LinearScaleHeight { delta: num("DeltaHeight") },
            b"AlFd" => Kind::AlphaFade { begin_alpha: num("BeginAlpha"), fade_begin: num("FadeBegin") },
            b"AFIO" => Kind::AlphaFadeInOut { end_fade_in: num("EndFadeIn"), max_alpha: num("MaxAlpha"), begin_fade_out: num("BeginFadeOut") },
            b"RFIO" => Kind::RandomAlphaFadeInOut { end_fade_in: num("EndFadeIn"), begin_fade_out: num("BeginFadeOut") },
            b"Move" => Kind::Move,
            b"Life" => Kind::Life,
            _ => Kind::Unread(id),
        };
        Some(Action { start: num("StartTime"), end: num("EndTime"), kind })
    }

    fn is_source(&self) -> bool {
        matches!(self.kind, Kind::Burst { .. } | Kind::Source { .. } | Kind::Intermittent { .. } | Kind::Eject { .. } | Kind::RandomSpawn { .. })
    }
}

/// How the picture is put over what is behind it (`FUN_00a9db20`, the render states a
/// `BlendMode` sets). [game]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Blend {
    /// 0 and 3: by the source's alpha, over the rest.
    Alpha,
    /// 1: the source times its alpha, added.
    Add,
    /// 2: the source times its alpha, taken away.
    Subtract,
    /// 4: the source as it is.
    Opaque,
}

/// The builders discard a particle below this raw alpha, before tint/depth fade.
/// [game: 0083aff0, default at 00b8d220; 00971170/00971fc0 compare >=]
pub const MIN_RENDER_ALPHA: f32 = 0.05;

/// Particle vertex shader `FOG`: slope, intercept, minimum and maximum.
/// [game: 009736a0; 00aa0500 supplies a zero minimum]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Fog(pub [f32; 4]);

impl Fog {
    pub fn new(start: f32, end: f32, minimum: f32, maximum: f32) -> Self {
        let slope = if end > start { (maximum - minimum) / (end - start) } else { 0.0 };
        Self([slope, minimum - start * slope, minimum, maximum])
    }

    /// [game: 728708_vs] Clamp camera-space depth's ramp at each vertex.
    pub fn amount(self, depth: f32) -> f32 {
        (depth * self.0[0] + self.0[1]).min(self.0[3]).max(self.0[2])
    }
}

/// How an element's particles are drawn. [data] The classes (`apsBillboardRenderer`...)
/// build the quads in `FUN_00971fc0` and its like: see `Sprite`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Renderer {
    /// The four-letter code: `Blbo`, `CBlb`, `UVA `, `CUVA` face the camera and are sized
    /// by `radius`; `Rect`, `CRct`, `UVAR`, `cUVr` are sized by `width` and `height`.
    pub id: u32,
    /// The texture, by its asset id, and the path it was built from.
    pub texture: u32,
    pub texture_path: String,
    /// `BlendMode`, as stored: his effects have 0, 1 and 3 (the text export prints 0 and 1
    /// as `false` and `true`, names from the wrong type). See `blend`.
    pub blend_mode: u32,
    /// `UVBlending`: the picture is a mix of the frame and the one after it. The shader
    /// always mixes by the frame's fraction (`Sprite::frame`). [assumed: without this the
    /// frame is handed over whole; where the builders use the switch was not found]
    pub uv_blending: bool,
    /// `IsShimmer`, or the renderer `Dstr`: drawn by the distortion shaders, which bend
    /// the picture behind. Not ported: such an element is not drawn.
    pub distortion: bool,
    /// The picture is a sheet of this many frames across and down (1 by 1 when the
    /// renderer has none).
    pub frames: (u32, u32),
    pub velocity_tracked: bool,
    pub normal: Vec3,
    /// `Tint`: red, green, blue, alpha, 0 to 1.
    pub tint: [f32; 4],
    /// `AlphaFadeStart`, `AlphaFadeEnd`: the depths in front of the camera between which
    /// a particle fades in (`FUN_00a9dce0` makes the vertex shader's `ALPHAFADE` of them:
    /// the alpha times `(depth - start) / (end - start)`, kept in 0..1; the two equal
    /// means no fade). [game]
    pub alpha_fade: (f32, f32),
    /// `ZFeatherDistance`: over 0, the particle is drawn by the shaders that fade it out
    /// where it nears what is behind it (`FUN_00973060`). The distance itself goes to
    /// the vertex shader and the pixel shader does not use it: the fade is over a fixed
    /// 0.001 of the depth buffer. [game]
    pub z_feather: f32,
    /// `AmbientCoeff`, `DiffuseCoeff` (bytes over 255) and `BackLitCoeff`: how much of the
    /// level's ambient and diffuse light a LIT particle takes, and how bright it is from
    /// behind (`FUN_00973310` makes the lit pixel shader's constants of them). [game]
    pub ambient: [f32; 3],
    pub diffuse: [f32; 3],
    pub back_lit: f32,
}

impl Renderer {
    /// [game: 00a9b620] Additive/subtractive particles have no fog; modes 0/3/4 do.
    pub fn fogged(&self) -> bool {
        !matches!(self.blend_mode, 1 | 2)
    }

    /// [game: 00a9dce0] ALPHAFADE x/y. Equal endpoints mean no depth fade.
    pub fn fade_constants(&self) -> [f32; 2] {
        let (start, end) = self.alpha_fade;
        if start == end { [0.0, 1.0] } else {
            let slope = 1.0 / (end - start);
            [slope, -start * slope]
        }
    }

    /// Drawn by the lit shaders: `BlendMode` 3 (the renderer's vtable `+0x18`,
    /// `FUN_0095c310`, which `FUN_00acdc00` asks to pick the builder with normals). [game]
    pub fn lit(&self) -> bool {
        self.blend_mode == 3
    }

    /// Whether the particles are sized by `width` and `height` and not by `radius`.
    pub fn rectangle(&self) -> bool {
        [code(b"Rect"), code(b"CRct"), code(b"UVAR"), code(b"cUVr")].contains(&self.id)
    }

    pub fn blend(&self) -> Blend {
        match self.blend_mode {
            1 => Blend::Add,
            2 => Blend::Subtract,
            4 => Blend::Opaque,
            _ => Blend::Alpha,
        }
    }

    fn read(node: Node) -> Self {
        let num = |name: &str| node.get(name).and_then(Node::float).unwrap_or(0.0);
        let int = |name: &str| node.get(name).and_then(Node::int).unwrap_or(0);
        let (texture, texture_path) = node.get("Texture").and_then(Node::asset_text).unwrap_or_default();
        let tint = node.get("Tint").map(Node::ints).filter(|t| t.len() == 4).map_or([1.0; 4], |t| std::array::from_fn(|i| t[i] as f32 / 255.0));
        let normal = node.get("Normal").map(Node::floats).filter(|n| n.len() == 3).map_or(Vec3::ZERO, |n| Vec3::new(n[0], n[1], n[2]));
        let coeff = |name: &str| node.get(name).map(Node::ints).filter(|c| c.len() >= 3).map_or([1.0; 3], |c| std::array::from_fn(|i| c[i] as f32 / 255.0));
        Renderer {
            id: int("id") as u32,
            texture,
            texture_path,
            blend_mode: int("BlendMode") as u32,
            uv_blending: int("UVBlending") == 1,
            distortion: int("IsShimmer") != 0 || int("id") as u32 == code(b"Dstr"),
            frames: (int("WidthFrames").max(1) as u32, int("HeightFrames").max(1) as u32),
            velocity_tracked: int("VelocityTracked") != 0,
            normal,
            tint,
            alpha_fade: (num("AlphaFadeStart"), num("AlphaFadeEnd")),
            z_feather: num("ZFeatherDistance"),
            ambient: coeff("AmbientCoeff"),
            diffuse: coeff("DiffuseCoeff"),
            back_lit: num("BackLitCoeff"),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Element {
    pub max_particles: usize,
    /// The element's group exists from `start` until the first update at or past `end`.
    pub start: f32,
    pub end: f32,
    /// `localspace`: the particles are kept in the emitter's own frame and move with it;
    /// otherwise they are put into the world as they are made.
    pub local_space: bool,
    /// `hAlign`, `vAlign`: where the quad sits on the particle's place (`extents`).
    pub h_align: u32,
    pub v_align: u32,
    pub renderer: Renderer,
    pub actions: Vec<Action>,
}

impl Element {
    /// How far the quad reaches from the particle's place to its picture's left, right,
    /// top and bottom, in half sizes (`FUN_008365f0`, `FUN_00836650`): 1 each way when
    /// centred; an alignment of 1 puts the place on the left (or top) edge, 2 on the
    /// right (or bottom) edge, so `vAlign` 2 stands the quad on its place. [game]
    /// [assumed: the first of the two routines is `hAlign`'s]
    pub fn extents(&self) -> [f32; 4] {
        let pair = |align: u32| match align {
            1 => (0.0, 2.0),
            2 => (2.0, 0.0),
            _ => (1.0, 1.0),
        };
        let ((left, right), (top, bottom)) = (pair(self.h_align), pair(self.v_align));
        [left, right, top, bottom]
    }

    /// Whether an `AngT` turns the particles the way they fly.
    pub fn tracks_angle(&self) -> bool {
        self.actions.iter().any(|a| a.kind == Kind::AngleTrackVelocity)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Template {
    /// The CRC-32 of its name (`r_hit_melee`).
    pub name: u32,
    pub radius: f32,
    pub priority: i32,
    pub elements: Vec<Element>,
}

impl Template {
    pub fn read(node: Node) -> Option<Self> {
        let elements = node.get("elements")?;
        let num = |n: Node, name: &str| n.get(name).and_then(Node::float).unwrap_or(0.0);
        let int = |n: Node, name: &str| n.get(name).and_then(Node::int).unwrap_or(0);
        Some(Template {
            name: node.label().unwrap_or(0),
            radius: num(node, "radius"),
            priority: int(node, "priority") as i32,
            elements: elements
                .items()
                .map(|e| Element {
                    max_particles: int(e, "maxParticles").max(0) as usize,
                    start: num(e, "start"),
                    end: num(e, "end"),
                    // By its number: the value's name is "true" in some of the pack's
                    // embedded files and another type's name in others.
                    local_space: e.get("localspace").is_some_and(|v| v.is_true() || v.int().is_some_and(|n| n != 0)),
                    h_align: int(e, "hAlign") as u32,
                    v_align: int(e, "vAlign") as u32,
                    renderer: e.get("renderer").map(Renderer::read).unwrap_or_default(),
                    actions: e.get("actions").into_iter().flat_map(|a| a.items()).filter_map(Action::read).collect(),
                })
                .collect(),
        })
    }

    /// The codes of the actions and domains in it that are not read, for reports.
    pub fn unread(&self) -> Vec<String> {
        let text = |id: u32| String::from_utf8_lossy(&id.to_be_bytes()).into_owned();
        let mut out = Vec::new();
        for action in self.elements.iter().flat_map(|e| &e.actions) {
            match &action.kind {
                Kind::Unread(id) => out.push(format!("action {}", text(*id))),
                Kind::Burst { slots, .. } | Kind::Source { slots, .. } => {
                    for (n, slot) in slots.iter().enumerate() {
                        match slot {
                            Some(Domain::Unread(id)) => out.push(format!("domain {} in slot {n}", text(*id))),
                            Some(_) if matches!(n, 5 | 13..) => out.push(format!("slot {n}")),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

/// One particle. The game keeps only the values an element's renderer and actions ask
/// for; here each has them all, at the value a source gives one that has no domain.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Particle {
    /// In the emitter's frame for a `localspace` element, in the world otherwise.
    pub pos: Vec3,
    pub radius: f32,
    pub width: f32,
    pub height: f32,
    pub colour: Vec3,
    pub alpha: f32,
    /// In whole turns (the renderer multiplies it by 2 pi). [game]
    pub turn: f32,
    pub frame: f32,
    pub age: f32,
    pub lifetime: f32,
    pub max_alpha: f32,
    pub velocity: Vec3,
    /// Turns a second.
    pub turn_rate: f32,
    dead: bool,
}

/// The particles of one element of a running effect (the library's `apsGroup`).
#[derive(Clone, Debug, Default)]
pub struct Group {
    pub particles: Vec<Particle>,
    /// What the sources carry between updates (group `+0x80`): the fraction of a particle
    /// a steady source still owes, or under zero once a burst has gone off.
    carry: f32,
    /// An `Intt` source's clock and whether it is on.
    gap: (f32, bool),
    /// At or past the element's `end`: removed at the next update.
    done: bool,
}

/// The share of a source's particles that are made (`DAT_00cfff98`): all of a steady
/// source's and of a burst's with the setting on, 0.6 and 0.33 with it off. [game]
/// [assumed: the setting is on; which option sets it is not read]
const BURST_SHARE: f32 = 1.0;
const SOURCE_SHARE: f32 = 1.0;

impl Group {
    /// Makes `count` particles, as far as there is room. `trail` is an `Ejct`'s: how far
    /// the emitter came since the last update, and the velocity its particles leave with;
    /// they are laid along that stretch, the last where the emitter is now.
    fn emit(&mut self, slots: &Slots, count: usize, element: &Element, place: &Affine3A, trail: Option<(Vec3, Vec3)>, rng: &mut Rng) {
        let room = element.max_particles.saturating_sub(self.particles.len());
        let count = count.min(room);
        for made in 0..count {
            // The order is the original's, so the random numbers fall to the same values.
            let number = |slot: usize, rng: &mut Rng, default: f32| slots[slot].as_ref().and_then(|d| d.number(rng)).unwrap_or(default);
            let vector = |slot: usize, rng: &mut Rng, default: Vec3| slots[slot].as_ref().and_then(|d| d.vector(rng)).unwrap_or(default);
            let mut pos = vector(0, rng, Vec3::ZERO);
            if !element.local_space {
                pos = place.transform_point3(pos);
            }
            if let (Some((moved, _)), false) = (trail, element.local_space) {
                pos -= moved * ((count - 1 - made) as f32 / count as f32);
            }
            let radius = number(1, rng, 1.0);
            let width = number(2, rng, 1.0);
            let colour = vector(3, rng, Vec3::ONE);
            let alpha = number(4, rng, 1.0);
            let _facing = vector(5, rng, Vec3::Z);
            let turn = number(6, rng, 1.0);
            let height = number(7, rng, 1.0);
            let frame = number(8, rng, 0.0);
            let lifetime = number(9, rng, 1.0);
            let max_alpha = number(10, rng, 1.0);
            let mut velocity = vector(11, rng, Vec3::ZERO);
            if !element.local_space {
                velocity = place.transform_vector3(velocity);
            }
            if let Some((_, leaving)) = trail {
                velocity = leaving;
            }
            let _ = vector(13, rng, Vec3::ZERO);
            let turn_rate = number(12, rng, 0.0);
            let _ = (number(14, rng, 0.0), number(15, rng, 0.0));
            self.particles.push(Particle { pos, radius, width, height, colour, alpha, turn, frame, age: 0.0, lifetime, max_alpha, velocity, turn_rate, dead: false });
        }
    }

    /// One update at the effect's time `time`. With `emitting` off the sources are passed
    /// over (an effect told to stop).
    /// `moved` is how far the emitter came since the update before.
    fn update(&mut self, element: &Element, time: f32, dt: f32, place: &Affine3A, moved: Vec3, emitting: bool, rng: &mut Rng) {
        let running = |action: &Action| action.start <= time && time <= action.end;
        let sources = element.actions.iter().take_while(|a| a.is_source()).count();
        if emitting {
            for action in element.actions[..sources].iter().filter(|a| running(a)) {
                match &action.kind {
                    Kind::Burst { count, slots } => {
                        if self.carry >= 0.0 {
                            self.carry = -1.0;
                            let count = ((count * BURST_SHARE) as usize).max(1);
                            self.emit(slots, count, element, place, None, rng);
                        }
                    }
                    Kind::Source { rate, slots } => {
                        let owed = rate * SOURCE_SHARE * dt + self.carry;
                        let count = owed as i32;
                        self.carry = owed - count as f32;
                        self.emit(slots, count.max(0) as usize, element, place, None, rng);
                    }
                    Kind::Intermittent { rate, on, off, slots } => {
                        let mut step = dt;
                        self.gap.0 -= dt;
                        if self.gap.0 < 0.0 {
                            // What is left of the update after the change is the step.
                            step = -self.gap.0;
                            let drawn = if self.gap.1 { off } else { on };
                            self.gap.0 += drawn.as_ref().and_then(|d| d.number(rng)).unwrap_or(0.0);
                            self.gap.1 = !self.gap.1;
                        }
                        if self.gap.1 {
                            let owed = rate * step + self.carry;
                            let count = owed as i32;
                            self.carry = owed - count as f32;
                            self.emit(slots, count.max(0) as usize, element, place, None, rng);
                        }
                    }
                    Kind::Eject { rate, speed_coeff, min_speed, slots } => {
                        // The count is taken, and the fraction kept, whether or not the
                        // emitter moved enough for the particles to be made.
                        let owed = rate * SOURCE_SHARE * dt + self.carry;
                        let count = owed as i32;
                        self.carry = owed - count as f32;
                        if count > 0 && *min_speed <= moved.length() {
                            let leaving = moved * (speed_coeff / dt);
                            self.emit(slots, count as usize, element, place, Some((moved, leaving)), rng);
                        }
                    }
                    Kind::RandomSpawn { wait, slots } => {
                        let mut clock = self.carry - dt;
                        let mut count = 0;
                        while clock <= 0.0 {
                            count += 1;
                            if count >= 10 {
                                break;
                            }
                            clock += wait.as_ref().and_then(|d| d.number(rng)).unwrap_or(0.0);
                        }
                        self.carry = clock;
                        self.emit(slots, count, element, place, None, rng);
                    }
                    _ => {}
                }
            }
        }
        for action in element.actions[sources..].iter().filter(|a| running(a)) {
            // What a step does to every particle alike.
            let drag = |exponent: f32| {
                let x = -exponent * dt;
                ((x * (1.0 / 6.0) + 0.5) * x + 1.0) * x + 1.0
            };
            let push = match action.kind {
                Kind::Force { acc, local } => {
                    let step = acc * dt;
                    if local { place.transform_vector3(step) } else { step }
                }
                _ => Vec3::ZERO,
            };
            let grow = match action.kind {
                Kind::ExponentialScale { exponent } => {
                    let x = exponent.ln() * dt;
                    x + 1.0 + x * 0.5 * x
                }
                _ => 1.0,
            };
            for p in &mut self.particles {
                match action.kind {
                    Kind::ExponentialScale { .. } => p.radius *= grow,
                    Kind::ColorShift { begin, from, end, to } => {
                        if p.lifetime * begin <= p.age && p.age <= p.lifetime * end {
                            let share = (p.age - p.lifetime * begin) * (1.0 / (end - begin)) / p.lifetime;
                            p.colour = from + (to - from) * share;
                        }
                    }
                    Kind::Force { .. } => p.velocity += push,
                    Kind::VelocityDrag { exponent } => p.velocity *= drag(exponent),
                    Kind::AngularVelocityDrag { exponent } => p.turn_rate *= drag(exponent),
                    Kind::PointAttractor { center, force } => {
                        let center = if element.local_space { center } else { place.transform_point3(center) };
                        let away = p.pos - center;
                        let squared = away.length_squared().max(0.001);
                        p.velocity -= away / squared.sqrt() * (force * dt / squared);
                    }
                    Kind::FrameAnim { begin, end, fps, cycles } => {
                        let step = if cycles > 0.0 {
                            if p.lifetime > p.age + dt { (end - begin) * cycles * dt / p.lifetime } else { 0.0 }
                        } else {
                            fps * dt
                        };
                        let mut frame = p.frame.max(begin) + step;
                        if frame >= end {
                            frame = frame - end + begin;
                        }
                        if frame < begin {
                            frame = frame + end - begin;
                        }
                        p.frame = frame;
                    }
                    Kind::LinearScale { delta } => p.radius += delta * dt,
                    Kind::LinearScaleWidth { delta } => p.width += delta * dt,
                    Kind::LinearScaleHeight { delta } => p.height += delta * dt,
                    Kind::AlphaFade { begin_alpha, fade_begin } => {
                        p.alpha = if p.age < p.lifetime * fade_begin {
                            begin_alpha
                        } else if p.age < p.lifetime {
                            (1.0 - p.age / p.lifetime) * (begin_alpha / (1.0 - fade_begin))
                        } else {
                            0.0
                        };
                    }
                    Kind::AlphaFadeInOut { end_fade_in, max_alpha, begin_fade_out } => {
                        p.alpha = fade_in_out(p.age / p.lifetime, end_fade_in, begin_fade_out, max_alpha);
                    }
                    Kind::RandomAlphaFadeInOut { end_fade_in, begin_fade_out } => {
                        p.alpha = fade_in_out(p.age / p.lifetime, end_fade_in, begin_fade_out, p.max_alpha);
                    }
                    Kind::Move => {
                        p.pos += p.velocity * dt;
                        p.turn += p.turn_rate * dt;
                    }
                    Kind::Life => p.dead |= p.lifetime < p.age,
                    Kind::Burst { .. } | Kind::Source { .. } | Kind::Intermittent { .. } | Kind::Eject { .. } | Kind::RandomSpawn { .. } | Kind::AngleTrackVelocity | Kind::Unread(_) => {}
                }
            }
        }
        for p in &mut self.particles {
            p.age += dt;
        }
        // The dead are overwritten from the end of the list (`FUN_00a9caf0`).
        let mut i = self.particles.len();
        while i > 0 {
            i -= 1;
            if self.particles[i].dead {
                self.particles.swap_remove(i);
            }
        }
    }
}

fn fade_in_out(share: f32, end_fade_in: f32, begin_fade_out: f32, most: f32) -> f32 {
    if share < end_fade_in {
        share * (most / end_fade_in)
    } else if share < begin_fade_out {
        most
    } else if share < 1.0 {
        (1.0 - share) * (most / (1.0 - begin_fade_out))
    } else {
        0.0
    }
}

/// One particle as it is to be drawn, in the world.
///
/// Read from the quad builders (`FUN_00971fc0` for the plain billboard, `FUN_0097a3c0`
/// for the colour rectangle; the other six are the same routine for other vertex
/// layouts) [game]:
/// - With the camera's left L and up U and the turn t (whole turns in the data, times
///   2 pi here), the picture's top lies along `sin t L + cos t U` and its left edge along
///   `cos t L - sin t U`: a positive turn is anticlockwise in the picture.
/// - The quad reaches `radius` each way; a rectangle `width` to each side and `height`
///   up and down, so both are HALF sizes. Each reach is times the element's alignment
///   (`Element::extents`).
/// - The corner at the top left has the picture's (0, 0), the bottom right (1, 1).
/// - A particle whose raw alpha is below `MIN_RENDER_ALPHA` is left out.
/// - The colour is the tint (`BLENDCOLOR`) times the particle's; the frame goes to the
///   shader with its fraction, which mixes that much of the next frame in.
///
/// [assumed]: that the two camera vectors are left and up (the names are the distortion
/// shader's; the routine that makes them, `FUN_00a9e0c0`, was not followed to the end);
/// for a renderer that tracks the velocity, that `height` lies along it and the
/// picture's top points along it (the branch was read only as far as its vectors: the
/// velocity's direction, and across it and the line of sight); a rectangle with a
/// `Normal` lying in the plane across it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sprite {
    pub pos: Vec3,
    /// Half the quad's size across and along.
    pub half: (f32, f32),
    /// Radians, anticlockwise in the picture.
    pub turn: f32,
    /// The way its length lies, for a renderer that follows the velocity or an element
    /// with an `AngT`.
    pub along: Option<Vec3>,
    pub colour: [f32; 4],
    /// The cell of the renderer's sheet, and how far on the way to the next it is.
    pub frame: f32,
}

/// A running effect (the library's system, `FUN_00a9f190`).
#[derive(Clone, Debug)]
pub struct Effect {
    pub time: f32,
    groups: Vec<Option<Group>>,
    stopping: bool,
    /// Where the emitter was at the update before (the group's `+0x50`).
    last: Option<Vec3>,
}

impl Effect {
    pub fn new(template: &Template) -> Self {
        Effect { time: 0.0, groups: vec![None; template.elements.len()], stopping: false, last: None }
    }

    /// Whether it has been told to stop.
    pub fn stopping(&self) -> bool {
        self.stopping
    }

    /// No more particles are made; what there is lives out its time.
    pub fn stop(&mut self) {
        self.stopping = true;
    }

    /// One update with the emitter at `place`. An element's group is made at the first
    /// update inside its `start` and `end`, updated at once, marked at the first update at
    /// or past `end` and removed, particles and all, at the one after. [game]
    /// [assumed: the time an element's actions are checked against is the effect's own;
    /// every element of the effects built so far starts at 0, where the two are the same]
    pub fn step(&mut self, template: &Template, place: &Affine3A, dt: f32, rng: &mut Rng) {
        self.time += dt;
        let time = self.time;
        let here = Vec3::from(place.translation);
        // [assumed: at its first update the emitter has come no way]
        let moved = here - self.last.unwrap_or(here);
        self.last = Some(here);
        for (slot, element) in self.groups.iter_mut().zip(&template.elements) {
            match slot {
                None => {
                    if self.stopping || time < element.start || time >= element.end {
                        continue;
                    }
                    *slot = Some(Group::default());
                }
                Some(group) if group.done => {
                    *slot = None;
                    continue;
                }
                Some(_) => {}
            }
            let group = slot.as_mut().unwrap();
            group.update(element, time, dt, place, moved, !self.stopping, rng);
            // A stopped effect's group is done with once its last particle is gone: its
            // element may have no end. [assumed: how the original clears one away]
            group.done = time >= element.end || (self.stopping && group.particles.is_empty());
        }
    }

    /// Nothing is left of it and nothing more will come.
    pub fn finished(&self, template: &Template) -> bool {
        self.groups.iter().zip(&template.elements).all(|(group, element)| group.is_none() && (self.stopping || self.time >= element.end))
    }

    pub fn group(&self, element: usize) -> Option<&Group> {
        self.groups.get(element)?.as_ref()
    }

    /// What element `element` shows now, with the emitter at `place`.
    pub fn sprites(&self, template: &Template, element: usize, place: &Affine3A) -> Vec<Sprite> {
        let (Some(group), Some(e)) = (self.group(element), template.elements.get(element)) else { return Vec::new() };
        let r = &e.renderer;
        let tracked = r.velocity_tracked || e.tracks_angle();
        group
            .particles
            .iter()
            .filter(|p| p.alpha >= MIN_RENDER_ALPHA)
            .map(|p| {
                let (pos, velocity) = if e.local_space { (place.transform_point3(p.pos), place.transform_vector3(p.velocity)) } else { (p.pos, p.velocity) };
                let half = if r.rectangle() { (p.width, p.height) } else { (p.radius, p.radius) };
                let frame = p.frame.max(0.0);
                Sprite {
                    pos,
                    half,
                    turn: p.turn * std::f32::consts::TAU,
                    along: tracked.then(|| velocity.try_normalize()).flatten(),
                    colour: [r.tint[0] * p.colour.x, r.tint[1] * p.colour.y, r.tint[2] * p.colour.z, r.tint[3] * p.alpha],
                    frame: if r.uv_blending { frame } else { frame.floor() },
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;

    fn slots(set: &[(usize, Domain)]) -> Box<Slots> {
        let mut slots: Slots = [None; 16];
        for &(n, d) in set {
            slots[n] = Some(d);
        }
        Box::new(slots)
    }

    fn one(v: f32) -> Domain {
        Domain::Point(Vec3::splat(v))
    }

    fn always(kind: Kind) -> Action {
        Action { start: 0.0, end: f32::MAX, kind }
    }

    /// The flash of `r_hit_melee`: a burst of 2 into room for 1, radius 5 growing by 7 a
    /// second, alive 0.2 s, whole until half its life and then fading.
    fn flash() -> Template {
        Template {
            elements: vec![Element {
                max_particles: 1,
                start: 0.0,
                end: 0.3,
                local_space: true,
                actions: vec![
                    Action { start: 0.0, end: 0.1, kind: Kind::Burst { count: 2.0, slots: slots(&[(1, one(5.0)), (9, one(0.2))]) } },
                    always(Kind::LinearScale { delta: 7.0 }),
                    always(Kind::AlphaFade { begin_alpha: 1.0, fade_begin: 0.5 }),
                    always(Kind::Life),
                ],
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn the_random_numbers_are_the_games() {
        // seed * 0xc1c64e6d + 0x3039, 31 bits kept, the top 23 of them the fraction.
        let mut rng = Rng(0);
        assert_eq!(rng.unit(), f32::from_bits(0x3039 >> 8 | 0x3f80_0000) - 1.0);
        assert_eq!(rng.0, 0x3039);
        let next = 0x3039u32.wrapping_mul(0xc1c6_4e6d).wrapping_add(0x3039) & 0x7fff_ffff;
        let u = rng.unit();
        assert_eq!(rng.0, next);
        assert!((0.0..1.0).contains(&u));
        // A bump leans to the middle: its ends map to 0 and 1 and a quarter to 3/8.
        assert_eq!((bump(0.0), bump(0.25), bump(0.5), bump(0.75)), (0.0, 0.375, 0.5, 0.625));
    }

    #[test]
    fn a_burst_goes_off_once_and_fills_only_the_room_there_is() {
        let t = flash();
        let mut effect = Effect::new(&t);
        let mut rng = Rng(1);
        effect.step(&t, &Affine3A::IDENTITY, DT, &mut rng);
        let group = effect.group(0).unwrap();
        assert_eq!(group.particles.len(), 1);
        let p = group.particles[0];
        // Made and then scaled and aged in the same update.
        assert!((p.radius - (5.0 + 7.0 * DT)).abs() < 1e-5 && p.age == DT && p.alpha == 1.0, "{p:?}");
        // No second one while the first lives, and none after it is gone.
        let mut most = 0;
        let mut seen_fading = false;
        while !effect.finished(&t) {
            effect.step(&t, &Affine3A::IDENTITY, DT, &mut rng);
            if let Some(p) = effect.group(0).and_then(|g| g.particles.first()) {
                // Whole for the first 0.1 s, then down to nothing at 0.2.
                if p.age - DT < 0.1 {
                    assert_eq!(p.alpha, 1.0);
                } else {
                    seen_fading = true;
                    let expect = (1.0 - (p.age - DT) / 0.2) * 2.0;
                    assert!((p.alpha - expect.max(0.0)).abs() < 1e-4, "{p:?}");
                }
            }
            most = most.max(effect.group(0).map_or(0, |g| g.particles.len()));
        }
        assert!(seen_fading && most == 1);
        // The element's group goes one update after its end, 0.3 s.
        assert!((effect.time - (0.3 + DT)).abs() < 2.0 * DT, "{}", effect.time);
    }

    #[test]
    fn a_particle_dies_the_update_after_its_age_passes_its_lifetime() {
        let t = flash();
        let mut effect = Effect::new(&t);
        let mut rng = Rng(1);
        let mut updates = 0;
        loop {
            effect.step(&t, &Affine3A::IDENTITY, DT, &mut rng);
            updates += 1;
            if effect.group(0).is_none_or(|g| g.particles.is_empty()) {
                break;
            }
        }
        // The lifetime action sees the age before the update adds to it: with 0.2 s and
        // sixtieths, the age first exceeds 0.2 after 13 updates, and the 14th removes it
        // (rounding may make it the 13th or 14th).
        assert!((13..=14).contains(&updates), "{updates}");
    }

    #[test]
    fn a_steady_source_carries_its_fraction() {
        // 20 a second for half a second: one every third sixtieth, ten in all, were there
        // room for them; with lives of two updates there is.
        let t = Template {
            elements: vec![Element {
                max_particles: 2,
                end: 0.6,
                local_space: true,
                actions: vec![Action { start: 0.0, end: 0.5, kind: Kind::Source { rate: 20.0, slots: slots(&[(9, one(DT))]) } }, always(Kind::Life)],
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut effect = Effect::new(&t);
        let mut rng = Rng(1);
        let (mut made, mut before) = (0, 0);
        while !effect.finished(&t) {
            effect.step(&t, &Affine3A::IDENTITY, DT, &mut rng);
            let now = effect.group(0).map_or(0, |g| g.particles.iter().filter(|p| p.age == DT).count());
            made += now;
            before = before.max(effect.group(0).map_or(0, |g| g.particles.len()));
        }
        assert!((9..=10).contains(&made), "{made}");
        assert!(before <= 2);
    }

    #[test]
    fn an_intermittent_source_goes_on_and_off() {
        // 60 a second, on for a tenth of a second and off for four tenths: about six
        // particles in each half second, all in its first fifth.
        let t = Template {
            elements: vec![Element {
                max_particles: 100,
                end: 2.0,
                local_space: true,
                actions: vec![Action {
                    start: 0.0,
                    end: 2.0,
                    kind: Kind::Intermittent { rate: 60.0, on: Some(one(0.1)), off: Some(one(0.4)), slots: slots(&[(9, one(10.0))]) },
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut effect = Effect::new(&t);
        let mut rng = Rng(1);
        let mut made = Vec::new();
        for _ in 0..60 {
            let before = effect.group(0).map_or(0, |g| g.particles.len());
            effect.step(&t, &Affine3A::IDENTITY, DT, &mut rng);
            made.push(effect.group(0).map_or(0, |g| g.particles.len()) - before);
        }
        let (first, second) = (made[..30].iter().sum::<usize>(), made[30..].iter().sum::<usize>());
        assert!((5..=7).contains(&first) && (5..=7).contains(&second), "{first} {second}");
        assert_eq!(made[8..29].iter().sum::<usize>(), 0, "nothing while it is off");
    }

    #[test]
    fn world_particles_stay_where_they_were_made_and_local_ones_follow() {
        let sparks = |local_space| Template {
            elements: vec![Element {
                max_particles: 3,
                end: 1.0,
                local_space,
                actions: vec![
                    Action { start: 0.0, end: 0.1, kind: Kind::Burst { count: 3.0, slots: slots(&[(9, one(0.5)), (11, Domain::Point(Vec3::X * 6.0))]) } },
                    always(Kind::Move),
                    always(Kind::Life),
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        for local in [false, true] {
            let t = sparks(local);
            let mut effect = Effect::new(&t);
            let mut rng = Rng(1);
            let first = Affine3A::from_translation(Vec3::new(10.0, 0.0, 0.0));
            effect.step(&t, &first, 0.1, &mut rng);
            // The emitter moves away; the particles have flown 0.6 along x.
            let moved = Affine3A::from_translation(Vec3::new(10.0, 50.0, 0.0));
            let sprites = effect.sprites(&t, 0, &moved);
            assert_eq!(sprites.len(), 3);
            let expect = if local { Vec3::new(10.6, 50.0, 0.0) } else { Vec3::new(10.6, 0.0, 0.0) };
            assert!(sprites.iter().all(|s| (s.pos - expect).length() < 1e-4), "{sprites:?}");
        }
    }

    #[test]
    fn a_circle_gives_its_rim() {
        let ring = Domain::Circle { center: Vec3::new(0.0, 0.0, 4.0), normal: Vec3::Z, radius: 30.0 };
        let mut rng = Rng(7);
        for _ in 0..50 {
            let v = ring.vector(&mut rng).unwrap();
            assert!((v.z - 4.0).abs() < 1e-4 && (v.truncate().length() - 30.0).abs() < 1e-3, "{v}");
        }
    }

    #[test]
    fn the_fades_in_and_out_follow_the_share_of_the_life() {
        // `AFIO` of the strong punch's bolts: in until 0.5, out from 0.6.
        assert_eq!(fade_in_out(0.25, 0.5, 0.6, 1.0), 0.5);
        assert_eq!(fade_in_out(0.55, 0.5, 0.6, 1.0), 1.0);
        assert!((fade_in_out(0.8, 0.5, 0.6, 1.0) - 0.5).abs() < 1e-6);
        assert_eq!(fade_in_out(1.0, 0.5, 0.6, 1.0), 0.0);
        // `RFIO` with a particle whose most is 0.4.
        assert!((fade_in_out(0.15, 0.3, 0.4, 0.4) - 0.2).abs() < 1e-6);
    }

    #[test]
    fn the_round_domains_stay_inside_their_shapes() {
        let mut rng = Rng(11);
        let center = Vec3::new(1.0, 2.0, 3.0);
        let disc = Domain::Disc { center, normal: Vec3::Z, radius: 3.0 };
        let ball = Domain::Sphere { center, radius: 2.0 };
        let shell = Domain::SphereSurface { center, radius: 15.0 };
        let line = Domain::Line { min: Vec3::new(0.0, 0.0, -0.4), max: Vec3::ZERO };
        let mut low = f32::MAX;
        for _ in 0..200 {
            let v = disc.vector(&mut rng).unwrap() - center;
            assert!(v.z.abs() < 1e-5 && v.length() <= 3.0 + 1e-4, "{v}");
            // The ball is its upper half only: z is drawn from nothing up.
            let v = ball.vector(&mut rng).unwrap() - center;
            assert!(v.length() < 2.0 + 1e-4 && v.z >= 0.0, "{v}");
            low = low.min(v.z);
            let v = shell.vector(&mut rng).unwrap() - center;
            assert!((v.length() - 15.0).abs() < 1e-3, "{v}");
            let v = line.vector(&mut rng).unwrap();
            assert!(v.x == 0.0 && v.y == 0.0 && (-0.4..=0.0).contains(&v.z), "{v}");
        }
        assert!(low < 0.2);
    }

    #[test]
    fn an_ejector_lays_its_particles_along_the_way_the_emitter_came() {
        // 60 a second from an emitter doing 30 a second along x: two in a thirtieth, one
        // half way and one at the emitter, each leaving at a quarter of its speed.
        let t = Template {
            elements: vec![Element {
                max_particles: 8,
                end: 10.0,
                actions: vec![always(Kind::Eject { rate: 60.0, speed_coeff: 0.25, min_speed: 0.5, slots: slots(&[(9, one(5.0))]) }), always(Kind::Life)],
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut effect = Effect::new(&t);
        let mut rng = Rng(1);
        let dt = 1.0 / 30.0;
        // The first update has no way come: nothing, though the count is taken.
        effect.step(&t, &Affine3A::IDENTITY, dt, &mut rng);
        assert_eq!(effect.group(0).unwrap().particles.len(), 0);
        effect.step(&t, &Affine3A::from_translation(Vec3::X), dt, &mut rng);
        let made = &effect.group(0).unwrap().particles;
        assert_eq!(made.len(), 2);
        assert!((made[0].pos - Vec3::X * 0.5).length() < 1e-5 && (made[1].pos - Vec3::X).length() < 1e-5, "{made:?}");
        assert!(made.iter().all(|p| (p.velocity - Vec3::X * 7.5).length() < 1e-4), "{made:?}");
        // Standing still (under the least way in an update) it makes none.
        effect.step(&t, &Affine3A::from_translation(Vec3::X * 1.2), dt, &mut rng);
        assert_eq!(effect.group(0).unwrap().particles.len(), 2);
    }

    #[test]
    fn the_later_actions_do_what_their_code_does() {
        let particle = Particle {
            pos: Vec3::ZERO,
            radius: 2.0,
            width: 1.0,
            height: 1.0,
            colour: Vec3::ONE,
            alpha: 1.0,
            turn: 0.0,
            frame: 0.0,
            age: 0.5,
            lifetime: 1.0,
            max_alpha: 1.0,
            velocity: Vec3::new(4.0, 0.0, 0.0),
            turn_rate: 2.0,
            dead: false,
        };
        let run = |kind: Kind| {
            let element = Element { max_particles: 1, end: 10.0, local_space: true, actions: vec![always(kind)], ..Default::default() };
            let mut group = Group { particles: vec![particle], ..Default::default() };
            group.update(&element, 0.1, 0.1, &Affine3A::IDENTITY, Vec3::ZERO, true, &mut Rng(1));
            group.particles[0]
        };
        // Drag: the first four terms of e to the minus 0.8 x 0.1.
        let x: f32 = -0.08;
        let factor = 1.0 + x + x * x / 2.0 + x * x * x / 6.0;
        assert!((run(Kind::VelocityDrag { exponent: 0.8 }).velocity.x - 4.0 * factor).abs() < 1e-6);
        assert!((run(Kind::AngularVelocityDrag { exponent: 0.8 }).turn_rate - 2.0 * factor).abs() < 1e-6);
        // Growth: the first three terms of 2.3 to the 0.1.
        let x = 2.3f32.ln() * 0.1;
        assert!((run(Kind::ExponentialScale { exponent: 2.3 }).radius - 2.0 * (1.0 + x + x * x / 2.0)).abs() < 1e-6);
        // Half way through its life, a shift over the whole life is half done.
        let shifted = run(Kind::ColorShift { begin: 0.0, from: Vec3::new(1.0, 0.9, 0.7), end: 1.0, to: Vec3::new(0.8, 0.7, 0.0) });
        assert!((shifted.colour - Vec3::new(0.9, 0.8, 0.35)).length() < 1e-5, "{shifted:?}");
        // A shift that is over leaves the colour alone.
        assert_eq!(run(Kind::ColorShift { begin: 0.0, from: Vec3::ZERO, end: 0.4, to: Vec3::ZERO }).colour, Vec3::ONE);
        assert!((run(Kind::Force { acc: Vec3::new(0.0, 0.0, -9.0), local: false }).velocity - Vec3::new(4.0, 0.0, -0.9)).length() < 1e-6);
        // Frames: 30 a second, wrapping inside 0 .. 2; or one pass of 8 over the life.
        assert!((run(Kind::FrameAnim { begin: 0.0, end: 2.0, fps: 30.0, cycles: 0.0 }).frame - 1.0).abs() < 1e-5);
        assert!((run(Kind::FrameAnim { begin: 0.0, end: 8.0, fps: 0.0, cycles: 1.0 }).frame - 0.8).abs() < 1e-5);
    }

    #[test]
    fn a_random_spawner_waits_what_its_last_domain_says() {
        let t = Template {
            elements: vec![Element {
                max_particles: 50,
                end: 10.0,
                local_space: true,
                actions: vec![always(Kind::RandomSpawn { wait: Some(one(0.25)), slots: slots(&[(9, one(100.0))]) }), always(Kind::Life)],
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut effect = Effect::new(&t);
        let mut rng = Rng(1);
        for _ in 0..60 {
            effect.step(&t, &Affine3A::IDENTITY, DT, &mut rng);
        }
        // One at once and one every quarter second after.
        let made = effect.group(0).unwrap().particles.len();
        assert!((4..=5).contains(&made), "{made}");
    }

    #[test]
    fn the_alignments_move_the_quad_off_its_place() {
        let at = |h_align, v_align| Element { h_align, v_align, ..Default::default() }.extents();
        assert_eq!(at(0, 0), [1.0, 1.0, 1.0, 1.0]);
        // `vAlign` 2: all of it above its place.
        assert_eq!(at(0, 2), [1.0, 1.0, 2.0, 0.0]);
        assert_eq!(at(1, 1), [0.0, 2.0, 0.0, 2.0]);
    }

    #[test]
    fn fog_uses_depth_and_caps_at_the_authored_amount_without_touching_alpha() {
        let fog = Fog::new(164.0, 700.0, 0.0, 0.95);
        assert_eq!(fog.amount(-20.0), 0.0);
        assert_eq!(fog.amount(164.0), 0.0);
        assert!((fog.amount(432.0) - 0.475).abs() < 1e-6);
        assert_eq!(fog.amount(900.0), 0.95);
        assert_eq!(Fog::new(10.0, 10.0, 0.2, 0.8).amount(100.0), 0.2);
        assert_eq!((0..=4).map(|blend_mode| Renderer { blend_mode, ..Default::default() }.fogged()).collect::<Vec<_>>(),
            vec![true, false, false, true, true]);
        assert_eq!(Renderer::default().fade_constants(), [0.0, 1.0]);
        assert_eq!(Renderer { alpha_fade: (2.0, 6.0), ..Default::default() }.fade_constants(), [0.25, -0.5]);
    }

    #[test]
    fn alpha_cutoff_precedes_tint_and_does_not_remove_live_simulated_particles() {
        let t = Template { elements: vec![Element {
            max_particles: 4, end: 5.0,
            renderer: Renderer { tint: [1.0, 1.0, 1.0, 0.01], ..Default::default() },
            actions: vec![always(Kind::Burst { count: 4.0, slots: slots(&[(9, one(10.0))]) })],
            ..Default::default()
        }], ..Default::default() };
        let mut effect = Effect::new(&t);
        effect.step(&t, &Affine3A::IDENTITY, DT, &mut Rng(1));
        for (p, alpha) in effect.groups[0].as_mut().unwrap().particles.iter_mut().zip([0.0, 0.049, 0.05, 1.0]) {
            p.alpha = alpha;
        }
        let sprites = effect.sprites(&t, 0, &Affine3A::IDENTITY);
        assert_eq!(sprites.len(), 2);
        assert_eq!(sprites[0].colour[3], 0.05 * 0.01);
        assert_eq!(effect.group(0).unwrap().particles.len(), 4);
    }
}
