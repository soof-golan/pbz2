use crate::Error;
use crate::block::BLOCK_SIZE_STEP;

/// A bzip2 block size, 1 to 9 hundred thousand bytes, which is also the compression
/// level: larger blocks compress better. A `Level` is always 1 to 9.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Level(u8);

impl Level {
    /// Level 1: the smallest blocks, the least memory, and the fastest compression.
    pub const FASTEST: Self = Self(1);
    /// Level 9: the largest blocks and the best compression, which bzip2 uses by default.
    pub const BEST: Self = Self(9);

    /// The level `level`, if it is 1 to 9.
    #[must_use]
    pub const fn new(level: u8) -> Option<Self> {
        if level >= 1 && level <= 9 {
            Some(Self(level))
        } else {
            None
        }
    }

    /// The level as a number, 1 to 9.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// The most bytes a block at this level holds before its last run-length step.
    #[must_use]
    pub const fn block_bytes(self) -> usize {
        self.0 as usize * BLOCK_SIZE_STEP
    }

    pub(crate) const fn from_digit(digit: u8) -> Option<Self> {
        match digit {
            b'1'..=b'9' => Some(Self(digit - b'0')),
            _ => None,
        }
    }

    pub(crate) const fn digit(self) -> u8 {
        b'0' + self.0
    }
}

impl Default for Level {
    fn default() -> Self {
        Self::BEST
    }
}

impl TryFrom<u8> for Level {
    type Error = Error;

    fn try_from(level: u8) -> Result<Self, Error> {
        Self::new(level).ok_or(Error::BadLevel)
    }
}
