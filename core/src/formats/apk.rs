//! The `APKF` blob that holds textures and models.
//!
//! A directory names the entries; an image block holds one fixed-layout structure per entry, in
//! directory order, as the game keeps them in memory (some Flash libraries pack these
//! structures contiguously); a bulk block holds what those structures
//! point into (pixels, vertices, indices). A pointer field on disk is `(block << 24) | word
//! offset`. The tail lists which image words are pointers and which must be pointed at another
//! asset (a material's texture, for instance).

use std::collections::{HashMap, HashSet};

use super::Bytes;

const BLOCK_IMAGE: u32 = 0x00;
const BLOCK_BULK: u32 = 0x04;
const LIST_END: u32 = 0xffff_ffff;

pub struct Entry {
    pub tag: [u8; 4],
    pub name: String,
    pub id: u32,
    pub image_offset: usize,
    pub image_size: usize,
    pub bulk_offset: usize,
    pub bulk_size: usize,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Pointer {
    Null,
    Image(usize),
    Bulk(usize),
    /// A per-asset slot the game fills in at load time; never followed here.
    Slot,
}

pub struct Blob<'a> {
    pub entries: Vec<Entry>,
    pub image: &'a [u8],
    pub bulk: &'a [u8],
    pointer_words: HashSet<usize>,
    /// Image byte offset of a field to the id of the asset it refers to.
    references: HashMap<usize, u32>,
}

impl<'a> Blob<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self, String> {
        if data.get(..4) != Some(b"APKF") {
            return Err("not an APKF blob".into());
        }
        let blocks = data.u32_at(16)?;
        let dir_size = data.u32_at(20)? as usize;
        let img = 0x14 + dir_size;
        if data.get(img..img + 4) != Some(b"IMG\0") {
            return Err("IMG header not where the directory size says".into());
        }
        let image_align = (data.u32_at(img + 8)? as usize).max(1);
        let image_size = data.u32_at(img + 16)? as usize;
        let image_start = img + 0x14 + data.u32_at(img + 20)? as usize;
        let phys = img + 0x18;
        let (bulk_start, bulk_size) = if data.get(phys..phys + 4) == Some(b"PHYS") {
            (phys + 0x14 + data.u32_at(phys + 20)? as usize, data.u32_at(phys + 16)? as usize)
        } else {
            (image_start + image_size, 0)
        };
        let image = data.get(image_start..image_start + image_size).ok_or("image block runs past the end")?;
        let bulk = data.get(bulk_start..bulk_start + bulk_size).ok_or("bulk block runs past the end")?;

        let mut at = (bulk_start + bulk_size + 3) & !3;
        let mut pointer_words = HashSet::new();
        loop {
            let word = data.u32_at(at)?;
            at += 4;
            if word == LIST_END {
                break;
            }
            pointer_words.insert(word as usize);
        }
        let mut references = HashMap::new();
        loop {
            let word = data.u32_at(at)?;
            if word == LIST_END {
                break;
            }
            references.insert(word as usize * 4, data.u32_at(at + 12)?);
            at += 16;
        }

        let entries = Self::directory(data, blocks, image_align, image_size)?;
        Ok(Self { entries, image, bulk, pointer_words, references })
    }

    fn directory(data: &[u8], blocks: u32, image_align: usize, image_size: usize) -> Result<Vec<Entry>, String> {
        let head_size = if blocks > 1 { 28 } else { 24 };
        let mut sections = Vec::new();
        let mut at = 0x1c;
        while data.u32_at(at)? != 0 {
            let tag: [u8; 4] = data[at..at + 4].try_into().unwrap();
            let kind = data.u32_at(at + 8)?;
            if kind != 1 && kind != 2 {
                return Err(format!("unexpected directory section header at {at:#x}"));
            }
            let first = at + 12 + data.u32_at(at + 12)? as usize;
            sections.push((tag, kind, first, data.u32_at(at + 16)? as usize));
            at += head_size;
        }
        let mut entries = Vec::new();
        let mut raw_image_size = 0usize;
        for (_, kind, first, count) in &sections {
            let step = if *kind == 1 { 12 } else { 16 };
            for i in 0..*count {
                raw_image_size = raw_image_size
                    .checked_add(data.u32_at(*first + i * step + 8)? as usize)
                    .ok_or("image directory size overflow")?;
            }
        }
        // Flash's globalassets.apk packs its texture structures contiguously even though
        // the IMG header advertises 16-byte alignment. Other APKF blobs align each entry.
        let contiguous = raw_image_size == image_size;
        let (mut offset, mut bulk_offset) = (0usize, 0usize);
        for (tag, kind, first, count) in sections {
            let step = if kind == 1 { 12 } else { 16 };
            for i in 0..count {
                let at = first + i * step;
                let name = data.cstr_at(at + data.u32_at(at)? as usize)?.to_owned();
                let size = data.u32_at(at + 8)? as usize;
                if !contiguous {
                    offset = offset.div_ceil(image_align) * image_align;
                }
                let bulk_size = if kind == 2 { data.u32_at(at + 12)? as usize } else { 0 };
                entries.push(Entry {
                    tag,
                    name,
                    id: data.u32_at(at + 4)?,
                    image_offset: offset,
                    image_size: size,
                    bulk_offset,
                    bulk_size,
                });
                offset += size;
                bulk_offset = bulk_offset.checked_add(bulk_size).ok_or("bulk directory size overflow")?;
            }
        }
        if offset != image_size {
            return Err(format!("directory sizes add up to {offset:#x}, the image block is {image_size:#x}"));
        }
        Ok(entries)
    }

    /// Reads a pointer field of the image block.
    pub fn pointer(&self, at: usize) -> Result<Pointer, String> {
        let word = self.image.u32_at(at)?;
        if !self.pointer_words.contains(&(at / 4)) {
            return Ok(Pointer::Null);
        }
        let target = (word & 0x00ff_ffff) as usize * 4;
        Ok(match word >> 24 {
            BLOCK_IMAGE => Pointer::Image(target),
            BLOCK_BULK => Pointer::Bulk(target),
            _ => Pointer::Slot,
        })
    }

    pub fn image_pointer(&self, at: usize) -> Result<Option<usize>, String> {
        match self.pointer(at)? {
            Pointer::Image(target) => Ok(Some(target)),
            Pointer::Null => Ok(None),
            other => Err(format!("pointer at {at:#x} is {other:?}, expected one into the image block")),
        }
    }

    pub fn bulk_pointer(&self, at: usize) -> Result<usize, String> {
        match self.pointer(at)? {
            Pointer::Bulk(target) => Ok(target),
            other => Err(format!("pointer at {at:#x} is {other:?}, expected one into the bulk block")),
        }
    }

    /// The asset a field of the image block must be pointed at, if the tail lists one.
    pub fn reference(&self, at: usize) -> Option<u32> {
        self.references.get(&at).copied()
    }

    /// The directory entry whose structure starts at this image offset.
    pub fn entry_at(&self, image_offset: usize) -> Option<&Entry> {
        self.entries.iter().find(|e| e.image_offset == image_offset)
    }
}
