//! Device-info feature reports 4..0x0E: 32 B, byte 0 = report ID [DOC].

use crate::protocol::{IMAGE_CMD, ProtocolError};

pub const REPORT_FIRMWARE: u8 = 0x04; // 0x05 is also accepted
pub const REPORT_FIRMWARE_ALT: u8 = 0x05;
pub const REPORT_SECONDARY: u8 = 0x06; // bootloader?
pub const REPORT_SERIAL: u8 = 0x07;
pub const REPORT_GEOMETRY: u8 = 0x09;

fn check_id(buf: &[u8], rid: u8) -> Result<(), ProtocolError> {
    // byte 1 == rid means the backend prepended the requested id (HidSharp)
    if buf.len() < 2 || buf[0] != rid || buf[1] == rid {
        return Err(ProtocolError::BadReportId { expected: rid, got: buf.first().copied() });
    }
    Ok(())
}

/// ASCII bytes -> `&str` borrowed from the report, minus `trim` characters.
/// Non-ASCII is an error rather than being replaced with U+FFFD: a borrowed
/// `&str` can't hold replacement characters without allocating.
fn ascii_text<'a>(bytes: &'a [u8], trim: &[char]) -> Result<&'a str, ProtocolError> {
    if !bytes.is_ascii() {
        return Err(ProtocolError::NotAscii);
    }
    let s = core::str::from_utf8(bytes).map_err(|_| ProtocolError::NotAscii)?;
    Ok(s.trim_matches(trim))
}

const VERSION_TRIM: &[char] = &[' ', '\0', '\x1c', '\x1d', '\x1e', '\x1f'];

/// Version layout: `[id, len, tag: 4 bytes, text...]`, text ends at `2 + len`.
fn version(buf: &[u8], rid: u8) -> Result<&str, ProtocolError> {
    check_id(buf, rid)?;
    let end = (2 + buf[1] as usize).min(buf.len());
    let text = buf.get(6..end).unwrap_or(&[]);
    ascii_text(text, VERSION_TRIM)
}

/// Report 0x04 / 0x05.
pub fn firmware_version(buf: &[u8]) -> Result<&str, ProtocolError> {
    let rid = match buf.first() {
        Some(&b) if b == REPORT_FIRMWARE || b == REPORT_FIRMWARE_ALT => b,
        _ => REPORT_FIRMWARE,
    };
    version(buf, rid)
}

/// Report 0x06.
pub fn secondary_version(buf: &[u8]) -> Result<&str, ProtocolError> {
    version(buf, REPORT_SECONDARY)
}

/// Report 0x07: `[id, len, ascii...]`.
pub fn serial(buf: &[u8]) -> Result<&str, ProtocolError> {
    check_id(buf, REPORT_SERIAL)?;
    let end = (2 + buf[1] as usize).min(buf.len());
    ascii_text(&buf[2..end], &['\0', ' '])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub width: u16,
    pub height: u16,
    pub bytes_per_pixel: u8,
    /// Equals `IMAGE_CMD`.
    pub command_byte: u8,
    /// Unexplained: bytes 1 and 2 (byte 2 is 6: buttons?).
    pub unknown_bytes: (u8, u8),
    /// Unexplained: the (72, 48) pair at bytes 3..7.
    pub unknown_pair: (u16, u16),
}

impl Geometry {
    pub fn matches_image_cmd(&self) -> bool {
        self.command_byte == IMAGE_CMD
    }
}

/// Report 0x09: `09 01 06 | 48 00 | 30 00 | 80 02 | 30 00 | 04 | 05`
pub fn geometry(buf: &[u8]) -> Result<Geometry, ProtocolError> {
    check_id(buf, REPORT_GEOMETRY)?;
    if buf.len() < 13 {
        return Err(ProtocolError::ReportTooShort { needed: 13, got: buf.len() });
    }
    let u16_at = |i: usize| u16::from_le_bytes([buf[i], buf[i + 1]]);
    Ok(Geometry {
        width: u16_at(7),
        height: u16_at(9),
        bytes_per_pixel: buf[11],
        command_byte: buf[12],
        unknown_bytes: (buf[1], buf[2]),
        unknown_pair: (u16_at(3), u16_at(5)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn padded(bytes: &[u8]) -> [u8; 32] {
        let mut r = [0u8; 32];
        r[..bytes.len()].copy_from_slice(bytes);
        r
    }

    #[test]
    fn geometry_parses_documented_report() {
        let r = padded(&[9, 1, 6, 0x48, 0, 0x30, 0, 0x80, 2, 0x30, 0, 4, 5]);
        let g = geometry(&r).unwrap();
        assert_eq!((g.width, g.height), (640, 48));
        assert_eq!((g.bytes_per_pixel, g.command_byte), (4, 5));
        assert_eq!(g.unknown_bytes, (1, 6));
        assert_eq!(g.unknown_pair, (72, 48));
        assert!(g.matches_image_cmd());
        assert_eq!(geometry(&r[..10]), Err(ProtocolError::ReportTooShort { needed: 13, got: 10 }));
    }

    #[test]
    fn versions_and_serial() {
        let r = padded(&[4, 8, 1, 2, 3, 4, b'1', b'.', b'2', b' ']);
        assert_eq!(firmware_version(&r), Ok("1.2"));
        let r = padded(&[5, 6, 0, 0, 0, 0, b'9', 0x1f]);
        assert_eq!(firmware_version(&r), Ok("9"));
        let r = padded(&[6, 7, 0, 0, 0, 0, b'b']);
        assert_eq!(secondary_version(&r), Ok("b"));
        let r = padded(&[7, 5, b'A', b'B', b'1', 0, 0]);
        assert_eq!(serial(&r), Ok("AB1"));
    }

    #[test]
    fn rejects_bad_ids_and_non_ascii() {
        let r = padded(&[7, 7, b'x']); // byte 1 == id: backend prepended it
        assert_eq!(serial(&r), Err(ProtocolError::BadReportId { expected: 7, got: Some(7) }));
        assert_eq!(serial(&[]), Err(ProtocolError::BadReportId { expected: 7, got: None }));
        let r = padded(&[7, 2, 0xC3, 0xA9]);
        assert_eq!(serial(&r), Err(ProtocolError::NotAscii));
        // length byte past the end of the buffer is clamped, not a panic
        assert_eq!(serial(&[7, 200, b'Z']), Ok("Z"));
    }
}
