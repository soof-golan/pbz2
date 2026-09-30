const POLYNOMIAL: u32 = 0x04c1_1db7;
const SLICES: usize = 16;

const fn tables() -> [[u32; 256]; SLICES] {
    let mut tables = [[0u32; 256]; SLICES];
    let mut index = 0;
    while index < 256 {
        let mut crc = (index as u32) << 24;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ POLYNOMIAL
            } else {
                crc << 1
            };
            bit += 1;
        }
        tables[0][index] = crc;
        index += 1;
    }
    let mut slice = 1;
    while slice < SLICES {
        let mut index = 0;
        while index < 256 {
            let previous = tables[slice - 1][index];
            tables[slice][index] = (previous << 8) ^ tables[0][(previous >> 24) as usize];
            index += 1;
        }
        slice += 1;
    }
    tables
}

static TABLES: [[u32; 256]; SLICES] = tables();

pub(crate) const START: u32 = 0xffff_ffff;

#[inline]
pub(crate) fn update(crc: u32, byte: u8) -> u32 {
    (crc << 8) ^ TABLES[0][usize::from((crc >> 24) as u8 ^ byte)]
}

#[inline(always)]
fn update_chunk<const N: usize>(crc: u32, chunk: &[u8; N]) -> u32 {
    let high = crc ^ u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    let mut crc = TABLES[N - 1][(high >> 24) as usize]
        ^ TABLES[N - 2][(high >> 16) as usize & 0xff]
        ^ TABLES[N - 3][(high >> 8) as usize & 0xff]
        ^ TABLES[N - 4][high as usize & 0xff];
    for (slice, byte) in chunk[4..].iter().enumerate() {
        crc ^= TABLES[N - 5 - slice][usize::from(*byte)];
    }
    crc
}

pub(crate) fn update_slice(crc: u32, bytes: &[u8]) -> u32 {
    #[cfg(any(
        all(feature = "simd", target_arch = "aarch64", target_feature = "aes"),
        all(feature = "simd", target_arch = "x86_64", target_feature = "pclmulqdq")
    ))]
    let (crc, bytes) = folded::update(crc, bytes);
    update_by_table(crc, bytes)
}

fn update_by_table(mut crc: u32, bytes: &[u8]) -> u32 {
    let (chunks, rest) = bytes.as_chunks::<SLICES>();
    for chunk in chunks {
        crc = update_chunk(crc, chunk);
    }
    let (chunks, rest) = rest.as_chunks::<8>();
    for chunk in chunks {
        crc = update_chunk(crc, chunk);
    }
    rest.iter().fold(crc, |crc, byte| update(crc, *byte))
}

#[cfg(any(
    all(feature = "simd", target_arch = "aarch64", target_feature = "aes"),
    all(feature = "simd", target_arch = "x86_64", target_feature = "pclmulqdq")
))]
mod folded {
    use super::{POLYNOMIAL, update_by_table};
    #[cfg(target_arch = "x86_64")]
    use core::arch::x86_64::{_mm_clmulepi64_si128, _mm_set_epi64x};

    const FOLD_BYTES: usize = 64;
    const LANE_BYTES: usize = 16;
    const LANES: usize = FOLD_BYTES >> LANE_BYTES.trailing_zeros();

    const fn x_to_the(power: u32) -> u64 {
        let mut remainder = 1u32;
        let mut step = 0;
        while step < power {
            remainder = if remainder & 0x8000_0000 != 0 {
                (remainder << 1) ^ POLYNOMIAL
            } else {
                remainder << 1
            };
            step += 1;
        }
        remainder as u64
    }

    const fn by_bytes(bytes: u32) -> (u64, u64) {
        (x_to_the(8 * bytes + 64), x_to_the(8 * bytes))
    }

    const BY_FOLD: (u64, u64) = by_bytes(FOLD_BYTES as u32);
    const BY_LANE: [(u64, u64); LANES - 1] = [
        by_bytes(3 * LANE_BYTES as u32),
        by_bytes(2 * LANE_BYTES as u32),
        by_bytes(LANE_BYTES as u32),
    ];

    #[cfg(target_arch = "aarch64")]
    #[allow(unsafe_code)]
    #[inline(always)]
    fn carryless(a: u64, b: u64) -> u128 {
        unsafe { core::arch::aarch64::vmull_p64(a, b) }
    }

    #[cfg(target_arch = "x86_64")]
    #[allow(unsafe_code)]
    #[inline(always)]
    fn carryless(a: u64, b: u64) -> u128 {
        unsafe {
            let product =
                _mm_clmulepi64_si128(_mm_set_epi64x(0, a as i64), _mm_set_epi64x(0, b as i64), 0);
            core::mem::transmute::<_, u128>(product)
        }
    }

    #[inline(always)]
    fn shifted(value: u128, (high, low): (u64, u64)) -> u128 {
        carryless((value >> 64) as u64, high) ^ carryless(value as u64, low)
    }

    fn lanes(block: &[u8; FOLD_BYTES]) -> [u128; LANES] {
        let (lanes, _) = block.as_chunks::<LANE_BYTES>();
        core::array::from_fn(|lane| u128::from_be_bytes(lanes[lane]))
    }

    pub(super) fn update(crc: u32, bytes: &[u8]) -> (u32, &[u8]) {
        let (blocks, rest) = bytes.as_chunks::<FOLD_BYTES>();
        let Some((first, blocks)) = blocks.split_first() else {
            return (crc, bytes);
        };
        let mut sums = lanes(first);
        sums[0] ^= u128::from(crc) << 96;
        for block in blocks {
            for (sum, lane) in sums.iter_mut().zip(lanes(block)) {
                *sum = shifted(*sum, BY_FOLD) ^ lane;
            }
        }
        let folded = sums
            .iter()
            .zip(BY_LANE)
            .fold(sums[LANES - 1], |folded, (sum, by)| {
                folded ^ shifted(*sum, by)
            });
        (update_by_table(0, &folded.to_be_bytes()), rest)
    }
}

/// Folds one block's CRC into the running CRC of its stream, the way bzip2 does.
#[inline]
#[must_use]
pub fn combine(stream_crc: u32, block_crc: u32) -> u32 {
    stream_crc.rotate_left(1) ^ block_crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_matches_known_crc() {
        let crc = b"123456789"
            .iter()
            .fold(START, |crc, byte| update(crc, *byte));
        assert_eq!(!crc, 0xfc89_1918);
        assert_eq!(!update_slice(START, b"123456789"), 0xfc89_1918);
    }

    #[test]
    fn update_slice_matches_update_for_every_length() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let bytes: std::vec::Vec<u8> = (0..1000)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 24) as u8
            })
            .collect();
        for length in 0..bytes.len() {
            for start in [START, 0, 0x1234_5678] {
                let expected = bytes[..length]
                    .iter()
                    .fold(start, |crc, byte| update(crc, *byte));
                assert_eq!(update_slice(start, &bytes[..length]), expected, "{length}");
            }
        }
    }

    #[test]
    fn update_slice_matches_sample3_crc() {
        let original = include_bytes!("../../testdata/sample3.ref");
        let packed = include_bytes!("../../testdata/sample3.bz2");
        let stored = u32::from_be_bytes([packed[10], packed[11], packed[12], packed[13]]);
        assert_eq!(!update_slice(START, original), stored);
    }
}
