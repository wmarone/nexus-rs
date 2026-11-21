pub const NEXUS_WIDTH: usize = 640;
pub const NEXUS_HEIGHT: usize = 48;

pub const NEXUS_BYTES_PER_PIXEL: usize = 4; // B, G, R, A

pub const NEXUS_FRAME_BYTES: usize = NEXUS_WIDTH * NEXUS_HEIGHT * NEXUS_BYTES_PER_PIXEL; // 122880


pub const NEXUS_RGB_BYTES: usize = NEXUS_WIDTH * NEXUS_HEIGHT * 3; // 92160

use crate::protocol::ProtocolError;

fn check(got: usize, expected: usize) -> Result<(), ProtocolError> {
    if got != expected {
        return Err(ProtocolError::BadFrameSize { expected, got });
    }
    Ok(())
}

/// [CAPTURE] Device pixel order is B,G,R,A with A always 0xFF.
/// Writes tightly packed R,G,B into `rgb` (alpha dropped). No allocation.
pub fn bgra_to_rgb(bgra: &[u8], rgb: &mut [u8]) -> Result<(), ProtocolError> {
    check(bgra.len(), NEXUS_FRAME_BYTES)?;
    check(rgb.len(), NEXUS_RGB_BYTES)?;
    for (src, dst) in bgra.chunks_exact(4).zip(rgb.chunks_exact_mut(3)) {
        dst.copy_from_slice(&[src[2], src[1], src[0]]);
    }
    Ok(())
}

/// Inverse of `bgra_to_rgb`; alpha is set to 0xFF.
/// [DOC] The panel is 6 bits/channel; dither before calling if it matters.
pub fn rgb_to_bgra(rgb: &[u8], bgra: &mut [u8]) -> Result<(), ProtocolError> {
    check(rgb.len(), NEXUS_RGB_BYTES)?;
    check(bgra.len(), NEXUS_FRAME_BYTES)?;
    for (src, dst) in rgb.chunks_exact(3).zip(bgra.chunks_exact_mut(4)) {
        dst.copy_from_slice(&[src[2], src[1], src[0], 0xFF]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgra_rgb_roundtrip() {
        let mut bgra = vec![0u8; NEXUS_FRAME_BYTES];
        bgra[..4].copy_from_slice(&[10, 20, 30, 0xFF]); // B,G,R,A
        let mut rgb = vec![0u8; NEXUS_RGB_BYTES];
        bgra_to_rgb(&bgra, &mut rgb).unwrap();
        assert_eq!(rgb[..3], [30, 20, 10]);

        let mut back = vec![0u8; NEXUS_FRAME_BYTES];
        rgb_to_bgra(&rgb, &mut back).unwrap();
        assert_eq!(back[..4], [10, 20, 30, 0xFF]);
    }

    #[test]
    fn rejects_wrong_sizes() {
        let mut rgb = vec![0u8; NEXUS_RGB_BYTES];
        assert!(bgra_to_rgb(&[0; 4], &mut rgb).is_err());
        assert!(rgb_to_bgra(&rgb, &mut [0; 4]).is_err());
    }
}
