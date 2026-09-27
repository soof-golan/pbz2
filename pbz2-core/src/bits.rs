use crate::Error;

pub(crate) struct BitReader<'a> {
    bytes: &'a [u8],
    window: u64,
    available: u32,
    next_byte: usize,
    position: u64,
    end: u64,
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(bytes: &'a [u8], start_bit: u64, end_bit: u64) -> Self {
        let mut reader = Self {
            bytes,
            window: 0,
            available: 0,
            next_byte: (start_bit >> 3) as usize,
            position: start_bit,
            end: end_bit,
        };
        reader.refill();
        let skipped = (start_bit & 7) as u32;
        reader.window <<= skipped;
        reader.available -= skipped;
        reader
    }

    pub(crate) const fn position(&self) -> u64 {
        self.position
    }

    #[inline(never)]
    fn refill(&mut self) {
        let chunk = self
            .bytes
            .get(self.next_byte..self.next_byte.wrapping_add(8))
            .and_then(|eight| <[u8; 8]>::try_from(eight).ok());
        if let Some(eight) = chunk {
            self.window |= u64::from_be_bytes(eight) >> self.available;
            self.next_byte += ((63 - self.available) >> 3) as usize;
            self.available |= 56;
            return;
        }
        while self.available < 56 {
            let byte = self.bytes.get(self.next_byte).copied().unwrap_or(0);
            self.window |= u64::from(byte) << (56 - self.available);
            self.next_byte += 1;
            self.available += 8;
        }
    }

    #[inline(always)]
    pub(crate) fn fill(&mut self) {
        let eight = self
            .bytes
            .get(self.next_byte..)
            .and_then(<[u8]>::first_chunk::<8>);
        if let Some(eight) = eight {
            self.window |= u64::from_be_bytes(*eight) >> self.available;
            self.next_byte += ((63 - self.available) >> 3) as usize;
            self.available |= 56;
        } else {
            self.refill();
        }
    }

    #[inline(always)]
    pub(crate) const fn peek_filled(&self, count: u32) -> u32 {
        (self.window >> (64 - count)) as u32
    }

    #[inline(always)]
    pub(crate) fn skip_filled(&mut self, count: u32) -> Result<(), Error> {
        let next = self.position + u64::from(count);
        if next > self.end {
            return Err(Error::Truncated);
        }
        self.position = next;
        self.window <<= count;
        self.available -= count;
        Ok(())
    }

    #[inline(always)]
    pub(crate) fn peek(&mut self, count: u32) -> u32 {
        if count == 0 {
            return 0;
        }
        if self.available < count {
            self.refill();
        }
        (self.window >> (64 - count)) as u32
    }

    #[inline(always)]
    pub(crate) fn skip(&mut self, count: u32) -> Result<(), Error> {
        let next = self.position + u64::from(count);
        if next > self.end {
            return Err(Error::Truncated);
        }
        if self.available < count {
            self.refill();
        }
        self.position = next;
        self.window <<= count;
        self.available -= count;
        Ok(())
    }

    #[inline]
    pub(crate) fn read(&mut self, count: u32) -> Result<u32, Error> {
        let value = self.peek(count);
        self.skip(count)?;
        Ok(value)
    }

    #[inline]
    pub(crate) fn bit(&mut self) -> Result<bool, Error> {
        Ok(self.read(1)? == 1)
    }

    pub(crate) fn read_48(&mut self) -> Result<u64, Error> {
        let high = u64::from(self.read(24)?);
        let low = u64::from(self.read(24)?);
        Ok((high << 24) | low)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct BitWriter {
    pending: u64,
    count: u32,
}

impl BitWriter {
    pub(crate) const fn new() -> Self {
        Self {
            pending: 0,
            count: 0,
        }
    }

    #[inline]
    pub(crate) const fn has_room(&self) -> bool {
        self.count <= 32
    }

    #[inline]
    pub(crate) fn put(&mut self, value: u32, bits: u32) {
        debug_assert!(self.count + bits <= 64 && (bits == 32 || value >> bits == 0));
        if bits == 0 {
            return;
        }
        self.pending = (self.pending << bits) | u64::from(value);
        self.count += bits;
    }

    pub(crate) fn put_48(&mut self, value: u64) {
        self.put((value >> 24) as u32 & 0xff_ffff, 24);
        self.put(value as u32 & 0xff_ffff, 24);
    }

    pub(crate) fn pad_to_byte(&mut self) {
        self.put(0, self.count.wrapping_neg() & 7);
    }

    pub(crate) const fn count(&self) -> u32 {
        self.count
    }

    #[inline]
    pub(crate) fn drain(&mut self, out: &mut [u8]) -> usize {
        let mut written = 0;
        while self.count >= 8 && written < out.len() {
            self.count -= 8;
            out[written] = (self.pending >> self.count) as u8;
            written += 1;
        }
        written
    }

    #[inline(always)]
    pub(crate) fn flush_word(&mut self, out: &mut [u8]) {
        self.count -= 32;
        let word = (self.pending >> self.count) as u32;
        out[..4].copy_from_slice(&word.to_be_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_reader_reads_msb_first() {
        let mut bits = BitReader::new(&[0b1010_1100, 0b0101_0011], 2, 16);
        assert_eq!(bits.read(4), Ok(0b1011));
        assert_eq!(bits.read(6), Ok(0b00_0101));
        assert_eq!(bits.read(4), Ok(0b0011));
        assert_eq!(bits.read(1), Err(Error::Truncated));
    }
}
