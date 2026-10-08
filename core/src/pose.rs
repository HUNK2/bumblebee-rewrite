//! Where a clip puts an object's nodes. No engine types.
//!
//! The drawing side poses the same tree through its engine; this is the same arithmetic for
//! the parts of the movement that need a limb's place (the collision bodies an attack hits
//! with). Everything is in the object's own space, which for the robot is his own frame:
//! x right, y forward, z up, feet at the origin.

use glam::{Affine3A, Quat, Vec3, Vec3A};

use crate::formats::anim::{Clip, ROOT_CHANNEL};
use crate::formats::scene::ObjectDef;

/// A stored matrix (three axis rows and a position row, for row vectors) as a transform of
/// column vectors.
pub fn affine(rows: &[[f32; 3]; 4]) -> Affine3A {
    Affine3A::from_cols(Vec3A::from(rows[0]), Vec3A::from(rows[1]), Vec3A::from(rows[2]), Vec3A::from(rows[3]))
}

/// A turn stored for row-vector matrices as the same turn for column vectors.
pub fn stored_turn(q: [f32; 4]) -> Quat {
    Quat::from_xyzw(-q[0], -q[1], -q[2], q[3])
}

/// Two turns blended as the engine blends them (`FUN_00542ff0`, used by
/// `BlendAnimation::BlendOutputs` for cross-fades and layers): a straight line between
/// the two the short way round, then normalised. [game]
pub fn blend_turn(from: Quat, to: Quat, share: f32) -> Quat {
    from.lerp(to, share)
}

/// An offset turn laid on a node's own by `weight` (`AdditiveAnimation::BlendOutputs`,
/// `FUN_0053e180`): the offset is blended from no turn by the weight, then applied in
/// the node's own frame, after the pose's turn. [game]
pub fn offset_turn(turn: Quat, offset: Quat, weight: f32) -> Quat {
    (turn * blend_turn(Quat::IDENTITY, offset, weight)).normalize()
}

/// An offset position (added, times the weight) and an offset scale (its distance from 1
/// times the weight, multiplied on), from the same routine. [game]
pub fn offset_position(position: Vec3, offset: Vec3, weight: f32) -> Vec3 {
    position + offset * weight
}

pub fn offset_scale(scale: Vec3, offset: Vec3, weight: f32) -> Vec3 {
    scale * ((offset - Vec3::ONE) * weight + Vec3::ONE)
}

/// An object's node tree at rest, ready to be posed.
pub struct Rig {
    name_hashes: Vec<u32>,
    parents: Vec<Option<usize>>,
    /// Each node relative to its parent: scale, rotation, position.
    rest: Vec<(Vec3, Quat, Vec3)>,
}

impl Rig {
    /// [game] Character layer above the base: sparse additive offsets or sampled poses,
    /// in node-local space before parent accumulation (0053e180 / 0053d600).
    pub fn pose_layer(&self, base: Option<(&Clip,bool,f32)>, over: &Clip, looping: bool, tick: f32, additive: bool, weight: f32) -> Vec<Affine3A> {
        let mut out: Vec<Affine3A>=Vec::with_capacity(self.rest.len());
        for (i,&(mut scale,mut rotation,mut position)) in self.rest.iter().enumerate() {
            let name=self.name_hashes[i];
            if name!=ROOT_CHANNEL {
                if let Some((clip,looping,tick))=base && let Some(channel)=clip.channel(name) {
                    if let Some(p)=channel.position.sample(tick,clip.length,looping) {position=Vec3::from(p);}
                    if let Some(q)=channel.rotation.sample(tick,clip.length,looping) {rotation=stored_turn(q);}
                    if let Some(s)=channel.scale.sample(tick,clip.length,looping) {scale=Vec3::from(s);}
                }
                if let Some(channel)=over.channel(name) {
                    if let Some(p)=channel.position.sample(tick,over.length,looping) {
                        position=if additive {offset_position(position,Vec3::from(p),weight)} else {position.lerp(Vec3::from(p),weight)};
                    }
                    if let Some(q)=channel.rotation.sample(tick,over.length,looping) {
                        rotation=if additive {offset_turn(rotation,stored_turn(q),weight)} else {blend_turn(rotation,stored_turn(q),weight)};
                    }
                    if let Some(s)=channel.scale.sample(tick,over.length,looping) {
                        scale=if additive {offset_scale(scale,Vec3::from(s),weight)} else {scale.lerp(Vec3::from(s),weight)};
                    }
                }
            }
            let local=Affine3A::from_scale_rotation_translation(scale,rotation,position);
            out.push(self.parents[i].map_or(local,|parent|out[parent]*local));
        }
        out
    }
    pub fn new(object: &ObjectDef) -> Self {
        let world: Vec<Affine3A> = object.nodes.iter().map(|n| affine(&n.bind)).collect();
        let rest = object
            .nodes
            .iter()
            .enumerate()
            .map(|(i, node)| match node.parent {
                Some(parent) => (world[parent].inverse() * world[i]).to_scale_rotation_translation(),
                None => world[i].to_scale_rotation_translation(),
            })
            .collect();
        Self {
            name_hashes: object.nodes.iter().map(|n| n.name_hash).collect(),
            // A node is listed after its parent; one that is not is taken as a root.
            parents: object.nodes.iter().enumerate().map(|(i, n)| n.parent.filter(|&p| p < i)).collect(),
            rest,
        }
    }

    /// Every node's transform in the object's space with the clip at `tick`. Nodes the clip
    /// does not move keep their rest; the root stays put, its travel and turn being movement
    /// (as the drawing side has it). [data]
    pub fn pose(&self, clip: &Clip, looping: bool, tick: f32) -> Vec<Affine3A> {
        let mut out: Vec<Affine3A> = Vec::with_capacity(self.rest.len());
        for (i, &(mut scale, mut rotation, mut position)) in self.rest.iter().enumerate() {
            let name = self.name_hashes[i];
            if let Some(channel) = clip.channel(name) {
                if let Some(p) = channel.position.sample(tick, clip.length, looping).filter(|_| name != ROOT_CHANNEL) {
                    position = Vec3::from(p);
                }
                if let Some(q) = channel.rotation.sample(tick, clip.length, looping).filter(|_| name != ROOT_CHANNEL) {
                    // Stored for row-vector matrices: the conjugate for column vectors.
                    rotation = Quat::from_xyzw(-q[0], -q[1], -q[2], q[3]);
                }
                if let Some(s) = channel.scale.sample(tick, clip.length, looping) {
                    scale = Vec3::from(s);
                }
            }
            let local = Affine3A::from_scale_rotation_translation(scale, rotation, position);
            out.push(match self.parents[i] {
                Some(parent) => out[parent] * local,
                None => local,
            });
        }
        out
    }

    /// As `pose`, with a clip of offsets (an additive clip of a layer above) laid over it:
    /// each node the second clip moves is shifted by its position key and turned by its
    /// rotation key, in the node's own frame when `own_frame` (after the pose's turn) or
    /// in its parent's (before it). For checking which the data means.
    pub fn pose_with_offsets(&self, clip: &Clip, looping: bool, tick: f32, over: &Clip, over_tick: f32, own_frame: bool) -> Vec<Affine3A> {
        let mut out: Vec<Affine3A> = Vec::with_capacity(self.rest.len());
        for (i, &(mut scale, mut rotation, mut position)) in self.rest.iter().enumerate() {
            let name = self.name_hashes[i];
            if let Some(channel) = clip.channel(name) {
                if let Some(p) = channel.position.sample(tick, clip.length, looping).filter(|_| name != ROOT_CHANNEL) {
                    position = Vec3::from(p);
                }
                if let Some(q) = channel.rotation.sample(tick, clip.length, looping).filter(|_| name != ROOT_CHANNEL) {
                    rotation = Quat::from_xyzw(-q[0], -q[1], -q[2], q[3]);
                }
                if let Some(s) = channel.scale.sample(tick, clip.length, looping) {
                    scale = Vec3::from(s);
                }
            }
            if let Some(channel) = over.channel(name).filter(|_| name != ROOT_CHANNEL) {
                if let Some(p) = channel.position.sample(over_tick, over.length, false) {
                    position += Vec3::from(p);
                }
                if let Some(q) = channel.rotation.sample(over_tick, over.length, false) {
                    let offset = Quat::from_xyzw(-q[0], -q[1], -q[2], q[3]);
                    rotation = if own_frame { rotation * offset } else { offset * rotation };
                }
            }
            let local = Affine3A::from_scale_rotation_translation(scale, rotation, position);
            out.push(match self.parents[i] {
                Some(parent) => out[parent] * local,
                None => local,
            });
        }
        out
    }

    /// As `pose`, with an offset clip laid on by `weight` as the engine lays one on (one of
    /// a weapon-set clip's two aim clips, by how far the aim is from level). [game]
    pub fn pose_offset(&self, clip: &Clip, looping: bool, tick: f32, over: &Clip, weight: f32) -> Vec<Affine3A> {
        let mut out: Vec<Affine3A> = Vec::with_capacity(self.rest.len());
        for (i, &(mut scale, mut rotation, mut position)) in self.rest.iter().enumerate() {
            let name = self.name_hashes[i];
            if let Some(channel) = clip.channel(name) {
                if let Some(p) = channel.position.sample(tick, clip.length, looping).filter(|_| name != ROOT_CHANNEL) {
                    position = Vec3::from(p);
                }
                if let Some(q) = channel.rotation.sample(tick, clip.length, looping).filter(|_| name != ROOT_CHANNEL) {
                    rotation = stored_turn(q);
                }
                if let Some(s) = channel.scale.sample(tick, clip.length, looping) {
                    scale = Vec3::from(s);
                }
            }
            if let Some(channel) = over.channel(name).filter(|_| name != ROOT_CHANNEL) {
                if let Some(p) = channel.position.sample(0.0, over.length, false) {
                    position = offset_position(position, Vec3::from(p), weight);
                }
                if let Some(q) = channel.rotation.sample(0.0, over.length, false) {
                    rotation = offset_turn(rotation, stored_turn(q), weight);
                }
                if let Some(s) = channel.scale.sample(0.0, over.length, false) {
                    scale = offset_scale(scale, Vec3::from(s), weight);
                }
            }
            let local = Affine3A::from_scale_rotation_translation(scale, rotation, position);
            out.push(match self.parents[i] {
                Some(parent) => out[parent] * local,
                None => local,
            });
        }
        out
    }

    /// The nodes at rest, in the object's space.
    pub fn rest(&self) -> Vec<Affine3A> {
        let mut out: Vec<Affine3A> = Vec::with_capacity(self.rest.len());
        for (i, &(scale, rotation, position)) in self.rest.iter().enumerate() {
            let local = Affine3A::from_scale_rotation_translation(scale, rotation, position);
            out.push(match self.parents[i] {
                Some(parent) => out[parent] * local,
                None => local,
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_offset_is_laid_on_in_the_nodes_own_frame_by_its_weight() {
        let turn = Quat::from_rotation_z(1.0);
        let offset = Quat::from_rotation_x(0.4);
        // In full: the pose's turn, then the offset about the turned node's own x.
        let full = offset_turn(turn, offset, 1.0);
        assert!(full.angle_between(turn * offset) < 1e-5);
        assert!(offset_turn(turn, offset, 0.0).angle_between(turn) < 1e-6);
        // Half the weight is half the turn (a straight blend from no turn, normalised).
        let half = offset_turn(Quat::IDENTITY, offset, 0.5);
        assert!(half.angle_between(Quat::from_rotation_x(0.2)) < 1e-3);
        assert_eq!(offset_position(Vec3::X, Vec3::new(0.0, 2.0, 0.0), 0.5), Vec3::new(1.0, 1.0, 0.0));
        assert_eq!(offset_scale(Vec3::splat(2.0), Vec3::splat(1.5), 0.5), Vec3::splat(2.5));
    }

    #[test]
    fn turns_blend_the_short_way_round() {
        let a = Quat::from_rotation_z(0.2);
        let b = -Quat::from_rotation_z(0.6);
        assert!(blend_turn(a, b, 0.5).angle_between(Quat::from_rotation_z(0.4)) < 1e-3);
    }
}
