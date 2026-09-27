use crate::Error;
use crate::bits::BitReader;
use crate::block::{BlockOutput, END_MAGIC};
use crate::crc;
use crate::level::Level;
use crate::stream::stream_level;

const HEADER_BITS: u64 = 32;
const END_BITS: u64 = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    BeforeFirstStream,
    InStream { level: Level, crc: u32 },
    BetweenStreams,
    Finished,
}

/// Checks bzip2 data in order when its blocks are decoded out of order, for example on
/// several threads.
///
/// Find the markers with [`crate::Scanner`], then hand the checker each piece between one
/// marker and the next, in order: [`StreamChecker::start`] for the bits before the first
/// marker, [`StreamChecker::block`] for each block decoded with
/// [`crate::decode_block_into`] once all its bytes have been read, and
/// [`StreamChecker::end`] for each end-of-stream marker. Decode every block at
/// [`Level::BEST`]; the checker makes sure each fits its stream's level.
///
/// The 48-bit marker can also occur inside compressed data. Decoding a block up to such
/// a false marker returns [`Error::Truncated`]; decode it again with the next pieces
/// added, taking at least twice as many bits on each try, so any number of false
/// markers costs at most two decodes. The block ends at [`BlockOutput::end_bit`], and the
/// pieces after that are the next to check.
///
/// All bit positions count from the start of the data, so the bytes given to each call
/// must start there too.
#[derive(Debug, Clone)]
pub struct StreamChecker {
    state: State,
}

impl Default for StreamChecker {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamChecker {
    /// A checker at the start of the data.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: State::BeforeFirstStream,
        }
    }

    /// Whether the last stream has ended and bytes that are not another stream were found
    /// after it. They are ignored, as the bzip2 tool does, so there is no need to decode or
    /// check them.
    #[must_use]
    pub const fn is_finished(&self) -> bool {
        matches!(self.state, State::Finished)
    }

    /// Whether a stream is open, so the next piece is a block or an end-of-stream marker.
    /// When it is not, a block marker found next is a false marker in the bytes after the
    /// last stream, and [`StreamChecker::block`] ends the data without checking the block.
    #[must_use]
    pub const fn is_in_stream(&self) -> bool {
        matches!(self.state, State::InStream { .. })
    }

    fn start_stream(&mut self, bytes: &[u8], header_bit: u64, end_bit: u64) -> Result<(), Error> {
        let level = BitReader::new(bytes, header_bit, end_bit)
            .read(32)
            .ok()
            .and_then(stream_level);
        match (level, self.state) {
            (Some(level), _) if end_bit - header_bit == HEADER_BITS => {
                self.state = State::InStream { level, crc: 0 };
                Ok(())
            }
            (Some(_), _) => Err(Error::BadBlockMagic),
            (None, State::BeforeFirstStream) => Err(Error::NotBzip2),
            (None, _) => {
                self.state = State::Finished;
                Ok(())
            }
        }
    }

    /// Checks the bits before the first marker at `end_bit` (or all the data if there is
    /// no marker), which must be the first stream's header.
    ///
    /// # Errors
    ///
    /// [`Error::NotBzip2`] if there is no header, and [`Error::BadBlockMagic`] if
    /// no marker comes right after it.
    pub fn start(&mut self, bytes: &[u8], end_bit: u64) -> Result<(), Error> {
        self.start_stream(bytes, 0, end_bit)
    }

    /// Checks a decoded block once all its bytes have been read. `end_bit` is where the
    /// next marker (or the end of the data) is.
    ///
    /// # Errors
    ///
    /// [`Error::BadBlockMagic`] if the block does not end at `end_bit`,
    /// [`Error::BlockTooLarge`] if it does not fit its stream's level, the errors of
    /// [`BlockOutput::finish`], and [`Error::NotBzip2`] before the first stream's header.
    pub fn block(&mut self, output: &BlockOutput, end_bit: u64) -> Result<(), Error> {
        let State::InStream { level, crc } = self.state else {
            return self.outside_a_stream();
        };
        if output.end_bit() != end_bit {
            return Err(Error::BadBlockMagic);
        }
        if output.block_length() as usize > level.block_bytes() {
            return Err(Error::BlockTooLarge);
        }
        self.state = State::InStream {
            level,
            crc: crc::combine(crc, output.finish()?),
        };
        Ok(())
    }

    /// Checks the end-of-stream marker at `start_bit` and the bits after it up to
    /// `end_bit`, the next marker or the end of the data: the stream's checksum, and the
    /// next stream's header if there is one.
    ///
    /// # Errors
    ///
    /// [`Error::StreamCrcMismatch`] if the checksum does not match,
    /// [`Error::BadBlockMagic`] or [`Error::Truncated`] for damaged data, and
    /// [`Error::NotBzip2`] before the first stream's header.
    pub fn end(&mut self, bytes: &[u8], start_bit: u64, end_bit: u64) -> Result<(), Error> {
        let State::InStream { crc, .. } = self.state else {
            return self.outside_a_stream();
        };
        let mut reader = BitReader::new(bytes, start_bit, end_bit);
        if reader.read_48()? != END_MAGIC {
            return Err(Error::BadBlockMagic);
        }
        if reader.read(32)? != crc {
            return Err(Error::StreamCrcMismatch);
        }
        self.state = State::BetweenStreams;
        let next_header = (start_bit + END_BITS + 7) & !7;
        if end_bit <= next_header {
            return Ok(());
        }
        self.start_stream(bytes, next_header, end_bit)
    }

    fn outside_a_stream(&mut self) -> Result<(), Error> {
        if self.state == State::BeforeFirstStream {
            return Err(Error::NotBzip2);
        }
        self.state = State::Finished;
        Ok(())
    }

    /// Checks that the data did not stop in the middle of a stream. Call it after the
    /// last piece.
    ///
    /// # Errors
    ///
    /// [`Error::Truncated`] if a stream is not finished, and [`Error::NotBzip2`] if there
    /// was no stream.
    pub const fn finish(&self) -> Result<(), Error> {
        match self.state {
            State::InStream { .. } => Err(Error::Truncated),
            State::BeforeFirstStream => Err(Error::NotBzip2),
            State::BetweenStreams | State::Finished => Ok(()),
        }
    }
}
