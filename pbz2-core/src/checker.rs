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

/// Checks, in order, the pieces between markers of data whose blocks were decoded out of
/// order at [`Level::BEST`]. Bit positions count from the start of the data.
///
/// A block cut short by a false marker decodes as [`Error::Truncated`]; decode it again
/// with the next pieces added, and go on from its [`BlockOutput::end_bit`].
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

    /// Whether ignored bytes followed the last stream, so the rest need no checking.
    #[must_use]
    pub const fn is_finished(&self) -> bool {
        matches!(self.state, State::Finished)
    }

    /// Whether a stream is open. If not, the next block marker is in ignored bytes.
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

    /// Checks the stream header before the first marker, at `end_bit`.
    ///
    /// # Errors
    ///
    /// [`Error::NotBzip2`] or [`Error::BadBlockMagic`].
    pub fn start(&mut self, bytes: &[u8], end_bit: u64) -> Result<(), Error> {
        self.start_stream(bytes, 0, end_bit)
    }

    /// Checks a block whose bytes have all been read and whose piece ends at `end_bit`.
    ///
    /// # Errors
    ///
    /// Damaged data, a block too large for its stream, or [`Error::NotBzip2`].
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

    /// Checks the end-of-stream piece from `start_bit` to `end_bit`, and the next stream's
    /// header if there is one.
    ///
    /// # Errors
    ///
    /// Damaged data, [`Error::StreamCrcMismatch`], or [`Error::NotBzip2`].
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

    /// Checks that the data did not stop inside a stream.
    ///
    /// # Errors
    ///
    /// [`Error::Truncated`] or [`Error::NotBzip2`].
    pub const fn finish(&self) -> Result<(), Error> {
        match self.state {
            State::InStream { .. } => Err(Error::Truncated),
            State::BeforeFirstStream => Err(Error::NotBzip2),
            State::BetweenStreams | State::Finished => Ok(()),
        }
    }
}
