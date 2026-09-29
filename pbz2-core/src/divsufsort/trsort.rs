use super::{P, ilg};

const INSERTION_LIMIT: P = 8;
const SMALL_PARTITION: P = 256;
const STACK: usize = 64;
const RADIX_FROM: usize = 256;
const MOST_DIGIT_BITS: u32 = 11;

fn sort_by_rank_digits(words: &mut [u64], spare: &mut [u64], lowest: u32, highest: u32) {
    let span_bits = u32::BITS - (highest - lowest).leading_zeros();
    let passes = span_bits.div_ceil(MOST_DIGIT_BITS);
    if passes == 0 {
        return;
    }
    let digit_bits = span_bits.div_ceil(passes);
    let buckets = 1usize << digit_bits;
    let mask = buckets as u32 - 1;
    let mut counts = [0u32; 1 << MOST_DIGIT_BITS];
    let counts = &mut counts[..buckets];
    let (mut from, mut to) = (&mut *words, &mut *spare);
    for pass in 0..passes {
        let shift = pass * digit_bits;
        let digit = |word: u64| ((((word >> 32) as u32 - lowest) >> shift) & mask) as usize;
        counts.fill(0);
        for word in from.iter() {
            counts[digit(*word)] += 1;
        }
        let mut total = 0;
        for count in counts.iter_mut() {
            let here = *count;
            *count = total;
            total += here;
        }
        for word in from.iter() {
            let slot = &mut counts[digit(*word)];
            to[*slot as usize] = *word;
            *slot += 1;
        }
        core::mem::swap(&mut from, &mut to);
    }
    if passes % 2 == 1 {
        words.copy_from_slice(spare);
    }
}

struct Budget {
    chance: P,
    remain: P,
    step: P,
    count: P,
}

impl Budget {
    const fn check(&mut self, size: P) -> bool {
        if size <= self.remain {
            self.remain -= size;
            return true;
        }
        if self.chance == 0 {
            self.count += size;
            return false;
        }
        self.remain += self.step - size;
        self.chance -= 1;
        true
    }
}

struct Ranks<'a> {
    sa: &'a mut [i32],
    isa: P,
    words: &'a mut [u64],
}

impl Ranks<'_> {
    fn sort_by_keys(&mut self, isad: P, first: P, last: P, budget: &mut Budget) -> bool {
        let count = (last - first) as usize;
        let Some((words, spare)) = self.words.split_at_mut_checked(count) else {
            return false;
        };
        let own = (last - 1) as u32;
        let (low, ranks) = self.sa.split_at_mut(self.isa as usize);
        let range = &mut low[first as usize..last as usize];
        let keys = &ranks[(isad - self.isa) as usize..];
        let (mut lowest, mut highest) = (u32::MAX, 0);
        for (word, value) in words.iter_mut().zip(range.iter()) {
            let key = keys[*value as u32 as usize] as u32;
            if key == own {
                return false;
            }
            lowest = lowest.min(key);
            highest = highest.max(key);
            *word = (u64::from(key) << 32) | u64::from(*value as u32);
        }
        match spare.get_mut(..count) {
            Some(spare) if count >= RADIX_FROM => {
                sort_by_rank_digits(words, spare, lowest, highest)
            }
            _ => super::sort_words(words),
        }
        let mut groups = 0;
        let mut at = 0;
        while at < count {
            let key = words[at] >> 32;
            let end = at
                + 1
                + words[at + 1..]
                    .iter()
                    .take_while(|word| *word >> 32 == key)
                    .count();
            let rank = (first + end as P - 1) as i32;
            for (value, word) in range[at..end].iter_mut().zip(&words[at..end]) {
                *value = *word as u32 as i32;
                ranks[*word as u32 as usize] = rank;
            }
            if end - at > 1 {
                let tandem = u64::from(key == u64::from(rank as u32)) << 63;
                words[groups] = tandem | ((at as u64) << 32) | end as u64;
                groups += 1;
            }
            at = end;
        }
        let step = isad - self.isa;
        for group in 0..groups {
            let run = self.words[group];
            let from = first + ((run >> 32) as u32 & (u32::MAX >> 1)) as P;
            let to = first + (run as u32) as P;
            let limit = if run >> 63 == 1 { -1 } else { ilg(to - from) };
            if budget.check(to - from) {
                self.introsort(isad + step, from, to, step, limit, budget);
            }
        }
        true
    }

    #[inline(always)]
    fn get(&self, at: P) -> i32 {
        self.sa[at as usize]
    }

    #[inline(always)]
    fn set(&mut self, at: P, value: i32) {
        self.sa[at as usize] = value;
    }

    #[inline(always)]
    fn swap(&mut self, a: P, b: P) {
        self.sa.swap(a as usize, b as usize);
    }

    #[inline(always)]
    fn rank_at(&self, isad: P, value: i32) -> i32 {
        self.sa[isad as usize + value as u32 as usize]
    }

    #[inline(always)]
    fn set_rank(&mut self, value: i32, rank: i32) {
        self.sa[self.isa as usize + value as u32 as usize] = rank;
    }

    #[inline(always)]
    fn key(&self, isad: P, at: P) -> i32 {
        self.rank_at(isad, self.get(at))
    }

    fn insertion_sort(&mut self, isad: P, first: P, last: P) {
        let mut a = first + 1;
        while a < last {
            let value = self.get(a);
            let mut b = a - 1;
            let mut r;
            loop {
                r = self.rank_at(isad, value) - self.key(isad, b);
                if r >= 0 {
                    break;
                }
                loop {
                    self.set(b + 1, self.get(b));
                    b -= 1;
                    if !(first <= b && self.get(b) < 0) {
                        break;
                    }
                }
                if b < first {
                    break;
                }
            }
            if r == 0 {
                self.set(b, !self.get(b));
            }
            self.set(b + 1, value);
            a += 1;
        }
    }

    fn sort_by_rank(&mut self, isad: P, first: P, last: P) {
        let (low, ranks) = self.sa.split_at_mut(self.isa as usize);
        let ranks = &ranks[(isad - self.isa) as usize..];
        low[first as usize..last as usize].sort_unstable_by_key(|value| ranks[*value as usize]);
    }

    fn median3(&self, isad: P, mut v1: P, mut v2: P, v3: P) -> P {
        if self.key(isad, v1) > self.key(isad, v2) {
            core::mem::swap(&mut v1, &mut v2);
        }
        if self.key(isad, v2) > self.key(isad, v3) {
            if self.key(isad, v1) > self.key(isad, v3) {
                return v1;
            }
            return v3;
        }
        v2
    }

    fn pivot(&self, isad: P, first: P, last: P) -> P {
        let t = (last - first) >> 3;
        let middle = first + (last - first) / 2;
        if t <= 4 {
            return self.median3(isad, first, middle, last - 1);
        }
        let first = self.median3(isad, first, first + t, first + (t << 1));
        let middle = self.median3(isad, middle - t, middle, middle + t);
        let last = self.median3(isad, last - 1 - (t << 1), last - 1 - t, last - 1);
        self.median3(isad, first, middle, last)
    }

    fn swap_ranges(&mut self, mut e: P, mut f: P, mut count: P) {
        while 0 < count {
            self.swap(e, f);
            count -= 1;
            e += 1;
            f += 1;
        }
    }

    fn partition_small(&mut self, isad: P, first: P, last: P, v: i32) -> (P, P) {
        let (start, end) = (first as usize, last as usize);
        let same = self.sa[start..end]
            .iter()
            .take_while(|value| self.rank_at(isad, **value) == v)
            .count();
        let (mut less, mut equal) = (start, start + same);
        for at in start + same..end {
            let value = self.sa[at];
            let key = self.rank_at(isad, value);
            self.sa[at] = self.sa[equal];
            self.sa[equal] = value;
            let (at_less, at_equal) = (self.sa[less], self.sa[equal]);
            let smaller = -i32::from(key < v);
            self.sa[less] = (at_equal & smaller) | (at_less & !smaller);
            self.sa[equal] = (at_less & smaller) | (at_equal & !smaller);
            less += usize::from(key < v);
            equal += usize::from(key <= v);
        }
        (less as P, equal as P)
    }

    fn partition(&mut self, isad: P, mut first: P, middle: P, mut last: P, v: i32) -> (P, P) {
        if last - first <= SMALL_PARTITION {
            return self.partition_small(isad, first, last, v);
        }
        let mut x = 0;
        let mut b = middle - 1;
        loop {
            b += 1;
            if b >= last {
                break;
            }
            x = self.key(isad, b);
            if x != v {
                break;
            }
        }
        let mut a = b;
        if a < last && x < v {
            loop {
                b += 1;
                if b >= last {
                    break;
                }
                x = self.key(isad, b);
                if x > v {
                    break;
                }
                if x == v {
                    self.swap(b, a);
                    a += 1;
                }
            }
        }
        let mut c = last;
        loop {
            c -= 1;
            if b >= c {
                break;
            }
            x = self.key(isad, c);
            if x != v {
                break;
            }
        }
        let mut d = c;
        if b < d && x > v {
            loop {
                c -= 1;
                if b >= c {
                    break;
                }
                x = self.key(isad, c);
                if x < v {
                    break;
                }
                if x == v {
                    self.swap(c, d);
                    d -= 1;
                }
            }
        }
        while b < c {
            self.swap(b, c);
            loop {
                b += 1;
                if b >= c {
                    break;
                }
                x = self.key(isad, b);
                if x > v {
                    break;
                }
                if x == v {
                    self.swap(b, a);
                    a += 1;
                }
            }
            loop {
                c -= 1;
                if b >= c {
                    break;
                }
                x = self.key(isad, c);
                if x < v {
                    break;
                }
                if x == v {
                    self.swap(c, d);
                    d -= 1;
                }
            }
        }
        if a <= d {
            c = b - 1;
            let s = (a - first).min(b - a);
            self.swap_ranges(first, b - s, s);
            let s = (d - c).min(last - d - 1);
            self.swap_ranges(b, last - s, s);
            first += b - a;
            last -= d - c;
        }
        (first, last)
    }

    fn copy(&mut self, first: P, a: P, b: P, last: P, depth: P) {
        let v = (b - 1) as i32;
        let mut c = first;
        let mut d = a - 1;
        while c <= d {
            let s = self.get(c) as P - depth;
            if 0 <= s && self.rank_at(self.isa, s as i32) == v {
                d += 1;
                self.set(d, s as i32);
                self.set_rank(s as i32, d as i32);
            }
            c += 1;
        }
        let mut c = last - 1;
        let e = d + 1;
        let mut d = b;
        while e < d {
            let s = self.get(c) as P - depth;
            if 0 <= s && self.rank_at(self.isa, s as i32) == v {
                d -= 1;
                self.set(d, s as i32);
                self.set_rank(s as i32, d as i32);
            }
            c -= 1;
        }
    }

    fn partial_copy(&mut self, first: P, a: P, b: P, last: P, depth: P) {
        let v = (b - 1) as i32;
        let mut last_rank = -1;
        let mut new_rank = -1;
        let mut c = first;
        let mut d = a - 1;
        while c <= d {
            let s = self.get(c) as P - depth;
            if 0 <= s && self.rank_at(self.isa, s as i32) == v {
                d += 1;
                self.set(d, s as i32);
                let rank = self.rank_at(self.isa, (s + depth) as i32);
                if last_rank != rank {
                    last_rank = rank;
                    new_rank = d as i32;
                }
                self.set_rank(s as i32, new_rank);
            }
            c += 1;
        }

        last_rank = -1;
        let mut e = d;
        while first <= e {
            let rank = self.rank_at(self.isa, self.get(e));
            if last_rank != rank {
                last_rank = rank;
                new_rank = e as i32;
            }
            if new_rank != rank {
                self.set_rank(self.get(e), new_rank);
            }
            e -= 1;
        }

        last_rank = -1;
        let mut c = last - 1;
        let e = d + 1;
        let mut d = b;
        while e < d {
            let s = self.get(c) as P - depth;
            if 0 <= s && self.rank_at(self.isa, s as i32) == v {
                d -= 1;
                self.set(d, s as i32);
                let rank = self.rank_at(self.isa, (s + depth) as i32);
                if last_rank != rank {
                    last_rank = rank;
                    new_rank = d as i32;
                }
                self.set_rank(s as i32, new_rank);
            }
            c -= 1;
        }
    }

    fn rank_range(&mut self, from: P, to: P, rank: i32) {
        for at in from..to {
            self.set_rank(self.get(at), rank);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn introsort(
        &mut self,
        mut isad: P,
        mut first: P,
        mut last: P,
        step: P,
        mut limit: i32,
        budget: &mut Budget,
    ) {
        let isa = self.isa;
        let mut stack = [(0 as P, 0 as P, 0 as P, 0i32, 0 as P); STACK];
        let mut size = 0usize;
        let mut link: P = -1;
        macro_rules! push {
            ($a:expr, $b:expr, $c:expr, $d:expr, $e:expr) => {{
                stack[size] = ($a, $b, $c, $d, $e);
                size += 1;
            }};
        }
        macro_rules! pop {
            () => {{
                if size == 0 {
                    return;
                }
                size -= 1;
                (isad, first, last, limit, link) = stack[size];
            }};
        }
        macro_rules! unlink {
            () => {
                if 0 <= link {
                    stack[link as usize].3 = -1;
                }
            };
        }
        loop {
            if limit < 0 {
                if limit == -1 {
                    let (a, b) = self.partition(isad - step, first, first, last, (last - 1) as i32);
                    if a < last {
                        self.rank_range(first, a, (a - 1) as i32);
                    }
                    if b < last {
                        self.rank_range(a, b, (b - 1) as i32);
                    }
                    if 1 < b - a {
                        push!(0, a, b, 0, 0);
                        push!(isad - step, first, last, -2, link);
                        link = size as P - 2;
                    }
                    if a - first <= last - b {
                        if 1 < a - first {
                            push!(isad, b, last, ilg(last - b), link);
                            last = a;
                            limit = ilg(a - first);
                        } else if 1 < last - b {
                            first = b;
                            limit = ilg(last - b);
                        } else {
                            pop!();
                        }
                    } else if 1 < last - b {
                        push!(isad, first, a, ilg(a - first), link);
                        first = b;
                        limit = ilg(last - b);
                    } else if 1 < a - first {
                        last = a;
                        limit = ilg(a - first);
                    } else {
                        pop!();
                    }
                } else if limit == -2 {
                    size -= 1;
                    let (_, a, b, kind, _) = stack[size];
                    if kind == 0 {
                        self.copy(first, a, b, last, isad - isa);
                    } else {
                        unlink!();
                        self.partial_copy(first, a, b, last, isad - isa);
                    }
                    pop!();
                } else {
                    if 0 <= self.get(first) {
                        let mut a = first;
                        loop {
                            self.set_rank(self.get(a), a as i32);
                            a += 1;
                            if !(a < last && 0 <= self.get(a)) {
                                break;
                            }
                        }
                        first = a;
                    }
                    if first < last {
                        let mut a = first;
                        loop {
                            self.set(a, !self.get(a));
                            a += 1;
                            if self.get(a) >= 0 {
                                break;
                            }
                        }
                        let value = self.get(a);
                        let next = if self.rank_at(isa, value) == self.rank_at(isad, value) {
                            -1
                        } else {
                            ilg(a - first + 1)
                        };
                        a += 1;
                        if a < last {
                            self.rank_range(first, a, (a - 1) as i32);
                        }
                        if budget.check(a - first) {
                            if a - first <= last - a {
                                push!(isad, a, last, -3, link);
                                isad += step;
                                last = a;
                                limit = next;
                            } else if 1 < last - a {
                                push!(isad + step, first, a, next, link);
                                first = a;
                                limit = -3;
                            } else {
                                isad += step;
                                last = a;
                                limit = next;
                            }
                        } else {
                            unlink!();
                            if 1 < last - a {
                                first = a;
                                limit = -3;
                            } else {
                                pop!();
                            }
                        }
                    } else {
                        pop!();
                    }
                }
                continue;
            }

            if last - first <= INSERTION_LIMIT {
                self.insertion_sort(isad, first, last);
                limit = -3;
                continue;
            }

            let was = limit;
            limit -= 1;
            if was == 0 {
                self.sort_by_rank(isad, first, last);
                let mut a = last - 1;
                while first < a {
                    let x = self.key(isad, a);
                    let mut b = a - 1;
                    while first <= b && self.key(isad, b) == x {
                        self.set(b, !self.get(b));
                        b -= 1;
                    }
                    a = b;
                }
                limit = -3;
                continue;
            }

            let a = self.pivot(isad, first, last);
            self.swap(first, a);
            let v = self.key(isad, first);
            let (a, b) = self.partition(isad, first, first + 1, last, v);
            if last - first != b - a {
                let next = if self.rank_at(isa, self.get(a)) == v {
                    -1
                } else {
                    ilg(b - a)
                };
                self.rank_range(first, a, (a - 1) as i32);
                if b < last {
                    self.rank_range(a, b, (b - 1) as i32);
                }
                if 1 < b - a && budget.check(b - a) {
                    if a - first <= last - b {
                        if last - b <= b - a {
                            if 1 < a - first {
                                push!(isad + step, a, b, next, link);
                                push!(isad, b, last, limit, link);
                                last = a;
                            } else if 1 < last - b {
                                push!(isad + step, a, b, next, link);
                                first = b;
                            } else {
                                isad += step;
                                first = a;
                                last = b;
                                limit = next;
                            }
                        } else if a - first <= b - a {
                            if 1 < a - first {
                                push!(isad, b, last, limit, link);
                                push!(isad + step, a, b, next, link);
                                last = a;
                            } else {
                                push!(isad, b, last, limit, link);
                                isad += step;
                                first = a;
                                last = b;
                                limit = next;
                            }
                        } else {
                            push!(isad, b, last, limit, link);
                            push!(isad, first, a, limit, link);
                            isad += step;
                            first = a;
                            last = b;
                            limit = next;
                        }
                    } else if a - first <= b - a {
                        if 1 < last - b {
                            push!(isad + step, a, b, next, link);
                            push!(isad, first, a, limit, link);
                            first = b;
                        } else if 1 < a - first {
                            push!(isad + step, a, b, next, link);
                            last = a;
                        } else {
                            isad += step;
                            first = a;
                            last = b;
                            limit = next;
                        }
                    } else if last - b <= b - a {
                        if 1 < last - b {
                            push!(isad, first, a, limit, link);
                            push!(isad + step, a, b, next, link);
                            first = b;
                        } else {
                            push!(isad, first, a, limit, link);
                            isad += step;
                            first = a;
                            last = b;
                            limit = next;
                        }
                    } else {
                        push!(isad, first, a, limit, link);
                        push!(isad, b, last, limit, link);
                        isad += step;
                        first = a;
                        last = b;
                        limit = next;
                    }
                } else {
                    if 1 < b - a {
                        unlink!();
                    }
                    if a - first <= last - b {
                        if 1 < a - first {
                            push!(isad, b, last, limit, link);
                            last = a;
                        } else if 1 < last - b {
                            first = b;
                        } else {
                            pop!();
                        }
                    } else if 1 < last - b {
                        push!(isad, first, a, limit, link);
                        first = b;
                    } else if 1 < a - first {
                        last = a;
                    } else {
                        pop!();
                    }
                }
            } else if budget.check(last - first) {
                limit = ilg(last - first);
                isad += step;
            } else {
                unlink!();
                pop!();
            }
        }
    }
}

#[inline(never)]
pub(super) fn trsort(sa: &mut [i32], isa: P, n: P, depth: P) {
    let (sa, free) = sa.split_at_mut((isa + n) as usize);
    let (_, words, _) = bytemuck::pod_align_to_mut::<i32, u64>(free);
    let mut ranks = Ranks { sa, isa, words };
    let mut budget = Budget {
        chance: ilg(n) as P * 2 / 3,
        remain: n,
        step: n,
        count: 0,
    };
    let mut isad = isa + depth;
    while -n < ranks.get(0) as P {
        let mut first: P = 0;
        let mut skip: P = 0;
        let mut unsorted: P = 0;
        loop {
            let t = ranks.get(first) as P;
            if t < 0 {
                first -= t;
                skip += t;
            } else {
                if skip != 0 {
                    ranks.set(first + skip, skip as i32);
                    skip = 0;
                }
                let last = ranks.rank_at(isa, t as i32) as P + 1;
                if 1 < last - first {
                    budget.count = 0;
                    if !ranks.sort_by_keys(isad, first, last, &mut budget) {
                        let limit = ilg(last - first);
                        ranks.introsort(isad, first, last, isad - isa, limit, &mut budget);
                    }
                    if budget.count == 0 {
                        skip = first - last;
                    } else {
                        unsorted += budget.count;
                    }
                } else if last - first == 1 {
                    skip = -1;
                }
                first = last;
            }
            if first >= n {
                break;
            }
        }
        if skip != 0 {
            ranks.set(first + skip, skip as i32);
        }
        if unsorted == 0 {
            break;
        }
        isad += isad - isa;
    }
}
