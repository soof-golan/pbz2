use crate::backend::{Backend, Recent};
use crate::compress::{equal_prefix, find_byte};
use crate::divsufsort::{burrows_wheeler, work_words};
use crate::huffman::MAX_ALPHABET;

const RUN_A: u16 = 0;
const RUN_B: u16 = 1;

pub(crate) const fn scratch_words(block_bytes: usize) -> usize {
    block_bytes + 1 + work_words(block_bytes)
}

pub(crate) const fn symbols_at(block_bytes: usize) -> usize {
    block_bytes.div_ceil(4)
}

pub(crate) fn symbols_in(scratch: &[u32], at: usize, count: usize) -> &[u16] {
    &bytemuck::cast_slice(&scratch[at..])[..count]
}

pub(crate) struct Symbols {
    pub(crate) origin: u32,
    pub(crate) count: usize,
    pub(crate) alphabet: usize,
    pub(crate) used: [bool; 256],
    pub(crate) frequencies: [u32; MAX_ALPHABET],
}

const COMPARISON_BUDGET: usize = 4;

struct Rotation {
    start: usize,
    repeats_itself: bool,
}

fn first_difference(left: &[u8], right: &[u8]) -> usize {
    let (left_words, left_rest) = left.as_chunks::<8>();
    let (right_words, _) = right.as_chunks::<8>();
    for (index, (a, b)) in left_words.iter().zip(right_words).enumerate() {
        let difference = u64::from_le_bytes(*a) ^ u64::from_le_bytes(*b);
        if difference != 0 {
            return (index << 3) + (difference.trailing_zeros() >> 3) as usize;
        }
    }
    let done = left_words.len() * 8;
    done + left_rest
        .iter()
        .zip(&right[done..])
        .take_while(|(a, b)| a == b)
        .count()
}

fn common_length(text: &[u8], mut first: usize, mut second: usize) -> usize {
    let n = text.len();
    let mut count = 0;
    while count < n {
        if first >= n {
            first -= n;
        }
        if second >= n {
            second -= n;
        }
        let span = (n - first).min(n - second).min(n - count);
        let same = first_difference(&text[first..first + span], &text[second..second + span]);
        count += same;
        if same < span {
            break;
        }
        first += span;
        second += span;
    }
    count
}

fn least_rotation_among_runs(text: &[u8]) -> Option<Rotation> {
    let n = text.len();
    if text.is_empty() {
        return None;
    }
    let smallest = text.iter().copied().fold(u8::MAX, u8::min);
    let Some(anchor) = text.iter().position(|byte| *byte != smallest) else {
        return Some(Rotation {
            start: 0,
            repeats_itself: n > 1,
        });
    };
    let wrap = |index: usize| if index < n { index } else { index - n };
    let at = |index: usize| text[wrap(index)];
    let segment = |from: usize, end: usize| {
        if from < n {
            &text[from..end.min(n)]
        } else {
            &text[from - n..end - n]
        }
    };
    let mut budget = COMPARISON_BUDGET * n;
    let (mut best, mut repeats_itself) = (0, false);
    let mut longest = 0;
    let end = anchor + n;
    let mut from = anchor + 1;
    while from < end {
        let mut found = from + find_byte(segment(from, end), smallest);
        if found == n && n < end {
            found += find_byte(segment(n, end), smallest);
        }
        if found >= end {
            break;
        }
        let mut run = equal_prefix(segment(found, end), smallest);
        if found + run == n && n < end {
            run += equal_prefix(segment(n, end), smallest);
        }
        from = found + run;
        if run >= longest {
            let start = wrap(found);
            if run > longest {
                longest = run;
                best = start;
                repeats_itself = false;
            } else {
                let word = |at: usize| {
                    text.get(at + run..at + run + 8)
                        .and_then(|bytes| bytes.first_chunk::<8>())
                        .map(|bytes| u64::from_be_bytes(*bytes))
                };
                if let (Some(here), Some(there)) = (word(start), word(best))
                    && here != there
                {
                    if here < there {
                        best = start;
                        repeats_itself = false;
                    }
                    continue;
                }
                let common = common_length(text, start, best);
                budget = budget.checked_sub(common + 1)?;
                if common == n {
                    repeats_itself = true;
                } else if at(start + common) < at(best + common) {
                    best = start;
                    repeats_itself = false;
                }
            }
        }
    }
    Some(Rotation {
        start: best,
        repeats_itself,
    })
}

fn least_rotation(text: &[u8]) -> Rotation {
    if let Some(rotation) = least_rotation_among_runs(text) {
        return rotation;
    }
    let n = text.len();
    let at = |index: usize| text[if index < n { index } else { index - n }];
    let (mut first, mut second, mut matched) = (0, 1, 0);
    while first < n && second < n && matched < n {
        let a = at(first + matched);
        let b = at(second + matched);
        if a == b {
            matched += 1;
            continue;
        }
        if a > b {
            first += matched + 1;
        } else {
            second += matched + 1;
        }
        if first == second {
            second += 1;
        }
        matched = 0;
    }
    Rotation {
        start: first.min(second),
        repeats_itself: matched == n,
    }
}

fn period(text: &[u8]) -> usize {
    let n = text.len();
    let (mut matched, mut next) = (0, 1);
    while next < n && text[matched] <= text[next] {
        matched = if text[matched] < text[next] {
            0
        } else {
            matched + 1
        };
        next += 1;
    }
    let period = next - matched;
    if n.is_multiple_of(period) && text[..n - period] == text[period..] {
        period
    } else {
        n
    }
}

struct Output<'a> {
    symbols: &'a mut [u16],
    count: usize,
}

fn frequencies(symbols: &[u16]) -> [u32; MAX_ALPHABET] {
    let mut counts = [[0u32; MAX_ALPHABET]; 4];
    let (chunks, rest) = symbols.as_chunks::<4>();
    for chunk in chunks {
        for (lane, symbol) in chunk.iter().enumerate() {
            counts[lane][*symbol as usize] += 1;
        }
    }
    for symbol in rest {
        counts[0][*symbol as usize] += 1;
    }
    core::array::from_fn(|symbol| counts.iter().map(|lane| lane[symbol]).sum())
}

impl Output<'_> {
    #[inline]
    fn push(&mut self, symbol: u16) {
        self.symbols[self.count] = symbol;
        self.count += 1;
    }

    fn push_zeros(&mut self, zeros: usize) {
        if zeros == 0 {
            return;
        }
        let mut run = zeros - 1;
        loop {
            self.push(if run & 1 == 1 { RUN_B } else { RUN_A });
            if run < 2 {
                break;
            }
            run = (run - 2) >> 1;
        }
    }
}

fn move_to_front<B: Backend>(
    backend: B,
    last: &[u8],
    rows: usize,
    repeats: usize,
    index_of: &[u8; 256],
    output: &mut Output<'_>,
) {
    backend.run(
        #[inline(always)]
        || {
            let in_register = cfg!(target_arch = "aarch64") || backend.front_in_register();
            let mut recent = Recent::new();
            let mut list: [u8; 256] = core::array::from_fn(|index| index as u8);
            let mut zeros = 0;
            let last = &last[..rows];
            let mut row = 0;
            while row < rows {
                let byte = last[row];
                let run = 1 + equal_prefix(&last[row + 1..], byte);
                row += run;
                let index = index_of[usize::from(byte)];
                let first = if in_register { recent.first() } else { list[0] };
                if first == index {
                    zeros += run * repeats;
                    continue;
                }
                output.push_zeros(zeros);
                let position = if in_register {
                    recent.take(backend, index)
                } else {
                    backend.move_to_front(&mut list, index)
                };
                output.push(position as u16 + 1);
                zeros = run * repeats - 1;
            }
            output.push_zeros(zeros);
        },
    );
}

pub(crate) fn symbols<B: Backend>(backend: B, block: &mut [u8], scratch: &mut [u32]) -> Symbols {
    let n = block.len();
    let rotation = least_rotation(block);
    let start = rotation.start;
    block.rotate_left(start);
    let period = if rotation.repeats_itself {
        period(block)
    } else {
        n
    };
    let repeats = n / period;
    let target = if start == 0 { 0 } else { n - start } % period;
    let (sa, buckets) = scratch.split_at_mut(n + 1);
    let origin = burrows_wheeler(&block[..period], sa, buckets, target) * repeats;
    let at = symbols_at(n);
    let (last, rest) = sa.split_at_mut(at);
    let mut used = [false; 256];
    for byte in &block[..period] {
        used[usize::from(*byte)] = true;
    }
    let mut index_of = [0u8; 256];
    let mut used_count = 0usize;
    for (byte, _) in used.iter().enumerate().filter(|(_, used)| **used) {
        index_of[byte] = used_count as u8;
        used_count += 1;
    }
    let mut output = Output {
        symbols: bytemuck::cast_slice_mut(rest),
        count: 0,
    };
    let last: &[u8] = bytemuck::cast_slice(last);
    move_to_front(backend, last, period, repeats, &index_of, &mut output);
    output.push(used_count as u16 + 1);
    let count = output.count;

    Symbols {
        origin: origin as u32,
        count,
        alphabet: used_count + 2,
        used,
        frequencies: frequencies(&output.symbols[..count]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    fn rotations_sorted(text: &[u8]) -> Vec<Vec<u8>> {
        let mut rows: Vec<Vec<u8>> = (0..text.len())
            .map(|start| [&text[start..], &text[..start]].concat())
            .collect();
        rows.sort();
        rows
    }

    fn move_to_front(last: &[u8]) -> Vec<u32> {
        let used: Vec<u8> = (0..=255u8).filter(|byte| last.contains(byte)).collect();
        let mut recent = used.clone();
        let mut out = Vec::new();
        let mut zeros = 0usize;
        let flush = |zeros: &mut usize, out: &mut Vec<u32>| {
            let mut run = *zeros;
            while run > 0 {
                out.push(if run % 2 == 1 { 0 } else { 1 });
                run = (run - 1) / 2;
            }
            *zeros = 0;
        };
        for byte in last {
            let position = recent.iter().position(|recent| recent == byte).unwrap_or(0);
            if position == 0 {
                zeros += 1;
                continue;
            }
            flush(&mut zeros, &mut out);
            let moved = recent.remove(position);
            recent.insert(0, moved);
            out.push(position as u32 + 1);
        }
        flush(&mut zeros, &mut out);
        out.push(used.len() as u32 + 1);
        out
    }

    fn texts() -> Vec<Vec<u8>> {
        let mut texts = vec![
            b"banana".to_vec(),
            b"abab".to_vec(),
            b"abcabcabcabc".to_vec(),
            b"bcabcabca".to_vec(),
            b"aaaaaaaa".to_vec(),
            b"x".to_vec(),
            b"hello world".to_vec(),
            b"mississippi".to_vec(),
            b"aabaaacaa".to_vec(),
            [
                &b"a".repeat(20)[..],
                b"b",
                &b"a".repeat(5),
                b"c",
                &b"a".repeat(7),
            ]
            .concat(),
            [
                &b"a".repeat(9)[..],
                b"zq",
                &b"a".repeat(3),
                b"x",
                &b"a".repeat(9),
                b"y",
            ]
            .concat(),
        ];
        let mut state = 0x2545_f491_4f6c_dd1du64;
        for length in [2, 3, 10, 100, 2000] {
            for alphabet in [2u64, 3, 256] {
                let text: Vec<u8> = (0..length)
                    .map(|_| {
                        state ^= state << 13;
                        state ^= state >> 7;
                        state ^= state << 17;
                        (state % alphabet) as u8
                    })
                    .collect();
                texts.push(text.repeat(3));
                texts.push(text);
            }
        }
        texts
    }

    #[test]
    fn symbols_match_sorted_rotations() {
        assert_symbols_match_sorted_rotations(crate::Scalar);
    }

    #[cfg(all(feature = "simd", target_arch = "aarch64"))]
    #[test]
    fn vectorized_symbols_match_sorted_rotations() {
        let neon = fearless_simd::Level::new().as_neon();
        assert!(neon.is_some(), "every aarch64 core has NEON");
        if let Some(neon) = neon {
            assert_symbols_match_sorted_rotations(crate::Vectorized(neon));
        }
    }

    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    #[test]
    fn vectorized_symbols_match_sorted_rotations() {
        if let Some(sse) = fearless_simd::Level::new().as_sse4_2() {
            assert_symbols_match_sorted_rotations(crate::Vectorized(sse));
        }
    }

    fn assert_symbols_match_sorted_rotations<B: Backend>(backend: B) {
        for text in texts() {
            let rows = rotations_sorted(&text);
            let last: Vec<u8> = rows.iter().map(|row| row[text.len() - 1]).collect();
            let mut block = text.clone();
            let mut scratch = vec![0; scratch_words(text.len())];
            let symbols = symbols(backend, &mut block, &mut scratch);
            assert_eq!(
                rows[symbols.origin as usize], text,
                "origin row for {text:?}"
            );
            let found: Vec<u32> = symbols_in(&scratch, symbols_at(text.len()), symbols.count)
                .iter()
                .map(|symbol| u32::from(*symbol))
                .collect();
            assert_eq!(found, move_to_front(&last), "symbols for {text:?}");
        }
    }
}
