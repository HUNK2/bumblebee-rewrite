//! Turns loaded game data into Bevy assets and entities.

use std::collections::HashMap;
use std::f32::consts::FRAC_PI_2;

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes};
use bevy::mesh::{Indices, PrimitiveTopology, VertexAttributeValues};
use bevy::prelude::*;
use bevy::render::render_resource::{
    Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension,
};

use crate::formats::hash::{crc32, lower33};
use crate::formats::model::{self, Library};
use crate::formats::scene::ObjectDef;
use crate::formats::texture::{PixelFormat, Texture};
use crate::fx::Glow;
use crate::surface::{self, Surface};

const BLEND_NORMAL: u32 = crc32(b"Normal");
const BLEND_ADDITIVE: u32 = crc32(b"Additive");

/// The entities of one spawned object, by node index.
#[derive(Component)]
pub struct ObjectNodes {
    pub entities: Vec<Entity>,
    pub names: Vec<String>,
    /// CRC-32 of each node's name, which is how animation channels address nodes.
    pub name_hashes: Vec<u32>,
    /// Each node's rest transform relative to its parent.
    pub rest: Vec<Transform>,
    pub parents: Vec<Option<usize>>,
    /// The nodes the object's `SpineIK` links.
    pub spine_links: Vec<usize>,
    pub foot_rig: Option<tf2_core::footik::FootRig>,
}

/// The game's space is right-handed with z up and y forward; Bevy's has y up and -z forward.
pub fn game_to_bevy(v: Vec3) -> Vec3 {
    Vec3::new(v.x, v.z, -v.y)
}

pub fn bevy_to_game(v: Vec3) -> Vec3 {
    Vec3::new(v.x, -v.z, v.y)
}

/// Rotation that takes game-space model data into Bevy's space.
pub fn model_space_rotation() -> Quat {
    Quat::from_rotation_x(-FRAC_PI_2)
}

/// The file stores matrices for row vectors: three axis rows and a position row.
fn matrix(rows: &[[f32; 3]; 4]) -> Mat4 {
    Mat4::from_cols(
        Vec3::from(rows[0]).extend(0.0),
        Vec3::from(rows[1]).extend(0.0),
        Vec3::from(rows[2]).extend(0.0),
        Vec3::from(rows[3]).extend(1.0),
    )
}

#[derive(Default)]
pub struct Cache {
    images: HashMap<(u32, bool), Option<Handle<Image>>>,
    materials: HashMap<u32, Made>,
    fallback: Option<Handle<StandardMaterial>>,
}

/// A part's material: one of the original's surface shaders where that is ported
/// (`surface.rs`), Bevy's own otherwise.
#[derive(Clone)]
enum Made {
    Standard(Handle<StandardMaterial>),
    Surface(Handle<Surface>),
    /// The original's `emissive` or `dull_fx_emissive` (`fx.rs`).
    Glow(Handle<Glow>),
}

pub struct Builder<'a> {
    pub library: &'a Library,
    pub meshes: &'a mut Assets<Mesh>,
    pub materials: &'a mut Assets<StandardMaterial>,
    pub surfaces: &'a mut Assets<Surface>,
    pub glows: &'a mut Assets<Glow>,
    pub images: &'a mut Assets<Image>,
    pub bindposes: &'a mut Assets<SkinnedMeshInverseBindposes>,
    pub cache: &'a mut Cache,
}

pub fn image(texture: &Texture, srgb: bool) -> Option<Image> {
    // One face, or the six of a cube map (a reflection): each face's levels follow it in
    // the file, which is the order Bevy wants. Block formats need whole blocks.
    if texture.faces != 1 && texture.faces != 6 {
        return None;
    }
    let compressed = matches!(texture.format, PixelFormat::Dxt1 | PixelFormat::Dxt3 | PixelFormat::Dxt5);
    if compressed && (texture.width % 4 != 0 || texture.height % 4 != 0) {
        return None;
    }
    let (format, data) = match (texture.format, srgb) {
        (PixelFormat::Dxt1, true) => (TextureFormat::Bc1RgbaUnormSrgb, texture.pixels.clone()),
        (PixelFormat::Dxt1, false) => (TextureFormat::Bc1RgbaUnorm, texture.pixels.clone()),
        (PixelFormat::Dxt3, true) => (TextureFormat::Bc2RgbaUnormSrgb, texture.pixels.clone()),
        (PixelFormat::Dxt3, false) => (TextureFormat::Bc2RgbaUnorm, texture.pixels.clone()),
        (PixelFormat::Dxt5, true) => (TextureFormat::Bc3RgbaUnormSrgb, texture.pixels.clone()),
        (PixelFormat::Dxt5, false) => (TextureFormat::Bc3RgbaUnorm, texture.pixels.clone()),
        (PixelFormat::Bgra8, true) => (TextureFormat::Bgra8UnormSrgb, texture.pixels.clone()),
        (PixelFormat::Bgra8, false) => (TextureFormat::Bgra8Unorm, texture.pixels.clone()),
        (PixelFormat::Bgr8, _) => {
            let data = texture.pixels.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect();
            (if srgb { TextureFormat::Bgra8UnormSrgb } else { TextureFormat::Bgra8Unorm }, data)
        }
        (PixelFormat::L8, _) => {
            let data = texture.pixels.iter().flat_map(|&l| [l, l, l, 255]).collect();
            (if srgb { TextureFormat::Bgra8UnormSrgb } else { TextureFormat::Bgra8Unorm }, data)
        }
    };
    let size = Extent3d { width: texture.width, height: texture.height, depth_or_array_layers: texture.faces };
    // Image::new validates a single uncompressed level. Our payload contains the
    // complete face-major mip chain, including the optional upscaled BGRA levels.
    let mut image = Image::new_uninit(size, TextureDimension::D2, format, RenderAssetUsages::RENDER_WORLD);
    image.data = Some(data);
    image.texture_descriptor.mip_level_count = texture.mips;
    if texture.faces == 6 {
        image.texture_view_descriptor =
            Some(TextureViewDescriptor { dimension: Some(TextureViewDimension::Cube), ..default() });
    }
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        anisotropy_clamp: 8,
        ..default()
    });
    Some(image)
}

fn mesh(part: &model::Part) -> Mesh {
    let count = part.positions.len();
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD);
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, part.positions.clone());
    if part.uvs.len() == count {
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, part.uvs.clone());
    }
    if part.normals.len() == count {
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, part.normals.clone());
        if part.tangents.len() == count && part.binormals.len() == count {
            // Bevy rebuilds the bitangent as cross(normal, tangent) times the sign in w.
            let tangents: Vec<[f32; 4]> = (0..count)
                .map(|i| {
                    let (n, t, b) =
                        (Vec3::from(part.normals[i]), Vec3::from(part.tangents[i]), Vec3::from(part.binormals[i]));
                    let sign = if n.cross(t).dot(b) < 0.0 { -1.0 } else { 1.0 };
                    [t.x, t.y, t.z, sign]
                })
                .collect();
            mesh.insert_attribute(Mesh::ATTRIBUTE_TANGENT, tangents);
        }
    }
    if !part.palette.is_empty() && part.blend_indices.len() == count && part.blend_weights.len() == count {
        let indices: Vec<[u16; 4]> = part.blend_indices.iter().map(|v| v.map(|i| i as u16)).collect();
        let weights: Vec<[f32; 4]> = part
            .blend_weights
            .iter()
            .map(|w| {
                let sum: f32 = w.iter().sum();
                if sum > 0.0 { w.map(|v| v / sum) } else { [1.0, 0.0, 0.0, 0.0] }
            })
            .collect();
        mesh.insert_attribute(Mesh::ATTRIBUTE_JOINT_INDEX, VertexAttributeValues::Uint16x4(indices));
        mesh.insert_attribute(Mesh::ATTRIBUTE_JOINT_WEIGHT, weights);
    }
    mesh.insert_indices(Indices::U16(part.indices.clone()));
    if part.normals.len() != count {
        mesh.compute_normals();
    }
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texture_upscale_bgra_mip_chain_is_uploaded_in_full() {
        let source = Texture {
            name: "test".into(), id: 1, width: 4, height: 4, faces: 1, mips: 3,
            format: PixelFormat::Bgra8, pixels: [10, 20, 30, 136].repeat(16 + 4 + 1),
        };
        for srgb in [false, true] {
            let result = image(&source, srgb).unwrap();
            assert_eq!(result.texture_descriptor.mip_level_count, 3);
            assert_eq!(result.data.as_deref().unwrap(), source.pixels);
            assert_eq!(result.texture_descriptor.format, if srgb { TextureFormat::Bgra8UnormSrgb } else { TextureFormat::Bgra8Unorm });
        }
    }
}

impl Builder<'_> {
    fn image(&mut self, id: u32, srgb: bool) -> Option<Handle<Image>> {
        if let Some(found) = self.cache.images.get(&(id, srgb)) {
            return found.clone();
        }
        let handle = self.library.textures.get(&id).and_then(|t| image(t, srgb)).map(|i| self.images.add(i));
        self.cache.images.insert((id, srgb), handle.clone());
        handle
    }

    fn material(&mut self, id: Option<u32>) -> Made {
        let Some((id, source)) = id.and_then(|id| Some((id, self.library.materials.get(&id)?))) else {
            let fallback =
                self.cache.fallback.get_or_insert_with(|| self.materials.add(Color::srgb(0.6, 0.6, 0.6))).clone();
            return Made::Standard(fallback);
        };
        if let Some(found) = self.cache.materials.get(&id) {
            return found.clone();
        }
        let colour = source.vector("diffuse_color").unwrap_or([1.0; 4]);
        let opacity = source.number("diffuse_texture1_opacity").unwrap_or(1.0);
        // The original's own shader for the techniques that are ported; its arithmetic is
        // on the stored values, so no texture is decoded from sRGB.
        if let Some(wanted) = surface::textures(source) {
            let images = wanted.map(|id| id.and_then(|id| self.image(id, false)));
            let alpha_mode = match source.word("blendmode") {
                Some(BLEND_ADDITIVE) => AlphaMode::Add,
                Some(BLEND_NORMAL) => AlphaMode::Blend,
                _ => AlphaMode::Opaque,
            };
            if let Some(built) = surface::build(source, images, alpha_mode) {
                let made = Made::Surface(self.surfaces.add(built));
                self.cache.materials.insert(id, made.clone());
                return made;
            }
        }
        // The same for the two unlit techniques that glow.
        if let Some(wanted) = Glow::texture(source) {
            let diffuse = wanted.and_then(|id| self.image(id, false));
            let alpha_mode = match source.word("blendmode") {
                Some(BLEND_ADDITIVE) => AlphaMode::Add,
                Some(BLEND_NORMAL) => AlphaMode::Blend,
                _ => AlphaMode::Opaque,
            };
            if let Some(built) = Glow::build(source, diffuse, alpha_mode) {
                let made = Made::Glow(self.glows.add(built));
                self.cache.materials.insert(id, made.clone());
                return made;
            }
        }
        let base_color_texture = source.texture("diffuse_texture1_texture").and_then(|t| self.image(t, true));
        let normal_map_texture = source.texture("normal_map_texture").and_then(|t| self.image(t, false));
        let blend = source.word("blendmode");
        // The game's own shaders are not reproduced; these are stand-in surface settings.
        let mut material = StandardMaterial {
            base_color: Color::srgb(colour[0], colour[1], colour[2]),
            base_color_texture,
            normal_map_texture,
            perceptual_roughness: 0.5,
            metallic: 0.35,
            ..default()
        };
        if blend == Some(BLEND_ADDITIVE) {
            material.alpha_mode = AlphaMode::Add;
            material.unlit = true;
            material.base_color = Color::srgba(colour[0], colour[1], colour[2], opacity.clamp(0.0, 1.0));
        } else if blend == Some(BLEND_NORMAL) {
            material.alpha_mode = AlphaMode::Blend;
            material.base_color = Color::srgba(colour[0], colour[1], colour[2], opacity.clamp(0.0, 1.0));
        }
        let made = Made::Standard(self.materials.add(material));
        self.cache.materials.insert(id, made.clone());
        made
    }

    /// Spawns an object's node tree with its rendered parts, and returns the root and each
    /// node's rest transform. The root holds the conversion from game space, so children keep
    /// the game's coordinates.
    pub fn spawn(&mut self, commands: &mut Commands, object: &ObjectDef, label: &str) -> (Entity, Vec<Transform>) {
        let world: Vec<Mat4> = object.nodes.iter().map(|n| matrix(&n.bind)).collect();
        let rest: Vec<Transform> = object
            .nodes
            .iter()
            .enumerate()
            .map(|(i, node)| match node.parent {
                Some(parent) => Transform::from_matrix(world[parent].inverse() * world[i]),
                None => Transform::from_matrix(world[i]),
            })
            .collect();
        let entities: Vec<Entity> = object
            .nodes
            .iter()
            .enumerate()
            .map(|(i, node)| commands.spawn((rest[i], Visibility::default(), Name::new(node.name.clone()))).id())
            .collect();
        let root = commands
            .spawn((Transform::from_rotation(model_space_rotation()), Visibility::default(), Name::new(label.to_owned())))
            .id();
        for (i, node) in object.nodes.iter().enumerate() {
            commands.entity(node.parent.map_or(root, |p| entities[p])).add_child(entities[i]);
        }

        for render in &object.renders {
            let Some(source) = self.library.meshes.get(&lower33(&render.model)) else {
                warn!("{label}: no mesh named {}", render.model);
                continue;
            };
            let skeleton = source.skeleton.and_then(|id| self.library.skeletons.get(&id));
            for part in &source.parts {
                let mesh_handle = self.meshes.add(mesh(part));
                let material = self.material(part.material);
                let parts = (Mesh3d(mesh_handle), Name::new(source.name.clone()));
                let child = match skeleton.filter(|_| !part.palette.is_empty()) {
                    None => {
                        let child = commands.spawn((parts, Transform::IDENTITY)).id();
                        commands.entity(entities[render.node]).add_child(child);
                        child
                    }
                    Some(bones) => {
                        // One joint per palette entry, so the bone numbers in the vertices index it directly.
                        let mut joints = Vec::new();
                        let mut inverse = Vec::new();
                        for &bone in &part.palette {
                            let Some(bone) = bones.get(bone as usize) else { continue };
                            let node = object.nodes.iter().position(|n| n.bone_hash == bone.name);
                            joints.push(node.map_or(root, |n| entities[n]));
                            inverse.push(matrix(&bone.inverse_bind));
                        }
                        let skin = SkinnedMesh { inverse_bindposes: self.bindposes.add(inverse), joints };
                        let child = commands.spawn((parts, Transform::IDENTITY, skin)).id();
                        commands.entity(root).add_child(child);
                        child
                    }
                };
                match material {
                    Made::Standard(handle) => commands.entity(child).insert(MeshMaterial3d(handle)),
                    Made::Surface(handle) => commands.entity(child).insert(MeshMaterial3d(handle)),
                    Made::Glow(handle) => commands.entity(child).insert(MeshMaterial3d(handle)),
                };
            }
        }
        let names = object.nodes.iter().map(|n| n.name.clone()).collect();
        let name_hashes = object.nodes.iter().map(|n| n.name_hash).collect();
        let parents = object.nodes.iter().map(|n| n.parent).collect();
        let spine_links = object.spine_links.clone();
        let foot_rig = tf2_core::footik::FootRig::read(object, self.library);
        commands.entity(root).insert(ObjectNodes { entities, names, name_hashes, rest: rest.clone(), parents, spine_links, foot_rig });
        (root, rest)
    }
}
