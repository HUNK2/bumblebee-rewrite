//! Installed-tuning display/camera jitter reproduction, before versus interpolation.
use glam::{Vec2, Vec3};
use tf2_core::{
    camera::{CAR_BODY_RISE, Camera, Target},
    sim::{self, Arena, Input, State},
    tuning::{self, Tuning},
};

fn target(s: &State, t: &Tuning, interpolated: bool) -> Target {
    let pose = s.car_presentation();
    let (yaw, pitch, roll) = if interpolated {
        pose.angles()
    } else {
        (s.yaw, s.pitch, s.roll)
    };
    Target {
        position: if interpolated { pose.pos } else { s.pos },
        height: t.vehicle_height + CAR_BODY_RISE,
        facing: Vec2::new(-yaw.sin(), yaw.cos()),
        velocity: if interpolated {
            pose.camera_velocity
        } else {
            s.camera_velocity()
        },
        motion: if interpolated { pose.velocity } else { s.vel },
        vehicle: true,
        climbing: false,
        aim: None,
        radius: t.vehicle_radius,
        pitch,
        roll,
        fighting: false,
        ground_distance: pose
            .wheels
            .iter()
            .map(|w| w.probe_distance)
            .fold(100.0, f32::min),
        body_height: Some((pose.rotation * Vec3::Z).z * t.drive.body_offset_z),
        ..Default::default()
    }
}

fn main() {
    let dir = std::env::var("TF2_GAME_DIR").unwrap_or_else(|_| "C:/Games2".into());
    let t = tuning::load(std::path::Path::new(&dir), "Bumblebee").unwrap();
    let arena = Arena::default();
    let gas = Input {
        vehicle: 1.0,
        ..Default::default()
    };
    for (name, frames) in [
        ("60Hz", vec![1.0 / 60.0]),
        ("120Hz", vec![1.0 / 120.0]),
        ("144Hz", vec![1.0 / 144.0]),
        ("variable", vec![0.007, 0.021, 0.012, 0.045, 0.009]),
    ] {
        let mut s = State::new(&t);
        for _ in 0..200 {
            sim::step(&mut s, &gas, &t, &arena, 0.032);
        }
        let mut cameras = [
            Camera::behind(&target(&s, &t, false), &t),
            Camera::behind(&target(&s, &t, true), &t),
        ];
        let mut last_pos = [s.pos, s.car_presentation().pos];
        let mut last_relative = [Vec3::ZERO; 2];
        let mut min = [f32::INFINITY; 2];
        let mut max = [0.0_f32; 2];
        let mut relative_max = [0.0_f32; 2];
        let mut holds = [0; 2];
        for i in 0..1800 {
            let dt = frames[i % frames.len()];
            sim::step(&mut s, &gas, &t, &arena, dt);
            for (j, camera) in cameras.iter_mut().enumerate() {
                let target = target(&s, &t, j == 1);
                let view = camera.step(&target, Vec2::ZERO, &t, &arena, dt);
                let travel = target.position.distance(last_pos[j]) / dt;
                let relative = target.position - view.eye;
                if i > 300 {
                    min[j] = min[j].min(travel);
                    max[j] = max[j].max(travel);
                    relative_max[j] = relative_max[j].max(relative.distance(last_relative[j]));
                    holds[j] += usize::from(travel < 0.001);
                }
                last_pos[j] = target.position;
                last_relative[j] = relative;
            }
        }
        println!(
            "{name}: raw displayed speed={:.3}..{:.3}m/s, held frames={}, max camera-relative jump={:.4}m; interpolated speed={:.3}..{:.3}m/s, held frames={}, max camera-relative jump={:.4}m",
            min[0], max[0], holds[0], relative_max[0], min[1], max[1], holds[1], relative_max[1]
        );
        assert!(holds[0] > 0 && holds[1] == 0);
        // Installed tyre forces vary the physical cruise slightly; preserve that
        // response while rejecting the held/jumping display and camera sawtooth.
        assert!(max[1] - min[1] < 1.0, "interpolated cruise still jitters");
        assert!(
            relative_max[1] < relative_max[0] * 0.05,
            "camera and body lose synchronization"
        );
    }
}
