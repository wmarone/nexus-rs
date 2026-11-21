use crate::screen::NEXUS_FRAME_BYTES;

// USB IDs
pub const CORSAIR_VID: u16 = 0x1B1C;
pub const NEXUS_PID: u16 = 0x1B8E;

// USB Interfaces
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NexusUsbInterfaces {
    Main = 0,
    Keyboard = 1,
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NexusEndpoints {
    TouchIn = 0x81,
    ImageOut = 0x02,
    KeyboardIn = 0x83,
}

/// HID report IDs.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NexusReports {
    /// input, 511 B (512 on the wire with the ID) [TOUCH-CAP]
    TouchIn = 1,
    /// output, 1023 B (1024 on the wire with the ID) [CAPTURE]
    ImageOut = 2,
    /// feature, 31 B (32 with the ID) [DOC]
    Command = 3,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProtocolError {
    BadFrameSize { expected: usize, got: usize },
    BadPacketSize { got: usize },
    NotImagePacket { report_id: u8, cmd: u8 },
    MalformedHeader,
    /// Caller-supplied buffer is smaller than a frame.
    BufferTooSmall { needed: usize, got: usize },
    /// A command argument outside what the firmware accepts.
    OutOfRange { what: &'static str },
    /// Feature report didn't start with the expected ID (or a backend
    /// prepended an extra one; HidSharp does). `got` is None if empty.
    BadReportId { expected: u8, got: Option<u8> },
    ReportTooShort { needed: usize, got: usize },
    /// Text field held non-ASCII bytes.
    NotAscii,
    /// Too short, or byte 0 isn't the touch report ID.
    NotTouchReport,
}

// Image Upload [CAPTURE]

pub const IMAGE_CMD: u8 = 0x05;
pub const IMAGE_PACKET_BYTES: usize = 1024;
pub const IMAGE_HEADER_BYTES: usize = 8;
pub const IMAGE_CHUNK_BYTES: usize = IMAGE_PACKET_BYTES - IMAGE_HEADER_BYTES; // 1016
pub const IMAGE_PACKETS: usize = 121; // 120 * 1016 + 960

/// Constant in the capture; [DOC] says don't-care.
pub const IMAGE_BYTE2: u8 = 0x40;

pub type ImagePacket = [u8; IMAGE_PACKET_BYTES];

/// Iterator over the 121 output-report packets of one frame. No allocation:
/// each packet is built on the stack when `next()` is called.
#[derive(Debug, Clone)]
pub struct ImagePackets<'a> {
    frame: &'a [u8],
    pad: &'a [u8],
    n: usize,
}

/// Split a BGRA frame into the 121 output-report packets.
///
/// Packet layout (little-endian):
/// `[report_id, cmd, byte2, final, block: u16, length: u16, payload..., padding]`
///
/// iCUE always sends 1024 bytes, including the last block (960 payload bytes,
/// then 56 bytes of stale buffer). `pad` fills that tail (truncated if too
/// long); empty means zeros, which [DOC] hardware tests accept.
pub fn image_packets<'a>(frame: &'a [u8], pad: &'a [u8]) -> Result<ImagePackets<'a>, ProtocolError> {
    if frame.len() != NEXUS_FRAME_BYTES {
        return Err(ProtocolError::BadFrameSize {
            expected: NEXUS_FRAME_BYTES,
            got: frame.len(),
        });
    }
    Ok(ImagePackets { frame, pad, n: 0 })
}

impl Iterator for ImagePackets<'_> {
    type Item = ImagePacket;

    fn next(&mut self) -> Option<ImagePacket> {
        if self.n >= IMAGE_PACKETS {
            return None;
        }
        let n = self.n;
        self.n += 1;

        let start = n * IMAGE_CHUNK_BYTES;
        let chunk = &self.frame[start..(start + IMAGE_CHUNK_BYTES).min(self.frame.len())];
        let last = n == IMAGE_PACKETS - 1;
        let mut pkt = [0u8; IMAGE_PACKET_BYTES]; // zero-filled: padding is free

        pkt[0] = NexusReports::ImageOut as u8;
        pkt[1] = IMAGE_CMD;
        pkt[2] = IMAGE_BYTE2;
        pkt[3] = last as u8;
        pkt[4..6].copy_from_slice(&(n as u16).to_le_bytes());
        pkt[6..8].copy_from_slice(&(chunk.len() as u16).to_le_bytes());

        let end = IMAGE_HEADER_BYTES + chunk.len();
        pkt[IMAGE_HEADER_BYTES..end].copy_from_slice(chunk);

        if last {
            let k = self.pad.len().min(IMAGE_PACKET_BYTES - end);
            pkt[end..end + k].copy_from_slice(&self.pad[..k]);
        }
        Some(pkt)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = IMAGE_PACKETS - self.n;
        (left, Some(left))
    }
}

impl ExactSizeIterator for ImagePackets<'_> {}

/// A decoded image packet. `payload` borrows from the raw packet (no copy).
#[derive(Debug, PartialEq, Eq)]
pub struct ParsedImagePacket<'a> {
    pub block: u16,
    pub is_final: bool, // `final` is a reserved word in Rust
    pub payload: &'a [u8], // its length is the header's length field
    pub byte2: u8,
}

pub fn parse_image_packet(pkt: &[u8]) -> Result<ParsedImagePacket<'_>, ProtocolError> {
    if pkt.len() != IMAGE_PACKET_BYTES {
        return Err(ProtocolError::BadPacketSize { got: pkt.len() });
    }
    let (report_id, cmd, byte2, fin) = (pkt[0], pkt[1], pkt[2], pkt[3]);
    let block = u16::from_le_bytes([pkt[4], pkt[5]]);
    let length = u16::from_le_bytes([pkt[6], pkt[7]]) as usize;

    if report_id != NexusReports::ImageOut as u8 || cmd != IMAGE_CMD {
        return Err(ProtocolError::NotImagePacket { report_id, cmd });
    }
    if fin > 1 || length > IMAGE_CHUNK_BYTES {
        return Err(ProtocolError::MalformedHeader);
    }
    Ok(ParsedImagePacket {
        block,
        is_final: fin == 1,
        payload: &pkt[IMAGE_HEADER_BYTES..IMAGE_HEADER_BYTES + length],
        byte2,
    })
}

/// Reassembles packets into frames. Feed ACKed packets in bus order.
///
/// Works in a caller-supplied buffer (at least `NEXUS_FRAME_BYTES`), so the
/// crate itself never allocates.
#[derive(Debug)]
pub struct FrameAssembler<'a> {
    buf: &'a mut [u8],
    len: usize,
    next_block: u16,
}

impl<'a> FrameAssembler<'a> {
    pub fn new(buf: &'a mut [u8]) -> Result<Self, ProtocolError> {
        if buf.len() < NEXUS_FRAME_BYTES {
            return Err(ProtocolError::BufferTooSmall {
                needed: NEXUS_FRAME_BYTES,
                got: buf.len(),
            });
        }
        Ok(Self { buf, len: 0, next_block: 0 })
    }

    /// Returns `Ok(Some(frame))` when a packet completes a full-size frame.
    /// The frame borrows the assembler's buffer, so it is valid until the
    /// next `feed`. An out-of-order block drops the partial frame and waits
    /// for block 0.
    pub fn feed(&mut self, pkt: &[u8]) -> Result<Option<&[u8]>, ProtocolError> {
        let p = parse_image_packet(pkt)?;
        if p.block == 0 {
            self.reset();
        }
        if p.block != self.next_block || self.len + p.payload.len() > NEXUS_FRAME_BYTES {
            self.reset(); // lost sync
            return Ok(None);
        }
        self.buf[self.len..self.len + p.payload.len()].copy_from_slice(p.payload);
        self.len += p.payload.len();
        self.next_block += 1;
        if p.is_final {
            let done = self.len == NEXUS_FRAME_BYTES;
            self.len = 0;
            self.next_block = 0;
            return Ok(done.then(|| &self.buf[..NEXUS_FRAME_BYTES]));
        }
        Ok(None)
    }

    fn reset(&mut self) {
        self.len = 0;
        self.next_block = 0;
    }
}

// Commands: feature report 3 [DOC]

/// Command bytes (byte 1 of feature report 3).
///
/// [TOUCH-CAP] iCUE's `03 01 40 DD 83 1C 32 2E ...` is `Brightness` 0x40 sent
/// from a reused buffer: bytes 3+ are stale from the preceding report-5
/// reply, not extra arguments. Pad with zeros instead.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    /// `<0-100>`
    Brightness = 0x01,
    Blank = 0x04,
    /// `<1-3> <loop 0/1>`
    Animation = 0x0D,
    /// [TOUCH-CAP] `03 0F 01` is the first iCUE command
    StopAnimation = 0x0F,
    /// [TOUCH-CAP] `03 10 01`, sent right after status 0C goes 00 -> 01
    Unknown10 = 0x10,
}

pub const COMMAND_REPORT_BYTES: usize = 32;
pub type CommandReport = [u8; COMMAND_REPORT_BYTES];

/// Feature report 3, already zero-padded to its 32-byte size, so a transport
/// can send it as is.
fn command(cmd: Cmd, args: &[u8]) -> CommandReport {
    let mut r = [0u8; COMMAND_REPORT_BYTES];
    r[0] = NexusReports::Command as u8;
    r[1] = cmd as u8;
    r[2..2 + args.len()].copy_from_slice(args);
    r
}

pub fn brightness(percent: u8) -> Result<CommandReport, ProtocolError> {
    if percent > 100 {
        return Err(ProtocolError::OutOfRange { what: "brightness is 0-100" });
    }
    Ok(command(Cmd::Brightness, &[percent]))
}

pub fn blank() -> CommandReport {
    command(Cmd::Blank, &[])
}

pub fn play_animation(n: u8, looping: bool) -> Result<CommandReport, ProtocolError> {
    if !(1..=3).contains(&n) {
        return Err(ProtocolError::OutOfRange { what: "firmware has animations 1-3" });
    }
    Ok(command(Cmd::Animation, &[n, looping as u8]))
}

pub fn stop_animation() -> CommandReport {
    command(Cmd::StopAnimation, &[])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packets(frame: &[u8]) -> Vec<ImagePacket> {
        image_packets(frame, &[]).unwrap().collect()
    }

    #[test]
    fn splits_frame_into_121_packets() {
        let frame = vec![0xAB; NEXUS_FRAME_BYTES];
        let it = image_packets(&frame, &[]).unwrap();
        assert_eq!(it.len(), IMAGE_PACKETS);
        let pkts = packets(&frame);
        assert_eq!(pkts.len(), IMAGE_PACKETS);

        assert_eq!(pkts[0][..8], [2, 0x05, 0x40, 0, 0, 0, 0xF8, 0x03]); // len 1016 = 0x03F8
        let last = &pkts[120];
        assert_eq!(last[..8], [2, 0x05, 0x40, 1, 120, 0, 0xC0, 0x03]); // len 960 = 0x03C0
        assert_eq!(last[8 + 959], 0xAB);
        assert_eq!(last[8 + 960], 0); // zero padding
    }

    #[test]
    fn pad_fills_and_truncates_tail() {
        let frame = vec![0; NEXUS_FRAME_BYTES];
        let last = image_packets(&frame, &[0xEE; 100]).unwrap().last().unwrap();
        assert_eq!(last[968], 0xEE);
        assert_eq!(last[1023], 0xEE);
    }

    #[test]
    fn rejects_wrong_size() {
        assert_eq!(
            image_packets(&[0; 10], &[]).unwrap_err(),
            ProtocolError::BadFrameSize { expected: NEXUS_FRAME_BYTES, got: 10 }
        );
    }

    #[test]
    fn parse_roundtrips_image_packets() {
        let frame: Vec<u8> = (0..NEXUS_FRAME_BYTES).map(|i| i as u8).collect();
        let mut rebuilt = Vec::new();
        for (n, raw) in packets(&frame).iter().enumerate() {
            let p = parse_image_packet(raw).unwrap();
            assert_eq!(p.block as usize, n);
            assert_eq!(p.is_final, n == IMAGE_PACKETS - 1);
            assert_eq!(p.byte2, IMAGE_BYTE2);
            rebuilt.extend_from_slice(p.payload);
        }
        assert_eq!(rebuilt, frame);
    }

    #[test]
    fn parse_rejects_bad_packets() {
        assert_eq!(parse_image_packet(&[0; 10]), Err(ProtocolError::BadPacketSize { got: 10 }));
        let mut pkt = [0u8; IMAGE_PACKET_BYTES];
        assert_eq!(
            parse_image_packet(&pkt),
            Err(ProtocolError::NotImagePacket { report_id: 0, cmd: 0 })
        );
        pkt[0] = 2;
        pkt[1] = IMAGE_CMD;
        pkt[3] = 2; // final must be 0 or 1
        assert_eq!(parse_image_packet(&pkt), Err(ProtocolError::MalformedHeader));
        pkt[3] = 0;
        pkt[6..8].copy_from_slice(&1017u16.to_le_bytes()); // too long
        assert_eq!(parse_image_packet(&pkt), Err(ProtocolError::MalformedHeader));
    }

    #[test]
    fn assembler_rebuilds_frame() {
        let frame: Vec<u8> = (0..NEXUS_FRAME_BYTES).map(|i| (i * 7) as u8).collect();
        let mut buf = vec![0u8; NEXUS_FRAME_BYTES];
        let mut asm = FrameAssembler::new(&mut buf).unwrap();
        let mut got = None;
        for pkt in packets(&frame) {
            if let Some(f) = asm.feed(&pkt).unwrap() {
                got = Some(f.to_vec());
            }
        }
        assert_eq!(got, Some(frame));
    }

    #[test]
    fn assembler_drops_on_gap_and_resyncs() {
        let frame = vec![1u8; NEXUS_FRAME_BYTES];
        let pkts = packets(&frame);
        let mut buf = vec![0u8; NEXUS_FRAME_BYTES];
        let mut asm = FrameAssembler::new(&mut buf).unwrap();
        for (n, pkt) in pkts.iter().enumerate() {
            if n == 50 {
                continue; // lost packet
            }
            assert_eq!(asm.feed(pkt).unwrap(), None);
        }
        let mut got = None;
        for pkt in &pkts {
            if let Some(f) = asm.feed(pkt).unwrap() {
                got = Some(f.to_vec());
            }
        }
        assert_eq!(got, Some(frame));
    }

    #[test]
    fn assembler_rejects_small_buffer() {
        let mut buf = [0u8; 10];
        assert_eq!(
            FrameAssembler::new(&mut buf).unwrap_err(),
            ProtocolError::BufferTooSmall { needed: NEXUS_FRAME_BYTES, got: 10 }
        );
    }

    #[test]
    fn command_reports() {
        assert_eq!(brightness(100).unwrap()[..3], [3, 0x01, 100]);
        assert_eq!(brightness(101), Err(ProtocolError::OutOfRange { what: "brightness is 0-100" }));
        assert_eq!(blank()[..3], [3, 0x04, 0]);
        assert_eq!(play_animation(2, true).unwrap()[..4], [3, 0x0D, 2, 1]);
        assert!(play_animation(0, false).is_err());
        assert!(play_animation(4, false).is_err());
        assert_eq!(stop_animation()[..2], [3, 0x0F]);
        assert_eq!(blank().len(), 32);
    }
}
