use crate::compress::MAX_GROUPS;
use crate::huffman::MAX_ALPHABET;

const LANES: usize = 8;

pub(crate) struct Costs {
    rows: [[u16; LANES]; MAX_ALPHABET],
    groups: usize,
}

impl Costs {
    pub(crate) fn new(
        lengths: &[[u8; MAX_ALPHABET]; MAX_GROUPS],
        groups: usize,
        alphabet: usize,
    ) -> Self {
        let mut rows = [[0u16; LANES]; MAX_ALPHABET];
        for (symbol, row) in rows[..alphabet].iter_mut().enumerate() {
            for (cost, lengths) in row.iter_mut().zip(&lengths[..groups]) {
                *cost = u16::from(lengths[symbol]);
            }
        }
        Self { rows, groups }
    }

    #[inline]
    pub(crate) fn cheapest(&self, chunk: &[u16]) -> usize {
        let mut total = [0u16; LANES];
        for symbol in chunk {
            let row = &self.rows[*symbol as usize];
            for (sum, cost) in total.iter_mut().zip(row) {
                *sum += *cost;
            }
        }
        let mut best = 0;
        for group in 1..self.groups {
            if total[group] < total[best] {
                best = group;
            }
        }
        best
    }
}
