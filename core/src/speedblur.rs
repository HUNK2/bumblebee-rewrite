//! Turbo's masked depth-of-field pass, read from the owned game's shaders and code.
//! See notes/shaders.md "Turbo speed blur port". No renderer or bundled game assets.

use std::path::Path;

use crate::formats::{
    hash::{crc32, lower33},
    lxb::{DataFile, Node},
    pack::{self, Pack},
    texture::{self, Texture},
};

#[derive(Clone, Debug)]
pub struct Preset {
    pub blur_amount: f32,
    pub max_blur: f32,
    pub stretch: f32,
    pub blend_in: f32,
    pub blend_out: f32,
    pub mask_id: u32,
}

/// Read this level's named environment and its referenced texture from the install.
/// This implements the hard masked pass with zero focus distances (Bumblebee's turbo).
/// Reject other DOF settings instead of silently applying the wrong depth arithmetic.
pub fn load(game_dir: &Path, level: &str) -> Result<(Preset, Texture), String> {
    let pack = Pack::open(&game_dir.join("levels").join(level).join("zone.str"))?;
    let mut preset = None;
    for chunk in pack.of_type(pack::DATA) {
        let bytes = pack.data(chunk);
        if !bytes.windows(21).any(|w| w == b"Environment_SpeedBlur") {
            continue;
        }
        let file = DataFile::parse(bytes.to_vec())?;
        if let Some(root) = file.root().field(crc32(b"Environment_SpeedBlur")) {
            let num = |name: &str| -> Result<f32, String> {
                root.get(name)
                    .and_then(Node::float)
                    .filter(|v| v.is_finite())
                    .ok_or_else(|| format!("SpeedBlur: missing or invalid {name}"))
            };
            for name in [
                "dofNearFocus",
                "dofFarFocus",
                "dofNearFalloff",
                "dofFarFalloff",
            ] {
                if num(name)? != 0.0 {
                    return Err(format!("SpeedBlur: unsupported nonzero {name}"));
                }
            }
            // [data/game: 00567660, 00551670] This preset selects the hard kernel.
            if num("dofKernelTap")? != 1.0 {
                return Err("SpeedBlur: unsupported kernel".into());
            }
            let blur_amount = num("dofBlurAmount")?;
            if blur_amount <= 0.0 {
                return Err("SpeedBlur: invalid Gaussian deviation".into());
            }
            preset = Some(Preset {
                blur_amount,
                max_blur: num("dofMaxBlur")?,
                stretch: num("dofSpeedStretch")?,
                blend_in: num("blendDuration")?,
                blend_out: num("blendOutDuration")?,
                // [game: 00567660 at 00568265..00568340] Runtime lookup uses
                // dofMaskName's full source path, not the short editor dofMask name.
                mask_id: lower33(
                    &root
                        .get("dofMaskName")
                        .and_then(Node::text)
                        .ok_or("SpeedBlur: missing mask path")?,
                ),
            });
            break;
        }
    }
    let preset = preset.ok_or("No Environment_SpeedBlur in level")?;
    // Pack chunk ids differ from the texture header's runtime asset id.
    let mut mask = None;
    for chunk in pack.of_type(pack::TEXTURE) {
        if let Some(t) = texture::parse(pack.data(chunk))? {
            if t.id == preset.mask_id {
                mask = Some(t);
                break;
            }
        }
    }
    let mask = mask.ok_or("SpeedBlur: referenced mask texture missing")?;
    if mask.id != preset.mask_id || mask.faces != 1 {
        return Err("SpeedBlur: invalid mask texture".into());
    }
    Ok((preset, mask))
}

/// [game: 01177140] Truncate screen/4, then round upward to a multiple of 16.
pub fn intermediate_size(width: u32, height: u32) -> [u32; 2] {
    [width, height].map(|n| (n / 4).max(1).div_ceil(16) * 16)
}

/// [game: 005839a0] Thirteen samples in x-major order; offsets in source texels.
/// The common 1/sqrt(2*pi*sigma^2) factor cancels when the weights are normalized.
pub fn gaussian_taps(sigma: f32) -> [[f32; 4]; 13] {
    assert!(sigma.is_finite() && sigma > 0.0);
    let mut taps = [[0.0; 4]; 13];
    let mut at = 0;
    let mut sum = 0.0;
    for x in -2_i32..=2 {
        for y in -2_i32..=2 {
            if x.abs() + y.abs() >= 3 {
                continue;
            }
            let weight = (-(x * x + y * y) as f32 / (2.0 * sigma * sigma)).exp();
            taps[at] = [x as f32, y as f32, weight, 0.0];
            sum += weight;
            at += 1;
        }
    }
    for tap in &mut taps {
        tap[2] /= sum;
    }
    taps
}

/// [game: 00565c00, 00733290] Linear active-environment weight and reciprocal rate.
#[derive(Default)]
pub struct Blend {
    pub weight: f32,
    active: bool,
    rate: f32,
}
impl Blend {
    pub fn step(&mut self, active: bool, preset: &Preset, dt: f32) -> f32 {
        if active != self.active {
            self.active = active;
            self.rate = if active {
                1.0 / preset.blend_in.max(1e-5)
            } else {
                -1.0 / preset.blend_out.max(1e-5)
            };
        }
        self.weight = (self.weight + self.rate * dt.max(0.0)).clamp(0.0, 1.0);
        self.weight
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_speed_blur_resolves_its_own_mask_and_instant_blends() {
        let (p, mask) = load(Path::new(r"C:\Games2"), "us_west_town").unwrap();
        assert_eq!(
            (
                p.blur_amount,
                p.max_blur,
                p.stretch,
                p.blend_in,
                p.blend_out
            ),
            (1.0, 1.0, 0.98, 0.0, -1.0)
        );
        assert_eq!((mask.width, mask.height, mask.id), (256, 256, p.mask_id));
        assert_eq!(mask.format, texture::PixelFormat::Dxt1);
        let mut blend = Blend::default();
        assert_eq!(blend.step(false, &p, 0.032), 0.0);
        assert_eq!(blend.step(true, &p, 0.032), 1.0);
        assert_eq!(blend.step(false, &p, 0.032), 0.0);
        assert_eq!(blend.step(true, &p, 0.032), 1.0);
    }

    #[test]
    fn gaussian_matches_the_thirteen_original_offsets_and_weights() {
        let taps = gaussian_taps(1.0);
        assert_eq!(taps[0][..2], [-2.0, 0.0]);
        assert_eq!(taps[6][..2], [0.0, 0.0]);
        assert_eq!(taps[12][..2], [2.0, 0.0]);
        assert!((taps.iter().map(|t| t[2]).sum::<f32>() - 1.0).abs() < 1e-6);
        // Independent golden values: axis distance2, distance1 and centre, sigma1.
        assert!((taps[0][2] - 0.024882467).abs() < 1e-7);
        assert!((taps[5][2] - 0.11151548).abs() < 1e-7);
        assert!((taps[6][2] - 0.18385795).abs() < 1e-7);
        for (a, b) in taps.iter().zip(taps.iter().rev()) {
            assert_eq!(a[0], -b[0]);
            assert_eq!(a[1], -b[1]);
            assert_eq!(a[2], b[2]);
        }
    }

    #[test]
    fn target_size_matches_original_integer_division_then_alignment() {
        assert_eq!(intermediate_size(1280, 720), [320, 192]);
        assert_eq!(intermediate_size(1920, 1080), [480, 272]);
        assert_eq!(intermediate_size(1283, 723), [320, 192]);
        assert_eq!(intermediate_size(1284, 724), [336, 192]);
        assert_eq!(intermediate_size(1, 1), [16, 16]);
    }

    #[test]
    fn blend_reverses_from_current_weight_and_clamps_at_both_ends() {
        let p = Preset {
            blur_amount: 1.0,
            max_blur: 1.0,
            stretch: 0.98,
            blend_in: 0.5,
            blend_out: 0.25,
            mask_id: 0,
        };
        let mut b = Blend::default();
        assert!((b.step(true, &p, 0.125) - 0.25).abs() < 1e-6);
        assert!((b.step(false, &p, 0.03125) - 0.125).abs() < 1e-6);
        assert!((b.step(true, &p, 0.125) - 0.375).abs() < 1e-6);
        assert_eq!(b.step(true, &p, 1.0), 1.0);
        assert_eq!(b.step(false, &p, 1.0), 0.0);
    }
}
