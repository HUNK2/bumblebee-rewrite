//! Input, the movement, the camera and the weapons each frame, and showing the result on
//! the loaded models.

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

use crate::assets::{ObjectNodes, bevy_to_game, game_to_bevy};
use crate::camera::{self, MOUSE_COUNTS_FOR_FULL_STICK, Rig};
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

/// Each axis of a stick reads as centred inside this, and the rest is stretched to run
/// from 0 to 1 again (`FUN_005aa890`, the engine's pad filter; the value is set by
/// `FUN_005aa700`). It is per axis, not round. [game] Whether the Beenox reader at
/// `004ec6c0` is fed from this filter or from the raw pad was not followed.
const STICK_DEAD_ZONE: f32 = 0.25;

fn dead_zone(stick: Vec2) -> Vec2 {
    let axis = |v: f32| {
        if v.abs() < STICK_DEAD_ZONE { 0.0 } else { v.signum() * ((v.abs() - STICK_DEAD_ZONE) / (1.0 - STICK_DEAD_ZONE)).min(1.0) }
    };
    Vec2::new(axis(stick.x), axis(stick.y))
}

pub fn read_input(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    mouse: Res<AccumulatedMouseMotion>,
    cursor: Single<&CursorOptions, With<PrimaryWindow>>,
    pads: Query<&Gamepad>,
    mut controls: ResMut<Controls>,
) {
    let now = time.elapsed_secs();
    let axis = |negative: KeyCode, positive: KeyCode| {
        (keys.pressed(positive) as i32 - keys.pressed(negative) as i32) as f32
    };
    let mut stick =
        controls.force_stick + Vec2::new(axis(KeyCode::KeyA, KeyCode::KeyD), axis(KeyCode::KeyS, KeyCode::KeyW));
    if controls.die_at.is_some_and(|at| now >= at) {
        controls.die_at = None;
        controls.damage_pressed = true;
        controls.die_all = true;
    }
    if controls.jump_at.is_some_and(|at| now >= at) {
        controls.jump_at = None;
        controls.jump_pressed = true;
    }
    let grabbed = cursor.grab_mode == CursorGrabMode::Locked;
    let firing = controls.fire_from.is_some_and(|from| now >= from) && controls.fire_until.is_none_or(|until| now < until);
    let scripted_aim = controls.aim_from.is_some_and(|from| now >= from);
    let scripted_melee = controls.melee_at.first().is_some_and(|&at| now >= at);
    if scripted_melee {
        controls.melee_at.remove(0);
    }
    let mut look = if scripted_aim || firing { Vec2::new(0.0, controls.force_look_y) } else { Vec2::ZERO };
    // The pad follows the game's own bindings: R2 vehicle mode, throttle and fire, L2 weapon
    // mode and the slide, L1 brake, R1 next weapon, the bottom face button jump and turbo.
    let scripted = controls.vehicle_window.is_some_and(|(from, until)| (from..until).contains(&now));
    let mut vehicle = if keys.pressed(KeyCode::ShiftLeft) || scripted { 1.0 } else { 0.0 };
    let mut brake = if keys.pressed(KeyCode::ControlLeft) { 1.0 } else { 0.0 };
    let mut aim =
        keys.pressed(KeyCode::AltLeft) || (grabbed && buttons.pressed(MouseButton::Right)) || firing || scripted_aim;
    let mut switch = keys.pressed(KeyCode::KeyQ) || controls.switch_at.is_some_and(|at| (at..at + 0.1).contains(&now));
    let mut jump = keys.just_pressed(KeyCode::Space);
    let mut recentre=keys.just_pressed(KeyCode::Home)||controls.recentre_at.is_some_and(|at|now>=at);
    if recentre {controls.recentre_at=None;}
    let mut jump_held = keys.pressed(KeyCode::Space);
    let mut melee = keys.just_pressed(KeyCode::KeyF) || scripted_melee;
    let mut melee_held = keys.pressed(KeyCode::KeyF) || controls.melee_hold_from.is_some_and(|from| now >= from && controls.melee_hold_until.is_none_or(|until| now < until));
    let scripted_climb = controls.climb_from.is_some_and(|from| now >= from);
    let mut climb = keys.just_pressed(KeyCode::KeyE) || (scripted_climb && !controls.climb_held);
    let mut climb_held = keys.pressed(KeyCode::KeyE) || scripted_climb;
    let scripted_special = controls.special_at.is_some_and(|at| now >= at);
    if scripted_special {
        controls.special_at = None;
    }
    let mut special = keys.just_pressed(KeyCode::KeyX) || scripted_special;
    let mut special_held = keys.pressed(KeyCode::KeyX) || scripted_special;
    for pad in &pads {
        // The top face button is the special ability in the game's bindings
        // (`AttackSpecial` on `DPAD_R_UP`). [data]
        special |= pad.just_pressed(GamepadButton::North);
        special_held |= pad.pressed(GamepadButton::North);
        melee |= pad.just_pressed(GamepadButton::West);
        melee_held |= pad.pressed(GamepadButton::West);
        // The right face button is action and climb in the game's bindings.
        climb |= pad.just_pressed(GamepadButton::East);
        climb_held |= pad.pressed(GamepadButton::East);
        stick += dead_zone(pad.left_stick());
        look += dead_zone(pad.right_stick());
        vehicle = f32::max(vehicle, pad.get(GamepadButton::RightTrigger2).unwrap_or(0.0));
        brake = f32::max(brake, pad.get(GamepadButton::LeftTrigger).unwrap_or(0.0));
        aim |= pad.get(GamepadButton::LeftTrigger2).unwrap_or(0.0) > 0.3;
        switch |= pad.pressed(GamepadButton::RightTrigger);
        jump |= pad.just_pressed(GamepadButton::South);
        jump_held |= pad.pressed(GamepadButton::South);
        // [data] CameraReset=R3 in the installed Controller table.
        recentre |= pad.just_pressed(GamepadButton::RightThumb);
    }
    controls.stick = stick.clamp_length_max(1.0);
    // The mouse is a camera stick too: counts this frame over the divisor, each way
    // stopping at 1. [game]
    if grabbed {
        let pushed = (mouse.delta / MOUSE_COUNTS_FOR_FULL_STICK).clamp(Vec2::NEG_ONE, Vec2::ONE);
        look += Vec2::new(pushed.x, -pushed.y);
    }
    controls.look = look.clamp(Vec2::NEG_ONE, Vec2::ONE);
    controls.vehicle = vehicle;
    controls.brake = brake;
    controls.aim = aim;
    controls.fire = (grabbed && buttons.pressed(MouseButton::Left)) || firing;
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
    controls.damage_pressed |= keys.just_pressed(KeyCode::KeyH);
    controls.reset_pressed |= keys.just_pressed(KeyCode::KeyR);
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
    let view = rig.camera.step(&target, controls.look, t, &world, dt);
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
