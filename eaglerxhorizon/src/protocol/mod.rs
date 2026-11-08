//! Minimal Minecraft wire protocol scaffolding.
//!
//! This is intentionally a typed skeleton: enough to frame packets, read and
//! write VarInt/VarLong, and build the handshake / status / login-start packets
//! that an EaglercraftXBungee WebSocket expects. Gameplay packets are out of
//! scope for the baseline.

use std::io::{Cursor, Read};

use byteorder::{ReadBytesExt, WriteBytesExt};
use thiserror::Error;

pub const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("varint is too long")]
    VarIntTooLong,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame declares {declared} bytes but only {available} are buffered")]
    Truncated { declared: usize, available: usize },
    #[error("frame of {0} bytes exceeds the {MAX_FRAME_BYTES} byte cap")]
    FrameTooLarge(usize),
}

/// Connection states used in the handshake `next_state` field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NextState {
    Status,
    Login,
}

impl NextState {
    pub fn id(self) -> i32 {
        match self {
            NextState::Status => 1,
            NextState::Login => 2,
        }
    }
}

/// Client -> server packet ids we actually build in the baseline.
pub mod serverbound {
    pub const HANDSHAKE: i32 = 0x00;
    pub const STATUS_REQUEST: i32 = 0x00;
    pub const LOGIN_START: i32 = 0x00;
    pub const KEEP_ALIVE: i32 = 0x0F;
    pub const CHAT_MESSAGE: i32 = 0x03;
}

/// Server -> client packet ids we recognise when decoding.
pub mod clientbound {
    pub const STATUS_RESPONSE: i32 = 0x00;
    pub const PONG: i32 = 0x01;
    pub const LOGIN_SUCCESS: i32 = 0x02;
    pub const DISCONNECT: i32 = 0x00;
    pub const KEEP_ALIVE: i32 = 0x21;
}

/// Writes an unsigned VarInt (the Minecraft encoding, not protobuf zig-zag).
pub fn write_varint(out: &mut Vec<u8>, value: i32) {
    let mut value = value as u32;
    loop {
        if value & !0x7F == 0 {
            out.push(value as u8);
            return;
        }
        out.push(((value & 0x7F) | 0x80) as u8);
        value >>= 7;
    }
}

/// Reads a VarInt from `bytes`, returning the value and how many bytes it used.
pub fn read_varint(bytes: &[u8]) -> Result<(i32, usize), ProtocolError> {
    let mut result: u32 = 0;
    let mut shift = 0;
    for (index, byte) in bytes.iter().copied().enumerate() {
        result |= ((byte & 0x7F) as u32) << shift;
        if byte & 0x80 == 0 {
            return Ok((result as i32, index + 1));
        }
        shift += 7;
        if shift >= 35 {
            return Err(ProtocolError::VarIntTooLong);
        }
    }
    Err(ProtocolError::Truncated {
        declared: 1,
        available: bytes.len(),
    })
}

/// Writes an unsigned VarLong.
pub fn write_varlong(out: &mut Vec<u8>, value: i64) {
    let mut value = value as u64;
    loop {
        if value & !0x7F == 0 {
            out.push(value as u8);
            return;
        }
        out.push(((value & 0x7F) | 0x80) as u8);
        value >>= 7;
    }
}

pub fn read_varlong(bytes: &[u8]) -> Result<(i64, usize), ProtocolError> {
    let mut result: u64 = 0;
    let mut shift = 0;
    for (index, byte) in bytes.iter().copied().enumerate() {
        result |= ((byte & 0x7F) as u64) << shift;
        if byte & 0x80 == 0 {
            return Ok((result as i64, index + 1));
        }
        shift += 7;
        if shift >= 70 {
            return Err(ProtocolError::VarIntTooLong);
        }
    }
    Err(ProtocolError::Truncated {
        declared: 1,
        available: bytes.len(),
    })
}

/// Wraps `payload` in a length-prefixed frame (length = VarInt of payload size).
pub fn frame(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 5);
    write_varint(&mut out, payload.len() as i32);
    out.extend_from_slice(payload);
    out
}

/// Splits one frame off the front of `buffer`.
///
/// Returns the packet id, the payload after it, and how many bytes were consumed
/// so a read loop can `drain(..consumed)`.
pub fn decode_frame(buffer: &[u8]) -> Result<Option<(i32, Vec<u8>, usize)>, ProtocolError> {
    if buffer.is_empty() {
        return Ok(None);
    }
    let (length, prefix) = match read_varint(buffer) {
        Ok(value) => value,
        // not enough bytes yet for the length prefix: wait for more
        Err(ProtocolError::Truncated { .. }) => return Ok(None),
        Err(error) => return Err(error),
    };
    let length = length.max(0) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge(length));
    }
    if buffer.len() < prefix + length {
        return Ok(None);
    }
    let body = &buffer[prefix..prefix + length];
    let (packet_id, id_len) = read_varint(body)?;
    Ok(Some((packet_id, body[id_len..].to_vec(), prefix + length)))
}

/// Builds the handshake packet for `host:port` with the given protocol version.
pub fn build_handshake(host: &str, port: u16, protocol_version: i32, next: NextState) -> Vec<u8> {
    let mut payload = Vec::new();
    write_varint(&mut payload, serverbound::HANDSHAKE);
    write_varint(&mut payload, protocol_version);
    let host_bytes = host.as_bytes();
    write_varint(&mut payload, host_bytes.len() as i32);
    payload.extend_from_slice(host_bytes);
    payload.extend_from_slice(&port.to_be_bytes());
    write_varint(&mut payload, next.id());
    frame(&payload)
}

pub fn build_status_request() -> Vec<u8> {
    let mut payload = Vec::new();
    write_varint(&mut payload, serverbound::STATUS_REQUEST);
    frame(&payload)
}

pub fn build_status_ping(now_millis: i64) -> Vec<u8> {
    let mut payload = Vec::new();
    write_varint(&mut payload, serverbound::STATUS_REQUEST + 1);
    payload.extend_from_slice(&now_millis.to_be_bytes());
    frame(&payload)
}

pub fn build_login_start(username: &str) -> Vec<u8> {
    let mut payload = Vec::new();
    write_varint(&mut payload, serverbound::LOGIN_START);
    write_varint(&mut payload, username.len() as i32);
    payload.extend_from_slice(username.as_bytes());
    // no UUID: let the proxy assign one
    frame(&payload)
}

pub fn build_chat(message: &str) -> Vec<u8> {
    let mut payload = Vec::new();
    write_varint(&mut payload, serverbound::CHAT_MESSAGE);
    write_varint(&mut payload, message.len() as i32);
    payload.extend_from_slice(message.as_bytes());
    frame(&payload)
}

/// Decodes a UTF-8 string prefixed with its byte length.
pub fn read_string(cursor: &mut Cursor<&[u8]>) -> Result<String, ProtocolError> {
    let (length, used) = {
        let mut prefix = Vec::with_capacity(5);
        for _ in 0..5 {
            let byte = cursor.read_u8()?;
            prefix.push(byte);
            if byte & 0x80 == 0 {
                break;
            }
        }
        read_varint(&prefix)?
    };
    let _ = used;
    let mut buffer = vec![0u8; length.max(0) as usize];
    cursor.read_exact(&mut buffer)?;
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

/// Latency probe frame: packet id 0xFE plus an 8 byte timestamp.
///
/// A proxy that understands it echoes it back; EaglercraftXBungee ignores
/// unknown ids, so the bridge treats "frame received" as the pong in that case.
pub const LATENCY_PROBE_ID: i32 = 0xFE;

pub fn build_latency_probe(timestamp_millis: f64) -> Vec<u8> {
    let mut payload = Vec::new();
    write_varint(&mut payload, LATENCY_PROBE_ID);
    payload.extend_from_slice(&(timestamp_millis as i64).to_be_bytes());
    frame(&payload)
}

pub fn build_varint_payload(packet_id: i32) -> Vec<u8> {
    let mut payload = Vec::new();
    write_varint(&mut payload, packet_id);
    payload
}

#[allow(dead_code)]
fn write_keepalive(id: i64) -> Vec<u8> {
    let mut out = Vec::new();
    let _ = out.write_i64::<byteorder::BigEndian>(id);
    frame(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_roundtrip() {
        for value in [0, 1, 127, 128, 255, 2_147_483_647] {
            let mut buffer = Vec::new();
            write_varint(&mut buffer, value);
            let (decoded, used) = read_varint(&buffer).unwrap();
            assert_eq!(decoded, value);
            assert_eq!(used, buffer.len());
        }
    }

    #[test]
    fn negative_varint_roundtrip() {
        let mut buffer = Vec::new();
        write_varint(&mut buffer, -1);
        assert_eq!(buffer.len(), 5);
        assert_eq!(read_varint(&buffer).unwrap().0, -1);
    }

    #[test]
    fn handshake_is_length_prefixed() {
        let packet = build_handshake("proxy.example", 8081, 47, NextState::Login);
        let (length, prefix) = read_varint(&packet).unwrap();
        assert_eq!(length as usize, packet.len() - prefix);
        let (id, payload, consumed) = decode_frame(&packet).unwrap().unwrap();
        assert_eq!(id, serverbound::HANDSHAKE);
        assert_eq!(consumed, packet.len());
        assert!(!payload.is_empty());
    }

    #[test]
    fn partial_frame_waits_for_more() {
        let packet = build_login_start("player");
        assert!(decode_frame(&packet[..2]).unwrap().is_none());
        assert!(decode_frame(&packet).unwrap().is_some());
    }

    #[test]
    fn two_frames_decode_in_sequence() {
        let mut buffer = build_status_request();
        buffer.extend_from_slice(&build_status_request());
        let (_, _, first) = decode_frame(&buffer).unwrap().unwrap();
        let (_, _, second) = decode_frame(&buffer[first..]).unwrap().unwrap();
        assert_eq!(first, second);
        assert_eq!(first + second, buffer.len());
    }
}
