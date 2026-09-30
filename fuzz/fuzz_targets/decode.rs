#![no_main]

use std::io::{Cursor, Read};

use libfuzzer_sys::fuzz_target;
use pbz2::ParallelDecoder;
use pbz2::Level;
use pbz2::pbz2_core::{Decoder, MAX_COMPRESSED_BLOCK_BYTES, Pulled, decode_scratch_words};

fn sequential(input: &[u8], piece_bytes: usize) -> Option<Vec<u8>> {
    let mut decoder = Decoder::new(
        vec![0u8; MAX_COMPRESSED_BLOCK_BYTES],
        vec![0u32; decode_scratch_words(Level::BEST)],
    );
    let mut pieces = input.chunks(piece_bytes);
    let mut pending: &[u8] = &[];
    let mut out = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match decoder.pull(&mut chunk).ok()? {
            Pulled::Bytes(count) => out.extend_from_slice(&chunk[..count]),
            Pulled::Finished => return Some(out),
            Pulled::NeedInput => {
                if pending.is_empty() {
                    match pieces.next() {
                        Some(piece) => pending = piece,
                        None => {
                            decoder.end_input();
                            continue;
                        }
                    }
                }
                let taken = decoder.push(pending);
                pending = &pending[taken..];
            }
        }
    }
}

fn parallel(input: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    ParallelDecoder::with_threads(Cursor::new(input.to_vec()), 2)
        .read_to_end(&mut out)
        .ok()?;
    Some(out)
}

fn libbzip2(input: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    bzip2::read::MultiBzDecoder::new(input)
        .read_to_end(&mut out)
        .ok()?;
    Some(out)
}

fuzz_target!(|input: &[u8]| {
    let whole = sequential(input, usize::MAX);
    let piece_bytes = 1 + input.first().map_or(0, |byte| usize::from(*byte) % 64);
    assert_eq!(
        whole,
        sequential(input, piece_bytes),
        "{piece_bytes}-byte pieces"
    );
    assert_eq!(whole, parallel(input), "parallel");
    if let Some(expected) = libbzip2(input) {
        assert_eq!(whole, Some(expected), "libbzip2 decodes it");
    }
});
