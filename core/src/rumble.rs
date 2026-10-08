//! The pad's rumble, as read from the original (`notes\melee.md`, "Rumble"). The pad has
//! one effect at a time: a strength, a hold and a fade for each of its two motors, set from
//! a type, a duration and a strength (`FUN_005aada0`) and played in sixtieths of a second
//! (`FUN_005aae80`). Who starts one: a `ControllerRumble` object (the ground punch's
//! blast carries one), or a preset from `rumbleEffects` by number (landing, climbing, the
//! car's knocks; `FUN_007dc0a0`).

/// The length of one update of the pad's effect (`00b6ab08`). [game]
pub const UPDATE: f32 = 1.0 / 60.0;

/// One motor's share of an effect: whole for `hold` seconds, then fading evenly to nothing
/// over `fade`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Motor {
    strength: f32,
    hold: f32,
    fade: f32,
}

impl Motor {
    fn at(&self, time: f32) -> f32 {
        if time < self.hold {
            return self.strength;
        }
        let into = time - self.hold;
        if into < self.fade && self.fade > 0.0 { (self.fade - into) * (self.strength / self.fade) } else { 0.0 }
    }
}

/// The pad's effect (`DAT_00d1e87c`): the two motors as the pad takes them, the heavy
/// (left) one first. [assumed: which word of the pair is which motor follows XInput's
/// order, left then right]
#[derive(Clone, Debug, Default)]
pub struct Pad {
    active: bool,
    time: f32,
    motors: [Motor; 2],
}

impl Pad {
    /// Starts an effect in place of the one running (`FUN_005aada0`). Type 1: the heavy
    /// motor at a quarter of the strength fading over the whole duration, the light one at
    /// all of it fading over half. Types 2 and 3: the heavy motor alone, whole for the
    /// duration and then off. Type 0: off. Any other type restarts the clock and leaves the
    /// motors as they were set. [game]
    pub fn start(&mut self, kind: u32, duration: f32, strength: f32) {
        self.active = true;
        self.time = 0.0;
        match kind {
            0 => self.motors = [Motor::default(); 2],
            1 => {
                self.motors = [
                    Motor { strength: strength * 0.25, hold: 0.0, fade: duration },
                    Motor { strength, hold: 0.0, fade: duration * 0.5 },
                ]
            }
            2 | 3 => self.motors = [Motor { strength, hold: duration, fade: 0.0 }, Motor::default()],
            _ => {}
        }
    }

    /// One update: what the two motors are given now, 0 to 1, heavy then light
    /// (`FUN_005aae80`). `None` when no effect is running; the update that finds both at
    /// nothing still sends that, and ends the effect. [game]
    pub fn step(&mut self) -> Option<[f32; 2]> {
        if !self.active {
            return None;
        }
        let out = self.motors.map(|m| m.at(self.time));
        self.time += UPDATE;
        if out.iter().all(|&v| v <= 0.0) {
            self.active = false;
        }
        Some(out)
    }
}

/// A `ControllerRumble` (loader `FUN_007dbaa0`, start `FUN_007dbcb0`). [data]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RumbleDef {
    pub kind: u32,
    pub duration: f32,
    /// 0 to 1: the loader takes a hundredth of the data's number.
    pub strength: f32,
    /// `scaleType`: 0 is the same for every player, 1 weakens with the player's distance.
    /// Any other does nothing.
    pub scale_type: u32,
    pub distance_near: f32,
    pub distance_far: f32,
}

impl RumbleDef {
    /// The strength for a player `distance` away, if any (`FUN_007dbdc0`): whole within
    /// `distance_near`, falling evenly to nothing at `distance_far`, none past it. [game]
    pub fn strength_at(&self, distance: f32) -> Option<f32> {
        match self.scale_type {
            0 => Some(self.strength),
            1 => {
                if distance > self.distance_far {
                    return None;
                }
                if distance < self.distance_near {
                    return Some(self.strength);
                }
                let share = (self.distance_far - distance) / (self.distance_far - self.distance_near);
                (share > 0.0).then_some(self.strength * share)
            }
            _ => None,
        }
    }
}

/// One entry of `rumbleEffects` (`effects\rumblefx.lxb`, loader `FUN_007dbed0`): the table is
/// indexed by the entry's `id`. [data]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Preset {
    pub kind: u32,
    pub duration: f32,
    pub strength: f32,
}

/// The presets by number, and who plays each (`FUN_00717c40`, which is the only way one is
/// played: for a character a local player runs). [game] Never played by anything: 0 and 1
/// (the blocks) and 6 `RUMBLE_JUMP_LAND`, so a landing on foot does not rumble in the
/// original; no clip of his has an `AnimControllerRumbleEvent` either. [game] [data]
pub mod preset {
    /// A melee hit of his that a target took (`FUN_00717d80`, called by the target's damage
    /// routine `FUN_00719830` for a hit whose flags have `DAMAGE_BY_MELEE`): the strong one
    /// while his clip is `AttackFast_3_ChargeAttk` or `Trans_Vehicle2RobotSlam`, else the
    /// fast one. The same in a second damage routine, `FUN_00743a40` (an object's, not a
    /// character's; its class is not identified).
    pub const STRONG_HIT_ATTACKER: usize = 2;
    pub const FAST_HIT_ATTACKER: usize = 4;
    /// The same two for a player's character that is hit (`FUN_00719830`).
    pub const STRONG_HIT_DEFENDER: usize = 3;
    pub const FAST_HIT_DEFENDER: usize = 5;
    /// A hit with one of the flags `0xe0f0` on a player's character... by an attacker whose
    /// hit record says so (`FUN_00719830`; not followed).
    pub const SPECIAL_CONNECTED: usize = 7;
    /// The climb mode's enter, only while carrying something (`FUN_00857dd0`).
    pub const CLIMB_ATTACHED: usize = 8;
    /// Every update of the climb mode going up or sideways (`FUN_00856d00`). Its data is
    /// type 3 for 0 seconds: nothing is felt, and whatever was running stops.
    pub const CLIMB_MOVED: usize = 9;
    /// Every update of the climb mode going down.
    pub const CLIMB_SLIDING: usize = 10;
    /// The car touching an object that can break and is no character (`FUN_00860870`).
    pub const VEHICLE_HIT_DESTRUCTABLE: usize = 11;
    /// The car touching anything else at a speed over 5, for a contact of kind 3
    /// (`FUN_00860870`; what the kinds are is not read).
    pub const VEHICLE_HIT_WALL: usize = 12;
    /// A wheel on the ground when none was after the wheels' last step, more than 0.1 s
    /// into the drive mode (`FUN_0085cad0`).
    pub const VEHICLE_HIT_GROUND: usize = 13;
    /// The turbo starting (`FUN_0085cad0`, `FUN_00733e10`).
    pub const VEHICLE_TURBO: usize = 14;
}

/// A rumble for the pad to start: what the movement asks for in an update.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rumble {
    pub kind: u32,
    pub duration: f32,
    pub strength: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_one_fades_both_motors_the_light_one_twice_as_fast() {
        let mut pad = Pad::default();
        assert_eq!(pad.step(), None);
        // The ground punch's: 0.55 for 0.7 s.
        pad.start(1, 0.7, 0.55);
        let first = pad.step().unwrap();
        assert!((first[0] - 0.1375).abs() < 1e-6 && (first[1] - 0.55).abs() < 1e-6);
        // After 21 updates (0.35 s) the light motor is done and the heavy one half way.
        let mut out = first;
        for _ in 0..21 {
            out = pad.step().unwrap();
        }
        assert!((out[0] - 0.068_75).abs() < 1e-4 && out[1].abs() < 1e-4, "{out:?}");
        // It ends with one update of nothing, 0.7 s in.
        let mut updates = 22;
        while let Some(out) = pad.step() {
            updates += 1;
            if out == [0.0, 0.0] {
                break;
            }
        }
        // 42 sixtieths added up are 0.7 to within rounding, on either side of it.
        assert!(updates == 43 || updates == 44, "{updates}");
        assert_eq!(pad.step(), None);
    }

    #[test]
    fn types_two_and_three_hold_the_heavy_motor() {
        for kind in [2, 3] {
            let mut pad = Pad::default();
            // RUMBLE_JUMP_LAND's strength, for a little over 12 updates.
            pad.start(kind, 0.21, 0.2);
            let mut held = 0;
            while let Some(out) = pad.step() {
                assert_eq!(out[1], 0.0);
                if out[0] > 0.0 {
                    assert_eq!(out[0], 0.2);
                    held += 1;
                }
            }
            assert_eq!(held, 13);
        }
        // A new effect replaces the one running; type 0 is silence.
        let mut pad = Pad::default();
        pad.start(2, 1.0, 1.0);
        pad.step();
        pad.start(0, 1.0, 1.0);
        assert_eq!(pad.step(), Some([0.0, 0.0]));
        assert_eq!(pad.step(), None);
    }

    /// `RUMBLE_CLIMB_MOVED` is type 3 for 0 seconds: it stops what was running and plays
    /// nothing itself.
    #[test]
    fn a_held_rumble_of_no_length_is_silence() {
        let mut pad = Pad::default();
        pad.start(1, 0.7, 0.55);
        pad.step();
        pad.start(3, 0.0, 0.4);
        assert_eq!(pad.step(), Some([0.0, 0.0]));
        assert_eq!(pad.step(), None);
    }

    #[test]
    fn a_rumble_weakens_with_distance() {
        // The ground punch's: near 1, far 25.
        let def = RumbleDef { kind: 1, duration: 0.7, strength: 0.55, scale_type: 1, distance_near: 1.0, distance_far: 25.0 };
        assert_eq!(def.strength_at(0.5), Some(0.55));
        assert!((def.strength_at(13.0).unwrap() - 0.275).abs() < 1e-6);
        assert_eq!(def.strength_at(25.0), None);
        assert_eq!(def.strength_at(40.0), None);
        assert_eq!(RumbleDef { scale_type: 0, ..def }.strength_at(40.0), Some(0.55));
        assert_eq!(RumbleDef { scale_type: 2, ..def }.strength_at(0.0), None);
    }
}
