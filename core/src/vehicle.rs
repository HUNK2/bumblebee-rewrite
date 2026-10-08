//! Four wheel contacts and the original spring rules (`notes/driving.md`).
//! Arena geometry is authored; the wheel, spring and support-plane rules are [game].
use glam::{EulerRot, Mat3, Quat, Vec3};
use crate::{sim::{World, VEHICLE_MASS}, tuning::DriveTuning};

/// A ray result supplied by the world, including the surface's material.
#[derive(Clone, Copy, Debug)]
pub struct GroundHit { pub point: Vec3, pub normal: Vec3, pub surface: u32 }

#[derive(Clone, Copy, Debug, Default)]
pub struct Wheel {
    pub grounded: bool,
    pub was_grounded: bool,
    pub point: Vec3,
    pub normal: Vec3,
    pub surface: u32,
    pub length: f32,
    pub previous_length: f32,
    pub rest_length: f32,
    /// Wheel +0x50, consumed by the driving camera's airborne hysteresis. [game]
    pub probe_distance: f32,
    pub spin: f32,
    /// Wheel model rotation, separately from the physical tyre spin (008601d0). [game]
    pub render_spin: f32,
    /// Stationary spring collider's world anchor (007a2cd0). [game]
    target: Vec3,
}

/// Static-world contact returned by continuous convex collision.
#[derive(Clone, Copy, Debug)]
pub struct Contact {
    /// Stable static collider identity, for CollisionTrigger's contact history.
    pub key: u32,
    pub fraction: f32,
    pub point: Vec3,
    pub normal: Vec3,
    pub depth: f32,
    pub surface: u32,
    pub velocity: Vec3,
}

#[derive(Clone, Copy, Debug)]
pub struct Chassis {
    pub wheels: [Wheel; 4],
    /// Conventional angular velocity in WORLD axes. Lux stores its negative;
    /// 007a2860 subtracts the stored omega cross arm. [game/trace]
    pub angular: Vec3,
    pub normal: Vec3,
    previous_normal: Vec3,
    previous_velocity: Vec3,
    correction: f32,
    force: Vec3,
    torque: Vec3,
}

impl Default for Chassis {
    fn default() -> Self {
        Self { wheels: [Wheel::default(); 4], angular: Vec3::ZERO,
            normal: Vec3::Z, previous_velocity: Vec3::ZERO, correction: 0.0,
            previous_normal: Vec3::Z, force: Vec3::ZERO, torque: Vec3::ZERO }
    }
}

pub fn rotation(yaw: f32, pitch: f32, roll: f32) -> Quat {
    Quat::from_rotation_z(yaw) * Quat::from_rotation_x(pitch) * Quat::from_rotation_y(roll)
}

/// 007a34c0 / 0056e7b0: wheel queries/targets use the support frame, independently
/// of the sprung body's lean. Project the body's forward axis onto that plane. [game]
pub fn support_rotation(body: Quat, normal: Vec3) -> Quat {
    if normal.length_squared()<0.0001 {return body;}
    let up=normal.normalize_or_zero();
    let forward=body*Vec3::Y;
    let forward=if forward.dot(up).abs()>0.96 {up.cross(body*Vec3::X)}
        else {forward-up*forward.dot(up)};
    let forward=forward.normalize_or_zero();
    if forward==Vec3::ZERO {return body;}
    Quat::from_mat3(&Mat3::from_cols(forward.cross(up),forward,up)).normalize()
}

/// 006ddf30: one normalized first-order quaternion step, using world omega.
/// This is the conjugate of Lux's row-vector q + q*storedOmega*dt/2. [game]
pub fn integrate_rotation(body: Quat, angular: Vec3, dt: f32) -> Quat {
    let omega=Quat::from_xyzw(angular.x,angular.y,angular.z,0.0);
    (body+(omega*body)*(dt*0.5)).normalize()
}

/// 006de390's actual column-convention order is R^T * diag(invI) * R.
/// Preserve that order: conventional R*diag*R^T fails the captured omega.
/// Raw390..506 and recorded force replay verify this engine rule. [game/trace]
pub fn angular_acceleration(body: Quat, torque: Vec3, inverse_inertia: Vec3) -> Vec3 {
    body.inverse()*(body*torque*inverse_inertia)
}

pub fn mounts(t: &DriveTuning) -> [Vec3; 4] {
    let y = t.long_axle_length * 0.5;
    [Vec3::new(-t.front_axle_length*0.5,y,0.0), Vec3::new(t.front_axle_length*0.5,y,0.0),
     Vec3::new(-t.rear_axle_length*0.5,-y,0.0), Vec3::new(t.rear_axle_length*0.5,-y,0.0)]
}

/// Inertia of the avatar cuboid; matches run1 (16670,5417,18750). [data/trace]
pub fn inertia(t: &DriveTuning) -> Vec3 {
    let h = t.body_half;
    VEHICLE_MASS / 3.0 * Vec3::new(h.y*h.y+h.z*h.z, h.x*h.x+h.z*h.z, h.x*h.x+h.y*h.y)
}

/// Spring force/dead zone/clamp read in 006da400. [game]
pub fn spring_force(displacement: Vec3, k: f32) -> Vec3 {
    let squared = displacement.length_squared();
    if squared <= 0.001 { Vec3::ZERO }
    else { displacement / squared.sqrt().max(1.0) * k }
}

/// 006da400 normalises with the *unclamped* distance after weighting the force
/// displacement by split/full dt. The damping projection uses that weighted axis
/// twice, so splitting does not multiply damping by the number of pieces. [game]
pub fn spring_damping_axis(displacement: Vec3, fraction: f32) -> Vec3 {
    let squared=displacement.length_squared();
    if squared<=0.001 {Vec3::ZERO}
    else {let length=squared.sqrt();displacement*(fraction/(length*length.max(1.0)))}
}

impl Chassis {
    pub fn grounded(&self) -> bool { self.wheels.iter().any(|w| w.grounded) }

    /// 007a34c0 updates the character frame only when the unnormalised previous
    /// and current support normals have dot < .98. Ordinary sprung lean survives.
    /// Preserve the holder centre while removing its avatar offset. [game/trace]
    pub fn prepare_frame(&self, root: &mut Vec3, angles: &mut Vec3, t: &DriveTuning) {
        if self.previous_normal.dot(self.normal)>=0.98 {return;}
        let body=rotation(angles.z,angles.x,angles.y);
        let centre=*root+body*Vec3::Z*t.body_offset_z;
        let support=support_rotation(body,self.normal);
        let (yaw,pitch,roll)=support.to_euler(EulerRot::ZXY);
        *angles=Vec3::new(pitch,roll,yaw);
        *root=centre-support*Vec3::Z*t.body_offset_z;
    }

    /// Wheel ray and collider placement (007a1460,007a2cd0), before tyre forces. [game]
    pub fn probe(&mut self, root: Vec3, velocity: Vec3, turn: Quat, height: f32,
        t: &DriveTuning, world: &dyn World, dt: f32) {
        let centre = root + turn * Vec3::Z * t.body_offset_z;
        let support=support_rotation(turn,self.normal);
        for (w, mount) in self.wheels.iter_mut().zip(mounts(t)) {
            if w.rest_length <= 0.0 { w.rest_length = t.suspension_length; }
            w.was_grounded = w.grounded;
            w.previous_length = w.length;
            w.grounded = false;
            w.surface = 0;
            let origin = centre + support * mount + self.normal * height;
            w.probe_distance=100.0;
            if let Some(hit) = world.wheel_ray(origin, origin-self.normal*15.0) {
                let distance = (hit.point-origin).length();
                let extension = distance - 2.0*height;
                w.probe_distance=(extension-w.rest_length).clamp(0.0,100.0);
                let reach = w.rest_length*1.5 - (velocity.z*dt*2.0).min(0.0);
                if hit.normal.z > 0.1 && distance > 0.001 && extension <= reach {
                    w.grounded = true;
                    w.point = hit.point;
                    w.normal = hit.normal;
                    w.surface = hit.surface;
                    w.length = w.rest_length-extension;
                }
            }
            if !w.grounded { w.length += (w.rest_length-w.length)*(dt*10.0).min(1.0); }
            // The holder is temporarily aligned while placing the collider, then
            // 007a3b00 restores the character's body frame before spring_apply.
            // Keep the target fixed even when support correction moves the body. [game]
            let anchor=mount-Vec3::Z*height;
            w.target = centre+support*anchor+self.normal*w.length;
        }
    }

    fn support_normal(&mut self, dt: f32) {
        self.previous_normal=self.normal;
        let Some((target,_,rate))=self.support_plane() else {
            self.normal=self.normal.lerp(Vec3::Z,dt.min(1.0));return;
        };
        self.normal=self.normal.lerp(target,(dt*rate).min(1.0));
    }

    fn support_plane(&self) -> Option<(Vec3,Vec3,f32)> {
        let mut grounded = self.wheels.iter().filter(|w| w.grounded).collect::<Vec<_>>();
        let (target, rate) = if grounded.len() >= 3 {
            if grounded.len()==4 {
                let lowest = grounded.iter().enumerate().min_by(|(_,a),(_,b)| a.point.z.total_cmp(&b.point.z)).unwrap().0;
                grounded[lowest]=grounded[3];
                grounded.truncate(3);
            }
            let a=grounded[0]; let b=grounded[1]; let c=grounded[2];
            let mut n=(b.point-a.point).cross(c.point-b.point).normalize_or_zero();
            if n.dot(a.normal)<0.0 {n=-n;}
            (if n==Vec3::ZERO {a.normal} else {n}, 2.0)
        } else if let Some(w)=grounded.first() { (w.normal,10.0) }
        else {return None;};
        Some((target,grounded[0].point,rate))
    }

    /// 007a30c0's weight transfer adjusts each wheel's rest length. [game]
    pub fn transfer_weight(&mut self, velocity: Vec3, turn: Quat, t: &DriveTuning, stopped: bool,
        reversing: bool, gear_progress: f32) {
        if stopped {return;}
        let velocity=Vec3::new(velocity.x,velocity.y,0.0);
        let delta=velocity-self.previous_velocity;
        let magnitude=delta.length();
        let offset=t.suspension_length*(1.0-t.suspension_wt_scale);
        if velocity.length() >= t.max_speed*0.95 && magnitude<0.1 {
            // 007a3212..32b6: gear rev progress, reversed axle order while reversing.
            let sign=if reversing {-1.0} else {1.0};
            for (i,w) in self.wheels.iter_mut().enumerate() {
                w.rest_length=t.suspension_length + if i<2 {sign} else {-sign} * gear_progress*offset*0.3;
            }
        } else {
            let local=turn.inverse()*delta;
            for (w,mount) in self.wheels.iter_mut().zip(mounts(t)) {
                w.rest_length=t.suspension_length+mount.dot(local).signum()*offset*magnitude.clamp(0.0,1.0);
            }
        }
        self.previous_velocity=velocity;
    }

    pub fn render_step(&mut self, velocity: Vec3, turn: Quat, waiting: bool, turbo: bool, dt: f32) {
        // The visual's axial travel is used directly as radians (0056fb50), without
        // division by renderWheelRadius. Physical spin continues to use the tyre radius.
        let distance=velocity.dot(turn*Vec3::Y)*dt;
        let amount=if waiting {0.0} else if distance>0.0 {distance.clamp(0.2,if turbo {0.55} else {0.375})}
            else {distance.max(-0.3)};
        for w in &mut self.wheels {w.render_spin-=amount;}
    }

    /// 008601d0: grounded model node shifted along body up, within the data's limits. [game]
    pub fn render_offset(wheel: &Wheel, node_z: f32, up_z: f32, t: &DriveTuning) -> f32 {
        if !wheel.grounded {return 0.0;}
        (node_z-t.render_wheel_radius*up_z-wheel.point.z)
            .clamp(t.min_render_wheel_offset,t.max_render_wheel_offset)
    }

    /// Angular damping/limits run once per DriveMode update (00862000). [game]
    pub fn damp_angular(&mut self, t: &DriveTuning) {
        self.angular.x=(self.angular.x*t.angular_vel_damp_x).clamp(-t.max_angular_vel_x,t.max_angular_vel_x);
        self.angular.y=(self.angular.y*t.angular_vel_damp_y).clamp(-t.max_angular_vel_y,t.max_angular_vel_y);
        self.angular.z=self.angular.z.clamp(-t.max_angular_vel_z,t.max_angular_vel_z);
    }

    /// 006de600 accumulates tyre forces; they take effect after the drive limits
    /// and weight transfer, together with the suspension and gravity. [game]
    pub fn add_force(&mut self, force: Vec3, arm: Vec3) {
        self.force+=force;
        self.torque+=arm.cross(force);
    }

    /// DriveMode's post-step precedes the world's force/pose integration. [game]
    pub fn finish_step(&mut self, root: &mut Vec3, velocity: &mut Vec3, dt: f32) {
        self.finish_support(root,velocity,dt);
        self.support_normal(dt);
    }

    /// Integrates springs, gravity and static convex contacts. Synchronous arena rays and
    /// inelastic normal impulses are [assumed]; see notes/status.md vehicle assumptions.
    pub fn advance(&mut self, root: &mut Vec3, velocity: &mut Vec3, angles: &mut Vec3,
        height: f32, gravity: f32, t: &DriveTuning, world: &dyn World, dt: f32) -> Vec<Contact> {
        let mut contacts=Vec::new();
        let inv_inertia=inertia(t).recip();
        let turn=rotation(angles.z,angles.x,angles.y);
        let mut centre=*root+turn*Vec3::Z*t.body_offset_z;
        let mut force=std::mem::take(&mut self.force)+Vec3::NEG_Z*VEHICLE_MASS*gravity;
        let mut torque=std::mem::take(&mut self.torque);
        let mut remaining=dt;
        // 006da400 splits only force accumulation/linear damping. No velocity,
        // position or orientation integration occurs between those pieces. [game]
        while remaining>1e-6 {
            let h=remaining.min(1.0/60.0);
            remaining-=h;
            for (w,mount) in self.wheels.iter().zip(mounts(t)).filter(|(w,_)|w.grounded) {
                let arm=turn*(mount-Vec3::Z*height);
                let displacement=w.target-(centre+arm);
                let f=spring_force(displacement,t.suspension_k)*(h/dt);
                force+=f;
                torque+=arm.cross(f);
                if displacement.length_squared()>0.001 {
                    let axis=spring_damping_axis(displacement,h/dt);
                    *velocity-=axis*velocity.dot(axis)*t.suspension_damp/60.0;
                }
            }
        }
        // 006e96c0 uses body +2c/+30 before force integration. Both are .9995
        // in every captured Bumblebee body (6,869 stable frames). [game/trace]
        *velocity*=0.9995;
        self.angular*=0.9995;
        let linear_squared=velocity.length_squared();
        let angular_squared=self.angular.length_squared();
        if linear_squared<2.5e-5 || linear_squared>100000.0 {*velocity=Vec3::ZERO;}
        if angular_squared<1e-8 || angular_squared>100000.0 {self.angular=Vec3::ZERO;}
        *velocity+=force/VEHICLE_MASS*dt;
        self.angular+=angular_acceleration(turn,torque,inv_inertia)*dt;
        // The drive angular limits have already run; never clip spring response
        // here. 006ddf30 integrates its resulting world angular velocity. [game]
        let mut move_left=dt;
        for _ in 0..8 {
            let travel=*velocity*move_left;
            let Some(mut contact)=world.vehicle_sweep(centre,turn,t.body_half,travel) else {
                centre+=travel; break;
            };
            centre+=travel*contact.fraction+contact.normal*(contact.depth+1e-5);
            let arm=contact.point-centre;
            contact.velocity=*velocity+self.angular.cross(arm);
            let speed=contact.velocity.dot(contact.normal);
            if speed<0.0 {
                let response=angular_acceleration(turn,arm.cross(contact.normal),inv_inertia);
                let denominator=1.0/VEHICLE_MASS + contact.normal.dot(response.cross(arm));
                let impulse=-speed/denominator;
                *velocity+=contact.normal*impulse/VEHICLE_MASS;
                self.angular+=response*impulse;
            }
            contacts.push(contact);
            move_left*=1.0-contact.fraction;
            if move_left<1e-6 {break;}
        }
        let next=integrate_rotation(turn,self.angular,dt);
        let (yaw,pitch,roll)=next.to_euler(EulerRot::ZXY);
        *angles=Vec3::new(pitch,roll,yaw);
        *root=centre-next*Vec3::Z*t.body_offset_z;
        contacts
    }

    /// 007a3b00's predictive support-plane correction; ramp departure keeps velocity. [game]
    fn finish_support(&mut self, root: &mut Vec3, velocity: &mut Vec3, dt: f32) {
        let Some((n,point,_))=self.support_plane() else {self.correction=0.0;return;};
        let distance=n.dot(*root-point);
        let speed=velocity.dot(n);
        if distance+speed*dt<0.0 {
            let penetration=distance.min(0.0);
            let target=if speed < -5.0 {*velocity-=n*speed*0.75;penetration} else {penetration*0.4};
            self.correction+=(target-self.correction)*(dt*10.0).min(1.0);
            *root-=n*self.correction;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn support_frame_keeps_heading_and_removes_body_lean() {
        let body=rotation(1.2,0.15,-0.25);
        let normal=Vec3::new(0.15,-0.2,1.0).normalize();
        let support=support_rotation(body,normal);
        assert!((support*Vec3::Z).abs_diff_eq(normal,1e-6));
        let forward=body*Vec3::Y;
        assert!((support*Vec3::Y).abs_diff_eq((forward-normal*forward.dot(normal)).normalize(),1e-6));
        assert!((support_rotation(body,Vec3::Z)*Vec3::X).z.abs()<1e-6);
    }
    #[test]
    fn terrain_frame_reset_preserves_centre_and_ordinary_spring_lean() {
        let mut c=Chassis::default();let mut t=tuning();t.body_offset_z=1.6;
        let mut root=Vec3::new(3.0,5.0,2.0);let mut angles=Vec3::new(0.13,-0.17,0.7);
        let before=root;let before_angles=angles;
        c.prepare_frame(&mut root,&mut angles,&t);
        assert_eq!(root,before);assert_eq!(angles,before_angles);
        let centre=root+rotation(angles.z,angles.x,angles.y)*Vec3::Z*t.body_offset_z;
        c.normal=Vec3::new(0.2,0.0,0.96);
        c.prepare_frame(&mut root,&mut angles,&t);
        let next=rotation(angles.z,angles.x,angles.y);
        assert!((next*Vec3::Z).abs_diff_eq(c.normal.normalize(),1e-6));
        assert!((root+next*Vec3::Z*t.body_offset_z).abs_diff_eq(centre,1e-6));
    }
    #[test]
    fn independent_ground_targets_restore_a_leaning_body() {
        let mut c=Chassis::default();let mut t=tuning();t.body_offset_z=1.6;
        let turn=rotation(0.0,0.0,0.2);
        c.probe(Vec3::ZERO,Vec3::ZERO,turn,1.0,&t,&crate::sim::Arena::default(),0.032);
        // Queries remain level. Actual body anchors pull toward those targets in 3D.
        assert!((c.wheels[0].length-c.wheels[1].length).abs()<1e-6);
        let torque=c.wheels.iter().zip(mounts(&t)).map(|(w,m)| {
            let arm=turn*(m-Vec3::Z);
            arm.cross(spring_force(w.target-(turn*Vec3::Z*t.body_offset_z+arm),t.suspension_k))
        }).sum::<Vec3>();
        assert!(torque.y<0.0,"{torque:?}");
    }
    #[test]
    fn spring_uses_original_dead_zone_and_displacement_clamp() {
        assert_eq!(spring_force(Vec3::Z*0.03,100000.0),Vec3::ZERO);
        assert_eq!(spring_force(Vec3::Z*0.5,100000.0),Vec3::Z*50000.0);
        assert_eq!(spring_force(Vec3::Z*5.0,100000.0),Vec3::Z*100000.0);
    }
    #[test]
    fn split_spring_damping_uses_original_weighted_axis() {
        assert_eq!(spring_damping_axis(Vec3::Z*0.5,0.5),Vec3::Z*0.5);
        assert_eq!(spring_damping_axis(Vec3::Z*2.0,0.5),Vec3::Z*0.25);
        assert_eq!(spring_damping_axis(Vec3::Z*0.03,1.0),Vec3::ZERO);
    }
    #[test]
    fn avatar_inertia_matches_recorded_body() {
        let t=DriveTuning {body_half:Vec3::new(1.5,3.0,1.0),..Default::default()};
        assert!(inertia(&t).abs_diff_eq(Vec3::new(16666.666,5416.6665,18750.0),0.002));
    }
    fn tuning()->DriveTuning {DriveTuning {body_half:Vec3::ONE,body_offset_z:0.0,
        suspension_length:1.0,front_axle_length:2.0,rear_axle_length:2.0,long_axle_length:4.0,
        suspension_k:100000.0,suspension_damp:0.5,max_angular_vel_x:4.0,max_angular_vel_y:4.0,
        max_angular_vel_z:4.0,..Default::default()}}
    #[test]
    fn independent_wheels_leave_the_platform_in_axle_order() {
        let mut chassis=Chassis::default();let mut t=tuning();t.body_offset_z=1.6;
        let world=crate::sim::Arena {boxes:vec![crate::sim::Box3 {
            min:Vec3::new(-4.0,-5.0,0.0),max:Vec3::new(4.0,0.0,4.0)}],..Default::default()};
        chassis.probe(Vec3::new(0.0,-1.0,4.0),Vec3::Y*20.0,Quat::IDENTITY,1.0,&t,&world,0.032);
        assert_eq!(chassis.wheels.map(|w|w.grounded),[false,false,true,true]);
        assert_eq!(chassis.wheels[2].point.z,4.0);
        chassis.probe(Vec3::new(0.0,3.0,4.0),Vec3::Y*20.0,Quat::IDENTITY,1.0,&t,&world,0.032);
        assert_eq!(chassis.wheels.map(|w|w.grounded),[false;4]);
    }
    #[test]
    fn fast_glancing_collision_preserves_wall_tangent_and_cannot_tunnel() {
        let mut chassis=Chassis::default();let t=tuning();
        let world=crate::sim::Arena {boxes:vec![crate::sim::Box3 {
            min:Vec3::new(5.0,-20.0,0.0),max:Vec3::new(5.01,20.0,20.0)}],..Default::default()};
        let mut root=Vec3::Z*10.0;let mut velocity=Vec3::new(300.0,20.0,0.0);let mut angles=Vec3::ZERO;
        let contacts=chassis.advance(&mut root,&mut velocity,&mut angles,1.0,0.0,&t,&world,0.1);
        assert!(root.x<=4.001,"{root:?}");assert!((root.y-20.0*0.9995*0.1).abs()<0.001,"{root:?}");
        assert!((velocity.y-20.0*0.9995).abs()<0.001);assert!(velocity.x.abs()<0.001);
        assert!(contacts.iter().any(|c|c.normal==Vec3::NEG_X));
    }

    #[test]
    fn original_recorded_body_rotations_replay_in_world_axes() {
        let capture=std::env::var("TF2_ROTATION_CAPTURE").ok()
            .map(|path|std::fs::read_to_string(path).expect("rotation capture fixture"));
        let fixture=capture.as_deref().unwrap_or(include_str!("../fixtures/vehicle_20261007_rotation.csv"));
        let mut count=0;
        for line in fixture.lines() {
            let v=line.split(',').map(|s|s.parse::<f32>().unwrap()).collect::<Vec<_>>();
            let start=Quat::from_slice(&v[2..6]);
            let angular=Vec3::from_slice(&v[6..9]);
            let expected=Quat::from_slice(&v[9..13]);
            let got=integrate_rotation(start,angular,v[1]);
            // Small matrix error avoids acos losing resolution near dot == 1.
            let error=(Mat3::from_quat(got)-Mat3::from_quat(expected)).to_cols_array()
                .into_iter().map(|v|v*v).sum::<f32>().sqrt();
            assert!(error<0.00005,"recorded t {}: matrix error {error}",v[0]);
            count+=1;
        }
        assert!(count>=100);
        eprintln!("{count} recorded rotation cases passed matrix tolerance0.00005");
    }

    #[test]
    fn original_recorded_spring_force_updates_replay() {
        // Each case seeds the measured pre-physics body/velocity, fixed collider
        // targets and queued tyre forces. The original arena/contact solver is
        // not replayed. Phase-crossing/contact outliers remain in these cases.
        struct Empty;
        impl World for Empty {
            fn floor(&self,_:f32,_:f32,_:f32)->f32 {-1e6}
            fn push_out(&self,_:&mut Vec3,_:f32,_:f32)->bool {false}
        }
        let t=DriveTuning {body_half:Vec3::new(1.5,3.0,1.0),body_offset_z:1.6,
            front_axle_length:2.0,rear_axle_length:2.0,long_axle_length:5.0,
            suspension_k:100000.0,suspension_damp:0.5,..Default::default()};
        let mut linear_errors=Vec::new();let mut angular_errors=Vec::new();
        let capture=std::env::var("TF2_SPRING_CAPTURE").ok()
            .map(|path|std::fs::read_to_string(path).expect("spring capture fixture"));
        let fixture=capture.as_deref().unwrap_or(include_str!("../fixtures/vehicle_20261007_springs.csv"));
        for line in fixture.lines() {
            let v=line.split(',').map(|s|s.parse::<f32>().unwrap()).collect::<Vec<_>>();
            let turn=Quat::from_slice(&v[2..6]);
            let (yaw,pitch,roll)=turn.to_euler(EulerRot::ZXY);
            let mut angles=Vec3::new(pitch,roll,yaw);
            let mut root=Vec3::from_slice(&v[6..9]);
            let mut velocity=Vec3::from_slice(&v[9..12]);
            let mut c=Chassis {angular:Vec3::from_slice(&v[12..15]),
                force:Vec3::from_slice(&v[15..18]),torque:Vec3::from_slice(&v[18..21]),..Default::default()};
            for (i,w) in c.wheels.iter_mut().enumerate() {
                w.grounded=true;w.target=Vec3::from_slice(&v[21+i*3..24+i*3]);
            }
            c.advance(&mut root,&mut velocity,&mut angles,1.0,35.0,&t,&Empty,v[1]);
            linear_errors.push((velocity-Vec3::from_slice(&v[33..36])).length());
            angular_errors.push((c.angular-Vec3::from_slice(&v[36..39])).length());
        }
        linear_errors.sort_by(f32::total_cmp);angular_errors.sort_by(f32::total_cmp);
        let n=linear_errors.len();assert!(n>=100);
        let median=n/2;let p95=(n-1)*95/100;
        eprintln!("{n} spring cases: velocity error median{}, p95{}, max{} m/s; angular error median{}, p95{}, max{} rad/s",
            linear_errors[median],linear_errors[p95],linear_errors[n-1],
            angular_errors[median],angular_errors[p95],angular_errors[n-1]);
        assert!(linear_errors[median]<0.0005 && linear_errors[p95]<0.1,
            "spring velocity median {}, p95 {}",linear_errors[median],linear_errors[p95]);
        assert!(angular_errors[median]<0.0002 && angular_errors[p95]<0.0005,
            "spring omega median {}, p95 {}",angular_errors[median],angular_errors[p95]);
    }

    #[test]
    fn spring_splits_accumulate_full_force_before_one_body_step() {
        let mut c=Chassis::default();let mut t=tuning();t.body_offset_z=1.6;
        t.max_angular_vel_y=0.01;
        let world=crate::sim::Arena::default();
        let mut root=Vec3::ZERO;let turn=rotation(0.7,0.0,0.2);
        c.probe(root,Vec3::ZERO,turn,1.0,&t,&world,0.032);
        let centre=root+turn*Vec3::Z*t.body_offset_z;
        let mut force=Vec3::ZERO;let mut torque=Vec3::ZERO;
        for (w,mount) in c.wheels.iter().zip(mounts(&t)) {
            let arm=turn*(mount-Vec3::Z);
            let f=spring_force(w.target-centre-arm,t.suspension_k);
            force+=f;torque+=arm.cross(f);
        }
        let mut velocity=Vec3::ZERO;let mut angles=Vec3::new(0.0,0.2,0.7);
        c.advance(&mut root,&mut velocity,&mut angles,1.0,0.0,&t,&world,0.032);
        assert!(velocity.abs_diff_eq(force/VEHICLE_MASS*0.032,0.00002));
        assert!(c.angular.abs_diff_eq(angular_acceleration(turn,torque,inertia(&t).recip())*0.032,0.00002));
        assert!(velocity.truncate().length()>0.1,"3D spring translation was discarded");
        assert!(c.angular.y.abs()>t.max_angular_vel_y,"spring response was clamped after accumulation");
        let next=integrate_rotation(turn,c.angular,0.032);
        assert!((root+next*Vec3::Z*t.body_offset_z).abs_diff_eq(centre+velocity*0.032,0.00001));
    }
    #[test]
    fn visual_spin_uses_original_limits_including_reverse_and_wait() {
        let mut c=Chassis::default();
        c.render_step(Vec3::Y*100.0,Quat::IDENTITY,false,false,0.032);
        assert_eq!(c.wheels[0].render_spin,-0.375);
        c.render_step(Vec3::Y*100.0,Quat::IDENTITY,false,true,0.032);
        assert!((c.wheels[0].render_spin+0.925).abs()<1e-6);
        c.render_step(Vec3::NEG_Y*100.0,Quat::IDENTITY,false,false,0.032);
        assert!((c.wheels[0].render_spin+0.625).abs()<1e-6);
        c.render_step(Vec3::Y,Quat::IDENTITY,true,false,0.032);
        assert!((c.wheels[0].render_spin+0.625).abs()<1e-6);
    }
}
