//! `ProxySocketBridge` and the pre-allocated packet ring.
//!
//! The WebSocket itself lives in `web/worker.js` in production; this module owns
//! the receiving end: a fixed 24 MiB cyclic buffer that binary frames are copied
//! into, plus the VarInt frame decoder that walks it.

mod buffer;
mod socket;

pub use buffer::{
    ClientPacketRingBuffer, RingError, RingStats, DEFAULT_CAPACITY_BYTES, MAX_CAPACITY_BYTES,
    MIN_CAPACITY_BYTES,
};
pub use socket::{
    extract_bytes, BackoffPolicy, BridgeState, IngestReport, ProxySocketBridge, SocketError,
};
