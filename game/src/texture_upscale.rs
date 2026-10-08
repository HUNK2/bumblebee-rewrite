//! Local enhancement cache. Source packs are read normally and never modified.
//! This is optional rebuild rendering quality, not original-game behaviour.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use tf2_core::formats::model::Library;
use tf2_core::formats::texture::{PixelFormat, Texture};
use tf2_core::texture_upscale::{self, MapKind};

const VERSION: &[u8; 8] = b"TF2UP001";

/// User-selected 4x quality. Select 1x to restore originals or 2x for lower memory use;
/// this choice is not a claim about the original game.
pub fn factor(args: &[String]) -> Result<u32, String> {
    if args.iter().any(|s| s == "--original-textures") {
        return Ok(1);
    }
    let Some(at) = args.iter().position(|s| s == "--texture-scale") else {
        return Ok(4);
    };
    match args.get(at + 1).map(String::as_str) {
        Some("1") => Ok(1),
        Some("2") => Ok(2),
        Some("4") => Ok(4),
        _ => Err("--texture-scale must be 1, 2 or 4".into()),
    }
}

fn hash(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

fn cache_path(dir: &Path, source: &Texture, factor: u32, kind: MapKind) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    VERSION.hash(&mut hasher);
    source.pixels.hash(&mut hasher);
    (
        source.id,
        source.width,
        source.height,
        source.faces,
        source.mips,
        factor,
    )
        .hash(&mut hasher);
    format!("{:?}:{kind:?}", source.format).hash(&mut hasher);
    let name = source.name.rsplit(['/', '\\']).next().unwrap_or("texture");
    dir.join(format!(
        "{}_{factor}x_{:016x}.bgra",
        name.trim_end_matches(".dds"),
        hasher.finish()
    ))
}

fn read_cache(path: &Path, source: &Texture, factor: u32) -> Option<Texture> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.get(..8)? != VERSION {
        return None;
    }
    let word = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
    };
    let (width, height, mips) = (
        source.width * factor,
        source.height * factor,
        source.mips + factor.ilog2(),
    );
    if (word(8)?, word(12)?, word(16)?, word(20)?) != (source.id, width, height, mips) {
        return None;
    }
    let expected: usize = (0..mips)
        .map(|i| (width >> i).max(1) as usize * (height >> i).max(1) as usize * 4)
        .sum();
    let pixels = bytes.get(32..)?;
    let checksum = u64::from_le_bytes(bytes.get(24..32)?.try_into().ok()?);
    if pixels.len() != expected || hash(pixels) != checksum {
        return None;
    }
    Some(Texture {
        name: source.name.clone(),
        id: source.id,
        width,
        height,
        faces: 1,
        mips,
        format: PixelFormat::Bgra8,
        pixels: pixels.to_vec(),
    })
}

fn write_cache(path: &Path, texture: &Texture) -> std::io::Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(path.parent().unwrap())?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(VERSION)?;
        for word in [texture.id, texture.width, texture.height, texture.mips] {
            file.write_all(&word.to_le_bytes())?;
        }
        file.write_all(&hash(&texture.pixels).to_le_bytes())?;
        file.write_all(&texture.pixels)?;
        file.sync_all()?;
        // Commit a complete payload together with its checksum.
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// Called after loading, before material creation. Original textures stay available
/// whenever enhancement fails; only selected character surfaces consume more memory.
pub fn apply(library: &mut Library, factor: u32) {
    if factor == 1 {
        eprintln!("textures: original resolution");
        return;
    }
    let dir = std::env::var_os("LOCALAPPDATA")
        .map_or_else(std::env::temp_dir, PathBuf::from)
        .join("Bumblebee/texture-cache/v1");
    let mut ids: Vec<_> = library
        .textures
        .iter()
        .filter_map(|(&id, t)| texture_upscale::map_kind(t).map(|kind| (id, kind)))
        .collect();
    ids.sort_by_key(|(id, _)| *id);
    let (mut count, mut cached, mut memory) = (0, 0, 0usize);
    for (id, kind) in ids {
        let source = &library.textures[&id];
        let path = cache_path(&dir, source, factor, kind);
        let result = match read_cache(&path, source, factor) {
            Some(t) => {
                cached += 1;
                Ok(t)
            }
            None => texture_upscale::upscale(source, factor, kind).inspect(|t| {
                if let Err(e) = write_cache(&path, t) {
                    eprintln!(
                        "texture cache could not be written: {e}; using the upscale in memory"
                    );
                }
            }),
        };
        match result {
            Ok(t) => {
                count += 1;
                memory += t.pixels.len();
                library.textures.insert(id, t);
            }
            Err(e) => eprintln!(
                "texture enhancement skipped {}: {e}; using original",
                source.name
            ),
        }
    }
    eprintln!(
        "textures: {count} Bumblebee maps at {factor}x ({cached} cached), {:.1} MiB with mip levels",
        memory as f64 / (1024.0 * 1024.0)
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_validates_source_and_payload() {
        let source = Texture {
            name: "bumblebee_color.dds".into(),
            id: 7,
            width: 1,
            height: 1,
            faces: 1,
            mips: 1,
            format: PixelFormat::Bgra8,
            pixels: vec![30, 20, 10, 136],
        };
        let dir =
            std::env::temp_dir().join(format!("bumblebee-texture-test-{}", std::process::id()));
        let path = cache_path(&dir, &source, 4, MapKind::Colour);
        let t = texture_upscale::upscale(&source, 4, MapKind::Colour).unwrap();
        write_cache(&path, &t).unwrap();
        assert_eq!(read_cache(&path, &source, 4).unwrap().pixels, t.pixels);
        assert!(read_cache(&path, &source, 2).is_none());
        let mut changed = source;
        changed.pixels[0] += 1;
        assert_ne!(cache_path(&dir, &changed, 4, MapKind::Colour), path);
        let mut bytes = std::fs::read(&path).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        std::fs::write(&path, &bytes).unwrap();
        assert!(read_cache(&path, &changed, 4).is_none());
        write_cache(&path, &t).unwrap();
        assert_eq!(read_cache(&path, &changed, 4).unwrap().pixels, t.pixels);
        bytes.truncate(12);
        std::fs::write(&path, &bytes).unwrap();
        assert!(read_cache(&path, &changed, 4).is_none());
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn quality_selection_rejects_invalid_factors() {
        let args = |s: &[&str]| s.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert_eq!(factor(&args(&[])).unwrap(), 4);
        assert_eq!(factor(&args(&["--original-textures"])).unwrap(), 1);
        assert_eq!(factor(&args(&["--texture-scale", "2"])).unwrap(), 2);
        assert!(factor(&args(&["--texture-scale", "8"])).is_err());
        assert!(factor(&args(&["--texture-scale"])).is_err());
    }
}
