mod sssort;
mod trsort;

use sssort::sssort;
use trsort::trsort;

type P = isize;

const ALPHABET: usize = 256;
const BUCKET_A_WORDS: usize = ALPHABET;
pub(crate) const BUCKET_WORDS: usize = BUCKET_A_WORDS + ALPHABET * ALPHABET;

#[inline(always)]
const fn ilg(n: P) -> i32 {
    debug_assert!(n >= 0);
    (P::BITS - 1) as i32 - n.leading_zeros() as i32
}

const ROWS: [u32; ALPHABET] = {
    let mut rows = [0; ALPHABET];
    let mut c0 = 0;
    while c0 < ALPHABET {
        rows[c0] = (c0 * (2 * ALPHABET - 1 - c0)) as u32;
        c0 += 1;
    }
    rows
};

#[inline(always)]
const fn row(c0: usize) -> usize {
    ROWS[c0 & (ALPHABET - 1)] as usize
}

#[inline(always)]
const fn b_index(c0: usize, c1: usize) -> usize {
    row(c0) + c1
}

#[inline(always)]
const fn b_star_index(c0: usize, c1: usize) -> usize {
    row(c0) + ALPHABET - 1 - c0 + c1
}

pub(crate) const fn star_words(length: usize) -> usize {
    length.div_ceil(32)
}

pub(crate) const fn work_words(length: usize) -> usize {
    BUCKET_WORDS + star_words(length)
}

#[inline(always)]
fn order_pair(first: &mut u64, second: &mut u64) {
    let (low, high) = ((*first).min(*second), (*first).max(*second));
    *first = low;
    *second = high;
}

#[inline(always)]
fn sort_words(words: &mut [u64]) {
    match words {
        [first, second] => order_pair(first, second),
        [first, second, third] => {
            order_pair(first, second);
            order_pair(second, third);
            order_pair(first, second);
        }
        _ => words.sort_unstable(),
    }
}

const SHORT_MOVE: usize = 32;

#[inline(always)]
fn move_up(sa: &mut [i32], from: usize, to: usize, count: usize) {
    if count > SHORT_MOVE {
        sa.copy_within(from..from + count, to);
        return;
    }
    for index in (0..count).rev() {
        sa[to + index] = sa[from + index];
    }
}

const HIGH_BITS: u64 = 0x8080_8080_8080_8080;
const GATHER_HIGH_BITS: u64 = 0x0102_0408_1020_4080;

#[inline(always)]
const fn high_bits(word: u64) -> u64 {
    ((word >> 7) & (HIGH_BITS >> 7)).wrapping_mul(GATHER_HIGH_BITS) >> 56
}

#[inline(always)]
fn comparisons(text: &[u8], base: usize) -> (u64, u64) {
    let (mut less, mut equal) = (0u64, 0u64);
    if let Some(bytes) = text[base..].first_chunk::<65>() {
        for eighth in 0..8 {
            let at = eighth * 8;
            let here = u64::from_le_bytes(*bytes[at..].first_chunk::<8>().expect("8 bytes"));
            let after = u64::from_le_bytes(*bytes[at + 1..].first_chunk::<8>().expect("8 bytes"));
            let differ = here ^ after;
            let same = !(((differ & !HIGH_BITS) + !HIGH_BITS) | differ) & HIGH_BITS;
            let low_not_less = (here | HIGH_BITS) - (after & !HIGH_BITS);
            let below = (!here & after) | (!differ & !low_not_less);
            less |= high_bits(below & HIGH_BITS) << at;
            equal |= high_bits(same) << at;
        }
    } else {
        for (bit, pair) in text[base..].windows(2).enumerate() {
            less |= u64::from(pair[0] < pair[1]) << bit;
            equal |= u64::from(pair[0] == pair[1]) << bit;
        }
    }
    (less, equal)
}

#[inline(always)]
fn types(text: &[u8], base: usize, above_is_b: u64) -> (u64, u64) {
    let (less, equal) = comparisons(text, base);
    let starts = less.reverse_bits();
    let spans = starts | equal.reverse_bits();
    let reached = starts | (spans & !spans.wrapping_add(starts).wrapping_add(above_is_b));
    let is_b = reached.reverse_bits();
    let star = is_b & !((is_b >> 1) | (above_is_b << 63));
    (is_b, star)
}

struct Buckets<'a> {
    a: &'a mut [i32; ALPHABET],
    b: &'a mut [i32; ALPHABET * ALPHABET],
    stars: &'a mut [u32],
}

#[inline(never)]
fn sort_type_b_star(text: &[u8], sa: &mut [i32], buckets: &mut Buckets<'_>) -> P {
    let sa = &mut sa[..=text.len()];
    let n = text.len() as P;
    let at = |index: P| usize::from(text[index as usize]);
    buckets.a.fill(0);
    buckets.b.fill(0);

    let mut counts = [[0i32; ALPHABET]; 4];
    let (quads, rest) = text.as_chunks::<4>();
    for quad in quads {
        for (lane, byte) in quad.iter().enumerate() {
            counts[lane][usize::from(*byte)] += 1;
        }
    }
    for byte in rest {
        counts[0][usize::from(*byte)] += 1;
    }
    for (c0, count) in buckets.a.iter_mut().enumerate() {
        *count = counts.iter().map(|lane| lane[c0]).sum();
    }
    let mut m = text.len();
    let mut above_is_b = 0;
    for (word, stars) in buckets.stars.chunks_mut(2).enumerate().rev() {
        let base = word * 64;
        let (is_b, star) = types(text, base, above_is_b);
        above_is_b = is_b & 1;
        let mut bits = is_b;
        while bits != 0 {
            let bit = bits.trailing_zeros() as usize;
            bits &= bits - 1;
            let here = usize::from(text[base + bit]);
            let after = usize::from(text[base + bit + 1]);
            let index = if (star >> bit) & 1 != 0 {
                b_star_index(here, after)
            } else {
                b_index(here, after)
            };
            buckets.b[index] += 1;
        }
        let mut bits = star;
        while bits != 0 {
            let bit = 63 - bits.leading_zeros() as usize;
            bits ^= 1 << bit;
            m -= 1;
            sa[m] = (base + bit) as i32;
        }
        for (half, stars) in stars.iter_mut().enumerate() {
            *stars = (star >> (half * 32)) as u32;
        }
    }
    let m = n - m as P;

    let mut i: P = 0;
    let mut j: P = 0;
    for c0 in 0..ALPHABET {
        let count = buckets.a[c0] as P;
        buckets.a[c0] = (i + j) as i32;
        let row_start = j;
        let stars = &mut buckets.b[b_star_index(c0, c0 + 1)..=b_star_index(c0, ALPHABET - 1)];
        for star in stars {
            j += *star as P;
            *star = j as i32;
        }
        i += count - (j - row_start);
    }

    if 0 < m {
        let pa = n - m;
        let isa = m;
        let mut i = m - 2;
        while 0 <= i {
            let t = sa[(pa + i) as usize] as P;
            let slot = &mut buckets.b[b_star_index(at(t), at(t + 1))];
            *slot -= 1;
            sa[*slot as usize] = i as i32;
            i -= 1;
        }
        let t = sa[(pa + m - 1) as usize] as P;
        let slot = &mut buckets.b[b_star_index(at(t), at(t + 1))];
        *slot -= 1;
        sa[*slot as usize] = (m - 1) as i32;

        let mut j = m;
        let mut c0 = ALPHABET as P - 2;
        while 0 < j && 0 <= c0 {
            let mut c1 = ALPHABET as P - 1;
            while c0 < c1 {
                let i = buckets.b[b_star_index(c0 as usize, c1 as usize)] as P;
                if 1 < j - i {
                    let last_suffix = sa[i as usize] == (m - 1) as i32;
                    sssort(text, sa, pa, i, j, 2, n, last_suffix);
                }
                j = i;
                c1 -= 1;
            }
            c0 -= 1;
        }

        let mut i = m - 1;
        while 0 <= i {
            if 0 <= sa[i as usize] {
                let j = i;
                loop {
                    sa[(isa + sa[i as usize] as P) as usize] = i as i32;
                    i -= 1;
                    if !(0 <= i && 0 <= sa[i as usize]) {
                        break;
                    }
                }
                sa[(i + 1) as usize] = (i - j) as i32;
                if i <= 0 {
                    break;
                }
            }
            let j = i as i32;
            loop {
                let flipped = !sa[i as usize];
                sa[i as usize] = flipped;
                sa[(isa + flipped as P) as usize] = j;
                i -= 1;
                if sa[i as usize] >= 0 {
                    break;
                }
            }
            sa[(isa + sa[i as usize] as P) as usize] = j;
            i -= 1;
        }

        trsort(sa, isa, m, 1);

        let mut j = m;
        for (at_word, word) in buckets.stars[..star_words(n as usize)]
            .iter()
            .enumerate()
            .rev()
        {
            let mut bits = *word;
            while bits != 0 {
                let bit = 31 - bits.leading_zeros();
                bits &= !(1 << bit);
                let t = (at_word * 32) as P + P::from(bit as u8);
                j -= 1;
                let rank = sa[(isa + j) as usize] as usize;
                let descends = (t > 0) & (at(t.max(1) - 1) > at(t));
                sa[rank] = (t as i32) ^ -i32::from(descends);
            }
        }

        buckets.b[b_index(ALPHABET - 1, ALPHABET - 1)] = n as i32;
        let mut k = m - 1;
        for c0 in (0..ALPHABET - 1).rev() {
            let mut i = buckets.a[c0 + 1] as P - 1;
            for c1 in (c0 + 1..ALPHABET).rev() {
                let t = i - buckets.b[b_index(c0, c1)] as P;
                buckets.b[b_index(c0, c1)] = i as i32;
                i = t;
                let j = buckets.b[b_star_index(c0, c1)] as P;
                if j <= k {
                    debug_assert!(i >= k);
                    let count = k - j + 1;
                    move_up(sa, j as usize, (i - count + 1) as usize, count as usize);
                    i -= count;
                    k = j - 1;
                }
            }
            buckets.b[b_star_index(c0, c0 + 1)] = (i - buckets.b[b_index(c0, c0)] as P + 1) as i32;
            buckets.b[b_index(c0, c0)] = i as i32;
        }
    }
    m
}

#[inline(never)]
fn construct_bwt(text: &[u8], sa: &mut [i32], buckets: &mut Buckets<'_>, m: P, target: P) -> usize {
    let sa = &mut sa[..=text.len()];
    let n = text.len() as P;
    let at = |index: P| usize::from(text[index as usize]);
    let mut target_row: P = 0;

    if 0 < m {
        for c1 in (0..ALPHABET - 1).rev() {
            let i = buckets.b[b_star_index(c1, c1 + 1)] as P;
            let mut j = buckets.a[c1 + 1] as P - 1;
            while i <= j {
                let s = sa[j as usize] as P;
                if 0 < s {
                    if s == target {
                        target_row = j;
                    }
                    let s = s - 1;
                    let c0 = at(s);
                    sa[j as usize] = !(c0 as i32);
                    let descends = (0 < s) & (at(s.max(1) - 1) > c0);
                    let placed = s ^ -P::from(descends);
                    let slot = &mut buckets.b[b_index(c0, c1)];
                    let k = *slot;
                    sa[k as usize] = placed as i32;
                    *slot = k - 1;
                } else if s != 0 {
                    sa[j as usize] = !(s as i32);
                }
                j -= 1;
            }
        }
    }

    let last = at(n - 1);
    let k = buckets.a[last];
    if n - 1 == target {
        target_row = k as P;
    }
    sa[k as usize] = if at(n - 2) < last {
        !(at(n - 2) as i32)
    } else {
        (n - 1) as i32
    };
    buckets.a[last] = k + 1;

    let mut word = 0u32;
    for i in 0..n {
        let s = sa[i as usize] as P;
        let byte = if 0 < s {
            if s == target {
                target_row = i;
            }
            let s = s - 1;
            let c0 = at(s);
            let before = at(s.max(1) - 1);
            let keep = -i32::from((0 < s) & (before < c0));
            let placed = (!(before as i32) & keep) | (s as i32 & !keep);
            let slot = &mut buckets.a[c0];
            let k = *slot;
            if s == target {
                target_row = k as P;
            }
            sa[k as usize] = placed;
            *slot = k + 1;
            c0 as u8
        } else if s != 0 {
            !s as u8
        } else {
            if target == 0 {
                target_row = i;
            }
            text[(n - 1) as usize]
        };
        word |= u32::from(byte) << ((i & 3) << 3);
        if i & 3 == 3 {
            sa[(i >> 2) as usize] = i32::from_ne_bytes(word.to_le_bytes());
            word = 0;
        }
    }
    if n & 3 != 0 {
        sa[(n >> 2) as usize] = i32::from_ne_bytes(word.to_le_bytes());
    }
    target_row as usize
}

pub(crate) fn burrows_wheeler(
    text: &[u8],
    sa: &mut [u32],
    work: &mut [u32],
    target: usize,
) -> usize {
    let n = text.len();
    if n < 2 {
        sa[0] = u32::from_ne_bytes([text.first().copied().unwrap_or(0), 0, 0, 0]);
        return 0;
    }
    let sa: &mut [i32] = bytemuck::cast_slice_mut(&mut sa[..=n]);
    let (counts, stars) = work.split_at_mut(BUCKET_WORDS);
    let (a, b) = bytemuck::cast_slice_mut::<u32, i32>(counts)
        .split_first_chunk_mut::<ALPHABET>()
        .expect("the work space holds the buckets");
    let b = b
        .first_chunk_mut::<{ ALPHABET * ALPHABET }>()
        .expect("the work space holds the buckets");
    let mut buckets = Buckets {
        a,
        b,
        stars: &mut stars[..star_words(n)],
    };
    let m = sort_type_b_star(text, sa, &mut buckets);
    construct_bwt(text, sa, &mut buckets, m, target as P)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    fn by_sorting(text: &[u8]) -> Vec<usize> {
        let mut order: Vec<usize> = (0..text.len()).collect();
        order.sort_by(|a, b| text[*a..].cmp(&text[*b..]));
        order
    }

    fn texts() -> Vec<Vec<u8>> {
        let mut texts = vec![
            b"banana".to_vec(),
            b"mississippi".to_vec(),
            b"abracadabra".to_vec(),
            b"aaaaaaaaaaaaaaaaaaaaaaaa".to_vec(),
            b"abababababababababababab".to_vec(),
            b"zyxwvutsrqponmlkjihgfedcba".to_vec(),
            b"abcdefghijklmnopqrstuvwxyz".to_vec(),
            b"x".to_vec(),
            b"ba".to_vec(),
            b"ab".to_vec(),
            b"abcabcabcabcabcabcabcabcabcabcabcabcabd".repeat(300),
            b"the quick brown fox jumps over the lazy dog. ".repeat(900),
        ];
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for length in [2, 3, 17, 100, 1000, 5000, 70_000] {
            for alphabet in [2u64, 3, 4, 256] {
                texts.push((0..length).map(|_| (next() % alphabet) as u8).collect());
            }
        }
        for period in [1, 7, 100, 3000] {
            let unit: Vec<u8> = (0..period).map(|_| (next() % 4) as u8).collect();
            let mut text = unit.repeat(40_000 / period + 1);
            let at = (next() % text.len() as u64) as usize;
            text[at] ^= 1;
            texts.push(text);
        }
        let fibonacci = (0..22).fold((b"a".to_vec(), b"b".to_vec()), |(a, b), _| {
            let next = [b.as_slice(), a.as_slice()].concat();
            (b, next)
        });
        texts.push(fibonacci.1);
        texts
    }

    #[test]
    fn sort_words_sorts_every_zero_one_input() {
        for length in 2..=3 {
            for bits in 0u32..1 << length {
                let mut words: Vec<u64> = (0..length).map(|at| u64::from(bits >> at & 1)).collect();
                let ones = bits.count_ones() as usize;
                sort_words(&mut words);
                let expected: Vec<u64> = (0..length)
                    .map(|at| u64::from(at >= length - ones))
                    .collect();
                assert_eq!(words, expected, "length {length}, bits {bits:b}");
            }
        }
    }

    #[test]
    fn column_and_rows_match_sorting() {
        for text in texts() {
            let n = text.len();
            let sorted = by_sorting(&text);
            let expected: Vec<u8> = sorted.iter().map(|at| text[(at + n - 1) % n]).collect();
            let mut row_of = vec![0; n];
            for (row, at) in sorted.iter().enumerate() {
                row_of[*at] = row;
            }
            let step = (n / 64).max(1);
            for target in (0..n).step_by(step).chain([n - 1]) {
                let mut sa = vec![0u32; n + 1];
                let mut buckets = vec![0u32; work_words(n)];
                let row = burrows_wheeler(&text, &mut sa, &mut buckets, target);
                let column = &bytemuck::cast_slice::<u32, u8>(&sa)[..n];
                assert!(column == expected, "column of a text of {n} bytes");
                assert_eq!(row, row_of[target], "row of suffix {target} of {n} bytes");
            }
        }
    }
}
