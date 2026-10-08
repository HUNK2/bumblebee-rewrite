//! A standalone Bevy sandbox for testing the decoded Transformers: Revenge of the Fallen data.
//!
//! It reads the game's packs from the install at runtime and ships none of them. Usage:
//!
//!   bumblebee                             run the sandbox as Bumblebee
//!   bumblebee     --check [pack]          load a character pack and report, no window
//!   bumblebee     --shot <file.png> [--after <s>] [--yaw <deg>] [--elev <deg>] [--no-hud]
//!                 [--clip <Set/Id@tick>] [--stick <0..1>] [--jump-at <s>] [--die-at <s>]
//!                 [--vehicle | --vehicle-from <s> [--vehicle-until <s>]] [--fire-from <s> [--fire-until <s>]]
//!                 [--stick-x <-1..1>] [--aim-from <s>] [--melee-at <s>,<s>...]
//!                 [--melee-hold-from <s>] [--special-at <s>]
//!                 [--hit-at <s> [--hit-flags <hex>]]
//!                 [--climb-from <s>] [--start <x,y,yaw>] [--bodies]
//!                                         render for a moment, save one frame, and quit;
//!                                         the second and third lines hold a pose or feed
//!                                         input meanwhile
//!   ... [--robot-camera <0..2>] [--drive-camera <0..2>] [--no-sound]
//!                                         the game's camera distance levels; silence
//!
//! The original PC game install must be supplied through TF2_GAME_DIR.

mod animation;
mod trails;
mod assets;
mod camera;
mod check;
mod effects;
mod skidmarks;
mod ssd;
mod fx;
mod hud;
mod player;
mod rumble;
mod sound;
mod speedblur;
mod surface;
mod texture_upscale;
mod targets;
mod weapons;
mod world;

// The decoders, the movement, the cameras, the sound mixer and the weapon rules live in
// `tf2-core`, which has no engine types; these imports keep the `crate::formats`,
// `crate::sim`... paths used across the test.
use tf2_core::{character, control, formats, sim, tuning};

use std::path::PathBuf;

use bevy::core_pipeline::prepass::DepthPrepass;
use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};

use crate::animation::{AnimLibrary, Animator, VehicleAnimator};
use crate::character::CharacterData;
use crate::player::{Controls, Player, RobotModel, TuningRes, VehicleModel};

const CHARACTER_PACK: &str = "bumblebee";
const CHARACTER_NAME: &str = "Bumblebee";
/// Seconds to let the scene settle before a capture, and to wait for the file after it.
const SHOT_DELAY: f32 = 2.5;
const SHOT_TIMEOUT: f32 = 10.0;

#[derive(Resource)]
struct GameData(CharacterData);

/// A clip (set, id) and the tick to hold the robot at.
#[derive(Resource)]
struct HeldClip(String, String, f32);

/// How he starts.
#[derive(Resource)]
struct Start(sim::State);

#[derive(Resource)]
struct Shot {
    path: String,
    delay: f32,
    taken_at: Option<f32>,
    fx_stats: bool,
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let texture_scale = texture_upscale::factor(&args).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    });
    let game_dir = match std::env::var_os("TF2_GAME_DIR").filter(|path| !path.is_empty()) {
        Some(path) => PathBuf::from(path),
        None => {
            eprintln!("Your own original PC game install is required. Set TF2_GAME_DIR to the folder containing bnxglobal.str, or use play.ps1 -GameDirectory <folder>.");
            std::process::exit(1);
        }
    };
    if args.first().map(String::as_str) == Some("--check") {
        let pack = args.get(1).map_or(CHARACTER_PACK, String::as_str);
        if let Err(e) = check::run(&game_dir, pack) {
            eprintln!("check failed: {e}");
            std::process::exit(1);
        }
        return;
    }

    let loaded = tuning::load(&game_dir, CHARACTER_NAME)
        .and_then(|tuning| character::load(&game_dir, CHARACTER_PACK).map(|data| (tuning, data)));
    let (mut tuning, mut data) = match loaded {
        Ok(loaded) => loaded,
        Err(e) => {
            eprintln!("Could not read the game data under {}: {e}", game_dir.display());
            eprintln!("Set TF2_GAME_DIR to the folder that holds bnxglobal.str.");
            std::process::exit(1);
        }
    };
    texture_upscale::apply(&mut data.library, texture_scale);
    let animations =
        AnimLibrary { robot: std::mem::take(&mut data.animations), vehicle: std::mem::take(&mut data.vehicle_animations) };
    tuning.locomotion = tuning::Locomotion::from_animations(&animations.robot, &mut tuning.missing);
    tuning.actions =
        tuning::Actions::from_animations(&animations.robot, &data.robot, &mut tuning.missing).with_vehicle(data.vehicle.as_ref());
    tuning.weapon_set = tuning::RuleSet::from_animations(&animations.robot, "WeaponSet", &mut tuning.missing);
    tuning.rule_sets = tuning::RuleSet::all(&animations.robot);
    for name in &tuning.missing {
        eprintln!("tuning value not found in the game data: {name}");
    }
    for problem in &data.library.problems {
        eprintln!("skipped: {problem}");
    }
    // The axle spacing is not a tuning number; measure it on the vehicle model.
    if let Some(vehicle) = &data.vehicle {
        let along = |name: &str| vehicle.nodes.iter().find(|n| n.name == name).map(|n| n.bind[3][1]);
        if let (Some(front), Some(rear)) = (along("Tire_Front_L"), along("Tire_Rear_L")) {
            tuning.wheelbase = (front - rear).abs();
        }
    }

    let number = |flag: &str| flag_value(&args, flag).and_then(|v| v.parse::<f32>().ok());
    // The game has three distance levels for each camera. The defaults are the ones the
    // recorded game ran with: the near follow camera and the far driving camera.
    let level = |flag: &str, default: usize| number(flag).map_or(default, |v| v as usize);
    if let Some(camera) = tuning.robot_camera_levels.get(level("--robot-camera", 0)) {
        tuning.robot_camera = camera.clone();
    }
    if let Some(camera) = tuning.drive_camera_levels.get(level("--drive-camera", 2)) {
        tuning.drive_camera = camera.clone();
    }
    // `--start x,y,yaw` puts him somewhere else to begin with (yaw in degrees, 0 along +y).
    let mut start = sim::State::new(&tuning);
    // [trace] The reference capture has4.8s turbo and1.6s cooldown,
    // matching level3 of both installed upgrade tables. Save import is still absent.
    let turbo_upgrade = level("--turbo-upgrade", 3).min(3) as u8;
    start.turbo_upgrades = [turbo_upgrade; 2];
    // [stand-in] Arena difficulty selector; importing the original save is not implemented.
    let difficulty=flag_value(&args,"--difficulty").unwrap_or_else(||"medium".into());
    let difficulty_index=match difficulty.to_ascii_lowercase().as_str() {"easy"=>0,"medium"=>1,"hard"=>2,"expert"=>3,_=>panic!("--difficulty must be easy, medium, hard or expert")};
    start.difficulty=tuning.damage.difficulties[difficulty_index];
    if let Some(spec) = flag_value(&args, "--start") {
        let v: Vec<f32> = spec.split(',').filter_map(|p| p.parse().ok()).collect();
        if let [x, y, yaw] = v[..] {
            start.pos = Vec3::new(x, y, 0.0);
            start.yaw = yaw.to_radians();
        }
    }
    let mut rig = camera::Rig::new(&start, &tuning, number("--yaw"), number("--elev"));
    rig.camera.options.sensitivity=number("--camera-sensitivity").unwrap_or(0.5).clamp(0.0,1.0);
    rig.camera.options.invert_yaw=args.iter().any(|a|a=="--invert-camera-x");
    rig.camera.options.invert_pitch=args.iter().any(|a|a=="--invert-camera-y");
    rig.camera.options.separate_distances=args.iter().any(|a|a=="--separate-camera-distances");
    rig.camera.options.disable_stick=args.iter().any(|a|a=="--disable-camera-stick");
    rig.camera.options.widescreen=!args.iter().any(|a|a=="--camera-4-3");
    rig.orbit = number("--orbit").unwrap_or(0.0);
    rig.zoom = number("--zoom").unwrap_or(1.0).max(0.1);
    let scout = tf2_core::combat::Scout::load(&game_dir).expect("Could not load the installed Decepticon scout");
    let sound = if args.iter().any(|a| a == "--no-sound") { None } else { sound::start(&game_dir, &data, &animations, &tuning, &scout) };
    let arsenal = weapons::Arsenal::load(&game_dir, &data.weapons);
    let enemy_layout = targets::layout(&scout.tuning);
    let encounter = targets::Encounter { scout, passive: args.iter().any(|a|a=="--passive-enemies"), report: args.iter().any(|a|a=="--combat-report") };
    let ssd_textures=ssd::load(&game_dir).expect("Could not load the original surface damage textures");
    let turbo_blur = speedblur::load(&game_dir, args.iter().any(|a| a == "--no-speed-blur"),
        args.iter().any(|a| a == "--speed-blur-report")).expect("Could not load original turbo blur data");
    // The arena's light and look: a level's own environment (`world::LEVEL`), unless asked not to.
    if !args.iter().any(|a| a == "--no-level-look") {
        world::set_look(tf2_core::formats::environment::load(&game_dir, world::LEVEL).inspect_err(|e| eprintln!("No level look: {e}")).ok());
    }
    let controls = Controls {
        // `--vehicle` holds the trigger throughout; the two times bound it instead.
        vehicle_window: (args.iter().any(|a| a == "--vehicle") || number("--vehicle-from").is_some())
            .then(|| (number("--vehicle-from").unwrap_or(0.0), number("--vehicle-until").unwrap_or(f32::MAX))),
        force_stick: Vec2::new(number("--stick-x").unwrap_or(0.0), number("--stick").unwrap_or(0.0)),
        jump_at: flag_value(&args, "--jump-at").and_then(|v| v.parse().ok()),
        die_at: number("--die-at"),
        keep_dead: args.iter().any(|a|a=="--keep-dead"),
        hit_at: number("--hit-at"),
        hit_flags: flag_value(&args, "--hit-flags").and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok()).unwrap_or(0),
        fire_from: number("--fire-from"),
        fire_until: number("--fire-until"),
        aim_from: number("--aim-from"),
        // Taps of the melee button, at times given as `0.5,1.1,1.7`.
        melee_at: flag_value(&args, "--melee-at")
            .map(|v| v.split(',').filter_map(|t| t.parse().ok()).collect())
            .unwrap_or_default(),
        melee_hold_from: number("--melee-hold-from"),
        melee_hold_until: number("--melee-hold-until"),
        climb_from: number("--climb-from"),
        force_look_y: number("--look-y").unwrap_or(0.0),
        switch_at: number("--switch-at"),
        camera_cycle_at: number("--camera-cycle-at"),
        recentre_at: number("--recentre-at"),
        mouse_divisor: number("--mouse-sensitivity").map(tf2_core::input::mouse_divisor),
        special_at: number("--special-at"),
        ..Default::default()
    };
    // `--clip Set/Id@tick` holds the robot in one frame of one clip.
    let held = flag_value(&args, "--clip").and_then(|spec| {
        let (name, tick) = spec.split_once('@').unwrap_or((&spec, "0"));
        let (set, id) = name.split_once('/')?;
        Some(HeldClip(set.to_owned(), id.to_owned(), tick.parse().unwrap_or(0.0)))
    });

    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "Transformers RotF data test (Bevy)".into(),
            resolution: (1280, 720).into(),
            ..default()
        }),
        ..default()
    }))
    .add_plugins(surface::plugin)
    .add_plugins(fx::plugin)
    .add_plugins(ssd::plugin)
    .add_plugins(speedblur::plugin)
    .insert_resource(turbo_blur)
    .insert_resource(ssd_textures)
    .init_resource::<ssd::Marks>()
    .insert_resource(ssd::Report(args.iter().any(|a|a=="--ssd-report")))
    .add_systems(Update,ssd::run.after(player::tick))
    .insert_resource(world::ArenaRes(sim::Arena { targets: enemy_layout, ..world::layout(&tuning) }))
    .insert_resource(encounter)
    .insert_resource(TuningRes(tuning))
    .insert_resource(GameData(data))
    .insert_resource(animations)
    .init_resource::<trails::Trails>()
    .add_systems(PostUpdate,trails::run.after(bevy::transform::TransformSystems::Propagate))
    .insert_resource(rig)
    .init_resource::<camera::Occluders>()
    .add_systems(Update,camera::fade_occluders.after(player::tick))
    .insert_resource(controls)
    .insert_resource(Start(start))
    .add_systems(Startup, (ssd::setup, world::spawn, targets::spawn, spawn_player).chain())
    .insert_resource(targets::ShowBodies(args.iter().any(|a| a == "--bodies")))
    .add_systems(Update, (targets::draw_bodies, targets::update).after(player::tick).before(player::present))
    .add_systems(Update, targets::animate.after(targets::update))
    .init_resource::<effects::Effects>()
    .init_resource::<skidmarks::SkidMarks>()
    .add_systems(Update, skidmarks::run.after(player::tick))
    .add_systems(Update, (effects::start.after(animation::animate), effects::run.after(effects::start).after(camera::apply)))
    .init_resource::<rumble::PadRumble>()
    .add_systems(Update, rumble::play.after(player::tick))
    .add_systems(
        Update,
        (camera::grab_cursor, player::read_input, camera::cycle_distance, camera::collect_occluders, player::tick, player::present, animation::animate, camera::apply).chain(),
    );
    if let Some(sound) = sound {
        app.insert_resource(sound);
    }
    if let Some(arsenal) = arsenal {
        app.insert_resource(arsenal)
            .add_systems(Startup, weapons::attach_models.after(spawn_player))
            .add_systems(Update, (weapons::draw, weapons::show_models, weapons::show_axe).after(player::tick));
    }
    if let Some(held) = held {
        app.insert_resource(held);
    }
    if !args.iter().any(|a| a == "--no-hud") {
        if let Some(source) = hud::ReticleSource::load(&game_dir) {
            app.insert_resource(source);
        }
        if let Some(flash) = hud::FlashHud::load(&game_dir) {
            app.insert_resource(flash);
        }
        app.add_systems(Startup, hud::spawn).add_systems(
            Update,
            (hud::toggle_help, hud::update, hud::health, hud::draw_reticle.after(player::tick), hud::draw_meters.after(player::tick)),
        );
    }
    if let Some(path) = flag_value(&args, "--shot") {
        let delay = flag_value(&args, "--after").and_then(|v| v.parse().ok()).unwrap_or(SHOT_DELAY);
        app.insert_resource(Shot { path, delay, taken_at: None,fx_stats:args.iter().any(|a|a=="--fx-stats") })
            .add_systems(Update, take_shot.after(effects::run));
    }
    app.run();
}

fn spawn_player(
    mut commands: Commands,
    data: Res<GameData>,
    tuning: Res<TuningRes>,
    start: Res<Start>,
    held: Option<Res<HeldClip>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut surfaces: ResMut<Assets<surface::Surface>>,
    mut glows: ResMut<Assets<fx::Glow>>,
    mut images: ResMut<Assets<Image>>,
    mut bindposes: ResMut<Assets<SkinnedMeshInverseBindposes>>,
) {
    let mut cache = assets::Cache::default();
    let mut builder = assets::Builder {
        library: &data.0.library,
        meshes: &mut meshes,
        materials: &mut materials,
        surfaces: &mut surfaces,
        glows: &mut glows,
        images: &mut images,
        bindposes: &mut bindposes,
        cache: &mut cache,
    };
    let (robot, rest) = builder.spawn(&mut commands, &data.0.robot, "robot");
    let mut animator = Animator::new(&rest);
    animator.forced = held.map(|h| (h.0.clone(), h.1.clone(), h.2));
    commands.entity(robot).insert((RobotModel, animator));
    // Each weapon's own model; `weapons::attach_models` hangs it on its node, whose space
    // is already the game's, so the root's turn into Bevy's space is taken off again.
    for (def, object) in data.0.weapons.iter().zip(&data.0.weapon_objects) {
        let Some(object) = object else { continue };
        let (model, _) = builder.spawn(&mut commands, object, "weapon");
        let hung = weapons::WeaponModel { name: def.name, bone: def.bone };
        commands.entity(model).insert((hung, Transform::IDENTITY, Visibility::Hidden));
    }
    // The melee weapon's model; `weapons::show_axe` hangs it on the node its events name.
    if let Some(object) = data.0.melee_weapon.as_ref().and_then(|weapon| weapon.object.as_ref()) {
        let (model, _) = builder.spawn(&mut commands, object, "axe");
        commands.entity(model).insert((weapons::AxeModel { hung: 0 }, Transform::IDENTITY, Visibility::Hidden));
    }
    let player = commands
        .spawn((Player::new(start.0.clone()), Transform::default(), Visibility::default()))
        .add_child(robot)
        .id();
    if let Some(vehicle) = &data.0.vehicle {
        let (vehicle, _) = builder.spawn(&mut commands, vehicle, "vehicle");
        commands.entity(vehicle).insert((VehicleModel, VehicleAnimator::default(), Visibility::Hidden));
        commands.entity(player).add_child(vehicle);
    }

    let camera = commands.spawn((
        Camera3d::default(),
        // The scene's depth, for the particles that fade where they near what is behind
        // them (`fx.wgsl`).
        DepthPrepass,
        Projection::Perspective(PerspectiveProjection {
            fov: tuning.0.robot_camera.fov.to_radians(),
            // [game] base camera constructor00781a00.
            near: 0.5,
            far: 1500.0,
            ..default()
        }),
        Transform::from_xyz(0.0, 6.0, 14.0).looking_at(Vec3::new(0.0, 3.0, 0.0), Vec3::Y),
    )).id();
    if let Some(look) = world::camera_look() {
        commands.entity(camera).insert(look);
    }
}

fn take_shot(time: Res<Time>, fx: Res<effects::Effects>, mut commands: Commands, mut shot: ResMut<Shot>, mut exit: MessageWriter<AppExit>) {
    let now = time.elapsed_secs();
    match shot.taken_at {
        None if now >= shot.delay => {
            if shot.fx_stats {
                eprintln!("vehicle FX: {} bullet visuals, {} bullet sprites submitted, {} collision visuals",
                    fx.bullet_visuals_started,fx.bullet_sprites_submitted,fx.collision_visuals_started);
                let (emitters, particles) = fx.gun_smoke_stats();
                eprintln!("gun smoke: {emitters} emitting effects, {particles} remaining particles");
            }
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(shot.path.clone()));
            shot.taken_at = Some(now);
        }
        // Quit once the file is there, or give up waiting for it.
        Some(at) if std::path::Path::new(&shot.path).exists() || now - at > SHOT_TIMEOUT => {
            exit.write(AppExit::Success);
        }
        _ => {}
    }
}
