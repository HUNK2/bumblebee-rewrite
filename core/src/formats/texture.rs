//! Texture blobs: an `APKF` blob with one `TEX` entry. The image block holds a small header and
//! the bulk block holds every mip level, largest first.

use super::Bytes;
use super::apk::Blob;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PixelFormat {
    Dxt1,
    Dxt3,
    Dxt5,
    /// Three bytes per pixel, stored blue, green, red.
    Bgr8,
    /// Four bytes per pixel, stored blue, green, red, alpha.
    Bgra8,
    L8,
}

pub struct Texture {
    /// The path the texture was built from, as stored in the pack.
    pub name: String,
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub faces: u32,
    pub mips: u32,
    pub format: PixelFormat,
    pub pixels: Vec<u8>,
}

impl PixelFormat {
    fn from_code(code: u32) -> Option<Self> {
        Some(match &code.to_le_bytes() {
            b"DXT1" => Self::Dxt1,
            b"DXT3" => Self::Dxt3,
            b"DXT5" => Self::Dxt5,
            _ => match code {
                20 => Self::Bgr8,
                21 => Self::Bgra8,
                50 => Self::L8,
                _ => return None,
            },
        })
    }

    fn level_bytes(self, width: u32, height: u32) -> usize {
        let blocks = (width.div_ceil(4).max(1) * height.div_ceil(4).max(1)) as usize;
        match self {
            Self::Dxt1 => blocks * 8,
            Self::Dxt3 | Self::Dxt5 => blocks * 16,
            Self::Bgr8 => (width * height * 3) as usize,
            Self::Bgra8 => (width * height * 4) as usize,
            Self::L8 => (width * height) as usize,
        }
    }
}

/// `Ok(None)` for an entry that is a frame list (an animated sequence) rather than an image.
pub fn parse(data: &[u8]) -> Result<Option<Texture>, String> {
    let blob = Blob::parse(data)?;
    let entry = blob.entries.iter().find(|e| &e.tag == b"TEX\0").ok_or("not a texture blob")?;
    parse_entry(&blob, entry)
}

/// Textures in a multi-entry APKF library, such as the Scaleform `globalassets.apk` file.
/// `None` entries are IFL frame lists, just like `parse`.
pub fn parse_library(data: &[u8]) -> Result<Vec<Texture>, String> {
    let blob = Blob::parse(data)?;
    let mut textures = Vec::new();
    for entry in blob.entries.iter().filter(|e| &e.tag == b"TEX\0") {
        if let Some(texture) = parse_entry(&blob, entry)? {
            textures.push(texture);
        }
    }
    Ok(textures)
}

fn parse_entry(blob: &Blob<'_>, entry: &super::apk::Entry) -> Result<Option<Texture>, String> {
    let image = blob.image.get(entry.image_offset..entry.image_offset + entry.image_size).ok_or("texture structure runs past the image block")?;
    let id = image.u32_at(12)?;
    let flags = image.u32_at(16)?;
    if entry.bulk_size == 0 && flags & 1 != 0 {
        return Ok(None);
    }
    let (width, height) = (image.u32_at(24)?, image.u32_at(28)?);
    let (faces, mips) = (image.u32_at(32)?, image.u32_at(36)?);
    let code = image.u32_at(40)?;
    let format = PixelFormat::from_code(code).ok_or_else(|| format!("{}: unhandled pixel format {code:#x}", entry.name))?;
    let expected: usize =
        (0..mips).map(|level| format.level_bytes((width >> level).max(1), (height >> level).max(1))).sum::<usize>()
            * faces as usize;
    if expected != entry.bulk_size {
        return Err(format!("{}: {} bytes of pixels, expected {expected}", entry.name, entry.bulk_size));
    }
    let end = entry.bulk_offset.checked_add(entry.bulk_size).ok_or("texture bulk range overflow")?;
    let pixels = blob.bulk.get(entry.bulk_offset..end).ok_or("texture payload runs past the bulk block")?;
    Ok(Some(Texture { name: entry.name.clone(), id, width, height, faces, mips, format, pixels: pixels.to_vec() }))
}
