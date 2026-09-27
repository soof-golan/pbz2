/// The code that runs the inner loops, picked at compile time.
///
/// [`Scalar`] works everywhere. With the `simd` feature, [`Vectorized`] runs them with the
/// SIMD instructions of a `fearless_simd` token. Output is the same with every backend.
pub trait Backend: Copy {
    /// Runs `work` with this backend's instructions enabled.
    fn run<R>(self, work: impl FnOnce() -> R) -> R;

    /// Moves `index` to the front of `recent` and returns where it was.
    fn move_to_front(self, recent: &mut [u8; 256], index: u8) -> usize;

    /// Moves the value at `position` to the front of `recent` and returns it.
    fn move_position_to_front(self, recent: &mut [u8; 256], position: usize) -> u8;

    /// Whether decoding is faster with the front of the move-to-front list kept in a
    /// register than with [`Backend::move_position_to_front`] for every position.
    fn front_in_register(self) -> bool {
        true
    }

    /// Returns a mask whose bit `i` is set when `bytes[i]` equals the byte before it;
    /// the byte before `bytes[0]` is `previous`.
    fn equal_to_previous(self, bytes: &[u8; 16], previous: u8) -> u16 {
        let (low, high) = bytes.split_at(8);
        let half = |word: &[u8], before: u8| {
            let word = u64::from_le_bytes(word.try_into().unwrap_or([0; 8]));
            let difference = word ^ ((word << 8) | u64::from(before));
            let low_bits = 0x7f7f_7f7f_7f7f_7f7f;
            let equal = !((((difference & low_bits) + low_bits) | difference) | low_bits);
            ((equal >> 7).wrapping_mul(0x0102_0408_1020_4080) >> 56) as u16
        };
        half(low, previous) | (half(high, bytes[7]) << 8)
    }
}

type Front = u128;
const FRONT_BYTES: usize = 16;
const LANE_ONES: Front = Front::MAX / 0xff;
const LANE_LOW: Front = LANE_ONES * 0x7f;

#[inline(always)]
const fn lanes_through(position: u32) -> Front {
    if position as usize >= FRONT_BYTES - 1 {
        Front::MAX
    } else {
        (1 << (8 * (position + 1))) - 1
    }
}

#[inline(always)]
const fn moved_to_front(front: Front, position: u32, value: u8) -> Front {
    let moved = lanes_through(position);
    (((front << 8) | value as Front) & moved) | (front & !moved)
}

pub(crate) struct Recent {
    front: Front,
    list: [u8; 256],
}

impl Recent {
    pub(crate) fn new() -> Self {
        let list: [u8; 256] = core::array::from_fn(|index| index as u8);
        Self {
            front: Front::from_le_bytes(core::array::from_fn(|index| index as u8)),
            list,
        }
    }

    fn front_from_list(&mut self) {
        let mut bytes = [0u8; FRONT_BYTES];
        bytes.copy_from_slice(&self.list[..FRONT_BYTES]);
        self.front = Front::from_le_bytes(bytes);
    }

    #[inline(always)]
    pub(crate) const fn first(&self) -> u8 {
        self.front as u8
    }

    #[inline(always)]
    pub(crate) fn take<B: Backend>(&mut self, backend: B, index: u8) -> usize {
        let difference = self.front ^ (Front::from(index) * LANE_ONES);
        let found = !((((difference & LANE_LOW) + LANE_LOW) | difference) | LANE_LOW);
        if found != 0 {
            let position = found.trailing_zeros() >> 3;
            self.front = moved_to_front(self.front, position, index);
            return position as usize;
        }
        self.list[..FRONT_BYTES].copy_from_slice(&self.front.to_le_bytes());
        let position = backend.move_to_front(&mut self.list, index);
        self.front_from_list();
        position
    }

    #[inline(always)]
    pub(crate) const fn first_of(&self, in_register: bool) -> u8 {
        if in_register {
            self.front as u8
        } else {
            self.list[0]
        }
    }

    #[inline(always)]
    pub(crate) fn take_position<B: Backend>(
        &mut self,
        backend: B,
        position: usize,
        in_register: bool,
    ) -> u8 {
        if !in_register {
            return backend.move_position_to_front(&mut self.list, position);
        }
        if position < FRONT_BYTES {
            let value = (self.front >> (8 * position)) as u8;
            self.front = moved_to_front(self.front, position as u32, value);
            return value;
        }
        self.list[..FRONT_BYTES].copy_from_slice(&self.front.to_le_bytes());
        let value = backend.move_position_to_front(&mut self.list, position);
        self.front_from_list();
        value
    }
}

/// Plain Rust code, for every target.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Scalar;

impl Backend for Scalar {
    #[inline(always)]
    fn run<R>(self, work: impl FnOnce() -> R) -> R {
        work()
    }

    #[inline(always)]
    fn move_to_front(self, recent: &mut [u8; 256], index: u8) -> usize {
        let mut carried = recent[0];
        let mut position = 0;
        while carried != index {
            position += 1;
            core::mem::swap(&mut carried, &mut recent[position]);
        }
        recent[0] = index;
        position
    }

    #[inline(always)]
    fn move_position_to_front(self, recent: &mut [u8; 256], position: usize) -> u8 {
        let value = recent[position];
        let mut carried = recent[0];
        for slot in &mut recent[1..=position] {
            core::mem::swap(&mut carried, slot);
        }
        recent[0] = value;
        value
    }
}

/// SIMD code for the instruction set of the `fearless_simd` token it holds, such as
/// `Vectorized(fearless_simd::Level::new().as_neon()?)`.
#[cfg(feature = "simd")]
#[derive(Debug, Clone, Copy)]
pub struct Vectorized<S>(pub S);

#[cfg(feature = "simd")]
impl<S: fearless_simd::Simd> Backend for Vectorized<S> {
    #[inline(always)]
    fn run<R>(self, work: impl FnOnce() -> R) -> R {
        self.0.vectorize(work)
    }

    #[inline(always)]
    fn front_in_register(self) -> bool {
        false
    }

    #[inline(always)]
    fn equal_to_previous(self, bytes: &[u8; 16], previous: u8) -> u16 {
        use fearless_simd::{SimdBase, SimdMask, u8x16};
        let simd = self.0;
        let values = u8x16::from_slice(simd, bytes);
        let before = u8x16::splat(simd, previous).slide::<15>(values);
        values.simd_eq(before).to_bitmask() as u16
    }

    #[inline(always)]
    fn move_to_front(self, recent: &mut [u8; 256], index: u8) -> usize {
        use fearless_simd::{Select, SimdBase, SimdMask, mask8x16, u8x16};
        let simd = self.0;
        let wanted = u8x16::splat(simd, index);
        let mut carry = wanted;
        for (chunk_index, chunk) in recent.as_chunks_mut::<16>().0.iter_mut().enumerate() {
            let values = u8x16::from_slice(simd, chunk);
            let found = values.simd_eq(wanted).to_bitmask();
            let shifted = carry.slide::<15>(values);
            if found == 0 {
                shifted.store_slice(chunk);
                carry = values;
                continue;
            }
            let lane = found.trailing_zeros();
            let moved = mask8x16::from_bitmask(simd, (2u64 << lane) - 1);
            moved.select(shifted, values).store_slice(chunk);
            return chunk_index * 16 + lane as usize;
        }
        0
    }

    #[inline(always)]
    fn move_position_to_front(self, recent: &mut [u8; 256], position: usize) -> u8 {
        use fearless_simd::{Select, SimdBase, SimdMask, mask8x16, u8x16};
        let simd = self.0;
        let value = recent[position];
        let mut carry = u8x16::splat(simd, value);
        let last_chunk = position >> 4;
        for chunk in recent.as_chunks_mut::<16>().0.iter_mut().take(last_chunk) {
            let values = u8x16::from_slice(simd, chunk);
            carry.slide::<15>(values).store_slice(chunk);
            carry = values;
        }
        let chunk = &mut recent[last_chunk * 16..last_chunk * 16 + 16];
        let values = u8x16::from_slice(simd, chunk);
        let moved = mask8x16::from_bitmask(simd, (2u64 << (position & 15)) - 1);
        moved
            .select(carry.slide::<15>(values), values)
            .store_slice(chunk);
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    fn picks() -> Vec<usize> {
        let mut state = 0x6a09_e667_f3bc_c908u64;
        let mut picks: Vec<usize> = (0..256).chain((0..256).rev()).collect();
        picks.extend((0..5000).map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % 256) as usize
        }));
        picks
    }

    fn assert_matches_list<B: Backend>(backend: B) {
        let mut list: Vec<u8> = (0..=255).collect();
        let mut recent: [u8; 256] = core::array::from_fn(|index| index as u8);
        for pick in picks() {
            let value = list.remove(pick);
            list.insert(0, value);
            assert_eq!(backend.move_position_to_front(&mut recent, pick), value);
            assert_eq!(recent.as_slice(), list.as_slice());
            let wanted = list[pick];
            let position = list.iter().position(|byte| *byte == wanted).unwrap_or(0);
            list.remove(position);
            list.insert(0, wanted);
            assert_eq!(backend.move_to_front(&mut recent, wanted), position);
            assert_eq!(recent.as_slice(), list.as_slice());
        }
    }

    fn assert_equal_to_previous_matches_bytes<B: Backend>(backend: B) {
        let picks = picks();
        for (at, window) in picks.windows(17).enumerate() {
            let bytes: [u8; 16] = core::array::from_fn(|index| (window[index + 1] % 3) as u8);
            let previous = (window[0] % 3) as u8;
            let expected = (0..16).fold(0u16, |mask, index| {
                let before = if index == 0 {
                    previous
                } else {
                    bytes[index - 1]
                };
                mask | (u16::from(bytes[index] == before) << index)
            });
            assert_eq!(
                backend.equal_to_previous(&bytes, previous),
                expected,
                "window {at}"
            );
        }
    }

    fn assert_recent_matches_list<B: Backend>(backend: B, in_register: bool) {
        let mut list: Vec<u8> = (0..=255).collect();
        let mut recent = Recent::new();
        for pick in picks() {
            let value = list.remove(pick);
            list.insert(0, value);
            assert_eq!(recent.take_position(backend, pick, in_register), value);
            assert_eq!(recent.first_of(in_register), list[0]);
            if !in_register {
                continue;
            }
            let wanted = list[pick];
            let position = list.iter().position(|byte| *byte == wanted).unwrap_or(0);
            list.remove(position);
            list.insert(0, wanted);
            assert_eq!(recent.take(backend, wanted), position);
            assert_eq!(recent.first(), list[0]);
        }
    }

    #[test]
    fn scalar_move_to_front_matches_list() {
        assert_matches_list(Scalar);
    }

    #[test]
    fn scalar_equal_to_previous_matches_bytes() {
        assert_equal_to_previous_matches_bytes(Scalar);
    }

    #[test]
    fn recent_matches_list() {
        assert_recent_matches_list(Scalar, true);
        assert_recent_matches_list(Scalar, false);
    }

    #[cfg(all(feature = "simd", target_arch = "aarch64"))]
    #[test]
    fn vectorized_move_to_front_matches_list() {
        let neon = fearless_simd::Level::new().as_neon();
        assert!(neon.is_some(), "every aarch64 core has NEON");
        if let Some(neon) = neon {
            assert_matches_list(Vectorized(neon));
            assert_equal_to_previous_matches_bytes(Vectorized(neon));
        }
    }

    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    #[test]
    fn vectorized_move_to_front_matches_list() {
        if let Some(sse) = fearless_simd::Level::new().as_sse4_2() {
            assert_matches_list(Vectorized(sse));
            assert_equal_to_previous_matches_bytes(Vectorized(sse));
        }
    }
}
