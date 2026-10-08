//! [stand-in] Authored test-arena slopes, not geometry or physics from an original level.
use glam::{Vec2, Vec3};

/// A solid rectangular wedge: a horizontal base and a planar top.
/// `height` is the top at `min`; `slope` is its rise per metre in x/y.
#[derive(Clone, Copy, Debug)]
pub struct Ramp {
    pub min: Vec2,
    pub max: Vec2,
    pub base: f32,
    pub height: f32,
    pub slope: Vec2,
}

impl Ramp {
    pub fn contains(&self, p: Vec2) -> bool {
        p.cmpge(self.min).all() && p.cmple(self.max).all()
    }

    pub fn top(&self, p: Vec2) -> f32 {
        self.height + self.slope.dot(p - self.min)
    }

    pub fn normal(&self) -> Vec3 {
        Vec3::new(-self.slope.x, -self.slope.y, 1.0).normalize()
    }

    /// Exterior half spaces, `normal.dot(point) <= distance` inside the wedge.
    pub fn planes(&self) -> [(Vec3, f32); 6] {
        let n = self.normal();
        [
            (n, n.dot(self.min.extend(self.height))),
            (-Vec3::X, -self.min.x),
            (Vec3::X, self.max.x),
            (-Vec3::Y, -self.min.y),
            (Vec3::Y, self.max.y),
            (-Vec3::Z, -self.base),
        ]
    }

    /// First exterior face crossed by a segment; starts inside have no entrance face.
    pub fn ray(&self, from: Vec3, to: Vec3) -> Option<(f32, Vec3)> {
        let (mut enter, mut leave, mut normal) = (0.0_f32, 1.0_f32, Vec3::ZERO);
        for (n, distance) in self.planes() {
            let outside = n.dot(from) - distance;
            let rate = n.dot(to - from);
            if rate.abs() < 1e-9 {
                if outside > 0.0 {
                    return None;
                }
            } else {
                let at = -outside / rate;
                if rate < 0.0 {
                    if at > enter {
                        enter = at;
                        normal = n;
                    }
                } else {
                    leave = leave.min(at);
                }
                if enter > leave {
                    return None;
                }
            }
        }
        (normal != Vec3::ZERO).then_some((enter, normal))
    }

    /// [stand-in] Alternating projections, as in the arena's primitive contact solver.
    fn nearest(&self, mut point: Vec3) -> Vec3 {
        for _ in 0..32 {
            let before = point;
            for (n, distance) in self.planes() {
                point -= n * (n.dot(point) - distance).max(0.0);
            }
            if point.distance_squared(before) < 1e-10 {
                break;
            }
        }
        point
    }

    pub fn contact(&self, body: &crate::melee::Body) -> Option<(Vec3, Vec3)> {
        let mut point = self.nearest(body.at.translation.into());
        for _ in 0..32 {
            let on_body = body.nearest(point);
            let next = self.nearest(on_body);
            if on_body.distance_squared(next) <= 0.01 * 0.01 {
                let (n, distance) = self.planes().into_iter().min_by(|(a, da), (b, db)| {
                    (da - a.dot(next))
                        .abs()
                        .total_cmp(&(db - b.dot(next)).abs())
                })?;
                return Some((next + n * (distance - n.dot(next)), n));
            }
            if point.distance_squared(next) < 1e-10 {
                break;
            }
            point = next;
        }
        None
    }
}

/// Continuous SAT for an oriented avatar cuboid translating against a static convex
/// solid. Geometry is exact for the arena's wedges/boxes; Lux contact impulses are
/// supplied separately by vehicle::Chassis (notes/status.md). [assumed]
pub fn sweep_cuboid(centre: Vec3, turn: glam::Quat, half: Vec3, travel: Vec3,
    vertices: &[Vec3], normals: &[Vec3], edges: &[Vec3]) -> Option<crate::vehicle::Contact> {
    let body_axes=[turn*Vec3::X,turn*Vec3::Y,turn*Vec3::Z];
    let mut axes=body_axes.to_vec();
    axes.extend_from_slice(normals);
    for a in body_axes { for &b in edges {
        if let Some(n)=a.cross(b).try_normalize() {axes.push(n);}
    }}
    let (mut enter,mut leave,mut entry_normal)=(0.0_f32,1.0_f32,Vec3::ZERO);
    let (mut depth,mut overlap_normal)=(f32::INFINITY,Vec3::ZERO);
    let mut overlapping=true;
    for axis in axes {
        let radius=half.x*axis.dot(body_axes[0]).abs()+half.y*axis.dot(body_axes[1]).abs()+half.z*axis.dot(body_axes[2]).abs();
        let projected=axis.dot(centre);
        let lo=vertices.iter().map(|v|axis.dot(*v)).fold(f32::INFINITY,f32::min);
        let hi=vertices.iter().map(|v|axis.dot(*v)).fold(f32::NEG_INFINITY,f32::max);
        let negative=projected+radius-lo;
        let positive=hi-(projected-radius);
        overlapping &= negative>1e-6 && positive>1e-6;
        let (penetration,n)=if negative<positive {(negative,-axis)} else {(positive,axis)};
        if penetration<depth {depth=penetration;overlap_normal=n;}
        let speed=axis.dot(travel);
        if speed.abs()<1e-9 {
            if negative<0.0 || positive<0.0 {return None;}
        } else {
            let a=(lo-radius-projected)/speed;
            let b=(hi+radius-projected)/speed;
            let (start,end,n)=if a<b {(a,b,-axis)} else {(b,a,axis)};
            if start>enter || (start==enter && entry_normal==Vec3::ZERO) {enter=start;entry_normal=n;}
            leave=leave.min(end);
            if enter>leave {return None;}
        }
    }
    let (fraction,normal,depth)=if overlapping {(0.0,overlap_normal,depth)}
        else if entry_normal!=Vec3::ZERO && entry_normal.dot(travel)<0.0 && enter<=1.0 && leave>=0.0 {(enter,entry_normal,0.0)}
        else {return None;};
    // Face centre, projected onto the contacted solid face. Keeps a centred wall hit
    // from acquiring a corner's artificial moment arm. Contact manifolds remain assumed.
    let radius=half.x*normal.dot(body_axes[0]).abs()+half.y*normal.dot(body_axes[1]).abs()+half.z*normal.dot(body_axes[2]).abs();
    Some(crate::vehicle::Contact {key:0,fraction,point:centre+travel*fraction-normal*radius,
        normal,depth,surface:crate::melee::surface::DEFAULT,velocity:Vec3::ZERO})
}

impl Ramp {
    pub fn vehicle_sweep(&self, centre: Vec3, turn: glam::Quat, half: Vec3, travel: Vec3) -> Option<crate::vehicle::Contact> {
        let corners=[self.min,Vec2::new(self.max.x,self.min.y),self.max,Vec2::new(self.min.x,self.max.y)];
        let vertices=corners.map(|p|p.extend(self.base)).into_iter()
            .chain(corners.map(|p|p.extend(self.top(p)))).collect::<Vec<_>>();
        let normals=self.planes().map(|(n,_)|n);
        let edges=[Vec3::X,Vec3::Y,Vec3::Z,Vec3::new(1.0,0.0,self.slope.x),Vec3::new(0.0,1.0,self.slope.y)];
        sweep_cuboid(centre,turn,half,travel,&vertices,&normals,&edges)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ramp() -> Ramp {
        Ramp {
            min: Vec2::ZERO,
            max: Vec2::new(10.0, 20.0),
            base: 0.0,
            height: 0.0,
            slope: Vec2::new(0.0, 0.5),
        }
    }

    #[test]
    fn rays_hit_the_slope_and_high_wall_but_clear_the_empty_space_above() {
        let r = ramp();
        let (at, normal) = r
            .ray(Vec3::new(5.0, 10.0, 12.0), Vec3::new(5.0, 10.0, 0.0))
            .unwrap();
        assert!((at - 7.0 / 12.0).abs() < 1e-6);
        assert!(normal.abs_diff_eq(Vec3::new(0.0, -0.5, 1.0).normalize(), 1e-6));
        let (_, n) = r
            .ray(Vec3::new(5.0, 25.0, 5.0), Vec3::new(5.0, 15.0, 5.0))
            .unwrap();
        assert_eq!(n, Vec3::Y);
        assert!(
            r.ray(Vec3::new(-5.0, 2.0, 4.0), Vec3::new(15.0, 2.0, 4.0))
                .is_none()
        );
        assert!(
            r.ray(Vec3::new(5.0, 10.0, 2.0), Vec3::new(5.0, 10.0, 20.0))
                .is_none()
        );
    }

    #[test]
    fn damage_contacts_use_the_sloped_surface() {
        let r = ramp();
        let body = crate::melee::Body {
            shape: crate::formats::scene::Shape::Sphere { radius: 1.0 },
            at: glam::Affine3A::from_translation(Vec3::new(5.0, 10.0, 5.8)),
        };
        let (point, normal) = r.contact(&body).unwrap();
        assert!((point.z - r.top(point.truncate())).abs() < 1e-5);
        assert_eq!(normal, r.normal());
        let high = crate::melee::Body {
            at: glam::Affine3A::from_translation(Vec3::new(5.0, 10.0, 9.0)),
            ..body
        };
        assert!(r.contact(&high).is_none());
    }

    #[test]
    fn cuboid_sweeps_stop_at_thin_faces_and_ignore_separating_motion() {
        let vertices=(0..8).map(|i|Vec3::new(if i&1==0 {5.0} else {5.01},
            if i&2==0 {-5.0} else {5.0},if i&4==0 {-5.0} else {5.0})).collect::<Vec<_>>();
        let axes=[Vec3::X,Vec3::Y,Vec3::Z];
        let hit=sweep_cuboid(Vec3::ZERO,glam::Quat::IDENTITY,Vec3::ONE,
            Vec3::X*100.0,&vertices,&axes,&axes).unwrap();
        assert!((hit.fraction-0.04).abs()<1e-6);assert_eq!(hit.normal,Vec3::NEG_X);
        assert!(sweep_cuboid(Vec3::X*4.0,glam::Quat::IDENTITY,Vec3::ONE,
            Vec3::NEG_X,&vertices,&axes,&axes).is_none());
        assert!(sweep_cuboid(Vec3::ZERO,glam::Quat::from_rotation_z(0.7),Vec3::ONE,
            Vec3::X*100.0,&vertices,&axes,&axes).is_some());
    }
}
