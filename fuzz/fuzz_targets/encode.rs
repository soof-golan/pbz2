#![no_main]

use std::io::{Read, Write};

use libfuzzer_sys::fuzz_target;
use pbz2::pbz2_core::{
    Backend, ENCODE_BUFFER_BYTES, ENCODE_SCRATCH_WORDS, Encoder, Level, Pulled, Scalar, native,
};
use pbz2::{ParallelEncoder, decompress};

const MOST_BYTES: usize = 1_200_000;

struct Recipe<'a> {
    steps: &'a [u8],
}

impl Recipe<'_> {
    fn byte(&mut self) -> Option<u8> {
        let (first, rest) = self.steps.split_first()?;
        self.steps = rest;
        Some(*first)
    }

    fn number(&mut self) -> Option<usize> {
        Some(usize::from(u16::from_le_bytes([
            self.byte()?,
            self.byte()?,
        ])))
    }

    fn data(mut self) -> Vec<u8> {
        let mut data = Vec::new();
        while data.len() < MOST_BYTES {
            let Some(step) = self.byte() else {
                break;
            };
            match step % 3 {
                0 => {
                    let length = usize::from(self.byte().unwrap_or(0)).min(self.steps.len());
                    let (literal, rest) = self.steps.split_at(length);
                    data.extend_from_slice(literal);
                    self.steps = rest;
                }
                1 => {
                    let (Some(byte), Some(length)) = (self.byte(), self.number()) else {
                        break;
                    };
                    data.resize(data.len() + length, byte);
                }
                _ => {
                    let (Some(length), Some(times)) = (self.number(), self.byte()) else {
                        break;
                    };
                    let start = data.len() - length.min(data.len());
                    for _ in 0..times {
                        if data.len() >= MOST_BYTES {
                            break;
                        }
                        data.extend_from_within(start..start + length.min(data.len() - start));
                    }
                }
            }
        }
        data.truncate(MOST_BYTES);
        data
    }
}

fn encoded<B: Backend>(
    data: &[u8],
    level: Level,
    in_piece: usize,
    out_piece: usize,
    backend: B,
) -> Vec<u8> {
    let mut encoder = Encoder::with_backend(
        level,
        vec![0u8; ENCODE_BUFFER_BYTES],
        vec![0u32; ENCODE_SCRATCH_WORDS],
        backend,
    )
    .unwrap();
    let mut pieces = data.chunks(in_piece);
    let mut pending: &[u8] = &[];
    let mut out = Vec::new();
    let mut chunk = vec![0u8; out_piece];
    loop {
        match encoder.pull(&mut chunk) {
            Pulled::Bytes(count) => out.extend_from_slice(&chunk[..count]),
            Pulled::Finished => return out,
            Pulled::NeedInput => {
                if pending.is_empty() {
                    match pieces.next() {
                        Some(piece) => pending = piece,
                        None => {
                            encoder.end_input();
                            continue;
                        }
                    }
                }
                let taken = encoder.push(pending);
                pending = &pending[taken..];
            }
        }
    }
}

fuzz_target!(|input: &[u8]| {
    let [level, in_piece, out_piece, steps @ ..] = input else {
        return;
    };
    let Some(level) = Level::new(1 + level % 9) else {
        return;
    };
    let in_piece = 1 + usize::from(*in_piece) * 64;
    let out_piece = 1 + usize::from(*out_piece);
    let data = Recipe { steps }.data();

    let whole = encoded(&data, level, usize::MAX, 1 << 16, native());
    assert_eq!(
        encoded(&data, level, in_piece, out_piece, Scalar),
        whole,
        "scalar in pieces"
    );

    let mut parallel = ParallelEncoder::with_threads(Vec::new(), level, 2);
    parallel.write_all(&data).unwrap();
    assert_eq!(parallel.finish().unwrap(), whole, "parallel");

    let mut by_libbzip2 = Vec::new();
    bzip2::read::BzDecoder::new(whole.as_slice())
        .read_to_end(&mut by_libbzip2)
        .unwrap();
    assert!(by_libbzip2 == data, "libbzip2 decodes it back");
    assert!(decompress(&whole).unwrap() == data, "pbz2 decodes it back");
});
