use super::P;

const RADIX_LEAST: u32 = 32;
const RADIX_BUCKETS: usize = 512;
const SMALL_STACK: usize = (RADIX_LEAST / 2) as usize;
const KEY_SHIFT: u32 = 22;
const INDEX_MASK: i32 = (1 << KEY_SHIFT) - 1;

#[derive(Clone, Copy, Default)]
struct Group {
    from: u32,
    to: u32,
    depth: u32,
}

struct Sorter<'a> {
    text: &'a [u8],
    sa: &'a mut [i32],
    pa: usize,
}

fn bytes<'t>(text: &'t [u8], pa: &[i32], value: i32, next: i32, depth: usize) -> &'t [u8] {
    let start = depth + pa[value as u32 as usize] as u32 as usize;
    text.get(start..next as u32 as usize + 2)
        .unwrap_or_default()
}

impl Sorter<'_> {
    #[inline(always)]
    fn sort_small(&mut self, mut next: Group, stack: &mut [Group; SMALL_STACK]) {
        let text = self.text;
        let (low, pa) = self.sa.split_at_mut(self.pa);
        let key = |value: i32, depth: u32| {
            let at = value as u32 as usize;
            let start = depth as usize + pa[at] as u32 as usize;
            (usize::from(text[start]) << 1) | usize::from(start <= pa[at + 1] as u32 as usize)
        };
        let mut size = 0;
        loop {
            let Group { from, to, depth } = next;
            if to - from > 1 {
                let range = &mut low[from as usize..to as usize];
                let a = key(range[0], depth);
                let b = key(range[range.len() / 2], depth);
                let c = key(range[range.len() - 1], depth);
                let pivot = a.max(b.min(c)).min(b.max(c));
                let (mut less, mut at, mut greater) = (0, 0, range.len());
                while at < greater {
                    let found = key(range[at], depth);
                    if found < pivot {
                        range.swap(less, at);
                        less += 1;
                    } else if found > pivot {
                        greater -= 1;
                        range.swap(at, greater);
                        continue;
                    }
                    at += 1;
                }
                if pivot & 1 == 0 {
                    for value in &mut range[less + 1..greater] {
                        *value = !*value;
                    }
                } else if greater - less > 1 {
                    stack[size] = Group {
                        from: from + less as u32,
                        to: from + greater as u32,
                        depth: depth + 1,
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

    fn sort(&mut self, mut group: Group, small: &mut [Group; SMALL_STACK]) {
        while RADIX_LEAST <= group.to - group.from {
            let text = self.text;
            let (low, pa) = self.sa.split_at_mut(self.pa);
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
                    for value in &mut self.sa[child.from as usize + 1..child.to as usize] {
                        *value = !*value;
                    }
                    continue;
                }
                let smaller = if largest.to - largest.from < child.to - child.from {
                    core::mem::replace(&mut largest, child)
                } else {
                    child
                };
                self.sort(smaller, small);
            }
            group = largest;
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
    let mut sorter = Sorter {
        text,
        sa,
        pa: pa as usize,
    };
    let group = Group {
        from: first as u32,
        to: last as u32,
        depth: depth as u32,
    };
    sorter.sort(group, &mut [Group::default(); SMALL_STACK]);

    if last_suffix {
        let (low, pa) = sorter.sa.split_at_mut(sorter.pa);
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
