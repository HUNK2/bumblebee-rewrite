//! Climbing walls: what counts as one, the frame the climb works in, and the climb mode's
//! own rules. Read from the original's code and checked against the 12 climbs in run 1
//! (`notes\climbing.md`). Tags as in `sim.rs`.

use std::f32::consts::{FRAC_1_SQRT_2, FRAC_PI_4, PI, SQRT_2};

use glam::{Vec2, Vec3};

use crate::formats::anim::TICKS_PER_SECOND;
use crate::formats::hash::crc32;
use crate::sim::World;
use crate::tuning::{ActionClip, Tuning};

/// How far his body shape rides over the ground (owner `+0x140`, one per form): the shape's
/// builder (`FUN_006e7e90`) gives the pencil the top 6/11 of his height, takes 5/11 of that
/// for the gap under it and stores 0.8 of the gap. 1.071 for the robot's 5.4, and 1.07 in
/// run 1. The climbable test starts this far above his feet, and the climb lets go at the
/// bottom by it. [game]
pub fn body_offset(height: f32) -> f32 {
    height * (6.0 / 11.0) * (5.0 / 11.0) * 0.8
}
/// Stick deflection (squared) below which the climb treats the stick as centred. [game]
const STICK_DEAD_SQUARED: f32 = 0.01;
/// The hand changes once each time an up or down clip passes this share of its length. [game]
const HAND_SWAP_AT: f32 = 0.3;
/// Blend time for clips with none given. [assumed]
const DEFAULT_CROSSFADE: f32 = 0.2;

/// The messages the climb mode sends the control state machine. [game]
pub mod message {
    pub const EXIT_PULLUP: u32 = 0x9879_9ecd;
    pub const EXIT_BOTTOM: u32 = 0xa884_a9c4;
    pub const EXIT_JUMP_UP: u32 = 0x0230_a6e3;
}

/// One step up in the level's ledge mesh, as the original's sweep reports it: where the line
/// along his facing first crosses into ground higher than where it started.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ledge {
    /// The crossing, on the ground plane.
    pub hit: Vec2,
    /// The wall's normal on the ground plane, pointing out of it toward where he stands.
    pub normal: Vec2,
    /// The ground in front of the wall and on top of it.
    pub bottom: f32,
    pub top: f32,
    /// How far the wall runs on from the crossing: to his left and to his right as he faces it.
    pub left: f32,
    pub right: f32,
    /// What the top's own cell carries: more height to a ledge above (0: none). [game]
    pub extra: f32,
}

/// A wall being climbed, as the original keeps it on the character (`+0x350`). The frame: x
/// along the wall from its left end, y into the wall, z up from its bottom.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wall {
    /// World to wall for row vectors (`local = p * M`): three rows and a translation row.
    pub m: [Vec3; 4],
    /// The frame's own axes and origin in the world (the inverse of `m`).
    pub axes: [Vec3; 3],
    pub origin: Vec3,
    pub width: f32,
    pub base_width: f32,
    pub left_extension: f32,
    pub height: f32,
    pub extra: f32,
}

impl Wall {
    /// The wall frame the original builds from its sweep (`FUN_007d6b70`). Where the ledge
    /// turns by less than 45 degrees at an end the original lets the wall run on past it;
    /// the box arena has no such corners, so that is not built. [game]
    pub fn from_ledge(ledge: &Ledge) -> Self {
        let n = ledge.normal;
        let x = Vec3::new(-n.y, n.x, 0.0);
        let y = Vec3::new(-n.x, -n.y, 0.0);
        let z = Vec3::Z;
        let origin = (ledge.hit - Vec2::new(x.x, x.y) * ledge.left).extend(ledge.bottom);
        let t = Vec3::new(-origin.dot(x), -origin.dot(y), -origin.dot(z));
        Self {
            m: [Vec3::new(x.x, y.x, z.x), Vec3::new(x.y, y.y, z.y), Vec3::new(x.z, y.z, z.z), t],
            axes: [x, y, z],
            origin,
            width: ledge.left + ledge.right,
            base_width: ledge.left + ledge.right,
            left_extension: 0.0,
            height: ledge.top - ledge.bottom,
            extra: ledge.extra,
        }
    }

    /// A point in the wall's frame.
    pub fn local(&self, p: Vec3) -> Vec3 {
        let m = &self.m;
        Vec3::new(
            m[0].x * p.x + m[1].x * p.y + m[2].x * p.z + m[3].x,
            m[0].y * p.x + m[1].y * p.y + m[2].y * p.z + m[3].y,
            m[0].z * p.x + m[1].z * p.y + m[2].z * p.z + m[3].z,
        )
    }

    /// A point of the wall's frame in the world.
    pub fn world(&self, local: Vec3) -> Vec3 {
        self.origin + self.axes[0] * local.x + self.axes[1] * local.y + self.axes[2] * local.z
    }

    /// Straight into the wall, along the ground.
    pub fn inward(&self) -> Vec2 {
        Vec2::new(self.axes[1].x, self.axes[1].y)
    }
}

/// The climbable test (`connectionEnvClimbable`, `FUN_00723f50` and `FUN_007d6b70`): a wall
/// along his facing, close enough, at least a jump high, and, for a player, faced within 45
/// degrees. `radius` and `height` are the avatar's; `min_height` the jump block's
/// `jumpHeight`. [game]
pub fn find(level: &dyn World, pos: Vec3, facing: Vec2, radius: f32, height: f32, min_height: f32) -> Option<Wall> {
    let start = pos + Vec3::Z * body_offset(height);
    let reach = (radius + 2.0) * SQRT_2;
    let ledge = level.ledge(start, facing, reach)?;
    // Anything lower than a jump is a ledge to jump onto, not to climb.
    if ledge.top - ledge.bottom < min_height {
        return None;
    }
    if (Vec2::new(start.x, start.y) - ledge.hit).dot(ledge.normal) >= radius + 2.0 {
        return None;
    }
    if facing.dot(ledge.normal) > -FRAC_1_SQRT_2 {
        return None;
    }
    Some(Wall::from_ledge(&ledge))
}

/// What the climb mode is doing (its `+0x18`). [game]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// Just caught hold.
    Catch,
    Up,
    Down,
    Hang,
    ShimLeft,
    ShimRight,
    PullUp,
}

/// The climb mode's state. [game]
#[derive(Clone, Debug)]
pub struct Climb {
    pub wall: Wall,
    pub phase: Phase,
    /// The right hand leads (`+0x1c`); the clips come in a left and a right version.
    pub right_hand: bool,
    /// The speed he arrived with, dying away (`+0x20`).
    pub arrival: Vec3,
    /// Where the top was when he went over it, and which way he faced (`+0x2c`, `+0x38`).
    pub top: Vec3,
    pub top_facing: Vec2,
    /// The clip, by the CRC-32 of its id, its tick and whether it has run out.
    pub clip: u32,
    pub tick: f32,
    last_tick: f32,
    pub finished: bool,
    /// The root's velocity when the clip changed, faded out over the new clip's blend time.
    carry: Vec3,
    fade: f32,
    fade_time: f32,
    /// The share of the clip played at the last look, and whether the hand has changed this
    /// time round (`+0x44`, `+0x48`).
    clip_share: f32,
    swapped: bool,
    /// The first update is still to come (`+0x49`).
    first: bool,
}

/// The id of a climb clip in the left or right hand's version.
fn hand_clip(right_hand: bool, stem: &str) -> u32 {
    crc32(format!("Wall{}_{stem}", if right_hand { "R" } else { "L" }).as_bytes())
}

/// The clips he catches hold with. They are not the mode's: `stateClimb`'s enter
/// (`FUN_0087a700`) starts `ClimbSet`, and the set's own entry rules pick `Wall_Catch` when
/// `I_IN_AIR` is 1 and `Wall_Catch_Ground` otherwise (the same clip, blended in over 0.2 s
/// instead of cut to). [data] The mode's enter then asks for a clip by the hash 685a280c,
/// which is the id of no clip in any pack of the game, so that request starts nothing and
/// the set's clip plays on; state 0 ends when whatever is playing has finished. [game]
pub const CATCH_CLIP: &str = "Wall_Catch";
pub const CATCH_CLIP_GROUND: &str = "Wall_Catch_Ground";

impl Climb {
    /// The climb mode's enter (`FUN_00857dd0`): the speed he brings, less its part along the
    /// second row of the wall's matrix, is kept; arriving above the pull-up line, it is
    /// replaced by a drop that ends exactly on it. `pos` is moved to the side limit if he
    /// caught hold beyond it. `in_air` is the state machine's ground flag, clear. [game]
    pub fn enter(wall: Wall, pos: &mut Vec3, vel: Vec3, t: &Tuning, radius: f32, in_air: bool) -> Self {
        let row = wall.m[1];
        let mut arrival = vel - row * vel.dot(row);
        let local = wall.local(*pos);
        let edge = t.climb.edge_limit_dist;
        let x = if wall.base_width <= edge * 2.0 {
            Some(wall.base_width * 0.5 + wall.left_extension)
        } else if local.x < edge {
            Some(edge)
        } else if local.x > wall.width - edge {
            Some(wall.width - edge)
        } else {
            None
        };
        if let Some(x) = x {
            *pos = wall.world(Vec3::new(x, -radius, local.z + 0.1));
        }
        let over = local.z - (wall.height - t.climb.pull_up_dist);
        if over > 0.0 {
            arrival = Vec3::new(0.0, 0.0, -(over * 100.0).sqrt());
        }
        let mut climb = Self {
            wall,
            phase: Phase::Catch,
            right_hand: false,
            arrival,
            top: Vec3::ZERO,
            top_facing: Vec2::ZERO,
            clip: 0,
            tick: 0.0,
            last_tick: 0.0,
            finished: false,
            carry: Vec3::ZERO,
            fade: 1.0,
            fade_time: 0.0,
            clip_share: 0.0,
            swapped: false,
            first: true,
        };
        let catch = if in_air { CATCH_CLIP } else { CATCH_CLIP_GROUND };
        climb.play(crc32(catch.as_bytes()), Vec3::ZERO, t);
        climb
    }

    /// Starts a clip unless it is the one playing, as the original's player does.
    fn play(&mut self, clip: u32, root_velocity: Vec3, t: &Tuning) {
        if clip == self.clip {
            return;
        }
        let blend = t.actions.clips.get(&clip).map_or(DEFAULT_CROSSFADE, |c| if c.crossfade < 0.0 { DEFAULT_CROSSFADE } else { c.crossfade });
        self.clip = clip;
        self.tick = 0.0;
        self.last_tick = 0.0;
        self.finished = false;
        self.carry = root_velocity;
        self.fade_time = blend;
        self.fade = if blend > 0.0 { 0.0 } else { 1.0 };
    }

    fn set_phase(&mut self, phase: Phase, stem: &str, root_velocity: Vec3, t: &Tuning) {
        self.phase = phase;
        self.play(hand_clip(self.right_hand, stem), root_velocity, t);
    }

    pub fn clip_data<'a>(&self, t: &'a Tuning) -> Option<&'a ActionClip> {
        t.actions.clips.get(&self.clip)
    }

    /// Moves the clip on and returns the velocity its root motion gives this step, in his own
    /// frame (x right, y forward, z up), faded in over what the last clip was giving.
    fn advance(&mut self, t: &Tuning, dt: f32) -> Vec3 {
        let Some(data) = t.actions.clips.get(&self.clip) else { return Vec3::ZERO };
        self.last_tick = self.tick;
        self.tick += dt * TICKS_PER_SECOND * data.speed;
        let mut wrapped = false;
        if self.tick >= data.ticks {
            if data.looping {
                self.tick %= data.ticks.max(1.0);
                wrapped = true;
            } else {
                self.tick = data.ticks;
                self.finished = true;
            }
        }
        let own = if dt <= 1e-6 {
            Vec3::ZERO
        } else if wrapped {
            // Across the loop: the rest of the lap and the start of the next.
            (data.root_at(data.ticks) - data.root_at(self.last_tick) + data.root_at(self.tick) - data.root_at(0.0)) / dt
        } else {
            (data.root_at(self.tick) - data.root_at(self.last_tick)) / dt
        };
        self.fade = (self.fade + dt / self.fade_time.max(1e-3)).min(1.0);
        self.carry.lerp(own, self.fade)
    }

    /// The share of the clip played, for the hand change (`FUN_00858490`). [game]
    fn hand_change(&mut self, t: &Tuning) -> bool {
        let share = t.actions.clips.get(&self.clip).map_or(0.0, |c| if c.ticks > 0.001 { self.tick / c.ticks } else { 0.0 });
        let mut change = false;
        if share < self.clip_share {
            self.swapped = false;
        }
        if self.clip_share < HAND_SWAP_AT && share >= HAND_SWAP_AT && !self.swapped {
            change = true;
            self.swapped = true;
        }
        self.clip_share = share;
        change
    }
}

/// What the climb update needs of the body, and gives back.
pub struct Body<'a> {
    pub pos: &'a mut Vec3,
    pub yaw: &'a mut f32,
    pub radius: f32,
    pub height: f32,
    pub on_ground: bool,
}

fn yaw_of(direction: Vec2) -> f32 {
    (-direction.x).atan2(direction.y)
}

fn turn_toward(yaw: f32, target: f32, max_step: f32) -> f32 {
    let mut delta = (target - yaw) % std::f32::consts::TAU;
    if delta > PI {
        delta -= std::f32::consts::TAU;
    } else if delta < -PI {
        delta += std::f32::consts::TAU;
    }
    yaw + delta.clamp(-max_step, max_step)
}

/// One update of the climb mode (`FUN_00856d00`). `stick` is the pad's own (x right, y up),
/// `wall_now` the climbable test run again from where he is. Returns the velocity the clips'
/// root motion gives (world space) and the message for the state machine, if any. [game]
pub fn update(c: &mut Climb, body: Body, stick: Vec2, wall_now: Option<Wall>, t: &Tuning, dt: f32) -> (Vec3, Option<u32>) {
    let facing = Vec2::new(-body.yaw.sin(), body.yaw.cos());
    let root = c.advance(t, dt);
    let right = Vec2::new(facing.y, -facing.x);
    let velocity = (right * root.x + facing * root.y).extend(root.z);

    // The pull-up has played out.
    if c.phase == Phase::PullUp && c.finished {
        return (velocity, Some(message::EXIT_PULLUP));
    }
    // The wall is looked for again from where he is now; gone, he lets go.
    let Some(wall) = wall_now else {
        let message = (c.phase != Phase::PullUp).then_some(message::EXIT_BOTTOM);
        return (velocity, message);
    };
    c.wall = wall;
    let climb = &t.climb;
    let mut local = wall.local(*body.pos);

    // The speed he arrived with dies away; beyond the side limits its part along the first
    // row of the wall's matrix goes at once.
    let speed = c.arrival.length();
    let kept = (speed - climb.arrival_deceleration * dt).max(0.0);
    c.arrival = if speed > 0.0 { c.arrival * (kept / speed) } else { Vec3::ZERO };
    if wall.width - climb.edge_limit_dist < local.x || local.x < climb.edge_limit_dist {
        let row = wall.m[0];
        c.arrival -= row * c.arrival.dot(row);
    }
    if std::mem::take(&mut c.first) {
        // Onto the wall: at his radius off it, square to it.
        local.y = -body.radius;
        *body.pos = wall.world(local);
        *body.yaw = yaw_of(wall.inward());
    } else if local.y < -(body.radius + 0.1) {
        // Drifted off it: a tenth of the way back each update. The original skips this
        // while something touches him from ahead (owner `+0x34d`: the body's contact scan
        // `FUN_008524b0` sets it for a contact within 45 degrees of his facing), which is
        // why he hangs 2.1 to 2.2 off the walls of run 1 whose collision stands in front of
        // the ledge mesh. Here the wall he hangs on is the only thing ahead, and it does
        // not touch him when he is this far off it. [game]
        local.y = -body.radius * 0.1 + local.y * 0.9;
        *body.pos = wall.world(local);
    }
    if kept > 0.0 {
        *body.pos += c.arrival * dt;
    }

    let pushed = stick.length_squared() > STICK_DEAD_SQUARED;
    if c.phase == Phase::Catch {
        if c.finished || pushed {
            // Carrying something he would hang by the other hand; he never carries here.
            c.right_hand = false;
            c.set_phase(Phase::Hang, "Idle", velocity, t);
        }
        return (velocity, None);
    }
    let below_top = local.z <= wall.height - climb.pull_up_dist;
    let pulling = c.phase == Phase::PullUp;
    let mut message = None;
    if !pushed {
        if below_top {
            if !pulling {
                c.set_phase(Phase::Hang, "Idle", velocity, t);
            }
        } else {
            message = Some(over_the_top(c, *body.pos, facing, velocity, t));
        }
    } else {
        // Up is 0, left positive. [game]
        let angle = (-stick.x).atan2(stick.y);
        if !(-3.0 * FRAC_PI_4..3.0 * FRAC_PI_4).contains(&angle) {
            if local.z < 0.1 - body_offset(body.height) || body.on_ground {
                message = Some(message::EXIT_BOTTOM);
            } else if (c.phase != Phase::Down || c.finished) && !pulling {
                c.set_phase(Phase::Down, "ClimbDown", velocity, t);
            }
        } else if (-FRAC_PI_4..FRAC_PI_4).contains(&angle) {
            if below_top {
                if (c.phase != Phase::Up || c.finished) && !pulling {
                    c.set_phase(Phase::Up, "ClimbUp", velocity, t);
                }
            } else {
                message = Some(over_the_top(c, *body.pos, facing, velocity, t));
            }
        } else {
            let (phase, stem, allowed) = if angle >= FRAC_PI_4 {
                (Phase::ShimLeft, "ShimLeft", climb.edge_limit_dist <= local.x)
            } else {
                (Phase::ShimRight, "ShimRight", local.x <= wall.width - climb.edge_limit_dist)
            };
            if !pulling {
                if allowed {
                    c.set_phase(phase, stem, velocity, t);
                } else {
                    c.set_phase(Phase::Hang, "Idle", velocity, t);
                }
            }
            // Turned square to the wall at half a turn a second (`FUN_00717770`). [game]
            *body.yaw = turn_toward(*body.yaw, yaw_of(wall.inward()), PI * dt);
        }
    }
    if matches!(c.phase, Phase::Up | Phase::Down) && c.hand_change(t) {
        c.right_hand = !c.right_hand;
    }
    // The original also plays a pad rumble here (`FUN_00717c40`): `sim::step_climb` does it.
    (velocity, message)
}

/// At the top: the point over it and his facing are kept; with nothing more above, the
/// pull-up starts, else the jump up to the ledge above is asked for. The point and facing
/// are read by the jump state after `stateClimbJumpUp` (`FUN_0087dde0`, `sim.rs`). [game]
fn over_the_top(c: &mut Climb, pos: Vec3, facing: Vec2, velocity: Vec3, t: &Tuning) -> u32 {
    let wall = c.wall;
    c.top_facing = facing;
    c.top = Vec3::new(pos.x, pos.y, wall.origin.z + wall.height);
    if wall.extra <= 0.0 {
        if c.phase != Phase::PullUp {
            c.set_phase(Phase::PullUp, "PullUp", velocity, t);
        }
        message::EXIT_PULLUP
    } else {
        c.top.z += wall.extra;
        message::EXIT_JUMP_UP
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{Arena, Box3};
    use crate::tuning::ClimbTuning;

    /// The game's update length on the recording PC.
    const DT: f32 = 0.032;

    /// The climb set's clips with their real lengths, loops and root travel, the travel spread
    /// evenly over the ticks (`cargo run -p tf2-core --example set_dump -- ClimbSet`).
    fn tuning() -> Tuning {
        let mut t = Tuning::default();
        t.climb = ClimbTuning { pull_up_dist: 8.0, edge_limit_dist: 3.0, wall_offset: 0.0, arrival_deceleration: 50.0 };
        t.jump.standing.height = 6.0;
        let mut clip = |id: &str, ticks: usize, looping: bool, crossfade: f32, travel: Vec3| {
            let data = ActionClip {
                set: "ClimbSet".into(),
                id: id.into(),
                ticks: ticks as f32,
                speed: 1.0,
                crossfade,
                looping,
                path: (0..=ticks).map(|tick| travel * tick as f32 / ticks as f32).collect(),
                ..Default::default()
            };
            t.actions.clips.insert(crc32(id.as_bytes()), data);
        };
        clip("Wall_Catch", 34, false, 0.0, Vec3::ZERO);
        clip("Wall_Catch_Ground", 34, false, 0.2, Vec3::ZERO);
        for hand in ["L", "R"] {
            clip(&format!("Wall{hand}_ClimbUp"), 46, false, 0.0, Vec3::Z * 7.28);
            clip(&format!("Wall{hand}_ClimbDown"), 60, true, 0.2, Vec3::Z * -26.65);
            clip(&format!("Wall{hand}_Idle"), 100, true, 0.2, Vec3::ZERO);
            clip(&format!("Wall{hand}_ShimLeft"), 46, true, 0.2, Vec3::X * -7.5);
            clip(&format!("Wall{hand}_ShimRight"), 46, true, 0.2, Vec3::X * 7.5);
            clip(&format!("Wall{hand}_PullUp"), 56, false, 0.2, Vec3::new(0.0, 5.33, 7.45));
        }
        for hand in ["L", "R"] {
            // The pull-up rises over the top first and comes down onto it going forward: the
            // real root path every 7 ticks (forward, up).
            let samples = [(0.0, 0.0), (0.0, 1.2), (0.0, 3.0), (0.1, 5.0), (0.7, 7.4), (1.6, 9.3), (2.7, 9.8), (4.1, 8.8), (5.3, 7.4)];
            let path = (0..=56)
                .map(|tick| {
                    let (i, f) = (tick / 7, (tick % 7) as f32 / 7.0);
                    let (a, b) = (samples[i], samples[(i + 1).min(8)]);
                    Vec3::new(0.0, a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f)
                })
                .collect();
            t.actions.clips.get_mut(&crc32(format!("Wall{hand}_PullUp").as_bytes())).unwrap().path = path;
        }
        t
    }

    /// A wall like the first one climbed in run 1: its face at x = 0 facing -x, 107 wide and
    /// 43.3 high, standing on ground 14 below where he runs at it.
    fn arena() -> Arena {
        Arena { boxes: vec![Box3 { min: Vec3::new(0.0, -50.0, 0.0), max: Vec3::new(40.0, 57.0, 43.3) }], ..Default::default() }
    }

    const RADIUS: f32 = 2.0;
    const HEIGHT: f32 = 5.4;

    fn facing_wall() -> Vec2 {
        Vec2::X
    }

    /// One update of the climb against the arena, moving him as the movement would.
    fn update_in(c: &mut Climb, pos: &mut Vec3, yaw: &mut f32, stick: Vec2, arena: &Arena, t: &Tuning) -> Option<u32> {
        let facing = Vec2::new(-yaw.sin(), yaw.cos());
        let wall = find(arena, *pos, facing, RADIUS, HEIGHT, t.jump.standing.height);
        let on_ground = pos.z <= arena.floor(pos.x, pos.y, pos.z) + 0.01;
        let body = Body { pos: &mut *pos, yaw: &mut *yaw, radius: RADIUS, height: HEIGHT, on_ground };
        let (velocity, message) = update(c, body, stick, wall, t, DT);
        *pos += velocity * DT;
        arena.push_out(pos, RADIUS, HEIGHT);
        pos.z = pos.z.max(arena.floor(pos.x, pos.y, pos.z));
        message
    }

    #[test]
    fn the_wall_frame_is_laid_out_as_in_the_running_game() {
        // Run 1 at 545.6: facing (-1, 0), and the matrix rows (0, -1, 0), (1, 0, 0), (0, 0, 1).
        let ledge = Ledge { hit: Vec2::new(-160.0, 152.0), normal: Vec2::X, bottom: -14.0, top: 30.16, left: 9.0, right: 14.0, extra: 0.0 };
        let wall = Wall::from_ledge(&ledge);
        assert_eq!(wall.m[0], Vec3::new(0.0, -1.0, 0.0));
        assert_eq!(wall.m[1], Vec3::new(1.0, 0.0, 0.0));
        assert_eq!(wall.m[2], Vec3::Z);
        assert_eq!(wall.inward(), Vec2::NEG_X);
        assert!((wall.width - 23.0).abs() < 1e-4 && (wall.height - 44.16).abs() < 1e-3);
        // x from the left end, y negative in front, z from the bottom; and back again.
        let p = Vec3::new(-158.0, 150.0, 7.0);
        let local = wall.local(p);
        assert!((local - Vec3::new(7.0, -2.0, 21.0)).length() < 1e-3, "{local}");
        assert!((wall.world(local) - p).length() < 1e-3);
    }

    #[test]
    fn a_wall_is_climbable_close_up_faced_and_higher_than_a_jump() {
        let t = tuning();
        let arena = arena();
        let at = |x: f32, z: f32| Vec3::new(x, 0.0, z);
        let found = find(&arena, at(-2.0, 0.0), facing_wall(), RADIUS, HEIGHT, 6.0).expect("the wall in front");
        assert!((found.height - 43.3).abs() < 1e-3 && (found.width - 107.0).abs() < 1e-3);
        // x runs to his right from the wall's left end, here y = 57.
        assert!((found.local(at(-2.0, 0.0)).x - 57.0).abs() < 1e-3);
        // Reach is (radius + 2) times root 2 along the facing, and the plane within radius + 2.
        assert!(find(&arena, at(-3.9, 0.0), facing_wall(), RADIUS, HEIGHT, 6.0).is_some());
        assert!(find(&arena, at(-4.1, 0.0), facing_wall(), RADIUS, HEIGHT, 6.0).is_none());
        // Faced within 45 degrees, not beyond.
        let turned = |degrees: f32| Vec2::from_angle(degrees.to_radians()).rotate(facing_wall());
        assert!(find(&arena, at(-2.0, 0.0), turned(40.0), RADIUS, HEIGHT, 6.0).is_some());
        assert!(find(&arena, at(-2.0, 0.0), turned(50.0), RADIUS, HEIGHT, 6.0).is_none());
        // Lower than the jump height is a ledge to jump onto.
        let low = Arena { boxes: vec![Box3 { min: Vec3::new(0.0, -5.0, 0.0), max: Vec3::new(5.0, 5.0, 5.9) }], ..Default::default() };
        assert!(find(&low, at(-2.0, 0.0), facing_wall(), RADIUS, HEIGHT, 6.0).is_none());
        // Side by side, two boxes make one wall.
        let pair = Arena {
            boxes: vec![
                Box3 { min: Vec3::new(0.0, -10.0, 0.0), max: Vec3::new(5.0, 0.0, 20.0) },
                Box3 { min: Vec3::new(0.0, 0.0, 0.0), max: Vec3::new(8.0, 12.0, 25.0) },
            ],
            ..Default::default()
        };
        let joined = find(&pair, Vec3::new(-2.0, -5.0, 0.0), facing_wall(), RADIUS, HEIGHT, 6.0).unwrap();
        assert!((joined.width - 22.0).abs() < 1e-3, "{}", joined.width);
        let _ = t;
    }

    #[test]
    fn the_body_rides_over_the_ground_as_in_the_running_game() {
        // Run 1 reads 1.07 at owner `+0x140` for the robot.
        assert!((body_offset(HEIGHT) - 1.07).abs() < 0.005, "{}", body_offset(HEIGHT));
    }

    #[test]
    fn the_catch_clip_is_the_sets_own_choice() {
        let t = tuning();
        let arena = arena();
        let mut pos = Vec3::new(-2.0, 0.0, 9.0);
        let wall = find(&arena, pos, facing_wall(), RADIUS, HEIGHT, 6.0).unwrap();
        let jumped_at = Climb::enter(wall, &mut pos, Vec3::ZERO, &t, RADIUS, true);
        assert_eq!(jumped_at.clip, crc32(b"Wall_Catch"));
        let mut pos = Vec3::new(-2.0, 0.0, 0.0);
        let stood_at = Climb::enter(wall, &mut pos, Vec3::ZERO, &t, RADIUS, false);
        assert_eq!(stood_at.clip, crc32(b"Wall_Catch_Ground"));
        // From the ground the catch is blended in, not cut to.
        assert!(stood_at.fade < 1.0 && jumped_at.fade == 1.0);
    }

    #[test]
    fn arriving_speed_dies_away_at_fifty_a_second() {
        let t = tuning();
        let arena = arena();
        let mut pos = Vec3::new(-2.0, 0.0, 9.18);
        let mut yaw = -std::f32::consts::FRAC_PI_2;
        let wall = find(&arena, pos, facing_wall(), RADIUS, HEIGHT, 6.0).unwrap();
        // Run 1 at 137.08: (32.38, 0.13, -1.72) into the wall; the part into it goes, then
        // (0.13, -1.72) is down to (0.01, -0.11) one update later.
        let mut c = Climb::enter(wall, &mut pos, Vec3::new(32.38, 0.13, -1.72), &t, RADIUS, true);
        assert!((c.arrival - Vec3::new(0.0, 0.13, -1.72)).length() < 1e-4, "{}", c.arrival);
        update_in(&mut c, &mut pos, &mut yaw, Vec2::Y, &arena, &t);
        assert!((c.arrival.length() - 0.125).abs() < 0.01, "{}", c.arrival);
        // Snapped square to the wall at his radius off it.
        assert!((yaw + std::f32::consts::FRAC_PI_2).abs() < 1e-5 && (c.wall.local(pos).y + RADIUS).abs() < 0.05);
    }

    #[test]
    fn climbing_up_swaps_hands_and_pulls_up_within_eight_of_the_top() {
        let t = tuning();
        let arena = arena();
        let mut pos = Vec3::new(-2.0, 0.0, 9.0);
        let mut yaw = -std::f32::consts::FRAC_PI_2;
        let wall = find(&arena, pos, facing_wall(), RADIUS, HEIGHT, 6.0).unwrap();
        let mut c = Climb::enter(wall, &mut pos, Vec3::ZERO, &t, RADIUS, true);
        let mut hands = vec![];
        let mut pulled_at = None;
        let mut done = false;
        for update in 0..200 {
            let message = update_in(&mut c, &mut pos, &mut yaw, Vec2::Y, &arena, &t);
            if c.phase == Phase::Up && hands.last() != Some(&c.right_hand) {
                hands.push(c.right_hand);
            }
            if pulled_at.is_none() && c.phase == Phase::PullUp {
                pulled_at = Some((update, c.wall.local(pos).z));
                assert_eq!(message, Some(message::EXIT_PULLUP));
            }
            if c.phase == Phase::PullUp && c.finished {
                done = true;
                break;
            }
        }
        // Each climb clip hands over to the other hand's.
        assert!(hands.len() >= 4 && hands.windows(2).all(|w| w[0] != w[1]), "{hands:?}");
        // In run 1 the pull-up started at 36.13 on the 43.3 wall: past 35.3, within a climb
        // update's rise of it.
        let (_, z) = pulled_at.expect("pulled up");
        assert!(z > 35.3 && z < 35.3 + 0.6, "{z}");
        assert!(done && pos.z > 43.3 - 0.01 && pos.x > 1.0, "ends on the top: {pos}");
    }

    #[test]
    fn climbing_speeds_are_the_clips() {
        let t = tuning();
        let arena = arena();
        let start = Vec3::new(-2.0, 0.0, 10.0);
        let mut pos = start;
        let mut yaw = -std::f32::consts::FRAC_PI_2;
        let wall = find(&arena, pos, facing_wall(), RADIUS, HEIGHT, 6.0).unwrap();
        let mut c = Climb::enter(wall, &mut pos, Vec3::ZERO, &t, RADIUS, true);
        // Shimmy right (toward -y here, his right as he faces +x): 9.78 a second in run 1.
        for _ in 0..40 {
            update_in(&mut c, &mut pos, &mut yaw, Vec2::X, &arena, &t);
        }
        let before = pos;
        for _ in 0..31 {
            update_in(&mut c, &mut pos, &mut yaw, Vec2::X, &arena, &t);
        }
        let speed = (pos - before).length() / (31.0 * DT);
        assert!((speed - 9.78).abs() < 0.1 && pos.y < before.y, "{speed} {pos}");
        // Down: 26.65 a second in run 1.
        for _ in 0..10 {
            update_in(&mut c, &mut pos, &mut yaw, Vec2::NEG_Y, &arena, &t);
        }
        let before = pos;
        update_in(&mut c, &mut pos, &mut yaw, Vec2::NEG_Y, &arena, &t);
        assert!(((before.z - pos.z) / DT - 26.65).abs() < 0.1 && c.phase == Phase::Down);
    }

    #[test]
    fn shimmying_stops_at_the_side_limit() {
        let t = tuning();
        let arena = arena();
        let mut pos = Vec3::new(-2.0, -40.0, 10.0);
        let mut yaw = -std::f32::consts::FRAC_PI_2;
        let wall = find(&arena, pos, facing_wall(), RADIUS, HEIGHT, 6.0).unwrap();
        let mut c = Climb::enter(wall, &mut pos, Vec3::ZERO, &t, RADIUS, true);
        for _ in 0..200 {
            update_in(&mut c, &mut pos, &mut yaw, Vec2::X, &arena, &t);
        }
        // The right end is at y = -50: he stops about edgeLimitDist (3) in, at most one
        // update's travel and the blend past it.
        assert_eq!(c.phase, Phase::Hang);
        assert!(pos.y > -50.0 && pos.y < -46.0, "{pos}");
    }

    #[test]
    fn down_at_the_ground_lets_go_and_drift_is_pulled_back() {
        let t = tuning();
        let arena = arena();
        let mut pos = Vec3::new(-2.0, 0.0, 6.0);
        let mut yaw = -std::f32::consts::FRAC_PI_2;
        let wall = find(&arena, pos, facing_wall(), RADIUS, HEIGHT, 6.0).unwrap();
        let mut c = Climb::enter(wall, &mut pos, Vec3::ZERO, &t, RADIUS, true);
        let mut message = None;
        for _ in 0..60 {
            message = update_in(&mut c, &mut pos, &mut yaw, Vec2::NEG_Y, &arena, &t);
            if message.is_some() {
                break;
            }
        }
        assert_eq!(message, Some(message::EXIT_BOTTOM));
        assert!(pos.z.abs() < 1e-4);
        // Run 1 at 665.4: 3.28 off the wall, then a tenth of the way back each update.
        let mut c = Climb::enter(wall, &mut Vec3::new(-2.0, 0.0, 10.0), Vec3::ZERO, &t, RADIUS, true);
        c.first = false;
        let mut pos = Vec3::new(-3.28, 0.0, 10.0);
        let body = Body { pos: &mut pos, yaw: &mut yaw, radius: RADIUS, height: HEIGHT, on_ground: false };
        update(&mut c, body, Vec2::ZERO, Some(wall), &t, DT);
        assert!((pos.x + 3.152).abs() < 1e-4, "{pos}");
    }
}
