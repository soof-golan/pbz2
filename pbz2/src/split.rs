use std::sync::{Arc, Mutex};

use pbz2_core::{
    Backend, BlockOutput, Error, Level, MAX_COMPRESSED_BLOCK_BYTES, MarkerKind, Scanner,
    decode_block_into_with,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Start,
    Block,
    End,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Segment {
    pub(crate) kind: Kind,
    pub(crate) bytes: Vec<u8>,
    pub(crate) first_bit: u64,
    pub(crate) bit_length: u64,
}

const EXPANDED_BYTES: usize = 2 << 20;
const SMALLEST_BYTES: usize = 64 << 10;
const MOST_SPARES: usize = 64;

#[derive(Clone, Default)]
pub(crate) struct Spares(Arc<Mutex<Vec<Vec<u8>>>>);

impl Spares {
    fn take(&self, length: usize) -> Vec<u8> {
        let spare = self.0.lock().ok().and_then(|mut spares| spares.pop());
        match spare {
            Some(mut bytes) => {
                bytes.resize(length, 0);
                bytes
            }
            None => vec![0; length],
        }
    }

    pub(crate) fn give(&self, bytes: Vec<u8>) {
        if let Ok(mut spares) = self.0.lock()
            && spares.len() < MOST_SPARES
        {
            spares.push(bytes);
        }
    }
}

pub(crate) struct Decoded {
    pub(crate) output: BlockOutput,
    bytes: Vec<u8>,
    start: usize,
    end: usize,
    rest: Vec<u32>,
}

impl Decoded {
    pub(crate) fn refill(&mut self) -> bool {
        if self.start == self.end && !self.output.is_finished() {
            self.end = self.output.read(&self.rest, &mut self.bytes);
            self.start = 0;
        }
        self.start < self.end
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes[self.start..self.end]
    }

    pub(crate) fn consume(&mut self, count: usize) {
        self.start = (self.start + count).min(self.end);
    }

    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl Segment {
    pub(crate) const fn end_bit(&self) -> u64 {
        self.first_bit + self.bit_length
    }

    pub(crate) fn decode<B: Backend>(
        &self,
        scratch: &mut [u32],
        spares: &Spares,
        backend: B,
    ) -> Result<Decoded, Error> {
        let mut output = decode_block_into_with(
            &self.bytes,
            self.first_bit,
            self.end_bit(),
            Level::BEST,
            scratch,
            backend,
        )?;
        let expected = output.block_length() as usize;
        let first = (expected + (expected >> 3)).clamp(SMALLEST_BYTES, EXPANDED_BYTES);
        let mut bytes = spares.take(first);
        let mut end = 0;
        while !output.is_finished() {
            if end == bytes.len() {
                if end == EXPANDED_BYTES {
                    break;
                }
                bytes.resize(EXPANDED_BYTES, 0);
            }
            end += output.read(scratch, &mut bytes[end..]);
        }
        let mut rest = Vec::new();
        if !output.is_finished() {
            rest = vec![0u32; output.byte_words()];
            output = output.move_bytes(scratch, &mut rest)?;
        }
        Ok(Decoded {
            output,
            bytes,
            start: 0,
            end,
            rest,
        })
    }

    pub(crate) fn join(&mut self, next: &Self) {
        let shares_a_byte = self.end_bit() & 7 != 0;
        self.bytes
            .extend_from_slice(&next.bytes[usize::from(shares_a_byte)..]);
        self.bit_length += next.bit_length;
    }

    #[cfg(test)]
    pub(crate) fn split_at(&self, bit: u64) -> (Self, Self) {
        let at = self.first_bit + bit;
        let first = Self {
            kind: self.kind,
            bytes: self.bytes[..((at + 7) >> 3) as usize].to_vec(),
            first_bit: self.first_bit,
            bit_length: bit,
        };
        let second = Self {
            kind: Kind::Block,
            bytes: self.bytes[(at >> 3) as usize..].to_vec(),
            first_bit: at & 7,
            bit_length: self.bit_length - bit,
        };
        (first, second)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Splitter {
    scanner: Scanner,
    pending: Vec<u8>,
    pending_bit: u64,
    current: (Kind, u64),
    markers: Vec<(Kind, u64)>,
}

impl Splitter {
    pub(crate) const fn new() -> Self {
        Self {
            scanner: Scanner::new(),
            pending: Vec::new(),
            pending_bit: 0,
            current: (Kind::Start, 0),
            markers: Vec::new(),
        }
    }

    pub(crate) fn push(&mut self, piece: &[u8], out: &mut Vec<Segment>) -> Result<(), Error> {
        self.pending.extend_from_slice(piece);
        let mut markers = std::mem::take(&mut self.markers);
        self.scanner.scan(piece, |marker| {
            let kind = match marker.kind {
                MarkerKind::Block => Kind::Block,
                MarkerKind::End => Kind::End,
            };
            markers.push((kind, marker.bit));
        });
        for (kind, bit) in markers.drain(..) {
            out.push(self.segment_to(bit, kind));
        }
        self.markers = markers;
        let consumed = ((self.current.1 - self.pending_bit) >> 3) as usize;
        self.pending.drain(..consumed);
        self.pending_bit += consumed as u64 * 8;
        if self.pending.len() > MAX_COMPRESSED_BLOCK_BYTES + 16 {
            return Err(Error::BlockTooLarge);
        }
        Ok(())
    }

    pub(crate) fn finish(mut self, out: &mut Vec<Segment>) {
        let end = self.pending_bit + self.pending.len() as u64 * 8;
        out.push(self.segment_to(end, Kind::End));
    }

    fn segment_to(&mut self, bit: u64, next: Kind) -> Segment {
        let (kind, from) = self.current;
        let first_byte = (from - self.pending_bit) >> 3;
        let segment = Segment {
            kind,
            bytes: self.pending[first_byte as usize..((bit - self.pending_bit + 7) >> 3) as usize]
                .to_vec(),
            first_bit: (from - self.pending_bit) & 7,
            bit_length: bit - from,
        };
        self.current = (next, bit);
        segment
    }
}
