//! Pre-allocated cyclic packet buffer.
//!
//! The browser client must never pause to grow a buffer while chunks are
//! streaming in: a single reallocation of a 24 MB `Vec` mid-frame is a visible
//! hitch, and the allocator spike makes it worse. `ClientPacketRingBuffer`
//! therefore reserves its whole capacity once, at boot, and then only moves
//! cursors. Every write is bounds-checked against that fixed capacity; if the
//! consumer falls behind, the write is refused and counted instead of growing.
//!
//! Layout:
//!
//! ```text
//!   storage: [ . . . . . . . . . . . . . . . . . . . . ]
//!                     ^tail(write)     ^head(read)
//!   len = bytes currently buffered, capacity = storage.len() (never changes)
//!   a frame that reaches the end continues at index 0 (wrap)
//! ```

use serde::Serialize;

use crate::protocol::{decode_frame, read_varint, ProtocolError};

/// 24 MiB: enough for a burst of chunk packets without starving the heap.
pub const DEFAULT_CAPACITY_BYTES: usize = 24 * 1024 * 1024;
/// Refuse anything smaller than 16 MiB: heavy chunk loading overruns it.
pub const MIN_CAPACITY_BYTES: usize = 16 * 1024 * 1024;
/// Hard ceiling so a misconfiguration cannot exhaust the wasm linear memory.
pub const MAX_CAPACITY_BYTES: usize = 32 * 1024 * 1024;
/// A length prefix is at most 5 bytes.
const MAX_VARINT_BYTES: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingError {
    /// The buffer cannot hold this many bytes right now.
    Full { requested: usize, free: usize },
    /// A single write can never exceed the capacity.
    Oversized { requested: usize, capacity: usize },
}

impl core::fmt::Display for RingError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            RingError::Full { requested, free } => {
                write!(formatter, "ring buffer full: wanted {requested} bytes, {free} free")
            }
            RingError::Oversized { requested, capacity } => {
                write!(formatter, "write of {requested} bytes exceeds the {capacity} byte ring")
            }
        }
    }
}

/// Cumulative counters, cheap to copy out for the HUD.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct RingStats {
    pub capacity: usize,
    pub len: usize,
    pub peak_len: usize,
    pub bytes_written: u64,
    pub bytes_read: u64,
    pub wraps: u64,
    pub overflow_drops: u64,
    pub frames: u64,
    pub reset_count: u64,
}

#[derive(Clone, Debug)]
pub struct ClientPacketRingBuffer {
    storage: Vec<u8>,
    head: usize,
    tail: usize,
    len: usize,
    peak_len: usize,
    bytes_written: u64,
    bytes_read: u64,
    wraps: u64,
    overflow_drops: u64,
    frames: u64,
    reset_count: u64,
}

impl Default for ClientPacketRingBuffer {
    fn default() -> Self {
        Self::preallocate(DEFAULT_CAPACITY_BYTES)
    }
}

impl ClientPacketRingBuffer {
    /// The only allocation this type ever performs.
    ///
    /// Capacity is clamped into `MIN..=MAX` so a caller cannot hand us 200 MB or
    /// a 4 KB toy buffer by accident.
    pub fn preallocate(capacity: usize) -> Self {
        let capacity = capacity.clamp(MIN_CAPACITY_BYTES, MAX_CAPACITY_BYTES);
        Self {
            storage: vec![0u8; capacity],
            head: 0,
            tail: 0,
            len: 0,
            peak_len: 0,
            bytes_written: 0,
            bytes_read: 0,
            wraps: 0,
            overflow_drops: 0,
            frames: 0,
            reset_count: 0,
        }
    }

    pub fn capacity(&self) -> usize {
        self.storage.len()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn free(&self) -> usize {
        self.storage.len() - self.len
    }

    pub fn stats(&self) -> RingStats {
        RingStats {
            capacity: self.capacity(),
            len: self.len,
            peak_len: self.peak_len,
            bytes_written: self.bytes_written,
            bytes_read: self.bytes_read,
            wraps: self.wraps,
            overflow_drops: self.overflow_drops,
            frames: self.frames,
            reset_count: self.reset_count,
        }
    }

    /// Bytes available from `head` before the write cursor or the end of storage.
    pub fn contiguous_readable(&self) -> usize {
        if self.len == 0 {
            return 0;
        }
        let to_end = self.capacity() - self.head;
        let to_tail = if self.tail > self.head {
            self.tail - self.head
        } else {
            to_end
        };
        to_end.min(to_tail)
    }

    /// Writes `bytes` into the ring, splitting the copy if it wraps.
    ///
    /// Never allocates. On a full buffer the data is dropped and counted, which
    /// is the correct behaviour for a real-time stream: back-pressure beats an
    /// OOM in a browser tab.
    pub fn write(&mut self, bytes: &[u8]) -> Result<(), RingError> {
        if bytes.is_empty() {
            return Ok(());
        }
        if bytes.len() > self.capacity() {
            self.overflow_drops += 1;
            return Err(RingError::Oversized {
                requested: bytes.len(),
                capacity: self.capacity(),
            });
        }
        if bytes.len() > self.free() {
            self.overflow_drops += 1;
            return Err(RingError::Full {
                requested: bytes.len(),
                free: self.free(),
            });
        }

        let first = (self.capacity() - self.tail).min(bytes.len());
        self.storage[self.tail..self.tail + first].copy_from_slice(&bytes[..first]);
        if first < bytes.len() {
            let rest = bytes.len() - first;
            self.storage[..rest].copy_from_slice(&bytes[first..]);
            self.wraps += 1;
        }

        self.tail = (self.tail + bytes.len()) % self.capacity();
        self.len += bytes.len();
        self.bytes_written += bytes.len() as u64;
        if self.len > self.peak_len {
            self.peak_len = self.len;
        }
        Ok(())
    }

    /// Drops the oldest `count` bytes.
    pub fn advance(&mut self, count: usize) {
        let count = count.min(self.len);
        if count == 0 {
            return;
        }
        self.head = (self.head + count) % self.capacity();
        self.len -= count;
        self.bytes_read += count as u64;
    }

    /// Copies up to `count` buffered bytes into `out` and advances.
    ///
    /// Use this only for debugging or tests; the hot path reads through
    /// [`Self::peek`] and [`Self::consume_frame`] to stay zero-copy.
    pub fn read_into(&mut self, out: &mut [u8]) -> usize {
        let count = out.len().min(self.len);
        if count == 0 {
            return 0;
        }
        let first = (self.capacity() - self.head).min(count);
        out[..first].copy_from_slice(&self.storage[self.head..self.head + first]);
        if first < count {
            out[first..count].copy_from_slice(&self.storage[..count - first]);
        }
        self.advance(count);
        count
    }

    /// Zero-copy view of the readable region plus how many bytes it holds.
    ///
    /// Returns `(&[], 0)` when empty. The slice borrows the buffer, so callers
    /// that need to advance must drop it first (see [`Self::consume_frame`]).
    pub fn peek(&self) -> (&[u8], usize) {
        let contiguous = self.contiguous_readable();
        if contiguous == 0 {
            return (&[], 0);
        }
        (&self.storage[self.head..self.head + contiguous], contiguous)
    }

    /// Total bytes of the next complete length-prefixed frame, if one is buffered.
    ///
    /// Reads only the VarInt prefix (at most 5 bytes, copied to the stack when it
    /// wraps) and never copies the payload.
    pub fn peek_frame_len(&self) -> Result<Option<usize>, ProtocolError> {
        if self.len < 1 {
            return Ok(None);
        }
        let mut prefix = [0u8; MAX_VARINT_BYTES];
        let mut available = 0usize;
        while available < MAX_VARINT_BYTES && available < self.len {
            prefix[available] = self.storage[(self.head + available) % self.capacity()];
            available += 1;
            if prefix[available - 1] & 0x80 == 0 {
                break;
            }
        }
        let (length, used) = match read_varint(&prefix[..available]) {
            Ok(value) => value,
            // prefix itself wrapped off the end of what we have: need more bytes
            Err(ProtocolError::Truncated { .. }) => return Ok(None),
            Err(error) => return Err(error),
        };
        let total = used + length.max(0) as usize;
        if total > self.capacity() {
            return Ok(None);
        }
        if self.len < total {
            Ok(None)
        } else {
            Ok(Some(total))
        }
    }

    /// Extracts one complete frame, consuming it from the ring.
    ///
    /// Contiguous frames are read straight out of `storage`; a frame straddling
    /// the wrap point is stitched together in one `Vec` sized exactly to the
    /// frame. That is the only allocation on the packet path and it is bounded by
    /// the frame size, not the buffer size.
    pub fn consume_frame(&mut self) -> Result<Option<(i32, Vec<u8>)>, ProtocolError> {
        let total = match self.peek_frame_len()? {
            Some(total) => total,
            None => return Ok(None),
        };

        let (slice, contiguous) = self.peek();
        let (packet_id, payload) = if contiguous >= total {
            let decoded = decode_frame(&slice[..total])?;
            let (id, body, _consumed) = decoded.expect("frame length already validated");
            (id, body)
        } else {
            let mut stitched = vec![0u8; total];
            let first = (self.capacity() - self.head).min(total);
            stitched[..first].copy_from_slice(&self.storage[self.head..self.head + first]);
            if first < total {
                stitched[first..].copy_from_slice(&self.storage[..total - first]);
            }
            let decoded = decode_frame(&stitched)?;
            let (id, body, _consumed) = decoded.expect("frame length already validated");
            (id, body)
        };

        self.advance(total);
        self.frames += 1;
        Ok(Some((packet_id, payload)))
    }

    /// Empties the buffer without freeing it.
    pub fn reset(&mut self) {
        self.head = 0;
        self.tail = 0;
        self.len = 0;
        self.reset_count += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{build_status_request, frame};

    /// Tests use the smallest legal capacity; the clamp still guarantees 16 MiB
    /// so the real allocation shape is what production sees.
    fn ring() -> ClientPacketRingBuffer {
        ClientPacketRingBuffer::preallocate(MIN_CAPACITY_BYTES)
    }

    #[test]
    fn capacity_is_clamped_and_allocated_once() {
        assert_eq!(ClientPacketRingBuffer::preallocate(1024).capacity(), MIN_CAPACITY_BYTES);
        assert_eq!(
            ClientPacketRingBuffer::preallocate(MAX_CAPACITY_BYTES * 4).capacity(),
            MAX_CAPACITY_BYTES
        );
        assert_eq!(ClientPacketRingBuffer::default().capacity(), DEFAULT_CAPACITY_BYTES);
        assert!(DEFAULT_CAPACITY_BYTES >= MIN_CAPACITY_BYTES && DEFAULT_CAPACITY_BYTES <= MAX_CAPACITY_BYTES);
    }

    #[test]
    fn write_and_read_round_trip() {
        let mut ring = ring();
        let payload = b"chunk-data";
        ring.write(payload).unwrap();
        assert_eq!(ring.len(), payload.len());
        let mut out = [0u8; 32];
        assert_eq!(ring.read_into(&mut out), payload.len());
        assert_eq!(&out[..payload.len()], payload);
        assert!(ring.is_empty());
    }

    #[test]
    fn wraps_without_reallocating() {
        let mut ring = ring();
        let capacity = ring.capacity();
        let block = vec![7u8; capacity / 3];
        // push well past the end of storage repeatedly
        for _ in 0..8 {
            ring.write(&block).unwrap();
            let mut sink = vec![0u8; block.len()];
            ring.read_into(&mut sink);
        }
        assert!(ring.stats().wraps > 0, "expected at least one wrap");
        assert_eq!(ring.capacity(), capacity, "capacity must never change");
        assert_eq!(ring.len(), 0);
    }

    #[test]
    fn frame_survives_a_wrap_boundary() {
        let mut ring = ring();
        let capacity = ring.capacity();
        let filler = vec![0u8; capacity - 8];
        ring.write(&filler).unwrap();

        // this frame starts 8 bytes before the end of storage and wraps
        let outbound = frame(&[0xAB, 0xCD, 0xEF]);
        ring.write(&outbound).unwrap();

        // drain the filler so the frame is at the head, straddling the wrap
        let mut sink = vec![0u8; filler.len()];
        ring.read_into(&mut sink);

        let (packet_id, payload) = ring.consume_frame().unwrap().expect("frame should decode");
        assert_eq!(packet_id, 0xAB);
        assert_eq!(payload, vec![0xCD, 0xEF]);
        assert!(ring.is_empty());
        assert_eq!(ring.stats().frames, 1);
    }

    #[test]
    fn partial_frame_waits_and_then_decodes() {
        let mut ring = ring();
        let outbound = build_status_request();
        let split_at = outbound.len() / 2;
        ring.write(&outbound[..split_at]).unwrap();
        assert!(ring.consume_frame().unwrap().is_none(), "half a frame must wait");
        ring.write(&outbound[split_at..]).unwrap();
        let (id, _) = ring.consume_frame().unwrap().unwrap();
        assert_eq!(id, crate::protocol::serverbound::STATUS_REQUEST);
    }

    #[test]
    fn overflow_is_refused_and_counted_not_grown() {
        let mut ring = ring();
        let capacity = ring.capacity();
        ring.write(&vec![1u8; capacity]).unwrap();
        assert_eq!(ring.free(), 0);
        let error = ring.write(&[9u8; 16]).unwrap_err();
        assert!(matches!(error, RingError::Full { .. }));
        assert_eq!(ring.capacity(), capacity);
        assert_eq!(ring.stats().overflow_drops, 1);
        assert_eq!(ring.stats().peak_len, capacity);

        let oversized = ring.write(&vec![0u8; capacity + 1]).unwrap_err();
        assert!(matches!(oversized, RingError::Oversized { .. }));
    }

    #[test]
    fn peek_is_zero_copy_until_wrap() {
        let mut ring = ring();
        ring.write(b"abcdef").unwrap();
        let (slice, contiguous) = ring.peek();
        assert_eq!(contiguous, 6);
        assert_eq!(slice, b"abcdef");
        ring.advance(2);
        let (slice, contiguous) = ring.peek();
        assert_eq!(contiguous, 4);
        assert_eq!(slice, b"cdef");
    }

    #[test]
    fn reset_keeps_the_allocation() {
        let mut ring = ring();
        let capacity = ring.capacity();
        ring.write(b"abc").unwrap();
        ring.reset();
        assert_eq!(ring.len(), 0);
        assert_eq!(ring.capacity(), capacity);
        assert_eq!(ring.stats().reset_count, 1);
        ring.write(b"xyz").unwrap();
        assert_eq!(ring.len(), 3);
    }
}
