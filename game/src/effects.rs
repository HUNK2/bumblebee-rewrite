//! The game's particle effects, run by `tf2_core::particles` and drawn as the original
//! draws them: the quads of its builders (`particles::Sprite`) with its own particle
//! shaders (`fx.wgsl`).
//!
//! Who starts an effect, and how sure that is:
//! - a melee hit: the impact preset's effect for the surface struck (`FUN_0077cd60`) [game]
//! - a clip's trigger message: the effects on his object that go by that name (the ground
//!   punches' `EffectSpawner` and `TriggerableParticle`, the special's) [game]
//! - a clip's `AnimSpawnEffectEvent` / `AnimStopEffectEvent` (landings, the change of
//!   form, the dodge, the charge, dying) [game]
//! - a clip's `FootStepEvent`: the named attachment probes the ground and selects
//!   its material preset [game/data; synchronous final-pose timing assumed]
//! - a weapon's muzzle flash, its round's impact effect by the surface and a missile's
//!   explosion [data] [assumed: when and where; the weapon code that starts them is not
//!   read. The flash is turned with its x along the shot, which is the way its particles
//!   leave in the data]
//! - the messages a state keeps on (`tf2_core::fxstate`): the exhaust, the headlights, the
//!   turbo's fire, the damage smoke, the health coming back [game: who sends each;
//!   `notes\particles.md` "The senders"]
//!
//! An `EffectSpawner`'s own object (the punches' shock ring, the special's dome) is its
//! mesh from his pack, moved by its own clip and removed by its `TimeoutDeleter`, with the
//! original's `dull_fx_emissive` shader (`glow.wgsl`).
//!
//! Elements of the distortion renderer (`Dstr`: the heat haze of the turbo, a muzzle
//! flash and the missile's explosion) are drawn by `warp.wgsl`. The tyres' dust and skid
//! smoke are started as the vehicle's update starts them [game]; their marks are
//! `skidmarks.rs`. A weapon object's own clip events (the overheat smoke) and the
//! climbing steps' dust are started here too. CollisionFX uses the original contact
//! history and material presets with assumed arena contact types. Car-gun trails use
//! BulletTrace's visual movement. Regeneration, stun and frost follow the damage
//! receiver's clocks (`notes\damage.md`).

use std::collections::HashMap;

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::light::NotShadowCaster;
use bevy::math::Affine3A;
use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use tf2_core::formats::anim::{Clip, EffectEventKind, TICKS_PER_SECOND};
use tf2_core::formats::hash::crc32;
use tf2_core::particles::{Effect, Element, Rng, Sprite, Template};
use tf2_core::pose::stored_turn;
use tf2_core::fxstate;
use tf2_core::sim::Mode;
use tf2_core::sound::tyre_sounds;

use crate::assets::{Builder, Cache, ObjectNodes, bevy_to_game, game_to_bevy, image, model_space_rotation};
use crate::fx::{Fx, Glow, Warp};
use crate::player::{Player, RobotModel, VehicleModel};
use crate::world::ArenaRes;

/// `ATTACH_TO_HIT_OBJECT`, bit 8 of an impact's `effectOnImpactFlags`: the effect is put
/// in the frame of the object that was hit (`FUN_0077cd60`). [game]
const ATTACH_TO_HIT_OBJECT: u32 = 1 << 8;
/// Bit 0 of the same: the effect's z lies along the surface's normal (`FUN_0077c180`).
const Z_ALONG_NORMAL: u32 = 1;



/// The wheels' nodes, in the order of the vehicle update's loop (its fallback names are
/// `Tread_Front_L`, `Tread_Front_R`, `Tread_Back_L`, `Tread_Back_R`). [game: the order]
const TYRES: [u32; 4] = [crc32(b"Tire_Front_L"), crc32(b"Tire_Front_R"), crc32(b"Tire_Rear_L"), crc32(b"Tire_Rear_R")];

/// What a running effect's place follows.
enum Anchor {
    Fixed,
    /// A target, by its id, and where on it. [stand-in: the dummies do not turn, so only
    /// their place is followed]
    Target { id: u32, offset: Vec3 },
    /// A node of his, of the robot or of the car, by its number: its place, and its turn
    /// too when the effect takes it.
    Node { car: bool, node: usize, turn: bool },
    /// A missile in flight, by its number (`Arsenal::missile`); moved in `start`.
    Missile(u32),
    Bullet(tf2_core::weapons::BulletVisual),
    Collision {key:u32,offset:Vec3},
    /// A node of another object of his (a weapon's), by its entity.
    Thing { entity: Entity, turn: bool },
}

struct Running {
    template: Template,
    effect: Effect,
    anchor: Anchor,
    /// The emitter's frame, in the game's space.
    place: Affine3A,
    /// The `eventId` of the clip event that started it, 0 for none: a stop event's handle.
    id: u32,
    /// The trigger's name, for an effect that is kept going by a state (the exhaust).
    script: u32,
    /// Ends with its clip (autoStop, or a weapon event paired with an explicit stop).
    with_clip: bool,
    /// The object whose clip owns the event id; None is the character's object.
    owner: Option<Entity>,
    /// A tyre's effect, kept going by the vehicle's update: the wheel, whether it is the
    /// skid's (else the rolling one), and the surface it was started for.
    tyre: Option<(usize, bool, u32)>,
    collision: Option<(usize,u32)>,
    /// Started on the car's object by a message (else the robot's).
    on_car: bool,
    /// Removed at once, particles and all.
    gone: bool,
    /// The quads of each element, once it has shown something.
    parts: Vec<Option<(Entity, Handle<Mesh>)>>,
}

/// An `EffectSpawner`'s object while it lives: the punches' shock ring, a mesh its own
/// clip scales up, removed by its `TimeoutDeleter`.
struct Shell {
    root: Entity,
    /// Whose object the effect is on, and which of its effects it is.
    car: bool,
    def: usize,
    age: f32,
    /// Its parts were told not to cast shadows.
    settled: bool,
}

/// A clip event waiting for the next `start`.
enum Pending {
    Spawn { node: u32, id: u32, template: Template, track: bool, turn: bool, with_clip: bool },
    /// The clip was left: the effects it started with `autoStop` end.
    ClipLeft,
    ThingClipLeft { owner: Entity },
    Stop { id: u32 },
    StopOn { owner: Entity, id: u32 },
    Step { side: u32 },
    /// A hand or a foot on the wall, or a slide down it (`anim::CLIMB_SIDES`).
    ClimbStep { limb: usize },
    StepContact { limb: Option<usize>, hit: tf2_core::vehicle::GroundHit },
    /// A spawn event of a clip of another object of his (a weapon's own clip), on that
    /// object's node.
    SpawnOn { owner: Entity, entity: Entity, id: u32, template: Template, track: bool, turn: bool, with_clip: bool },
}

/// What a weapon asks for as it fires (`weapons::Arsenal::fx`).
#[derive(Clone, Copy, Debug)]
pub enum WeaponFxKind {
    Muzzle,
    /// A round landed on a surface of this material.
    Impact(u32),
    /// A missile left: its trail, kept under the missile's number.
    Trail(u32),
    BulletTrail {end:Vec3,speed:f32},
    Blast,
}

#[derive(Clone, Copy, Debug)]
pub struct WeaponFx {
    pub kind: WeaponFxKind,
    /// The weapon's name.
    pub weapon: u32,
    pub at: Vec3,
    /// The way the round flies.
    pub along: Vec3,
}

#[derive(Resource)]
pub struct Effects {
    running: Vec<Running>,
    shells: Vec<Shell>,
    pending: Vec<Pending>,
    cache: Cache,
    /// One generator for every effect, as in the original. [assumed: the seed]
    rng: Rng,
    materials: HashMap<(u32, u32, bool, [u8; 7], [u32; 2]), Handle<Fx>>,
    warps: HashMap<u32, Handle<Warp>>,
    /// The level's switch for headlights (`FUN_00815790`). [stand-in: the arena has no
    /// level settings; on, so that they can be seen]
    pub headlights: bool,
    collision_history: tf2_core::collisionfx::History,
    collision_pending: std::collections::BTreeMap<u32,tf2_core::vehicle::Contact>,
    collision_clock: f32,
    /// Capture diagnostics: visual objects created and nonempty bullet sprites submitted.
    pub bullet_visuals_started: u64,
    pub bullet_sprites_submitted: u64,
    pub collision_visuals_started: u64,
}

impl Default for Effects {
    fn default() -> Self {
        Self {
            running: Vec::new(),
            shells: Vec::new(),
            pending: Vec::new(),
            cache: Cache::default(),
            rng: Rng(0x2009),
            materials: HashMap::new(),
            warps: HashMap::new(),
            headlights: true,
            collision_history: Default::default(),collision_pending:Default::default(),collision_clock:0.0,
            bullet_visuals_started:0,bullet_sprites_submitted:0,collision_visuals_started:0,
        }
    }
}

impl Effects {
    /// Capture diagnostics for the installed gun's smoke, including particles draining
    /// after a stop. Count simulation state so hidden meshes cannot mask a stuck source.
    pub fn gun_smoke_stats(&self) -> (usize, usize) {
        let mut emitters = 0;
        let mut particles = 0;
        for r in self.running.iter().filter(|r| r.template.name == crc32(b"w_overheat_smoke_small")) {
            emitters += usize::from(!r.effect.stopping());
            particles += (0..r.template.elements.len()).filter_map(|i| r.effect.group(i)).map(|g| g.particles.len()).sum::<usize>();
        }
        (emitters, particles)
    }

    /// [assumed] Use the final animated pose for synchronous contact probes, as the
    /// audio adapter does; original asynchronous callback timing remains open.
    pub fn steps(&mut self, pose: &[Affine3A], data: &tf2_core::character::CharacterData, forward: Vec3, world: &impl tf2_core::sim::World) {
        let pending = std::mem::take(&mut self.pending);
        for cue in pending {
            let (limb, hits) = match cue {
                Pending::Step { side } => (None, tf2_core::stepfx::footsteps(side, pose, &data.robot.footsteps, world)),
                Pending::ClimbStep { limb } => (Some(limb), tf2_core::stepfx::climbing(limb, pose, &data.robot.climb_steps, forward, world)),
                other => { self.pending.push(other); continue }
            };
            self.pending.extend(hits.into_iter().map(|hit| Pending::StepContact { limb, hit }));
        }
    }

    fn start(&mut self, template: &Template, anchor: Anchor, place: Affine3A, id: u32, script: u32) {
        if template.elements.is_empty() {
            return;
        }
        if matches!(&anchor,Anchor::Bullet(_)) {self.bullet_visuals_started+=1;}
        self.running.push(Running { template: template.clone(), effect: Effect::new(template), anchor, place, id, script, with_clip: false, owner: None, tyre: None, collision:None, on_car: false, gone: false, parts: vec![None; template.elements.len()] });
    }

    fn stop_event(&mut self, id: u32, owner: Option<Entity>) {
        self.running.iter_mut().filter(|r| r.id == id && r.owner == owner).for_each(|r| r.effect.stop());
    }

    fn stop_clip_effects(&mut self, owner: Option<Entity>) {
        for r in self.running.iter_mut().filter(|r| r.with_clip && r.owner == owner) {
            r.with_clip = false;
            r.effect.stop();
        }
    }

    fn start_on(&mut self, template: &Template, anchor: Anchor, place: Affine3A, owner: Entity, id: u32, with_clip: bool) {
        if id != 0 {
            self.stop_event(id, Some(owner));
        }
        if template.elements.is_empty() { return }
        self.start(template, anchor, place, id, 0);
        let started = self.running.last_mut().unwrap();
        started.owner = Some(owner);
        started.with_clip = with_clip;
    }

    /// His clip changed: the effects the old one started with `autoStop` end. The clip's
    /// reference lists their ids for this (`FUN_0070ccd0`, `+0x50`). [game: the list]
    /// [assumed: that it is walked when the clip is left; that code is not read]
    pub fn clip_changed(&mut self) {
        self.pending.push(Pending::ClipLeft);
    }

    /// The object bundle walks its autoStop list on replacement. [game: 007c3870]
    /// [assumed] Also cancel weapon effects with a paired explicit stop when interrupted;
    /// otherwise the skipped stop leaves an unbounded emitter. See notes/particles.md.
    pub fn thing_clip_changed(&mut self, nodes: &ObjectNodes) {
        if let Some(&owner) = nodes.entities.first() {
            self.pending.push(Pending::ThingClipLeft { owner });
        }
    }

    /// The same for a clip of another object of his (a weapon's own clip: the overheat
    /// smoke is a spawn event of the gun's overheat clip), on that object's nodes. [data]
    /// [assumed: the object's clips run their effect events as his own do]
    pub fn thing_clip_events(&mut self, clip: &Clip, from: f32, to: f32, nodes: &ObjectNodes) {
        let Some(&owner) = nodes.entities.first() else { return };
        for event in &clip.effect_events {
            let t = event.time;
            let due = if to >= from { (t > from && t <= to) || (from == 0.0 && t == 0.0 && to > 0.0) } else { t > from || t <= to };
            if !due {
                continue;
            }
            match &event.kind {
                EffectEventKind::Spawn { id, template, track, use_rotation, auto_stop, .. } => {
                    let Some(entity) = nodes.name_hashes.iter().position(|&name| name == event.node).map(|node| nodes.entities[node]) else { continue };
                    // [assumed] A paired stop bounds this weapon effect even if the clip
                    // is interrupted before that key. The installed smoke has autoStop=0.
                    let paired_stop = clip.effect_events.iter().any(|e| e.time > event.time && matches!(e.kind, EffectEventKind::Stop { id: stop } if stop == *id));
                    self.pending.push(Pending::SpawnOn { owner, entity, id: *id, template: template.clone(), track: *track || *id != 0, turn: *use_rotation, with_clip: (*auto_stop || paired_stop) && *id != 0 });
                }
                EffectEventKind::Stop { id } => self.pending.push(Pending::StopOn { owner, id: *id }),
                EffectEventKind::FootStep { .. } | EffectEventKind::ClimbStep { .. } => {}
            }
        }
    }

    /// The effect events of his clip between two positions in ticks, kept for the next
    /// `start`. `to` below `from` means the clip went round; a clip that has just started
    /// has `from` at 0 and its events at tick 0 count.
    /// `rerun`: the clip had gone round before this stretch; an event that does not allow
    /// a rerun then stays quiet (`FUN_0053ab90`). [game]
    pub fn clip_events(&mut self, clip: &Clip, from: f32, to: f32, rerun: bool) {
        for event in &clip.effect_events {
            let t = event.time;
            let (due, again) = if to >= from {
                ((t > from && t <= to) || (from == 0.0 && t == 0.0 && to > 0.0), rerun)
            } else if t > from {
                (true, rerun)
            } else {
                (t <= to, true)
            };
            if !due || (again && !event.rerun) {
                continue;
            }
            self.pending.push(match &event.kind {
                EffectEventKind::Spawn { id, template, track, use_rotation, auto_stop, .. } => Pending::Spawn {
                    node: event.node,
                    id: *id,
                    template: template.clone(),
                    track: *track || *id != 0,
                    turn: *use_rotation,
                    with_clip: *auto_stop && *id != 0,
                },
                EffectEventKind::Stop { id } => Pending::Stop { id: *id },
                EffectEventKind::FootStep { side } => Pending::Step { side: *side },
                EffectEventKind::ClimbStep { limb } => Pending::ClimbStep { limb: *limb },
            });
        }
    }
}

/// A node's frame in the game's space, from where it was drawn last: its place alone, or
/// with its turn (no scale: no effect of his asks for it).
/// [stand-in: the frame is the one drawn last frame]
fn node_frame(nodes: &ObjectNodes, node: usize, turn: bool, placed: &Query<&GlobalTransform>) -> Option<Affine3A> {
    entity_frame(*nodes.entities.get(node)?, turn, placed)
}

fn entity_frame(entity: Entity, turn: bool, placed: &Query<&GlobalTransform>) -> Option<Affine3A> {
    let at = placed.get(entity).ok()?;
    let (_, rotation, translation) = at.to_scale_rotation_translation();
    let place = bevy_to_game(translation);
    Some(if turn {
        Affine3A::from_rotation_translation(model_space_rotation().inverse() * rotation, place)
    } else {
        Affine3A::from_translation(place)
    })
}

/// What of a renderer's lighting tells two materials apart (`Fx::new`).
fn lit_key(renderer: &tf2_core::particles::Renderer) -> [u8; 7] {
    if !renderer.lit() {
        return [0; 7];
    }
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0) as u8;
    let (a, d) = (renderer.ambient, renderer.diffuse);
    [byte(a[0]), byte(a[1]), byte(a[2]), byte(d[0]), byte(d[1]), byte(d[2]), byte(renderer.back_lit)]
}

/// A frame at `at` whose x lies along `along` and whose z is as near up as that leaves.
fn frame_along_x(at: Vec3, along: Vec3) -> Affine3A {
    let x = along.try_normalize().unwrap_or(Vec3::X);
    let y = Vec3::Z.cross(x).try_normalize().unwrap_or(Vec3::Y);
    Affine3A::from_mat3_translation(Mat3::from_cols(x, y, x.cross(y)), at)
}

/// Starts the effects the last step of the movement, his clips and his weapons asked for.
#[allow(clippy::too_many_arguments)]
pub fn start(
    timing: (Res<Time>,Res<crate::player::TuningRes>),
    data: Res<crate::GameData>,
    arena: Res<ArenaRes>,
    player: Single<&Player>,
    robot: Single<&ObjectNodes, (With<RobotModel>, Without<VehicleModel>)>,
    car: Option<Single<&ObjectNodes, (With<VehicleModel>, Without<RobotModel>)>>,
    placed: Query<&GlobalTransform>,
    arsenal: Option<ResMut<crate::weapons::Arsenal>>,
    mut effects: ResMut<Effects>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut surfaces: ResMut<Assets<crate::surface::Surface>>,
    mut glows: ResMut<Assets<Glow>>,
    mut images: ResMut<Assets<Image>>,
    mut bindposes: ResMut<Assets<SkinnedMeshInverseBindposes>>,
) {
    let (time,tuning)=timing;
    let state = &player.state;
    // A hit of his own body: the impact preset's effect for the surface. A blast's hits
    // do not come this way (`notes\melee.md`). [game]
    for hit in state.hits.iter().filter(|hit| !hit.blast) {
        let Some(info) = data.0.melee_impact.pick(hit.surface) else { continue };
        let Some(template) = &info.template else { continue };
        let target = arena.0.targets.iter().find(|t| t.id == hit.target).filter(|_| info.effect_flags & ATTACH_TO_HIT_OBJECT != 0);
        let anchor = target.map_or(Anchor::Fixed, |t| Anchor::Target { id: t.id, offset: hit.point - t.pos });
        effects.start(template, anchor, Affine3A::from_translation(hit.point), 0, 0);
    }
    // The messages of this update: a clip's triggers, and the car's own.
    let mut triggers = state.triggers.clone();
    let driving = state.mode == Mode::Drive;
    let in_car = state.is_vehicle();
    // CollisionTrigger's game-update history and CollisionFX's keyed loop lifecycle.
    // [assumed] SAT contacts provide initial push3 / ongoing slide5; dispatcher unread.
    for &contact in &state.vehicle_contacts {effects.collision_pending.insert(contact.key,contact);}
    effects.collision_clock+=time.delta_secs();
    let mut collision_events=Vec::new();
    if !driving {
        collision_events=effects.collision_history.clear();
        effects.collision_pending.clear();effects.collision_clock=0.0;
    }
    while driving && effects.collision_clock>=tf2_core::sim::GAME_UPDATE {
        effects.collision_clock-=tf2_core::sim::GAME_UPDATE;
        for (_,contact) in std::mem::take(&mut effects.collision_pending) {
            if data.0.vehicle.as_ref().is_some_and(|object|object.collision_fx.iter().any(|d|d.accepts(&contact))) {
                let kind=if effects.collision_history.contains(contact.key) {5} else {3};
                effects.collision_history.touch(contact,kind);
            }
        }
        collision_events.extend(effects.collision_history.step(tf2_core::sim::GAME_UPDATE));
        let effects=&mut *effects;
        for running in &mut effects.running {
            if let Anchor::Collision {key,ref mut offset}=running.anchor {
                if let Some(point)=effects.collision_history.point(key) {
                    let delta=point-state.pos-*offset;
                    *offset+=delta.clamp_length_max(0.01); // 007d94d0, body-relative contact smoothing
                    running.place.translation=(state.pos+*offset).into();
                }
            }
        }
    }
    for event in collision_events {
        for running in effects.running.iter_mut().filter(|r|r.collision.is_some_and(|(_,key)|key==event.key)) {
            running.effect.stop();running.anchor=Anchor::Fixed;
        }
        let Some(kind)=event.kind.index() else {continue;};
        let hard=usize::from(tuning.0.surface_hardness.get(&event.surface).copied().unwrap_or(false));
        for (index,def) in data.0.vehicle.iter().flat_map(|o|o.collision_fx.iter()).enumerate() {
            if !def.accepts_speed(event.speed) {continue;}
            let Some(template)=&def.particles[hard][kind] else {continue;};
            effects.collision_visuals_started+=1;
            let turn=if def.align {tf2_core::collisionfx::normal_rotation(event.normal)} else {Quat::IDENTITY};
            let anchor=if kind==0 {Anchor::Fixed} else {Anchor::Collision {key:event.key,offset:event.point-state.pos}};
            effects.start(template,anchor,Affine3A::from_rotation_translation(turn,event.point),0,0);
            if kind!=0 && let Some(r)=effects.running.last_mut() {r.collision=Some((index,event.key));}
        }
    }
    // The messages a state keeps on (`fxstate::kept`: the exhaust and the headlights in
    // the car, the turbo's fire, the damage smoke, the health coming back): one that went
    // on is triggered, one that went off is told to stop. [game: who sends each]
    // [assumed: a message reaches the object of the form he is in; in the game it goes to
    // his object and to the one at `+600`, so on a change of form the effect is stopped
    // on the old object and started on the new]
    let kept = fxstate::kept(state, &tuning.0, effects.headlights);
    for r in effects.running.iter_mut().filter(|r| fxstate::KEPT.contains(&r.script) && !r.effect.stopping()) {
        if !kept.contains(&r.script) || r.on_car != in_car {
            r.effect.stop();
            // A headlight is a spawned object, and its stop deletes the object
            // (`FUN_008158a0`): the glow goes at once, it does not live out. [game]
            r.gone = [fxstate::HEADLIGHT_L, fxstate::HEADLIGHT_R].contains(&r.script);
        }
    }
    for name in kept {
        if !effects.running.iter().any(|r| r.script == name && !r.effect.stopping()) {
            triggers.push(name);
        }
    }
    // Every effect on his object that goes by a message's name, at its node: the car's
    // object while he is the car, the robot's otherwise.
    let (object, nodes) = match (in_car, data.0.vehicle.as_ref(), car.as_deref()) {
        (true, Some(vehicle), Some(nodes)) => (vehicle, *nodes),
        _ => (&data.0.robot, *robot),
    };
    for name in &triggers {
        for (index, def) in object.effects.iter().enumerate().filter(|(_, def)| def.script_name == *name) {
            let Some(place) = node_frame(nodes, def.node, !def.upright, &placed) else { continue };
            // The spawner's own object, if it has anything to show: put there upright.
            // [stand-in: it stays there; none of his is tied to its node]
            if let Some(proxy) = def.proxy.as_deref().filter(|proxy| !proxy.renders.is_empty()) {
                let effects = &mut *effects;
                let mut builder = Builder {
                    library: &data.0.library,
                    meshes: &mut meshes,
                    materials: &mut materials,
                    surfaces: &mut surfaces,
                    glows: &mut glows,
                    images: &mut images,
                    bindposes: &mut bindposes,
                    cache: &mut effects.cache,
                };
                let (root, _) = builder.spawn(&mut commands, proxy, "effect");
                let at = game_to_bevy(Vec3::from(place.translation));
                commands.entity(root).insert(Transform::from_translation(at).with_rotation(model_space_rotation()));
                effects.shells.push(Shell { root, car: in_car, def: index, age: 0.0, settled: false });
            }
            let anchor = if def.attach { Anchor::Node { car: in_car, node: def.node, turn: !def.upright } } else { Anchor::Fixed };
            effects.start(&def.template, anchor, place, 0, *name);
            if let Some(started) = effects.running.last_mut().filter(|r| r.script == *name) {
                started.on_car = in_car;
            }
        }
    }
    // The tyres (`FUN_007347c0`, `notes\sound.md` "The tyres and the suspension"): at each
    // wheel, while he rolls faster than 1 m/s on a surface, `vehicleTireRoadFX`'s effect
    // for that surface, and while the tyres skid `vehicleTireSkidFX`'s with it. Each is
    // kept by the wheel, started again when the surface changes and stopped when its
    // condition ends. [game] The presets' flags tie the effect to the car (bit 8). [data]
    // [assumed: tied means it follows the wheel's node, upright]
    let input = if driving { state.tyre_inputs.last() } else { None };
    if let Some(nodes) = car.as_deref() {
        for (wheel, name) in TYRES.into_iter().enumerate() {
            let heard=input.map(|input|tyre_sounds(&input.for_wheel(wheel))).unwrap_or_default();
            let Some(node) = nodes.name_hashes.iter().position(|&n| n == name) else { continue };
            for (skid, preset, surface) in [(true, &data.0.tyre_skid, heard.skid), (false, &data.0.tyre_road, heard.road)] {
                let going = effects.running.iter().position(|r| matches!(r.tyre, Some((w, s, _)) if w == wheel && s == skid) && !r.effect.stopping());
                if going.and_then(|i| effects.running[i].tyre).map(|tyre| tyre.2) == surface {
                    continue;
                }
                if let Some(i) = going {
                    effects.running[i].effect.stop();
                }
                let Some(surface) = surface else { continue };
                let Some(template) = preset.pick(surface).and_then(|info| info.template.as_ref()).filter(|t| !t.elements.is_empty()) else { continue };
                let Some(place) = node_frame(nodes, node, false, &placed) else { continue };
                effects.start(template, Anchor::Node { car: true, node, turn: false }, place, 0, 0);
                if let Some(started) = effects.running.last_mut() {
                    started.tyre = Some((wheel, skid, surface));
                }
            }
        }
    }
    // His clips' events, on the robot's nodes.
    for event in std::mem::take(&mut effects.pending) {
        let find = |hash: u32| robot.name_hashes.iter().position(|&name| name == hash);
        match event {
            Pending::ClipLeft => effects.stop_clip_effects(None),
            Pending::ThingClipLeft { owner } => effects.stop_clip_effects(Some(owner)),
            Pending::Spawn { node, id, template, track, turn, with_clip } => {
                let Some(node) = find(node) else { continue };
                let Some(place) = node_frame(&robot, node, turn, &placed) else { continue };
                // The object keeps one effect under each id: a new one takes the place of
                // one still going, which is stopped (`FUN_00747ab0`). [game]
                if id != 0 {
                    effects.stop_event(id, None);
                }
                let anchor = if track { Anchor::Node { car: false, node, turn } } else { Anchor::Fixed };
                debug!("clip effect {:08x} at node {node} ({:?})", template.name, place.translation);
                effects.start(&template, anchor, place, id, 0);
                if let Some(started) = effects.running.last_mut().filter(|r| r.id == id) {
                    started.with_clip = with_clip;
                }
            }
            // [assumed: a stopped effect makes no more and what it has made lives out]
            Pending::Stop { id } => effects.stop_event(id, None),
            Pending::StopOn { owner, id } => effects.stop_event(id, Some(owner)),
            Pending::SpawnOn { owner, entity, id, template, track, turn, with_clip } => {
                let Some(place) = entity_frame(entity, turn, &placed) else { continue };
                let anchor = if track { Anchor::Thing { entity, turn } } else { Anchor::Fixed };
                effects.start_on(&template, anchor, place, owner, id, with_clip);
            }
            // The character's handler (`FUN_00718720`) starts the limb's climb preset's
            // effect for the surface where the limb's own ray found the wall
            // (`FUN_0073fa20`, through `FUN_0077cd60` as a melee hit does). [game]
            Pending::StepContact { limb, hit } => {
                let preset = match limb { Some(limb) => data.0.climb_impact.get(limb), None => Some(&data.0.step_impact) };
                let Some(info) = preset.and_then(|preset| preset.pick(hit.surface)) else { continue };
                let Some(template) = &info.template else { continue };
                // [data] Step flags0x100 attach to the struck object; his climb flags1
                // put z along normal. Arena surfaces are static, so fixed world placement
                // retains the step effect's attachment position (moving surfaces absent).
                let turn = tf2_core::stepfx::rotation(info.effect_flags, hit.normal);
                effects.start(template, Anchor::Fixed, Affine3A::from_rotation_translation(turn, hit.point), 0, 0);
            }
            // Resolved from the final pose by animation::animate before start runs.
            Pending::Step { .. } | Pending::ClimbStep { .. } => {}
        }
    }
    // What his weapons asked for.
    let mut asked = Vec::new();
    if let Some(mut arsenal) = arsenal {
        // A trail goes with its missile and stops making more where the missile ends.
        // [assumed: the emitter's x is the way the missile flies, as for the flash]
        for r in effects.running.iter_mut() {
            let Anchor::Missile(number) = r.anchor else { continue };
            match arsenal.missile(number) {
                Some((at, along)) => r.place = frame_along_x(at, along),
                None => {
                    r.effect.stop();
                    r.anchor = Anchor::Fixed;
                }
            }
        }
        asked = std::mem::take(&mut arsenal.fx);
    }
    for fx in asked {
        let Some(weapon) = data.0.weapon_effects.iter().find(|w| w.name == fx.weapon) else { continue };
        match fx.kind {
            WeaponFxKind::Muzzle => {
                if let Some(template) = &weapon.muzzle_flash {
                    effects.start(template, Anchor::Fixed, frame_along_x(fx.at, fx.along), 0, 0);
                }
            }
            WeaponFxKind::Impact(surface) => {
                let Some(info) = weapon.impact.pick(surface) else { continue };
                let Some(template) = &info.template else { continue };
                // [stand-in: the surface's normal is taken to face the round]
                let normal = (-fx.along).try_normalize().unwrap_or(Vec3::Z);
                let turn = if info.effect_flags & Z_ALONG_NORMAL != 0 { Quat::from_rotation_arc(Vec3::Z, normal) } else { Quat::IDENTITY };
                effects.start(template, Anchor::Fixed, Affine3A::from_rotation_translation(turn, fx.at), 0, 0);
            }
            // The projectile object's own emitter. [data; assumed: it runs from the
            // launch to the missile's end]
            WeaponFxKind::Trail(number) => {
                if let Some(template) = &weapon.trail {
                    effects.start(template, Anchor::Missile(number), frame_along_x(fx.at, fx.along), 0, 0);
                }
            }
            WeaponFxKind::BulletTrail {end,speed} => {
                if let Some(template)=&weapon.trail {
                    // 007832b0 uses the projectile object's +y row for velocity. [game]
                    let direction=(end-fx.at).normalize_or_zero();
                    let turn=Quat::from_rotation_arc(Vec3::Y,direction);
                    let bullet=tf2_core::weapons::BulletVisual::new(fx.at,end,speed);
                    effects.start(template,Anchor::Bullet(bullet),Affine3A::from_rotation_translation(turn,fx.at),0,0);
                }
            }
            WeaponFxKind::Blast => {
                if let Some(template) = &weapon.blast {
                    effects.start(template, Anchor::Fixed, Affine3A::from_translation(fx.at), 0, 0);
                }
            }
        }
    }
}

/// Runs the effects and rebuilds their quads to face the camera.
#[allow(clippy::too_many_arguments)]
pub fn run(
    time: Res<Time>,
    data: Res<crate::GameData>,
    arena: Res<ArenaRes>,
    robot: Single<&ObjectNodes, (With<RobotModel>, Without<VehicleModel>)>,
    car: Option<Single<&ObjectNodes, (With<VehicleModel>, Without<RobotModel>)>>,
    placed: Query<&GlobalTransform>,
    camera: Single<&Transform, With<Camera3d>>,
    shell_nodes: Query<&ObjectNodes, (Without<RobotModel>, Without<VehicleModel>)>,
    children: Query<&Children>,
    part_materials: Query<(Option<&MeshMaterial3d<StandardMaterial>>, Option<&MeshMaterial3d<Glow>>)>,
    mut transforms: Query<&mut Transform, Without<Camera3d>>,
    mut effects: ResMut<Effects>,
    mut commands: Commands,
    (mut meshes, mut materials, mut glows, mut fx_materials, mut warp_materials, mut images): (
        ResMut<Assets<Mesh>>,
        ResMut<Assets<StandardMaterial>>,
        ResMut<Assets<Glow>>,
        ResMut<Assets<Fx>>,
        ResMut<Assets<Warp>>,
        ResMut<Assets<Image>>,
    ),
) {
    let dt = time.delta_secs().min(0.05);
    let Effects { running, shells, rng, materials: made, warps, bullet_sprites_submitted, .. } = &mut *effects;
    shells.retain_mut(|shell| {
        let object = if shell.car { data.0.vehicle.as_ref().unwrap_or(&data.0.robot) } else { &data.0.robot };
        let Some(def) = object.effects.get(shell.def) else { return false };
        let (life, fade_start) = def.timeout.unwrap_or((1.0, 1.0));
        shell.age += dt;
        if shell.age >= life {
            commands.entity(shell.root).despawn();
            return false;
        }
        // Its clip moves its nodes, each by its name, from the start once.
        if let (Ok(nodes), Some(clip)) = (shell_nodes.get(shell.root), &def.clip) {
            let tick = (shell.age * TICKS_PER_SECOND).min(clip.length);
            for (i, &hash) in nodes.name_hashes.iter().enumerate() {
                let (Some(channel), Ok(mut transform)) = (clip.channel(hash), transforms.get_mut(nodes.entities[i])) else { continue };
                *transform = nodes.rest[i];
                if let Some(p) = channel.position.sample(tick, clip.length, false) {
                    transform.translation = Vec3::from(p);
                }
                if let Some(q) = channel.rotation.sample(tick, clip.length, false) {
                    transform.rotation = stored_turn(q);
                }
                if let Some(s) = channel.scale.sample(tick, clip.length, false) {
                    transform.scale = Vec3::from(s);
                }
            }
        }
        // The deleter fades it out over the end of its life. [assumed: evenly, by the
        // object's own colour's alpha, which the shader multiplies into the material's;
        // `TimeoutDeleter`'s code is not read]
        let fade = if life > fade_start { ((life - shell.age) / (life - fade_start)).clamp(0.0, 1.0) } else { 1.0 };
        for part in children.iter_descendants(shell.root) {
            let Ok((standard, glow)) = part_materials.get(part) else { continue };
            if !shell.settled && (standard.is_some() || glow.is_some()) {
                commands.entity(part).insert(NotShadowCaster);
            }
            if let Some(mut material) = standard.and_then(|m| materials.get_mut(&m.0)) {
                material.base_color.set_alpha(fade);
            }
            if let Some(mut material) = glow.and_then(|m| glows.get_mut(&m.0)) {
                material.set_object_alpha(fade);
            }
        }
        shell.settled = true;
        true
    });
    for r in running.iter_mut() {
        match r.anchor {
            Anchor::Fixed | Anchor::Missile(_) | Anchor::Collision {..} => {}
            Anchor::Bullet(ref mut bullet) => {
                if bullet.step(dt) {r.place.translation=bullet.position.into();}
                else {r.gone=true;} // installed emitter instantKill=true; no residual local particles
            }
            Anchor::Thing { entity, turn } => {
                if let Some(place) = entity_frame(entity, turn, &placed) {
                    r.place = place;
                }
            }
            Anchor::Target { id, offset } => {
                if let Some(target) = arena.0.targets.iter().find(|t| t.id == id) {
                    r.place.translation = (target.pos + offset).into();
                }
            }
            Anchor::Node { car: of_car, node, turn } => {
                let nodes = if of_car { car.as_deref().copied() } else { Some(*robot) };
                if let Some(place) = nodes.and_then(|nodes| node_frame(nodes, node, turn, &placed)) {
                    r.place = place;
                }
            }
        }
        if r.gone {continue;}
        r.effect.step(&r.template, &r.place, dt, rng);
        for (index, element) in r.template.elements.iter().enumerate() {
            let sprites = r.effect.sprites(&r.template, index, &r.place);
            if matches!(&r.anchor,Anchor::Bullet(_)) {*bullet_sprites_submitted+=sprites.len() as u64;}
            let mesh = quads(&sprites, element, &camera);
            match &r.parts[index] {
                // An element with nothing to show keeps its last quads, hidden: a mesh
                // with no vertices upsets the renderer.
                Some((entity, _)) if sprites.is_empty() => {
                    commands.entity(*entity).insert(Visibility::Hidden);
                }
                Some((entity, handle)) => {
                    let _ = meshes.insert(handle, mesh);
                    commands.entity(*entity).insert(Visibility::Inherited);
                }
                // The distortion renderer bends the picture behind it (`warp.wgsl`).
                None if !sprites.is_empty() && element.renderer.distortion => {
                    let texture = element.renderer.texture;
                    let material = warps
                        .entry(texture)
                        .or_insert_with(|| {
                            let map = data.0.library.textures.get(&texture).and_then(|t| image(t, false)).map(|i| images.add(i));
                            warp_materials.add(Warp { map })
                        })
                        .clone();
                    let handle = meshes.add(mesh);
                    let entity = commands
                        .spawn((Mesh3d(handle.clone()), MeshMaterial3d(material), Transform::IDENTITY, NoFrustumCulling, NotShadowCaster))
                        .id();
                    r.parts[index] = Some((entity, handle));
                }
                None if !sprites.is_empty() => {
                    let renderer = &element.renderer;
                    let material = made
                        .entry((renderer.texture, renderer.blend_mode, renderer.z_feather > 0.0, lit_key(renderer),
                            [renderer.alpha_fade.0.to_bits(), renderer.alpha_fade.1.to_bits()]))
                        .or_insert_with(|| {
                            // The shader works on the stored values: no decoding.
                            let texture = data.0.library.textures.get(&renderer.texture).and_then(|t| image(t, false)).map(|i| images.add(i));
                            fx_materials.add(Fx::new(renderer, texture))
                        })
                        .clone();
                    let handle = meshes.add(mesh);
                    let entity = commands
                        .spawn((Mesh3d(handle.clone()), MeshMaterial3d(material), Transform::IDENTITY, NoFrustumCulling, NotShadowCaster))
                        .id();
                    r.parts[index] = Some((entity, handle));
                }
                None => {}
            }
        }
    }
    running.retain(|r| {
        let finished = r.gone || r.effect.finished(&r.template);
        if finished {
            for (entity, _) in r.parts.iter().flatten() {
                commands.entity(*entity).despawn();
            }
        }
        !finished
    });
}

/// The quads of one element's particles, in Bevy's space, as the original's builders lay
/// them (`particles::Sprite` says what of this is read and what assumed). Each vertex
/// carries the two frames' places in the picture, the colour, and in its normal how far
/// the picture is on the way to the next frame (`fx.wgsl`).
fn quads(sprites: &[Sprite], element: &Element, camera: &Transform) -> Mesh {
    let renderer = &element.renderer;
    let (left, up, eye, forward) = (-*camera.right(), *camera.up(), camera.translation, *camera.forward());
    let (across, down) = (renderer.frames.0 as f32, renderer.frames.1 as f32);
    let [reach_left, reach_right, reach_top, reach_bottom] = element.extents();
    let mut positions = Vec::with_capacity(sprites.len() * 4);
    let mut uvs = Vec::with_capacity(sprites.len() * 4);
    let mut next_uvs = Vec::with_capacity(sprites.len() * 4);
    let mut colours = Vec::with_capacity(sprites.len() * 4);
    let mut normals = Vec::with_capacity(sprites.len() * 4);
    // A lit element's corners carry true normals, and the frame's fraction in a tangent.
    let lit = renderer.lit();
    // A distortion element's corners carry the particle's size the same way.
    let warp = renderer.distortion;
    let mut tangents = Vec::new();
    let mut indices = Vec::with_capacity(sprites.len() * 12);
    for sprite in sprites {
        let pos = game_to_bevy(sprite.pos);
        let to_eye = (eye - pos).normalize_or_zero();
        // The picture's top and its left edge.
        let (top, side) = if let Some(along) = sprite.along {
            let along = game_to_bevy(along);
            (along, to_eye.cross(along).normalize_or(left))
        } else {
            let (l, u) = if renderer.rectangle() && renderer.normal != Vec3::ZERO {
                // A rectangle with a facing of its own lies in the plane across it.
                let normal = game_to_bevy(renderer.normal).normalize();
                let u = normal.any_orthonormal_vector();
                (normal.cross(u), u)
            } else {
                (left, up)
            };
            let (sin, cos) = sprite.turn.sin_cos();
            (l * sin + u * cos, l * cos - u * sin)
        };
        let (side, top) = (side * sprite.half.0, top * sprite.half.1);
        let corners = [
            pos + top * reach_top + side * reach_left,
            pos - top * reach_bottom + side * reach_left,
            pos + top * reach_top - side * reach_right,
            pos - top * reach_bottom - side * reach_right,
        ];
        // The vertex shader's fade by the depth in front of the camera (`ALPHAFADE`).
        let mut colour = sprite.colour;
        let (near, far) = renderer.alpha_fade;
        // Fx's vertex shader applies this to each corner, as the original does.
        // Warp still uses the existing CPU fade until its vertex shader is ported.
        if warp && far != near {
            colour[3] *= (((pos - eye).dot(forward) - near) / (far - near)).clamp(0.0, 1.0);
        }
        // The frame's cell in the sheet, and the next frame's (the vertex shader's
        // arithmetic: the row is the frame over the sheet's width, whole).
        let cell = |frame: f32| {
            let row = (frame / across).floor();
            (frame - row * across, row)
        };
        let (this, next) = (cell(sprite.frame.floor()), cell(sprite.frame.ceil()));
        let fraction = sprite.frame.fract();
        let first = positions.len() as u32;
        for (corner, (u, v)) in corners.into_iter().zip([(0.0, 0.0), (0.0, 1.0), (1.0, 0.0), (1.0, 1.0)]) {
            positions.push(corner.to_array());
            uvs.push([(this.0 + u) / across, (this.1 + v) / down]);
            next_uvs.push([(next.0 + u) / across, (next.1 + v) / down]);
            colours.push(colour);
            if warp {
                normals.push([fraction, 1.0 - fraction, 0.0]);
                tangents.push([1.0, 0.0, 0.0, sprite.half.0.max(sprite.half.1)]);
            } else if lit {
                // The lit builder (`FUN_00971170`) gives each corner a normal of its own.
                // [assumed: the way to the camera leant toward the corner, so the quad
                // is lit like a ball; the builder's arithmetic was not followed]
                normals.push((to_eye + (corner - pos).normalize_or_zero()).normalize_or(to_eye).to_array());
                tangents.push([1.0, 0.0, 0.0, fraction]);
            } else {
                normals.push([fraction, 1.0 - fraction, 0.0]);
            }
        }
        // Both ways round: it is seen from either side.
        indices.extend([first, first + 1, first + 2, first + 2, first + 1, first + 3]);
        indices.extend([first, first + 2, first + 1, first + 2, first + 3, first + 1]);
    }
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, next_uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colours);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    if lit || warp {
        mesh.insert_attribute(Mesh::ATTRIBUTE_TANGENT, tangents);
    }
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object_nodes(owner: Entity, node: Entity, hash: u32) -> ObjectNodes {
        ObjectNodes {
            entities: vec![owner, node], names: vec![String::new(); 2], name_hashes: vec![0, hash],
            rest: vec![Transform::IDENTITY; 2], parents: vec![None, Some(0)],
            spine_links: Vec::new(), foot_rig: None,
        }
    }

    #[test]
    fn installed_gun_smoke_drains_when_its_clip_is_interrupted() {
        let data = tf2_core::character::load(std::path::Path::new("C:/Games2"), "bumblebee").unwrap();
        let mut world = World::new();
        let owner = world.spawn_empty().id();
        let node = world.spawn_empty().id();
        let mut checked = 0;
        for reference in &data.weapon_animations[0] {
            let Some(clip) = reference.clip.as_ref() else { continue };
            for event in &clip.effect_events {
                let EffectEventKind::Spawn { template, auto_stop, .. } = &event.kind else { continue };
                if template.name != crc32(b"w_overheat_smoke_small") { continue }
                assert!(!auto_stop, "installed smoke uses explicit stops, not autoStop");
                let nodes = object_nodes(owner, node, event.node);
                let mut effects = Effects::default();
                effects.thing_clip_events(clip, 0.0, event.time + 1.0, &nodes);
                for pending in std::mem::take(&mut effects.pending) {
                    let Pending::SpawnOn { owner, entity, id, template, turn, with_clip, .. } = pending else {
                        panic!("expected smoke spawn before its explicit stop");
                    };
                    assert!(with_clip, "a paired weapon stop must survive interruption");
                    effects.start_on(&template, Anchor::Thing { entity, turn }, Affine3A::IDENTITY, owner, id, with_clip);
                }
                assert_eq!(effects.running.len(), 1);
                let dt = 0.032;
                for _ in 0..32 {
                    let r = &mut effects.running[0];
                    r.effect.step(&r.template, &r.place, dt, &mut effects.rng);
                }
                let r = &effects.running[0];
                assert!(r.effect.group(0).is_some_and(|g| !g.particles.is_empty()));
                assert!(!r.effect.finished(&r.template));
                // Cooling, switching or stowing leaves this clip before its stop event.
                effects.thing_clip_changed(&nodes);
                let Pending::ThingClipLeft { owner } = effects.pending.pop().unwrap() else { panic!("clip exit") };
                effects.stop_clip_effects(Some(owner));
                assert!(effects.running[0].effect.stopping());
                assert!(!effects.running[0].effect.finished(&effects.running[0].template), "existing smoke lives out");
                for _ in 0..160 {
                    let r = &mut effects.running[0];
                    r.effect.step(&r.template, &r.place, dt, &mut effects.rng);
                }
                let r = &effects.running[0];
                assert!(r.effect.finished(&r.template), "smoke must finish without a timeout workaround");

                // An explicit stop needs the object's id, even if its channel is absent.
                let smoke_id = r.id;
                let stop = clip.effect_events.iter().find(|e| matches!(e.kind, EffectEventKind::Stop { id } if id == smoke_id)).unwrap();
                let missing_node = object_nodes(owner, node, event.node.wrapping_add(1));
                effects.thing_clip_events(clip, stop.time - 1.0, stop.time, &missing_node);
                assert!(matches!(effects.pending.as_slice(), [Pending::StopOn { owner: o, id }] if *o == owner && *id == smoke_id));
                effects.pending.clear();
                if reference.looping {
                    effects.thing_clip_events(clip, stop.time - 1.0, 1.0, &nodes);
                    assert!(effects.pending.iter().any(|e| matches!(e, Pending::StopOn { owner: o, id } if *o == owner && *id == smoke_id)), "a wrap must process the previous loop's stop key");
                }
                checked += 1;
            }
        }
        assert_eq!(checked, 4, "all installed gun smoke references are covered");
    }

    #[test]
    fn clip_stops_and_event_ids_are_scoped_to_their_object() {
        let mut world = World::new();
        let first = world.spawn_empty().id();
        let second = world.spawn_empty().id();
        let template = Template { elements: vec![Element { end: f32::MAX, ..Default::default() }], ..Default::default() };
        let mut effects = Effects::default();
        effects.start_on(&template, Anchor::Fixed, Affine3A::IDENTITY, first, 1, true);
        effects.start_on(&template, Anchor::Fixed, Affine3A::IDENTITY, second, 1, true);
        effects.start_on(&template, Anchor::Fixed, Affine3A::IDENTITY, first, 2, false);
        effects.start(&template, Anchor::Fixed, Affine3A::IDENTITY, 1, 0);
        effects.running[3].with_clip = true;
        assert!(effects.running.iter().all(|r| !r.effect.stopping()));
        effects.stop_clip_effects(None);
        assert_eq!(effects.running.iter().map(|r| r.effect.stopping()).collect::<Vec<_>>(), [false, false, false, true]);
        effects.stop_clip_effects(Some(first));
        assert_eq!(effects.running.iter().map(|r| r.effect.stopping()).collect::<Vec<_>>(), [true, false, false, true]);
        effects.stop_event(1, Some(second));
        assert!(effects.running[1].effect.stopping());
        assert!(!effects.running[2].effect.stopping(), "autoStop=false stays alive until its own stop");
    }
}
