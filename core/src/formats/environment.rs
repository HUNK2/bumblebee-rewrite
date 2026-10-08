//! A level's `Environment`: its light, fog, bloom, colour filter and exposure
//! (`notes\shaders.md` "The environment"). The class is `Environment` (factory
//! `FUN_005690a0`, loader `FUN_00567660`); a level's own is a root of one of its zone
//! pack's data chunks, with the flag `DEFAULT`, and the zone's first data chunk holds the
//! special ones every level has (`Environment_SpeedBlur`, `Environment_LowHeath`...).

use std::path::Path;

use glam::Vec3;

use super::lxb::{DataFile, Node};
use super::pack::{self, Pack};

/// The numbers are as the loader keeps them (what it scales is said on each).
#[derive(Clone, Debug, PartialEq)]
pub struct Environment {
    pub name: String,
    /// `ambient`, bytes over 255.
    pub ambient: Vec3,
    /// `shadowLight`: the way TO the sun (the light's z axis: its place over its
    /// distance from the middle of the level), its `colour` and `range`. [data]
    /// [assumed: `range` scales a directional light's colour; the light's update,
    /// `FUN_00569cd0`, is not read]
    pub to_sun: Vec3,
    pub sun_colour: Vec3,
    pub sun_range: f32,
    /// `fog` (bytes over 255), `fogHotspot`, `fogRange`, `fogFalloff`, `fogHeight`.
    pub fog_colour: Vec3,
    pub fog_hotspot: f32,
    pub fog_range: f32,
    pub fog_falloff: f32,
    pub fog_height: f32,
    /// `bloomThreshold` x 0.01, `bloomBrightness`, `bloomSaturation` x 0.01, `bloomTint`
    /// bytes over 255, `bloomBlurLevel` (`FUN_00567660`). [game: the scales]
    pub bloom_threshold: f32,
    pub bloom_brightness: f32,
    pub bloom_saturation: f32,
    pub bloom_tint: Vec3,
    pub bloom_blur: f32,
    /// `colorFilterBrightness`, `Contrast`, `Saturation`, `Lightness`, as stored (127.5
    /// is the middle of each).
    pub filter_brightness: f32,
    pub filter_contrast: f32,
    pub filter_saturation: f32,
    pub filter_lightness: f32,
    /// `irisTargetLum`, `irisMin`, `irisMax`, `irisBloomGain`.
    pub iris_target: f32,
    pub iris_min: f32,
    pub iris_max: f32,
    pub iris_bloom_gain: f32,
}

fn bytes(node: Option<Node>) -> Vec3 {
    let v = node.map(Node::ints).unwrap_or_default();
    if v.len() >= 3 { Vec3::new(v[0] as f32, v[1] as f32, v[2] as f32) / 255.0 } else { Vec3::ZERO }
}

fn read(name: &str, root: Node) -> Option<Environment> {
    let num = |field: &str| root.get(field).and_then(Node::float).unwrap_or(0.0);
    root.get("bloomThreshold")?;
    let sun = root.get("shadowLight")?;
    let matrix = sun.get("matrix").map(Node::floats).filter(|m| m.len() == 12)?;
    let light = sun.get("things")?.at(0)?.get("attachments")?.items().filter_map(|a| a.get("thing")).find(|t| t.type_name() == Some("Light"))?;
    let colour = light.get("colour").map(Node::floats).filter(|c| c.len() == 3)?;
    Some(Environment {
        name: name.to_owned(),
        ambient: bytes(root.get("ambient")),
        to_sun: Vec3::new(matrix[6], matrix[7], matrix[8]).normalize_or_zero(),
        sun_colour: Vec3::new(colour[0], colour[1], colour[2]),
        sun_range: light.get("range").and_then(Node::float).unwrap_or(1.0),
        fog_colour: bytes(root.get("fog")),
        fog_hotspot: num("fogHotspot"),
        fog_range: num("fogRange"),
        fog_falloff: num("fogFalloff"),
        fog_height: num("fogHeight"),
        bloom_threshold: num("bloomThreshold") * 0.01,
        bloom_brightness: num("bloomBrightness"),
        bloom_saturation: num("bloomSaturation") * 0.01,
        bloom_tint: bytes(root.get("bloomTint")),
        bloom_blur: num("bloomBlurLevel"),
        filter_brightness: num("colorFilterBrightness"),
        filter_contrast: num("colorFilterContrast"),
        filter_saturation: num("colorFilterSaturation"),
        filter_lightness: num("colorFilterLightness"),
        iris_target: num("irisTargetLum"),
        iris_min: num("irisMin"),
        iris_max: num("irisMax"),
        iris_bloom_gain: num("irisBloomGain"),
    })
}

/// The level's own environment: the first root of the zone's data chunks that has a sun
/// (`shadowLight`). The special environments have none.
pub fn load(game_dir: &Path, level: &str) -> Result<Environment, String> {
    let pack = Pack::open(&game_dir.join("levels").join(level).join("zone.str"))?;
    for chunk in pack.of_type(pack::DATA) {
        // A chunk with no environment in it is not worth parsing: the field's name is in
        // the chunk's own name table as text.
        let data = pack.data(chunk);
        if !data.windows(14).any(|w| w == b"bloomThreshold") {
            continue;
        }
        let file = DataFile::parse(data.to_vec())?;
        for (hash, root) in file.root().fields() {
            let name = file.name(hash).map_or_else(|| format!("#{hash:08x}"), str::to_owned);
            if let Some(environment) = read(&name, root) {
                return Ok(environment);
            }
        }
    }
    Err(format!("{level}: no environment with a sun"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs the game install.
    #[test]
    fn the_west_town_has_a_low_warm_sun_and_a_blue_ambient() {
        let e = load(Path::new(r"C:\Games2"), "us_west_town").unwrap();
        assert_eq!(e.name, "Environment01");
        assert!((e.ambient - Vec3::new(35.0, 54.0, 71.0) / 255.0).length() < 1e-4);
        assert!((e.to_sun - Vec3::new(-0.24337, 0.815195, 0.525575)).length() < 1e-3);
        assert!((e.sun_colour - Vec3::new(1.2902, 0.953922, 0.706863)).length() < 1e-4);
        assert!((e.bloom_threshold - 0.586).abs() < 1e-4 && e.bloom_brightness == 0.2 && e.bloom_saturation == 1.0);
        assert_eq!((e.fog_hotspot, e.fog_range, e.fog_falloff), (164.0, 700.0, 95.0));
        assert_eq!((e.filter_brightness, e.filter_contrast, e.filter_saturation), (142.0, 150.0, 126.0));
        assert!((e.iris_target - 0.368).abs() < 1e-4);
    }
}
