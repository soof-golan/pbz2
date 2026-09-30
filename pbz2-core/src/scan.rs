use crate::backend::Backend;
use crate::block::{BLOCK_MAGIC, END_MAGIC};

/// Which bzip2 marker was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerKind {
    /// The start of a block.
    Block,
    /// The end of a stream.
    End,
}

/// A marker at a bit position from the start of the data. It may be a false marker inside
/// compressed data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Marker {
    /// Which marker it is.
    pub kind: MarkerKind,
    /// The marker's first bit.
    pub bit: u64,
}

const fn byte_inside(magic: u64, offset: u32) -> u8 {
    ((magic >> (40 - offset)) & 0xff) as u8
}

const fn worth_a_look() -> [bool; 256] {
    let mut table = [false; 256];
    let mut offset = 32;
    while offset <= 39 {
        table[byte_inside(BLOCK_MAGIC, offset) as usize] = true;
        table[byte_inside(END_MAGIC, offset) as usize] = true;
        offset += 1;
    }
    table
}

static WORTH_A_LOOK: [bool; 256] = worth_a_look();

const PAIR_AHEAD: usize = 5;
const WHOLE_CHECK_BYTES: usize = 8;

const fn marker_pairs() -> [u64; 1024] {
    let mut table = [0u64; 1024];
    let magics = [BLOCK_MAGIC, END_MAGIC];
    let mut which = 0;
    while which < magics.len() {
        let mut offset = 32;
        while offset <= 39 {
            let first = (magics[which] >> (72 - offset)) & 0xff;
            let second = (magics[which] >> (64 - offset)) & 0xff;
            let pair = ((first << 8) | second) as usize;
            table[pair >> 6] |= 1 << (pair & 63);
            offset += 1;
        }
        which += 1;
    }
    table
}

static MARKER_PAIRS: [u64; 1024] = marker_pairs();

#[cfg(feature = "simd")]
const fn pair_nibbles() -> [[u8; 16]; 4] {
    let mut tables = [[0u8; 16]; 4];
    let magics = [BLOCK_MAGIC, END_MAGIC];
    let mut which = 0;
    while which < magics.len() {
        let mut offset = 32;
        while offset <= 39 {
            let bit = 1u8 << (offset - 32);
            let first = ((magics[which] >> (72 - offset)) & 0xff) as usize;
            let second = ((magics[which] >> (64 - offset)) & 0xff) as usize;
            tables[0][first & 15] |= bit;
            tables[1][first >> 4] |= bit;
            tables[2][second & 15] |= bit;
            tables[3][second >> 4] |= bit;
            offset += 1;
        }
        which += 1;
    }
    tables
}

#[cfg(feature = "simd")]
pub(crate) const PAIR_NIBBLES: [[u8; 16]; 4] = pair_nibbles();

pub(crate) fn exact_marker_pairs(bytes: &[u8]) -> u16 {
    bytes
        .windows(2)
        .take(16)
        .enumerate()
        .fold(0, |mask, (at, pair)| {
            mask | (u16::from(starts_a_marker_pair(pair[0], pair[1])) << at)
        })
}

fn window_before(piece: &[u8], end: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&piece[end - 8..end]);
    u64::from_be_bytes(bytes)
}

#[inline(always)]
fn starts_a_marker_pair(first: u8, second: u8) -> bool {
    let pair = (usize::from(first) << 8) | usize::from(second);
    MARKER_PAIRS[pair >> 6] >> (pair & 63) & 1 != 0
}

/// Finds block and end-of-stream markers in data that arrives in pieces.
#[derive(Debug, Clone, Default)]
pub struct Scanner {
    window: u64,
    bytes_seen: u64,
    first_byte: u64,
}

impl Scanner {
    /// A scanner at the start of the data.
    #[must_use]
    pub const fn new() -> Self {
        Self::starting_at(0)
    }

    pub(crate) const fn starting_at(first_byte: u64) -> Self {
        Self {
            window: 0,
            bytes_seen: first_byte,
            first_byte,
        }
    }

    pub(crate) fn first_in<B: Backend>(
        &mut self,
        backend: B,
        piece: &[u8],
        from_bit: u64,
    ) -> Option<Marker> {
        let mut first = None;
        self.scan_until(backend, piece, |marker| {
            if marker.bit < from_bit {
                return false;
            }
            first = Some(marker);
            true
        });
        first
    }

    /// Scans the next piece and calls `found` for each marker that ends in it, in order.
    pub fn scan<B: Backend>(&mut self, backend: B, piece: &[u8], mut found: impl FnMut(Marker)) {
        backend.run(
            #[inline(always)]
            || {
                self.scan_until(backend, piece, |marker| {
                    found(marker);
                    false
                });
            },
        );
    }

    #[inline(always)]
    fn scan_until<B: Backend>(
        &mut self,
        backend: B,
        piece: &[u8],
        mut found: impl FnMut(Marker) -> bool,
    ) {
        let whole = piece.len().min(WHOLE_CHECK_BYTES);
        for byte in &piece[..whole] {
            if self.push(*byte, &mut found) {
                return;
            }
        }
        if piece.len() <= WHOLE_CHECK_BYTES {
            return;
        }
        let piece_start = self.bytes_seen - whole as u64;
        let pairs = &piece[WHOLE_CHECK_BYTES - PAIR_AHEAD..piece.len() - PAIR_AHEAD + 1];
        let mut base = 0;
        while base < pairs.len().saturating_sub(1) {
            let mask = match pairs[base..].first_chunk::<17>() {
                Some(chunk) => backend.marker_pairs(chunk),
                None => exact_marker_pairs(&pairs[base..]),
            };
            let mut left = mask;
            while left != 0 {
                let at = base + left.trailing_zeros() as usize;
                left &= left - 1;
                if !starts_a_marker_pair(pairs[at], pairs[at + 1]) {
                    continue;
                }
                let end = at + WHOLE_CHECK_BYTES;
                self.window = window_before(piece, end);
                self.bytes_seen = piece_start + end as u64;
                if self.push(piece[end], &mut found) {
                    return;
                }
            }
            base += 16;
        }
        self.window = window_before(piece, piece.len());
        self.bytes_seen = piece_start + piece.len() as u64;
    }

    #[inline]
    fn push(&mut self, byte: u8, found: &mut impl FnMut(Marker) -> bool) -> bool {
        let index = self.bytes_seen;
        self.window = (self.window << 8) | u64::from(byte);
        self.bytes_seen += 1;
        let previous = (self.window >> 8) as u8;
        if !WORTH_A_LOOK[usize::from(previous)] {
            return false;
        }
        for offset in (32..=39u64).rev() {
            let Some(bit) = (index * 8).checked_sub(8 + offset) else {
                continue;
            };
            if bit < self.first_byte * 8 {
                continue;
            }
            let value = (self.window << (48 - offset)) >> 16;
            let kind = match value {
                BLOCK_MAGIC => MarkerKind::Block,
                END_MAGIC => MarkerKind::End,
                _ => continue,
            };
            if found(Marker { kind, bit }) {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Scalar;

    fn magic_at(bit: u32, magic: u64) -> [u8; 16] {
        let value = u128::from(magic) << (128 - 48 - bit);
        value.to_be_bytes()
    }

    #[test]
    fn finds_marker_at_any_bit_and_split() {
        for bit in 0..60 {
            let bytes = magic_at(bit, BLOCK_MAGIC);
            for split in 0..bytes.len() {
                let mut scanner = Scanner::new();
                let mut found = [None; 2];
                let mut count = 0;
                let (first, second) = bytes.split_at(split);
                for piece in [first, second] {
                    scanner.scan(Scalar, piece, |marker| {
                        found[count] = Some(marker);
                        count += 1;
                    });
                }
                assert_eq!(count, 1, "bit {bit} split {split}");
                assert_eq!(
                    found[0],
                    Some(Marker {
                        kind: MarkerKind::Block,
                        bit: u64::from(bit)
                    })
                );
            }
        }
    }

    fn with_magic_at(bytes: &mut [u8], bit: u64, magic: u64) {
        for index in 0..48 {
            if magic >> (47 - index) & 1 == 1 {
                let at = bit + index;
                bytes[(at >> 3) as usize] |= 0x80 >> (at & 7);
            }
        }
    }

    #[test]
    fn finds_markers_far_into_long_pieces() {
        for bit in 0..200u64 {
            let mut bytes = [0u8; 400];
            with_magic_at(&mut bytes, 1000 + bit, BLOCK_MAGIC);
            with_magic_at(&mut bytes, 2000 + bit, END_MAGIC);
            let expected = [
                Marker {
                    kind: MarkerKind::Block,
                    bit: 1000 + bit,
                },
                Marker {
                    kind: MarkerKind::End,
                    bit: 2000 + bit,
                },
            ];
            for piece_bytes in [400, 131, 64, 9, 8, 3] {
                let mut scanner = Scanner::new();
                let mut found = [None; 3];
                let mut count = 0;
                for piece in bytes.chunks(piece_bytes) {
                    scanner.scan(Scalar, piece, |marker| {
                        found[count.min(2)] = Some(marker);
                        count += 1;
                    });
                }
                assert_eq!(count, 2, "bit {bit}, pieces of {piece_bytes}");
                assert_eq!(
                    found[..2],
                    expected.map(Some),
                    "bit {bit}, pieces of {piece_bytes}"
                );
                assert_eq!(scanner.bytes_seen, 400);
            }
        }
    }

    #[test]
    fn distinguishes_end_and_block_markers() {
        let mut scanner = Scanner::new();
        let mut kinds = [None; 1];
        scanner.scan(Scalar, &magic_at(13, END_MAGIC), |marker| {
            kinds[0] = Some(marker);
        });
        assert_eq!(
            kinds[0],
            Some(Marker {
                kind: MarkerKind::End,
                bit: 13
            })
        );
    }

    #[test]
    fn starting_at_offsets_bit_positions() {
        let bytes = magic_at(21, BLOCK_MAGIC);
        let mut scanner = Scanner::starting_at(2);
        let found = scanner.first_in(Scalar, &bytes[2..], 0);
        assert_eq!(
            found,
            Some(Marker {
                kind: MarkerKind::Block,
                bit: 21
            })
        );
    }
}
