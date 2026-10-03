// Minimal KTX1 validation for the ASTC textures we produce. This is a fresh
// implementation from the KTX1 spec, not derived from the GPL validate_ktx.cpp.

use std::path::Path;

pub const KTX1_IDENTIFIER: [u8; 12] = [
    0xAB, b'K', b'T', b'X', b' ', b'1', b'1', 0xBB, 0x0D, 0x0A, 0x1A, 0x0A,
];

/// GL_COMPRESSED_RGBA_ASTC_*_KHR and the sRGB variants, in block-size order.
const ASTC_LDR_BASE: u32 = 0x93B0;
const ASTC_SRGB_BASE: u32 = 0x93D0;

const ASTC_BLOCK_DIMS: [(u32, u32); 14] = [
    (4, 4),
    (5, 4),
    (5, 5),
    (6, 5),
    (6, 6),
    (8, 5),
    (8, 6),
    (8, 8),
    (10, 5),
    (10, 6),
    (10, 8),
    (10, 10),
    (12, 10),
    (12, 12),
];

fn astc_block_dims(internal_format: u32) -> Option<(u32, u32)> {
    let idx = if (ASTC_LDR_BASE..ASTC_LDR_BASE + 14).contains(&internal_format) {
        internal_format - ASTC_LDR_BASE
    } else if (ASTC_SRGB_BASE..ASTC_SRGB_BASE + 14).contains(&internal_format) {
        internal_format - ASTC_SRGB_BASE
    } else {
        return None;
    };
    ASTC_BLOCK_DIMS.get(idx as usize).copied()
}

fn u32_le(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn align4(n: usize) -> usize {
    (n + 3) & !3
}

/// Validate a KTX1 ASTC 2D texture. Returns a human error on any problem.
pub fn validate_bytes(data: &[u8]) -> Result<(), String> {
    if data.len() < 64 {
        return Err("file smaller than KTX1 header".into());
    }
    if data[0..12] != KTX1_IDENTIFIER {
        return Err("not a KTX1 file (bad identifier)".into());
    }
    let endianness = u32_le(data, 12);
    if endianness != 0x0403_0201 {
        return Err(format!("unsupported KTX endianness {:#010x}", endianness));
    }

    let gl_type = u32_le(data, 16);
    let gl_format = u32_le(data, 24);
    let internal_format = u32_le(data, 28);
    let width = u32_le(data, 36);
    let height = u32_le(data, 40);
    let depth = u32_le(data, 44);
    let array_elems = u32_le(data, 48);
    let faces = u32_le(data, 52);
    let mips = u32_le(data, 56);
    let kv_bytes = u32_le(data, 60) as usize;

    if gl_type != 0 || gl_format != 0 {
        return Err("uncompressed glType/glFormat; expected ASTC".into());
    }
    let Some((bx, by)) = astc_block_dims(internal_format) else {
        return Err(format!(
            "internalFormat {:#x} is not ASTC LDR/sRGB",
            internal_format
        ));
    };
    if width == 0 || height == 0 {
        return Err("zero width or height".into());
    }
    if width > 16384 || height > 16384 {
        return Err(format!("texture too large: {}x{}", width, height));
    }
    if depth > 1 {
        return Err("3D ASTC textures are not expected".into());
    }
    if faces != 1 {
        return Err(format!("faces must be 1, got {faces}"));
    }
    if array_elems > 1 {
        return Err(format!("array elements must be 0/1, got {array_elems}"));
    }
    if mips == 0 || mips > 16 {
        return Err(format!("bad mip count {mips}"));
    }
    if 64 + kv_bytes > data.len() {
        return Err("key/value data extends past end of file".into());
    }

    let mut off = 64 + kv_bytes;
    for level in 0..mips {
        let w = (width >> level).max(1);
        let h = (height >> level).max(1);
        let blocks = w.div_ceil(bx) * h.div_ceil(by);
        let expect = blocks as usize * 16; // ASTC blocks are 16 bytes

        if off + 4 > data.len() {
            return Err(format!("truncated at mip {level} imageSize"));
        }
        let image_size = u32_le(data, off) as usize;
        off += 4;
        if image_size == 0 {
            return Err(format!("mip {level} has zero image size"));
        }
        if image_size != expect {
            return Err(format!(
                "mip {level} size {image_size} != expected {expect} for {}x{}",
                w, h
            ));
        }
        off += align4(image_size);
        if off > data.len() {
            return Err(format!("truncated at mip {level} payload"));
        }
    }
    Ok(())
}

pub fn validate_file(path: &Path) -> Result<(), String> {
    let data = std::fs::read(path).map_err(|e| format!("read failed: {e}"))?;
    validate_bytes(&data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_dims_map() {
        assert_eq!(astc_block_dims(0x93B0), Some((4, 4)));
        assert_eq!(astc_block_dims(0x93B4), Some((6, 6)));
        assert_eq!(astc_block_dims(0x93BD), Some((12, 12)));
        assert_eq!(astc_block_dims(0x93DD), Some((12, 12)));
        assert_eq!(astc_block_dims(0x1234), None);
    }

    #[test]
    fn rejects_garbage() {
        assert!(validate_bytes(b"nope").is_err());
        assert!(validate_bytes(&[0u8; 200]).is_err());
    }

    fn tiny_astc6x6() -> Vec<u8> {
        // 6x6 ASTC -> one block, 16 bytes.
        let mut v = vec![0u8; 64];
        v[0..12].copy_from_slice(&KTX1_IDENTIFIER);
        v[12..16].copy_from_slice(&0x0403_0201u32.to_le_bytes());
        v[28..32].copy_from_slice(&0x93B4u32.to_le_bytes()); // ASTC 6x6
        v[36..40].copy_from_slice(&6u32.to_le_bytes());
        v[40..44].copy_from_slice(&6u32.to_le_bytes());
        v[52..56].copy_from_slice(&1u32.to_le_bytes()); // faces
        v[56..60].copy_from_slice(&1u32.to_le_bytes()); // mips
        v[60..64].copy_from_slice(&0u32.to_le_bytes()); // kv
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&[0u8; 16]);
        v
    }

    #[test]
    fn accepts_minimal_valid() {
        assert!(validate_bytes(&tiny_astc6x6()).is_ok());
    }

    #[test]
    fn rejects_truncated_payload() {
        let mut v = tiny_astc6x6();
        v.truncate(v.len() - 4);
        assert!(validate_bytes(&v).is_err());
    }
}
