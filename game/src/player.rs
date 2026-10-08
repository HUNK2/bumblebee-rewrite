//! Input, the movement, the camera and the weapons each frame, and showing the result on
//! the loaded models.

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

use crate::assets::{ObjectNodes, bevy_to_game, game_to_bevy};
use crate::camera::{self, Rig};
use tf2_core::input::{Directions, LookSampler, MouseLatch, MOUSE_DIVISOR};
use crate::control::button;
use crate::sim::{self, Input, State};
use crate::sound::SoundOut;
use crate::tuning::Tuning;
use crate::weapons::Arsenal;
use crate::world::ArenaRes;

/// Damage dealt by the test key, to watch health come back.
const TEST_DAMAGE: f32 = 80.0;
/// The longest step the movement is given, in seconds; a hitch is not turned into a leap.
const LONGEST_STEP: f32 = 0.05;

#[derive(Resource)]
pub struct TuningRes(pub Tuning);

/// What the pads and keys ask for, gathered every frame.
#[derive(Resource, Default)]
pub struct Controls {
    /// Left stick: x right, y forward.
    pub stick: Vec2,
    /// Right stick, for the camera: x right, y up.
    pub look: Vec2,
    pub look_samples: Vec<Vec2>,
    pub look_sampler: LookSampler,
    pub mouse_latch: MouseLatch,
    pub mouse_captured: bool,
    pub mouse_divisor: Option<i32>,
    /// One scripted D-pad Up press for deterministic camera captures.
    pub camera_cycle_at: Option<f32>,
    pub recentre_pressed: bool,
    pub recentre_at: Option<f32>,
    /// The right trigger: car form and throttle; in weapon mode, fire.
    pub vehicle: f32,
    pub brake: f32,
    /// The left trigger: weapon mode on foot, the slide in the car.
    pub aim: bool,
    /// Fire from the mouse, which only counts in weapon mode.
    pub fire: bool,
    pub switch: bool,
    pub jump_held: bool,
    pub jump_pressed: bool,
    /// The left face button: the fast attack, and in weapon mode the dodge (the game binds
    /// `AttackFast` and `Dodge` to the same button).
    pub melee_held: bool,
    pub melee_pressed: bool,
    /// The right face button: climb (and action).
    pub climb_held: bool,
    pub climb_pressed: bool,
    pub damage_pressed: bool,
    pub reset_pressed: bool,
    /// Inputs held or fired without a player, for captures: the vehicle trigger between two
    /// times after start, the stick, a jump at a given time, and weapon mode with the
    /// trigger down from a given time.
    pub vehicle_window: Option<(f32, f32)>,
    pub force_stick: Vec2,
    pub jump_at: Option<f32>,
    /// A capture takes all his health at this time (`--die-at`).
    pub die_at: Option<f32>,
    /// Synthetic received hits for checking real reaction clips, separate from self-damage.
    pub hit_at: Option<f32>,
    pub hit_flags: u32,
    pub die_all: bool,
    /// Capture harness: retain the corpse instead of restarting the arena.
    pub keep_dead: bool,
    pub fire_from: Option<f32>,
    /// When a capture lets the trigger go again (`--fire-until`): a missile leaves then.
    pub fire_until: Option<f32>,
    /// Weapon mode without firing from a given time, and taps of the melee button.
    pub aim_from: Option<f32>,
    pub melee_at: Vec<f32>,
    /// The melee button held from a given time.
    pub melee_hold_from: Option<f32>,
    /// And let go at this time, if given.
    pub melee_hold_until: Option<f32>,
    /// Climb held from a given time.
    pub climb_from: Option<f32>,
    /// The camera stick held up (or down) by this much while the scripted aim is on.
    pub force_look_y: f32,
    /// The next weapon button tapped at a given time.
    pub switch_at: Option<f32>,
    /// The top face button: the special ability. And a capture's press of it at a time.
    pub special_held: bool,
    pub special_pressed: bool,
    pub special_at: Option<f32>,
}

#[derive(Component)]
pub struct Player {
    pub state: State,
    /// Weapon mode is on this frame.
    pub aiming: bool,
    pub reset_serial: u32,
}

impl Player {
    pub fn new(state: State) -> Self {
        Self { state, aiming: false, reset_serial: 0 }
    }
}

#[derive(Component)]
pub struct RobotModel;

#[derive(Component)]
pub struct VehicleModel;

pub fn read_input(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    mouse: Res<AccumulatedMouseMotion>,
    cursor: Single<(&Window, &CursorOptions), With<PrimaryWindow>>,
    pads: Query<&Gamepad>,
    mut controls: ResMut<Controls>,
) {
    let controls = &mut *controls;
    let now = time.elapsed_secs();
    let focused = (*cursor).0.focused;
    let pressed = |key| focused && keys.pressed(key);
    let just_pressed = |key| focused && keys.just_pressed(key);
    let mut stick = Directions { left: pressed(KeyCode::KeyA) as u8 as f32,
        right: pressed(KeyCode::KeyD) as u8 as f32, down: pressed(KeyCode::KeyS) as u8 as f32,
        up: pressed(KeyCode::KeyW) as u8 as f32 };
    if controls.die_at.is_some_and(|at| now >= at) {
        controls.die_at = None;
        controls.damage_pressed = true;
        controls.die_all = true;
    }
    if controls.jump_at.is_some_and(|at| now >= at) {
        controls.jump_at = None;
        controls.jump_pressed = true;
    }
    let grabbed = focused && (*cursor).1.grab_mode == CursorGrabMode::Locked;
    let was_captured = controls.mouse_captured;
    if grabbed != was_captured || !focused { controls.look_sampler.clear_mouse(); }
    controls.mouse_captured = grabbed;
    let mouse_held = controls.mouse_latch.frame(grabbed, [buttons.pressed(MouseButton::Left),
        buttons.pressed(MouseButton::Right), buttons.pressed(MouseButton::Middle)]);
    let firing = controls.fire_from.is_some_and(|from| now >= from) && controls.fire_until.is_none_or(|until| now < until);
    let scripted_aim = controls.aim_from.is_some_and(|from| now >= from);
    let scripted_melee = controls.melee_at.first().is_some_and(|&at| now >= at);
    if scripted_melee {
        controls.melee_at.remove(0);
    }
    let capture_look = if scripted_aim || firing { Vec2::new(0.0, controls.force_look_y) } else { Vec2::ZERO };
    let mut look = Directions { left: pressed(KeyCode::ArrowLeft) as u8 as f32,
        right: pressed(KeyCode::ArrowRight) as u8 as f32, down: pressed(KeyCode::ArrowDown) as u8 as f32,
        up: pressed(KeyCode::ArrowUp) as u8 as f32 };
    // The pad follows the game's own bindings: R2 vehicle mode, throttle and fire, L2 weapon
    // mode and the slide, L1 brake, R1 next weapon, the bottom face button jump and turbo.
    let scripted = controls.vehicle_window.is_some_and(|(from, until)| (from..until).contains(&now));
    // [data] Installed Beenox defaults: Shift/LMB share R2; Ctrl is action,
    // R is L1, T is R1, F is the top face button, MMB is the left face button.
    let mut vehicle = if pressed(KeyCode::ShiftLeft) || mouse_held[0] || scripted { 1.0 } else { 0.0 };
    let mut brake = if pressed(KeyCode::KeyR) { 1.0 } else { 0.0 };
    let mut aim =
        pressed(KeyCode::AltLeft) || mouse_held[1] || firing || scripted_aim;
    let mut switch = pressed(KeyCode::KeyT) || controls.switch_at.is_some_and(|at| (at..at + 0.1).contains(&now));
    let mut recentre=just_pressed(KeyCode::KeyE)||just_pressed(KeyCode::Home)||controls.recentre_at.is_some_and(|at|now>=at);
    if recentre {controls.recentre_at=None;}
    let mut jump_held = pressed(KeyCode::Space);
    let mut melee_held = mouse_held[2] || controls.melee_hold_from.is_some_and(|from| now >= from && controls.melee_hold_until.is_none_or(|until| now < until));
    let scripted_climb = controls.climb_from.is_some_and(|from| now >= from);
    let mut climb_held = pressed(KeyCode::ControlLeft) || scripted_climb;
    let scripted_special = controls.special_at.is_some_and(|at| now >= at);
    if scripted_special {
        controls.special_at = None;
    }
    let mut special_held = pressed(KeyCode::KeyF) || pressed(KeyCode::KeyX) || scripted_special;
    for pad in pads.iter().filter(|_| focused) {
        // The top face button is the special ability in the game's bindings
        // (`AttackSpecial` on `DPAD_R_UP`). [data]
        special_held |= pad.pressed(GamepadButton::North);
        melee_held |= pad.pressed(GamepadButton::West);
        // The right face button is action and climb in the game's bindings.
        climb_held |= pad.pressed(GamepadButton::East);
        stick.merge(Directions::signed(pad.left_stick()));
        look.merge(Directions::signed(pad.right_stick()));
        vehicle = f32::max(vehicle, pad.get(GamepadButton::RightTrigger2).unwrap_or(0.0));
        brake = f32::max(brake, pad.get(GamepadButton::LeftTrigger).unwrap_or(0.0));
        aim |= pad.get(GamepadButton::LeftTrigger2).unwrap_or(0.0) > 0.3;
        switch |= pad.pressed(GamepadButton::RightTrigger);
        jump_held |= pad.pressed(GamepadButton::South);
        // [data] CameraReset=R3 in the installed Controller table.
        recentre |= pad.just_pressed(GamepadButton::RightThumb);
    }
    let jump = jump_held && !controls.jump_held;
    let melee = scripted_melee || (melee_held && !controls.melee_held);
    let climb = climb_held && !controls.climb_held;
    let special = scripted_special || (special_held && !controls.special_held);
    controls.stick = (stick.filtered() + controls.force_stick).clamp_length_max(1.0);
    let delta = if grabbed && was_captured { mouse.delta } else { Vec2::ZERO };
    let dt = time.delta_secs().clamp(0.0, LONGEST_STEP);
    let sampled = controls.look_sampler.frame(look, delta, controls.mouse_divisor.unwrap_or(MOUSE_DIVISOR), dt, &mut controls.look_samples);
    controls.look = (sampled + capture_look).clamp(Vec2::NEG_ONE, Vec2::ONE);
    // Preserve the existing deterministic capture-only stick injection.
    for sample in &mut controls.look_samples { *sample = (*sample + capture_look).clamp(Vec2::NEG_ONE, Vec2::ONE); }
    controls.vehicle = vehicle;
    controls.brake = brake;
    controls.aim = aim;
    controls.fire = mouse_held[0] || firing;
    controls.switch = switch;
    controls.jump_held = jump_held;
    controls.jump_pressed |= jump;
    controls.recentre_pressed |= recentre;
    controls.melee_held = melee_held;
    controls.melee_pressed |= melee;
    controls.climb_held = climb_held;
    controls.climb_pressed |= climb;
    controls.special_held = special_held;
    controls.special_pressed |= special;
    controls.damage_pressed |= just_pressed(KeyCode::KeyH);
    // [stand-in] Debug reset moves to F5 because native R is brake.
    controls.reset_pressed |= just_pressed(KeyCode::F5);
}

/// One frame of Bumblebee: the movement, then the camera that follows it, his weapons and
/// his sounds. The movement and the camera keep the game's own update length inside.
#[allow(clippy::too_many_arguments)]
pub fn tick(
    time: Res<Time>,
    tuning: Res<TuningRes>,
    arena: Res<ArenaRes>,
    occluders: Res<camera::Occluders>,
    sound: Option<Res<SoundOut>>,
    library: Res<crate::animation::AnimLibrary>,
    data: Res<crate::GameData>,
    mut controls: ResMut<Controls>,
    mut rig: ResMut<Rig>,
    mut arsenal: Option<ResMut<Arsenal>>,
    mut player: Single<&mut Player>,
    robot: Single<&ObjectNodes, With<RobotModel>>,
    car_nodes: Option<Single<&ObjectNodes, (With<VehicleModel>, Without<RobotModel>)>>,
    placed: Query<&GlobalTransform>,
    enemies: Query<&crate::targets::EnemyTarget>,
) {
    let controls = &mut *controls;
    let player = &mut **player;
    let t = &tuning.0;
    let sound = sound.as_deref();
    if let Some(arsenal)=arsenal.as_mut() {arsenal.receive_heat(std::mem::take(&mut player.state.heat_damage));}
    let dt = time.delta_secs().clamp(0.0, LONGEST_STEP);
    if std::mem::take(&mut controls.reset_pressed) || (player.state.restart_requested && !controls.keep_dead) {
        let difficulty=player.state.difficulty;
        let turbo_upgrades=player.state.turbo_upgrades;
        player.state = State::new(t);
        player.state.difficulty=difficulty;
        player.state.turbo_upgrades=turbo_upgrades;
        player.reset_serial = player.reset_serial.wrapping_add(1);
        let options=rig.camera.options;
        let (orbit,zoom)=(rig.orbit,rig.zoom);
        *rig = Rig::new(&player.state, t, None, None);
        controls.look_sampler = LookSampler::default();
        controls.look = controls.look_sampler.frame(Directions::default(), Vec2::ZERO,
            controls.mouse_divisor.unwrap_or(MOUSE_DIVISOR), dt, &mut controls.look_samples);
        rig.camera.options=options;
        rig.orbit=orbit;rig.zoom=zoom;
        if let Some(sound) = sound {
            sound.silence();
        }
        if let Some(arsenal) = arsenal.as_mut() {
            arsenal.reset(sound);
        }
    }
    if std::mem::take(&mut controls.damage_pressed) {
        if std::mem::take(&mut controls.die_all) {
            player.state.damage(f32::MAX);
        } else {
            // [stand-in] Debug key supplies an already resolved synthetic hit.
            player.state.receive_hit(TEST_DAMAGE, 0, Vec3::ZERO);
        }
    }
    if controls.hit_at.is_some_and(|at| time.elapsed_secs() >= at) {
        controls.hit_at = None;
        // [stand-in] Capture-only incoming impulse; reaction choice/timing are the game's.
        player.state.receive_hit(TEST_DAMAGE, controls.hit_flags, Vec3::new(0.0, -18.0, 12.0));
    }

    // On foot the stick is relative to the camera; in the car it steers.
    let forward = Vec2::new(rig.view.forward.x, rig.view.forward.y).normalize_or(Vec2::Y);
    let right = Vec2::new(forward.y, -forward.x);
    let pressed = std::mem::take(&mut controls.jump_pressed);
    let melee = std::mem::take(&mut controls.melee_pressed);
    let climb = std::mem::take(&mut controls.climb_pressed);
    // The left face button is the fast attack, the dodge and, in the car, the car's gun
    // (`AttackFast`, `Dodge` and `AttackVehicle` are all bound to `DPAD_R_LEFT`). [data]
    let both = |down: bool| {
        (down as u64) << button::ATTACK_FAST | (down as u64) << button::DODGE | (down as u64) << button::ATTACK_VEHICLE
    };
    let climbing = |down: bool| (down as u64) << button::CLIMB;
    let special = std::mem::take(&mut controls.special_pressed);
    let special_bits = |down: bool| (down as u64) << button::ATTACK_SPECIAL;
    let vehicle = player.state.is_vehicle();
    // Weapon mode is the original's: held on the left trigger while on foot, and then the
    // right trigger fires instead of changing him into the car. In the car the same trigger
    // is the slide.
    let aiming = controls.aim && !vehicle && arsenal.is_some();
    let was_aiming = std::mem::replace(&mut player.aiming, aiming);
    // The fire button is on the same trigger as the change of form (the bindings put both
    // on R2); the state machine's firing state reads it. [data]
    let fire = (aiming && (controls.vehicle > 0.1 || controls.fire)) as u64;
    // In weapon mode the camera stick turns his AIM, and his body and the weapon camera
    // follow that (`FUN_0071b990`). Otherwise the aim follows the character object's
    // forward row (`0071af5c..0071af62`). Weapon entry resets it to the camera. [game]
    //
    // As weapon mode starts the aim is put on the camera's forward and, with the aim assist
    // on (the options' default), a target within 30 degrees of the camera's line is looked
    // for and turned to (`r2wLockOn`); after that the stick turns the aim, slowed over a
    // target in the assist's box and pulled toward it while he moves. [game] [stand-in:
    // target collision is cylindrical; the assist's target is the one the box held last
    // frame; taking aim is taken to be the weapon state's enter]
    let looking = rig.view.forward;
    if aiming {
        // [game] The same yaw/pitch inversion switches also turn weapon aim (0071b990).
        let look=rig.camera.options.stick(controls.look);
        let right = looking.cross(Vec3::Z).normalize_or(Vec3::X);
        let eye = tf2_core::aim::Camera { eye: rig.view.eye, right, forward: looking, up: right.cross(looking) };
        let targets = &arena.0.targets;
        let clear = |from: Vec3, to: Vec3| tf2_core::camera::Sight::sight(&arena.0, from, to).is_none();
        if !was_aiming {
            rig.aim.enter(looking, look);
        }
        if rig.aim.snap_wanted && rig.aim.snap.is_none() {
            let range = arsenal.as_ref().map_or(t.aim.default_max_range, |a| a.assist_range(&t.aim));
            rig.aim.snap = tf2_core::aim::snap_target(&eye, range, targets, &t.aim, &clear);
        }
        match rig.aim.snap.and_then(|id| targets.iter().find(|target| target.id == id)) {
            Some(target) => rig.aim.step_snap(tf2_core::aim::assist_point(target), eye.eye, player.state.pos, dt),
            None => {
                rig.aim.snap = None;
                let held = arsenal.as_ref().and_then(|a| a.assist_target(targets, &rig.view,
                    |id| enemies.iter().find(|e|e.id==id).map_or(Vec3::ZERO,|e|e.combat.state.vel)));
                let assist = tf2_core::aim::assist(look, &eye, player.state.vel, held.as_ref(), &t.aim, dt);
                let moving = controls.stick.length_squared() > 0.001;
                rig.aim.step_assisted(look, moving, player.state.pos, assist, &t.aim, dt);
            }
        }
    } else {
        rig.aim.leave();
        rig.aim.set(player.state.body_forward());
    }
    let aim = aiming.then(|| rig.aim.direction());
    let input = Input {
        stick: right * controls.stick.x + forward * controls.stick.y,
        steer: controls.stick.x,
        // On a wall the stick is the pad's own: up climbs up.
        pad_stick: controls.stick,
        jump: pressed,
        jump_held: controls.jump_held,
        vehicle: if aiming { 0.0 } else { controls.vehicle },
        // In the car the stick pulled back brakes too (and, held at a stop, reverses).
        brake: f32::max(controls.brake, if vehicle { -controls.stick.y } else { 0.0 }),
        // The original puts turbo on the jump button while driving.
        turbo: pressed && vehicle,
        drift: controls.aim,
        aim: aim.map(|aim| Vec2::new(aim.x, aim.y)),
        buttons_down: both(controls.melee_held || melee)
            | climbing(controls.climb_held || climb)
            | special_bits(controls.special_held || special)
            | fire << button::WEAPON_FIRE,
        buttons_hit: both(melee) | climbing(climb) | special_bits(special),
    };
    // Whether his base layer plays the change from the car as this update starts: a
    // weapon state entered straight from it shows the gun at once (`FUN_0087f4a0`).
    let was_unfolding = player.state.action_clip(t).is_some_and(|(set, _, _)| set == "Vehicle2RobotSet");
    sim::step(&mut player.state, &input, t, &arena.0, dt);

    let target = camera::target(&player.state, t, aim);
    if controls.recentre_pressed {rig.camera.recentre(&target);controls.recentre_pressed=false;}
    let world=tf2_core::camera::Scene {solids:&arena.0,fadeable:&occluders.0};
    let view = rig.camera.step_sampled(&target, controls.look, t, &world, dt, &controls.look_samples);
    rig.view = view;
    rig.shake(&player.state.shakes, t, dt);

    if let Some(arsenal) = arsenal.as_mut() {
        // Shots leave from the weapon's `FireDummy` node [data], the weapon taken to sit on
        // the node it hangs from as that was drawn last frame [assumed: the weapon object
        // is not placed yet; how it is fixed to the node is not read]. If the model has no
        // such node, from a point in front of his chest. [stand-in]
        let state = &player.state;
        let chest = state.pos + Vec3::Z * (t.robot_height * 0.7) + state.facing().extend(0.0) * 1.5;
        let (bone, barrel) = (arsenal.bone(), arsenal.muzzle());
        let muzzle = robot
            .name_hashes
            .iter()
            .position(|&hash| hash == bone)
            .and_then(|node| placed.get(robot.entities[node]).ok())
            .map_or(chest, |node| bevy_to_game(node.transform_point(barrel)));
        // The axe follows the weapon events of the clip his control state plays.
        if let Some(weapon) = &data.0.melee_weapon {
            let playing = state.action_clip(t).and_then(|(set, id, tick)| {
                let clip = library.robot.find(set, id)?.clip.as_ref()?;
                Some((clip, tf2_core::formats::hash::crc32(id.as_bytes()), tick))
            });
            arsenal.tick_axe(weapon, playing, state.state_class(t) == Some("StateAttack"), dt);
        }
        // The gun is out in the weapon states, and through a dodge out of one. [game]
        let armed = match state.state_class(t) {
            Some("StateWeapon") => true,
            Some("StateDash") => arsenal.gun.out,
            _ => false,
        };
        // The car's weapon hangs on a node of the car; its barrels are that object's
        // `FireDummy` nodes, placed by the node as it was drawn last frame, with the
        // car's own axes for the way they point (`FUN_007a7a40` without
        // `useFireDummyMatrix`). [game] [stand-in for a car with no such node: ahead of
        // its nose]
        let nose = state.body_forward();
        let car_barrels: Vec<Vec3> = arsenal.car_weapon().map_or(Vec::new(), |(bone, barrels)| {
            let node = car_nodes
                .as_ref()
                .and_then(|car| Some(car.entities[car.name_hashes.iter().position(|&hash| hash == bone)?]))
                .and_then(|node| placed.get(node).ok());
            barrels
                .iter()
                .map(|&barrel| node.map_or(state.pos + nose * 3.0 + Vec3::Z, |node| bevy_to_game(node.transform_point(barrel))))
                .collect()
        });
        let car = crate::weapons::Car {
            mode: state.car_mode,
            formed: state.is_vehicle(),
            wait: t.vehicle_weapon_wait,
            barrels: &car_barrels,
            forward: nose,
        };
        let fired = arsenal.tick(
            armed,
            was_unfolding,
            crate::weapons::gun_clips(&library, &data.0),
            state.firing,
            controls.switch,
            muzzle,
            state.pos,
            view.eye,
            view.forward,
            view.fov,
            &arena.0,
            &arena.0.targets,
            &car,
            &t.aim,
            &t.damage,
            state.damage_timers.slowed>0.0,
            sound,
            dt,
        );
        // A round shakes the pad with the weapon's own values, effect type 1. [game]
        if let Some((duration, strength)) = fired {
            player.state.rumbles.push(tf2_core::rumble::Rumble { kind: 1, duration, strength });
        }
    }
    if let Some(sound) = sound {
        // A trigger message also starts each `SoundPlayer` of the object he is (his own, or
        // the car's) that answers to its name: the special ability's sound, the dodge's
        // jets. [game: 0076d490/0076e500] idtext takes precedence; each player's
        // retrigger flag determines whether its active event is restarted.
        let object = if player.state.car_mode { data.0.vehicle.as_ref() } else { Some(&data.0.robot) };
        for name in &player.state.triggers {
            for (index, definition) in object.iter().flat_map(|o| &o.sound_players).enumerate().filter(|(_, p)| p.script == *name) {
                sound.trigger_event(player.state.car_mode, index, definition);
            }
        }
        // His sounds are heard from where the camera is.
        sound.tick(&player.state, &tuning.0, view.eye, view.forward, time.delta_secs());
    }
}

/// Places the character where the movement has him; the car leans with the ground.
pub fn present(mut player: Single<(&Player, &mut Transform)>) {
    let (player, transform) = &mut *player;
    let state = &player.state;
    let pose=state.car_presentation();
    let (yaw,pitch,roll)=if state.is_vehicle() {pose.angles()} else {(state.yaw,state.pitch,state.roll)};
    transform.translation = game_to_bevy(pose.pos);
    // Pitch lifts the nose and roll the left side, about the car's own axes.
    transform.rotation =
        Quat::from_rotation_y(yaw) * Quat::from_rotation_x(pitch) * Quat::from_rotation_z(-roll);
}

#[cfg(test)]
mod input_tests {
    use super::*;
    use std::time::Duration;

    fn app() -> App {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<ButtonInput<MouseButton>>()
            .init_resource::<AccumulatedMouseMotion>()
            .init_resource::<Controls>()
            .add_systems(Update, (camera::grab_cursor, read_input).chain());
        app.world_mut().spawn((Window { focused: true, ..Default::default() },
            CursorOptions::default(), PrimaryWindow));
        app
    }

    fn frame(app: &mut App, dt: f32) {
        app.world_mut().resource_mut::<Time>().advance_by(Duration::from_secs_f32(dt));
        app.update();
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().clear();
        app.world_mut().resource_mut::<ButtonInput<MouseButton>>().clear();
    }

    #[test]
    fn native_keyboard_bindings_reach_gameplay_controls() {
        let mut app = app();
        for key in [KeyCode::KeyW, KeyCode::ArrowRight, KeyCode::Space, KeyCode::ControlLeft,
            KeyCode::KeyR, KeyCode::KeyT, KeyCode::KeyF, KeyCode::KeyE] {
            app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(key);
        }
        frame(&mut app, 0.032);
        let controls = app.world().resource::<Controls>();
        assert!((controls.stick - Vec2::Y).length() < 0.0001);
        assert_eq!(controls.look, Vec2::X);
        assert!(controls.jump_pressed && controls.climb_pressed && controls.special_pressed);
        assert!(controls.recentre_pressed && controls.switch);
        assert_eq!(controls.brake, 1.0);
        assert!(!controls.melee_held && !controls.reset_pressed);
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::F5);
        frame(&mut app, 0.032);
        assert!(app.world().resource::<Controls>().reset_pressed);
    }

    #[test]
    fn capture_click_is_suppressed_and_native_mouse_buttons_work_after_release() {
        let mut app = app();
        app.world_mut().resource_mut::<ButtonInput<MouseButton>>().press(MouseButton::Left);
        frame(&mut app, 0.032);
        let controls = app.world().resource::<Controls>();
        assert_eq!(controls.vehicle, 0.0);
        assert!(!controls.fire);
        frame(&mut app, 0.032);
        assert_eq!(app.world().resource::<Controls>().vehicle, 0.0);
        app.world_mut().resource_mut::<ButtonInput<MouseButton>>().release(MouseButton::Left);
        frame(&mut app, 0.032);
        for button in [MouseButton::Left, MouseButton::Right, MouseButton::Middle] {
            app.world_mut().resource_mut::<ButtonInput<MouseButton>>().press(button);
        }
        frame(&mut app, 0.032);
        let controls = app.world().resource::<Controls>();
        assert_eq!(controls.vehicle, 1.0);
        assert!(controls.aim && controls.fire && controls.melee_pressed);
        assert!(!controls.special_held);
        let entity = app.world_mut().query_filtered::<Entity, With<PrimaryWindow>>()
            .single(app.world()).unwrap();
        app.world_mut().get_mut::<Window>(entity).unwrap().focused = false;
        frame(&mut app, 0.032);
        let controls = app.world().resource::<Controls>();
        assert_eq!(controls.vehicle, 0.0);
        assert!(!controls.aim && !controls.fire && !controls.melee_held);
        assert_eq!(app.world().get::<CursorOptions>(entity).unwrap().grab_mode, CursorGrabMode::None);
    }

    #[test]
    fn a_mouse_flick_between_camera_updates_is_retained() {
        let mut app = app();
        let entity = app.world_mut().query_filtered::<Entity, With<PrimaryWindow>>()
            .single(app.world()).unwrap();
        app.world_mut().get_mut::<CursorOptions>(entity).unwrap().grab_mode = CursorGrabMode::Locked;
        frame(&mut app, 0.032);
        app.world_mut().resource_mut::<AccumulatedMouseMotion>().delta = Vec2::new(8.0, 0.0);
        frame(&mut app, 0.008);
        assert!(app.world().resource::<Controls>().look_samples.is_empty());
        app.world_mut().resource_mut::<AccumulatedMouseMotion>().delta = Vec2::ZERO;
        frame(&mut app, 0.024);
        assert!(app.world().resource::<Controls>().look_samples[0].x > 0.7);
    }

    #[test]
    fn keyboard_camera_input_stays_held_without_mouse_capture() {
        let mut app = app();
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::ArrowRight);
        frame(&mut app, 0.032);
        for _ in 0..3 {
            frame(&mut app, 0.008);
            assert_eq!(app.world().resource::<Controls>().look, Vec2::X);
        }
    }
}
