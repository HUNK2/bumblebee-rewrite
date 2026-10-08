//! Readers for the game's own file formats. Everything here works on bytes read from the
//! user's install at runtime; nothing is written back and nothing is bundled with the test.

pub mod anim;
pub mod apk;
pub mod environment;
pub mod fev;
pub mod fsb;
pub mod gfx;
pub mod hash;
pub mod lxb;
pub mod model;
pub mod pack;
pub mod scene;
pub mod texture;

/// Little-endian reads that report a short buffer instead of panicking.
pub(crate) trait Bytes {
    fn u16_at(&self, at: usize) -> Result<u16, String>;
    fn u32_at(&self, at: usize) -> Result<u32, String>;
    fn i32_at(&self, at: usize) -> Result<i32, String>;
    fn f32_at(&self, at: usize) -> Result<f32, String>;
    fn cstr_at(&self, at: usize) -> Result<&str, String>;
}

impl Bytes for [u8] {
    fn u16_at(&self, at: usize) -> Result<u16, String> {
        self.get(at..at + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .ok_or_else(|| format!("read of 2 bytes at {at:#x} runs past the end ({:#x})", self.len()))
    }

    fn u32_at(&self, at: usize) -> Result<u32, String> {
        self.get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .ok_or_else(|| format!("read of 4 bytes at {at:#x} runs past the end ({:#x})", self.len()))
    }

    fn i32_at(&self, at: usize) -> Result<i32, String> {
        self.u32_at(at).map(|v| v as i32)
    }

    fn f32_at(&self, at: usize) -> Result<f32, String> {
        self.u32_at(at).map(f32::from_bits)
    }

    fn cstr_at(&self, at: usize) -> Result<&str, String> {
        let tail = self.get(at..).ok_or_else(|| format!("string at {at:#x} starts past the end"))?;
        let len = tail.iter().position(|&b| b == 0).ok_or_else(|| format!("string at {at:#x} is not terminated"))?;
        std::str::from_utf8(&tail[..len]).map_err(|_| format!("string at {at:#x} is not plain text"))
    }
}
