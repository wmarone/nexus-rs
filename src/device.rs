//! Device layer: the `Transport` trait a client implements (rusb, hidapi, a
//! mock, ...) and `Nexus<T>`, which drives the protocol over it.
//!
//! The crate owns no USB handle, clock, or thread. Opening the device,
//! claiming interface 0, detaching the kernel driver, permission errors and
//! closing all belong to the client's transport. All calls block.

use crate::device_info::{self, Geometry};
use crate::protocol::{
    self, CommandReport, IMAGE_PACKET_BYTES, NexusReports, ProtocolError,
};
use crate::touch::{self, TOUCH_PACKET_BYTES, Touch};

/// Size of a feature report on the wire (report ID + 31 bytes).
pub const FEATURE_REPORT_BYTES: usize = 32;

/// The I/O the device needs, in protocol terms. Implement this over your USB
/// library of choice.
pub trait Transport {
    type Error;

    /// EP2 OUT: one 1024-byte image packet (report ID 2 in byte 0).
    fn write_image(
        &mut self,
        pkt: &[u8; IMAGE_PACKET_BYTES],
        timeout_ms: u32,
    ) -> Result<(), Self::Error>;

    /// EP1 IN: read one touch report into `buf`, returning its length.
    /// `Ok(None)` means the timeout elapsed with nothing to read; that is
    /// not an error.
    fn read_touch(
        &mut self,
        buf: &mut [u8; TOUCH_PACKET_BYTES],
        timeout_ms: u32,
    ) -> Result<Option<usize>, Self::Error>;

    /// Send a feature report on interface 0 (HID SET_REPORT; on the USB
    /// control pipe: request type 0x21, request 0x09, wValue
    /// `0x0300 | report[0]`, wIndex 0). `report[0]` is the report ID and the
    /// rest is already zero-padded.
    fn set_feature(
        &mut self,
        report: &[u8; FEATURE_REPORT_BYTES],
        timeout_ms: u32,
    ) -> Result<(), Self::Error>;
}

/// Optional capability: reading feature reports (HID GET_REPORT). Only
/// needed for the device-info calls (firmware, serial, geometry); a transport
/// that can't do it just doesn't implement this, and `Nexus` still works for
/// display, touch and commands.
pub trait ReadFeature: Transport {
    /// Fetch feature report `id` into `buf`, returning the length read.
    /// Byte 0 of the result should be the report ID. A backend that prepends
    /// an extra ID should strip it (the parsers reject it otherwise).
    fn get_feature(
        &mut self,
        id: u8,
        buf: &mut [u8; FEATURE_REPORT_BYTES],
        timeout_ms: u32,
    ) -> Result<usize, Self::Error>;
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error<E> {
    Protocol(ProtocolError),
    Transport(E),
}

impl<E> From<ProtocolError> for Error<E> {
    fn from(e: ProtocolError) -> Self {
        Error::Protocol(e)
    }
}

pub struct Nexus<T> {
    transport: T,
    /// Timeout for writes, feature reports and feature sets. Default 1000 ms.
    pub timeout_ms: u32,
}

impl<T: Transport> Nexus<T> {
    pub fn new(transport: T) -> Self {
        Self { transport, timeout_ms: 1000 }
    }

    pub fn transport(&mut self) -> &mut T {
        &mut self.transport
    }

    pub fn into_transport(self) -> T {
        self.transport
    }

    /// Send one full 640x48 BGRA frame to EP2 OUT (121 packets).
    pub fn send_frame(&mut self, frame: &[u8]) -> Result<(), Error<T::Error>> {
        self.send_frame_padded(frame, &[])
    }

    /// As `send_frame`, filling the last packet's 56-byte tail with `pad`.
    pub fn send_frame_padded(&mut self, frame: &[u8], pad: &[u8]) -> Result<(), Error<T::Error>> {
        for pkt in protocol::image_packets(frame, pad)? {
            self.transport
                .write_image(&pkt, self.timeout_ms)
                .map_err(Error::Transport)?;
        }
        Ok(())
    }

    /// The next touch report, or `None` if nothing arrived in time (or the
    /// report wasn't a touch report).
    pub fn read_touch(&mut self, timeout_ms: u32) -> Result<Option<Touch>, Error<T::Error>> {
        let mut buf = [0u8; TOUCH_PACKET_BYTES];
        let Some(n) = self
            .transport
            .read_touch(&mut buf, timeout_ms)
            .map_err(Error::Transport)?
        else {
            return Ok(None);
        };
        let data = &buf[..n.min(TOUCH_PACKET_BYTES)];
        if data.first() != Some(&(NexusReports::TouchIn as u8)) {
            return Ok(None);
        }
        Ok(Some(touch::parse_touch(data)?))
    }

    /// Send a prebuilt command report (see `protocol::brightness` etc.).
    pub fn send_command(&mut self, report: &CommandReport) -> Result<(), Error<T::Error>> {
        self.transport
            .set_feature(report, self.timeout_ms)
            .map_err(Error::Transport)
    }

    pub fn brightness(&mut self, percent: u8) -> Result<(), Error<T::Error>> {
        self.send_command(&protocol::brightness(percent)?)
    }

    pub fn blank(&mut self) -> Result<(), Error<T::Error>> {
        self.send_command(&protocol::blank())
    }

    pub fn play_animation(&mut self, n: u8, looping: bool) -> Result<(), Error<T::Error>> {
        self.send_command(&protocol::play_animation(n, looping)?)
    }

    pub fn stop_animation(&mut self) -> Result<(), Error<T::Error>> {
        self.send_command(&protocol::stop_animation())
    }
}

/// Device info; available only when the transport implements `ReadFeature`.
/// Text results borrow from the buffer you pass in.
impl<T: ReadFeature> Nexus<T> {
    fn fetch<'b>(
        &mut self,
        id: u8,
        buf: &'b mut [u8; FEATURE_REPORT_BYTES],
    ) -> Result<&'b [u8], Error<T::Error>> {
        let n = self
            .transport
            .get_feature(id, buf, self.timeout_ms)
            .map_err(Error::Transport)?;
        Ok(&buf[..n.min(FEATURE_REPORT_BYTES)])
    }

    pub fn firmware_version<'b>(
        &mut self,
        buf: &'b mut [u8; FEATURE_REPORT_BYTES],
    ) -> Result<&'b str, Error<T::Error>> {
        let r = self.fetch(device_info::REPORT_FIRMWARE, buf)?;
        Ok(device_info::firmware_version(r)?)
    }

    pub fn secondary_version<'b>(
        &mut self,
        buf: &'b mut [u8; FEATURE_REPORT_BYTES],
    ) -> Result<&'b str, Error<T::Error>> {
        let r = self.fetch(device_info::REPORT_SECONDARY, buf)?;
        Ok(device_info::secondary_version(r)?)
    }

    pub fn serial<'b>(
        &mut self,
        buf: &'b mut [u8; FEATURE_REPORT_BYTES],
    ) -> Result<&'b str, Error<T::Error>> {
        let r = self.fetch(device_info::REPORT_SERIAL, buf)?;
        Ok(device_info::serial(r)?)
    }

    pub fn geometry(&mut self) -> Result<Geometry, Error<T::Error>> {
        let mut buf = [0u8; FEATURE_REPORT_BYTES];
        let r = self.fetch(device_info::REPORT_GEOMETRY, &mut buf)?;
        Ok(device_info::geometry(r)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::NEXUS_FRAME_BYTES;
    use std::collections::VecDeque;
    use std::vec::Vec;

    #[derive(Default)]
    struct Mock {
        images: Vec<[u8; IMAGE_PACKET_BYTES]>,
        touches: VecDeque<Vec<u8>>,
        sets: Vec<[u8; FEATURE_REPORT_BYTES]>,
        fail_writes: bool,
    }

    impl Transport for Mock {
        type Error = &'static str;

        fn write_image(&mut self, pkt: &[u8; IMAGE_PACKET_BYTES], _: u32) -> Result<(), &'static str> {
            if self.fail_writes {
                return Err("usb");
            }
            self.images.push(*pkt);
            Ok(())
        }

        fn read_touch(&mut self, buf: &mut [u8; TOUCH_PACKET_BYTES], _: u32) -> Result<Option<usize>, &'static str> {
            Ok(self.touches.pop_front().map(|t| {
                buf[..t.len()].copy_from_slice(&t);
                t.len()
            }))
        }

        fn set_feature(&mut self, r: &[u8; FEATURE_REPORT_BYTES], _: u32) -> Result<(), &'static str> {
            self.sets.push(*r);
            Ok(())
        }
    }

    /// A transport that also reads feature reports.
    struct Info;
    impl Transport for Info {
        type Error = ();
        fn write_image(&mut self, _: &[u8; IMAGE_PACKET_BYTES], _: u32) -> Result<(), ()> { Ok(()) }
        fn read_touch(&mut self, _: &mut [u8; TOUCH_PACKET_BYTES], _: u32) -> Result<Option<usize>, ()> { Ok(None) }
        fn set_feature(&mut self, _: &[u8; FEATURE_REPORT_BYTES], _: u32) -> Result<(), ()> { Ok(()) }
    }
    impl ReadFeature for Info {
        fn get_feature(&mut self, id: u8, buf: &mut [u8; FEATURE_REPORT_BYTES], _: u32) -> Result<usize, ()> {
            let bytes: &[u8] = match id {
                4 => &[4, 8, 1, 2, 3, 4, b'1', b'.', b'2', b' '],
                7 => &[7, 5, b'A', b'B', b'1'],
                9 => &[9, 1, 6, 0x48, 0, 0x30, 0, 0x80, 2, 0x30, 0, 4, 5],
                _ => return Err(()),
            };
            buf.fill(0);
            buf[..bytes.len()].copy_from_slice(bytes);
            Ok(FEATURE_REPORT_BYTES)
        }
    }

    #[test]
    fn send_frame_writes_121_packets_and_propagates_errors() {
        let frame = std::vec![0u8; NEXUS_FRAME_BYTES];
        let mut dev = Nexus::new(Mock::default());
        dev.send_frame(&frame).unwrap();
        assert_eq!(dev.transport().images.len(), 121);

        assert_eq!(
            dev.send_frame(&[0; 3]),
            Err(Error::Protocol(ProtocolError::BadFrameSize { expected: NEXUS_FRAME_BYTES, got: 3 }))
        );
        dev.transport().fail_writes = true;
        assert_eq!(dev.send_frame(&frame), Err(Error::Transport("usb")));
    }

    #[test]
    fn read_touch_handles_timeout_wrong_id_and_good_report() {
        let mut dev = Nexus::new(Mock::default());
        assert_eq!(dev.read_touch(50), Ok(None)); // timeout

        dev.transport().touches.push_back(std::vec![9, 0, 0]); // not a touch report
        assert_eq!(dev.read_touch(50), Ok(None));

        dev.transport().touches.push_back(touch::touch_report(300, true, 0).to_vec());
        assert_eq!(dev.read_touch(50), Ok(Some(Touch { down: true, x: 300, flags: 0 })));

        dev.transport().touches.push_back(std::vec![1, 2, 0x21]); // too short
        assert_eq!(dev.read_touch(50), Err(Error::Protocol(ProtocolError::NotTouchReport)));
    }

    #[test]
    fn commands_go_out_as_feature_reports() {
        let mut dev = Nexus::new(Mock::default());
        dev.brightness(80).unwrap();
        dev.blank().unwrap();
        dev.play_animation(2, true).unwrap();
        dev.stop_animation().unwrap();
        assert_eq!(dev.brightness(101), Err(Error::Protocol(ProtocolError::OutOfRange { what: "brightness is 0-100" })));
        let sets = &dev.transport().sets;
        assert_eq!(sets.len(), 4); // the rejected one never reached the transport
        assert_eq!(sets[0][..3], [3, 0x01, 80]);
        assert_eq!(sets[2][..4], [3, 0x0D, 2, 1]);
    }

    #[test]
    fn device_info_needs_read_feature() {
        let mut dev = Nexus::new(Info);
        let mut buf = [0u8; FEATURE_REPORT_BYTES];
        assert_eq!(dev.firmware_version(&mut buf), Ok("1.2"));
        assert_eq!(dev.serial(&mut buf), Ok("AB1"));
        let g = dev.geometry().unwrap();
        assert_eq!((g.width, g.height), (640, 48));
        assert_eq!(dev.secondary_version(&mut buf), Err(Error::Transport(())));
    }
}
