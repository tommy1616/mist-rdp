//! Pure-Rust BMP <-> Windows CF_DIB (ClipboardFormatId 8) conversion helpers.
//! Compiled on all platforms so `cargo test` validates image clipboard conversion on both Windows & macOS.

/// Convert a full Windows BMP file (14-byte BITMAPFILEHEADER + DIB) to raw CF_DIB bytes.
#[allow(dead_code)]
pub fn bmp_to_dib(bmp: &[u8]) -> Option<Vec<u8>> {
    if bmp.len() < 14 + 40 {
        return None;
    }
    if &bmp[0..2] != b"BM" {
        return None;
    }
    let dib = &bmp[14..];
    let bi_size = u32::from_le_bytes(dib[0..4].try_into().ok()?) as usize;
    if bi_size < 40 || dib.len() < bi_size {
        return None;
    }
    Some(dib.to_vec())
}

/// Wrap raw CF_DIB bytes with a 14-byte BITMAPFILEHEADER so macOS `sips` / image tools can decode it as BMP.
#[allow(dead_code)]
pub fn dib_to_bmp(dib: &[u8]) -> Option<Vec<u8>> {
    if dib.len() < 40 {
        return None;
    }
    let bi_size = u32::from_le_bytes(dib[0..4].try_into().ok()?);
    if bi_size < 40 || (bi_size as usize) > dib.len() {
        return None;
    }

    let bit_count = u16::from_le_bytes(dib[14..16].try_into().ok()?);
    let compression = u32::from_le_bytes(dib[16..20].try_into().ok()?);
    let clr_used = u32::from_le_bytes(dib[32..36].try_into().ok()?);

    // Extra bytes between BITMAPINFOHEADER and pixel array (color palette or bitfield masks)
    let extra_bytes: u32 = if bi_size == 40 && compression == 3 {
        // BI_BITFIELDS (RGB masks: 3 x u32)
        12
    } else if bi_size == 40 && compression == 6 {
        // BI_ALPHABITFIELDS (RGBA masks: 4 x u32)
        16
    } else if bit_count <= 8 {
        let colors = if clr_used > 0 {
            clr_used
        } else {
            1u32.checked_shl(u32::from(bit_count))?
        };
        colors.checked_mul(4)?
    } else if clr_used > 0 {
        clr_used.checked_mul(4)?
    } else {
        0
    };

    let file_size = u32::try_from(14usize.checked_add(dib.len())?).ok()?;
    let raw_offset = 14u32.checked_add(bi_size)?.checked_add(extra_bytes)?;
    let pixel_offset = raw_offset.min(file_size);

    let mut out = Vec::with_capacity(file_size as usize);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&file_size.to_le_bytes());
    out.extend_from_slice(&[0u8; 4]); // bfReserved1 + bfReserved2
    out.extend_from_slice(&pixel_offset.to_le_bytes());
    out.extend_from_slice(dib);
    Some(out)
}

/// Fast 64-bit FNV-1a hash for clipboard deduplication.
#[allow(dead_code)]
pub fn fnv1a_64(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_dib_32bit(compression: u32, extra_mask_bytes: usize) -> Vec<u8> {
        let mut dib = vec![0u8; 40 + extra_mask_bytes + 4];
        dib[0..4].copy_from_slice(&40u32.to_le_bytes()); // biSize
        dib[4..8].copy_from_slice(&1i32.to_le_bytes()); // biWidth = 1
        dib[8..12].copy_from_slice(&1i32.to_le_bytes()); // biHeight = 1
        dib[12..14].copy_from_slice(&1u16.to_le_bytes()); // biPlanes = 1
        dib[14..16].copy_from_slice(&32u16.to_le_bytes()); // biBitCount = 32
        dib[16..20].copy_from_slice(&compression.to_le_bytes());
        // pixel BGRA
        let px_off = 40 + extra_mask_bytes;
        dib[px_off..px_off + 4].copy_from_slice(&[0x11, 0x22, 0x33, 0xFF]);
        dib
    }

    #[test]
    fn dib_to_bmp_and_back_roundtrip_rgb() {
        let dib = sample_dib_32bit(0, 0);
        let bmp = dib_to_bmp(&dib).expect("valid bmp");
        assert_eq!(&bmp[0..2], b"BM");
        assert_eq!(u32::from_le_bytes(bmp[2..6].try_into().unwrap()) as usize, 14 + dib.len());
        assert_eq!(u32::from_le_bytes(bmp[10..14].try_into().unwrap()), 54);

        let roundtrip = bmp_to_dib(&bmp).expect("valid dib");
        assert_eq!(roundtrip, dib);
    }

    #[test]
    fn dib_to_bmp_handles_bitfields_offset() {
        let dib = sample_dib_32bit(3, 12); // BI_BITFIELDS
        let bmp = dib_to_bmp(&dib).expect("valid bitfields bmp");
        assert_eq!(u32::from_le_bytes(bmp[10..14].try_into().unwrap()), 14 + 40 + 12);
        assert_eq!(bmp_to_dib(&bmp).unwrap(), dib);
    }

    #[test]
    fn invalid_inputs_return_none() {
        assert!(bmp_to_dib(b"not a bmp").is_none());
        assert!(dib_to_bmp(&[0u8; 10]).is_none());
        let mut bad_hdr = vec![0u8; 40];
        bad_hdr[0..4].copy_from_slice(&12u32.to_le_bytes());
        assert!(dib_to_bmp(&bad_hdr).is_none());
    }

    #[test]
    fn fnv1a_differs_on_change() {
        assert_ne!(fnv1a_64(b"hello"), fnv1a_64(b"hello!"));
        assert_eq!(fnv1a_64(b"same"), fnv1a_64(b"same"));
    }
}
