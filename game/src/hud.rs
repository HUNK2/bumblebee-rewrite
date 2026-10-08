//! On-screen readout: what the simulation is doing and which parts of it are real.

use bevy::prelude::*;

use crate::animation::Animator;
use crate::camera::Rig;
use crate::player::{Player, TuningRes};
use crate::sim::{JumpType, Mode};
use crate::targets::EnemyTarget;
use crate::weapons::Arsenal;

use std::collections::{HashMap, HashSet};
use std::path::Path;

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use tf2_core::formats::gfx::{Character, Fill, Movie};
use tf2_core::hud::{Hud, Meter};
use tf2_core::formats::pack::{self, Pack};
use tf2_core::formats::texture;
use tf2_core::reticle;

/// The reticle's textures as the pack has them (`bnxglobal.str`, found by the hash of the
/// path the executable asks for), until `spawn` makes images of them.
#[derive(Resource)]
pub struct ReticleSource(Vec<(reticle::Texture, texture::Texture)>);

impl ReticleSource {
    pub fn load(game_dir: &Path) -> Option<Self> {
        let pack = Pack::open(&game_dir.join("bnxglobal.str")).inspect_err(|e| eprintln!("The reticle: {e}")).ok()?;
        let mut found = Vec::new();
        for chunk in pack.of_type(pack::TEXTURE) {
            let Ok(Some(texture)) = texture::parse(pack.data(chunk)) else { continue };
            if let Some(which) = reticle::Texture::ALL.into_iter().find(|t| t.id() == texture.id) {
                found.push((which, texture));
            }
        }
        if found.len() < reticle::Texture::ALL.len() {
            eprintln!("The reticle: {} of {} textures found in bnxglobal.str", found.len(), reticle::Texture::ALL.len());
        }
        (!found.is_empty()).then_some(Self(found))
    }
}

/// Each texture's image and its size in pixels.
#[derive(Resource)]
pub struct ReticleImages(HashMap<reticle::Texture, (Handle<Image>, Vec2)>);

/// One of the quads the reticle is drawn with, by its place in the frame's list.
#[derive(Component)]
pub struct ReticleQuad(usize);

/// More than the reticle ever draws in a frame.
const RETICLE_QUADS: usize = 24;

/// Places the frame's quads (`Arsenal::sprites`): the game's picture is 1280 x 720, fixed
/// places are scaled about the middle by the window's height, projected ones by the
/// window's own size. [assumed: a 16:9 picture; the original squeezes x by 0.75 on 4:3]
pub fn draw_reticle(
    arsenal: Option<Res<Arsenal>>,
    images: Option<Res<ReticleImages>>,
    window: Single<&Window>,
    mut quads: Query<(&ReticleQuad, &mut Node, &mut ImageNode, &mut UiTransform, &mut Visibility)>,
) {
    let (Some(arsenal), Some(images)) = (arsenal, images) else { return };
    let size = Vec2::new(window.width(), window.height());
    let k = size.y / reticle::PICTURE.y;
    for (quad, mut node, mut image, mut transform, mut visibility) in &mut quads {
        let shown = arsenal.sprites.get(quad.0).and_then(|sprite| Some((sprite, images.0.get(&sprite.texture)?)));
        let Some((sprite, (handle, pixels))) = shown else {
            if *visibility != Visibility::Hidden {
                *visibility = Visibility::Hidden;
            }
            continue;
        };
        let at = if sprite.projected { sprite.at / reticle::PICTURE * size } else { size * 0.5 + (sprite.at - reticle::PICTURE * 0.5) * k };
        let extent = sprite.size * k;
        node.left = Val::Px(at.x - extent.x * 0.5);
        node.top = Val::Px(at.y - extent.y * 0.5);
        node.width = Val::Px(extent.x);
        node.height = Val::Px(extent.y);
        let [u0, v0, u1, v1] = sprite.uv;
        image.image = handle.clone();
        image.flip_x = u0 > u1;
        image.rect = Some(Rect::new(u0 * pixels.x, v0 * pixels.y, u1 * pixels.x, v1 * pixels.y));
        image.color = Color::srgba(1.0, 1.0, 1.0, sprite.alpha.clamp(0.0, 1.0));
        transform.rotation = Rot2::radians(sprite.turn);
        *visibility = Visibility::Inherited;
    }
}

#[derive(Component)]
pub struct StatusText;

#[derive(Component)]
pub struct HelpPanel;

#[derive(Component)]
pub struct HealthFill;

#[derive(Component)]
pub struct DeathText;

/// Keeps the stand-in health bar (there only when the game's HUD could not be read) and
/// the death message up to date.
pub fn health(
    tuning: Res<TuningRes>,
    player: Single<&Player>,
    fill: Option<Single<(&mut Node, &mut BackgroundColor), With<HealthFill>>>,
    mut message: Single<&mut Text, With<DeathText>>,
) {
    let s = &player.state;
    if let Some(mut fill) = fill {
        let part = (s.health / tuning.0.base_health).clamp(0.0, 1.0);
        fill.0.width = Val::Percent(part * 100.0);
        fill.1.0 = if part < 0.3 { Color::srgb(0.9, 0.15, 0.1) } else { Color::srgb(0.95, 0.75, 0.1) };
    }
    message.0 = if s.dead() { "DESTROYED\nR to start again".to_owned() } else { String::new() };
}

/// The game's HUD movie (`UI\Flash\hud.gfx`) and what its three meters show
/// (`tf2_core::hud`: the movie's own shapes, placed as its scripts place them).
#[derive(Resource)]
pub struct FlashHud {
    movie: Movie,
    state: Hud,
    icon_movie: Option<Movie>,
    icon_textures: HashMap<String, texture::Texture>,
}

impl FlashHud {
    pub fn load(game_dir: &Path) -> Option<Self> {
        let path = game_dir.join("UI").join("Flash").join("hud.gfx");
        let file = std::fs::read(&path).inspect_err(|e| eprintln!("The HUD: {}: {e}", path.display())).ok()?;
        let movie = Movie::parse(&file).inspect_err(|e| eprintln!("The HUD: {e}")).ok()?;
        let icon_movie_path = game_dir.join("UI").join("Flash").join("globalassets.gfx");
        let icon_movie = std::fs::read(&icon_movie_path)
            .inspect_err(|e| eprintln!("The HUD icons: {}: {e}", icon_movie_path.display()))
            .ok()
            .and_then(|file| Movie::parse(&file).inspect_err(|e| eprintln!("The HUD icons: {e}")).ok());
        let icon_pack_path = game_dir.join("UI").join("Flash").join("globalassets.apk");
        let icon_textures = std::fs::read(&icon_pack_path)
            .inspect_err(|e| eprintln!("The HUD icons: {}: {e}", icon_pack_path.display()))
            .ok()
            .and_then(|file| texture::parse_library(&file).inspect_err(|e| eprintln!("The HUD icons: {e}")).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|image| (image.name.to_ascii_lowercase(), image))
            .collect();
        Some(Self { movie, state: Hud::default(), icon_movie, icon_textures })
    }

    fn icon_texture(&self, export: &str) -> Option<&texture::Texture> {
        let movie = self.icon_movie.as_ref()?;
        let sprite = *movie.exports.get(export)?;
        let bitmap = first_bitmap(movie, sprite, &mut HashSet::new())?;
        // The APK names the imported bitmaps with a lowercase hexadecimal ID. A decimal
        // lookup can silently pick a different faction badge (or miss the texture).
        self.icon_textures.get(&format!("globalassets_i{bitmap:x}").to_ascii_lowercase())
    }
}

fn first_bitmap(movie: &Movie, id: u16, visited: &mut HashSet<u16>) -> Option<u16> {
    if !visited.insert(id) {
        return None;
    }
    match movie.characters.get(&id)? {
        Character::Shape(shape) => shape.fills.iter().find_map(|fill| match fill {
            Fill::Bitmap { id, .. } if *id != u16::MAX => Some(*id),
            _ => None,
        }),
        Character::Sprite(sprite) => sprite.shown(1).into_iter().filter_map(|place| place.id).find_map(|child| first_bitmap(movie, child, visited)),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FlashIconKind {
    Ability,
    Turbo,
}

#[derive(Component)]
pub struct FlashHudIcon {
    kind: FlashIconKind,
    image: Handle<Image>,
}

/// One of the movie's meters: its picture, what it was drawn from and at what size.
#[derive(Component)]
pub struct FlashMeter {
    which: Meter,
    drawn: Option<([i32; 3], u32)>,
}

/// Steps the meters and draws again those that changed. What the executable hands the
/// movie (`UI\uiroot.lxb` ties each of the movie's fields to a named game value): [game]
/// `CurrentPlayerHealth` is health over the most (`FUN_00716890`);
/// `HUD_STAT_TURBO_COOLDOWN` is 1 while the turbo burns, then the cooldown left over the
/// cooldown, 0 when ready (`FUN_0071dc20`, `FUN_007324d0`); `HUD_STAT_ABILITY_COOLDOWN`
/// is the special's cooldown left over the cooldown, from the moment it is used
/// (`FUN_0072a550`). [assumed] the turbo meter shows in the car only.
pub fn draw_meters(
    time: Res<Time>,
    tuning: Res<TuningRes>,
    player: Single<&Player>,
    window: Single<&Window>,
    hud: Option<ResMut<FlashHud>>,
    mut images: ResMut<Assets<Image>>,
    mut meters: Query<(&mut FlashMeter, &mut Node, &mut ImageNode, &mut Visibility), (With<FlashMeter>, Without<FlashHudIcon>)>,
    mut icons: Query<(&FlashHudIcon, &mut Node, &mut ImageNode, &mut Visibility), (With<FlashHudIcon>, Without<FlashMeter>)>,
) {
    let Some(mut hud) = hud else { return };
    let (t, s) = (&tuning.0, &player.state);
    let cooling = |left: f32, cooldown: f32, whole: f32| if left > 0.0 { 1.0 } else if whole > 0.0 { (cooldown / whole).clamp(0.0, 1.0) } else { 0.0 };
    let turbo = cooling(s.turbo_left, s.turbo_cooldown, tf2_core::driving::turbo_times(&t.drive, s.turbo_upgrades).1);
    let ability = cooling(0.0, s.special_cooldown, t.special.cooldown);
    let FlashHud { movie, state, .. } = &mut *hud;
    state.step(movie, s.health / t.base_health, turbo, ability, time.delta_secs());
    let size = Vec2::new(window.width(), window.height());
    let k = size.y / reticle::PICTURE.y;
    for (icon, mut node, mut image, mut visibility) in &mut icons {
        let (center, extent, show) = match icon.kind {
            FlashIconKind::Ability => (Vec2::new(122.2, 600.0), Vec2::splat(64.0), true),
            // The turbo symbol's exported bounds are [-29, -24 .. 19, 24], and its
            // class clip places it at (142.4, 454.7) in the 1280 x 720 HUD. [data]
            FlashIconKind::Turbo => (Vec2::new(137.4, 454.7), Vec2::splat(48.0), s.is_vehicle()),
        };
        *visibility = if show { Visibility::Inherited } else { Visibility::Hidden };
        if !show {
            continue;
        }
        let at = size * 0.5 + (center - reticle::PICTURE * 0.5) * k;
        let extent = extent * k;
        node.left = Val::Px(at.x - extent.x * 0.5);
        node.top = Val::Px(at.y - extent.y * 0.5);
        node.width = Val::Px(extent.x);
        node.height = Val::Px(extent.y);
        if image.image != icon.image {
            image.image = icon.image.clone();
        }
    }
    // The movie is drawn at the window's own pixels.
    let scale = k * window.scale_factor();
    for (mut meter, mut node, mut image, mut visibility) in &mut meters {
        let show = meter.which != Meter::Turbo || s.is_vehicle();
        *visibility = if show { Visibility::Inherited } else { Visibility::Hidden };
        if !show {
            continue;
        }
        let rect = meter.which.stage_rect();
        let corner = size * 0.5 + (Vec2::new(rect[0], rect[1]) - reticle::PICTURE * 0.5) * k;
        node.left = Val::Px(corner.x);
        node.top = Val::Px(corner.y);
        node.width = Val::Px((rect[2] - rect[0]) * k);
        node.height = Val::Px((rect[3] - rect[1]) * k);
        let key = (state.key(meter.which), (scale * 64.0) as u32);
        if meter.drawn == Some(key) {
            continue;
        }
        let Some(canvas) = state.draw(movie, meter.which, scale) else { continue };
        let extent = Extent3d { width: canvas.width as u32, height: canvas.height as u32, depth_or_array_layers: 1 };
        let picture = Image::new(extent, TextureDimension::D2, canvas.rgba8(), TextureFormat::Rgba8UnormSrgb, RenderAssetUsages::default());
        if meter.drawn.is_some() && images.contains(&image.image) {
            images.insert(&image.image, picture).ok();
        } else {
            image.image = images.add(picture);
        }
        meter.drawn = Some(key);
    }
}

#[derive(Component)]
pub struct Crosshair;

const HELP: &str = "\
Keyboard and mouse                 Pad (the game's own bindings)
  W A S D     move / steer           left stick
  mouse       camera (click first)   right stick
  Space       jump / turbo           bottom face button
  L-Shift     hold: car form         R2 (hold, analog throttle)
  L-Ctrl / S  brake, reverse         L1
  L-Alt / RMB weapon mode; slide     L2
  LMB         fire (in weapon mode)  R2 (in weapon mode)
  Q           next weapon            R1
  C           camera distance        D-pad Up
  Home        camera recentre        R3 (press right stick)
  F           melee; held: charge    left face button
              in weapon mode with the stick: dodge
              in the air: tap attack, hold ground punch; in the car, held: its gun
  X           special (stun wave)    top face button
  E           climb, facing a wall   right face button
              on it: stick to climb, jump (stick back: off), press again: let go
  H  take damage    R  reset    F1  hide this    Esc  free the mouse

Test terrain behind the start: blue deck / orange launch ramp (left)
  green hill / purple bank (right); tower for climbing farther left
Suspension lanes to the right of the start, drive forward:
  cyan rollers / yellow alternating left-right bumps
Climbing building farther right/forward (x 221, y 100):
  coloured U-shaped wings, courtyard terraces, gold overhead bridge

From your install, the original's code, or a recording of the game
  models, textures, clips, sounds, every number; on-foot pace, jumps, air control, the car's
  rules (caps, braking, turbo, steering, wheels, slide), the follow, driving and weapon
  cameras, the aim the stick turns, the mouse as a stick, the change of form,
  weapon rate and heat, the aim point, a round's line and damage, the gun pulled out,
  recoiling, overheating and put away (and how those arm clips blend), the aim's
  rise bending him up and down, the axe in his hand, the missile's lock-on
  (hold fire on a target, let go) and its guidance, the car's gun, the special, the aim
  assist; when he stands, moves, jumps, falls, changes form, attacks, fires, dodges and
  climbs (the game's own state machine); the climb itself; weapon mode on foot
  (facing the aim, the eight strafing runs, the twist and turn while standing) and in
  the air (jumps facing the aim, the slow motion at the top; no recording to check it)
  the reticle, and the HUD's health, ability and turbo meters (and the values the
  game feeds them)
Stand-ins
  drawn shots and hits, engine revolutions,
  the lighting and materials, parts of which clip plays when, the arena's blocks as the
  level's ledge mesh, the dummies, their shape and what they do when hit (B draws what
  he hits with)
Not there yet
  the HUD's text, radar and messages, some effects, enemies";

fn panel(left: Option<f32>, top: Option<f32>, bottom: Option<f32>) -> impl Bundle {
    (
        Node {
            position_type: PositionType::Absolute,
            left: left.map_or(Val::Auto, Val::Px),
            top: top.map_or(Val::Auto, Val::Px),
            bottom: bottom.map_or(Val::Auto, Val::Px),
            padding: UiRect::all(Val::Px(8.0)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.55)),
        TextFont::from_font_size(15.0),
        TextColor(Color::srgb(0.95, 0.95, 0.9)),
    )
}

pub fn spawn(
    mut commands: Commands,
    source: Option<Res<ReticleSource>>,
    flash: Option<Res<FlashHud>>,
    tuning: Res<TuningRes>,
    mut images: ResMut<Assets<Image>>,
) {
    for which in Meter::ALL.into_iter().filter(|_| flash.is_some()) {
        commands.spawn((
            Node { position_type: PositionType::Absolute, ..default() },
            ImageNode::default(),
            Visibility::Hidden,
            FlashMeter { which, drawn: None },
        ));
    }
    if let Some(flash) = flash.as_deref() {
        let ability = tf2_core::hud::ability_icon_export(tuning.0.special.icon)
            .map(|name| (FlashIconKind::Ability, name));
        let turbo = tf2_core::hud::turbo_icon_export(12).map(|name| (FlashIconKind::Turbo, name));
        for (kind, name) in [ability, turbo].into_iter().flatten() {
            let Some(texture) = flash.icon_texture(name) else { continue };
            let Some(mut picture) = crate::assets::image(texture, true) else { continue };
            picture.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                address_mode_u: ImageAddressMode::ClampToEdge,
                address_mode_v: ImageAddressMode::ClampToEdge,
                ..ImageSamplerDescriptor::linear()
            });
            let image = images.add(picture);
            commands.spawn((
                Node { position_type: PositionType::Absolute, ..default() },
                ImageNode::default(),
                Visibility::Hidden,
                FlashHudIcon { kind, image },
            ));
        }
    }
    // The reticle's quads, under everything else of the readout. [game: the textures and
    // where they go; see `tf2_core::reticle`]
    if let Some(source) = source {
        let mut made = HashMap::new();
        for (which, texture) in &source.0 {
            let Some(mut image) = crate::assets::image(texture, true) else { continue };
            // A fill is a part of its texture: the edges must not wrap round.
            image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                address_mode_u: ImageAddressMode::ClampToEdge,
                address_mode_v: ImageAddressMode::ClampToEdge,
                ..ImageSamplerDescriptor::linear()
            });
            made.insert(*which, (images.add(image), Vec2::new(texture.width as f32, texture.height as f32)));
        }
        commands.insert_resource(ReticleImages(made));
        commands.remove_resource::<ReticleSource>();
        for index in 0..RETICLE_QUADS {
            commands.spawn((
                Node { position_type: PositionType::Absolute, ..default() },
                ImageNode::default(),
                UiTransform::default(),
                Visibility::Hidden,
                ReticleQuad(index),
            ));
        }
    }
    commands.spawn((Text::new(""), panel(Some(12.0), Some(10.0), None), StatusText));
    commands.spawn((Text::new(HELP), panel(Some(12.0), None, Some(10.0)), HelpPanel));
    // His health: a bar at the bottom right. [stand-in, there only when the game's HUD
    // movie could not be read]
    if flash.is_none() {
        commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(20.0),
                bottom: Val::Px(20.0),
                width: Val::Px(260.0),
                height: Val::Px(18.0),
                padding: UiRect::all(Val::Px(2.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.6)),
        ))
        .with_child((Node { width: Val::Percent(100.0), height: Val::Percent(100.0), ..default() }, BackgroundColor(Color::srgb(0.95, 0.75, 0.1)), HealthFill));
    }
    commands.spawn((
        Text::new(""),
        Node { position_type: PositionType::Absolute, top: Val::Percent(40.0), width: Val::Percent(100.0), justify_content: JustifyContent::Center, ..default() },
        TextFont::from_font_size(40.0),
        TextColor(Color::srgb(0.9, 0.2, 0.15)),
        TextLayout::justify(Justify::Center),
        DeathText,
    ));
    // The middle of the picture, which is what his weapons shoot at. [stand-in, shown
    // only when the reticle's textures could not be read]
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            Visibility::Hidden,
            Crosshair,
        ))
        .with_child((Text::new("+"), TextFont::from_font_size(28.0), TextColor(Color::srgb(1.0, 0.9, 0.4))));
}

pub fn toggle_help(keys: Res<ButtonInput<KeyCode>>, shot: Option<Res<crate::Shot>>, mut help: Single<&mut Visibility, With<HelpPanel>>) {
    // A capture is taken without the help, which would lie over the readout.
    if shot.is_some() {
        if **help != Visibility::Hidden {
            **help = Visibility::Hidden;
        }
        return;
    }
    if keys.just_pressed(KeyCode::F1) {
        **help = if **help == Visibility::Hidden { Visibility::Inherited } else { Visibility::Hidden };
    }
}

pub fn update(
    tuning: Res<TuningRes>,
    rig: Res<Rig>,
    arsenal: Option<Res<Arsenal>>,
    player: Single<&Player>,
    animator: Single<&Animator>,
    dummies: Query<&EnemyTarget>,
    reticle_images: Option<Res<ReticleImages>>,
    mut text: Single<&mut Text, With<StatusText>>,
    mut crosshair: Single<&mut Visibility, (With<Crosshair>, Without<HelpPanel>)>,
) {
    let t = &tuning.0;
    let s = &player.state;
    **crosshair = if player.aiming && reticle_images.is_none() { Visibility::Inherited } else { Visibility::Hidden };
    let clip = animator.describe();
    let mode = match s.mode {
        Mode::Unfold => "robot, unfolding out of the car".to_owned(),
        Mode::Move if s.action.is_some() => "robot, folding into the car".to_owned(),
        Mode::Strafe => format!(
            "robot, weapon mode (facing {:.0}, stick {:.0} from the aim, spine twisted {:.0})",
            s.yaw.to_degrees(),
            s.strafe.angle,
            s.strafe.twist.to_degrees()
        ),
        Mode::Move => "robot, on foot".to_owned(),
        Mode::Stand => "robot, standing".to_owned(),
        Mode::Drive if s.car_airborne() => "car, in the air".to_owned(),
        Mode::Drive if s.sliding() => "car, sliding".to_owned(),
        Mode::Drive => "car".to_owned(),
        Mode::Action => format!("robot, {}", s.control_state(t).unwrap_or("?")),
        Mode::Climb => match &s.climb {
            Some(c) => {
                let at = c.wall.local(s.pos);
                format!(
                    "robot, climbing ({:?}, {} hand), {:.1} along {:.1} and {:.1} up {:.1}",
                    c.phase,
                    if c.right_hand { "right" } else { "left" },
                    at.x,
                    c.wall.width,
                    at.z,
                    c.wall.height
                )
            }
            None => "robot, climbing".to_owned(),
        },
        Mode::Jump(_) if s.action.is_some() => format!("robot in the air, {}", s.control_state(t).unwrap_or("?")),
        Mode::Jump(_) if s.weapon_air() => format!(
            "robot in the air, weapon mode (facing {:.0}, time factor {:.2})",
            s.yaw.to_degrees(),
            s.dilation
        ),
        Mode::Jump(kind) => format!(
            "robot, {}",
            match kind {
                JumpType::Standing => "standing jump",
                JumpType::Moving => "running jump",
                JumpType::HighStanding => "high standing jump (out of the car)",
                JumpType::Long => "long jump (out of the car)",
                JumpType::Fall => "falling",
                JumpType::Climb => "jump off a wall",
            }
        ),
    };
    let speed = Vec2::new(s.vel.x, s.vel.y).length();
    let top = if s.is_vehicle() {
        if s.turbo_left > 0.0 { t.drive.max_turbo_speed } else { t.drive.max_speed }
    } else {
        t.moving.max_speed
    };
    let turbo = if s.turbo_left > 0.0 {
        format!("boosting, {:.1} s left of {:.1}", s.turbo_left, tf2_core::driving::turbo_times(&t.drive, s.turbo_upgrades).0)
    } else if s.turbo_cooldown > 0.0 {
        format!("cooling down, {:.1} s of {:.1}", s.turbo_cooldown, tf2_core::driving::turbo_times(&t.drive, s.turbo_upgrades).1)
    } else {
        "ready".to_owned()
    };
    let camera = match (rig.camera.cornered(), rig.camera.blocked()) {
        (true, _) => "round a corner",
        (false, true) => "pulled in",
        (false, false) => "clear",
    };
    let distance = (rig.view.eye - s.pos).length();
    let weapon = arsenal.map_or_else(|| "none loaded".to_owned(), |arsenal| arsenal.describe());
    let mut standing: Vec<(u32, String)> = dummies
        .iter()
        .map(|d| (d.id, if d.down_for > 0.0 { "down".to_owned() } else { format!("{:.0}", d.health) }))
        .collect();
    standing.sort();
    let dummies = standing.into_iter().map(|(_, health)| health).collect::<Vec<_>>().join(", ");
    let going_for = s.action.as_ref().and_then(|a| a.target).map_or(String::new(), |id| format!("     going for scout {id}"));
    text.0 = format!(
        "{}  -  {mode}\n\
         speed {speed:5.1} of {top:.0}     height {:4.1}     last jump peaked {:.1} above take-off\n\
         health {:.0} of {:.0}     turbo: {turbo}\n\
         weapon: {weapon}\n\
         scouts: {dummies}{going_for}\n\
         camera: {camera}, {distance:.1} from him, view {:.0} degrees\n\
         clip: {clip}",
        t.name,
        s.pos.z,
        s.apex_z - s.launch_z,
        s.health,
        t.base_health,
        rig.view.fov,
    );
}
