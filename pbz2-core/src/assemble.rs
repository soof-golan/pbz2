use crate::Error;
use crate::backend::{Backend, Scalar};
use crate::bits::BitWriter;
use crate::block::END_MAGIC;
use crate::compress::{self, BlockBuilder, BlockCode, Uncopied};
use crate::crc;
use crate::encoder::{encode_scratch_words, stream_header};
use crate::level::Level;
use crate::native::native;

const STREAM_END_BYTES: usize = 16;

/// The most bytes [`encode_block`] writes for a block of `block_bytes` bytes.
#[must_use]
pub const fn encoded_block_bytes(block_bytes: usize) -> usize {
    ((18 * (block_bytes + 1) + 52_000) >> 3) + 8
}

/// A block that [`BlockSplitter`] filled, ready for [`encode_block`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputBlock {
    length: usize,
}

impl InputBlock {
    /// How many bytes of the block buffer the block uses.
    #[must_use]
    pub const fn length(&self) -> usize {
        self.length
    }
}

/// A block that [`BlockSplitter::scan`] found in raw input, ready for
/// [`encode_raw_block`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawBlock {
    raw_length: usize,
    length: usize,
}

impl RawBlock {
    /// How many raw input bytes the block holds.
    #[must_use]
    pub const fn raw_length(&self) -> usize {
        self.raw_length
    }

    /// How many bytes the block is once its runs are coded.
    #[must_use]
    pub const fn length(&self) -> usize {
        self.length
    }
}

/// A block that [`encode_block`] compressed, ready for [`StreamAssembler::append`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodedBlock {
    byte_length: usize,
    crc: u32,
}

impl EncodedBlock {
    /// How many bytes of the output buffer hold the block. Blocks are padded to whole
    /// bytes, so the stream is the blocks' bytes one after another.
    #[must_use]
    pub const fn byte_length(&self) -> usize {
        self.byte_length
    }
}

/// Cuts uncompressed data into blocks, so the blocks can be compressed on any thread.
///
/// Each block is made in one of two ways:
///
/// - [`BlockSplitter::fill`] codes the runs of the block into a buffer the caller
///   provides, for [`encode_block`].
/// - [`BlockSplitter::scan`] only finds where the block ends in the raw input, and
///   [`encode_raw_block`] codes the runs on the thread that compresses the block. The
///   caller keeps the raw bytes of the block: the [`BlockSplitter::pending_run`] from the
///   end of the last block, then the bytes `scan` took since.
///
/// [`BlockSplitter::fill_scanned`] switches a block that is being scanned to filling.
#[derive(Debug, Clone)]
pub struct BlockSplitter {
    builder: BlockBuilder,
    most: usize,
    raw: usize,
    finished: bool,
}

impl BlockSplitter {
    /// A splitter that makes blocks for `level`.
    #[must_use]
    pub const fn new(level: Level) -> Self {
        Self {
            builder: BlockBuilder::new(compress::block_limit(level)),
            most: compress::block_limit(level),
            raw: 0,
            finished: false,
        }
    }

    /// The run of equal bytes that the next block starts with: its byte and how many raw
    /// bytes of it the splitter has taken. It is written at the start of the next block.
    #[must_use]
    pub const fn pending_run(&self) -> (u8, usize) {
        self.builder.pending_run()
    }

    /// Takes as much of `input` as fits into the current block and returns how many
    /// bytes were taken, like [`BlockSplitter::fill`], without writing the block. When
    /// the block is full it is returned.
    pub fn scan(&mut self, input: &[u8]) -> (usize, Option<RawBlock>) {
        self.scan_with(native(), input)
    }

    /// Like [`BlockSplitter::scan`], with the inner loop run by `backend`.
    pub fn scan_with<B: Backend>(&mut self, backend: B, input: &[u8]) -> (usize, Option<RawBlock>) {
        let builder = &mut self.builder;
        let taken = backend.run(
            #[inline(always)]
            || builder.push(input, &mut Uncopied, backend),
        );
        self.raw += taken;
        let full = self.builder.is_full().then(|| self.take_raw());
        (taken, full)
    }

    /// Ends the data when it was scanned. Call it until it returns `None`: it returns the
    /// last block, and sometimes one more.
    pub fn finish_scan(&mut self) -> Option<RawBlock> {
        if self.finished {
            return None;
        }
        if !self.builder.flush_run(&mut Uncopied) {
            return Some(self.take_raw());
        }
        self.finished = true;
        (self.builder.filled() > 0).then(|| self.take_raw())
    }

    /// Codes the runs of the block scanned so far into `block`, from `scanned`, the raw
    /// bytes of the block. After that, [`BlockSplitter::fill`] goes on with the block.
    ///
    /// # Errors
    ///
    /// [`Error::BufferTooSmall`] if `block` is smaller than [`BlockSplitter::block_bytes`],
    /// and [`Error::NotScanned`] if `scanned` is not the raw bytes of the block.
    pub fn fill_scanned(&self, scanned: &[u8], block: &mut [u8]) -> Result<(), Error> {
        self.fill_scanned_with(Scalar, scanned, block)
    }

    /// Like [`BlockSplitter::fill_scanned`], with the inner loop run by `backend`.
    ///
    /// # Errors
    ///
    /// The same as [`BlockSplitter::fill_scanned`].
    pub fn fill_scanned_with<B: Backend>(
        &self,
        backend: B,
        scanned: &[u8],
        block: &mut [u8],
    ) -> Result<(), Error> {
        if block.len() < self.block_bytes() {
            return Err(Error::BufferTooSmall);
        }
        let mut builder = BlockBuilder::new(self.builder.limit());
        let taken = backend.run(
            #[inline(always)]
            || builder.push(scanned, block, backend),
        );
        if taken != scanned.len()
            || builder.filled() != self.builder.filled()
            || builder.pending_run() != self.builder.pending_run()
        {
            return Err(Error::NotScanned);
        }
        Ok(())
    }

    fn take_raw(&mut self) -> RawBlock {
        let pending = self.builder.pending_run().1;
        let raw_length = self.raw - pending;
        self.raw = pending;
        RawBlock {
            raw_length,
            length: self.builder.take_block(),
        }
    }

    /// How large each block buffer must be: the block size of the level.
    #[must_use]
    pub const fn block_bytes(&self) -> usize {
        self.most
    }

    /// Makes the block being filled, and the ones after it, hold at most `bytes`. It is
    /// kept between 100 KB and the block size of the level. Call it between blocks: a
    /// block already larger than `bytes` ends at its next run.
    pub fn set_block_bytes(&mut self, bytes: usize) {
        let least = compress::block_limit(Level::FASTEST).min(self.most);
        self.builder.set_limit(bytes.clamp(least, self.most));
    }

    /// Adds as much of `input` to the block in `block` as fits and returns how many bytes
    /// were taken. When the block is full it is returned, and the next call starts a new
    /// block in whatever buffer it is given. Until then, pass the same buffer each time.
    ///
    /// # Errors
    ///
    /// [`Error::BufferTooSmall`] if `block` is smaller than [`BlockSplitter::block_bytes`].
    pub fn fill(
        &mut self,
        input: &[u8],
        block: &mut [u8],
    ) -> Result<(usize, Option<InputBlock>), Error> {
        self.fill_with(Scalar, input, block)
    }

    /// Like [`BlockSplitter::fill`], with the inner loop run by `backend`.
    ///
    /// # Errors
    ///
    /// The same as [`BlockSplitter::fill`].
    pub fn fill_with<B: Backend>(
        &mut self,
        backend: B,
        input: &[u8],
        block: &mut [u8],
    ) -> Result<(usize, Option<InputBlock>), Error> {
        if block.len() < self.block_bytes() {
            return Err(Error::BufferTooSmall);
        }
        let builder = &mut self.builder;
        let taken = backend.run(
            #[inline(always)]
            || builder.push(input, block, backend),
        );
        self.raw += taken;
        let full = self.builder.is_full().then(|| self.take());
        Ok((taken, full))
    }

    /// Ends the data. Call it until it returns `None`: it returns the last block, and
    /// sometimes one more.
    ///
    /// # Errors
    ///
    /// [`Error::BufferTooSmall`] if `block` is smaller than [`BlockSplitter::block_bytes`].
    pub fn finish(&mut self, block: &mut [u8]) -> Result<Option<InputBlock>, Error> {
        if block.len() < self.block_bytes() {
            return Err(Error::BufferTooSmall);
        }
        if self.finished {
            return Ok(None);
        }
        if !self.builder.flush_run(block) {
            return Ok(Some(self.take()));
        }
        self.finished = true;
        Ok((self.builder.filled() > 0).then(|| self.take()))
    }

    fn take(&mut self) -> InputBlock {
        InputBlock {
            length: self.take_raw().length,
        }
    }
}

/// Compresses a block that [`BlockSplitter`] filled in `block`, writing its bits into
/// `out`. It can run on any thread. `scratch` must hold [`encode_scratch_words`] of the
/// block's length ([`crate::ENCODE_SCRATCH_WORDS`] is always enough); `out` must hold
/// [`encoded_block_bytes`] of it. The block buffer is changed.
///
/// # Errors
///
/// [`Error::ScratchTooSmall`] or [`Error::BufferTooSmall`] if a buffer is too small.
pub fn encode_block(
    block: &mut [u8],
    input: InputBlock,
    scratch: &mut [u32],
    out: &mut [u8],
) -> Result<EncodedBlock, Error> {
    encode_block_with(block, input, scratch, out, native())
}

/// Like [`encode_block`], with the inner loops run by `backend`.
///
/// # Errors
///
/// See [`encode_block`].
pub fn encode_block_with<B: Backend>(
    block: &mut [u8],
    input: InputBlock,
    scratch: &mut [u32],
    out: &mut [u8],
    backend: B,
) -> Result<EncodedBlock, Error> {
    let block = block.get_mut(..input.length).ok_or(Error::BufferTooSmall)?;
    if scratch.len() < encode_scratch_words(block.len()) {
        return Err(Error::ScratchTooSmall);
    }
    if out.len() < encoded_block_bytes(block.len()) {
        return Err(Error::BufferTooSmall);
    }
    let mut code = BlockCode::EMPTY;
    code.encode(backend, block, scratch);
    let (byte_length, _) = write_code(&mut code, scratch, out);
    Ok(EncodedBlock {
        byte_length,
        crc: code.crc(),
    })
}

/// Compresses a block that [`BlockSplitter::scan`] found. `raw` starts with the block's
/// raw bytes; their runs are coded into `block`, which must hold [`RawBlock::length`]
/// bytes, and then it is compressed like [`encode_block`] does.
///
/// # Errors
///
/// [`Error::NotScanned`] if `raw` does not start with the bytes the splitter scanned for
/// the block, and the errors of [`encode_block`].
pub fn encode_raw_block(
    raw: &[u8],
    input: RawBlock,
    block: &mut [u8],
    scratch: &mut [u32],
    out: &mut [u8],
) -> Result<EncodedBlock, Error> {
    encode_raw_block_with(raw, input, block, scratch, out, native())
}

/// Like [`encode_raw_block`], with the inner loops run by `backend`.
///
/// # Errors
///
/// See [`encode_raw_block`].
pub fn encode_raw_block_with<B: Backend>(
    raw: &[u8],
    input: RawBlock,
    block: &mut [u8],
    scratch: &mut [u32],
    out: &mut [u8],
    backend: B,
) -> Result<EncodedBlock, Error> {
    let filled = code_runs_with(raw, input, block, backend)?;
    encode_block_with(block, filled, scratch, out, backend)
}

/// The first step of [`encode_raw_block_with`]: codes the runs of the block's raw bytes
/// into `block`, which must hold [`RawBlock::length`] bytes. After it, `raw` is no longer
/// needed, and [`encode_block_with`] compresses `block`.
///
/// # Errors
///
/// [`Error::NotScanned`] if `raw` does not start with the bytes the splitter scanned for
/// the block, and [`Error::BufferTooSmall`] if `block` is too small.
pub fn code_runs_with<B: Backend>(
    raw: &[u8],
    input: RawBlock,
    block: &mut [u8],
    backend: B,
) -> Result<InputBlock, Error> {
    let raw = raw.get(..input.raw_length).ok_or(Error::NotScanned)?;
    let block = block.get_mut(..input.length).ok_or(Error::BufferTooSmall)?;
    let mut builder = BlockBuilder::new(input.length);
    if builder.push(raw, block, backend) != raw.len()
        || !builder.flush_run(block)
        || builder.filled() != input.length
    {
        return Err(Error::NotScanned);
    }
    Ok(InputBlock {
        length: input.length,
    })
}

fn write_code(code: &mut BlockCode, scratch: &[u32], out: &mut [u8]) -> (usize, u32) {
    let mut writer = BitWriter::new();
    let mut written = 0;
    loop {
        let (count, finished) = code.write(scratch, &mut writer, &mut out[written..]);
        written += count;
        if finished {
            break;
        }
    }
    written += writer.drain(&mut out[written..]);
    (written, writer.count())
}

/// Joins compressed blocks, in the order the [`BlockSplitter`] made them, into a bzip2
/// stream written to buffers the caller provides.
#[derive(Debug, Clone)]
pub struct StreamAssembler {
    level: Level,
    stream_crc: u32,
    started: bool,
}

impl StreamAssembler {
    /// An assembler for a stream at `level`, the level the blocks were split at.
    #[must_use]
    pub const fn new(level: Level) -> Self {
        Self {
            level,
            stream_crc: 0,
            started: false,
        }
    }

    fn start(&mut self, out: &mut [u8]) -> usize {
        if self.started {
            return 0;
        }
        self.started = true;
        out[..4].copy_from_slice(&stream_header(self.level).to_be_bytes());
        4
    }

    /// Writes the next block into `out` and returns how many bytes were written. `bytes`
    /// holds the block as [`encode_block`] wrote it. `out` must hold 16 bytes more than
    /// the block.
    ///
    /// # Errors
    ///
    /// [`Error::BufferTooSmall`] if `bytes` is shorter than the block or `out` is too small.
    pub fn append(
        &mut self,
        bytes: &[u8],
        block: EncodedBlock,
        out: &mut [u8],
    ) -> Result<usize, Error> {
        let bytes = bytes
            .get(..block.byte_length)
            .ok_or(Error::BufferTooSmall)?;
        if out.len() < bytes.len() + STREAM_END_BYTES {
            return Err(Error::BufferTooSmall);
        }
        let written = self.start(out);
        self.stream_crc = crc::combine(self.stream_crc, block.crc);
        out[written..written + bytes.len()].copy_from_slice(bytes);
        Ok(written + bytes.len())
    }

    /// Ends the stream, writing its last bytes into `out`, and returns how many were
    /// written.
    ///
    /// # Errors
    ///
    /// [`Error::BufferTooSmall`] if `out` holds fewer than 16 bytes.
    pub fn finish(mut self, out: &mut [u8]) -> Result<usize, Error> {
        if out.len() < STREAM_END_BYTES {
            return Err(Error::BufferTooSmall);
        }
        let written = self.start(out);
        out[written..written + 6].copy_from_slice(&END_MAGIC.to_be_bytes()[2..]);
        out[written + 6..written + 10].copy_from_slice(&self.stream_crc.to_be_bytes());
        Ok(written + 10)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;

    fn random() -> impl FnMut() -> u64 {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        }
    }

    fn filled_blocks(input: &[u8], chunk: usize) -> vec::Vec<vec::Vec<u8>> {
        let mut splitter = BlockSplitter::new(Level::FASTEST);
        let mut block = vec![0u8; splitter.block_bytes()];
        let mut blocks = vec::Vec::new();
        for piece in input.chunks(chunk) {
            let mut piece = piece;
            while !piece.is_empty() {
                let (taken, full) = splitter.fill(piece, &mut block).expect("block fits");
                piece = &piece[taken..];
                if let Some(full) = full {
                    blocks.push(block[..full.length()].to_vec());
                }
            }
        }
        while let Some(last) = splitter.finish(&mut block).expect("block fits") {
            blocks.push(block[..last.length()].to_vec());
        }
        blocks
    }

    fn scanned_blocks(input: &[u8], chunk: usize) -> vec::Vec<vec::Vec<u8>> {
        let mut splitter = BlockSplitter::new(Level::FASTEST);
        let mut raw = vec::Vec::new();
        let mut blocks = vec::Vec::new();
        let mut block = vec![0u8; splitter.block_bytes()];
        let mut builder = |raw: &[u8], found: RawBlock| {
            let mut coder = BlockBuilder::new(found.length());
            let taken = coder.push(&raw[..found.raw_length()], &mut block[..], Scalar);
            assert_eq!(taken, found.raw_length());
            assert!(coder.flush_run(&mut block[..]));
            assert_eq!(coder.filled(), found.length());
            block[..found.length()].to_vec()
        };
        for piece in input.chunks(chunk) {
            let mut piece = piece;
            while !piece.is_empty() {
                let (taken, full) = splitter.scan(piece);
                raw.extend_from_slice(&piece[..taken]);
                piece = &piece[taken..];
                if let Some(full) = full {
                    blocks.push(builder(&raw, full));
                    raw.drain(..full.raw_length());
                    let (byte, count) = splitter.pending_run();
                    assert_eq!(raw, vec![byte; count]);
                }
            }
        }
        while let Some(last) = splitter.finish_scan() {
            blocks.push(builder(&raw, last));
            raw.drain(..last.raw_length());
        }
        assert!(raw.is_empty());
        blocks
    }

    #[test]
    fn scanned_blocks_match_filled_blocks() {
        let mut next = random();
        for round in 0..40 {
            let mut input = vec::Vec::new();
            while input.len() < 250_000 {
                let byte = (next() % [4, 256][round % 2]) as u8;
                let run = match next() % 8 {
                    0 => 200 + (next() % 400) as usize,
                    1..=3 => 1 + (next() % 6) as usize,
                    _ => 1,
                };
                input.extend(core::iter::repeat_n(byte, run));
            }
            let chunk = 1 + (next() % 70_000) as usize;
            assert_eq!(
                scanned_blocks(&input, chunk),
                filled_blocks(&input, chunk),
                "round {round}"
            );
        }
    }

    #[test]
    fn set_block_bytes_caps_blocks() {
        let mut next = random();
        let input: vec::Vec<u8> = (0..700_000).map(|_| (next() % 8) as u8).collect();
        let lengths = |bytes: usize, scan: bool| {
            let mut splitter = BlockSplitter::new(Level::BEST);
            splitter.set_block_bytes(bytes);
            let mut block = vec![0u8; splitter.block_bytes()];
            let full = if scan {
                splitter.scan(&input).1.map(|found| found.length())
            } else {
                let (_, full) = splitter.fill(&input, &mut block).expect("block fits");
                full.map(|found| found.length())
            };
            full.expect("the input is longer than a block")
        };
        for scan in [false, true] {
            let capped = lengths(150_000, scan);
            assert!((149_995..=150_000).contains(&capped), "{capped}");
            let least = lengths(10, scan);
            assert!((99_976..=99_981).contains(&least), "{least}");
        }
        assert_eq!(lengths(150_000, true), lengths(150_000, false));
    }

    #[test]
    fn fill_scanned_goes_on_where_scan_stopped() {
        let mut next = random();
        let input: vec::Vec<u8> = (0..300_000).map(|_| (next() % 3) as u8).collect();
        let whole = filled_blocks(&input, input.len());
        let mut splitter = BlockSplitter::new(Level::FASTEST);
        let mut block = vec![0u8; splitter.block_bytes()];
        let (taken, full) = splitter.scan(&input[..1000]);
        assert_eq!((taken, full), (1000, None));
        assert_eq!(
            splitter.fill_scanned(&input[..999], &mut block),
            Err(Error::NotScanned)
        );
        splitter
            .fill_scanned(&input[..1000], &mut block)
            .expect("the bytes are the scanned ones");
        let (_, full) = splitter
            .fill(&input[1000..], &mut block)
            .expect("block fits");
        let full = full.expect("the input is longer than a block");
        assert_eq!(block[..full.length()], whole[0]);
    }

    #[test]
    fn raw_blocks_must_be_the_scanned_bytes() {
        let mut splitter = BlockSplitter::new(Level::FASTEST);
        let input = b"aaaaaaaabcd";
        let (taken, _) = splitter.scan(input);
        assert_eq!(taken, input.len());
        let found = splitter.finish_scan().expect("there is a block");
        assert_eq!((found.raw_length(), found.length()), (11, 8));
        let mut block = vec![0u8; splitter.block_bytes()];
        let mut scratch = vec![0u32; encode_scratch_words(splitter.block_bytes())];
        let mut out = vec![0u8; encoded_block_bytes(splitter.block_bytes())];
        let mut encode =
            |raw: &[u8]| encode_raw_block(raw, found, &mut block, &mut scratch, &mut out).err();
        assert_eq!(encode(b"aaaaaaaabcd"), None);
        assert_eq!(encode(b"aaaaaaaabc"), Some(Error::NotScanned));
        assert_eq!(encode(b"aaaaaaabbcd"), Some(Error::NotScanned));
    }

    #[test]
    fn encoded_blocks_end_on_a_byte() {
        let mut next = random();
        let level = Level::FASTEST;
        let mut splitter = BlockSplitter::new(level);
        let mut block = vec![0u8; splitter.block_bytes()];
        let mut scratch = vec![0u32; encode_scratch_words(splitter.block_bytes())];
        let mut out = vec![0u8; encoded_block_bytes(splitter.block_bytes())];
        for round in 0..300 {
            let length = 1 + (next() % 6000) as usize;
            let alphabet = 1 + next() % 256;
            let input: vec::Vec<u8> = (0..length).map(|_| (next() % alphabet) as u8).collect();
            splitter = BlockSplitter::new(level);
            let (_, full) = splitter.fill(&input, &mut block).expect("block fits");
            assert_eq!(full, None);
            let last = splitter
                .finish(&mut block)
                .expect("block fits")
                .expect("input is not empty");
            let mut code = BlockCode::EMPTY;
            code.encode(native(), &mut block[..last.length], &mut scratch);
            let (_, bits_left) = write_code(&mut code, &scratch, &mut out);
            assert_eq!(bits_left, 0, "round {round}");
        }
    }
}
