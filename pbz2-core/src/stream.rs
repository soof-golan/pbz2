use crate::Error;
use crate::backend::Backend;
use crate::bits::BitReader;
use crate::block::{BLOCK_MAGIC, BlockOutput, END_MAGIC, decode_block_into, decode_scratch_words};
use crate::crc;
use crate::level::Level;
use crate::native::{Native, native};
use crate::scan::{MarkerKind, Scanner};

/// An input buffer this large holds any bzip2 block, even one that compressed badly.
pub const MAX_COMPRESSED_BLOCK_BYTES: usize = 2_400_000;

const HEADER_BITS: u64 = 32;
const MAGIC_BITS: u64 = 48;
const END_BITS: u64 = MAGIC_BITS + 32;

/// What [`Decoder::pull`] or [`crate::Encoder::pull`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pulled {
    /// This many bytes were written.
    Bytes(usize),
    /// More input is needed before anything more can be written.
    NeedInput,
    /// All output has been written.
    Finished,
}

pub(crate) const fn stream_level(header: u32) -> Option<Level> {
    match header.to_be_bytes() {
        [b'B', b'Z', b'h', digit] => Level::from_digit(digit),
        _ => None,
    }
}

enum State {
    StreamHeader,
    BlockOrEnd(Level),
    Block(BlockOutput, Level),
    Finished,
}

/// A sequential bzip2 decoder: push compressed bytes, pull decompressed bytes.
///
/// It needs an input buffer of [`MAX_COMPRESSED_BLOCK_BYTES`] and a scratch space of
/// [`crate::decode_scratch_words`] for [`Level::BEST`].
pub struct Decoder<Buffer, Scratch, B = Native> {
    buffer: Buffer,
    scratch: Scratch,
    backend: B,
    filled: usize,
    buffer_bit: u64,
    position: u64,
    input_ended: bool,
    stream_crc: u32,
    streams: u32,
    searched_from: u64,
    tried_to: u64,
    state: State,
}

impl<Buffer: AsMut<[u8]> + AsRef<[u8]>, Scratch: AsMut<[u32]> + AsRef<[u32]>>
    Decoder<Buffer, Scratch, Native>
{
    /// A decoder that runs in the [`Native`] backend.
    pub fn new(buffer: Buffer, scratch: Scratch) -> Self {
        Self::with_backend(buffer, scratch, native())
    }
}

impl<Buffer: AsMut<[u8]> + AsRef<[u8]>, Scratch: AsMut<[u32]> + AsRef<[u32]>, B: Backend>
    Decoder<Buffer, Scratch, B>
{
    /// A decoder that runs in `backend`.
    pub const fn with_backend(buffer: Buffer, scratch: Scratch, backend: B) -> Self {
        Self {
            buffer,
            scratch,
            backend,
            filled: 0,
            buffer_bit: 0,
            position: 0,
            input_ended: false,
            stream_crc: 0,
            streams: 0,
            searched_from: 0,
            tried_to: 0,
            state: State::StreamHeader,
        }
    }

    /// Copies as much of `input` as fits and returns how many bytes were taken.
    pub fn push(&mut self, input: &[u8]) -> usize {
        self.drop_consumed_input();
        let buffer = self.buffer.as_mut();
        let taken = input.len().min(buffer.len() - self.filled);
        buffer[self.filled..self.filled + taken].copy_from_slice(&input[..taken]);
        self.filled += taken;
        taken
    }

    /// The free end of the input buffer, to read into instead of calling [`Decoder::push`].
    pub fn spare_input(&mut self) -> &mut [u8] {
        self.drop_consumed_input();
        &mut self.buffer.as_mut()[self.filled..]
    }

    /// Adds `count` bytes written into [`Decoder::spare_input`] to the input.
    pub fn commit_input(&mut self, count: usize) {
        self.filled = (self.filled + count).min(self.buffer.as_ref().len());
    }

    /// Says that all input has been pushed.
    pub const fn end_input(&mut self) {
        self.input_ended = true;
    }

    /// Writes decompressed bytes into `out`.
    ///
    /// # Errors
    ///
    /// Damaged or incomplete data, or [`Error::BufferTooSmall`] if a block does not fit.
    pub fn pull(&mut self, out: &mut [u8]) -> Result<Pulled, Error> {
        loop {
            match &mut self.state {
                State::Finished => return Ok(Pulled::Finished),
                State::Block(output, level) => {
                    if !output.is_finished() {
                        return Ok(Pulled::Bytes(output.read(self.scratch.as_ref(), out)));
                    }
                    let block_crc = output.finish()?;
                    self.stream_crc = crc::combine(self.stream_crc, block_crc);
                    self.position = output.end_bit();
                    self.state = State::BlockOrEnd(*level);
                }
                State::StreamHeader => {
                    if let Some(pulled) = self.read_stream_header()? {
                        return Ok(pulled);
                    }
                }
                State::BlockOrEnd(level) => {
                    let level = *level;
                    if let Some(pulled) = self.read_block_or_end(level)? {
                        return Ok(pulled);
                    }
                }
            }
        }
    }

    /// Whether ignored bytes followed the last stream.
    #[must_use]
    pub fn has_trailing_bytes(&self) -> bool {
        matches!(self.state, State::Finished) && self.available_end() > self.position
    }

    fn available_end(&self) -> u64 {
        self.buffer_bit + self.filled as u64 * 8
    }

    fn reader(&self) -> BitReader<'_> {
        BitReader::new(
            &self.buffer.as_ref()[..self.filled],
            self.position - self.buffer_bit,
            self.available_end() - self.buffer_bit,
        )
    }

    fn drop_consumed_input(&mut self) {
        let consumed = ((self.position - self.buffer_bit) >> 3) as usize;
        if consumed == 0 {
            return;
        }
        self.buffer.as_mut().copy_within(consumed..self.filled, 0);
        self.filled -= consumed;
        self.buffer_bit += consumed as u64 * 8;
    }

    fn wait_for_input(&mut self) -> Result<Option<Pulled>, Error> {
        if self.input_ended {
            return Err(Error::Truncated);
        }
        self.drop_consumed_input();
        if self.filled == self.buffer.as_ref().len() {
            return Err(Error::BufferTooSmall);
        }
        Ok(Some(Pulled::NeedInput))
    }

    fn read_stream_header(&mut self) -> Result<Option<Pulled>, Error> {
        if self.available_end() < self.position + HEADER_BITS {
            if !self.input_ended {
                return self.wait_for_input();
            }
            if self.streams == 0 {
                return Err(Error::NotBzip2);
            }
            self.state = State::Finished;
            return Ok(None);
        }
        let header = self.reader().read(32)?;
        let Some(level) = stream_level(header) else {
            if self.streams == 0 {
                return Err(Error::NotBzip2);
            }
            self.state = State::Finished;
            return Ok(None);
        };
        if self.scratch.as_ref().len() < decode_scratch_words(level) {
            return Err(Error::ScratchTooSmall);
        }
        self.stream_crc = 0;
        self.position += HEADER_BITS;
        self.state = State::BlockOrEnd(level);
        Ok(None)
    }

    fn read_block_or_end(&mut self, level: Level) -> Result<Option<Pulled>, Error> {
        if self.available_end() < self.position + MAGIC_BITS {
            return self.wait_for_input();
        }
        match self.reader().read_48()? {
            BLOCK_MAGIC => match self.decode_available_block(level)? {
                Some(output) => {
                    self.searched_from = 0;
                    self.state = State::Block(output, level);
                    Ok(None)
                }
                None => self.wait_for_input(),
            },
            END_MAGIC => {
                if self.available_end() < self.position + END_BITS {
                    return self.wait_for_input();
                }
                let mut reader = self.reader();
                reader.skip(MAGIC_BITS as u32)?;
                if reader.read(32)? != self.stream_crc {
                    return Err(Error::StreamCrcMismatch);
                }
                self.position = (self.position + END_BITS + 7) & !7;
                self.streams += 1;
                self.state = State::StreamHeader;
                Ok(None)
            }
            _ => Err(Error::BadBlockMagic),
        }
    }

    fn decode_available_block(&mut self, level: Level) -> Result<Option<BlockOutput>, Error> {
        if self.filled == self.buffer.as_ref().len() {
            self.drop_consumed_input();
        }
        let start = self.position;
        let end = self.available_end();
        let buffer_full = self.filled == self.buffer.as_ref().len();
        let all_input = self.input_ended || buffer_full;
        if !all_input {
            let from = self.searched_from.max(start + 1);
            let Some(marker) = self.next_marker(from) else {
                self.searched_from = end.saturating_sub(MAGIC_BITS).max(from);
                return Ok(None);
            };
            self.searched_from = marker;
            if end - start < 2 * self.tried_to.saturating_sub(start) {
                return Ok(None);
            }
        }
        let bytes = &self.buffer.as_ref()[..self.filled];
        match decode_block_into(
            self.backend,
            bytes,
            start - self.buffer_bit,
            end - self.buffer_bit,
            level,
            self.scratch.as_mut(),
        ) {
            Ok(output) => {
                self.tried_to = 0;
                Ok(Some(output.with_end_bit_moved_by(self.buffer_bit)))
            }
            Err(Error::Truncated) if !all_input => {
                self.tried_to = end;
                self.searched_from = end.saturating_sub(MAGIC_BITS).max(start + 1);
                Ok(None)
            }
            Err(Error::Truncated) if !self.input_ended && buffer_full => Err(Error::BufferTooSmall),
            Err(problem) => Err(problem),
        }
    }

    fn next_marker(&self, from: u64) -> Option<u64> {
        let first_byte = (from - self.buffer_bit) >> 3;
        let bytes = &self.buffer.as_ref()[first_byte as usize..self.filled];
        let backend = self.backend;
        backend
            .run(
                #[inline(always)]
                || {
                    Scanner::starting_at(first_byte).first_in(
                        backend,
                        bytes,
                        from - self.buffer_bit,
                    )
                },
            )
            .map(|marker| {
                debug_assert!(matches!(marker.kind, MarkerKind::Block | MarkerKind::End));
                marker.bit + self.buffer_bit
            })
    }
}
