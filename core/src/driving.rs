//! The drive mode's trigger filter and drift turbo, read from the installed executable.
//! See `notes/driving.md`, "Assumptions replaced (2026-10-07)".

use crate::tuning::DriveTuning;

/// DriveMode +0x204 (`0085cd56..0085cda0`). A nonpositive filter strength, or a
/// strength * dt of at least one, copies the trigger immediately. [game]
pub fn filter_trigger(previous: f32, trigger: f32, strength: f32, dt: f32) -> f32 {
    if strength > 0.0 && strength * dt < 1.0 {
        previous + (trigger - previous) * strength * dt
    } else {
        trigger
    }
}

/// The player branch of `0085f8b0`: the mode update passes dt>0 and uses the filtered
/// trigger; the tyre sound update passes dt=0 and skips it (`0085fa3e..0085fa65`).
/// NPC corner limits and navigation overrides are separate branches. [game]
pub fn allowed_speed(t: &DriveTuning, filtered_trigger: f32, use_trigger: bool) -> f32 {
    t.max_speed * if use_trigger { filtered_trigger } else { 1.0 }
}

/// Standard turbo's registered ability keeps ticking outside DriveMode. Expiry starts
/// a full cooldown; unused time in that update is not taken from it. [game: 00733180]
pub fn step_turbo(left: &mut f32, cooldown: &mut f32, cooldown_time: f32, dt: f32) -> bool {
    if *left > 0.0 {
        *left = (*left - dt).max(0.0);
        if *left == 0.0 {
            *cooldown = cooldown_time;
            return true;
        }
    } else {
        *cooldown = (*cooldown - dt).max(0.0);
    }
    false
}

/// Single-player upgrade scaling at activation and expiry (00732e60). [game]
/// Level0 or an unavailable level has multiplier1 (0079d060).
pub fn turbo_times(t: &DriveTuning, levels: [u8; 2]) -> (f32, f32) {
    let scale = |i: usize| levels[i].checked_sub(1)
        .and_then(|level| t.turbo_upgrades[i].get(level as usize))
        .copied().filter(|v| *v > 0.0).unwrap_or(1.0);
    (t.max_turbo_time * scale(0), t.max_turbo_cooldown_time * scale(1))
}

/// Timed drift ability ea8020ae, DriveMode +0x24, vtable 00b5d630. Its reset starts
/// uncharged (state3); sufficient lateral motion arms it (state0); release starts it
/// (state1). It has no cooldown state. [game: 00733ab0, 00733e10, 00734170]
#[derive(Clone, Copy, Debug, Default)]
pub struct DriftTurbo {
    charge: f32,
    charged: bool,
    left: Option<f32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DriftEvent {
    pub started: bool,
    pub ended: bool,
}

impl DriftTurbo {
    pub fn active(&self) -> bool { self.left.is_some() }

    /// `lateral_speed` is the body's velocity dotted with its right axis, not total
    /// speed. The update does not test wheel contact. Once charged, losing lateral
    /// motion resets the elapsed charge but keeps the ready latch while held. [game]
    pub fn step(&mut self, lateral_speed: f32, held: bool, turbo_active: bool, t: &DriveTuning, dt: f32) -> DriftEvent {
        if let Some(left) = self.left.as_mut() {
            *left -= dt;
            // Unlike standard turbo, zero is still active (`00733e98..00733ea3`).
            if *left < 0.0 {
                self.left = None;
                self.charged = false;
                return DriftEvent { ended: true, ..Default::default() };
            }
            return DriftEvent::default();
        }
        if held && lateral_speed.abs() > t.min_drift_turbo_speed {
            self.charge += dt;
            if self.charge >= t.min_drift_time { self.charged = true; }
        } else {
            self.charge = 0.0;
        }
        if self.charged && !turbo_active {
            if !held {
                self.left = Some(t.drift_turbo_time);
                return DriftEvent { started: true, ..Default::default() };
            }
        } else {
            self.charged = false;
        }
        DriftEvent::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tuning() -> DriveTuning {
        DriveTuning { min_drift_time: 0.25, min_drift_turbo_speed: 20.0,
            drift_turbo_time: 2.0, max_speed: 35.0, ..Default::default() }
    }

    #[test]
    fn drift_charge_needs_continuous_lateral_motion_strictly_over_the_threshold() {
        let t = tuning();
        let mut d = DriftTurbo::default();
        d.step(20.0, true, false, &t, 0.5);
        assert!(!d.step(0.0, false, false, &t, 0.032).started);
        d.step(25.0, true, false, &t, 0.2);
        d.step(0.0, true, false, &t, 0.032);
        d.step(-25.0, true, false, &t, 0.2);
        assert!(!d.step(0.0, false, false, &t, 0.032).started);
        d.step(-25.0, true, false, &t, 0.25);
        assert!(d.step(0.0, false, false, &t, 0.032).started);
    }

    #[test]
    fn charged_drift_keeps_ready_until_release_and_runs_even_with_drift_held_again() {
        let t = tuning();
        let mut d = DriftTurbo::default();
        d.step(25.0, true, false, &t, 0.25);
        d.step(0.0, true, false, &t, 0.5);
        assert!(d.step(0.0, false, false, &t, 0.032).started);
        assert!(!d.step(0.0, true, false, &t, 2.0).ended);
        assert!(d.active(), "zero remaining is still state1");
        assert!(d.step(0.0, true, false, &t, 0.032).ended);
        assert!(!d.active());
        assert!(!d.step(0.0, false, false, &t, 0.032).started);
    }

    #[test]
    fn standard_turbo_blocks_drift_release_and_consumes_the_charge() {
        let t = tuning();
        let mut d = DriftTurbo::default();
        d.step(25.0, true, false, &t, 0.25);
        assert!(!d.step(0.0, false, true, &t, 0.032).started);
        assert!(!d.step(0.0, false, false, &t, 0.032).started);
    }

    #[test]
    fn standard_turbo_expires_at_zero_and_starts_a_full_cooldown() {
        let t = DriveTuning { max_turbo_cooldown_time: 3.2, ..tuning() };
        let (mut left, mut cooldown) = (0.032, 0.0);
        assert!(step_turbo(&mut left, &mut cooldown, t.max_turbo_cooldown_time, 0.032));
        assert_eq!(left, 0.0);
        assert_eq!(cooldown, 3.2);
        assert!(!step_turbo(&mut left, &mut cooldown, t.max_turbo_cooldown_time, 0.2));
        assert_eq!(cooldown, 3.0);
    }

    #[test]
    fn trigger_filter_and_sound_cap_have_different_call_rules() {
        let t = tuning();
        let filtered = filter_trigger(0.0, 1.0, 10.0, 0.032);
        assert!((filtered - 0.32).abs() < 1e-6);
        assert!((allowed_speed(&t, filtered, true) - 11.2).abs() < 1e-5);
        assert_eq!(allowed_speed(&t, filtered, false), 35.0);
        for strength in [-1.0, 0.0, 32.0] {
            assert_eq!(filter_trigger(0.0, 0.75, strength, 0.032), 0.75);
        }
    }

    #[test]
    fn captured_turbo_upgrade_doubles_duration_and_halves_cooldown() {
        // [data/trace] Base values in block15 and the installed Upgrades table.
        // Capture's completed boosts last4.771..4.799s (activation update consumed).
        let t = DriveTuning { max_turbo_time: 2.4, max_turbo_cooldown_time: 3.2,
            turbo_upgrades: [[1.25,1.5,2.0],[0.85,0.65,0.5]], ..tuning() };
        assert_eq!(turbo_times(&t,[0,0]),(2.4,3.2));
        assert_eq!(turbo_times(&t,[3,3]),(4.8,1.6));
        assert_eq!(turbo_times(&t,[4,4]),(2.4,3.2));
        let (mut left,cooldown_time)=turbo_times(&t,[3,3]);
        let mut cooldown=0.0;
        for _ in 0..149 {assert!(!step_turbo(&mut left,&mut cooldown,cooldown_time,0.032));}
        assert!(left>0.0 && left<0.033);
        // Float roundoff may carry the last fraction into update151.
        step_turbo(&mut left,&mut cooldown,cooldown_time,0.032);
        if left>0.0 {step_turbo(&mut left,&mut cooldown,cooldown_time,0.032);}
        assert_eq!((left,cooldown),(0.0,1.6));
    }

    #[test]
    fn installed_upgrade_tables_match_the_captured_turbo_timers() {
        let dir=std::env::var("TF2_GAME_DIR").unwrap_or_else(|_|"C:/Games2".into());
        let t=crate::tuning::load(std::path::Path::new(&dir),"Bumblebee").expect("installed tuning");
        assert_eq!(turbo_times(&t.drive,[0,0]),(2.4,3.2));
        assert_eq!(turbo_times(&t.drive,[3,3]),(4.8,1.6));
    }
}
