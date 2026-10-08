//! The marks his tyres leave: the game's `TireSkidMark` (`notes\particles.md` "Skid
//! marks"). One sits on each tyre's node of the car. It is a chain of at most fifty points
//! on the ground, drawn as a strip of quads.
//!
//! Read from the class's code: the constructor `FUN_0080a9a0`, the loader `FUN_0080aa10`,
//! the trigger `FUN_0080bb30`, the update `FUN_0080afa0`, the point taker `FUN_0080bbe0`
//! with `FUN_0080bab0`, and the drawing `FUN_0080b0f0`. The vehicle's update
//! (`FUN_007347c0`) sends the messages.

use std::collections::VecDeque;

use glam::Vec3;

use crate::sound::{TyreInput, skidding};

/// The points a mark holds (the fifty slots at `+0x60`). [game]
pub const POINTS: usize = 50;
/// How far over the wheel's contact a mark's first point is put, and the later ones
/// (`FUN_0080bb30`, `FUN_0080afa0`). [game]
pub const FIRST_LIFT: f32 = 0.01;
pub const LIFT: f32 = 0.1;

/// A `TireSkidMark`'s data. [data] The defaults are the constructor's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SkidMarkDef {
    /// The node the thing sits on (his: an effect node by each tyre). The points are put
    /// at the node `tyre` names (`FUN_0080bc30`).
    pub node: usize,
    /// `tireName`: the hash of the name of the tyre's node and wheel.
    pub tyre: u32,
    /// `skidPointLength`: how far the tyre goes before the next point.
    pub point_length: f32,
    /// `skidPointWidth`: how far the strip reaches to EACH side of the points.
    pub point_width: f32,
    /// `skidPointLifetime`. Stored at `+0x54` and read by nothing in the class: a mark
    /// goes only when its points are taken for new ones. [game]
    pub lifetime: f32,
    /// `skidColor`, each byte over 255.
    pub colour: [f32; 4],
    /// The material's id (his: `bumblebeevehicle.fx_tireskid01`).
    pub material: u32,
}

impl Default for SkidMarkDef {
    fn default() -> Self {
        Self { node: 0, tyre: 0, point_length: 5.0, point_width: 0.25, lifetime: 10.0, colour: [0.0; 4], material: 0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Point {
    pos: Vec3,
    /// The game's clock when it was put down (the oldest is the one taken again).
    time: f32,
    /// No quad leads to this point: a strip starts here.
    breaks: bool,
}

/// A corner of the strip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vertex {
    pub pos: Vec3,
    pub uv: [f32; 2],
    pub colour: [f32; 4],
}

/// What the vehicle's update tells the marks in one update (`FUN_007347c0`, in its loop
/// over the wheels): `Trigger` while the tyres skid on a surface above 1 m/s, the stop
/// (message `9caa723d`) when they do not skid or he is slow, nothing when they skid over
/// no surface. [game] The message goes to his object by the class's name, so to all four
/// marks at once.
pub fn message(input: &TyreInput) -> Option<bool> {
    if !skidding(input) || input.speed <= 1.0 {
        Some(false)
    } else {
        (input.surface != 0).then_some(true)
    }
}

/// One tyre's mark.
#[derive(Clone, Debug, Default)]
pub struct SkidMark {
    /// `+0x44`: set by `Trigger`, cleared by the stop.
    active: bool,
    /// Oldest first.
    points: VecDeque<Point>,
}

impl SkidMark {
    pub fn active(&self) -> bool {
        self.active
    }

    pub fn len(&self) -> usize {
        self.points.len()
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// A free point, else the oldest one's (`FUN_0080bbe0`); the point after a removed
    /// first one starts a strip (`FUN_0080bab0`). [game]
    fn put(&mut self, point: Point) {
        if self.points.len() >= POINTS {
            self.points.pop_front();
            if let Some(first) = self.points.front_mut() {
                first.breaks = true;
            }
        }
        self.points.push_back(point);
    }

    /// `Trigger` (`FUN_0080bb30`): a mark that is not going starts one, with a first point
    /// at the tyre's node, at the height of the wheel's contact. [game]
    pub fn trigger(&mut self, tyre: Vec3, contact_z: f32, now: f32) {
        if self.active {
            return;
        }
        self.active = true;
        self.put(Point { pos: Vec3::new(tyre.x, tyre.y, contact_z + FIRST_LIFT), time: now, breaks: true });
    }

    /// The stop (vtable `+0x1c`, `FUN_007ca780`). [game]
    pub fn stop(&mut self) {
        self.active = false;
    }

    /// One update (`FUN_0080afa0`): going, and with the wheel on the ground, a point is
    /// added once the tyre is more than `skidPointLength` from the last one. The new
    /// point starts a strip when the wheel was in the air the update before (wheel
    /// `+0x92`). [game]
    pub fn update(&mut self, def: &SkidMarkDef, tyre: Vec3, contact_z: f32, on_ground: bool, was_on_ground: bool, now: f32) {
        let Some(last) = self.points.back().filter(|_| self.active && on_ground) else { return };
        let pos = Vec3::new(tyre.x, tyre.y, contact_z + LIFT);
        if pos.distance(last.pos) > def.point_length {
            self.put(Point { pos, time: now, breaks: !was_on_ground });
        }
    }

    /// The strip (`FUN_0080b0f0`): a quad from each point back to the one before it,
    /// unless the point starts a strip. Its corners are `skidPointWidth` to either side,
    /// along the step turned a quarter about z; where quads follow each other the next
    /// one starts on the corners the last one ended on. The colour is `skidColor`, with
    /// no alpha at a strip's first point and at its last one. The picture is laid once
    /// over each quad. Corners in the order the game writes them (two triangles: 0 1 2,
    /// 2 3 0); the normal is +z. [game]
    pub fn quads(&self, def: &SkidMarkDef) -> Vec<[Vertex; 4]> {
        let mut out = Vec::new();
        let mut carried = (Vec3::ZERO, Vec3::ZERO);
        for i in 1..self.points.len() {
            let (from, to) = (self.points[i - 1], self.points[i]);
            if to.breaks {
                continue;
            }
            let step = to.pos - from.pos;
            let length = step.length();
            let along = if length > 1e-8 { step / length } else { step };
            let side = Vec3::new(along.y, -along.x, 0.0) * def.point_width;
            let mut start_alpha = def.colour[3];
            if from.breaks {
                carried = (from.pos - side, from.pos + side);
                start_alpha = 0.0;
            }
            let ends = self.points.get(i + 1).is_none_or(|next| next.breaks);
            let end_alpha = if ends { 0.0 } else { def.colour[3] };
            let colour = |alpha: f32| [def.colour[0], def.colour[1], def.colour[2], alpha];
            let (far_left, far_right) = (to.pos - side, to.pos + side);
            out.push([
                Vertex { pos: carried.0, uv: [0.0, 0.0], colour: colour(start_alpha) },
                Vertex { pos: carried.1, uv: [1.0, 0.0], colour: colour(start_alpha) },
                Vertex { pos: far_right, uv: [1.0, 1.0], colour: colour(end_alpha) },
                Vertex { pos: far_left, uv: [0.0, 1.0], colour: colour(end_alpha) },
            ]);
            carried = (far_left, far_right);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn his() -> SkidMarkDef {
        SkidMarkDef { node: 40, tyre: 0x9be3_dd9b, point_length: 2.0, point_width: 0.3, lifetime: 10.0, colour: [0.75; 4], material: 0xace5_0b6e }
    }

    /// A mark dragged along +y at 1 m an update from the origin.
    fn dragged(metres: usize) -> SkidMark {
        let mut mark = SkidMark::default();
        mark.trigger(Vec3::ZERO, 0.0, 0.0);
        for i in 1..=metres {
            mark.update(&his(), Vec3::new(0.0, i as f32, 0.3), 0.0, true, true, i as f32);
        }
        mark
    }

    #[test]
    fn a_point_every_two_metres_once_triggered() {
        let mut mark = SkidMark::default();
        mark.update(&his(), Vec3::new(0.0, 9.0, 0.3), 0.0, true, true, 0.0);
        assert!(mark.is_empty(), "nothing before the trigger");
        // The first point is at the contact + 0.01, the later ones + 0.1, so the first
        // step is a little over the ground distance: 2 m along is already over 2. After
        // that a point needs MORE than 2 m, so at 1 m an update one comes every 3 m.
        let mark = dragged(8);
        assert_eq!(mark.len(), 4);
        assert_eq!(mark.points[0].pos, Vec3::new(0.0, 0.0, FIRST_LIFT));
        assert_eq!(mark.points[1].pos, Vec3::new(0.0, 2.0, LIFT));
        assert_eq!(mark.points[3].pos, Vec3::new(0.0, 8.0, LIFT));
        // A second trigger while it goes adds nothing.
        let mut again = mark.clone();
        again.trigger(Vec3::new(0.0, 8.5, 0.3), 0.0, 9.0);
        assert_eq!(again.len(), 4);
    }

    #[test]
    fn the_strip_joins_its_quads_and_fades_at_both_ends() {
        let def = his();
        let quads = dragged(8).quads(&def);
        assert_eq!(quads.len(), 3);
        // 0.3 to each side, across the way it went.
        // (A hair under 0.3: the step rises 0.09 and its flat part is what is turned.)
        assert!(quads[0][0].pos.distance(Vec3::new(-0.3, 0.0, FIRST_LIFT)) < 1e-3);
        assert!(quads[0][1].pos.distance(Vec3::new(0.3, 0.0, FIRST_LIFT)) < 1e-3);
        assert!((quads[0][2].pos.x - 0.3).abs() < 1e-3 && (quads[0][3].pos.x + 0.3).abs() < 1e-3);
        // Each quad starts where the one before ended.
        assert_eq!((quads[1][0].pos, quads[1][1].pos), (quads[0][3].pos, quads[0][2].pos));
        // No alpha at the first point and at the last; the colour's between.
        assert_eq!((quads[0][0].colour[3], quads[0][2].colour[3]), (0.0, 0.75));
        assert_eq!((quads[2][0].colour[3], quads[2][2].colour[3]), (0.75, 0.0));
        assert_eq!(quads[1].map(|v| v.uv), [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]);
        // One quad alone has no alpha anywhere.
        assert!(dragged(3).quads(&def)[0].iter().all(|v| v.colour[3] == 0.0));
    }

    #[test]
    fn stopping_and_leaving_the_ground_break_the_strip() {
        let def = his();
        let mut mark = dragged(8);
        mark.stop();
        mark.update(&def, Vec3::new(0.0, 20.0, 0.3), 0.0, true, true, 20.0);
        assert_eq!(mark.len(), 4, "a stopped mark adds nothing");
        mark.trigger(Vec3::new(0.0, 30.0, 0.3), 0.0, 30.0);
        for i in 31..=35 {
            mark.update(&def, Vec3::new(0.0, i as f32, 0.3), 0.0, true, true, i as f32);
        }
        // 0, 2, 5, 8 | 30, 32, 35: no quad across the gap.
        assert_eq!(mark.len(), 7);
        assert_eq!(mark.quads(&def).len(), 5);
        // In the air nothing is added; the point after the landing starts a new strip.
        mark.update(&def, Vec3::new(0.0, 40.0, 2.0), 0.0, false, true, 40.0);
        assert_eq!(mark.len(), 7);
        mark.update(&def, Vec3::new(0.0, 45.0, 0.3), 0.0, true, false, 45.0);
        assert_eq!(mark.len(), 8);
        assert_eq!(mark.quads(&def).len(), 5);
    }

    #[test]
    fn the_fifty_first_point_takes_the_oldest() {
        let def = his();
        let mark = dragged(3 * POINTS + 20);
        assert_eq!(mark.len(), POINTS);
        assert!(mark.points[0].breaks, "the new first point starts the strip");
        assert!(mark.points[0].pos.y > 2.0);
        assert_eq!(mark.quads(&def).len(), POINTS - 1);
        assert_eq!(mark.quads(&def)[0][0].colour[3], 0.0);
    }

    #[test]
    fn the_vehicle_triggers_the_marks_while_the_tyres_skid() {
        const PAVE: u32 = 0x37f7_18bf;
        let sliding = TyreInput { speed: 20.0, cap: 40.0, drift: true, grounded: true, surface: PAVE, ..Default::default() };
        assert_eq!(message(&sliding), Some(true));
        assert_eq!(message(&TyreInput { surface: 0, ..sliding }), None);
        assert_eq!(message(&TyreInput { drift: false, ..sliding }), Some(false));
        // The brake counts as a skid at any speed, but the marks stop at 1 m/s.
        assert_eq!(message(&TyreInput { drift: false, brake: 1.0, speed: 0.5, ..sliding }), Some(false));
    }
}
