mod common;

use std::fmt::Write as _;

use common::{
    ENCODE_WAYS, assert_decodes_to, decoded_by_bzip2, encoded_every_way, level, noise, samples,
    text_of_lines,
};
use md5::{Digest, Md5};
use pbz2::Level;
use pbz2::pbz2_core::{
    BlockSplitter, ENCODE_BUFFER_BYTES, ENCODE_SCRATCH_WORDS, Encoder, Error, Pulled,
    StreamAssembler, encode_block, encode_scratch_words, encoded_block_bytes,
};

const SAMPLE1_REF: &[u8] = include_bytes!("../../testdata/sample1.ref");
const SAMPLE2_REF: &[u8] = include_bytes!("../../testdata/sample2.ref");
const SAMPLE3_REF: &[u8] = include_bytes!("../../testdata/sample3.ref");
const ISSUE_137: &[u8] = include_bytes!("../../testdata/issue_137.bin");
const LEVEL_1_BLOCK: usize = 100_000 - 19;

fn assert_decodes_back(packed: &[u8], data: &[u8], way: &str) {
    let by_libbzip2 = decoded_by_bzip2(packed).expect("libbzip2 decodes it");
    assert!(
        by_libbzip2 == data,
        "{way}: libbzip2 returns {} bytes, expected {}",
        by_libbzip2.len(),
        data.len()
    );
    let by_pbz2 = pbz2::decompress(packed).expect("pbz2 decodes it");
    assert!(by_pbz2 == data, "{way}: pbz2 decodes other bytes");
}

fn assert_round_trips(data: &[u8], number: u8) {
    let ways = encoded_every_way(data, level(number));
    let (first_way, packed) = &ways[0];
    for (way, other) in &ways[1..] {
        assert!(other == packed, "{way} writes other bytes than {first_way}");
    }
    assert_decodes_back(packed, data, first_way);
}

#[test]
fn compress_sample1() {
    assert_round_trips(SAMPLE1_REF, 9);
}

#[test]
fn compress_sample2() {
    assert_round_trips(SAMPLE2_REF, 9);
}

#[test]
fn compress_sample3() {
    assert_round_trips(SAMPLE3_REF, 9);
}

#[test]
fn issue_137() {
    assert_round_trips(ISSUE_137, 9);
}

#[test]
fn compress_fuzzer_regression() {
    assert_round_trips(&[0, 0, 67, 0, 67, 0, 0, 5, 0, 0], 1);
}

#[test]
fn compress_almost_periodic() {
    let data = [&b"12834"[..], "12345".repeat(100_000).as_bytes()].concat();
    assert_round_trips(&data, 6);
}

#[test]
fn compress_empty() {
    assert_round_trips(b"", 6);
}

#[test]
fn compress_random_small_inputs() {
    let mut state = 0x853c_49e6_748f_ea9bu64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for index in 0..200 {
        let length = (next() % 3000) as usize;
        let alphabet = 1 + next() % 256;
        let data: Vec<u8> = (0..length).map(|_| (next() % alphabet) as u8).collect();
        let number = 1 + (next() % 9) as u8;
        let (way, encode) = ENCODE_WAYS[index % ENCODE_WAYS.len()];
        assert_decodes_back(&encode(&data, level(number)), &data, way);
    }
}

#[test]
fn concatenated_pbz2_streams_decode() {
    let data = vec![3u8; 128 * 1024 + 1];
    let packed = encoded_every_way(&data, level(6)).swap_remove(0).1;
    let three = packed.repeat(3);
    assert_eq!(decoded_by_bzip2(&three).ok(), Some(data.repeat(3)));
    assert_decodes_to(&three, &data.repeat(3));
}

#[test]
fn samples_compress_to_known_bytes_at_every_level() {
    let mut lines = String::new();
    for (index, (name, data)) in samples().iter().enumerate() {
        for number in 1..=9u8 {
            let (_, encode) = ENCODE_WAYS[(index + usize::from(number)) % ENCODE_WAYS.len()];
            let packed = encode(data, level(number));
            let digest = Md5::digest(&packed);
            writeln!(
                lines,
                "{name}, level {number}: {} bytes, md5 {digest:x}",
                packed.len()
            )
            .expect("writing to a string works");
        }
    }
    insta::assert_snapshot!(lines);
}

#[test]
fn block_limit_splits_a_run() {
    for length in [
        LEVEL_1_BLOCK - 1,
        LEVEL_1_BLOCK,
        LEVEL_1_BLOCK + 1,
        3 * LEVEL_1_BLOCK + 7,
    ] {
        assert_round_trips(&noise(length, length as u64), 1);
    }
    let mut runs = noise(LEVEL_1_BLOCK - 3, 5);
    runs.extend(std::iter::repeat_n(b'r', 1000));
    runs.extend(noise(LEVEL_1_BLOCK, 6));
    assert_round_trips(&runs, 1);
}

#[test]
fn long_runs_compress_the_same_every_way() {
    let mut runs = Vec::new();
    for round in 0..12u8 {
        runs.extend(noise(3_000 * usize::from(round), u64::from(round)));
        runs.extend(std::iter::repeat_n(round, 60_000 * usize::from(round % 5)));
    }
    runs.extend(std::iter::repeat_n(0, 2_000_000));
    runs.extend(noise(LEVEL_1_BLOCK, 99));
    assert_round_trips(&runs, 1);
}

#[test]
fn compress_periodic_data() {
    for period in [&b"ab"[..], b"abc", b"abcdefghij", b"zyx"] {
        for repeats in [1, 2, 7, 50_000, 120_000] {
            assert_round_trips(&period.repeat(repeats), 1);
        }
    }
}

#[test]
fn output_size_within_half_percent_of_libbzip2() {
    let text = text_of_lines(60_000);
    for (name, data, number, theirs) in [
        ("sample1", SAMPLE1_REF, 1, 32_348),
        ("sample1", SAMPLE1_REF, 9, 32_348),
        ("sample2", SAMPLE2_REF, 1, 78_736),
        ("sample2", SAMPLE2_REF, 9, 72_612),
        ("sample3", SAMPLE3_REF, 1, 273),
        ("sample3", SAMPLE3_REF, 9, 235),
        ("text", &text[..], 1, 249_216),
        ("text", &text[..], 9, 251_386),
    ] {
        let ours = pbz2::compress(data, level(number)).len();
        assert!(
            ours * 1000 <= theirs * 1005,
            "{name} at level {number}: {ours} bytes, libbzip2 makes {theirs}"
        );
    }
}

#[test]
fn small_buffer_makes_small_blocks() {
    let data = text_of_lines(20_000);
    let block = 1000;
    let mut encoder = Encoder::new(
        Level::BEST,
        vec![0u8; block],
        vec![0u32; encode_scratch_words(block)],
    )
    .expect("the buffers are big enough");
    let mut out = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut pending = &data[..];
    loop {
        match encoder.pull(&mut chunk) {
            Pulled::Bytes(count) => out.extend_from_slice(&chunk[..count]),
            Pulled::Finished => break,
            Pulled::NeedInput if pending.is_empty() => encoder.end_input(),
            Pulled::NeedInput => pending = &pending[encoder.push(pending)..],
        }
    }
    assert_eq!(decoded_by_bzip2(&out).ok(), Some(data.clone()));
    assert_decodes_to(&out, &data);
}

#[test]
fn level_accepts_only_1_to_9() {
    assert_eq!(Level::new(0), None);
    assert_eq!(Level::new(10), None);
    assert_eq!(Level::try_from(0), Err(Error::BadLevel));
    assert_eq!(Level::new(1), Some(Level::FASTEST));
    assert_eq!(Level::try_from(9), Ok(Level::BEST));
    assert_eq!(Level::default(), Level::BEST);
}

#[test]
fn small_buffers_are_refused() {
    let encoder = |buffer: usize, scratch: usize| {
        Encoder::new(Level::BEST, vec![0u8; buffer], vec![0u32; scratch]).err()
    };
    assert_eq!(
        encoder(7, ENCODE_SCRATCH_WORDS),
        Some(Error::BufferTooSmall)
    );
    assert_eq!(
        encoder(ENCODE_BUFFER_BYTES, ENCODE_SCRATCH_WORDS - 1),
        Some(Error::ScratchTooSmall)
    );
    assert_eq!(encoder(ENCODE_BUFFER_BYTES, ENCODE_SCRATCH_WORDS), None);

    let mut splitter = BlockSplitter::new(Level::FASTEST);
    let bytes = splitter.block_bytes();
    let mut small = vec![0u8; bytes - 1];
    assert_eq!(
        splitter.fill(b"data", &mut small).err(),
        Some(Error::BufferTooSmall)
    );
    let mut block = vec![0u8; bytes];
    let (taken, filled) = splitter.fill(b"data", &mut block).expect("the block fits");
    assert_eq!((taken, filled), (4, None));
    let input = splitter
        .finish(&mut block)
        .expect("the block fits")
        .expect("there is a block");
    let words = encode_scratch_words(input.length());
    let out_bytes = encoded_block_bytes(input.length());
    let encode = |scratch: usize, out: usize| {
        encode_block(
            &mut block.clone(),
            input,
            &mut vec![0u32; scratch],
            &mut vec![0u8; out],
        )
        .err()
    };
    assert_eq!(encode(words - 1, out_bytes), Some(Error::ScratchTooSmall));
    assert_eq!(encode(words, out_bytes - 1), Some(Error::BufferTooSmall));
    assert_eq!(encode(words, out_bytes), None);
    let assembler = StreamAssembler::new(Level::FASTEST);
    assert_eq!(
        assembler.finish(&mut [0u8; 15]).err(),
        Some(Error::BufferTooSmall)
    );
}
