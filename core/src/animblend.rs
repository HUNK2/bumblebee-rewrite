//! Moving cross-fades shared by the character and weapon objects. [game: the old player
//! remains an input to BlendAnimation, FUN_00748f00 / FUN_0053d600; nested blends keep
//! playing when another transition interrupts them]. No engine or asset ownership here.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Clock {
    pub set: u32,
    pub clip: u32,
    pub tick: f32,
    pub length: f32,
    pub speed: f32,
    pub looping: bool,
    pub exit_events: bool,
}

impl Clock {
    fn advance(&mut self, dt: f32) -> (f32, f32) {
        let before = self.tick;
        // Keep an unwrapped loop clock: samplers wrap it; event callers can see every lap.
        self.tick += dt * 60.0 * self.speed;
        if !self.looping {
            self.tick = self.tick.min(self.length);
        }
        (before, self.tick)
    }
}

#[derive(Clone, Debug)]
pub struct Blend {
    pub clock: Clock,
    from: Option<Box<Blend>>,
    elapsed: f32,
    duration: f32,
}

impl Blend {
    pub fn new(clock: Clock) -> Self {
        Self {
            clock,
            from: None,
            elapsed: 0.0,
            duration: 0.0,
        }
    }

    /// Replace the leading player while retaining its entire moving blend underneath.
    pub fn start(&mut self, clock: Clock, duration: f32) {
        let old = std::mem::replace(self, Self::new(clock));
        if duration > 0.001 {
            self.from = Some(Box::new(old));
            self.duration = duration;
        }
    }

    /// The incoming clock belongs to the simulation. All outgoing clocks and their fades
    /// advance here; only players permitting outgoing events invoke `events`. [game]
    pub fn update(&mut self, tick: f32, dt: f32, events: &mut impl FnMut(Clock, f32, f32)) {
        self.clock.tick = tick;
        if let Some(from) = self.from.as_mut() {
            from.advance(dt, events);
        }
        self.finish_fade(dt);
    }

    fn advance(&mut self, dt: f32, events: &mut impl FnMut(Clock, f32, f32)) {
        let (before, after) = self.clock.advance(dt);
        if self.clock.exit_events {
            events(self.clock, before, after);
        }
        if let Some(from) = self.from.as_mut() {
            from.advance(dt, events);
        }
        self.finish_fade(dt);
    }

    fn finish_fade(&mut self, dt: f32) {
        self.elapsed += dt;
        if self.elapsed >= self.duration {
            self.from = None;
        }
    }

    /// Evaluate the tree with the caller's sampler and blend arithmetic. This preserves
    /// nested quaternion blends instead of flattening their weights. [game]
    pub fn evaluate<T>(&self, sample: &impl Fn(Clock) -> T, mix: &impl Fn(T, T, f32) -> T) -> T {
        let pose = sample(self.clock);
        match &self.from {
            Some(from) => mix(
                from.evaluate(sample, mix),
                pose,
                (self.elapsed / self.duration).clamp(0.0, 1.0),
            ),
            None => pose,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock(clip: u32, tick: f32) -> Clock {
        Clock {
            set: 1,
            clip,
            tick,
            length: 100.0,
            speed: 1.0,
            looping: false,
            exit_events: false,
        }
    }

    fn position(c: Clock) -> f32 {
        c.clip as f32 * 100.0 + c.tick
    }
    fn mix(a: f32, b: f32, w: f32) -> f32 {
        a + (b - a) * w
    }

    #[test]
    fn outgoing_pose_keeps_moving_and_completed_fade_releases_it() {
        let mut b = Blend::new(clock(1, 10.0));
        b.start(clock(2, 0.0), 1.0);
        b.update(30.0, 0.5, &mut |_, _, _| {});
        assert_eq!(b.evaluate(&position, &mix), 185.0); // moving old 140, new 230
        b.update(60.0, 0.5, &mut |_, _, _| {});
        assert_eq!(b.evaluate(&position, &mix), 260.0);
        assert!(b.from.is_none());
    }

    #[test]
    fn interrupted_fade_keeps_both_previous_players_and_fade_moving() {
        let mut b = Blend::new(clock(1, 0.0));
        b.start(clock(2, 0.0), 1.0);
        b.update(15.0, 0.25, &mut |_, _, _| {});
        b.start(clock(3, 0.0), 1.0);
        b.update(15.0, 0.25, &mut |_, _, _| {});
        // Old tree is now half-way (130 -> 230 = 180), new is 315 at a quarter share.
        assert_eq!(b.evaluate(&position, &mix), 213.75);
    }

    #[test]
    fn outgoing_events_obey_the_clip_flag_and_keep_loop_crossings() {
        let mut old = clock(1, 95.0);
        old.looping = true;
        old.exit_events = true;
        let mut b = Blend::new(old);
        b.start(clock(2, 0.0), 0.3);
        let mut cues = Vec::new();
        b.update(12.0, 0.2, &mut |c, a, z| cues.push((c.clip, a, z)));
        assert_eq!(cues, [(1, 95.0, 107.0)]);
        b.start(clock(3, 0.0), 0.0);
        b.update(6.0, 0.1, &mut |c, a, z| cues.push((c.clip, a, z)));
        assert_eq!(cues.len(), 1);
    }
}
