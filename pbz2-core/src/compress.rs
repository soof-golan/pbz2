use crate::backend::Backend;
use crate::bits::BitWriter;
use crate::block::BLOCK_MAGIC;
use crate::bwt;
use crate::crc;
use crate::huffman::{MAX_ALPHABET, MAX_CODE_LENGTH, canonical_codes, code_lengths};
use crate::level::Level;
use crate::tables;

const GROUP_SIZE: usize = 50;
pub(crate) const MAX_GROUPS: usize = 6;
const MAX_SELECTORS: usize = 18002;
const ITERATIONS: usize = 4;
const COUNT_LANES: usize = 4;
const LESSER_COST: u8 = 0;
const GREATER_COST: u8 = 15;
const LONGEST_RUN: u16 = 255;
const MIN_BLOCK_BYTES: usize = 8;

pub(crate) const fn block_limit(level: Level) -> usize {
    level.block_bytes() - 19
}

pub(crate) const fn scratch_words(block_bytes: usize) -> usize {
    bwt::scratch_words(block_bytes)
}

pub(crate) const fn usable_block_bytes(level: Level, buffer_bytes: usize) -> Option<usize> {
    let limit = block_limit(level);
    let usable = if buffer_bytes < limit {
        buffer_bytes
    } else {
        limit
    };
    if usable < MIN_BLOCK_BYTES {
        None
    } else {
        Some(usable)
    }
}

const LOW: u64 = 0x7f7f_7f7f_7f7f_7f7f;

const fn word_steps() -> [[u16; 256]; 4] {
    let mut steps = [[0u16; 256]; 4];
    let mut class = 0;
    while class < 4 {
        let mut mask = 0;
        while mask < 256 {
            let mut run = class as u16 + 1;
            let mut flushed = 0u16;
            let mut bit = 0;
            while bit < 8 {
                if (mask >> bit) & 1 == 1 {
                    run += 1;
                } else {
                    flushed += if run >= 4 { 5 } else { run };
                    run = 1;
                }
                bit += 1;
            }
            steps[class][mask] = flushed | (run << 8);
            mask += 1;
        }
        class += 1;
    }
    steps
}

static WORD_STEPS: [[u16; 256]; 4] = word_steps();

const fn has_zero_byte(word: u64) -> bool {
    !((((word & LOW) + LOW) | word) | LOW) != 0
}

pub(crate) fn block_crc(block: &[u8]) -> u32 {
    let mut crc = crc::START;
    let mut span_start = 0;
    let mut at = 0;
    let mut last = 0u8;
    let mut same = 0u8;
    while at < block.len() {
        if same < 4
            && let Some(chunk) = block.get(at..at + 8)
        {
            let bytes = u64::from_le_bytes(chunk.try_into().unwrap_or([0; 8]));
            let with_last = (bytes << 8) | u64::from(last);
            if !has_zero_byte(bytes ^ with_last) {
                last = (bytes >> 56) as u8;
                same = 1;
                at += 8;
                continue;
            }
        }
        let byte = block[at];
        if same == 4 {
            crc = crc::update_slice(crc, &block[span_start..at]);
            let run = [last; LONGEST_RUN as usize];
            crc = crc::update_slice(crc, &run[..usize::from(byte)]);
            span_start = at + 1;
            same = 0;
        } else if same > 0 && byte == last {
            same += 1;
        } else {
            last = byte;
            same = 1;
        }
        at += 1;
    }
    !crc::update_slice(crc, &block[span_start..])
}

pub(crate) fn find_byte(bytes: &[u8], byte: u8) -> usize {
    let spread = u64::from(byte) * 0x0101_0101_0101_0101;
    let (chunks, rest) = bytes.as_chunks::<8>();
    for (index, chunk) in chunks.iter().enumerate() {
        let word = u64::from_le_bytes(*chunk) ^ spread;
        let zeros = !((((word & LOW) + LOW) | word) | LOW);
        if zeros != 0 {
            return (index << 3) + (zeros.trailing_zeros() >> 3) as usize;
        }
    }
    chunks.len() * 8
        + rest
            .iter()
            .position(|next| *next == byte)
            .unwrap_or(rest.len())
}

pub(crate) fn equal_prefix(bytes: &[u8], byte: u8) -> usize {
    let spread = u64::from(byte) * 0x0101_0101_0101_0101;
    let (chunks, rest) = bytes.as_chunks::<8>();
    for (index, chunk) in chunks.iter().enumerate() {
        let difference = u64::from_le_bytes(*chunk) ^ spread;
        if difference != 0 {
            return (index << 3) + (difference.trailing_zeros() >> 3) as usize;
        }
    }
    chunks.len() * 8 + rest.iter().take_while(|next| **next == byte).count()
}

pub(crate) trait Rle1Output {
    const COPIES: bool = true;

    fn literals(&mut self, at: usize, bytes: [u8; 8]);
    fn run(&mut self, at: usize, byte: u8, length: u16);
}

impl Rle1Output for [u8] {
    #[inline(always)]
    fn literals(&mut self, at: usize, bytes: [u8; 8]) {
        self[at..at + 8].copy_from_slice(&bytes);
    }

    #[inline(always)]
    fn run(&mut self, at: usize, byte: u8, length: u16) {
        let copies = usize::from(length.min(4));
        self[at..at + copies].fill(byte);
        if length >= 4 {
            self[at + 4] = (length - 4) as u8;
        }
    }
}

pub(crate) struct Uncopied;

impl Rle1Output for Uncopied {
    const COPIES: bool = false;

    #[inline(always)]
    fn literals(&mut self, _: usize, _: [u8; 8]) {}

    #[inline(always)]
    fn run(&mut self, _: usize, _: u8, _: u16) {}
}

#[derive(Debug, Clone)]
pub(crate) struct BlockBuilder {
    limit: usize,
    filled: usize,
    run_byte: u8,
    run_length: u16,
    full: bool,
}

impl BlockBuilder {
    pub(crate) const fn new(limit: usize) -> Self {
        Self {
            limit,
            filled: 0,
            run_byte: 0,
            run_length: 0,
            full: false,
        }
    }

    pub(crate) const fn is_full(&self) -> bool {
        self.full
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.filled == 0 && self.run_length == 0
    }

    pub(crate) const fn limit(&self) -> usize {
        self.limit
    }

    pub(crate) const fn set_limit(&mut self, limit: usize) {
        self.limit = limit;
    }

    pub(crate) const fn filled(&self) -> usize {
        self.filled
    }

    pub(crate) const fn pending_run(&self) -> (u8, usize) {
        (self.run_byte, self.run_length as usize)
    }

    #[inline(always)]
    fn copy_short_runs<O: Rle1Output + ?Sized, B: Backend>(
        &mut self,
        backend: B,
        input: &[u8],
        block: &mut O,
    ) -> usize {
        let pending = usize::from(self.run_length);
        if !(1..=3).contains(&pending) {
            return 0;
        }
        let room = self.limit.saturating_sub(self.filled + pending) / 16;
        let (chunks, _) = input.as_chunks::<16>();
        let mut taken = 0;
        for chunk in chunks.iter().take(room) {
            let pending = usize::from(self.run_length);
            let mask = backend.equal_to_previous(chunk, self.run_byte);
            let equal = (u32::from(mask) << 2) | [0b00, 0b10, 0b11][pending - 1];
            if equal & (equal << 1) & (equal << 2) != 0 {
                break;
            }
            block.run(self.filled, self.run_byte, pending as u16);
            let (low, high) = chunk.split_at(8);
            block.literals(self.filled + pending, low.try_into().unwrap_or([0; 8]));
            block.literals(self.filled + pending + 8, high.try_into().unwrap_or([0; 8]));
            let trailing = 1 + mask.leading_ones() as usize;
            self.filled += pending + 16 - trailing;
            self.run_byte = chunk[15];
            self.run_length = trailing as u16;
            taken += 16;
        }
        taken
    }

    #[inline(always)]
    fn copy_literals<O: Rle1Output + ?Sized>(&mut self, input: &[u8], block: &mut O) -> usize {
        let (chunks, _) = input.as_chunks::<8>();
        let mut taken = 0;
        for chunk in chunks {
            if self.filled + 8 > self.limit {
                break;
            }
            let bytes = u64::from_le_bytes(*chunk);
            let with_pending = (bytes << 8) | u64::from(self.run_byte);
            let difference = bytes ^ with_pending;
            if has_zero_byte(difference) {
                break;
            }
            block.literals(self.filled, with_pending.to_le_bytes());
            self.filled += 8;
            self.run_byte = (bytes >> 56) as u8;
            taken += 8;
        }
        taken
    }

    #[inline(always)]
    fn count_without_runs<B: Backend>(&mut self, backend: B, input: &[u8]) -> usize {
        let (blocks, _) = input.as_chunks::<64>();
        let mut taken = 0;
        for block in blocks {
            let pending = self.run_length;
            if pending == 0 || pending > 3 {
                break;
            }
            let (chunks, _) = block.as_chunks::<16>();
            let mut previous = self.run_byte;
            let mut equal = 0u64;
            for (quarter, chunk) in chunks.iter().enumerate() {
                equal |= u64::from(backend.equal_to_previous(chunk, previous)) << (quarter * 16);
                previous = chunk[15];
            }
            let carried = ((1u128 << (pending - 1)) - 1) << (4 - pending);
            let pairs = (u128::from(equal) << 3) | carried;
            let run_length = equal.leading_ones() as u16 + 1;
            let flushed = 64 + usize::from(pending) - usize::from(run_length);
            if pairs & (pairs >> 1) & (pairs >> 2) != 0 || self.filled + flushed > self.limit {
                break;
            }
            self.filled += flushed;
            self.run_length = run_length;
            self.run_byte = previous;
            taken += 64;
        }
        taken
    }

    #[inline(always)]
    fn count_chunks<B: Backend>(&mut self, backend: B, input: &[u8]) -> usize {
        let mut taken = self.count_without_runs(backend, input);
        let (chunks, _) = input[taken..].as_chunks::<16>();
        for chunk in chunks {
            let pending = self.run_length;
            if pending == 0 || LONGEST_RUN - 16 <= pending {
                break;
            }
            let mask = backend.equal_to_previous(chunk, self.run_byte);
            for (half, last) in [(mask as u8, chunk[7]), ((mask >> 8) as u8, chunk[15])] {
                let pending = self.run_length;
                let step = WORD_STEPS[usize::from(pending.min(4)) - 1][usize::from(half)];
                let flushed = usize::from(step as u8);
                if self.filled + flushed > self.limit {
                    return taken;
                }
                self.filled += flushed;
                self.run_length = if half == 0xff { pending + 8 } else { step >> 8 };
                self.run_byte = last;
                taken += 8;
            }
        }
        taken
    }

    #[inline(always)]
    pub(crate) fn push<O: Rle1Output + ?Sized, B: Backend>(
        &mut self,
        input: &[u8],
        block: &mut O,
        backend: B,
    ) -> usize {
        if self.full {
            return 0;
        }
        let mut taken = 0;
        while taken < input.len() {
            if !O::COPIES {
                taken += self.count_chunks(backend, &input[taken..]);
            } else {
                taken += self.copy_short_runs(backend, &input[taken..], block);
                if self.run_length == 1 {
                    taken += self.copy_literals(&input[taken..], block);
                }
            }
            if taken == input.len() {
                break;
            }
            let byte = input[taken];
            if self.run_length > 0 && byte == self.run_byte && self.run_length < LONGEST_RUN {
                let room = usize::from(LONGEST_RUN - self.run_length);
                let end = input.len().min(taken + room);
                let same = equal_prefix(&input[taken..end], byte);
                self.run_length += same as u16;
                taken += same;
                continue;
            }
            if self.run_length > 0 && !self.flush_run(block) {
                return taken;
            }
            self.run_byte = byte;
            self.run_length = 1;
            taken += 1;
        }
        taken
    }

    pub(crate) fn flush_run<O: Rle1Output + ?Sized>(&mut self, block: &mut O) -> bool {
        if self.run_length == 0 {
            return true;
        }
        let bytes = if self.run_length >= 4 {
            5
        } else {
            usize::from(self.run_length)
        };
        if self.filled + bytes > self.limit {
            self.full = true;
            return false;
        }
        block.run(self.filled, self.run_byte, self.run_length);
        self.filled += bytes;
        self.run_length = 0;
        true
    }

    pub(crate) const fn take_block(&mut self) -> usize {
        let taken = self.filled;
        self.filled = 0;
        self.full = false;
        taken
    }
}

fn counts_of(lanes: &[[u32; MAX_ALPHABET]; COUNT_LANES]) -> [u32; MAX_ALPHABET] {
    core::array::from_fn(|symbol| lanes.iter().map(|lane| lane[symbol]).sum())
}

fn tally(lanes: &mut [[u32; MAX_ALPHABET]; COUNT_LANES], chunk: &[u16], step: u32) {
    let (runs, rest) = chunk.as_chunks::<COUNT_LANES>();
    for run in runs {
        for (lane, symbol) in lanes.iter_mut().zip(run) {
            lane[*symbol as usize] = lane[*symbol as usize].wrapping_add(step);
        }
    }
    for symbol in rest {
        lanes[0][*symbol as usize] = lanes[0][*symbol as usize].wrapping_add(step);
    }
}

#[derive(Debug, Clone, Copy)]
enum Step {
    Header(usize),
    Symbol(usize),
    Done,
}

#[derive(Debug, Clone)]
pub(crate) struct BlockCode {
    crc: u32,
    origin: u32,
    used: [bool; 256],
    alphabet: usize,
    symbols_at: usize,
    symbol_count: usize,
    header_at: usize,
    header_bits: usize,
    groups: usize,
    selector_count: usize,
    selectors: [u8; MAX_SELECTORS],
    lengths: [[u8; MAX_ALPHABET]; MAX_GROUPS],
    codes: [[u32; MAX_ALPHABET]; MAX_GROUPS],
    step: Step,
}

impl BlockCode {
    pub(crate) const EMPTY: Self = Self {
        crc: 0,
        origin: 0,
        used: [false; 256],
        alphabet: 0,
        symbols_at: 0,
        symbol_count: 0,
        header_at: 0,
        header_bits: 0,
        groups: 0,
        selector_count: 0,
        selectors: [0; MAX_SELECTORS],
        lengths: [[0; MAX_ALPHABET]; MAX_GROUPS],
        codes: [[0; MAX_ALPHABET]; MAX_GROUPS],
        step: Step::Done,
    };

    pub(crate) const fn crc(&self) -> u32 {
        self.crc
    }

    pub(crate) fn encode<B: Backend>(&mut self, backend: B, block: &mut [u8], scratch: &mut [u32]) {
        self.crc = block_crc(block);
        let symbols = bwt::symbols(backend, block, scratch);
        self.origin = symbols.origin;
        self.used = symbols.used;
        self.alphabet = symbols.alphabet;
        self.symbols_at = bwt::symbols_at(block.len());
        self.symbol_count = symbols.count;
        self.header_at = block.len() + 1;
        let (low, header) = scratch.split_at_mut(self.header_at);
        let coded = self.symbols(low);
        let symbol_bits = self.choose_tables(coded, &symbols.frequencies);
        let header = bytemuck::cast_slice_mut(header);
        let mut starts = core::array::from_fn(|table| self.lengths[table][0]);
        let padding = (self.write_header(&starts, header) + symbol_bits).wrapping_neg() & 7;
        if padding & 1 == 1 {
            self.selectors[self.selector_count] = self.selectors[self.selector_count - 1];
            self.selector_count += 1;
        }
        let steps = (padding >> 1) as u8;
        starts[0] = if starts[0] + steps <= MAX_CODE_LENGTH {
            starts[0] + steps
        } else {
            starts[0] - steps
        };
        self.header_bits = self.write_header(&starts, header);
        self.step = Step::Header(0);
    }

    pub(crate) fn symbols<'s>(&self, scratch: &'s [u32]) -> &'s [u16] {
        bwt::symbols_in(scratch, self.symbols_at, self.symbol_count)
    }

    fn write_header(&self, starts: &[u8; MAX_GROUPS], out: &mut [u8]) -> usize {
        let mut writer = BitWriter::new();
        let mut at = 0;
        let mut put = |value: u32, bits: u32| {
            writer.put(value, bits);
            if writer.count() >= 32 {
                writer.flush_word(&mut out[at..]);
                at += 4;
            }
        };
        put((BLOCK_MAGIC >> 24) as u32, 24);
        put(BLOCK_MAGIC as u32 & 0xff_ffff, 24);
        put(self.crc, 32);
        put(self.origin, 25);
        let used: [u32; 16] = core::array::from_fn(|group| self.used_group(group));
        put(
            (0..16)
                .filter(|group| used[*group] != 0)
                .fold(0, |bits, group| bits | 0x8000 >> group),
            16,
        );
        for bits in used.into_iter().filter(|bits| *bits != 0) {
            put(bits, 16);
        }
        put(
            ((self.groups as u32) << 15) | self.selector_count as u32,
            18,
        );
        const LANE_ONES: u64 = u64::MAX / 0xff;
        let mut order = u64::from_le_bytes([0, 1, 2, 3, 4, 5, 0xff, 0xff]);
        for selector in &self.selectors[..self.selector_count] {
            let difference = order ^ (u64::from(*selector) * LANE_ONES);
            let equal = difference.wrapping_sub(LANE_ONES) & !difference & (LANE_ONES << 7);
            let position = (equal.trailing_zeros() / 8).min(MAX_GROUPS as u32 - 1);
            let shift = 8 * position;
            let before = order & ((1 << shift) - 1);
            let after = order & !((1 << (shift + 8)) - 1);
            order = after | (before << 8) | u64::from(*selector);
            put((2 << position) - 2, position + 1);
        }
        for (lengths, start) in self.lengths[..self.groups].iter().zip(starts) {
            let mut length = *start;
            put(u32::from(length), 5);
            for wanted in &lengths[..self.alphabet] {
                while length < *wanted {
                    put(0b10, 2);
                    length += 1;
                }
                while length > *wanted {
                    put(0b11, 2);
                    length -= 1;
                }
                put(0, 1);
            }
        }
        let bits = at * 8 + writer.count() as usize;
        writer.pad_to_byte();
        writer.drain(&mut out[at..]);
        bits
    }

    fn choose_tables(&mut self, symbols: &[u16], frequencies: &[u32; MAX_ALPHABET]) -> usize {
        let alphabet = self.alphabet;
        let groups = match symbols.len() {
            0..200 => 2,
            200..600 => 3,
            600..1200 => 4,
            1200..2400 => 5,
            _ => 6,
        };
        self.groups = groups;

        let mut remaining = symbols.len() as u32;
        let mut first = 0usize;
        for part in (1..=groups).rev() {
            let target = remaining / part as u32;
            let mut end = first;
            let mut taken = 0;
            while taken < target && end < alphabet {
                taken += frequencies[end];
                end += 1;
            }
            if end > first + 1 && part != groups && part != 1 && (groups - part) & 1 == 1 {
                end -= 1;
                taken -= frequencies[end];
            }
            for (symbol, length) in self.lengths[part - 1][..alphabet].iter_mut().enumerate() {
                *length = if (first..end).contains(&symbol) {
                    LESSER_COST
                } else {
                    GREATER_COST
                };
            }
            first = end;
            remaining -= taken;
        }

        let mut lanes = [[[0u32; MAX_ALPHABET]; COUNT_LANES]; MAX_GROUPS];
        self.selector_count = symbols.len().div_ceil(GROUP_SIZE);
        for iteration in 0..ITERATIONS {
            let costs = tables::Costs::new(&self.lengths, groups, alphabet);
            let mut changed = false;
            for (chunk, selector) in symbols.chunks(GROUP_SIZE).zip(&mut self.selectors) {
                let best = costs.cheapest(chunk) as u8;
                if iteration > 0 {
                    if *selector == best {
                        continue;
                    }
                    tally(&mut lanes[usize::from(*selector)], chunk, u32::MAX);
                }
                tally(&mut lanes[usize::from(best)], chunk, 1);
                *selector = best;
                changed = true;
            }
            if !changed {
                break;
            }
            for (lengths, lanes) in self.lengths[..groups].iter_mut().zip(&lanes) {
                code_lengths(&counts_of(lanes)[..alphabet], &mut lengths[..alphabet]);
            }
        }
        for (codes, lengths) in self.codes[..groups].iter_mut().zip(&self.lengths) {
            canonical_codes(&lengths[..alphabet], &mut codes[..alphabet]);
        }
        self.lengths[..groups]
            .iter()
            .zip(&lanes)
            .map(|(lengths, lanes)| {
                counts_of(lanes)[..alphabet]
                    .iter()
                    .zip(lengths)
                    .map(|(count, length)| *count as usize * usize::from(*length))
                    .sum::<usize>()
            })
            .sum()
    }

    fn used_group(&self, group: usize) -> u32 {
        (0..16)
            .filter(|index| self.used[group * 16 + index])
            .fold(0, |bits, index| bits | 0x8000 >> index)
    }

    fn put_symbols(
        &self,
        mut index: usize,
        symbols: &[u16],
        writer: &mut BitWriter,
        out: &mut [u8],
        written: &mut usize,
    ) -> usize {
        while index < self.symbol_count && out.len() - *written >= 8 {
            let group = index / GROUP_SIZE;
            let table = usize::from(self.selectors[group]);
            let (codes, lengths) = (&self.codes[table], &self.lengths[table]);
            let group_end = ((group + 1) * GROUP_SIZE).min(self.symbol_count);
            let end = group_end.min(index + (out.len() - *written - 8) / 4 + 1);
            for symbol in &symbols[index..end] {
                let symbol = *symbol as usize;
                writer.put(codes[symbol], u32::from(lengths[symbol]));
                if writer.count() >= 32 {
                    writer.flush_word(&mut out[*written..]);
                    *written += 4;
                }
            }
            index = end;
        }
        index
    }

    pub(crate) fn write(
        &mut self,
        scratch: &[u32],
        writer: &mut BitWriter,
        out: &mut [u8],
    ) -> (usize, bool) {
        let symbols = self.symbols(scratch);
        let header: &[u8] = bytemuck::cast_slice(&scratch[self.header_at..]);
        let mut written = 0;
        loop {
            written += writer.drain(&mut out[written..]);
            match self.step {
                Step::Symbol(index) if index == self.symbol_count => {
                    self.step = Step::Done;
                }
                Step::Symbol(start) if out.len() - written >= 8 => {
                    let index = self.put_symbols(start, symbols, writer, out, &mut written);
                    self.step = Step::Symbol(index);
                }
                Step::Done => return (written, true),
                _ if !writer.has_room() => return (written, false),
                Step::Symbol(index) => {
                    let table = usize::from(self.selectors[index / GROUP_SIZE]);
                    let symbol = symbols[index] as usize;
                    let bits = u32::from(self.lengths[table][symbol]);
                    writer.put(self.codes[table][symbol], bits);
                    self.step = Step::Symbol(index + 1);
                }
                Step::Header(word) if word * 32 >= self.header_bits => {
                    self.step = Step::Symbol(0);
                }
                Step::Header(word) => {
                    let bits = (self.header_bits - word * 32).min(32) as u32;
                    let value = header[word * 4..word * 4 + 4]
                        .try_into()
                        .map_or(0, u32::from_be_bytes);
                    writer.put(value >> (32 - bits), bits);
                    self.step = Step::Header(word + 1);
                }
            }
        }
    }
}
