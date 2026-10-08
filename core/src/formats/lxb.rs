//! Data objects (`.lxb` files and the data chunks of packs): self-describing typed trees.
//!
//! The file carries its own type descriptors, so values are read in place through a cursor
//! instead of being unpacked into a tree first. Offsets inside a descriptor are signed and
//! relative to the start of that descriptor; offsets inside data are signed and relative to
//! the field that holds them. Names are stored as CRC-32 with a table to turn them back.

use std::collections::HashMap;

use super::Bytes;
use super::hash::crc32;

const KIND_INTEGER: u32 = 0;
const KIND_FLOAT: u32 = 1;
const KIND_ENUM: u32 = 2;
const KIND_FLAGS: u32 = 3;
const KIND_STRING: u32 = 4;
const KIND_STRUCT_A: u32 = 5;
const KIND_STRUCT_B: u32 = 6;
const KIND_EXTENSION: u32 = 7;
const KIND_FIXED_ARRAY: u32 = 8;
const KIND_ARRAY: u32 = 9;
const KIND_POINTER: u32 = 10;
const KIND_NAMED: u32 = 12;

/// The type offset an object header carries when no typed body follows it.
const UNTYPED_OBJECT: i32 = 8;

pub struct DataFile {
    data: Vec<u8>,
    names: HashMap<u32, String>,
    roots: Vec<(u32, usize, usize)>,
}

#[derive(Clone, Copy)]
enum Place {
    /// The roots of the whole file.
    Top,
    /// A value of the type described at `ty`, stored at `at`.
    Value { ty: usize, at: usize },
    /// A data file embedded in this one, starting at `at`. It shares the outer name table.
    Embedded { at: usize },
    /// A pointer that only names an object defined somewhere else.
    Link,
}

#[derive(Clone, Copy)]
pub struct Node<'a> {
    file: &'a DataFile,
    place: Place,
    label: u32,
}

fn offset(base: usize, delta: i64) -> Option<usize> {
    usize::try_from(base as i64 + delta).ok()
}

fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

impl DataFile {
    pub fn parse(data: Vec<u8>) -> Result<Self, String> {
        let version = data.u32_at(0)?;
        if version != 5 {
            return Err(format!("unexpected data object version {version}"));
        }
        let names_base = data.u32_at(4)? as usize;
        let root_count = data.u32_at(8)? as usize;
        let mut roots = Vec::with_capacity(root_count);
        for i in 0..root_count {
            let at = 12 + i * 12;
            roots.push((data.u32_at(at)?, data.u32_at(at + 4)? as usize, data.u32_at(at + 8)? as usize));
        }
        let name_count = data.u32_at(names_base.checked_sub(4).ok_or("name table offset is too small")?)? as usize;
        let mut names = HashMap::with_capacity(name_count);
        for i in 0..name_count {
            let hash = data.u32_at(names_base + i * 8)?;
            let start = names_base + data.u32_at(names_base + i * 8 + 4)? as usize;
            let tail = data.get(start..).ok_or("name string starts past the end")?;
            let len = tail.iter().position(|&b| b == 0).ok_or("name string is not terminated")?;
            names.insert(hash, latin1(&tail[..len]));
        }
        Ok(Self { data, names, roots })
    }

    pub fn root(&self) -> Node<'_> {
        Node { file: self, place: Place::Top, label: 0 }
    }

    /// The text behind a name hash, when this file's table has it.
    pub fn name(&self, hash: u32) -> Option<&str> {
        self.names.get(&hash).map(String::as_str)
    }

    fn u32(&self, at: usize) -> Option<u32> {
        self.data.u32_at(at).ok()
    }

    fn i32(&self, at: usize) -> Option<i32> {
        self.data.i32_at(at).ok()
    }

    /// Follows named types down to the descriptor that says how the value is stored.
    /// Also returns the outermost type name met on the way.
    fn resolve(&self, mut ty: usize) -> Option<(usize, Option<u32>)> {
        let mut name = None;
        for _ in 0..32 {
            if self.u32(ty)? != KIND_NAMED {
                return Some((ty, name));
            }
            name.get_or_insert(self.u32(ty + 4)?);
            ty = offset(ty, self.i32(ty + 8)? as i64)?;
        }
        None
    }

    fn is_embedded_file(&self, at: usize) -> bool {
        let Some(len) = self.data.len().checked_sub(at) else { return false };
        if len < 12 || self.u32(at) != Some(5) || self.u32(at + 4) != Some(0) {
            return false;
        }
        let count = self.u32(at + 8).unwrap_or(u32::MAX) as usize;
        if count > 100_000 || 12 + count * 12 > len {
            return false;
        }
        (0..count).all(|i| {
            let ty = self.u32(at + 12 + i * 12 + 4).unwrap_or(u32::MAX) as usize;
            let data = self.u32(at + 12 + i * 12 + 8).unwrap_or(u32::MAX) as usize;
            ty + 4 <= len && data <= len
        })
    }
}

impl<'a> Node<'a> {
    fn value(self, ty: usize, at: usize) -> Node<'a> {
        Node { file: self.file, place: Place::Value { ty, at }, label: 0 }
    }

    /// Follows pointers until something that is not one. `None` for a null pointer.
    fn settle(self) -> Option<Node<'a>> {
        let file = self.file;
        let mut node = self;
        for _ in 0..16 {
            let Place::Value { ty, at } = node.place else { return Some(node) };
            let (stored, _) = file.resolve(ty)?;
            if file.u32(stored)? != KIND_POINTER {
                return Some(node);
            }
            let rel = file.i32(at)?;
            if rel == 0 {
                return None;
            }
            let object = offset(at, rel as i64)?;
            let label = file.u32(object.checked_sub(8)?)?;
            let type_rel = file.i32(object - 4)?;
            let place = if type_rel != UNTYPED_OBJECT {
                Place::Value { ty: offset(object - 8, type_rel as i64)?, at: object }
            } else if file.is_embedded_file(object) {
                Place::Embedded { at: object }
            } else {
                Place::Link
            };
            node = Node { file, place, label };
        }
        None
    }

    /// The storage descriptor and data position of a settled value.
    fn stored(self) -> Option<(u32, usize, usize)> {
        let Place::Value { ty, at } = self.settle()?.place else { return None };
        let (ty, _) = self.file.resolve(ty)?;
        Some((self.file.u32(ty)?, ty, at))
    }

    /// The file the value is in.
    pub fn file(self) -> &'a DataFile {
        self.file
    }

    /// Name hash from the object header, for a node reached through a pointer.
    pub fn label(self) -> Option<u32> {
        self.settle().map(|n| n.label).filter(|&l| l != 0)
    }

    /// Name of the object's type ("Object", "RenderThing", ...).
    pub fn type_name(self) -> Option<&'a str> {
        let Place::Value { ty, .. } = self.settle()?.place else { return None };
        self.file.name(self.file.resolve(ty)?.1?)
    }

    pub fn is_embedded_file(self) -> bool {
        matches!(self.settle().map(|n| n.place), Some(Place::Embedded { .. }))
    }

    /// Every named member: struct fields, or the roots of a file.
    pub fn fields(self) -> Vec<(u32, Node<'a>)> {
        let mut out = Vec::new();
        let Some(node) = self.settle() else { return out };
        match node.place {
            Place::Top => out.extend(self.file.roots.iter().map(|&(name, ty, at)| (name, self.value(ty, at)))),
            Place::Embedded { at } => {
                let count = self.file.u32(at + 8).unwrap_or(0) as usize;
                for i in 0..count {
                    let entry = at + 12 + i * 12;
                    if let (Some(name), Some(ty), Some(data)) =
                        (self.file.u32(entry), self.file.u32(entry + 4), self.file.u32(entry + 8))
                    {
                        out.push((name, self.value(at + ty as usize, at + data as usize)));
                    }
                }
            }
            Place::Value { ty, at } => self.struct_fields(ty, at, &mut out, 0),
            Place::Link => {}
        }
        out
    }

    fn struct_fields(self, ty: usize, at: usize, out: &mut Vec<(u32, Node<'a>)>, depth: u32) {
        let file = self.file;
        let Some((ty, _)) = file.resolve(ty) else { return };
        let (first, count) = match file.u32(ty) {
            Some(KIND_STRUCT_A) => (ty + 12, file.u32(ty + 8)),
            Some(KIND_STRUCT_B) => (ty + 8, file.u32(ty + 4)),
            Some(KIND_EXTENSION) if depth < 16 => {
                let (Some(second_at), Some(first_rel), Some(second_rel)) =
                    (file.u32(ty + 4), file.i32(ty + 8), file.i32(ty + 12))
                else {
                    return;
                };
                if let Some(first) = offset(ty, first_rel as i64) {
                    self.struct_fields(first, at, out, depth + 1);
                }
                if let Some(second) = offset(ty, second_rel as i64) {
                    self.struct_fields(second, at + second_at as usize, out, depth + 1);
                }
                return;
            }
            _ => return,
        };
        for i in 0..count.unwrap_or(0) as usize {
            let entry = first + i * 12;
            let (Some(name), Some(type_rel), Some(data)) = (file.u32(entry), file.i32(entry + 4), file.u32(entry + 8))
            else {
                return;
            };
            if let Some(field_ty) = offset(ty, type_rel as i64) {
                out.push((name, self.value(field_ty, at + data as usize)));
            }
        }
    }

    /// Member names as text, for looking around.
    pub fn field_names(self) -> Vec<String> {
        let name = |hash| self.file.name(hash).map_or_else(|| format!("#{hash:08x}"), str::to_owned);
        self.fields().into_iter().map(|(hash, _)| name(hash)).collect()
    }

    pub fn field(self, hash: u32) -> Option<Node<'a>> {
        self.fields().into_iter().find(|&(name, _)| name == hash).map(|(_, node)| node)
    }

    pub fn get(self, name: &str) -> Option<Node<'a>> {
        self.field(crc32(name.as_bytes()))
    }

    /// Walks a path of field names.
    pub fn path(self, names: &[&str]) -> Option<Node<'a>> {
        names.iter().try_fold(self, |node, name| node.get(name))
    }

    /// Element type, element size, first element and count of an array value.
    fn array(self) -> Option<(usize, usize, usize, usize)> {
        let (kind, ty, at) = self.stored()?;
        let file = self.file;
        match kind {
            KIND_FIXED_ARRAY => {
                let element = offset(ty, file.i32(ty + 12)? as i64)?;
                Some((element, file.u32(ty + 8)? as usize, at, file.u32(ty + 4)? as usize))
            }
            KIND_ARRAY => {
                let element = offset(ty, file.i32(ty + 8)? as i64)?;
                let size = file.u32(ty + 4)? as usize;
                let start = file.i32(at)?;
                if start == 0 {
                    return Some((element, size, at, 0));
                }
                let first = offset(at, start as i64)?;
                let count = usize::try_from(file.i32(first.checked_sub(4)?)?).ok()?;
                (first + count * size <= file.data.len()).then_some((element, size, first, count))
            }
            _ => None,
        }
    }

    pub fn len(self) -> usize {
        self.array().map_or(0, |a| a.3)
    }

    pub fn at(self, index: usize) -> Option<Node<'a>> {
        let (element, size, first, count) = self.array()?;
        (index < count).then(|| self.value(element, first + index * size))
    }

    pub fn items(self) -> impl Iterator<Item = Node<'a>> {
        let array = self.array();
        (0..array.map_or(0, |a| a.3)).map(move |i| {
            let (element, size, first, _) = array.unwrap();
            self.value(element, first + i * size)
        })
    }

    pub fn int(self) -> Option<i64> {
        let (kind, ty, at) = self.stored()?;
        let file = self.file;
        match kind {
            KIND_INTEGER => {
                let (lo, hi) = (file.i32(ty + 4)? as i64, file.i32(ty + 8)? as i64);
                let span = if lo < 0 { hi.max(0) - lo } else { hi } as u32;
                let signed = lo < 0;
                Some(if span < 0x100 {
                    let b = *file.data.get(at)?;
                    if signed { b as i8 as i64 } else { b as i64 }
                } else if span < 0x1_0000 {
                    let w = file.data.u16_at(at).ok()?;
                    if signed { w as i16 as i64 } else { w as i64 }
                } else if signed {
                    file.i32(at)? as i64
                } else {
                    file.u32(at)? as i64
                })
            }
            KIND_ENUM | KIND_FLAGS => file.u32(at).map(i64::from),
            _ => None,
        }
    }

    pub fn float(self) -> Option<f32> {
        let (kind, _, at) = self.stored()?;
        match kind {
            KIND_FLOAT => self.file.data.f32_at(at).ok(),
            KIND_INTEGER => self.int().map(|v| v as f32),
            _ => None,
        }
    }

    pub fn text(self) -> Option<String> {
        let (kind, _, at) = self.stored()?;
        if kind != KIND_STRING {
            return None;
        }
        let rel = self.file.i32(at)?;
        if rel == 0 {
            return None;
        }
        let tail = self.file.data.get(offset(at, rel as i64)?..)?;
        let len = tail.iter().position(|&b| b == 0)?;
        Some(latin1(&tail[..len]))
    }

    /// A string that names an asset: the asset's id in four bytes, then its source path
    /// (a particle renderer's `Texture`).
    pub fn asset_text(self) -> Option<(u32, String)> {
        let (kind, _, at) = self.stored()?;
        if kind != KIND_STRING {
            return None;
        }
        let rel = self.file.i32(at)?;
        if rel == 0 {
            return None;
        }
        let start = offset(at, rel as i64)?;
        let id = self.file.u32(start)?;
        let tail = self.file.data.get(start + 4..)?;
        let len = tail.iter().position(|&b| b == 0)?;
        Some((id, latin1(&tail[..len])))
    }

    pub fn enum_name(self) -> Option<&'a str> {
        let (kind, ty, at) = self.stored()?;
        if kind != KIND_ENUM {
            return None;
        }
        let file = self.file;
        let value = file.u32(at)?;
        (0..file.u32(ty + 4)? as usize)
            .find(|i| file.u32(ty + 12 + i * 8) == Some(value))
            .and_then(|i| file.name(file.u32(ty + 8 + i * 8)?))
    }

    /// The hash of an enum value's name, for names the file's table does not spell out
    /// (a `FootStepEvent`'s `side`: the code compares the hashes).
    pub fn enum_hash(self) -> Option<u32> {
        let (kind, ty, at) = self.stored()?;
        if kind != KIND_ENUM {
            return None;
        }
        let file = self.file;
        let value = file.u32(at)?;
        (0..file.u32(ty + 4)? as usize).find(|i| file.u32(ty + 12 + i * 8) == Some(value)).and_then(|i| file.u32(ty + 8 + i * 8))
    }

    pub fn is_true(self) -> bool {
        self.enum_name() == Some("true")
    }

    /// Every float under this value in storage order: a vector, or a matrix as nested arrays.
    pub fn floats(self) -> Vec<f32> {
        let mut out = Vec::new();
        self.collect_floats(&mut out, 0);
        out
    }

    fn collect_floats(self, out: &mut Vec<f32>, depth: u32) {
        if let Some(v) = self.float() {
            out.push(v);
        } else if depth < 4 {
            for item in self.items() {
                item.collect_floats(out, depth + 1);
            }
        }
    }

    /// Every integer of an array of integers.
    pub fn ints(self) -> Vec<i64> {
        self.items().filter_map(Node::int).collect()
    }
}
