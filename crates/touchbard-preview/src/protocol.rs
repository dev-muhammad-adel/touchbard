//! Versioned WebSocket preview protocol.
//!
//! The protocol uses a small binary framing for efficiency:
//!
//! ```text
//! +-+-+-+-+-+-+-+-+
//! | MsgType (1B)  |
//! +-+-+-+-+-+-+-+-+
//! | Payload...     |
//! +-+-+-+-+-+-+-+-+
//! ```
//!
//! ## Message types
//!
//! | Type | Name     | Direction | Meaning                                     |
//! |------|----------|-----------|---------------------------------------------|
//! | 0x01 | `HELLO`  | → client  | Server greeting with protocol version + size |
//! | 0x02 | `FRAME`  | → client  | A rendered framebuffer                       |
//! | 0x03 | `INPUT`  | ← client  | A pointer input event from the browser       |
//! | 0x04 | `RESIZE` | → client  | Framebuffer dimensions changed               |
//! | 0x05 | `PING`   | ↔         | Keepalive / latency probe                    |
//! | 0x06 | `PONG`   | ↔         | Reply to a PING                              |
//!
//! Frames use binary payloads; control messages (HELLO/RESIZE) are sent as
//! JSON text for easy debugging. All integers are big-endian.

use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite::Message;

/// Protocol version. Increment on any incompatible change.
pub const PROTOCOL_VERSION: u16 = 1;

/// Message type byte values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum MsgType {
    Hello = 0x01,
    Frame = 0x02,
    Input = 0x03,
    Resize = 0x04,
    Ping = 0x05,
    Pong = 0x06,
    RawFrame = 0x07,
}

impl TryFrom<u8> for MsgType {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(MsgType::Hello),
            0x02 => Ok(MsgType::Frame),
            0x03 => Ok(MsgType::Input),
            0x04 => Ok(MsgType::Resize),
            0x05 => Ok(MsgType::Ping),
            0x06 => Ok(MsgType::Pong),
            0x07 => Ok(MsgType::RawFrame),
            _ => Err(()),
        }
    }
}

/// Server greeting (JSON-text payload of a `HELLO`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub protocol_version: u16,
    pub width: u32,
    pub height: u32,
    pub scale_factor: f64,
}

/// Framebuffer resize notification (JSON-text payload of a `RESIZE`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resize {
    pub width: u32,
    pub height: u32,
    pub scale_factor: f64,
}

/// Pointer event sent from the browser (JSON-text payload of an `INPUT`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputEvent {
    /// `"pointerdown"`, `"pointerup"`, `"pointermove"`, `"click"`.
    pub r#type: String,
    /// Position in CSS (logical) pixels relative to the canvas.
    pub x: f64,
    pub y: f64,
    /// DOM `button` value (0 = main, 1 = auxiliary, 2 = secondary, ...).
    pub button: u8,
    /// DOM `buttons` bitmask of currently pressed buttons.
    pub buttons: u8,
}

/// Build a binary `HELLO` message.
pub fn hello(hello: &Hello) -> Message {
    let json = serde_json::to_string(hello).expect("Hello serialization is infallible");
    Message::Text(format!("HELLO {json}").into())
}

/// Build a binary `FRAME` message.
///
/// Layout:
/// ```text
/// [0]      msg type (0x02)
/// [1..5]   width  (u32 BE)
/// [5..9]   height (u32 BE)
/// [9..]    premultiplied RGBA8 pixel data (length = width * height * 4)
/// ```
///
/// Pixel bytes are the canonical [`Frame`](touchbard_renderer::Frame) format
/// (premultiplied alpha). The browser client un-premultiplies them at the
/// backend boundary because `canvas.putImageData` requires straight alpha.
pub fn frame(width: u32, height: u32, pixels: &[u8]) -> Message {
    let mut bytes = Vec::with_capacity(9 + pixels.len());
    bytes.push(MsgType::Frame as u8);
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(pixels);
    Message::Binary(bytes.into())
}

/// Build a `RESIZE` message (JSON text).
pub fn resize(resize: &Resize) -> Message {
    let json = serde_json::to_string(resize).expect("Resize serialization is infallible");
    Message::Text(format!("RESIZE {json}").into())
}

/// Build a `PING` message (binary, payload is a u64 nanoseconds timestamp BE).
pub fn ping(timestamp_ns: u64) -> Message {
    let mut bytes = Vec::with_capacity(9);
    bytes.push(MsgType::Ping as u8);
    bytes.extend_from_slice(&timestamp_ns.to_be_bytes());
    Message::Binary(bytes.into())
}

/// Build a `PONG` message mirroring the timestamp.
pub fn pong(timestamp_ns: u64) -> Message {
    let mut bytes = Vec::with_capacity(9);
    bytes.push(MsgType::Pong as u8);
    bytes.extend_from_slice(&timestamp_ns.to_be_bytes());
    Message::Binary(bytes.into())
}

/// Parse a client `PONG` message into the reflected (echoed) ping timestamp.
///
/// The client mirrors the 8-byte BE nanoseconds timestamp from the server's
/// `PING`, prefixed with the `PONG` type byte.
pub fn parse_pong(bytes: &[u8]) -> Option<u64> {
    if bytes.len() != 9 || bytes[0] != MsgType::Pong as u8 {
        return None;
    }
    Some(u64::from_be_bytes(bytes[1..9].try_into().ok()?))
}

/// Parse a client message into an `InputEvent`, if it is one.
///
/// Text messages have the form `INPUT <json>` or `CLICK <json>` / `INPUT:click <json>`.
pub fn parse_input_message(text: &str) -> Option<InputEvent> {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix("INPUT ") {
        serde_json::from_str::<InputEvent>(rest).ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    #[test]
    fn test_protocol_version() {
        assert_eq!(PROTOCOL_VERSION, 1);
    }

    #[test]
    fn test_frame_message_layout() {
        let pixels = vec![0u8; 4 * 4]; // 2x2 rgb a
        let msg = frame(2, 2, &pixels);
        match msg {
            WsMessage::Binary(bytes) => {
                assert_eq!(bytes[0], MsgType::Frame as u8);
                assert_eq!(
                    u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]),
                    2
                );
                assert_eq!(
                    u32::from_be_bytes([bytes[5], bytes[6], bytes[7], bytes[8]]),
                    2
                );
                assert_eq!(&bytes[9..], pixels.as_slice());
            }
            other => panic!("expected Binary, got {other:?}"),
        }
    }

    #[test]
    fn test_msg_type_roundtrip() {
        assert_eq!(MsgType::try_from(0x01).unwrap(), MsgType::Hello);
        assert_eq!(MsgType::try_from(0x02).unwrap(), MsgType::Frame);
        assert_eq!(MsgType::try_from(0x03).unwrap(), MsgType::Input);
        assert_eq!(MsgType::try_from(0x04).unwrap(), MsgType::Resize);
        assert_eq!(MsgType::try_from(0x07).unwrap(), MsgType::RawFrame);
        assert!(MsgType::try_from(0x99).is_err());
    }

    #[test]
    fn test_pong_parse_roundtrip() {
        let ts = 1_700_000_000_123_456_789u64;
        let msg = pong(ts);
        let WsMessage::Binary(bytes) = msg else {
            panic!("expected binary pong");
        };
        assert_eq!(parse_pong(&bytes), Some(ts));
        assert_eq!(parse_pong(&bytes[..8]), None); // missing type byte
        let bad = {
            let mut b = bytes.as_ref().to_vec();
            b[0] = MsgType::Frame as u8;
            b
        };
        assert_eq!(parse_pong(&bad), None); // wrong type byte
    }

    #[test]
    fn test_parse_input_message() {
        let e = parse_input_message(
            r#"INPUT {"type":"pointermove","x":12.5,"y":3.0,"button":0,"buttons":0}"#,
        );
        assert!(e.is_some());
        let e = e.unwrap();
        assert_eq!(e.r#type, "pointermove");
        assert_eq!(e.x, 12.5);
        assert_eq!(e.y, 3.0);
        assert_eq!(e.button, 0);
        assert_eq!(e.buttons, 0);
    }

    #[test]
    fn test_parse_invalid_input_message() {
        assert!(parse_input_message("HELLO something").is_none());
        assert!(parse_input_message("INPUT not-json").is_none());
    }

    #[test]
    fn test_hello_serde() {
        let h = Hello {
            protocol_version: PROTOCOL_VERSION,
            width: 1080,
            height: 30,
            scale_factor: 1.0,
        };
        let msg = hello(&h);
        match msg {
            WsMessage::Text(t) => assert!(t.starts_with("HELLO ")),
            other => panic!("expected Text, got {other:?}"),
        }
    }
}
