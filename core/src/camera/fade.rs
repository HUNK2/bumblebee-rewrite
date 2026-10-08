//! Camera occluder alpha, from the active/recovering lists in the executable. [game]
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug)]
struct Entry {
    alpha: f32,
    seen: bool,
    active: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Fades {
    entries: BTreeMap<u64, Entry>,
}

impl Fades {
    /// A nonblocking collision object with fade bit5 set. A retouch keeps its alpha.
    pub fn touch(&mut self, id: u64) {
        let e = self.entries.entry(id).or_insert(Entry {
            alpha: 1.0,
            seen: false,
            active: true,
        });
        e.seen = true;
        e.active = true;
    }
    pub fn step(&mut self, dt: f32) {
        self.entries.retain(|_, e| {
            if e.active && !e.seen {
                e.active = false;
            }
            if e.active {
                e.alpha = (e.alpha - 4.0 * dt).max(0.45);
            } else {
                e.alpha = (e.alpha + 4.0 * dt).min(1.0);
            }
            e.seen = false;
            e.active || e.alpha < 1.0
        });
    }
    pub fn alpha(&self, id: u64) -> f32 {
        self.entries.get(&id).map_or(1.0, |e| e.alpha)
    }
    pub fn iter(&self) -> impl Iterator<Item = (u64, f32)> + '_ {
        self.entries.iter().map(|(&id, e)| (id, e.alpha))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fade_touch_recovers_in_the_same_update_and_retouch_keeps_alpha() {
        let mut f = Fades::default();
        for _ in 0..8 {
            f.touch(7);
            f.step(0.032);
        }
        assert_eq!(f.alpha(7), 0.45);
        f.step(0.032);
        assert!((f.alpha(7) - 0.578).abs() < 1e-6);
        f.touch(7);
        f.step(0.032);
        assert!((f.alpha(7) - 0.45).abs() < 1e-6);
        for _ in 0..8 {
            f.step(0.032);
        }
        assert_eq!(f.iter().count(), 0);
        assert_eq!(f.alpha(7), 1.0);
    }
}
