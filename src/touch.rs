//! Touch input: report 1 on EP1 IN [TOUCH-CAP].
//!
//! Wire layout, 512 B, only the first 10 bytes are ever non-zero:
//!
//! ```text
//! 0     01        report id
//! 1     02        constant
//! 2-3   21 00     constant (0x0021)
//! 4     flags     00; 04 seen once per drag (meaning unknown)
//! 5     state     01 finger down, 00 up
//! 6-7   x         little-endian, observed 23..577 of 0..639
//! 8-9   x again   identical to bytes 6-7 in all 292 reports captured
//! ```
//!
//! Reports arrive about every 10 ms while a finger is down. A lift is a
//! report with state=0 and x=0; state=0 with x!=0 happened mid-drag
//! (flags=04) and is a tracking dropout, not a lift.

use crate::protocol::{NexusReports, ProtocolError};

pub const TOUCH_PACKET_BYTES: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Touch {
    pub down: bool,
    /// 0..639; the strip has no Y axis.
    pub x: u16,
    /// Byte 4; 0x04 appears once per drag, meaning unknown.
    pub flags: u8,
}

impl Touch {
    /// state=0 and x==0 is a real lift; state=0 with x!=0 is a tracking
    /// dropout and the finger is still down.
    pub fn is_lift(&self) -> bool {
        !self.down && self.x == 0
    }
}

pub fn parse_touch(buf: &[u8]) -> Result<Touch, ProtocolError> {
    if buf.len() < 10 || buf[0] != NexusReports::TouchIn as u8 {
        return Err(ProtocolError::NotTouchReport);
    }
    Ok(Touch {
        down: buf[5] != 0,
        x: u16::from_le_bytes([buf[6], buf[7]]),
        flags: buf[4],
    })
}

/// Build a 512-byte touch report (for tests and replay).
pub fn touch_report(x: u16, down: bool, flags: u8) -> [u8; TOUCH_PACKET_BYTES] {
    let mut r = [0u8; TOUCH_PACKET_BYTES];
    r[0] = NexusReports::TouchIn as u8;
    r[1] = 0x02;
    r[2..4].copy_from_slice(&0x0021u16.to_le_bytes());
    r[4] = flags;
    r[5] = down as u8;
    r[6..8].copy_from_slice(&x.to_le_bytes());
    r[8..10].copy_from_slice(&x.to_le_bytes());
    r
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GestureKind {
    Tap,
    Drag,
}

/// Timestamps are whatever unit the caller feeds in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gesture {
    pub kind: GestureKind,
    pub start: u64,
    pub end: u64,
    pub x_start: u16,
    pub x_end: u16,
    pub x_min: u16,
    pub x_max: u16,
    pub reports: u32,
}

/// Groups touch reports into taps and drags.
///
/// A gesture ends at a real lift (`Touch::is_lift`). It is a drag if x
/// travelled more than `drag_px` between its minimum and maximum, otherwise
/// a tap. In captures, taps were 46-83 ms with no x travel; drags had
/// 200+ px.
///
/// Only the first timestamp, first/last/min/max x and a count are needed to
/// describe a gesture, so that is all it stores.
/// No allocation, and the caller supplies all timestamps.
#[derive(Clone, Debug)]
pub struct GestureTracker {
    drag_px: u16,
    open: Option<Open>,
}

#[derive(Clone, Copy, Debug)]
struct Open {
    start: u64,
    x_start: u16,
    x_last: u16,
    x_min: u16,
    x_max: u16,
    reports: u32,
}

impl Default for GestureTracker {
    fn default() -> Self {
        Self::new(16)
    }
}

impl GestureTracker {
    pub fn new(drag_px: u16) -> Self {
        Self { drag_px, open: None }
    }

    pub fn feed(&mut self, ts: u64, t: Touch) -> Option<Gesture> {
        if t.is_lift() {
            let g = self.open.take()?; // stray lift: ignored
            let kind = if g.x_max - g.x_min > self.drag_px {
                GestureKind::Drag
            } else {
                GestureKind::Tap
            };
            return Some(Gesture {
                kind,
                start: g.start,
                end: ts,
                x_start: g.x_start,
                x_end: g.x_last,
                x_min: g.x_min,
                x_max: g.x_max,
                reports: g.reports,
            });
        }
        if t.down || t.x != 0 {
            // dropouts keep the gesture open
            let o = self.open.get_or_insert(Open {
                start: ts,
                x_start: t.x,
                x_last: t.x,
                x_min: t.x,
                x_max: t.x,
                reports: 0,
            });
            o.x_last = t.x;
            o.x_min = o.x_min.min(t.x);
            o.x_max = o.x_max.max(t.x);
            o.reports += 1;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_roundtrip_and_rejects() {
        let r = touch_report(577, true, 4);
        assert_eq!(r[..10], [1, 2, 0x21, 0, 4, 1, 0x41, 0x02, 0x41, 0x02]);
        assert_eq!(parse_touch(&r), Ok(Touch { down: true, x: 577, flags: 4 }));
        assert_eq!(parse_touch(&r[..9]), Err(ProtocolError::NotTouchReport));
        assert_eq!(parse_touch(&[0; 512]), Err(ProtocolError::NotTouchReport));
        assert!(parse_touch(&touch_report(0, false, 0)).unwrap().is_lift());
        assert!(!parse_touch(&touch_report(50, false, 4)).unwrap().is_lift()); // dropout
    }

    #[test]
    fn gestures_tap_drag_dropout_and_stray_lift() {
        let mut tracker = GestureTracker::default();
        let mut out = std::vec::Vec::new();
        let mut feed = |ts: u64, x: u16, down: bool| {
            if let Some(g) = tracker.feed(ts, Touch { down, x, flags: 0 }) {
                out.push(g);
            }
        };
        for ts in (0..50).step_by(10) {
            feed(ts, 231, true); // tap: no x travel
        }
        feed(60, 0, false);
        for i in 0..10u64 {
            feed(100 + i * 10, 131 + i as u16 * 20, i != 5); // drag with a dropout
        }
        feed(300, 0, false);
        feed(310, 0, false); // stray lift: ignored

        let summary: std::vec::Vec<_> =
            out.iter().map(|g| (g.kind, g.x_start, g.x_end, g.reports)).collect();
        assert_eq!(
            summary,
            [(GestureKind::Tap, 231, 231, 5), (GestureKind::Drag, 131, 311, 10)]
        );
        assert_eq!((out[0].start, out[0].end), (0, 60));
        assert_eq!(out[1].x_max, 311);
    }
}
