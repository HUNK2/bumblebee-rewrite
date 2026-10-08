//! The reticle: what the game draws round the middle of the picture for his weapons. It is
//! not part of the Flash HUD: the executable draws it itself, textured quads in a picture
//! of 1280 x 720 (`FUN_007b8420` and what it calls; the textures are loaded by path in
//! `FUN_007b54c0` and live in `bnxglobal.str`). [game]
//!
//! Not ported: the arrows towards whoever hurt him (`FUN_007b80b0`, `hitarrow`: nothing
//! here hurts him), the sniper's and the charged shot's pictures (he has neither).

use glam::Vec2;

use crate::lockon::View;
use crate::weapons::Kind;

/// The picture the quads are placed in.
pub const PICTURE: Vec2 = Vec2::new(1280.0, 720.0);
const MIDDLE: Vec2 = Vec2::new(640.0, 360.0);

/// [game: 007b8420] At most one cue per displayed frame, even if several
/// thresholds were crossed. A completed lock takes precedence over progress.
pub fn lock_sound(previous: f32, current: f32) -> Option<u32> {
    if current >= 1.0 && previous < 1.0 {
        Some(crate::formats::hash::crc32(b"UI_LOCKED_ON"))
    } else if [0.25, 0.5, 0.7, 0.85, 0.95].into_iter().any(|p| previous < p && current >= p) {
        Some(crate::formats::hash::crc32(b"UI_LOCK_ON"))
    } else {
        None
    }
}

/// The textures, named as `FUN_007b54c0` names them.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Texture {
    BaseGun,
    BaseGunTarget,
    BaseMissile,
    BaseMissileTarget,
    HeatLeftFrame,
    HeatLeftFill,
    HeatRightFrame,
    HeatRightFill,
    HeatLeftFrameLocked,
    HeatLeftFillLocked,
    HeatRightFrameLocked,
    HeatRightFillLocked,
    LockLine,
    Hit,
    WeaponFrame,
    IconRapid,
    IconShotgun,
    IconCharge,
    IconMissile,
    Bracket,
    BracketFocus,
}

impl Texture {
    pub const ALL: [Texture; 21] = [
        Texture::BaseGun,
        Texture::BaseGunTarget,
        Texture::BaseMissile,
        Texture::BaseMissileTarget,
        Texture::HeatLeftFrame,
        Texture::HeatLeftFill,
        Texture::HeatRightFrame,
        Texture::HeatRightFill,
        Texture::HeatLeftFrameLocked,
        Texture::HeatLeftFillLocked,
        Texture::HeatRightFrameLocked,
        Texture::HeatRightFillLocked,
        Texture::LockLine,
        Texture::Hit,
        Texture::WeaponFrame,
        Texture::IconRapid,
        Texture::IconShotgun,
        Texture::IconCharge,
        Texture::IconMissile,
        Texture::Bracket,
        Texture::BracketFocus,
    ];

    /// The file's name under `c:\tf2\export\textures\crosshair\`, without `.dds`.
    pub fn file(self) -> &'static str {
        match self {
            Texture::BaseGun => "hud_reticle_base_gun",
            Texture::BaseGunTarget => "hud_reticle_base_gun_target",
            Texture::BaseMissile => "hud_reticle_base_missle",
            Texture::BaseMissileTarget => "hud_reticle_base_missle_target",
            Texture::HeatLeftFrame => "hud_reticle_heat_l_frame",
            Texture::HeatLeftFill => "hud_reticle_heat_l_fill",
            Texture::HeatRightFrame => "hud_reticle_heat_r_frame",
            Texture::HeatRightFill => "hud_reticle_heat_r_fill",
            Texture::HeatLeftFrameLocked => "hud_reticle_heat_l_frame_locked",
            Texture::HeatLeftFillLocked => "hud_reticle_heat_l_fill_locked",
            Texture::HeatRightFrameLocked => "hud_reticle_heat_r_frame_locked",
            Texture::HeatRightFillLocked => "hud_reticle_heat_r_fill_locked",
            Texture::LockLine => "hud_reticle_lockline",
            Texture::Hit => "hud_reticle_hit",
            Texture::WeaponFrame => "hud_reticle_weapon_frame_l",
            Texture::IconRapid => "hud_icon_weapon_rapid",
            Texture::IconShotgun => "hud_icon_weapon_shotgun",
            Texture::IconCharge => "hud_icon_weapon_charge",
            Texture::IconMissile => "hud_icon_weapon_missile",
            Texture::Bracket => "crosshair_missile",
            Texture::BracketFocus => "crosshair_missile_focus",
        }
    }

    /// The texture's id in the pack: the hash of the whole path (`FUN_00550ac0`). [game]
    pub fn id(self) -> u32 {
        crate::formats::hash::lower33(&format!("c:\\tf2\\export\\textures\\crosshair\\{}.dds", self.file()))
    }
}

/// One quad (`FUN_0055f670`): its middle and size in the picture, the part of the texture
/// (left, top, right, bottom, 0 to 1; left over right is mirrored), its opacity and its
/// turn about its middle, clockwise in radians.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Sprite {
    pub texture: Texture,
    pub at: Vec2,
    pub size: Vec2,
    pub uv: [f32; 4],
    pub alpha: f32,
    pub turn: f32,
    /// `at` came from a point of the world (the game scales it by 1280 / width and 720 /
    /// height); the others are fixed places.
    pub projected: bool,
}

impl Sprite {
    fn fixed(texture: Texture, at: Vec2, size: Vec2, alpha: f32) -> Self {
        Self { texture, at, size, uv: [0.0, 0.0, 1.0, 1.0], alpha, turn: 0.0, projected: false }
    }
}

/// One of the manager's two weapons (`+0x34`, drawn on the left, and `+0x3c`, on the right).
#[derive(Clone, Copy, Debug)]
pub struct Side {
    pub kind: Kind,
    /// The heat (slot `+0xf0`) and the most it holds (slot `+0xb0`).
    pub heat: f32,
    pub capacity: f32,
    pub charged_shot: bool,
    pub projectiles: u32,
    /// Seconds the weapon's word 0xf6 has counted (`FUN_007a73c0`: up while its slot
    /// `+0x50` says yes, else 0).
    pub shown_for: f32,
}

impl Side {
    /// The meter: the heat, at most the capacity, over the capacity.
    fn meter(&self) -> f32 {
        if self.capacity <= 0.0 { 0.0 } else { self.heat.min(self.capacity) / self.capacity }
    }

    fn icon(&self) -> Texture {
        // `FUN_007b7560`: a launcher's is the missile; a gun's the charge with a charged
        // shot, the shotgun with more than one projectile a firing, else the rapid one.
        // [not ported: the "snaker" one, for attribute values his weapons do not have]
        match self.kind {
            Kind::Launcher => Texture::IconMissile,
            Kind::Gun if self.charged_shot => Texture::IconCharge,
            Kind::Gun if self.projectiles > 1 => Texture::IconShotgun,
            Kind::Gun => Texture::IconRapid,
        }
    }
}

/// The bracket round the target a launcher is locking on or has locked.
#[derive(Clone, Copy, Debug)]
pub struct Bracket {
    pub at: Vec2,
    pub size: f32,
    pub locked: bool,
}

/// The bracket for a target standing at `place`, `height` tall and `half_width` to each
/// side: its middle is the target's, its size 1.5 times the larger of the height and the
/// width as the picture shows them, between 30 and 300 (`00b8d9c8`...). [game]
pub fn bracket(view: &View, place: glam::Vec3, height: f32, half_width: f32, locked: bool) -> Option<Bracket> {
    let at = view.project(place + glam::Vec3::Z * (height * 0.5))?;
    let low = view.project(place)?;
    let high = view.project(place + glam::Vec3::Z * height)?;
    let left = view.project(place - view.right * half_width)?;
    let right = view.project(place + view.right * half_width)?;
    let size = ((low.y - high.y).abs().max((right.x - left.x).abs()) * 1.5).clamp(30.0, 300.0);
    Some(Bracket { at, size, locked })
}

/// What one frame's drawing is made from.
#[derive(Clone, Copy, Debug, Default)]
pub struct Frame {
    /// His control mode is the weapon one (`+0x31c` == 2, or `FUN_00721b50`).
    pub weapon_mode: bool,
    pub primary: Option<Side>,
    pub secondary: Option<Side>,
    /// The weapon in hand (`FUN_007b5030(manager, 0)`), which picks the base.
    pub current: Option<Kind>,
    /// The aim assist has a target within the weapon's range: the `_target` textures.
    pub on_target: bool,
    /// The lock, 0 to 1 (`FUN_007b2240`; 1 once locked).
    pub lock: f32,
    pub bracket: Option<Bracket>,
    /// Seconds since `+0x234`'s `+0x30`. [assumed: the time one of his shots last hit;
    /// what writes it is not read]
    pub since_hit: f32,
    /// The car's weapon is in hand (`FUN_007b44e0`): the points 30 and 50 ahead of it in
    /// the picture, where the two bases go.
    pub car: Option<(Option<Vec2>, Option<Vec2>)>,
}

/// The reticle's own state: the two meters' flashes (`00d4fdd8`, `00d4fddc`) and the
/// bracket's (`00cf5c9f`, `00cf5d6c`).
#[derive(Clone, Copy, Debug, Default)]
pub struct Reticle {
    flash: [f32; 2],
    focus: bool,
    focus_wait: f32,
}

/// The hit mark's strength: whole for 0.225 s, gone at 0.45. Drawn squared. [game]
pub fn hit_strength(since: f32) -> f32 {
    if !(0.0..=0.45).contains(&since) {
        0.0
    } else if since > 0.225 {
        1.0 - (since - 0.225) * 4.444_444_7
    } else {
        1.0
    }
}

impl Reticle {
    /// A full meter's fill flashes: the square root of 1 - 2 t, t running to 0.5 and
    /// starting again (`FUN_007b6f90`). [game]
    fn fill_alpha(clock: &mut f32, meter: f32, dt: f32) -> f32 {
        if meter != 1.0 {
            *clock = 0.0;
            return 1.0;
        }
        let alpha = (1.0 - *clock * 2.0).clamp(0.0, 1.0).sqrt();
        *clock += dt;
        if *clock >= 0.5 {
            *clock = 0.0;
        }
        alpha
    }

    /// A weapon's icon and its frame (`FUN_007b7560`): they come up beside the middle as
    /// the weapon's clock starts, and after 1.5 s shrink into a small frame that stays in
    /// weapon mode and is gone out of it. `side` is -1 for the left, 1 for the right.
    fn icon(out: &mut Vec<Sprite>, weapon: &Side, side: f32, weapon_mode: bool) {
        let t = weapon.shown_for;
        if t <= 0.0 {
            return;
        }
        let frame_alpha = if t < 0.2 { t * 5.0 } else { 1.0 };
        // [assumed: 1 between 0.5 and 1.5 s; the decompiler leaves the value unset there]
        let mut icon_alpha = if t < 0.5 { (t - 0.3) * 5.0 } else { 1.0 };
        let (mut scale, mut away) = (1.0, 90.0);
        if t > 1.5 {
            if t >= 1.7 {
                (scale, away, icon_alpha) = (0.3, 74.0, 0.0);
            } else {
                let k = (t - 1.5) * 5.0;
                (scale, away, icon_alpha) = (1.0 - k * 0.7, 90.0 - k * 16.0, 1.0 - k);
            }
        }
        let uv = if side < 0.0 { [0.0, 0.0, 1.0, 1.0] } else { [1.0, 0.0, 0.0, 1.0] };
        if t < 1.5 || weapon_mode {
            let at = MIDDLE + Vec2::new(side * away, 0.0);
            out.push(Sprite { uv, ..Sprite::fixed(Texture::WeaponFrame, at, Vec2::splat(scale * 52.0), frame_alpha) });
            if icon_alpha > 0.0 {
                out.push(Sprite::fixed(weapon.icon(), at, Vec2::splat(scale * 48.0), icon_alpha));
            }
        } else if icon_alpha > 0.0 {
            let at = MIDDLE + Vec2::new(side * 90.0, 0.0);
            out.push(Sprite { uv, ..Sprite::fixed(Texture::WeaponFrame, at, Vec2::splat(52.0), icon_alpha) });
            out.push(Sprite::fixed(weapon.icon(), at, Vec2::splat(48.0), icon_alpha));
        }
    }

    /// One frame's quads, in the order the game draws them.
    pub fn draw(&mut self, f: &Frame, dt: f32) -> Vec<Sprite> {
        let mut out = Vec::new();
        let left = f.primary.as_ref().map_or(0.0, Side::meter);
        let right = f.secondary.as_ref().map_or(0.0, Side::meter);
        if let Some(weapon) = &f.primary {
            Self::icon(&mut out, weapon, -1.0, f.weapon_mode);
        }
        if let Some(weapon) = &f.secondary {
            Self::icon(&mut out, weapon, 1.0, f.weapon_mode);
        }
        // Out of weapon mode the meters stay while there is heat. [game, from the
        // pseudocode alone: their opacity there is the last weapon's heat, not its share]
        let alpha = if f.weapon_mode {
            1.0
        } else {
            f.secondary.or(f.primary).map_or(1.0, |w| w.heat.min(w.capacity)).clamp(0.0, 1.0)
        };
        let mut fills = [1.0, 1.0];
        if f.weapon_mode || alpha > 0.0 {
            fills = [Self::fill_alpha(&mut self.flash[0], left, dt) * alpha, Self::fill_alpha(&mut self.flash[1], right, dt) * alpha];
        }
        let (base, base_target) = match f.current {
            Some(Kind::Launcher) => (Texture::BaseMissile, Texture::BaseMissileTarget),
            _ => (Texture::BaseGun, Texture::BaseGunTarget),
        };
        let hit = hit_strength(f.since_hit);
        if f.weapon_mode {
            if let Some(bracket) = f.bracket {
                // While it is being locked the bracket changes picture every 0.1 s.
                self.focus_wait -= dt;
                if self.focus_wait < 0.0 {
                    self.focus = !self.focus;
                    self.focus_wait = 0.1;
                }
                let texture = if bracket.locked || self.focus { Texture::BracketFocus } else { Texture::Bracket };
                out.push(Sprite { projected: true, ..Sprite::fixed(texture, bracket.at, Vec2::splat(bracket.size), 1.0) });
            }
            out.push(Sprite::fixed(if f.on_target { base_target } else { base }, MIDDLE, Vec2::splat(128.0), 1.0));
            // Four lines that grow towards the middle with the lock (`FUN_007b67c0`).
            let p = f.lock;
            let size = Vec2::new(16.0, p * 48.0 + 16.0);
            let uv = [0.0, 0.0, 1.0, p * 0.75 + 0.25];
            let reach = p * 24.0;
            for (at, turn) in [
                (Vec2::new(640.0, reach + 304.0), 0.0),
                (Vec2::new(640.0, 424.0 - (reach + 8.0)), std::f32::consts::PI),
                (Vec2::new(704.0 - (reach + 8.0), 360.0), std::f32::consts::FRAC_PI_2),
                (Vec2::new(reach + 584.0, 360.0), -std::f32::consts::FRAC_PI_2),
            ] {
                out.push(Sprite { uv, turn, ..Sprite::fixed(Texture::LockLine, at, size, 1.0) });
            }
            if hit > 0.0 {
                out.push(Sprite::fixed(Texture::Hit, MIDDLE, Vec2::splat(128.0), hit * hit));
            }
        } else if let Some((near, far)) = f.car {
            // The car's gun: a small base 50 ahead of it, the full one 30 ahead.
            if let Some(far) = far {
                out.push(Sprite { projected: true, ..Sprite::fixed(base, far, Vec2::splat(77.0), 1.0) });
            }
            if let Some(near) = near {
                out.push(Sprite { projected: true, ..Sprite::fixed(if f.on_target { base_target } else { base }, near, Vec2::splat(128.0), 1.0) });
                if hit > 0.0 {
                    out.push(Sprite { projected: true, ..Sprite::fixed(Texture::Hit, near, Vec2::splat(128.0), hit * hit) });
                }
            }
        }
        if alpha > 0.0 {
            // The two meters (`FUN_007b7170`): frames either side, fills from the bottom up.
            let locked = f.on_target;
            let pick = |plain, locked_one| if locked { locked_one } else { plain };
            out.push(Sprite::fixed(pick(Texture::HeatLeftFrame, Texture::HeatLeftFrameLocked), Vec2::new(576.0, 360.0), Vec2::new(32.0, 64.0), alpha));
            out.push(Sprite::fixed(pick(Texture::HeatRightFrame, Texture::HeatRightFrameLocked), Vec2::new(704.0, 360.0), Vec2::new(32.0, 64.0), alpha));
            for (meter, x, texture, fill) in [
                (left, 576.0, pick(Texture::HeatLeftFill, Texture::HeatLeftFillLocked), fills[0]),
                (right, 704.0, pick(Texture::HeatRightFill, Texture::HeatRightFillLocked), fills[1]),
            ] {
                if meter > 0.0 {
                    out.push(Sprite {
                        uv: [0.0, 1.0 - meter, 1.0, 1.0],
                        ..Sprite::fixed(texture, Vec2::new(x, 392.0 - meter * 32.0), Vec2::new(32.0, meter * 64.0), fill)
                    });
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_cues_cross_once_and_completion_wins_over_skipped_thresholds() {
        for threshold in [0.25, 0.5, 0.7, 0.85, 0.95] {
            assert_eq!(lock_sound(threshold - 0.01, threshold), Some(crate::formats::hash::crc32(b"UI_LOCK_ON")));
            assert_eq!(lock_sound(threshold, threshold), None);
            assert_eq!(lock_sound(threshold, 0.0), None);
        }
        assert_eq!(lock_sound(0.0, 0.96), Some(crate::formats::hash::crc32(b"UI_LOCK_ON")));
        assert_eq!(lock_sound(0.0, 1.0), Some(crate::formats::hash::crc32(b"UI_LOCKED_ON")));
        assert_eq!(lock_sound(1.0, 1.0), None);
    }

    fn gun(heat: f32, shown_for: f32) -> Side {
        Side { kind: Kind::Gun, heat, capacity: 10.0, charged_shot: false, projectiles: 1, shown_for }
    }

    fn find(sprites: &[Sprite], texture: Texture) -> Vec<Sprite> {
        sprites.iter().copied().filter(|s| s.texture == texture).collect()
    }

    #[test]
    fn weapon_mode_draws_the_base_the_lines_and_the_meters() {
        let frame = Frame { weapon_mode: true, primary: Some(gun(5.0, 5.0)), current: Some(Kind::Gun), since_hit: 100.0, ..Default::default() };
        let sprites = Reticle::default().draw(&frame, 0.016);
        let base = find(&sprites, Texture::BaseGun);
        assert_eq!((base[0].at, base[0].size), (Vec2::new(640.0, 360.0), Vec2::splat(128.0)));
        // With no lock the lines are 16 square, a quarter of the texture, 56 from the middle.
        let lines = find(&sprites, Texture::LockLine);
        assert_eq!(lines.len(), 4);
        assert_eq!((lines[0].at, lines[0].size, lines[0].uv[3]), (Vec2::new(640.0, 304.0), Vec2::splat(16.0), 0.25));
        assert_eq!(lines[1].at, Vec2::new(640.0, 416.0));
        assert_eq!(lines[2].at, Vec2::new(696.0, 360.0));
        assert_eq!(lines[3].at, Vec2::new(584.0, 360.0));
        // Half a meter fills the lower half of the left frame.
        let fill = find(&sprites, Texture::HeatLeftFill);
        assert_eq!((fill[0].at, fill[0].size, fill[0].uv), (Vec2::new(576.0, 376.0), Vec2::new(32.0, 32.0), [0.0, 0.5, 1.0, 1.0]));
        assert!(find(&sprites, Texture::HeatRightFill).is_empty());
        assert!(find(&sprites, Texture::Hit).is_empty());
        // Past 1.7 s the weapon's frame is the small one, 74 from the middle, no icon.
        let frames = find(&sprites, Texture::WeaponFrame);
        assert_eq!((frames[0].at, frames[0].size), (Vec2::new(566.0, 360.0), Vec2::splat(0.3 * 52.0)));
        assert!(find(&sprites, Texture::IconRapid).is_empty());
    }

    #[test]
    fn a_lock_grows_the_lines_and_a_target_changes_the_textures() {
        let launcher = Side { kind: Kind::Launcher, ..gun(0.0, 5.0) };
        let frame = Frame {
            weapon_mode: true,
            primary: Some(gun(0.0, 5.0)),
            secondary: Some(launcher),
            current: Some(Kind::Launcher),
            on_target: true,
            lock: 1.0,
            bracket: Some(Bracket { at: Vec2::new(700.0, 300.0), size: 90.0, locked: true }),
            since_hit: 0.3,
            ..Default::default()
        };
        let sprites = Reticle::default().draw(&frame, 0.016);
        assert_eq!(find(&sprites, Texture::BaseMissileTarget).len(), 1);
        let lines = find(&sprites, Texture::LockLine);
        assert_eq!((lines[0].at, lines[0].size, lines[0].uv[3]), (Vec2::new(640.0, 328.0), Vec2::new(16.0, 64.0), 1.0));
        let bracket = find(&sprites, Texture::BracketFocus);
        assert!(bracket[0].projected && bracket[0].size == Vec2::splat(90.0));
        assert_eq!(find(&sprites, Texture::HeatLeftFrameLocked).len(), 1);
        // 0.3 s after a hit the mark is two thirds gone, and drawn squared.
        let hit = find(&sprites, Texture::Hit);
        let strength = 1.0 - 0.075 * 4.444_444_7;
        assert!((hit[0].alpha - strength * strength).abs() < 1e-5);
    }

    #[test]
    fn a_full_meter_flashes_and_out_of_weapon_mode_only_heat_shows() {
        let mut reticle = Reticle::default();
        let frame = Frame { weapon_mode: true, primary: Some(gun(10.0, 5.0)), current: Some(Kind::Gun), since_hit: 100.0, ..Default::default() };
        let alphas: Vec<f32> = (0..6).map(|_| find(&reticle.draw(&frame, 0.1), Texture::HeatLeftFill)[0].alpha).collect();
        assert!((alphas[0] - 1.0).abs() < 1e-6 && (alphas[2] - 0.6f32.sqrt()).abs() < 1e-5);
        assert!((alphas[5] - 1.0).abs() < 1e-6, "the flash starts again at 0.5 s: {alphas:?}");
        // On foot with a cold gun nothing is drawn; with heat, the meters alone.
        let cold = Frame { primary: Some(gun(0.0, 5.0)), since_hit: 100.0, ..Default::default() };
        assert!(Reticle::default().draw(&cold, 0.016).is_empty());
        let warm = Frame { primary: Some(gun(4.0, 5.0)), since_hit: 100.0, ..Default::default() };
        let sprites = Reticle::default().draw(&warm, 0.016);
        assert_eq!(sprites.len(), 3);
        assert!(find(&sprites, Texture::BaseGun).is_empty());
    }

    #[test]
    fn an_icon_comes_up_then_shrinks_into_its_frame() {
        let at = |t: f32, weapon_mode| {
            let frame = Frame { weapon_mode, secondary: Some(Side { kind: Kind::Launcher, ..gun(0.0, t) }), since_hit: 100.0, ..Default::default() };
            let sprites = Reticle::default().draw(&frame, 0.016);
            (find(&sprites, Texture::WeaponFrame), find(&sprites, Texture::IconMissile))
        };
        let (frame, icon) = at(0.1, false);
        assert!((frame[0].alpha - 0.5).abs() < 1e-6 && icon.is_empty());
        // The right one's frame is the left one's mirrored.
        assert_eq!((frame[0].at, frame[0].uv), (Vec2::new(730.0, 360.0), [1.0, 0.0, 0.0, 1.0]));
        let (frame, icon) = at(1.0, false);
        assert_eq!((frame[0].size, icon[0].size, icon[0].alpha), (Vec2::splat(52.0), Vec2::splat(48.0), 1.0));
        let (frame, icon) = at(1.6, true);
        assert!((frame[0].at.x - 722.0).abs() < 1e-4 && (icon[0].alpha - 0.5).abs() < 1e-5);
        let (frame, icon) = at(2.0, false);
        assert!(frame.is_empty() && icon.is_empty());
    }

    #[test]
    fn the_car_has_two_bases_ahead_of_it() {
        let frame = Frame {
            primary: Some(gun(0.0, 0.0)),
            current: Some(Kind::Gun),
            since_hit: 100.0,
            car: Some((Some(Vec2::new(600.0, 380.0)), Some(Vec2::new(610.0, 370.0)))),
            ..Default::default()
        };
        let sprites = Reticle::default().draw(&frame, 0.016);
        let bases = find(&sprites, Texture::BaseGun);
        assert_eq!((bases[0].size, bases[1].size), (Vec2::splat(77.0), Vec2::splat(128.0)));
        assert!(bases.iter().all(|s| s.projected));
    }

    #[test]
    fn a_bracket_is_sized_by_the_target_in_the_picture() {
        let view = View::looking(glam::Vec3::ZERO, glam::Vec3::Y, 60.0, 16.0 / 9.0);
        // 4 tall at 20 away: 4 / (20 tan 30) of half the picture's height, times 1.5.
        let b = bracket(&view, glam::Vec3::new(0.0, 20.0, -2.0), 4.0, 1.0, false).unwrap();
        let tall = 4.0 / (20.0 * 30f32.to_radians().tan()) * 360.0;
        assert!((b.size - tall * 1.5).abs() < 1e-2, "{}", b.size);
        assert!((b.at - Vec2::new(640.0, 360.0)).length() < 1e-3);
        // Far away it is never under 30.
        assert_eq!(bracket(&view, glam::Vec3::new(0.0, 2000.0, 0.0), 4.0, 1.0, true).unwrap().size, 30.0);
    }

    #[test]
    fn the_ids_are_the_hashes_of_the_paths() {
        assert_eq!(Texture::BaseGun.id(), 0x1e43_a6fd);
    }
}
