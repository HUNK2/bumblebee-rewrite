//! `.str` packs: a small header, then one zlib stream holding a type table and the chunks.

use std::io::Read;
use std::ops::Range;
use std::path::Path;

use flate2::read::ZlibDecoder;

use super::Bytes;

pub const DATA: u32 = 0x8592_6d4a;
pub const TEXTURE: u32 = 0xb053_e597;
pub const MODEL: u32 = 0x6787_6b72;

pub struct Chunk {
    pub index: usize,
    pub tag: u32,
    pub id: u32,
    range: Range<usize>,
}

pub struct Pack {
    raw: Vec<u8>,
    pub chunks: Vec<Chunk>,
}

impl Pack {
    pub fn open(path: &Path) -> Result<Self, String> {
        let blob = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&blob).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn parse(blob: &[u8]) -> Result<Self, String> {
        if blob.get(..4) != Some(b"mrts") {
            return Err("not a .str pack".into());
        }
        let file_size = blob.u32_at(4)? as usize;
        let type_count = blob.u32_at(12)? as usize;
        let chunk_count = blob.u32_at(16)? as usize;
        if file_size != blob.len() {
            return Err(format!("header says {file_size} bytes, file has {}", blob.len()));
        }
        let mut raw = Vec::new();
        ZlibDecoder::new(&blob[20..]).read_to_end(&mut raw).map_err(|e| format!("inflate failed: {e}"))?;

        let mut chunks = Vec::with_capacity(chunk_count);
        let mut at = type_count * 8;
        while at < raw.len() {
            let tag = raw.u32_at(at)?;
            let id = raw.u32_at(at + 4)?;
            let size = raw.u32_at(at + 12)? as usize;
            let start = at + 16;
            if start + size > raw.len() {
                return Err(format!("chunk {} runs past the end of the pack", chunks.len()));
            }
            chunks.push(Chunk { index: chunks.len(), tag, id, range: start..start + size });
            at = start + size;
        }
        if chunks.len() != chunk_count {
            return Err(format!("walked {} chunks, header says {chunk_count}", chunks.len()));
        }
        Ok(Self { raw, chunks })
    }

    pub fn data(&self, chunk: &Chunk) -> &[u8] {
        &self.raw[chunk.range.clone()]
    }

    pub fn of_type(&self, tag: u32) -> impl Iterator<Item = &Chunk> {
        self.chunks.iter().filter(move |c| c.tag == tag)
    }
}
