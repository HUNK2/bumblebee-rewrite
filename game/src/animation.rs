//! Plays the character's own clips on the robot's and the vehicle's node trees.
//!
//! The clips, their loop flags, blend times, speeds, model events and sound cues are the
//! game's, and so is the choice of which one plays. A control state that plays a clip of its
//! own (the attacks, the dodge, the climb, both halves of the change of form, the weapon
//! set) names it; the rest (standing, walking, running, stopping, jumping, falling,
//! landing) is the base layer's clip, picked by the sets' own entry and exit rules
//! (`tf2_core::animrules::Base`, run by the simulation). Either is drawn at the tick the
//! movement keeps, so what is drawn and what the movement acts on are the same moment.

use bevy::prelude::*;

use crate::assets::ObjectNodes;
use crate::camera::Rig;
use crate::formats::anim::{AnimRef, AnimSet, Clip, TICKS_PER_SECOND};
use crate::formats::hash::crc32;
use crate::player::{Player, RobotModel, TuningRes, VehicleModel};
use tf2_core::pose::{blend_turn, offset_position, offset_scale, offset_turn, stored_turn};
use tf2_core::animblend::{Blend, Clock};
use crate::sim::Mode;
use crate::sound::SoundOut;

const ROOT: u32 = crc32(b"Root");
/// Blend time for clips whose own is negative, which the data uses for "no value given"
/// (`FUN_00748f00` takes 0.1 for any blend time under 0). [game]
const DEFAULT_CROSSFADE: f32 = 0.1;

#[derive(Resource)]
pub struct AnimLibrary {
    pub robot: AnimSet,
    /// The clips the vehicle model carries for itself.
    pub vehicle: Vec<AnimRef>,
}

#[derive(Component)]
pub struct Animator {
    /// A clip with model events (the change of form) says which model shows.
    transforming: bool,
    /// Position in the clip, in ticks, and how many of its model events have been applied.
    time: f32,
    events_done: usize,
    /// The clip has gone round since it started: its events that do not allow a rerun
    /// (`allowReRun` 0) are done with. [game] `FUN_0053ab90`
    went_round: bool,
    /// The still-playing outgoing clips and their nested fades. [game]
    blend: Option<Blend>,
    pose: Vec<Transform>,
    foot_ik: tf2_core::footik::FootIk,
    show_robot: bool,
    show_vehicle: bool,
    /// Hold one clip at one time instead of following the simulation, for captures.
    pub forced: Option<(String, String, f32)>,
    /// The clip the movement says is playing (a control state's, or the base layer's), by set
    /// and id, and the base layer's count of clips started when it was taken up, which tells
    /// one clip started again from one still playing.
    action: Option<(String, String)>,
    serial: Option<u32>,
}

/// The vehicle model's own clip player; the robot's clips tell it what to play.
#[derive(Component, Default)]
pub struct VehicleAnimator {
    playing: Option<String>,
    then: Option<String>,
    time: f32,
}

impl Animator {
    pub fn new(rest: &[Transform]) -> Self {
        Self {
            transforming: false,
            time: 0.0,
            events_done: 0,
            went_round: false,
            blend: None,
            pose: rest.to_vec(),
            foot_ik: tf2_core::footik::FootIk::default(),
            show_robot: true,
            show_vehicle: false,
            forced: None,
            action: None,
            serial: None,
        }
    }

    pub fn describe(&self) -> String {
        match (&self.forced, &self.action) {
            (Some((set, id, time)), _) => format!("{set}/{id} held at tick {time:.0}"),
            (None, Some((set, id))) => format!("{set}/{id} at tick {:.0}", self.time),
            (None, None) => "none".into(),
        }
    }
}

fn find_vehicle<'a>(clips: &'a [AnimRef], id: &str) -> Option<(&'a AnimRef, &'a Clip)> {
    clips.iter().find(|r| r.id == id).and_then(|r| r.clip.as_ref().map(|clip| (r, clip)))
}

fn find_clock(set: &AnimSet, clock: Clock) -> Option<(&AnimRef, &Clip)> {
    let refs = set.sets.iter().find(|(name, _)| crc32(name.as_bytes()) == clock.set)?.1;
    let r = refs.iter().find(|r| crc32(r.id.as_bytes()) == clock.clip)?;
    Some((r, r.clip.as_ref()?))
}

/// Weapon-object fades use the same moving player tree as the character. [game:
/// FUN_007c3870 creates a BlendAnimation with the old clip and the new clip's xfadeTime]
#[derive(Default)]
pub(crate) struct ObjectBlend {
    blend: Option<Blend>,
    last: Option<(u32, f32)>,
}

impl ObjectBlend {
    pub(crate) fn pose(&mut self, refs: &[AnimRef], id: u32, tick: f32, dt: f32, nodes: &ObjectNodes) -> Vec<Transform> {
        let Some(r) = refs.iter().find(|r| crc32(r.id.as_bytes()) == id) else {
            *self = Self::default();
            return nodes.rest.clone();
        };
        let Some(clip) = r.clip.as_ref() else { return nodes.rest.clone() };
        let changed = self.last.is_none_or(|(old, at)| old != id || (!r.looping && tick < at));
        if changed {
            let clock = Clock { set: 0, clip: id, tick, length: clip.length, speed: r.speed, looping: r.looping, exit_events: r.exit_events };
            // Object clips take their own time directly; a negative value swaps at once.
            // [game: FUN_007c3870, unlike the character starter's 0.1s fallback]
            let duration = r.crossfade;
            match self.blend.as_mut() {
                Some(blend) => blend.start(clock, duration),
                None => self.blend = Some(Blend::new(clock)),
            }
        }
        self.last = Some((id, tick));
        let blend = self.blend.as_mut().unwrap();
        blend.update(tick, dt, &mut |_, _, _| {});
        blend.evaluate(&|clock| {
            refs.iter().find(|r| crc32(r.id.as_bytes()) == clock.clip).and_then(|r| {
                r.clip.as_ref().map(|clip| sample(clip, r.looping, clock.tick, &nodes.name_hashes, &nodes.rest))
            }).unwrap_or_else(|| nodes.rest.clone())
        }, &|from, mut pose, weight| {
            cross_fade(&mut pose, &from, weight);
            pose
        })
    }
}

/// The pose a clip gives at `time`, for every node; nodes the clip does not move keep `rest`.
pub(crate) fn sample(clip: &Clip, looping: bool, time: f32, name_hashes: &[u32], rest: &[Transform]) -> Vec<Transform> {
    name_hashes
        .iter()
        .zip(rest)
        .map(|(&name, rest)| {
            let Some(channel) = clip.channel(name) else { return *rest };
            let mut out = *rest;
            // The root's place in a clip is where the clip was made, not where the model is:
            // clips that follow each other carry on from each other's (the car's idle sits 4
            // behind, part B of a change of form starts 3.5 ahead). Its travel is movement,
            // which the simulation does, so the root itself stays put. [data]
            // Its turn likewise: the strafes and side dodges are made running along a root
            // turned a quarter round, and that turn says which way the travel goes. [data]
            if let Some(p) = channel.position.sample(time, clip.length, looping).filter(|_| name != ROOT) {
                out.translation = Vec3::from(p);
            }
            if let Some(q) = channel.rotation.sample(time, clip.length, looping).filter(|_| name != ROOT) {
                // Stored for the game's row-vector matrices: the same turn the other way round here.
                out.rotation = Quat::from_xyzw(-q[0], -q[1], -q[2], q[3]);
            }
            if let Some(s) = channel.scale.sample(time, clip.length, looping) {
                out.scale = Vec3::from(s);
            }
            out
        })
        .collect()
}

/// The pose a clip is fading in over, blended toward the clip's by `weight`: turns go in a
/// straight line the short way and are normalised (`BlendAnimation::BlendOutputs`). [game]
pub(crate) fn cross_fade(pose: &mut [Transform], from: &[Transform], weight: f32) {
    for (to, from) in pose.iter_mut().zip(from) {
        to.translation = from.translation.lerp(to.translation, weight);
        to.rotation = blend_turn(from.rotation, to.rotation, weight);
        to.scale = from.scale.lerp(to.scale, weight);
    }
}

/// A clip of offsets laid over a pose by `weight`, on the nodes it has channels for
/// (`AdditiveAnimation::BlendOutputs`, `FUN_0053e180`). [game]
pub(crate) fn lay_offsets(pose: &mut [Transform], over: &Clip, tick: f32, looping: bool, weight: f32, name_hashes: &[u32]) {
    for (node, &name) in pose.iter_mut().zip(name_hashes) {
        let Some(channel) = over.channel(name).filter(|_| name != ROOT) else { continue };
        if let Some(p) = channel.position.sample(tick, over.length, looping) {
            node.translation = offset_position(node.translation, Vec3::from(p), weight);
        }
        if let Some(q) = channel.rotation.sample(tick, over.length, looping) {
            node.rotation = offset_turn(node.rotation, stored_turn(q), weight);
        }
        if let Some(s) = channel.scale.sample(tick, over.length, looping) {
            node.scale = offset_scale(node.scale, Vec3::from(s), weight);
        }
    }
}

/// The aim laid on a clip's pose: each weapon-set clip carries two offset clips, `Down`
/// and `Up` (spine, arms, neck and head turned 65 degrees in all), and the aim's rise says
/// which is on and how much (`aim::Aim::offset`). The clip's player is wrapped with them
/// when it starts (`MetaAdditiveAnimation`, `FUN_00748f00`), so the offsets are part of
/// the clip's own pose and fade in and out with it. [game] [data: the offset clips hold
/// one pose for their 4 ticks, so the time in them does not matter]
fn lay_aim(pose: &mut [Transform], r: &AnimRef, (side, amount): (usize, f32), name_hashes: &[u32]) {
    if let Some(over) = r.aim_offsets.get(side).filter(|_| amount > 0.0) {
        lay_offsets(pose, over, 0.0, false, amount, name_hashes);
    }
}

fn visibility(shown: bool) -> Visibility {
    if shown { Visibility::Inherited } else { Visibility::Hidden }
}

pub fn animate(
    time: Res<Time>,
    library: Res<AnimLibrary>,
    tuning: Res<TuningRes>,
    arena: Res<crate::world::ArenaRes>,
    data: Res<crate::GameData>,
    sound: Option<Res<SoundOut>>,
    arsenal: Option<Res<crate::weapons::Arsenal>>,
    mut rig: ResMut<Rig>,
    mut effects: ResMut<crate::effects::Effects>,
    mut trails: ResMut<crate::trails::Trails>,
    player: Single<&Player>,
    mut robot: Single<(&mut Animator, &ObjectNodes, &mut Visibility), (With<RobotModel>, Without<VehicleModel>)>,
    mut vehicle: Single<(&mut VehicleAnimator, &ObjectNodes, &mut Visibility), (With<VehicleModel>, Without<RobotModel>)>,
    mut transforms: Query<&mut Transform, (Without<RobotModel>, Without<VehicleModel>)>,
) {
    let (animator, nodes, robot_visibility) = &mut *robot;
    let (driver, vehicle_nodes, vehicle_visibility) = &mut *vehicle;
    let state = &player.state;
    let dt = time.delta_secs();
    let set = &library.robot;
    let sound = sound.as_deref();

    let pose = if let Some((set_name, id, at)) = &animator.forced {
        match set.find(set_name, id).and_then(|r| r.clip.as_ref()) {
            Some(clip) => sample(clip, false, *at, &nodes.name_hashes, &nodes.rest),
            None => nodes.rest.clone(),
        }
    } else if let Some((set_name, id, tick, base)) = state
        .action_clip(&tuning.0)
        .map(|(set, id, tick)| (set, id, tick, None))
        .or_else(|| state.base_clip(&tuning.0).map(|clip| (clip.set, clip.id, clip.tick, Some(clip))))
    {
        // A control state plays its own clip, or the base layer the one its rules picked, at
        // the tick the movement keeps, so what is drawn and what the movement acts on are the
        // same moment. [game]
        match set.find(set_name, id).and_then(|r| r.clip.as_ref().map(|clip| (r, clip))) {
            Some((r, clip)) => {
                let current = Some((set_name.to_owned(), id.to_owned()));
                let serial = base.map(|clip| clip.serial);
                let restarted = !r.looping && tick < animator.time;
                if animator.action != current || animator.serial != serial || restarted {
                    if let Some(sound) = sound {
                        sound.clip_changed();
                    }
                    effects.clip_changed();
                    trails.clip_changed();
                    animator.went_round = false;
                    // A clip fades in over the time the movement has for it: its own blend,
                    // or the one the clip before asked for on its way out where that names it
                    // (a landing into a run). [game]
                    let own = if r.crossfade < 0.0 { DEFAULT_CROSSFADE } else { r.crossfade };
                    let blend_time = match base {
                        Some(clip) => clip.blend,
                        None => state.weapon_fade().filter(|_| set_name == "WeaponSet").unwrap_or(own),
                    };
                    let clock = Clock {
                        set: crc32(set_name.as_bytes()), clip: crc32(id.as_bytes()), tick,
                        length: clip.length, speed: r.speed, looping: r.looping, exit_events: r.exit_events,
                    };
                    match animator.blend.as_mut() {
                        Some(blend) => blend.start(clock, blend_time),
                        None => animator.blend = Some(Blend::new(clock)),
                    }
                    animator.action = current;
                    animator.serial = serial;
                    // A weapon-set clip can start where the one before had got to
                    // (`usePrevAnimFrame`); its cues before that are not played.
                    animator.time = if state.mode == Mode::Strafe { tick } else { 0.0 };
                    animator.events_done = 0;
                }
                // A loop that has gone round plays the cues after its last position and up to
                // its new one.
                let due = tick >= animator.time || (r.looping && base.is_some());
                if let Some(sound) = sound.filter(|_| due) {
                    sound.clip_cues(clip, animator.time, tick, animator.went_round);
                }
                if due {
                    rig.clip_shakes(clip, animator.time, tick, state.pos, &tuning.0);
                    effects.clip_events(clip, animator.time, tick, animator.went_round);
                    trails.clip_events(clip, animator.time, tick);
                }
                animator.went_round |= tick < animator.time;
                // The clip's own events say which model shows and what the vehicle plays:
                // this is how the change of form swaps the robot and the car. [data]
                animator.transforming = !clip.model_events.is_empty();
                while let Some(event) = clip.model_events.get(animator.events_done).filter(|e| e.time <= tick) {
                    animator.events_done += 1;
                    animator.show_robot = event.show_true;
                    animator.show_vehicle = event.show_alternate;
                    if let Some(id) = &event.alternate_clip {
                        driver.playing = Some(id.clone());
                        driver.then = event.alternate_then.clone();
                        driver.time = 0.0;
                    }
                }
                animator.time = tick;
                let aim = rig.aim.offset();
                if let Some(blend) = animator.blend.as_mut() {
                    blend.update(tick, dt, &mut |clock, before, after| {
                        if let Some((_, outgoing)) = find_clock(set, clock) {
                            let length = outgoing.length.max(1.0);
                            let from = if clock.looping { before % length } else { before };
                            let to = if clock.looping { after % length } else { after };
                            if let Some(sound) = sound {
                                sound.clip_cues(outgoing, from, to, before >= length);
                            }
                            rig.clip_shakes(outgoing, from, to, state.pos, &tuning.0);
                            effects.clip_events(outgoing, from, to, before >= length);
                        }
                    });
                    blend.evaluate(&|clock| {
                        let Some((r, clip)) = find_clock(set, clock) else { return nodes.rest.clone() };
                        let mut pose = sample(clip, r.looping, clock.tick, &nodes.name_hashes, &nodes.rest);
                        lay_aim(&mut pose, r, aim, &nodes.name_hashes);
                        pose
                    }, &|from, mut pose, weight| {
                        cross_fade(&mut pose, &from, weight);
                        pose
                    })
                } else {
                    sample(clip, r.looping, tick, &nodes.name_hashes, &nodes.rest)
                }
            }
            None => animator.pose.clone(),
        }
    } else {
        // Nothing is playing on the robot: in the car, or before the first update has picked
        // a clip. It keeps the pose it had.
        if animator.action.is_some() { trails.clip_changed(); }
        animator.action = None;
        animator.serial = None;
        animator.transforming = false;
        animator.pose.clone()
    };
    // The weapon layer: its clip is laid over the nodes it has channels for (the right arm,
    // and for some the neck, head or spine), by the layer's weight. A clip of play type 1
    // (the recoil, the overheat) holds offsets, which are added to the pose under it; the
    // others hold poses, which replace it. [data] [game: FUN_0053e180 / FUN_0053d600]
    // What the next clip fades in over is the base layer's pose, before the layers above
    // it and the spine's twist are put on.
    animator.pose = pose.clone();
    let mut pose = pose;
    let layer = arsenal.as_deref().filter(|_| animator.forced.is_none() && !state.is_vehicle()).and_then(|arsenal| {
        let clips = library.robot.sets.get(crate::weapons::WEAPON_LAYER_SET)?;
        let arm = arsenal.gun.arm?;
        let r = clips.get(arm.clip)?;
        Some((r, r.clip.as_ref()?, arm.tick, arm.weight()))
    });
    if let Some((r, clip, tick, weight)) = layer.filter(|layer| layer.3 > 0.0) {
        if r.additive {
            // The layer's `AdditiveAnimation`: the same routine as the aim's offsets.
            lay_offsets(&mut pose, clip, tick, r.looping, weight, &nodes.name_hashes);
        } else {
            // The layer's `BlendAnimation`: the clip's nodes blended over the pose's.
            for (node, &name) in pose.iter_mut().zip(&nodes.name_hashes) {
                let Some(channel) = clip.channel(name).filter(|_| name != ROOT) else { continue };
                if let Some(p) = channel.position.sample(tick, clip.length, r.looping) {
                    node.translation = node.translation.lerp(Vec3::from(p), weight);
                }
                if let Some(q) = channel.rotation.sample(tick, clip.length, r.looping) {
                    node.rotation = blend_turn(node.rotation, stored_turn(q), weight);
                }
                if let Some(s) = channel.scale.sample(tick, clip.length, r.looping) {
                    node.scale = node.scale.lerp(Vec3::from(s), weight);
                }
            }
        }
    }
    // Layer 2 sits above the weapon arm. Its HitFront keys are additive offsets, so
    // locomotion and recoil continue underneath it. [game: FUN_0084dfa0 / FUN_007482c0]
    if animator.forced.is_none() {
        if let Some(r) = state.hit_partial.current(&tuning.0.rule_sets).and_then(|c| {
            library.robot.sets.get("HitReactPartialSet")?.iter().find(|r| crc32(r.id.as_bytes()) == c.id)
        }) {
            if let Some(clip) = r.clip.as_ref() {
                lay_offsets(&mut pose, clip, state.hit_partial.tick, r.looping, 1.0, &nodes.name_hashes);
            }
        }
    }
    // Standing in weapon mode his spine twists toward the aim (StrafeMode's `+0x24`, handed
    // to the model's `SpineIK`, which links `Spine_02` and `Spine_03`). [game] [data]
    // [assumed: the links share it equally, each turned about the vertical; the `SpineIK`
    // code itself is not read]
    let twist = if state.mode == Mode::Strafe && animator.forced.is_none() { state.strafe.twist } else { 0.0 };
    if twist != 0.0 && !nodes.spine_links.is_empty() {
        let share = Quat::from_rotation_z(twist / nodes.spine_links.len() as f32);
        for &link in &nodes.spine_links {
            // The turn its parents give it, so the twist is about his own vertical.
            let mut above = Quat::IDENTITY;
            let mut parent = nodes.parents[link];
            while let Some(p) = parent {
                above = pose[p].rotation * above;
                parent = nodes.parents[p];
            }
            pose[link].rotation = above.inverse() * share * above * pose[link].rotation;
        }
    }
    if !animator.transforming {
        let in_car = state.mode == Mode::Drive;
        animator.show_robot = !in_car;
        animator.show_vehicle = in_car;
    }
    **robot_visibility = visibility(animator.show_robot);
    **vehicle_visibility = visibility(animator.show_vehicle);
    if let Some(foot_rig) = &nodes.foot_rig {
        // [assumed] Ground locomotion/weapon stance only. Original FootIK enable
        // state writes are unresolved; see notes/foot-ik.md and notes/status.md.
        let active = animator.forced.is_none() && !animator.transforming && !state.dead()
            && matches!(state.mode, Mode::Stand | Mode::Move | Mode::Strafe)
            && state.ground_flag() && state.vel.z <= 0.0;
        let mut local: Vec<_> = pose.iter().map(Transform::compute_affine).collect();
        let root = bevy::math::Affine3A::from_rotation_translation(Quat::from_rotation_z(state.yaw), state.pos);
        animator.foot_ik.apply(foot_rig, &mut local, &nodes.parents, root, active, dt, &arena.0);
        for (pose, at) in pose.iter_mut().zip(local) {
            let (scale, rotation, translation) = at.to_scale_rotation_translation();
            *pose = Transform { scale, rotation, translation };
        }
    }
    let local: Vec<_> = pose.iter().map(Transform::compute_affine).collect();
    let root = bevy::math::Affine3A::from_rotation_translation(Quat::from_rotation_z(state.yaw), state.pos);
    let world: Vec<_> = tf2_core::footik::accumulate(&local, &nodes.parents).into_iter().map(|at| root * at).collect();
    effects.steps(&world, &data.0, state.body_forward(), &arena.0);
    if let Some(sound) = sound {
        sound.component_sources(false, &world);
        sound.footsteps(&world, &data.0.robot.footsteps, &arena.0);
        sound.climb_steps(&world, &data.0.robot.climb_steps, state.body_forward(), &arena.0);
        sound.spawn_cues(&world, &data.0.robot.spawn_sound_players);
    }
    for (entity, transform) in nodes.entities.iter().zip(&pose) {
        if let Ok(mut node) = transforms.get_mut(*entity) {
            *node = *transform;
        }
    }

    // The vehicle: its own clip if one is playing, then the tyres on top.
    let mut vehicle_pose = vehicle_nodes.rest.clone();
    if let Some((r, clip)) = driver.playing.as_deref().and_then(|id| find_vehicle(&library.vehicle, id)) {
        let before = driver.time;
        driver.time += dt * TICKS_PER_SECOND * r.speed;
        if let Some(sound) = sound.filter(|_| !r.looping) {
            sound.clip_cues(clip, before, driver.time.min(clip.length), false);
        }
        if r.looping {
            driver.time %= clip.length.max(1.0);
        } else if driver.time >= clip.length {
            driver.time = clip.length;
            if let Some(next) = driver.then.take() {
                driver.playing = Some(next);
                driver.time = 0.0;
            }
        }
        vehicle_pose = sample(clip, r.looping, driver.time, &vehicle_nodes.name_hashes, &vehicle_nodes.rest);
    }
    // 008601d0: visual travel clamps, front steering multiplier and suspension offsets.
    // Model nodes remain in game space under the root's model-space conversion. [game]
    let presentation=state.car_presentation();
    let wheels=&presentation.wheels;
    let turn=presentation.rotation;
    let up=turn*Vec3::Z;
    let steer = Quat::from_rotation_z(presentation.steer_angle*-3.0);
    let tyre_names=["Tire_Front_L","Tire_Front_R","Tire_Rear_L","Tire_Rear_R"];
    for (i, name) in vehicle_nodes.names.iter().enumerate() {
        if let Some(wheel)=tyre_names.iter().position(|n|*n==name) {
            vehicle_pose[i].rotation *= Quat::from_rotation_x(wheels[wheel].render_spin);
        } else if name.starts_with("TireGOAL_Front") {
            vehicle_pose[i].rotation *= steer;
        }
    }
    if state.mode==Mode::Drive {
        for (wheel,name) in tyre_names.into_iter().enumerate() {
            let Some(node)=vehicle_nodes.names.iter().position(|n|n==name) else {continue;};
            let mut frame=vehicle_pose[node].to_matrix();
            let mut parent=vehicle_nodes.parents[node];
            while let Some(p)=parent {frame=vehicle_pose[p].to_matrix()*frame;parent=vehicle_nodes.parents[p];}
            let point=presentation.pos+turn*frame.w_axis.truncate();
            let offset=tf2_core::vehicle::Chassis::render_offset(&wheels[wheel],point.z,up.z,&tuning.0.drive);
            // World body-up becomes root-local +z; rotate through the node's parents.
            let mut parent_turn=Quat::IDENTITY;
            let mut parent=vehicle_nodes.parents[node];
            while let Some(p)=parent {parent_turn=vehicle_pose[p].rotation*parent_turn;parent=vehicle_nodes.parents[p];}
            vehicle_pose[node].translation-=parent_turn.inverse()*Vec3::Z*offset;
        }
    }
    if let Some(sound) = sound {
        let local: Vec<_> = vehicle_pose.iter().map(Transform::compute_affine).collect();
        let root = bevy::math::Affine3A::from_rotation_translation(turn, presentation.pos);
        let world: Vec<_> = tf2_core::footik::accumulate(&local, &vehicle_nodes.parents)
            .into_iter().map(|at| root * at).collect();
        sound.component_sources(true, &world);
    }
    for (entity, transform) in vehicle_nodes.entities.iter().zip(&vehicle_pose) {
        if let Ok(mut node) = transforms.get_mut(*entity) {
            *node = *transform;
        }
    }
}
