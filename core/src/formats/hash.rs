//! The two name hashes the game uses.

const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
};

/// Standard CRC-32, case sensitive: field, enum and object names in data objects.
pub const fn crc32(bytes: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    let mut i = 0;
    while i < bytes.len() {
        c = CRC_TABLE[((c ^ bytes[i] as u32) & 0xff) as usize] ^ (c >> 8);
        i += 1;
    }
    !c
}

/// Multiply-by-33 over the lower-cased name: asset ids of meshes, materials and textures, and
/// the second ("tl") name a hierarchy node carries, which is what skeleton bones are matched by.
pub fn lower33(name: &str) -> u32 {
    name.bytes().fold(0u32, |h, b| h.wrapping_mul(33).wrapping_add(b.to_ascii_lowercase() as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_reference_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn lower33_matches_a_known_bone() {
        assert_eq!(lower33("Hips"), 0x003a_d4f4);
    }
}
