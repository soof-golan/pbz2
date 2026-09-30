use crate::Error;
use crate::backend::{Backend, Recent};
use crate::bits::BitReader;
use crate::crc;
use crate::huffman::{MAX_ALPHABET, MAX_CODE_LENGTH, Table};
use crate::level::Level;
use crate::randomized;

pub(crate) const BLOCK_MAGIC: u64 = 0x3141_5926_5359;
pub(crate) const END_MAGIC: u64 = 0x1772_4538_5090;
pub(crate) const BLOCK_SIZE_STEP: usize = 100_000;

/// Scratch words needed to decode a block at `level`.
#[must_use]
pub const fn decode_scratch_words(level: Level) -> usize {
    let block = level.block_bytes();
    block + ((block + 3) >> 2) + (STARTS + 1) * PAGE_WORDS
}

const RUN_B: u16 = 1;
const MIN_GROUPS: usize = 2;
const MAX_GROUPS: usize = 6;
const GROUP_SIZE: usize = 50;
const MAX_SELECTORS: usize = 18002;
const LONGEST_RUN_WEIGHT: u32 = 2 * 1024 * 1024;
const SYMBOLS_PER_FILL: usize = 2;
const WRITTEN_AHEAD: usize = 8;

/// A decoded block whose bytes are read out of the scratch given to [`decode_block_into`].
#[derive(Debug, Clone)]
pub struct BlockOutput {
    bytes_at: u32,
    position: u32,
    randomized: bool,
    random_countdown: u16,
    random_index: u16,
    last: u8,
    run: u8,
    repeats_left: u32,
    crc: u32,
    stored_crc: u32,
    block_length: u32,
    moved_past: u32,
    end_bit: u64,
}

impl BlockOutput {
    #[inline]
    fn next_byte(&mut self, scratch: &[u32]) -> u8 {
        let position = self.position as usize;
        self.position += 1;
        let word = scratch
            .get(self.bytes_at as usize + (position >> 2))
            .copied()
            .unwrap_or(0);
        let mut byte = (word >> ((position & 3) << 3)) as u8;
        if self.randomized {
            if self.random_countdown == 0 {
                self.random_countdown = randomized::FLIP_GAPS[usize::from(self.random_index)];
                self.random_index = (self.random_index + 1) & 511;
            }
            self.random_countdown -= 1;
            if self.random_countdown == 1 {
                byte ^= 1;
            }
        }
        byte
    }

    fn load_eight(scratch: &[u32], region: usize, index: usize) -> Option<u64> {
        let at = region + (index >> 2);
        let words = scratch.get(at..at + 3)?;
        let low = u64::from(words[0]) | (u64::from(words[1]) << 32);
        let shift = (index & 3) << 3;
        Some(if shift == 0 {
            low
        } else {
            (low >> shift) | (u64::from(words[2]) << (64 - shift))
        })
    }

    fn eight_bytes(&self, scratch: &[u32]) -> Option<u64> {
        Self::load_eight(scratch, self.bytes_at as usize, self.position as usize)
    }

    fn copy_without_runs(&mut self, scratch: &[u32], out: &mut [u8]) -> usize {
        const LANES: u64 = 0x8080_8080_8080_8080;
        const LOW: u64 = 0x7f7f_7f7f_7f7f_7f7f;
        let mut written = 0;
        while written + 8 <= out.len() && self.position as usize + 12 <= self.block_length as usize
        {
            let Some(bytes) = self.eight_bytes(scratch) else {
                break;
            };
            let before = (bytes << 8) | u64::from(self.last);
            let difference = bytes ^ before;
            let mut same = !((((difference & LOW) + LOW) | difference) | LOW);
            if self.run == 0 {
                same &= !0x80;
            }
            let carried = self.run.saturating_sub(1);
            let ends = (same & (same << 8) & (same << 16))
                | (same & 0x80 & 0u64.wrapping_sub(u64::from(carried == 2)))
                | (same & (same << 8) & 0x8000 & 0u64.wrapping_sub(u64::from(carried == 1)));
            if ends != 0 {
                let last = (ends.trailing_zeros() >> 3) as usize;
                if last == 7 {
                    break;
                }
                let byte = (bytes >> (last * 8)) as u8;
                let repeats = usize::from((bytes >> ((last + 1) * 8)) as u8);
                if written + last + 1 + repeats > out.len() {
                    break;
                }
                out[written..written + 8].copy_from_slice(&bytes.to_le_bytes());
                written += last + 1;
                out[written..written + repeats].fill(byte);
                written += repeats;
                self.position += last as u32 + 2;
                self.last = byte;
                self.run = 0;
                continue;
            }
            out[written..written + 8].copy_from_slice(&bytes.to_le_bytes());
            written += 8;
            self.position += 8;
            self.last = (bytes >> 56) as u8;
            let trailing = (!same & LANES).leading_zeros() >> 3;
            self.run = trailing as u8 + 1;
        }
        written
    }

    /// Writes as many of the block's bytes into `out` as fit and returns how many.
    pub fn read(&mut self, scratch: &[u32], out: &mut [u8]) -> usize {
        let mut written = 0;
        while written < out.len() {
            if !self.randomized && self.repeats_left == 0 && self.run < 4 {
                written += self.copy_without_runs(scratch, &mut out[written..]);
                if written == out.len() {
                    break;
                }
            }
            if self.repeats_left > 0 {
                let count = (self.repeats_left as usize).min(out.len() - written);
                out[written..written + count].fill(self.last);
                written += count;
                self.repeats_left -= count as u32;
                continue;
            }
            if self.position == self.block_length {
                break;
            }
            let byte = self.next_byte(scratch);
            if self.run == 4 {
                self.repeats_left = u32::from(byte);
                self.run = 0;
                continue;
            }
            out[written] = byte;
            written += 1;
            if self.run > 0 && byte == self.last {
                self.run += 1;
            } else {
                self.last = byte;
                self.run = 1;
            }
        }
        self.crc = crc::update_slice(self.crc, &out[..written]);
        written
    }

    /// Whether every byte of the block has been read.
    #[must_use]
    pub const fn is_finished(&self) -> bool {
        self.position == self.block_length && self.repeats_left == 0
    }

    /// Checks the block's checksum once every byte has been read, and returns it.
    ///
    /// # Errors
    ///
    /// [`Error::Truncated`] or [`Error::BlockCrcMismatch`].
    pub const fn finish(&self) -> Result<u32, Error> {
        if !self.is_finished() {
            return Err(Error::Truncated);
        }
        if !self.crc != self.stored_crc {
            return Err(Error::BlockCrcMismatch);
        }
        Ok(self.stored_crc)
    }

    /// The bit right after the block.
    #[must_use]
    pub const fn end_bit(&self) -> u64 {
        self.end_bit
    }

    /// The block's length before its last run-length step.
    #[must_use]
    pub const fn block_length(&self) -> u32 {
        self.block_length + self.moved_past
    }

    /// How many words [`BlockOutput::move_bytes`] needs.
    #[must_use]
    pub const fn byte_words(&self) -> usize {
        ((self.block_length as usize + 3) >> 2) - (self.position as usize >> 2)
    }

    /// Moves the unread bytes from `scratch` into `into` and returns the output that reads
    /// them there, freeing `scratch`.
    ///
    /// # Errors
    ///
    /// [`Error::ScratchTooSmall`].
    pub fn move_bytes(&self, scratch: &[u32], into: &mut [u32]) -> Result<Self, Error> {
        let words = self.byte_words();
        let skipped = self.position & !3;
        let from = self.bytes_at as usize + (skipped as usize >> 2);
        let source = scratch
            .get(from..from + words)
            .ok_or(Error::ScratchTooSmall)?;
        into.get_mut(..words)
            .ok_or(Error::ScratchTooSmall)?
            .copy_from_slice(source);
        Ok(Self {
            bytes_at: 0,
            position: self.position - skipped,
            block_length: self.block_length - skipped,
            moved_past: self.moved_past + skipped,
            ..self.clone()
        })
    }

    pub(crate) const fn with_end_bit_moved_by(mut self, bits: u64) -> Self {
        self.end_bit += bits;
        self
    }
}

/// Decodes the block whose marker is at `start_bit` of `bytes`, reading no further than
/// `end_bit`, into `scratch` of [`decode_scratch_words`] words.
///
/// # Errors
///
/// [`Error::Truncated`] if the block does not end before `end_bit`, or damaged data.
pub fn decode_block_into<B: Backend>(
    backend: B,
    bytes: &[u8],
    start_bit: u64,
    end_bit: u64,
    level: Level,
    scratch: &mut [u32],
) -> Result<BlockOutput, Error> {
    backend.run(
        #[inline(always)]
        || decode(bytes, start_bit, end_bit, level, scratch, backend),
    )
}

const LINK_LANES: usize = 4;

fn link(tt: &mut [u32]) {
    let lane_length = tt.len() / LINK_LANES;
    let lane_start: [usize; LINK_LANES] = core::array::from_fn(|lane| lane * lane_length);
    let last = lane_start[LINK_LANES - 1];
    let mut slots = [[0u32; 256]; LINK_LANES];
    for step in 0..lane_length {
        for (lane, counts) in slots.iter_mut().enumerate() {
            counts[(tt[lane_start[lane] + step] & 0xff) as usize] += 1;
        }
    }
    for entry in &tt[last + lane_length..] {
        slots[LINK_LANES - 1][(*entry & 0xff) as usize] += 1;
    }
    let mut total = 0u32;
    for byte in 0..256 {
        for lane in &mut slots {
            let count = lane[byte];
            lane[byte] = total;
            total += count;
        }
    }
    let put = |slots: &mut [u32; 256], index: usize, tt: &mut [u32]| {
        let slot = &mut slots[(tt[index] & 0xff) as usize];
        if let Some(entry) = tt.get_mut(*slot as usize) {
            *entry |= (index as u32) << 8;
        }
        *slot += 1;
    };
    for step in 0..lane_length {
        for (lane, lane_slots) in slots.iter_mut().enumerate() {
            put(lane_slots, lane_start[lane] + step, tt);
        }
    }
    for index in last + lane_length..tt.len() {
        put(&mut slots[LINK_LANES - 1], index, tt);
    }
}

const STARTS: usize = 128;
const LANES: usize = 16;
const PAGE_WORDS: usize = 256;
const MOST_PAGES: usize = 1024;
const START_FLAG: u32 = 1 << 31;
const PIECES_FROM: usize = 1 << 16;

#[inline(always)]
const fn next_row(entry: u32) -> usize {
    ((entry & !START_FLAG) >> 8) as usize
}

struct Stream<'a> {
    words: &'a mut [u32],
    next: usize,
    pending: u64,
    bits: u32,
}

impl Stream<'_> {
    #[inline(always)]
    fn push(&mut self, value: u32, bits: u32) {
        self.pending |= u64::from(value) << self.bits;
        self.bits += bits;
        if self.bits >= 32 {
            if let Some(slot) = self.words.get_mut(self.next) {
                *slot = self.pending as u32;
            }
            self.next += 1;
            self.pending >>= 32;
            self.bits -= 32;
        }
    }

    fn flush(&mut self) {
        if let Some(slot) = self.words.get_mut(self.next) {
            *slot = self.pending as u32;
        }
    }
}

fn walk_whole(tt: &[u32], out: &mut [u32], origin: usize) {
    let mut stream = Stream {
        words: out,
        next: 0,
        pending: 0,
        bits: 0,
    };
    let mut row = origin;
    for _ in 0..tt.len() {
        let entry = tt.get(row).copied().unwrap_or(0);
        stream.push(entry & 0xff, 8);
        row = next_row(entry);
    }
    stream.flush();
}

#[derive(Debug, Clone, Copy, Default)]
struct Piece {
    start: usize,
    stop: usize,
    first_page: usize,
    length: usize,
}

#[derive(Debug, Clone, Copy, Default)]
struct Walk {
    piece: usize,
    row: usize,
    page: usize,
    offset: usize,
}

const PAGE_BYTES: usize = PAGE_WORDS * 4;

struct Pages<'a> {
    bytes: &'a mut [u8],
    next: [u16; MOST_PAGES],
    used: usize,
}

impl Pages<'_> {
    fn take(&mut self) -> Option<usize> {
        if self.used == MOST_PAGES || (self.used + 1) * PAGE_BYTES > self.bytes.len() {
            return None;
        }
        self.used += 1;
        Some(self.used - 1)
    }

    #[inline(always)]
    fn put(&mut self, walk: &mut Walk, byte: u32) -> Option<()> {
        self.bytes[walk.page * PAGE_BYTES + walk.offset] = byte as u8;
        walk.offset += 1;
        if walk.offset == PAGE_BYTES {
            let page = self.take()?;
            self.next[walk.page] = page as u16;
            walk.page = page;
            walk.offset = 0;
        }
        Some(())
    }

    fn copy(&self, piece: &Piece, stream: &mut Stream<'_>) {
        let mut page = piece.first_page;
        let mut left = piece.length;
        while left > 0 {
            let bytes = left.min(PAGE_BYTES);
            let (whole, rest) =
                self.bytes[page * PAGE_BYTES..page * PAGE_BYTES + bytes].as_chunks::<4>();
            for word in whole {
                stream.push(u32::from_le_bytes(*word), 32);
            }
            if !rest.is_empty() {
                let mut word = [0u8; 4];
                word[..rest.len()].copy_from_slice(rest);
                stream.push(u32::from_le_bytes(word), (rest.len() << 3) as u32);
            }
            left -= bytes;
            page = usize::from(self.next[page]);
        }
    }
}

fn walk_in_pieces(tt: &mut [u32], arena: &mut [u32], origin: usize) -> Option<()> {
    let n = tt.len();
    let mut pieces = [Piece::default(); STARTS];
    let mut count = 0;
    for start in core::iter::once(origin)
        .chain((1..STARTS).map(|index| (index * n) >> STARTS.trailing_zeros()))
    {
        let entry = tt.get_mut(start)?;
        if *entry & START_FLAG == 0 {
            *entry |= START_FLAG;
            pieces[count].start = start;
            count += 1;
        }
    }
    let mut pages = Pages {
        bytes: bytemuck::cast_slice_mut(arena),
        next: [0; MOST_PAGES],
        used: 0,
    };
    let begin = |pages: &mut Pages<'_>, pieces: &mut [Piece; STARTS], piece: usize| {
        let page = pages.take()?;
        pieces[piece].first_page = page;
        let mut walk = Walk {
            piece,
            row: pieces[piece].start,
            page,
            ..Walk::default()
        };
        let entry = *tt.get(walk.row)?;
        pages.put(&mut walk, entry)?;
        pieces[piece].length = 1;
        walk.row = next_row(entry);
        Some(walk)
    };
    let mut lanes = [Walk::default(); LANES];
    let mut active = 0;
    let mut waiting = 0;
    while active < LANES && waiting < count {
        lanes[active] = begin(&mut pages, &mut pieces, waiting)?;
        active += 1;
        waiting += 1;
    }
    while active > 0 {
        let mut lane = 0;
        while lane < active {
            let walk = &mut lanes[lane];
            let entry = *tt.get(walk.row)?;
            if entry & START_FLAG == 0 {
                pages.put(walk, entry)?;
                pieces[walk.piece].length += 1;
                walk.row = next_row(entry);
                lane += 1;
                continue;
            }
            pieces[walk.piece].stop = walk.row;
            if waiting < count {
                lanes[lane] = begin(&mut pages, &mut pieces, waiting)?;
                waiting += 1;
                lane += 1;
            } else {
                active -= 1;
                lanes[lane] = lanes[active];
            }
        }
    }

    let mut order = [0u8; STARTS];
    let mut piece = 0;
    let mut total = 0;
    let mut chained = 0;
    for slot in &mut order[..count] {
        *slot = piece as u8;
        chained += 1;
        total += pieces[piece].length;
        let stop = pieces[piece].stop;
        piece = pieces[..count].iter().position(|next| next.start == stop)?;
        if piece == 0 {
            break;
        }
    }
    if total != n {
        return None;
    }
    let mut stream = Stream {
        words: tt,
        next: 0,
        pending: 0,
        bits: 0,
    };
    for piece in &order[..chained] {
        pages.copy(&pieces[usize::from(*piece)], &mut stream);
    }
    stream.flush();
    Some(())
}

fn unwind(tt: &mut [u32], arena: &mut [u32], origin: usize) -> bool {
    let origin = tt.get(origin).map_or(0, |entry| next_row(*entry));
    if tt.len() >= PIECES_FROM && walk_in_pieces(tt, arena, origin).is_some() {
        return true;
    }
    walk_whole(tt, arena, origin);
    false
}

#[inline(always)]
fn decode<B: Backend>(
    bytes: &[u8],
    start_bit: u64,
    end_bit: u64,
    level: Level,
    scratch: &mut [u32],
    backend: B,
) -> Result<BlockOutput, Error> {
    let largest = level.block_bytes();
    let scratch = scratch
        .get_mut(..decode_scratch_words(level))
        .ok_or(Error::ScratchTooSmall)?;
    let (tt, arena) = scratch.split_at_mut(largest);
    let mut bits = BitReader::new(bytes, start_bit, end_bit);

    if bits.read_48()? != BLOCK_MAGIC {
        return Err(Error::BadBlockMagic);
    }
    let stored_crc = bits.read(32)?;
    let randomized = bits.bit()?;
    let origin = bits.read(24)? as usize;
    if origin > largest + 10 {
        return Err(Error::BadOriginPointer);
    }

    let mut byte_of = [0u8; 256];
    let mut used = 0usize;
    let used_groups = bits.read(16)?;
    for group in 0..16 {
        if used_groups & (0x8000 >> group) == 0 {
            continue;
        }
        let used_in_group = bits.read(16)?;
        for index in 0..16 {
            if used_in_group & (0x8000 >> index) != 0 {
                byte_of[used] = (group * 16 + index) as u8;
                used += 1;
            }
        }
    }
    if used == 0 {
        return Err(Error::BadSymbolMap);
    }
    let alphabet = used + 2;

    let groups = bits.read(3)? as usize;
    if !(MIN_GROUPS..=MAX_GROUPS).contains(&groups) {
        return Err(Error::BadGroupCount);
    }
    let selector_count = bits.read(15)? as usize;
    if selector_count == 0 {
        return Err(Error::BadSelectors);
    }
    let selectors = arena
        .first_chunk_mut::<MAX_SELECTORS>()
        .ok_or(Error::ScratchTooSmall)?;
    for index in 0..selector_count {
        let mut position = 0;
        while bits.bit()? {
            position += 1;
            if position >= groups {
                return Err(Error::BadSelectors);
            }
        }
        if let Some(slot) = selectors.get_mut(index) {
            *slot = position as u32;
        }
    }
    let selector_count = selector_count.min(MAX_SELECTORS);
    let mut order = u64::from_le_bytes([0, 1, 2, 3, 4, 5, 0, 0]);
    for selector in &mut selectors[..selector_count] {
        let shift = 8 * (*selector).min(MAX_GROUPS as u32 - 1);
        let group = (order >> shift) & 0xff;
        let before = order & ((1 << shift) - 1);
        let after = order & !((1 << (shift + 8)) - 1);
        order = after | (before << 8) | group;
        *selector = group as u32;
    }

    let mut tables: [Option<Table>; MAX_GROUPS] = [const { None }; MAX_GROUPS];
    let mut lengths = [0u8; MAX_ALPHABET];
    for table in &mut tables[..groups] {
        let mut length = bits.read(5)? as i32;
        for slot in &mut lengths[..alphabet] {
            loop {
                if !(1..=i32::from(MAX_CODE_LENGTH)).contains(&length) {
                    return Err(Error::BadCodeLengths);
                }
                if !bits.bit()? {
                    break;
                }
                if bits.bit()? {
                    length -= 1;
                } else {
                    length += 1;
                }
            }
            *slot = length as u8;
        }
        *table = Some(Table::new(&lengths[..alphabet]));
    }

    let end_of_block = (used + 1) as u16;
    let mut recent = Recent::new();
    let in_register = backend.front_in_register();
    let mut length = 0usize;
    let mut weight = 1u32;
    let mut ended = false;
    for selector in &selectors[..selector_count] {
        let Some(table) = &tables[*selector as usize] else {
            return Err(Error::BadSelectors);
        };
        for step in 0..GROUP_SIZE {
            if step % SYMBOLS_PER_FILL == 0 {
                bits.fill();
            }
            let symbol = table
                .decode(&mut bits)
                .map_err(|problem| bits.check_end().err().unwrap_or(problem))?;
            if symbol == end_of_block {
                ended = true;
                break;
            }
            let is_run = symbol <= RUN_B;
            if is_run & (weight >= LONGEST_RUN_WEIGHT) {
                bits.check_end()?;
                return Err(Error::RunTooLong);
            }
            let run = u32::from(is_run).wrapping_neg();
            let count = ((weight << (symbol & 1)) & run) | (1 & !run);
            weight = ((weight << 1) & run) | (1 & !run);
            let index = recent.take_position(backend, usize::from(symbol.max(1) - 1), in_register);
            let byte = u32::from(byte_of[usize::from(index)]);
            let end = length + count as usize;
            match tt.get_mut(length..length + WRITTEN_AHEAD) {
                Some(ahead) if end <= length + WRITTEN_AHEAD => ahead.fill(byte),
                _ => match tt.get_mut(length..end) {
                    Some(span) => span.fill(byte),
                    None => {
                        bits.check_end()?;
                        return Err(Error::BlockTooLarge);
                    }
                },
            }
            length = end;
        }
        bits.check_end()?;
        if ended {
            break;
        }
    }
    if !ended {
        return Err(Error::BadSelectors);
    }

    if origin >= length {
        return Err(Error::BadOriginPointer);
    }
    link(&mut tt[..length]);
    let in_place = unwind(&mut tt[..length], arena, origin);

    Ok(BlockOutput {
        bytes_at: if in_place { 0 } else { largest as u32 },
        position: 0,
        randomized,
        random_countdown: 0,
        random_index: 0,
        last: 0,
        run: 0,
        repeats_left: 0,
        crc: crc::START,
        stored_crc,
        block_length: length as u32,
        moved_past: 0,
        end_bit: bits.position(),
    })
}
