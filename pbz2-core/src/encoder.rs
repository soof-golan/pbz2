use crate::Error;
use crate::backend::Backend;
use crate::bits::BitWriter;
use crate::block::END_MAGIC;
use crate::compress::{self, BlockBuilder, BlockCode};
use crate::crc;
use crate::level::Level;
use crate::native::{Native, native};
use crate::stream::Pulled;

/// Scratch words [`Encoder`] needs for blocks of up to `block_bytes` bytes.
#[must_use]
pub const fn encode_scratch_words(block_bytes: usize) -> usize {
    compress::scratch_words(block_bytes)
}

/// Scratch words that are enough to encode at every level.
pub const ENCODE_SCRATCH_WORDS: usize = encode_scratch_words(ENCODE_BUFFER_BYTES);

/// An input buffer this large holds a whole block at every level.
pub const ENCODE_BUFFER_BYTES: usize = compress::block_limit(Level::BEST);

pub(crate) const fn stream_header(level: Level) -> u32 {
    u32::from_be_bytes([b'B', b'Z', b'h', level.digit()])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Header,
    Blocks,
    Emitting,
    End(u8),
    Finished,
}

/// A sequential bzip2 encoder that does no I/O and does not allocate.
///
/// Push uncompressed bytes in with [`Encoder::push`], pull compressed bytes out with
/// [`Encoder::pull`], and call [`Encoder::end_input`] once all input has been pushed.
///
/// The caller provides the storage: an input buffer that holds one block before it is
/// compressed ([`ENCODE_BUFFER_BYTES`] fits the largest block, a smaller one makes smaller
/// blocks), and scratch space of [`encode_scratch_words`] words for that block size
/// ([`ENCODE_SCRATCH_WORDS`] is always enough).
///
/// The inner loops run in the [`Native`] backend.
pub struct Encoder<Buffer, Scratch, B = Native> {
    buffer: Buffer,
    scratch: Scratch,
    backend: B,
    level: Level,
    builder: BlockBuilder,
    writer: BitWriter,
    code: BlockCode,
    stream_crc: u32,
    input_ended: bool,
    state: State,
}

impl<Buffer: AsMut<[u8]> + AsRef<[u8]>, Scratch: AsMut<[u32]> + AsRef<[u32]>>
    Encoder<Buffer, Scratch, Native>
{
    /// An encoder at `level` that uses `buffer` for input and `scratch` to sort blocks.
    ///
    /// # Errors
    ///
    /// [`Error::BufferTooSmall`] if the buffer holds fewer than 8 bytes, and
    /// [`Error::ScratchTooSmall`] if the scratch space is smaller than the buffer's blocks
    /// need.
    pub fn new(level: Level, buffer: Buffer, scratch: Scratch) -> Result<Self, Error> {
        Self::with_backend(level, buffer, scratch, native())
    }
}

impl<Buffer: AsMut<[u8]> + AsRef<[u8]>, Scratch: AsMut<[u32]> + AsRef<[u32]>, B: Backend>
    Encoder<Buffer, Scratch, B>
{
    /// Like [`Encoder::new`], with the inner loops run by `backend`.
    ///
    /// # Errors
    ///
    /// See [`Encoder::new`].
    pub fn with_backend(
        level: Level,
        buffer: Buffer,
        scratch: Scratch,
        backend: B,
    ) -> Result<Self, Error> {
        let block_bytes = compress::usable_block_bytes(level, buffer.as_ref().len())
            .ok_or(Error::BufferTooSmall)?;
        if scratch.as_ref().len() < encode_scratch_words(block_bytes) {
            return Err(Error::ScratchTooSmall);
        }
        Ok(Self {
            buffer,
            scratch,
            backend,
            level,
            builder: BlockBuilder::new(block_bytes),
            writer: BitWriter::new(),
            code: BlockCode::EMPTY,
            stream_crc: 0,
            input_ended: false,
            state: State::Header,
        })
    }

    /// Takes as much of `input` as fits into the current block and returns how many bytes
    /// were taken. It takes nothing while a full block waits for [`Encoder::pull`].
    pub fn push(&mut self, input: &[u8]) -> usize {
        if self.input_ended {
            return 0;
        }
        self.builder.push(input, self.buffer.as_mut(), self.backend)
    }

    /// Tells the encoder that all input has been pushed.
    pub const fn end_input(&mut self) {
        self.input_ended = true;
    }

    /// Writes compressed bytes into `out`. A block is compressed once it is full or the
    /// input has ended, so a call can take a while.
    pub fn pull(&mut self, out: &mut [u8]) -> Pulled {
        let mut written = 0;
        loop {
            written += self.writer.drain(&mut out[written..]);
            if written == out.len() && (self.writer.count() >= 8 || self.state != State::Finished) {
                return Pulled::Bytes(written);
            }
            match self.state {
                State::Header => {
                    self.writer.put(stream_header(self.level), 32);
                    self.state = State::Blocks;
                }
                State::Emitting => {
                    let (count, finished) = self.code.write(
                        self.scratch.as_ref(),
                        &mut self.writer,
                        &mut out[written..],
                    );
                    written += count;
                    if finished {
                        self.state = State::Blocks;
                    }
                }
                State::Blocks => {
                    if !self.encode_next_block() {
                        if !self.input_ended {
                            return if written > 0 {
                                Pulled::Bytes(written)
                            } else {
                                Pulled::NeedInput
                            };
                        }
                        self.state = State::End(0);
                    }
                }
                State::End(part) => {
                    match part {
                        0 => self.writer.put_48(END_MAGIC),
                        1 => self.writer.put(self.stream_crc, 32),
                        _ => self.writer.pad_to_byte(),
                    }
                    self.state = if part < 2 {
                        State::End(part + 1)
                    } else {
                        State::Finished
                    };
                }
                State::Finished => {
                    return if written > 0 {
                        Pulled::Bytes(written)
                    } else {
                        Pulled::Finished
                    };
                }
            }
        }
    }

    fn encode_next_block(&mut self) -> bool {
        let ready = self.builder.is_full()
            || (self.input_ended && !self.builder.is_empty() && {
                self.builder.flush_run(self.buffer.as_mut());
                true
            });
        if !ready || self.builder.filled() == 0 {
            return false;
        }
        let length = self.builder.take_block();
        self.code.encode(
            self.backend,
            &mut self.buffer.as_mut()[..length],
            self.scratch.as_mut(),
        );
        self.stream_crc = crc::combine(self.stream_crc, self.code.crc());
        self.state = State::Emitting;
        true
    }
}
