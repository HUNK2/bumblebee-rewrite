//! Model blobs: an `APKF` blob with `MAT`, `MESH` and sometimes `SKEL` entries.
//!
//! A mesh is a list of parts, one per material. Each part has its own vertex buffer, 16-bit
//! index buffer and Direct3D 9 vertex declaration. Skinned parts also carry a palette that maps
//! the bone numbers in their vertices to bones of the skeleton.

use std::collections::HashMap;

use super::Bytes;
use super::apk::{Blob, Entry};
use super::pack::{self, Pack};
use super::texture::{self, Texture};

const TRIANGLE_LIST: u32 = 4;

const USAGE_POSITION: u8 = 0;
const USAGE_BLEND_WEIGHT: u8 = 1;
const USAGE_BLEND_INDICES: u8 = 2;
const USAGE_NORMAL: u8 = 3;
const USAGE_TEXCOORD: u8 = 5;
const USAGE_TANGENT: u8 = 6;
const USAGE_BINORMAL: u8 = 7;

const PARAM_TEXTURE: u32 = 3;
const PARAM_FLOAT: u32 = 2;
const PARAM_VECTOR: u32 = 4;

#[derive(Default)]
pub struct Part {
    /// Asset id of the material.
    pub material: Option<u32>,
    /// Skeleton bone for each bone number used by this part's vertices; empty for rigid parts.
    pub palette: Vec<u16>,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub tangents: Vec<[f32; 3]>,
    pub binormals: Vec<[f32; 3]>,
    pub blend_indices: Vec<[f32; 4]>,
    pub blend_weights: Vec<[f32; 4]>,
    pub indices: Vec<u16>,
}

pub struct Mesh {
    pub name: String,
    /// Centre and radius of the bounding sphere.
    pub sphere: [f32; 4],
    pub parts: Vec<Part>,
    /// Asset id of the skeleton, for a skinned mesh.
    pub skeleton: Option<u32>,
    /// Lower-detail meshes and the distance each takes over at.
    pub lods: Vec<(u32, f32)>,
}

pub enum ParamValue {
    Number(f32),
    /// An integer, or the name hash of an enum value.
    Word(u32),
    Vector([f32; 4]),
    /// Asset id of a texture.
    Texture(Option<u32>),
}

pub struct Param {
    pub name: u32,
    /// For a sampler setting, the name of the texture parameter it belongs to.
    pub owner: u32,
    pub value: ParamValue,
}

pub struct Material {
    pub name: String,
    pub technique: u32,
    pub params: Vec<Param>,
}

pub struct Bone {
    /// Lower-case multiply-by-33 hash of the bone's name.
    pub name: u32,
    /// Model space to bone space at rest: four rows of x, y, z (row-vector convention).
    pub inverse_bind: [[f32; 3]; 4],
}

/// Everything a pack's model and texture chunks define, keyed by asset id.
#[derive(Default)]
pub struct Library {
    pub meshes: HashMap<u32, Mesh>,
    pub materials: HashMap<u32, Material>,
    pub skeletons: HashMap<u32, Vec<Bone>>,
    pub textures: HashMap<u32, Texture>,
    /// Chunks that could not be read, with the reason.
    pub problems: Vec<String>,
}

impl Material {
    fn find(&self, name: &str) -> Option<&ParamValue> {
        let hash = super::hash::crc32(name.as_bytes());
        self.params.iter().find(|p| p.name == hash && p.owner == 0).map(|p| &p.value)
    }

    pub fn texture(&self, name: &str) -> Option<u32> {
        match self.find(name)? {
            ParamValue::Texture(id) => *id,
            _ => None,
        }
    }

    pub fn vector(&self, name: &str) -> Option<[f32; 4]> {
        match self.find(name)? {
            ParamValue::Vector(v) => Some(*v),
            _ => None,
        }
    }

    pub fn number(&self, name: &str) -> Option<f32> {
        match self.find(name)? {
            ParamValue::Number(v) => Some(*v),
            _ => None,
        }
    }

    pub fn word(&self, name: &str) -> Option<u32> {
        match self.find(name)? {
            ParamValue::Word(v) => Some(*v),
            _ => None,
        }
    }
}

impl Library {
    pub fn load(pack: &Pack) -> Self {
        let mut library = Self::default();
        for chunk in pack.of_type(pack::TEXTURE) {
            match texture::parse(pack.data(chunk)) {
                Ok(Some(texture)) => {
                    library.textures.insert(texture.id, texture);
                }
                Ok(None) => {}
                Err(e) => library.problems.push(format!("texture chunk {}: {e}", chunk.index)),
            }
        }
        for chunk in pack.of_type(pack::MODEL) {
            if let Err(e) = library.add_model(pack.data(chunk)) {
                library.problems.push(format!("model chunk {}: {e}", chunk.index));
            }
        }
        library
    }

    fn add_model(&mut self, data: &[u8]) -> Result<(), String> {
        let blob = Blob::parse(data)?;
        for entry in &blob.entries {
            let result = match &entry.tag {
                b"MESH" => read_mesh(&blob, entry).map(|mesh| drop(self.meshes.insert(entry.id, mesh))),
                b"MAT\0" => read_material(&blob, entry).map(|mat| drop(self.materials.insert(entry.id, mat))),
                b"SKEL" => read_skeleton(&blob, entry).map(|bones| drop(self.skeletons.insert(entry.id, bones))),
                _ => Ok(()),
            };
            result.map_err(|e| format!("{}: {e}", entry.name))?;
        }
        Ok(())
    }
}

fn id_at(blob: &Blob, pointer_field: usize) -> Result<Option<u32>, String> {
    let Some(target) = blob.image_pointer(pointer_field)? else { return Ok(None) };
    blob.entry_at(target).map(|e| Some(e.id)).ok_or_else(|| format!("pointer at {pointer_field:#x} is not to an entry"))
}

fn floats<const N: usize>(bytes: &[u8], at: usize) -> Result<[f32; N], String> {
    let mut out = [0.0; N];
    for (i, v) in out.iter_mut().enumerate() {
        *v = bytes.f32_at(at + i * 4)?;
    }
    Ok(out)
}

fn read_mesh(blob: &Blob, entry: &Entry) -> Result<Mesh, String> {
    let image = blob.image;
    let base = entry.image_offset;
    let part_count = image.u32_at(base + 0x0c)? as usize;
    let lod_count = image.u32_at(base + 0x18)? as usize;

    let mut parts = Vec::with_capacity(part_count);
    if let Some(table) = blob.image_pointer(base + 0x10)? {
        for i in 0..part_count {
            let part = blob.image_pointer(table + i * 8 + 4)?.ok_or("part table entry without a part")?;
            parts.push(read_part(blob, part)?);
        }
    }
    let mut lods = Vec::with_capacity(lod_count);
    if let Some(table) = blob.image_pointer(base + 0x1c)? {
        for i in 0..lod_count {
            let mesh = id_at(blob, table + i * 8)?.ok_or("level-of-detail entry without a mesh")?;
            lods.push((mesh, image.f32_at(table + i * 8 + 4)?));
        }
    }
    Ok(Mesh {
        name: entry.name.clone(),
        sphere: floats(image, base + 0x20)?,
        parts,
        skeleton: id_at(blob, base + 0x14)?,
        lods,
    })
}

fn read_part(blob: &Blob, at: usize) -> Result<Part, String> {
    let image = blob.image;
    let mut part = Part { material: id_at(blob, at + 0x30)?, ..Default::default() };
    if let Some(palette) = blob.image_pointer(at + 0x34)? {
        for i in 0..image.u32_at(at + 0x38)? as usize {
            part.palette.push(image.u16_at(palette + i * 2)?);
        }
    }
    let vertex_count = image.u32_at(at + 0x44)? as usize;
    let index_count = image.u32_at(at + 0x54)? as usize;
    if image.u32_at(at + 0x58)? != 2 {
        return Err("indices are not 16-bit".into());
    }
    if image.u32_at(at + 0x60)? != TRIANGLE_LIST {
        return Err(format!("primitive type {} is not a triangle list", image.u32_at(at + 0x60)?));
    }
    let layout = blob.image_pointer(at + 0x5c)?.ok_or("part without a vertex layout")?;
    let stride = image.u32_at(layout)? as usize;
    let declaration = blob.image_pointer(layout + 4)?.ok_or("part without a vertex declaration")?;

    let vertices = blob.bulk_pointer(at + 0x3c)?;
    let vertices = blob.bulk.get(vertices..vertices + vertex_count * stride).ok_or("vertex buffer runs past the end")?;
    let indices = blob.bulk_pointer(at + 0x4c)?;
    let indices = blob.bulk.get(indices..indices + index_count * 2).ok_or("index buffer runs past the end")?;
    part.indices = indices.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
    if part.indices.iter().any(|&i| i as usize >= vertex_count) {
        return Err("an index points past the vertex buffer".into());
    }

    // One element per 8 bytes: stream, offset, type, method, usage, usage index.
    for element in 0.. {
        let e = declaration + element * 8;
        if image.u16_at(e)? == 0xff {
            break;
        }
        let offset = image.u16_at(e + 2)? as usize;
        let (kind, usage, usage_index) = (image[e + 4], image[e + 6], image[e + 7]);
        let wanted = usage_index == 0
            && matches!(
                usage,
                USAGE_POSITION
                    | USAGE_NORMAL
                    | USAGE_TANGENT
                    | USAGE_BINORMAL
                    | USAGE_TEXCOORD
                    | USAGE_BLEND_INDICES
                    | USAGE_BLEND_WEIGHT
            );
        if !wanted {
            // Second texture coordinates and vertex colours (on effect meshes) are not used yet.
            continue;
        }
        // Types 0..=3 are one to four floats; the elements read here are always stored that way.
        let width = match kind {
            0..=3 => kind as usize + 1,
            _ => return Err(format!("vertex element type {kind} for usage {usage} is not handled")),
        };
        if offset + width * 4 > stride {
            return Err("vertex element runs past the stride".into());
        }
        let read = |vertex: &[u8]| -> [f32; 4] {
            let mut out = [0.0; 4];
            for (i, v) in out.iter_mut().take(width).enumerate() {
                let b = &vertex[offset + i * 4..offset + i * 4 + 4];
                *v = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            }
            out
        };
        let three = || vertices.chunks_exact(stride).map(|v| read(v)).map(|v| [v[0], v[1], v[2]]).collect();
        let four = || vertices.chunks_exact(stride).map(|v| read(v)).collect();
        match (usage, usage_index) {
            (USAGE_POSITION, 0) => part.positions = three(),
            (USAGE_NORMAL, 0) => part.normals = three(),
            (USAGE_TANGENT, 0) => part.tangents = three(),
            (USAGE_BINORMAL, 0) => part.binormals = three(),
            (USAGE_TEXCOORD, 0) => {
                part.uvs = vertices.chunks_exact(stride).map(|v| read(v)).map(|v| [v[0], v[1]]).collect();
            }
            (USAGE_BLEND_INDICES, 0) => part.blend_indices = four(),
            (USAGE_BLEND_WEIGHT, 0) => part.blend_weights = four(),
            _ => {}
        }
    }
    if part.positions.len() != vertex_count {
        return Err("part has no positions".into());
    }
    Ok(part)
}

fn read_material(blob: &Blob, entry: &Entry) -> Result<Material, String> {
    let image = blob.image;
    let base = entry.image_offset;
    let count = image.u32_at(base + 0x20)? as usize;
    let mut params = Vec::with_capacity(count);
    if let Some(table) = blob.image_pointer(base + 0x24)? {
        for i in 0..count {
            let at = table + i * 16;
            let value = match image.u32_at(at + 12)? & 0xffff {
                PARAM_FLOAT => ParamValue::Number(image.f32_at(at)?),
                PARAM_TEXTURE => ParamValue::Texture(blob.reference(at)),
                PARAM_VECTOR => match blob.image_pointer(at)? {
                    Some(target) => ParamValue::Vector(floats(image, target)?),
                    None => ParamValue::Vector([0.0; 4]),
                },
                _ => ParamValue::Word(image.u32_at(at)?),
            };
            params.push(Param { name: image.u32_at(at + 4)?, owner: image.u32_at(at + 8)?, value });
        }
    }
    Ok(Material { name: entry.name.clone(), technique: image.u32_at(base + 0x14)?, params })
}

fn read_skeleton(blob: &Blob, entry: &Entry) -> Result<Vec<Bone>, String> {
    let image = blob.image;
    let base = entry.image_offset;
    let count = image.u32_at(base + 0x08)? as usize;
    let Some(first) = blob.image_pointer(base + 0x0c)? else { return Ok(Vec::new()) };
    let mut bones = Vec::with_capacity(count);
    for i in 0..count {
        // A bone is 0x90 bytes: a local matrix, the inverse bind matrix, then the name.
        let at = first + i * 0x90;
        let mut inverse_bind = [[0.0; 3]; 4];
        for (row, out) in inverse_bind.iter_mut().enumerate() {
            // The fourth column of each row is not part of the matrix.
            *out = floats(image, at + 0x40 + row * 16)?;
        }
        bones.push(Bone { name: image.u32_at(at + 0x84)?, inverse_bind });
    }
    Ok(bones)
}
