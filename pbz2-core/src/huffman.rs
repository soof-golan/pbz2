use crate::Error;
use crate::bits::BitReader;

pub(crate) const MAX_ALPHABET: usize = 258;
pub(crate) const MAX_CODE_LENGTH: u8 = 20;
const LIMIT_SLOTS: usize = 23;
const FAST_BITS: u32 = 11;

const FAST_ENTRIES: usize = 1 << FAST_BITS;

pub(crate) struct Table {
    fast: [u16; FAST_ENTRIES],
    codes: Codes,
}

struct Codes {
    limit: [i32; LIMIT_SLOTS],
    base: [i32; LIMIT_SLOTS],
    permutation: [u16; MAX_ALPHABET],
    symbols: usize,
    shortest: usize,
}

impl Table {
    pub(crate) fn new(lengths: &[u8]) -> Self {
        let shortest = usize::from(lengths.iter().copied().min().unwrap_or(1));
        let longest = usize::from(lengths.iter().copied().max().unwrap_or(1));

        let mut base = [0i32; LIMIT_SLOTS];
        for length in lengths {
            base[usize::from(*length) + 1] += 1;
        }
        for slot in 1..LIMIT_SLOTS {
            base[slot] += base[slot - 1];
        }

        let mut next = base.map(|start| start as usize);
        let mut permutation = [0u16; MAX_ALPHABET];
        for (symbol, length) in lengths.iter().enumerate() {
            let slot = &mut next[usize::from(*length)];
            permutation[*slot] = symbol as u16;
            *slot += 1;
        }

        let mut limit = [0i32; LIMIT_SLOTS];
        let mut code = 0i32;
        for length in shortest..=longest {
            code += base[length + 1] - base[length];
            limit[length] = code - 1;
            code <<= 1;
        }
        for length in shortest + 1..=longest {
            base[length] = ((limit[length - 1] + 1) << 1) - base[length];
        }

        let codes = Codes {
            limit,
            base,
            permutation,
            symbols: lengths.len(),
            shortest,
        };
        let mut fast = [0; FAST_ENTRIES];
        if !fill_fast(
            &mut fast,
            lengths,
            &codes.permutation[..codes.symbols],
            shortest,
        ) {
            for (prefix, entry) in fast.iter_mut().enumerate() {
                *entry = codes.fast_entry(prefix as u32);
            }
        }
        Self { fast, codes }
    }

    #[inline(always)]
    pub(crate) fn decode(&self, bits: &mut BitReader<'_>) -> Result<u16, Error> {
        let entry = self.fast[bits.peek_filled(FAST_BITS) as usize & (FAST_ENTRIES - 1)];
        if entry != 0 {
            bits.skip_filled(u32::from(entry & 0xf));
            return Ok(entry >> 4);
        }
        self.codes.decode_long(bits)
    }
}

impl Codes {
    fn symbol_at(&self, code: i32, length: usize) -> Option<u16> {
        let index = usize::try_from(code - self.base[length]).ok()?;
        self.permutation[..self.symbols].get(index).copied()
    }

    fn fast_entry(&self, prefix: u32) -> u16 {
        let mut length = self.shortest;
        while length as u32 <= FAST_BITS {
            let code = (prefix >> (FAST_BITS - length as u32)) as i32;
            if code <= self.limit[length] {
                return self
                    .symbol_at(code, length)
                    .map_or(0, |symbol| (symbol << 4) | length as u16);
            }
            length += 1;
        }
        0
    }

    #[cold]
    #[inline(never)]
    fn decode_long(&self, bits: &mut BitReader<'_>) -> Result<u16, Error> {
        let mut length = self.shortest;
        let mut code = bits.read(length as u32)? as i32;
        loop {
            if length > usize::from(MAX_CODE_LENGTH) {
                return Err(Error::BadHuffmanCode);
            }
            if code <= self.limit[length] {
                break;
            }
            length += 1;
            code = (code << 1) | bits.read(1)? as i32;
        }
        self.symbol_at(code, length).ok_or(Error::BadHuffmanCode)
    }
}

fn fill_fast(fast: &mut [u16], lengths: &[u8], permutation: &[u16], shortest: usize) -> bool {
    let mut code = 0u32;
    let mut current = shortest as u32;
    for &symbol in permutation {
        let length = u32::from(lengths[usize::from(symbol)]);
        code <<= length - current;
        current = length;
        if code >= 1 << length {
            return false;
        }
        if length <= FAST_BITS {
            let spare = FAST_BITS - length;
            let first = (code << spare) as usize;
            fast[first..first + (1 << spare)].fill((symbol << 4) | length as u16);
        }
        code += 1;
    }
    true
}

pub(crate) const MAX_ENCODE_LENGTH: usize = 17;
const LIST_SLOTS: usize = 2 * MAX_ALPHABET;
const MARK_WORDS: usize = (LIST_SLOTS + 63) >> 6;

pub(crate) fn code_lengths(frequencies: &[u32], lengths: &mut [u8]) {
    let n = frequencies.len();
    let mut order = [0u16; MAX_ALPHABET];
    for (slot, symbol) in order.iter_mut().zip(0u16..) {
        *slot = symbol;
    }
    let order = &mut order[..n];
    order.sort_unstable_by_key(|symbol| (frequencies[usize::from(*symbol)].max(1), *symbol));
    let mut leaves = [0u64; MAX_ALPHABET];
    for (leaf, symbol) in leaves.iter_mut().zip(order.iter()) {
        *leaf = u64::from(frequencies[usize::from(*symbol)].max(1));
    }
    let leaves = &leaves[..n];
    let mut per_length = [0u16; MAX_ENCODE_LENGTH + 1];
    if !huffman_length_counts(leaves, &mut per_length) {
        package_merge_counts(leaves, &mut per_length);
    }
    let mut symbols = order.iter();
    for (length, count) in per_length.iter().enumerate().rev() {
        for symbol in symbols.by_ref().take(usize::from(*count)) {
            lengths[usize::from(*symbol)] = length as u8;
        }
    }
}

fn huffman_length_counts(leaves: &[u64], per_length: &mut [u16; MAX_ENCODE_LENGTH + 1]) -> bool {
    let n = leaves.len();
    let mut weight = [0u64; LIST_SLOTS];
    let mut parent = [0u16; LIST_SLOTS];
    let (mut leaf, mut node) = (0, n);
    for next in n..2 * n - 1 {
        let mut pick = || {
            if leaf < n && (node == next || leaves[leaf] <= weight[node]) {
                leaf += 1;
                (leaf - 1, leaves[leaf - 1])
            } else {
                node += 1;
                (node - 1, weight[node - 1])
            }
        };
        let (a, first) = pick();
        let (b, second) = pick();
        weight[next] = first + second;
        parent[a] = next as u16;
        parent[b] = next as u16;
    }
    let mut depth = [0u8; LIST_SLOTS];
    for index in (0..2 * n - 2).rev() {
        depth[index] = depth[usize::from(parent[index])] + 1;
        if usize::from(depth[index]) > MAX_ENCODE_LENGTH {
            return false;
        }
    }
    for depth in &depth[..n] {
        per_length[usize::from(*depth)] += 1;
    }
    true
}

fn package_merge_counts(leaves: &[u64], per_length: &mut [u16; MAX_ENCODE_LENGTH + 1]) {
    let n = leaves.len();
    let mut lists = [[0u64; LIST_SLOTS]; 2];
    lists[0][..n].copy_from_slice(leaves);
    let mut current_length = n;
    let mut leaf_marks = [[0u64; MARK_WORDS]; MAX_ENCODE_LENGTH];
    for (level, marks) in leaf_marks[1..].iter_mut().enumerate() {
        let packages = current_length >> 1;
        let (low, high) = lists.split_at_mut(1);
        let (current, merged) = if level & 1 == 0 {
            (&low[0], &mut high[0])
        } else {
            (&high[0], &mut low[0])
        };
        let (mut leaf, mut package) = (0, 0);
        for (slot, weight) in merged[..n + packages].iter_mut().enumerate() {
            let package_weight =
                (package < packages).then(|| current[2 * package] + current[2 * package + 1]);
            match package_weight {
                Some(package_weight) if leaf == n || package_weight < leaves[leaf] => {
                    *weight = package_weight;
                    package += 1;
                }
                _ => {
                    *weight = leaves[leaf];
                    marks[slot >> 6] |= 1 << (slot & 63);
                    leaf += 1;
                }
            }
        }
        current_length = n + packages;
    }

    let mut at_least = [0usize; MAX_ENCODE_LENGTH + 1];
    let mut take = 2 * n - 2;
    for (length, marks) in leaf_marks[1..].iter().rev().enumerate() {
        let (whole, part) = (take >> 6, take & 63);
        let leaves_taken = marks[..whole]
            .iter()
            .map(|word| word.count_ones() as usize)
            .sum::<usize>()
            + marks
                .get(whole)
                .map_or(0, |word| (word & ((1 << part) - 1)).count_ones() as usize);
        at_least[length + 1] = leaves_taken;
        take = 2 * (take - leaves_taken);
    }
    at_least[MAX_ENCODE_LENGTH] = take;
    for length in 1..=MAX_ENCODE_LENGTH {
        per_length[length] =
            (at_least[length] - at_least.get(length + 1).copied().unwrap_or(0)) as u16;
    }
}

pub(crate) fn canonical_codes(lengths: &[u8], codes: &mut [u32]) {
    let mut code = 0u32;
    for length in 1..=MAX_CODE_LENGTH {
        for (symbol, _) in lengths
            .iter()
            .enumerate()
            .filter(|(_, candidate)| **candidate == length)
        {
            codes[symbol] = code;
            code += 1;
        }
        code <<= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lengths_for(frequencies: &[u32]) -> std::vec::Vec<u8> {
        let mut lengths = std::vec![0u8; frequencies.len()];
        code_lengths(frequencies, &mut lengths);
        lengths
    }

    fn kraft_sum_in_units_of_the_longest(lengths: &[u8]) -> u64 {
        lengths
            .iter()
            .map(|length| 1u64 << (MAX_ENCODE_LENGTH - usize::from(*length)))
            .sum()
    }

    #[test]
    fn code_lengths_are_optimal_below_limit() {
        assert_eq!(
            lengths_for(&[1, 1, 2, 3, 5, 8, 13, 21]),
            [7, 7, 6, 5, 4, 3, 2, 1]
        );
        assert_eq!(lengths_for(&[10, 10, 10, 10]), [2, 2, 2, 2]);
        assert_eq!(lengths_for(&[0, 0, 5]), [2, 2, 1]);
    }

    #[test]
    fn huffman_counts_match_package_merge_when_short_enough() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut compared = 0;
        for case in 0..20_000 {
            let n = 2 + (next() % 257) as usize;
            let spread = [4, 64, 1 << 12, 1 << 20][case % 4];
            let mut leaves: std::vec::Vec<u64> = (0..n).map(|_| 1 + next() % spread).collect();
            if case % 3 == 0 {
                for leaf in leaves.iter_mut().step_by(2) {
                    *leaf = 1 + next() % 3;
                }
            }
            leaves.sort_unstable();
            let mut fast = [0u16; MAX_ENCODE_LENGTH + 1];
            let mut merged = [0u16; MAX_ENCODE_LENGTH + 1];
            if huffman_length_counts(&leaves, &mut fast) {
                package_merge_counts(&leaves, &mut merged);
                assert_eq!(fast, merged, "leaves {leaves:?}");
                compared += 1;
            }
        }
        assert!(compared > 10_000);
    }

    #[test]
    fn code_lengths_are_limited_and_complete() {
        let mut fibonacci = [0u32; 40];
        fibonacci[0] = 1;
        fibonacci[1] = 1;
        for index in 2..fibonacci.len() {
            fibonacci[index] = fibonacci[index - 1] + fibonacci[index - 2];
        }
        let lengths = lengths_for(&fibonacci);
        assert!(lengths.iter().all(|length| (1..=17).contains(length)));
        assert_eq!(kraft_sum_in_units_of_the_longest(&lengths), 1 << 17);
        let mut frequencies = [0u32; 258];
        frequencies[0] = 900_000;
        let lengths = lengths_for(&frequencies);
        assert!(lengths.iter().all(|length| (1..=17).contains(length)));
        assert_eq!(kraft_sum_in_units_of_the_longest(&lengths), 1 << 17);
    }

    #[test]
    fn canonical_codes_decode_to_their_symbols() {
        let lengths = [3u8, 2, 3, 2, 2];
        let mut codes = [0u32; 5];
        canonical_codes(&lengths, &mut codes);
        assert_eq!(codes, [0b110, 0b00, 0b111, 0b01, 0b10]);
        let table = Table::new(&lengths);
        let mut bits = BitReader::new(&[0b1100_0111, 0b0110_0000], 0, 12);
        let symbols: [u16; 5] = core::array::from_fn(|_| table.decode(&mut bits).unwrap_or(99));
        assert_eq!(symbols, [0, 1, 2, 3, 4]);
    }

    #[test]
    fn table_decodes_shortest_codes_first() {
        let table = Table::new(&[2, 1, 3, 3]);
        let mut bits = BitReader::new(&[0b0101_1011, 0b1000_0000], 0, 9);
        let symbols: [u16; 4] = core::array::from_fn(|_| table.decode(&mut bits).unwrap_or(99));
        assert_eq!(symbols, [1, 0, 2, 3]);
        assert_eq!(bits.check_end(), Ok(()));
        let _ = table.decode(&mut bits);
        assert_eq!(bits.check_end(), Err(Error::Truncated));
    }

    #[test]
    fn overfull_table_matches_libbzip2() {
        let table = Table::new(&[1, 1, 1]);
        let mut bits = BitReader::new(&[0b0110_0000], 0, 3);
        let symbols: [u16; 3] = core::array::from_fn(|_| table.decode(&mut bits).unwrap_or(99));
        assert_eq!(symbols, [0, 1, 1]);
    }

    #[test]
    fn long_code_uses_slow_path() {
        let mut lengths = [12u8; 4];
        lengths[0] = 1;
        lengths[1] = 2;
        let table = Table::new(&lengths);
        let mut bits = BitReader::new(&[0b1100_0000, 0b0000_1111], 0, 16);
        assert_eq!(table.decode(&mut bits), Ok(2));
    }
}
