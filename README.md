# nexus-rs

A Rust library for the Corsair iCUE NEXUS touch display (USB `1b1c:1b8e`).

The device is a 640x48 framebuffer plus a one-axis touch strip. This crate
implements the wire protocol and a small device layer on top of it.

- `#![no_std]`, no `alloc`, no dependencies.
- Blocking only; there is no `async`.
- No USB handle, clock or thread inside. Your application supplies the USB
  backend by implementing a trait (see below).

## Modules

| Module | Contents |
|---|---|
| `protocol` | IDs, endpoints, report IDs, image packetising (`image_packets`, `parse_image_packet`, `FrameAssembler`), command reports (`brightness`, `blank`, `play_animation`, `stop_animation`), `ProtocolError` |
| `screen` | Display constants and `bgra_to_rgb` / `rgb_to_bgra` |
| `device_info` | Parsers for feature reports 4..0x0E: firmware, serial, geometry |
| `touch` | `Touch`, `parse_touch`, `GestureTracker` (taps and drags) |
| `device` | The `Transport` and `ReadFeature` traits and `Nexus<T>` |

## Using it

Implement `Transport` over whatever USB library you use. The trait is at the
protocol level (image packet out, touch report in, feature report out), so it
fits libusb/`rusb` as well as hidraw or hidapi.

```rust
use nexus_rs::device::{Nexus, Transport};

let mut nexus = Nexus::new(MyTransport::open()?);   // your impl

nexus.brightness(80)?;
nexus.send_frame(&bgra_frame)?;                     // 640*48*4 bytes, B,G,R,A

if let Some(touch) = nexus.read_touch(50)? {        // 50 ms timeout
    println!("x = {}, down = {}", touch.x, touch.down);
}
```

Opening the device, claiming interface 0, detaching the kernel driver,
permission and busy errors, and closing are the transport's job.

A `Transport` for USB must do three things:

| Method | USB operation |
|---|---|
| `write_image` | write the 1024-byte packet to EP2 OUT (`0x02`) |
| `read_touch` | read up to 512 bytes from EP1 IN (`0x81`); `Ok(None)` on timeout |
| `set_feature` | control transfer: type `0x21`, request `0x09` (SET_REPORT), wValue `0x0300 \| report[0]`, wIndex 0 |

### Optional: device info

Implement `ReadFeature` as well (HID GET_REPORT, wValue `0x0300 | id`) to
enable `firmware_version`, `secondary_version`, `serial` and `geometry` on
`Nexus`. Without it those methods don't exist. Text results borrow from a
32-byte buffer you pass in.

### Gestures

```rust
use nexus_rs::touch::GestureTracker;

let mut tracker = GestureTracker::default();        // drag threshold: 16 px
if let Some(g) = tracker.feed(now_ms, touch) {
    // g.kind is Tap or Drag; g.x_start..g.x_end, g.start..g.end
}
```

Timestamps are `u64` in whatever unit you pass; the crate never reads a clock.

### Receiving frames

`FrameAssembler` rebuilds frames from captured image packets. It works in a
buffer you provide (at least `NEXUS_FRAME_BYTES`), so nothing is allocated.

## Protocol notes

- Frame: 640x48, 4 bytes per pixel in B,G,R,A order, 122,880 bytes.
- Upload: 121 output reports of 1024 bytes (8-byte header + 1016 payload; the
  last payload is 960). Header: `[02, 05, 40, final, block:u16, length:u16]`,
  little-endian.
- Commands are 32-byte feature reports with ID 3.
- Touch reports are 512 bytes on EP1 IN; only the first 10 are non-zero.
  A lift is `down == false && x == 0`; `down == false` with `x != 0` is a
  tracking dropout.
- Values in the source are tagged `[CAPTURE]`, `[TOUCH-CAP]` or `[DOC]`:
  verified on hardware captures versus taken from third-party documentation.

## Design notes

- Errors are values (`ProtocolError`, `device::Error<E>`), not exceptions.
- Command reports are returned already zero-padded to 32 bytes.
- Non-ASCII device text is an error, since a borrowed `&str` can't hold
  replacement characters without allocating.
- `image_packets` is a lazy iterator, and `FrameAssembler` and the BGRA/RGB
  helpers use caller-supplied buffers.

## Testing

```
cargo test
```

The device layer is tested against a mock `Transport`; no hardware needed.
