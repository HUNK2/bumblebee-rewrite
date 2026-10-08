//! RibbonTrail histories and mesh sampling. [game: FUN_00790180 / FUN_00790390,
//! cubic basis initialized by FUN_00ad9f60; data read by FUN_00790ef0]

use crate::formats::{anim::Clip, hash::crc32, lxb::Node};
use glam::Vec3;
use std::collections::VecDeque;

pub const TRIGGER: u32 = crc32(b"Trigger");
pub const STOP: u32 = 0x9caa_723d;
const INTERVAL: f32 = 1.0 / 60.0;

#[derive(Clone, Debug)]
pub struct Def {
    pub name: u32,
    pub links: Vec<u32>,
    pub material: u32,
    pub trails: usize,
    pub subdivisions: usize,
    /// Endpoint displacement threshold, despite the field's name. [game]
    pub trigger_speed: f32,
    pub enabled: bool,
}

pub fn read(node: Node) -> Option<Def> {
    let number = |name| node.get(name).and_then(Node::int).unwrap_or(0) as u32;
    let links = node
        .get("links")?
        .items()
        .filter_map(Node::label)
        .collect::<Vec<_>>();
    if links.len() < 2 {
        return None;
    }
    Some(Def {
        name: number("LuxScriptName"),
        links,
        material: node.get("material").and_then(Node::asset_text)?.0,
        trails: number("numTrails") as usize,
        subdivisions: number("numSubdivisions") as usize,
        trigger_speed: node
            .get("triggerSpeed")
            .and_then(Node::float)
            .unwrap_or(0.0),
        enabled: node
            .get("enabledOnInit")
            .is_some_and(|n| n.is_true() || n.int() == Some(1)),
    })
}

#[derive(Clone, Copy, Debug)]
pub struct TriggerEvent {
    pub time: f32,
    pub name: u32,
    pub message: u32,
    /// AnimExitTriggerEvent also sends its message when the player is left. [game:
    /// registration FUN_00addae0 installs the same timed and exit handler]
    pub on_exit: bool,
}

impl TriggerEvent {
    pub fn due(&self, from: f32, to: f32) -> bool {
        if to >= from {
            (self.time > from && self.time <= to) || (from == 0.0 && self.time == 0.0 && to > 0.0)
        } else {
            self.time > from || self.time <= to
        }
    }
}

pub fn read_events(keys: Node, out: &mut Vec<TriggerEvent>) {
    for key in keys.get("array").into_iter().flat_map(Node::items) {
        let Some(event) = key.get("value") else {
            continue;
        };
        let Some(kind @ ("AnimSendTriggerEvent" | "AnimExitTriggerEvent")) = event.type_name()
        else {
            continue;
        };
        // Named external objects have a separate lookup path; these are local messages.
        if event.get("name").and_then(Node::int).unwrap_or(0) != 0 {
            continue;
        }
        out.push(TriggerEvent {
            time: key.get("time").and_then(Node::float).unwrap_or(0.0),
            name: event.get("LuxScriptName").and_then(Node::int).unwrap_or(0) as u32,
            message: event.get("triggerMsg").and_then(Node::int).unwrap_or(0) as u32,
            on_exit: kind == "AnimExitTriggerEvent",
        });
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Vertex {
    pub position: Vec3,
    pub normal: Vec3,
    pub uv: [f32; 2],
}

pub struct Trail {
    pub enabled: bool,
    rows: VecDeque<Vec<Vec3>>,
    remainder: f32,
}

impl Trail {
    pub fn new(def: &Def) -> Self {
        Self {
            enabled: def.enabled,
            rows: VecDeque::new(),
            remainder: 0.0,
        }
    }

    pub fn message(&mut self, msg: u32) {
        match msg {
            TRIGGER => self.enabled = true,
            STOP => self.enabled = false,
            _ => {}
        }
    }

    /// One sample per update that crosses 1/60s, even when the update crosses several.
    /// Disabled/slow trails shrink to three support rows; drawing requires four. [game]
    pub fn update(&mut self, def: &Def, points: Vec<Vec3>, dt: f32) {
        if points.len() != def.links.len() || def.trails == 0 {
            return;
        }
        self.remainder += dt;
        if self.remainder < INTERVAL {
            return;
        }
        self.remainder %= INTERVAL;
        let moved = def.trigger_speed <= 0.0
            || points.iter().enumerate().any(|(i, at)| {
                let before = self
                    .rows
                    .front()
                    .and_then(|row| row.get(i))
                    .copied()
                    .unwrap_or(Vec3::ZERO);
                at.distance_squared(before) > def.trigger_speed * def.trigger_speed
            });
        let count = self.rows.len();
        let next = if (self.enabled && moved) || count < 3 {
            (count + 1).min(def.trails)
        } else {
            count.saturating_sub(1).max(3).min(def.trails)
        };
        self.rows.push_front(points);
        self.rows.truncate(next);
    }

    /// Duplicate each vertex with opposite normal, matching the original's two strips.
    pub fn mesh(&self, def: &Def, eye: Vec3) -> (Vec<Vertex>, Vec<u32>) {
        let n = self.rows.len();
        let width = def.links.len();
        if n < 4 || width < 2 {
            return (Vec::new(), Vec::new());
        }
        let ends = [
            self.rows[0][0],
            self.rows[0][width - 1],
            self.rows[n - 1][0],
            self.rows[n - 1][width - 1],
        ];
        let low = ends.iter().copied().reduce(Vec3::min).unwrap();
        let high = ends.iter().copied().reduce(Vec3::max).unwrap();
        let radius = (high - low).length() * 0.5;
        let ratio = if radius > 0.0 {
            eye.distance((low + high) * 0.5) / (radius * 10.0)
        } else {
            1.0
        };
        let subs = (((def.subdivisions.max(1) - 1) as f32 * (1.0 - ratio.clamp(0.0, 1.0)) + 1.5)
            .floor() as usize)
            .max(1);
        let samples = subs * n;
        let mut vertices = Vec::with_capacity(samples * width * 2);
        for row in 0..samples {
            let v = row as f32 / (samples - 1) as f32;
            let distance = v * (n - 1) as f32;
            let index = (distance - 1.0).floor().clamp(0.0, (n - 4) as f32) as usize;
            let t = (distance - index as f32) / 3.0;
            for col in 0..width {
                let point = |column| {
                    if subs == 1 {
                        self.rows[row][column]
                    } else {
                        bezier(std::array::from_fn(|i| self.rows[index + i][column]), t)
                    }
                };
                let position = point(col);
                let across = point((col + 1).min(width - 1)) - point(col.saturating_sub(1));
                let along = self.rows[(index + 2).min(n - 1)][col] - self.rows[index][col];
                // [assumed] Tangents approximate the original's neighbor differences;
                // the unlit emissive trails do not read normals. See status item3.
                let normal = along.cross(across).normalize_or_zero();
                let uv = [col as f32 / (width - 1) as f32, v];
                vertices.push(Vertex {
                    position,
                    normal,
                    uv,
                });
                vertices.push(Vertex {
                    position,
                    normal: -normal,
                    uv,
                });
            }
        }
        let mut indices = Vec::with_capacity((samples - 1) * (width - 1) * 12);
        for row in 0..samples - 1 {
            for col in 0..width - 1 {
                let a = ((row * width + col) * 2) as u32;
                let b = a + 2;
                let c = a + (width * 2) as u32;
                let d = c + 2;
                indices.extend_from_slice(&[
                    a,
                    b,
                    c,
                    c,
                    b,
                    d,
                    b + 1,
                    a + 1,
                    d + 1,
                    d + 1,
                    a + 1,
                    c + 1,
                ]);
            }
        }
        (vertices, indices)
    }
}

fn bezier(p: [Vec3; 4], t: f32) -> Vec3 {
    let r = 1.0 - t;
    p[0] * r * r * r + p[1] * 3.0 * t * r * r + p[2] * 3.0 * t * t * r + p[3] * t * t * t
}

/// Clip-exit messages, including interrupted attacks before the timed stop key.
pub fn exit_messages(clip: &Clip) -> impl Iterator<Item = &TriggerEvent> {
    clip.trigger_events.iter().filter(|e| e.on_exit)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn def() -> Def {
        Def {
            name: 1,
            links: vec![1, 2],
            material: 0,
            trails: 8,
            subdivisions: 4,
            trigger_speed: 0.5,
            enabled: false,
        }
    }
    fn points(x: f32) -> Vec<Vec3> {
        vec![Vec3::new(x, 0.0, 0.0), Vec3::new(x, 1.0, 0.0)]
    }
    #[test]
    fn timed_trigger_grows_and_stop_drains_even_while_the_owner_moves() {
        let d = def();
        let mut t = Trail::new(&d);
        for i in 0..8 {
            t.update(&d, points(i as f32), INTERVAL);
        }
        assert_eq!(t.rows.len(), 3);
        assert!(t.mesh(&d, Vec3::ZERO).0.is_empty());
        t.message(TRIGGER);
        for i in 8..16 {
            t.update(&d, points(i as f32), INTERVAL);
        }
        assert_eq!(t.rows.len(), 8);
        assert!(!t.mesh(&d, Vec3::ZERO).0.is_empty());
        t.message(STOP);
        for i in 16..21 {
            t.update(&d, points(i as f32), INTERVAL);
        }
        assert_eq!(t.rows.len(), 3);
        assert!(t.mesh(&d, Vec3::ZERO).0.is_empty());
    }
    #[test]
    fn threshold_uses_displacement_and_a_long_update_takes_one_sample() {
        let d = def();
        let mut t = Trail::new(&d);
        t.message(TRIGGER);
        for _ in 0..5 {
            t.update(&d, points(0.0), INTERVAL);
        }
        assert_eq!(t.rows.len(), 3);
        t.update(&d, points(0.4), 0.1);
        assert_eq!(t.rows.len(), 3);
        t.update(&d, points(1.0), 0.1);
        assert_eq!(t.rows.len(), 4);
    }
    #[test]
    fn mesh_keeps_endpoints_uvs_opposite_normals_and_bounded_indices() {
        let d = def();
        let mut t = Trail::new(&d);
        t.message(TRIGGER);
        for i in 0..8 {
            t.update(&d, points(i as f32), INTERVAL);
        }
        let (v, indices) = t.mesh(&d, Vec3::new(3.5, 0.5, 0.0));
        assert_eq!(v[0].position, Vec3::new(7.0, 0.0, 0.0));
        assert_eq!(v[v.len() - 1].position, Vec3::Y);
        assert_eq!(v[0].uv, [0.0, 0.0]);
        assert_eq!(v[v.len() - 1].uv, [1.0, 1.0]);
        assert!(v.chunks_exact(2).all(|p| p[0].normal == -p[1].normal));
        assert!(indices.iter().all(|&i| (i as usize) < v.len()));
    }
    #[test]
    fn installed_melee_trails_resolve_nodes_and_stop_on_timed_or_interrupted_exit() {
        let data = crate::character::load(std::path::Path::new("C:/Games2"), "bumblebee").unwrap();
        assert!(!data.robot.ribbons.is_empty());
        let axe = data.melee_weapon.as_ref().unwrap().object.as_ref().unwrap();
        assert_eq!(axe.ribbons.len(), 1);
        assert_eq!(axe.ribbons[0].links.len(), 4);
        assert_eq!(axe.ribbons[0].trails, 8);
        for object in [&data.robot, axe] {
            for trail in &object.ribbons {
                assert!(
                    trail
                        .links
                        .iter()
                        .all(|name| object.nodes.iter().any(|n| n.name_hash == *name))
                );
                assert!(data.library.materials.contains_key(&trail.material));
            }
        }
        let clip = data
            .animations
            .find("CombatSet", "AttackFast_2")
            .unwrap()
            .clip
            .as_ref()
            .unwrap();
        let name = crc32(b"Trail_Weapon_L");
        let on = clip
            .trigger_events
            .iter()
            .find(|e| e.name == name && e.message == TRIGGER)
            .unwrap();
        let off = exit_messages(clip)
            .find(|e| e.name == name && e.message == STOP)
            .unwrap();
        assert_eq!((on.time, off.time), (18.0, 32.0));
        assert!(on.due(17.0, 19.0));
        assert!(!off.due(19.0, 20.0));
        // An interruption at tick20 still sends the exit's stop.
        assert_eq!(exit_messages(clip).filter(|e| e.name == name).count(), 1);
    }
}
