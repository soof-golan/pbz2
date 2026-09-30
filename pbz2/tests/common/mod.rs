#![allow(dead_code)]

use std::io::{Cursor, Read, Write};

use pbz2::pbz2_core::{
    Backend, Decoder, Encoder, Error, MAX_COMPRESSED_BLOCK_BYTES, Pulled, Scalar,
    decode_scratch_words, encode_scratch_words, native,
};
use pbz2::{DecoderReader, EncoderWriter, Level, ParallelDecoder, ParallelEncoder};

pub fn level(level: u8) -> Level {
    Level::new(level).expect("the tests use levels 1 to 9")
}

pub fn noise(length: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 32) as u8
        })
        .collect()
}

pub fn text_of_lines(count: u64) -> Vec<u8> {
    (0..count)
        .map(|line| format!("line {line} holds {}\n", line.wrapping_mul(line) % 9973))
        .collect::<String>()
        .into_bytes()
}

fn increasing_runs(longest: usize) -> Vec<u8> {
    let mut data = Vec::new();
    for length in 1..=longest {
        data.extend(std::iter::repeat_n((length % 251) as u8, length));
    }
    data
}

pub fn samples() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("empty", Vec::new()),
        ("one byte", vec![b'x']),
        ("every byte value", (0..=255).collect()),
        ("zeros", vec![0; 300_000]),
        ("noise", noise(150_000, 7)),
        ("text", text_of_lines(15_000)),
        ("runs of length 1 to 600", increasing_runs(600)),
        ("four of a kind", b"aaaabbbbccccdddd".repeat(1000)),
        ("five of a kind", b"aaaaabbbbbcccccddddd".repeat(1000)),
    ]
}

pub fn encoded_in_pieces<B: Backend>(
    data: &[u8],
    level: Level,
    in_piece: usize,
    out_piece: usize,
    backend: B,
) -> Vec<u8> {
    let mut encoder = Encoder::with_backend(
        level,
        vec![0u8; level.block_bytes()],
        vec![0u32; encode_scratch_words(level)],
        backend,
    )
    .expect("the buffers are big enough");
    let mut pieces = data.chunks(in_piece.max(1));
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

pub fn encoded_by_writer(data: &[u8], level: Level) -> Vec<u8> {
    let mut encoder = EncoderWriter::new(Vec::new(), level);
    for piece in data.chunks(100_003) {
        encoder.write_all(piece).expect("writing to a vector works");
    }
    encoder.finish().expect("writing to a vector works")
}

pub fn encoded_in_parallel(data: &[u8], level: Level, threads: usize) -> Vec<u8> {
    let mut encoder = ParallelEncoder::with_threads(Vec::new(), level, threads);
    for piece in data.chunks(65_537) {
        encoder.write_all(piece).expect("writing to a vector works");
    }
    encoder.finish().expect("writing to a vector works")
}

pub type EncodeWay = fn(&[u8], Level) -> Vec<u8>;

pub const ENCODE_WAYS: [(&str, EncodeWay); 6] = [
    ("sequential scalar, whole", |data, level| {
        encoded_in_pieces(data, level, usize::MAX, 1 << 16, Scalar)
    }),
    (
        "sequential native, 997-byte pieces in, 13-byte pieces out",
        |data, level| encoded_in_pieces(data, level, 997, 13, native()),
    ),
    ("encoder writer", encoded_by_writer),
    ("parallel on 1 thread", |data, level| {
        encoded_in_parallel(data, level, 1)
    }),
    ("parallel on 4 threads", |data, level| {
        encoded_in_parallel(data, level, 4)
    }),
    ("compress", pbz2::compress),
];

pub fn encoded_every_way(data: &[u8], level: Level) -> Vec<(&'static str, Vec<u8>)> {
    ENCODE_WAYS
        .iter()
        .map(|(way, encode)| (*way, encode(data, level)))
        .collect()
}

pub fn sequential_in_pieces<B: Backend>(
    input: &[u8],
    piece_bytes: usize,
    backend: B,
) -> Result<Vec<u8>, Error> {
    let mut decoder = Decoder::with_backend(
        vec![0u8; MAX_COMPRESSED_BLOCK_BYTES],
        vec![0u32; decode_scratch_words(Level::BEST)],
        backend,
    );
    let mut pieces = input.chunks(piece_bytes.max(1));
    let mut pending: &[u8] = &[];
    let mut out = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match decoder.pull(&mut chunk)? {
            Pulled::Bytes(count) => out.extend_from_slice(&chunk[..count]),
            Pulled::Finished => return Ok(out),
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

fn read_all(mut reader: impl Read) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    reader
        .read_to_end(&mut out)
        .map(|_| out)
        .map_err(|problem| problem.to_string())
}

pub type DecodeWay = fn(&[u8]) -> Result<Vec<u8>, String>;

pub const DECODE_WAYS: [(&str, DecodeWay); 6] = [
    ("sequential scalar, whole", |input| {
        sequential_in_pieces(input, usize::MAX, Scalar).map_err(|problem| problem.to_string())
    }),
    ("sequential native, 997-byte pieces", |input| {
        sequential_in_pieces(input, 997, native()).map_err(|problem| problem.to_string())
    }),
    ("decoder reader", |input| {
        read_all(DecoderReader::new(input))
    }),
    ("parallel on 1 thread", |input| {
        read_all(ParallelDecoder::with_threads(
            Cursor::new(input.to_vec()),
            1,
        ))
    }),
    ("parallel on 4 threads", |input| {
        read_all(ParallelDecoder::with_threads(
            Cursor::new(input.to_vec()),
            4,
        ))
    }),
    ("decompress", |input| {
        pbz2::decompress(input).map_err(|problem| problem.to_string())
    }),
];

pub fn every_way(input: &[u8]) -> Vec<(&'static str, Result<Vec<u8>, String>)> {
    DECODE_WAYS
        .iter()
        .map(|(way, decode)| (*way, decode(input)))
        .collect()
}

pub fn assert_decodes_to(input: &[u8], expected: &[u8]) {
    for (way, result) in every_way(input) {
        match result {
            Ok(out) => assert!(
                out == expected,
                "{way}: {} bytes differ from the {} expected",
                out.len(),
                expected.len()
            ),
            Err(problem) => panic!("{way}: {problem}"),
        }
    }
}

pub fn assert_decode_fails(input: &[u8]) {
    for (way, result) in every_way(input) {
        assert!(result.is_err(), "{way}: damaged data was accepted");
    }
}

pub fn made_by_bzip2(data: &[u8], level: u32) -> Vec<u8> {
    let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::new(level));
    encoder.write_all(data).expect("bzip2 compresses");
    encoder.finish().expect("bzip2 finishes")
}

pub fn decoded_by_bzip2(input: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    bzip2::read::MultiBzDecoder::new(input).read_to_end(&mut out)?;
    Ok(out)
}
