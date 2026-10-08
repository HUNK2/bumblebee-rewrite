//! The game's three cameras (`tf2_core::camera`: follow, driving and weapon), shown
//! through Bevy's. Their rules were read from the original and
//! checked against a recording; this file only feeds them and places Bevy's camera.
//!
//! In weapon mode the stick turns his aim (`tf2_core::aim`) and the weapon camera follows
//! that. The mouse is the original's: a second camera stick (see
//! `MOUSE_COUNTS_FOR_FULL_STICK`).
//!
//! The camera shakes (`tf2_core::shake`) are put on the view as it is drawn; the view the
//! stick and the weapons go by is the one without them.

use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};
use tf2_core::aim::Aim;
use tf2_core::camera::{Camera, Mode, Target, View};
use tf2_core::formats::anim::Clip;
use tf2_core::shake::{Jolt, Shakes};

use crate::assets::game_to_bevy;
use crate::player::{Controls, Player, TuningRes};
use crate::sim::{ShakeStart, State};
use crate::tuning::Tuning;

/// The mouse is a stick in the original: each direction of it is a control that reads
/// counts moved this frame over this divisor, from 0 to 1 (`FUN_004f0e30`; the divisor
/// `DAT_00bf361c` is 10 in the executable; options rewrite it as
/// trunc((1.05 - SETTING_AIM_MOUSE) * 20), checked at008e8bb0). So it turns the cameras and the aim at their stick rates, and
/// more than this many counts in a frame is the stick held over. [game]
pub const MOUSE_COUNTS_FOR_FULL_STICK: f32 = 10.0;

/// Scene importers attach this only to collision objects carrying the original fade
/// flag. Bounds are in game/world space; a moving object updates them with its pose.
/// Materials must be unique to the object, as opacity is per object in Lux. [game]
#[derive(Component)]
pub struct Fadeable {
    pub bounds: crate::sim::Box3,
    pub alpha: f32,
    pub alpha_mode: AlphaMode,
}

#[derive(Resource, Default)]
pub struct Occluders(pub Vec<tf2_core::camera::Fadeable>);

pub fn collect_occluders(query: Query<(Entity, &Fadeable)>, mut occluders: ResMut<Occluders>) {
    occluders.0.clear();
    occluders
        .0
        .extend(query.iter().map(|(entity, o)| tf2_core::camera::Fadeable {
            id: entity.to_bits(),
            bounds: o.bounds,
        }));
}

pub fn fade_occluders(
    rig: Res<Rig>,
    query: Query<(Entity, &Fadeable, &MeshMaterial3d<crate::ssd::Material>)>,
    mut materials: ResMut<Assets<crate::ssd::Material>>,
    mut report_frames: Local<u32>,
) {
    *report_frames += 1;
    for (entity, original, handle) in &query {
        if let Some(mut material) = materials.get_mut(&handle.0) {
            let alpha = rig.camera.fades.alpha(entity.to_bits());
            if *report_frames == 60 && std::env::args().any(|a| a == "--camera-fade-test") {
                eprintln!(
                    "camera fade fixture: alpha={alpha:.3}, eye={:?}, forward={:?}",
                    rig.view.eye, rig.view.forward
                );
            }
            material.base.base_color = material.base.base_color.with_alpha(original.alpha * alpha);
            material.base.alpha_mode = if alpha < 1.0 {
                AlphaMode::Blend
            } else {
                original.alpha_mode
            };
        }
    }
}

/// The length of an update of the shakes. The original runs them once a frame with the
/// frame's own length, and how they look depends on how often that is. [assumed: the
/// length of the game's update in the recording, as for the cameras]
const SHAKE_UPDATE: f32 = 0.032;

#[derive(Resource)]
pub struct Rig {
    pub camera: Camera,
    /// Where he aims: turned by the stick in weapon mode, the camera's direction otherwise.
    pub aim: Aim,
    /// Degrees the drawn picture is swung round him, for captures (`--orbit`).
    pub orbit: f32,
    /// How many times the drawn picture is magnified, for captures (`--zoom`); 1 is the game's.
    pub zoom: f32,
    /// The view to draw this frame from, in the game's space, before the shakes.
    pub view: View,
    pub levels: [usize; 3],
    /// The camera shakes running, the time not yet given to them, and what they do to the
    /// view until their next update.
    shakes: Shakes,
    shake_time: f32,
    jolt: Jolt,
}

/// What the camera follows, from the movement's state; `aim` is his aim in weapon mode,
/// which hands him to the weapon camera and raises the point looked at by the character's
/// `cameraWeaponTargetHeightOffset`. [game]
pub fn target(state: &State, tuning: &Tuning, aim: Option<Vec3>) -> Target {
    let vehicle = state.is_vehicle();
    let pose = state.car_presentation();
    let (yaw, pitch, _) = pose.angles();
    let up = pose.rotation * Vec3::Z;
    let right = pose.rotation * Vec3::X;
    let aim = aim.filter(|_| !vehicle);
    Target {
        position: pose.pos,
        height: if vehicle {
            // [game] 0070abe0: form height plus the bottom of the physics holder
            // above the character root. Replaces the fixed run1 rise of1.12.
            tuning.vehicle_height
                + (up.z * (tuning.drive.body_offset_z - tuning.vehicle_height * 0.5)).max(0.0)
        } else {
            tuning.robot_height
                + if aim.is_some() {
                    tuning.weapon_look_height
                } else {
                    0.0
                }
        },
        facing: if vehicle {
            Vec2::new(-yaw.sin(), yaw.cos())
        } else {
            state.facing()
        },
        velocity: pose.camera_velocity,
        motion: pose.velocity,
        vehicle,
        climbing: state.climb.is_some(),
        aim,
        radius: if vehicle {
            tuning.vehicle_radius
        } else {
            tuning.robot_radius
        },
        pitch: if vehicle { pitch } else { 0.0 },
        roll: if vehicle {
            (-right.z).clamp(-1.0, 1.0).asin()
        } else {
            0.0
        },
        fighting: state.fight > 0.0,
        mode: if state.dead() {
            Mode::Fast
        }
        // [game] character dead flag requests7d110862
        else if state.control_state(tuning) == Some("stateGroundPunchWeakFall") {
            Mode::GroundPunch
        } else {
            Mode::Normal
        },
        ground_distance: pose
            .wheels
            .iter()
            .map(|w| w.probe_distance)
            .fold(100.0, f32::min),
        body_height: vehicle.then_some(up.z * tuning.drive.body_offset_z),
        flying: false,
    }
}

impl Rig {
    /// Behind him at the camera's own distance and elevation; or, for captures, looking
    /// along `yaw` degrees (0 is +y, positive turns toward -x) from `elevation` degrees up.
    pub fn new(state: &State, tuning: &Tuning, yaw: Option<f32>, elevation: Option<f32>) -> Self {
        let target = target(state, tuning, None);
        let index = |levels: &[tf2_core::tuning::CameraTuning],
                     current: &tf2_core::tuning::CameraTuning| {
            levels
                .iter()
                .position(|s| s.min_dist == current.min_dist && s.max_dist == current.max_dist)
                .unwrap_or(0)
        };
        let levels = [
            index(&tuning.robot_camera_levels, &tuning.robot_camera),
            index(&tuning.weapon_camera_levels, &tuning.weapon_camera),
            index(&tuning.drive_camera_levels, &tuning.drive_camera),
        ];
        let mut camera = Camera::default();
        camera.reset(&target, tuning);
        if yaw.is_some() || elevation.is_some() {
            let settings = &tuning.robot_camera;
            let yaw = yaw.unwrap_or(0.0).to_radians();
            let elevation = elevation.unwrap_or(settings.default_elevation).to_radians();
            let look_at = target.position + Vec3::Z * (target.height + settings.height_offset);
            let back = Vec2::new(yaw.sin(), -yaw.cos());
            let eye =
                look_at + (back * elevation.cos()).extend(elevation.sin()) * settings.max_dist;
            camera = Camera::from_view(
                View {
                    eye,
                    forward: (look_at - eye).normalize(),
                    fov: settings.fov,
                    roll: 0.0,
                },
                &target,
                tuning,
            );
            // The smallest turn the camera takes as the player's own, so it keeps this
            // elevation instead of settling back to its default.
            camera.nudge(Vec2::new(0.0, 0.0015));
        }
        let view = camera.latest();
        let mut aim = Aim::default();
        aim.set(state.body_forward());
        Self {
            camera,
            aim,
            orbit: 0.0,
            zoom: 1.0,
            view,
            levels,
            shakes: Shakes::default(),
            shake_time: 0.0,
            jolt: Jolt::default(),
        }
    }

    /// Starts the shakes the movement asked for this frame, each by its name in the game's
    /// list (a name the game does not have starts nothing, as in the original), and gives
    /// the running ones the frame's time.
    pub fn shake(&mut self, starts: &[ShakeStart], tuning: &Tuning, dt: f32) {
        for start in starts {
            if let Some(def) = tuning.shakes.get(&start.name) {
                self.shakes.start(*def, Some(start.at));
            }
        }
        self.run_shakes(dt);
    }

    /// The shakes a clip of his asks for in the stretch just played (`AnimCameraShakeEvent`,
    /// `FUN_007d0ff0`): at his place, or wherever the camera is. [assumed: the place is
    /// where he stands; the original takes it from the object the event is handed]
    pub fn clip_shakes(&mut self, clip: &Clip, from: f32, to: f32, at: Vec3, tuning: &Tuning) {
        for event in clip.shakes_between(from, to) {
            if let Some(def) = tuning.shakes.get(&event.name) {
                self.shakes.start(*def, event.positional.then_some(at));
            }
        }
    }

    fn run_shakes(&mut self, dt: f32) {
        if !self.shakes.any() {
            (self.shake_time, self.jolt) = (0.0, Jolt::default());
            return;
        }
        self.shake_time += dt;
        while self.shake_time >= SHAKE_UPDATE {
            self.shake_time -= SHAKE_UPDATE;
            self.jolt = self
                .shakes
                .step_with_clock(self.view.eye, SHAKE_UPDATE, || {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() as u32
                });
        }
    }
}

pub fn grab_cursor(
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut cursor: Single<&mut CursorOptions, With<PrimaryWindow>>,
) {
    if buttons.just_pressed(MouseButton::Left) {
        cursor.grab_mode = CursorGrabMode::Locked;
        cursor.visible = false;
    }
    if keys.just_pressed(KeyCode::Escape) {
        cursor.grab_mode = CursorGrabMode::None;
        cursor.visible = true;
    }
}

/// D-pad Up sends a signed-1 level step; camera options are shared across forms in
/// the user's capture (57.867s L1->L0;96.147s L0->L2). [trace/game]
/// C is the keyboard equivalent. Camera Options also support separate per-form levels.
pub fn cycle_distance(
    keys: Res<ButtonInput<KeyCode>>,
    pads: Query<&Gamepad>,
    mut controls: ResMut<Controls>,
    time: Res<Time>,
    mut tuning: ResMut<TuningRes>,
    mut rig: ResMut<Rig>,
    player: Single<&Player>,
) {
    let scripted = controls
        .camera_cycle_at
        .is_some_and(|at| time.elapsed_secs() >= at);
    if scripted {
        controls.camera_cycle_at = None;
    }
    if !scripted
        && !keys.just_pressed(KeyCode::KeyC)
        && !pads
            .iter()
            .any(|pad| pad.just_pressed(GamepadButton::DPadUp))
    {
        return;
    }
    let t = &mut tuning.0;
    let vehicle = player.state.is_vehicle();
    let weapon = !vehicle && controls.aim;
    let form = if vehicle {
        2
    } else if weapon {
        1
    } else {
        0
    };
    let options = rig.camera.options;
    options.cycle(&mut rig.levels, form, -1);
    if let Some(s) = t.robot_camera_levels.get(rig.levels[0]) {
        t.robot_camera = s.clone();
    }
    if let Some(s) = t.weapon_camera_levels.get(rig.levels[1]) {
        t.weapon_camera = s.clone();
    }
    if let Some(s) = t.drive_camera_levels.get(rig.levels[2]) {
        t.drive_camera = s.clone();
    }
    let aim = weapon.then(|| rig.aim.direction());
    let target = target(&player.state, t, aim);
    rig.camera.settings_changed(&target, t);
}

/// Puts Bevy's camera where the game's is. The game's field of view is the picture's height.
pub fn apply(rig: Res<Rig>, mut camera: Single<(&mut Transform, &mut Projection), With<Camera3d>>) {
    let (transform, projection) = &mut *camera;
    let mut shaken = rig.jolt.apply(&rig.view);
    // For captures only: the picture is taken from round to one side of where the game's
    // camera is, about the point 11 ahead of it; the game's own view is left alone.
    if rig.orbit != 0.0 {
        let turn = Quat::from_rotation_z(rig.orbit.to_radians());
        let pivot = shaken.eye + shaken.forward * 11.0;
        shaken.eye = pivot + turn * (shaken.eye - pivot);
        shaken.forward = turn * shaken.forward;
        shaken.up = turn * shaken.up;
    }
    **transform = Transform::from_translation(game_to_bevy(shaken.eye))
        .looking_to(game_to_bevy(shaken.forward), game_to_bevy(shaken.up));
    if let Projection::Perspective(perspective) = &mut **projection {
        let fov = rig.view.fov.clamp(10.0, 120.0).to_radians();
        perspective.fov = if rig.zoom == 1.0 {
            fov
        } else {
            2.0 * ((fov * 0.5).tan() / rig.zoom).atan()
        };
    }
}
