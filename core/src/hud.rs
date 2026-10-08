//! The game's HUD (`UI\Flash\hud.gfx`): the part of its ActionScript classes that the
//! three meters of a single-player character need, played over `formats::gfx`. Read from
//! the movie's own scripts (`work\gfx_dump.py <file> code <id>`). [game] where the scripts
//! say it; what the executable sends them is in `notes\hud.md` "The binding".
//!
//! - `core.BaseMeter` (health): the value, 1 above 0.9999 and 0 under 0.00001, slides
//!   `Mask` to `_x = -w + value * w`, w the mask's width. `hud2.common.HealthMeter`: under
//!   `low_threshold` 0.4 the bar plays `low_loop`; else it stands on frame 1.
//! - `core.CoolDownBaseMeter` (turbo, ability): `Mask._y = h - value * h`, h the mask's
//!   height (`orientationModifier` -1). Reaching 1 plays `activate`, coming back to 0
//!   plays `ready`, on the meter's animation clip.
//!
//! Not ported here: the button prompt on the ability, text, the radar, the counters, the
//! messages, and each element's `in` and `out` animations (they stand on `main`).

use crate::formats::gfx::{Affine, Canvas, Movie, Pose, Tint, Tweak};

/// Names in `characterselect2.gfx`'s shared `IconList`, indexed by
/// `specialAbilityIcon` in a character's attributes. [game]
pub fn ability_icon_export(index: u32) -> Option<&'static str> {
    ["Ability_Damage", "Ability_Defense", "Ability_Heal", "Ability_Projectile", "Ability_Turret"]
        .get(index as usize)
        .copied()
}

/// `FUN_00716890` sends icon 12 for Bumblebee's turbo. [game]
pub fn turbo_icon_export(index: u32) -> Option<&'static str> {
    (index == 12).then_some("SC_Turbo_Icon_1")
}

/// The three meters, by the names the movie exports them under.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Meter {
    Health,
    Turbo,
    Ability,
}

impl Meter {
    pub const ALL: [Meter; 3] = [Meter::Health, Meter::Turbo, Meter::Ability];

    fn export(self) -> &'static str {
        match self {
            Meter::Health => "Health_Meter Class",
            Meter::Turbo => "Turbo_Meter Class",
            Meter::Ability => "Ability_Meter Class",
        }
    }

    /// Where `Hud_SP` puts the clip on the 1280 x 720 stage. [data]
    fn origin(self) -> (f32, f32) {
        match self {
            Meter::Health | Meter::Turbo => (639.2, 359.1),
            Meter::Ability => (122.2, 600.0),
        }
    }

    /// The part of the stage it is drawn in: left, top, right, bottom, a little more than
    /// its shapes' bounds.
    pub fn stage_rect(self) -> [f32; 4] {
        match self {
            Meter::Health => [150.0, 580.0, 274.0, 620.0],
            Meter::Turbo => [120.0, 433.0, 165.0, 478.0],
            Meter::Ability => [90.0, 564.0, 154.0, 636.0],
        }
    }

    /// The clip whose `Meter` holds the mask, and the mask's size along the way it slides.
    fn animation(self) -> (&'static str, f32) {
        match self {
            Meter::Health => ("Meter_Animation", 111.0),
            Meter::Turbo => ("Turbo_Meter_Animation", 31.0),
            Meter::Ability => ("Ability_Meter_Animation", 57.2),
        }
    }
}

/// The movie's frames a second.
const RATE: f32 = 30.0;
/// `low_loop` in the health bar's `Meter`: frames 2 to 32, then back.
const LOW_LOOP: (f32, f32) = (2.0, 32.0);

/// `BaseMeter`'s and `CoolDownBaseMeter`'s rounding of a value.
pub fn rounded(value: f32) -> f32 {
    if value > 0.9999 {
        1.0
    } else if value < 0.00001 {
        0.0
    } else {
        value
    }
}

/// A cooldown meter's state: its value and the frame its animation clip is playing.
#[derive(Clone, Copy, Debug, Default)]
struct Cooldown {
    value: f32,
    playing: Option<f32>,
}

impl Cooldown {
    fn step(&mut self, value: f32, labels: (f32, f32), frames: f32, dt: f32) {
        let value = rounded(value);
        if let Some(frame) = &mut self.playing {
            *frame += dt * RATE;
            if *frame >= frames + 1.0 {
                self.playing = None;
            }
        }
        // [simplified: the script tells the two apart by its `_lastmeter`]
        if value >= 1.0 && self.value < 1.0 {
            self.playing = Some(labels.1);
        } else if value == 0.0 && self.value > 0.0 {
            self.playing = Some(labels.0);
        }
        self.value = value;
    }
}

/// What the three meters show.
#[derive(Clone, Copy, Debug, Default)]
pub struct Hud {
    health: f32,
    low_frame: Option<f32>,
    turbo: Cooldown,
    ability: Cooldown,
    clock: f32,
}

impl Hud {
    /// One frame. `health` is his health's share; `turbo` and `ability` are 1 as the
    /// thing is used, falling to 0 as it becomes ready again.
    pub fn step(&mut self, movie: &Movie, health: f32, turbo: f32, ability: f32, dt: f32) {
        self.clock += dt;
        self.health = rounded(health);
        // `onMeterChange`: low under the threshold; `onMeterFull` and the rest: normal.
        self.low_frame = if self.health < 0.4 {
            let frame = self.low_frame.map_or(LOW_LOOP.0, |frame| frame + dt * RATE);
            Some(if frame >= LOW_LOOP.1 + 1.0 { LOW_LOOP.0 + (frame - LOW_LOOP.1 - 1.0) } else { frame })
        } else {
            None
        };
        for (meter, state, value) in [(Meter::Turbo, &mut self.turbo, turbo), (Meter::Ability, &mut self.ability, ability)] {
            let clip = Self::clip(movie, meter);
            let labels = clip.map_or((2.0, 2.0), |c| (c.label("ready").unwrap_or(2) as f32, c.label("activate").unwrap_or(2) as f32));
            state.step(value, labels, clip.map_or(1.0, |c| c.frames.len() as f32), dt);
        }
    }

    fn clip(movie: &Movie, meter: Meter) -> Option<&crate::formats::gfx::Sprite> {
        let root = movie.sprite(*movie.exports.get(meter.export())?)?;
        let main = root.label("main")?;
        let id = root.shown(main).into_iter().find(|p| p.name.as_deref() == Some(meter.animation().0))?.id?;
        movie.sprite(id)
    }

    /// What the scripts have set on a meter's clips.
    pub fn pose(&self, movie: &Movie, meter: Meter) -> Pose {
        let mut pose = Pose::new();
        let root = movie.exports.get(meter.export()).and_then(|id| movie.sprite(*id));
        let main = root.and_then(|sprite| sprite.label("main")).unwrap_or(1);
        pose.insert(String::new(), Tweak { frame: Some(main), ..Default::default() });
        let (animation, size) = meter.animation();
        let (clip_frame, bar_frame, mask) = match meter {
            Meter::Health => {
                let x = -size + self.health * size;
                (1, self.low_frame.map_or(1, |f| f as usize), Tweak { x: Some(x), ..Default::default() })
            }
            Meter::Turbo | Meter::Ability => {
                let state = if meter == Meter::Turbo { &self.turbo } else { &self.ability };
                let y = size - state.value * size;
                (state.playing.map_or(1, |f| f as usize), 1, Tweak { y: Some(y), ..Default::default() })
            }
        };
        pose.insert(animation.to_owned(), Tweak { frame: Some(clip_frame), ..Default::default() });
        pose.insert(format!("{animation}/Meter"), Tweak { frame: Some(bar_frame), ..Default::default() });
        pose.insert(format!("{animation}/Meter/Mask"), mask);
        pose
    }

    /// What decides a meter's picture: it need not be drawn again while this is the same.
    pub fn key(&self, meter: Meter) -> [i32; 3] {
        let tick = (self.clock * RATE) as i32;
        match meter {
            // Its sheen plays by itself.
            Meter::Health => [(self.health * 1024.0) as i32, self.low_frame.map_or(0, |f| f as i32), tick],
            Meter::Turbo => [(self.turbo.value * 1024.0) as i32, self.turbo.playing.map_or(0, |f| f as i32), 0],
            Meter::Ability => [(self.ability.value * 1024.0) as i32, self.ability.playing.map_or(0, |f| f as i32), 0],
        }
    }

    /// A meter's picture at `scale` pixels to a stage pixel, covering `Meter::stage_rect`.
    pub fn draw(&self, movie: &Movie, meter: Meter, scale: f32) -> Option<Canvas> {
        let id = *movie.exports.get(meter.export())?;
        let rect = meter.stage_rect();
        let mut canvas = Canvas::new(((rect[2] - rect[0]) * scale).ceil() as usize, ((rect[3] - rect[1]) * scale).ceil() as usize);
        let origin = meter.origin();
        let to_canvas = Affine::scale_then_move(scale, (origin.0 - rect[0]) * scale, (origin.1 - rect[1]) * scale);
        movie.draw(id, &to_canvas, &Tint::NONE, &self.pose(movie, meter), "", self.clock, &mut canvas);
        Some(canvas)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn movie() -> Option<Movie> {
        // Needs the install.
        Movie::parse(&std::fs::read(r"C:\Games2\UI\Flash\hud.gfx").ok()?).ok()
    }

    /// The colour the bars are filled with.
    const BAR: [f32; 3] = [187.0 / 255.0, 90.0 / 255.0, 99.0 / 255.0];

    fn is_bar(pixel: [f32; 4]) -> bool {
        pixel[3] > 0.9 && (0..3).all(|i| (pixel[i] / pixel[3] - BAR[i]).abs() < 0.2)
    }

    #[test]
    fn values_round_as_the_scripts_round_them() {
        assert_eq!((rounded(0.99995), rounded(0.000001), rounded(0.5)), (1.0, 0.0, 0.5));
    }

    #[test]
    fn the_health_bar_is_as_long_as_the_health() {
        let Some(movie) = movie() else { return };
        let mut hud = Hud::default();
        hud.step(&movie, 0.5, 0.0, 0.0, 0.0);
        let canvas = hud.draw(&movie, Meter::Health, 1.0).unwrap();
        // The bar runs from x = 156.7 to 267.7 on the stage, y = 588.5 to 611.5: at half
        // health its left half is filled and its right half is not.
        let at = |x: f32, y: f32| canvas.pixels[(y - 580.0) as usize * canvas.width + (x - 150.0) as usize];
        assert!(is_bar(at(180.0, 600.0)), "{:?}", at(180.0, 600.0));
        assert!(!is_bar(at(240.0, 600.0)), "{:?}", at(240.0, 600.0));
        assert_eq!(hud.pose(&movie, Meter::Health)["Meter_Animation/Meter/Mask"].x, Some(-55.5));
        // Under 0.4 the bar plays its loop, frames 2 to 32, and goes back to 1 above it.
        hud.step(&movie, 0.3, 0.0, 0.0, 0.0);
        assert_eq!(hud.pose(&movie, Meter::Health)["Meter_Animation/Meter"].frame, Some(2));
        hud.step(&movie, 0.3, 0.0, 0.0, 1.0);
        assert_eq!(hud.pose(&movie, Meter::Health)["Meter_Animation/Meter"].frame, Some(32));
        hud.step(&movie, 0.3, 0.0, 0.0, 0.1);
        assert_eq!(hud.pose(&movie, Meter::Health)["Meter_Animation/Meter"].frame, Some(4));
        hud.step(&movie, 0.9, 0.0, 0.0, 0.1);
        assert_eq!(hud.pose(&movie, Meter::Health)["Meter_Animation/Meter"].frame, Some(1));
    }

    #[test]
    fn a_cooldown_meter_drains_and_plays_its_two_animations() {
        let Some(movie) = movie() else { return };
        let mut hud = Hud::default();
        hud.step(&movie, 1.0, 1.0, 0.0, 0.0);
        let pose = hud.pose(&movie, Meter::Turbo);
        // Used: the mask is over the whole fill, and `activate` (frame 8) starts.
        assert_eq!((pose["Turbo_Meter_Animation/Meter/Mask"].y, pose["Turbo_Meter_Animation"].frame), (Some(0.0), Some(8)));
        hud.step(&movie, 1.0, 0.5, 0.0, 0.1);
        let pose = hud.pose(&movie, Meter::Turbo);
        assert_eq!((pose["Turbo_Meter_Animation/Meter/Mask"].y, pose["Turbo_Meter_Animation"].frame), (Some(15.5), Some(11)));
        // Ready again: `ready` plays from frame 2, and in time the clip is back on 1.
        hud.step(&movie, 1.0, 0.0, 0.0, 0.1);
        assert_eq!(hud.pose(&movie, Meter::Turbo)["Turbo_Meter_Animation"].frame, Some(2));
        hud.step(&movie, 1.0, 0.0, 0.0, 4.0);
        assert_eq!(hud.pose(&movie, Meter::Turbo)["Turbo_Meter_Animation"].frame, Some(1));
        // Half way through, the lower half of its 31 x 31 fill shows (stage 127 to 158,
        // 440 to 471).
        hud.step(&movie, 1.0, 1.0, 0.0, 0.0);
        hud.step(&movie, 1.0, 0.5, 0.0, 10.0);
        let canvas = hud.draw(&movie, Meter::Turbo, 1.0).unwrap();
        let at = |x: f32, y: f32| canvas.pixels[(y - 433.0) as usize * canvas.width + (x - 120.0) as usize];
        assert!(is_bar(at(142.0, 465.0)), "{:?}", at(142.0, 465.0));
        assert!(!is_bar(at(142.0, 446.0)), "{:?}", at(142.0, 446.0));
        assert_ne!(hud.key(Meter::Turbo), Hud::default().key(Meter::Turbo));
    }

    #[test]
    fn the_ability_fills_its_hexagon() {
        let Some(movie) = movie() else { return };
        let mut hud = Hud::default();
        hud.step(&movie, 1.0, 0.0, 1.0, 0.0);
        hud.step(&movie, 1.0, 0.0, 1.0, 10.0);
        let canvas = hud.draw(&movie, Meter::Ability, 1.0).unwrap();
        // Its middle is at 122, 600 on the stage; the fill is 60% opaque over a dark hexagon.
        let middle = canvas.pixels[(600 - 564) * canvas.width + (122 - 90)];
        assert!(middle[0] / middle[3] > 0.4 && middle[3] > 0.6, "{middle:?}");
        let empty = Hud::default().draw(&movie, Meter::Ability, 1.0).unwrap();
        let middle = empty.pixels[(600 - 564) * empty.width + (122 - 90)];
        assert!(middle[0] / middle[3] < 0.1, "{middle:?}");
    }
}
