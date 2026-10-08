//! Deterministic vehicle diagnostics using installed tuning and authored arena fixtures.
use glam::{Vec2,Vec3};
use tf2_core::{sim::{self,Arena,Input,State,Box3},terrain::Ramp,tuning};
fn main() {
    let dir=std::env::var("TF2_GAME_DIR").unwrap_or_else(|_|"C:/Games2".into());
    let t=tuning::load(std::path::Path::new(&dir),"Bumblebee").unwrap();
    let flat=Arena::default();
    let ramp=Arena {ramps:vec![Ramp {min:Vec2::new(-106.0,-90.0),max:Vec2::new(-84.0,-40.0),
        base:0.0,height:14.0,slope:Vec2::new(0.0,-0.28)}],..Default::default()};
    let bank=Arena {ramps:vec![Ramp {min:Vec2::new(125.0,-130.0),max:Vec2::new(165.0,-35.0),
        base:0.0,height:0.0,slope:Vec2::new(0.15,0.0)}],..Default::default()};
    let wall=Arena {boxes:vec![Box3 {min:Vec3::new(-20.0,30.0,0.0),max:Vec3::new(20.0,30.01,20.0)}],
        ..Default::default()};
    for (name,arena,pos,yaw) in [
        ("flat",flat,Vec3::ZERO,0.0),
        ("launch",ramp,Vec3::new(-95.0,-25.0,0.0),std::f32::consts::PI),
        ("bank",bank,Vec3::new(145.0,-45.0,3.0),std::f32::consts::PI),
        ("wall",wall,Vec3::ZERO,0.0)] {
        let mut s=State::new(&t);s.pos=pos;s.yaw=yaw;
        let (mut z,mut pitch,mut roll,mut launches,mut landings,mut contacts)=(pos.z,0.0_f32,0.0_f32,0,0,0);
        let mut was_down=false;
        for i in 0..375 {
            sim::step(&mut s,&Input {vehicle:1.0,..Default::default()},&t,&arena,0.032);
            assert!(s.pos.is_finite() && s.vel.is_finite(),"{name}, frame {i}");
            z=z.max(s.pos.z);pitch=pitch.max(s.pitch.abs());roll=roll.max(s.roll.abs());
            let down=s.car_wheels().iter().any(|w|w.grounded);
            if was_down && !down && s.vel.z>0.0 {launches+=1;println!("{name}: launch at {:.3}s, vz={:.2}",(i+1) as f32*0.032,s.vel.z);}
            if !was_down && down && i>20 {landings+=1;}
            was_down=down;
            contacts+=s.vehicle_contacts.len();
        }
        println!("{name}: max_z={z:.3}, max_pitch={pitch:.3}, max_roll={roll:.3}, launches={launches}, landings={landings}, contacts={contacts}, final={:?}",s.pos);
        match name {
            "flat"=>assert!(s.pos.z.abs()<0.2 && s.speed>34.0),
            "launch"=>assert!(z>14.0 && launches>0 && landings>0),
            "bank"=>assert!(roll>0.05),
            "wall"=>assert!(s.pos.y<30.0 && contacts>0),
            _=>{}
        }
    }
}
