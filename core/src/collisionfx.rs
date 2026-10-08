//! CollisionTrigger history and CollisionFX presets (007dab90,007da7b0,007d9e40).
use std::collections::BTreeMap;
use glam::{Mat3,Quat,Vec3};
use crate::{formats::lxb::Node,particles::Template,vehicle::Contact};

#[derive(Clone,Debug)]
pub struct Def {
    pub mask: u32, pub types: u32, pub min_speed: f32, pub max_speed: f32,
    pub align: bool,
    /// soft/hard, each impact/roll/scrape. [data]
    pub particles: [[Option<Template>;3];2],
    pub sounds: [[u32;3];2],
}
impl Def {
    pub fn read(node:Node)->Self {
        let int=|name|node.get(name).and_then(Node::int).unwrap_or(0) as u32;
        let num=|name,default|node.get(name).and_then(Node::float).unwrap_or(default);
        let kinds=["impact","roll","scrape"];
        Self {mask:int("maskFilter"),types:node.get("typeFilter").and_then(Node::int).unwrap_or(0xffff) as u32,
            min_speed:num("minVelocity",0.0),max_speed:num("maxVelocity",1e37),align:int("alignToNormal")!=0,
            particles:std::array::from_fn(|hard|std::array::from_fn(|kind| {
                let info=node.path(&[if hard==0 {"softPFX"} else {"hardPFX"},kinds[kind]])?;
                if info.get("disable").and_then(Node::int).unwrap_or(0)!=0 {return None;}
                info.get("id").and_then(Template::read)
            })),
            sounds:std::array::from_fn(|hard|std::array::from_fn(|kind| {
                let Some(info)=node.path(&[if hard==0 {"softSFX"} else {"hardSFX"},kinds[kind]]) else {return 0;};
                if info.get("disable").and_then(Node::int).unwrap_or(0)!=0 {return 0;}
                info.get("override").and_then(Node::int).unwrap_or(0) as u32
            })),
        }
    }
    pub fn accepts(&self,contact:&Contact)->bool {
        self.accepts_speed(contact.velocity.length())
    }
    pub fn accepts_speed(&self,speed:f32)->bool {
        self.mask&1!=0 && self.types&1!=0 && speed>=self.min_speed && speed<=self.max_speed
    }
}

#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum Kind {Impact,Roll,Scrape,End}
impl Kind {pub fn index(self)->Option<usize> {match self {Self::Impact=>Some(0),Self::Roll=>Some(1),Self::Scrape=>Some(2),Self::End=>None}}}

/// SoundPhysicsReactor / CollisionSFX (01184450,0077f620).
#[derive(Clone, Copy, Debug, Default)]
pub struct SoundChoice { pub id: u32, pub once: bool, pub attached: bool }
#[derive(Clone, Debug)]
pub struct SoundDef {
    pub filter: Def,
    /// Hard, soft, bottom; each impact, roll, scrape. [data]
    pub choices: [[SoundChoice; 3]; 3],
}
impl SoundDef {
    pub fn read(node: Node) -> Self {
        Self { filter: Def::read(node), choices: ["hard", "soft", "bottom"].map(|material|
            ["impact", "roll", "scrape"].map(|kind| {
                let Some(info) = node.path(&[material, kind]) else { return SoundChoice::default() };
                if info.get("disable").and_then(Node::int) == Some(1) { return SoundChoice::default() }
                SoundChoice { id: info.get("override").and_then(Node::int).unwrap_or(0) as u32,
                    once: info.get("oneShot").and_then(Node::int) == Some(1),
                    attached: info.get("attachToObject").and_then(Node::int) == Some(1) }
            })) }
    }
    /// [game: 0077f620] Soft and bottom override only when they name a sound.
    /// `oneShot` consumes a preset entry, rather than changing the FEV's loop mode.
    pub fn select(&mut self, kind: Kind, hard: bool, bottom: bool) -> Option<SoundChoice> {
        let kind = kind.index()?;
        let material = if !hard && self.choices[1][kind].id != 0 { 1 } else { 0 };
        let selected = self.choices[if bottom && self.choices[2][kind].id != 0 { 2 } else { material }][kind];
        for row in [if hard { 0 } else { 1 }].into_iter().chain(bottom.then_some(2)) {
            if self.choices[row][kind].once { self.choices[row][kind].id = 0; }
        }
        (selected.id != 0).then_some(selected)
    }
}
#[derive(Clone,Copy,Debug)]
pub struct Event {pub key:u32,pub kind:Kind,pub point:Vec3,pub normal:Vec3,pub surface:u32,pub speed:f32}

/// [game] 0056e7b0: +z along normal, +y from projected reference +y; use +x
/// as reference when +y is too nearly parallel. This fixes emitter twist too.
pub fn normal_rotation(normal:Vec3)->Quat {
    let z=normal.normalize_or_zero();
    if z==Vec3::ZERO {return Quat::IDENTITY;}
    let y=if z.dot(Vec3::Y).abs()<=0.96 {
        (Vec3::Y-z*z.y).normalize()
    } else {z.cross(Vec3::X).normalize()};
    Quat::from_mat3(&Mat3::from_cols(y.cross(z),y,z))
}
struct Track {contact:Contact,left:f32,history:[f32;10],sum:f32,kind:Option<Kind>,fresh:bool}
#[derive(Default)]
pub struct History {tracks:BTreeMap<u32,Track>,slot:usize}
impl History {
    /// Lux physics types: 1 roll, 2/5 sliding, 3/4 initial push. [game]
    pub fn touch(&mut self,contact:Contact,physics_type:u32) {
        let t=self.tracks.entry(contact.key).or_insert_with(||Track {contact,left:0.25,
            history:[0.0;10],sum:0.0,kind:None,fresh:physics_type==3||physics_type==4});
        t.contact=contact;
        if physics_type==3 || physics_type==4 {return;}
        t.left=0.25;
        t.history[self.slot]=if physics_type==1 {1.0} else {-1.0};
    }
    pub fn contains(&self,key:u32)->bool {self.tracks.contains_key(&key)}
    /// Called once per original .032 s update. Keeps a ten-update signed history,
    /// threshold seven, and expires after .25 s without further contacts. [game]
    pub fn step(&mut self,dt:f32)->Vec<Event> {
        let old=self.slot; self.slot=(self.slot+1)%10;
        let mut events=Vec::new();
        self.tracks.retain(|&key,t| {
            let mut emit=|kind|events.push(Event {key,kind,point:t.contact.point,normal:t.contact.normal,
                surface:t.contact.surface,speed:t.contact.velocity.length()});
            if t.left<=0.0 {emit(Kind::End);return false;}
            t.sum+=t.history[old]-t.history[self.slot];t.history[self.slot]=0.0;
            let kind=if t.sum>=7.0 && t.kind!=Some(Kind::Roll) {Some(Kind::Roll)}
                else if t.sum<=-7.0 && t.kind!=Some(Kind::Scrape) {Some(Kind::Scrape)}
                else if t.fresh && t.left==0.25 {Some(Kind::Impact)} else {None};
            if let Some(kind)=kind {emit(kind);t.kind=Some(kind);}
            t.fresh=false; t.left-=dt;
            true
        }); events
    }
    pub fn clear(&mut self)->Vec<Event> {
        let events=self.tracks.iter().map(|(&key,t)|Event {key,kind:Kind::End,
            point:t.contact.point,normal:t.contact.normal,surface:t.contact.surface,
            speed:t.contact.velocity.length()}).collect();
        self.tracks.clear();events
    }
    pub fn point(&self,key:u32)->Option<Vec3> {self.tracks.get(&key).map(|t|t.contact.point)}
}

#[cfg(test)]
mod tests {
    use super::*;
    fn contact()->Contact {Contact {key:7,fraction:0.0,point:Vec3::ZERO,normal:Vec3::X,depth:0.0,surface:0,velocity:Vec3::Y*20.0}}
    #[test]
    fn collision_sound_overrides_and_once_flags_follow_the_material_branch() {
        let filter = Def { mask: 1, types: 1, min_speed: 0.0, max_speed: 100.0, align: false,
            particles: std::array::from_fn(|_| std::array::from_fn(|_| None)), sounds: [[0; 3]; 2] };
        let mut sounds = SoundDef { filter, choices: [[SoundChoice::default(); 3]; 3] };
        sounds.choices[0][0] = SoundChoice { id: 1, once: true, attached: true };
        sounds.choices[2][0] = SoundChoice { id: 2, once: true, attached: false };
        // Soft's empty entry falls back to hard, but consumes only soft's once flag.
        assert_eq!(sounds.select(Kind::Impact, false, false).unwrap().id, 1);
        assert_eq!(sounds.choices[0][0].id, 1);
        assert_eq!(sounds.select(Kind::Impact, true, true).unwrap().id, 2);
        assert_eq!(sounds.choices[0][0].id, 0);
        assert_eq!(sounds.choices[2][0].id, 0);
        assert!(sounds.select(Kind::Impact, true, true).is_none());
        assert!(sounds.select(Kind::End, true, true).is_none());
    }
    #[test]
    fn contact_alignment_keeps_reference_twist_and_handles_parallel_normal() {
        for z in [Vec3::Z,Vec3::X,Vec3::Y,Vec3::NEG_Y,Vec3::new(0.2,0.4,0.8).normalize()] {
            let q=normal_rotation(z);
            assert!((q*Vec3::Z).abs_diff_eq(z,1e-6));
            assert!((q*Vec3::X).cross(q*Vec3::Y).abs_diff_eq(z,1e-6));
        }
        assert_eq!(normal_rotation(Vec3::Z),Quat::IDENTITY);
    }
    #[test]
    fn initial_impact_scrape_window_and_end_are_separate_events() {
        let mut h=History::default();h.touch(contact(),3);
        assert_eq!(h.step(0.032)[0].kind,Kind::Impact);
        for _ in 0..6 {h.touch(contact(),5);assert!(h.step(0.032).is_empty());}
        h.touch(contact(),5);assert_eq!(h.step(0.032)[0].kind,Kind::Scrape);
        h.touch(contact(),5);assert!(h.step(0.032).is_empty());
        let mut end=0;
        for _ in 0..10 {end+=h.step(0.032).iter().filter(|e|e.kind==Kind::End).count();}
        assert_eq!(end,1);assert!(!h.contains(7));
    }
    #[test]
    fn sparse_rolling_contacts_do_not_cross_the_ten_update_threshold() {
        let mut h=History::default();
        for i in 0..60 {if i%2==0 {h.touch(contact(),1);}
            assert!(h.step(0.032).iter().all(|e|e.kind!=Kind::Roll));}
        for _ in 0..10 {h.touch(contact(),1);h.step(0.032);}
        assert_eq!(h.tracks[&7].kind,Some(Kind::Roll));
    }
}
