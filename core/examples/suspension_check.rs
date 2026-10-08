//! Sharp-turn and suspension diagnostics using the installed model/tuning.
use glam::{Quat, Vec3};
use tf2_core::{character, pose::Rig, sim::{self, Arena, Input, State}, tuning, vehicle::Chassis};

fn main() {
    let dir=std::env::var("TF2_GAME_DIR").unwrap_or_else(|_|"C:/Games2".into());
    let dir=std::path::Path::new(&dir);
    let t=tuning::load(dir,"Bumblebee").unwrap();
    let data=character::load(dir,"bumblebee").unwrap();
    let model=data.vehicle.as_ref().unwrap();
    let rest=Rig::new(model).rest();
    let names=["Tire_Front_L","Tire_Front_R","Tire_Rear_L","Tire_Rear_R"];
    let centres=names.map(|name| rest[model.nodes.iter().position(|n|n.name==name).unwrap()].translation);
    println!("tyre centres {centres:?}; drive {:?}",t.drive);
    for render in &model.renders {
        if !render.model.contains("Tire") {continue;}
        if let Some(mesh)=data.library.meshes.values().find(|m|m.name.eq_ignore_ascii_case(&render.model)) {
            let (mut lo,mut hi)=(Vec3::splat(f32::INFINITY),Vec3::splat(f32::NEG_INFINITY));
            for p in mesh.parts.iter().flat_map(|p|&p.positions) {let v=Vec3::from(*p);lo=lo.min(v);hi=hi.max(v);}
            println!("{} mesh={} bounds {lo:?}..{hi:?}",model.nodes[render.node].name,mesh.name);
        }
    }
    for steer in [-1.0,1.0] {
        let mut s=State::new(&t);
        let arena=Arena::default();
        let (mut max_roll,mut min_bottom,mut min_at)=(0.0_f32, f32::INFINITY,0);
        for i in 0..625 {
            let input=Input {vehicle:1.0,steer:if i>94 {steer} else {0.0},..Default::default()};
            sim::step(&mut s,&input,&t,&arena,0.032);
            if s.mode!=sim::Mode::Drive {continue;}
            let turn=tf2_core::vehicle::rotation(s.yaw,s.pitch,s.roll);
            let up=turn*Vec3::Z;
            max_roll=max_roll.max(s.roll.abs());
            for (w, centre) in s.car_wheels().iter().zip(centres) {
                let point=s.pos+turn*Vec3::from(centre);
                let offset=Chassis::render_offset(w,point.z,up.z,&t.drive);
                // Bottom of a circular tyre in its rotated wheel plane (local yz).
                let wheel_turn=turn*Quat::from_rotation_z(if centre.y>0.0 {-3.0*s.steer_angle} else {0.0});
                let bottom=point.z-up.z*offset-t.drive.render_wheel_radius*(1.0-(wheel_turn*Vec3::X).z.powi(2)).sqrt();
                if bottom<min_bottom {min_bottom=bottom;min_at=i;}
            }
            if (i<40 && i%3==0) || i%94==0 || i==624 {
                println!("steer {steer:+}, t={:.3}, speed={:.3}, root_z={:.4}, roll={:.4}, pitch={:.4}, springs={:?}, rest={:?}, min_bottom={min_bottom:.4}",
                    (i+1) as f32*0.032,s.speed,s.pos.z,s.roll,s.pitch,s.car_wheels().map(|w|w.length),s.car_wheels().map(|w|w.rest_length));
            }
        }
        println!("steer {steer:+}: max_roll={max_roll:.4}, lowest_tyre={min_bottom:.4} at {:.3}s",(min_at+1) as f32*0.032);
    }
}
