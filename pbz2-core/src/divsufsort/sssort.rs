use super::P;

const RADIX_LEAST: u32 = 32;
const RADIX_BUCKETS: usize = 512;
const SMALL_STACK: usize = (RADIX_LEAST / 2) as usize;
const SMALL_KEY_BYTES: usize = 3;
const KEY_SHIFT: u32 = 22;
const INDEX_MASK: i32 = (1 << KEY_SHIFT) - 1;
const WORD_BYTES: usize = 5;
const LENGTH_SHIFT: u32 = 21;
const WORD_INDEX: u64 = (1 << LENGTH_SHIFT) - 1;
const WORD_CONTINUES: u64 = WORD_BYTES as u64 + 1;

#[derive(Clone, Copy, Default)]
struct Group {
    from: u32,
    to: u32,
    depth: u32,
}

struct Sorter<'a> {
    text: &'a [u8],
    low: &'a mut [i32],
    words: &'a mut [u64],
    pa: &'a [i32],
}

fn bytes<'t>(text: &'t [u8], pa: &[i32], value: i32, next: i32, depth: usize) -> &'t [u8] {
    let start = depth + pa[value as u32 as usize] as u32 as usize;
    text.get(start..next as u32 as usize + 2)
        .unwrap_or_default()
}

#[inline(always)]
fn word(text: &[u8], pa: &[i32], value: i32, depth: u32) -> u64 {
    let at = value as u32 as usize;
    let start = depth as usize + pa[at] as u32 as usize;
    let remaining = (pa[at + 1] as u32 as usize + 2).saturating_sub(start);
    let bytes = match text.get(start..).and_then(|rest| rest.first_chunk::<8>()) {
        Some(bytes) => u64::from_be_bytes(*bytes),
        None => (0..8).fold(0, |word, index| {
            (word << 8) | u64::from(text.get(start + index).copied().unwrap_or(0))
        }),
    };
    let kept = remaining.min(WORD_BYTES);
    let bytes = bytes & !(u64::MAX >> (kept * 8));
    let length = (remaining as u64).min(WORD_CONTINUES);
    bytes | (length << LENGTH_SHIFT) | at as u64
}

impl Sorter<'_> {
    #[inline(always)]
    fn sort_small(&mut self, mut next: Group, stack: &mut [Group; SMALL_STACK]) {
        let text = self.text;
        let (low, pa) = (&mut *self.low, self.pa);
        let key = |value: i32, depth: u32| {
            let at = value as u32 as usize;
            let start = depth as usize + pa[at] as u32 as usize;
            let last = pa[at + 1] as u32 as usize + 1;
            (start..start + SMALL_KEY_BYTES).fold(0, |key, here| {
                let byte = usize::from(text.get(here).copied().unwrap_or(0));
                let part = if here <= last {
                    (byte << 1) | usize::from(here < last)
                } else {
                    0
                };
                (key << 9) | part
            })
        };
        let mut size = 0;
        loop {
            let Group { from, to, depth } = next;
            if to - from > 1 {
                let range = &mut low[from as usize..to as usize];
                let mut keys = [0usize; RADIX_LEAST as usize];
                let keys = &mut keys[..range.len()];
                for (slot, value) in keys.iter_mut().zip(range.iter()) {
                    *slot = key(*value, depth);
                }
                let (a, b, c) = (keys[0], keys[keys.len() / 2], keys[keys.len() - 1]);
                let pivot = a.max(b.min(c)).min(b.max(c));
                let mut less = 0;
                for at in 0..keys.len() {
                    let found = keys[at];
                    keys.swap(less, at);
                    range.swap(less, at);
                    less += usize::from(found < pivot);
                }
                let mut greater = less;
                for at in less..keys.len() {
                    let found = keys[at];
                    keys.swap(greater, at);
                    range.swap(greater, at);
                    greater += usize::from(found == pivot);
                }
                if pivot & 1 == 0 {
                    for value in &mut range[less + 1..greater] {
                        *value = !*value;
                    }
                } else if greater - less > 1 {
                    stack[size] = Group {
                        from: from + less as u32,
                        to: from + greater as u32,
                        depth: depth + SMALL_KEY_BYTES as u32,
                    };
                    size += 1;
                }
                if less > 1 {
                    stack[size] = Group {
                        from,
                        to: from + less as u32,
                        depth,
                    };
                    size += 1;
                }
                next.from = from + greater as u32;
                continue;
            }
            if size == 0 {
                return;
            }
            size -= 1;
            next = stack[size];
        }
    }

    fn sort_by_words(&mut self, group: Group, spare: usize) {
        let count = (group.to - group.from) as usize;
        let text = self.text;
        let pa = self.pa;
        let range = &mut self.low[group.from as usize..group.to as usize];
        let words = &mut self.words[spare..spare + count];
        for (word_slot, value) in words.iter_mut().zip(range.iter()) {
            *word_slot = word(text, pa, *value, group.depth);
        }
        super::sort_words(words);
        for (value, word) in range.iter_mut().zip(words.iter()) {
            *value = (word & WORD_INDEX) as i32;
        }
        let mut continuing = 0;
        let mut at = 0;
        while at < count {
            let head = words[at] >> LENGTH_SHIFT;
            let end = at
                + 1
                + words[at + 1..]
                    .iter()
                    .take_while(|word| *word >> LENGTH_SHIFT == head)
                    .count();
            if end - at > 1 {
                if head & 7 < WORD_CONTINUES {
                    for value in &mut range[at + 1..end] {
                        *value = !*value;
                    }
                } else {
                    words[continuing] = ((at as u64) << 32) | end as u64;
                    continuing += 1;
                }
            }
            at = end;
        }
        for index in 0..continuing {
            let run = self.words[spare + index];
            let child = Group {
                from: group.from + (run >> 32) as u32,
                to: group.from + run as u32,
                depth: group.depth + WORD_BYTES as u32,
            };
            self.sort(
                child,
                spare + continuing,
                &mut [Group::default(); SMALL_STACK],
            );
        }
    }

    fn sort(&mut self, mut group: Group, spare: usize, small: &mut [Group; SMALL_STACK]) {
        if (group.to - group.from) as usize <= self.words.len().saturating_sub(spare) {
            self.sort_by_words(group, spare);
            return;
        }
        while RADIX_LEAST <= group.to - group.from {
            let text = self.text;
            let (low, pa) = (&mut *self.low, self.pa);
            let range = &mut low[group.from as usize..group.to as usize];
            let mut ends = [0u32; RADIX_BUCKETS];
            let (mut lowest, mut highest) = (RADIX_BUCKETS - 1, 0);
            for value in range.iter_mut() {
                let at = *value as u32 as usize;
                let start = group.depth as usize + pa[at] as u32 as usize;
                let continues = start <= pa[at + 1] as u32 as usize;
                let bucket = (usize::from(text[start]) << 1) | usize::from(continues);
                *value |= (bucket as i32) << KEY_SHIFT;
                ends[bucket] += 1;
                lowest = lowest.min(bucket);
                highest = highest.max(bucket);
            }
            let buckets = lowest..=highest;
            let mut next = [0u32; RADIX_BUCKETS];
            let mut total = 0;
            for bucket in buckets.clone() {
                next[bucket] = total;
                total += ends[bucket];
                ends[bucket] = total;
            }
            for bucket in buckets.clone() {
                while next[bucket] < ends[bucket] {
                    let mut value = range[next[bucket] as usize];
                    let mut key = (value >> KEY_SHIFT) as usize % RADIX_BUCKETS;
                    while key != bucket {
                        let slot = &mut range[next[key] as usize];
                        value = core::mem::replace(slot, value & INDEX_MASK);
                        next[key] += 1;
                        key = (value >> KEY_SHIFT) as usize % RADIX_BUCKETS;
                    }
                    range[next[bucket] as usize] = value & INDEX_MASK;
                    next[bucket] += 1;
                }
            }
            let mut largest = Group::default();
            let mut start = 0;
            for bucket in buckets {
                let child = Group {
                    from: group.from + start,
                    to: group.from + ends[bucket],
                    depth: group.depth + 1,
                };
                start = ends[bucket];
                if child.to - child.from < 2 {
                    continue;
                }
                if bucket & 1 == 0 {
                    for value in &mut self.low[child.from as usize + 1..child.to as usize] {
                        *value = !*value;
                    }
                    continue;
                }
                let smaller = if largest.to - largest.from < child.to - child.from {
                    core::mem::replace(&mut largest, child)
                } else {
                    child
                };
                self.sort(smaller, spare, small);
            }
            group = largest;
            if (group.to - group.from) as usize <= self.words.len().saturating_sub(spare) {
                self.sort_by_words(group, spare);
                return;
            }
        }
        self.sort_small(group, small);
    }
}

#[allow(clippy::too_many_arguments)]
#[inline(never)]
pub(super) fn sssort(
    text: &[u8],
    sa: &mut [i32],
    pa: P,
    first: P,
    last: P,
    depth: P,
    n: P,
    last_suffix: bool,
) {
    debug_assert!(sa.len() <= INDEX_MASK as usize);
    let first = first + P::from(last_suffix);
    let m = (n - pa) as usize;
    let (low, pa) = sa.split_at_mut(pa as usize);
    let (low, free) = low.split_at_mut(m);
    let (_, words, _) = bytemuck::pod_align_to_mut::<i32, u64>(free);
    let mut sorter = Sorter {
        text,
        low,
        words,
        pa,
    };
    let group = Group {
        from: first as u32,
        to: last as u32,
        depth: depth as u32,
    };
    sorter.sort(group, 0, &mut [Group::default(); SMALL_STACK]);

    if last_suffix {
        let (low, pa) = (sorter.low, sorter.pa);
        let value = low[first as usize - 1];
        let own = bytes(text, pa, value, (n - 2) as i32, depth as usize);
        let mut at = first as usize;
        while at < last as usize && {
            let here = low[at];
            here < 0 || {
                let next = pa[here as usize + 1];
                own > bytes(text, pa, here, next, depth as usize)
            }
        } {
            low[at - 1] = low[at];
            at += 1;
        }
        low[at - 1] = value;
    }
}
