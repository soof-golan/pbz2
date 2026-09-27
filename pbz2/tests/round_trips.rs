mod common;

use std::io::{self, Cursor, Read};

use pbz2::{DecoderReader, ParallelDecoder};

use common::{
    assert_decode_fails, assert_decodes_to, made_by_bzip2, noise, samples, text_of_lines,
};

#[test]
fn decodes_libbzip2_output_at_levels_1_2_9() {
    for (name, data) in samples() {
        for level in [1, 2, 9] {
            eprintln!("{name} at level {level}");
            assert_decodes_to(&made_by_bzip2(&data, level), &data);
        }
    }
}

#[test]
fn concatenated_streams_decode() {
    let first = text_of_lines(30_000);
    let second = noise(150_000, 3);
    let third = b"the end".to_vec();
    let packed = [
        made_by_bzip2(&first, 1),
        made_by_bzip2(&second, 9),
        made_by_bzip2(&third, 5),
    ]
    .concat();
    assert_decodes_to(&packed, &[first, second, third].concat());
}

#[test]
fn blocks_of_long_runs_decode() {
    let mut data = vec![0u8; 6_000_000];
    data.extend(std::iter::repeat_n(b'x', 1_000));
    data.extend(std::iter::repeat_n(7u8, 6_000_000));
    let packed = made_by_bzip2(&data, 1);
    assert!(packed.len() < 2_000, "{} bytes", packed.len());
    assert_decodes_to(&packed, &data);
}

#[test]
fn trailing_bytes_are_ignored() {
    let data = text_of_lines(20_000);
    for garbage in [&b"garbage"[..], &[0u8; 3], &[0u8; 100], b"BZ"] {
        let packed = [made_by_bzip2(&data, 1), garbage.to_vec()].concat();
        assert_decodes_to(&packed, &data);
    }
}

fn trailing_bytes_every_way(input: &[u8]) -> [bool; 3] {
    let mut sequential = DecoderReader::new(input);
    let mut one_thread = ParallelDecoder::with_threads(Cursor::new(input.to_vec()), 1);
    let mut four_threads = ParallelDecoder::with_threads(Cursor::new(input.to_vec()), 4);
    for reader in [
        &mut sequential as &mut dyn Read,
        &mut one_thread,
        &mut four_threads,
    ] {
        io::copy(reader, &mut io::sink()).expect("the data decodes");
    }
    [
        sequential.has_trailing_bytes(),
        one_thread.has_trailing_bytes(),
        four_threads.has_trailing_bytes(),
    ]
}

#[test]
fn trailing_bytes_are_reported() {
    let packed = made_by_bzip2(&text_of_lines(20_000), 1);
    assert_eq!(trailing_bytes_every_way(&packed), [false; 3]);
    assert_eq!(trailing_bytes_every_way(&packed.repeat(2)), [false; 3]);
    let block_marker = [0x31, 0x41, 0x59, 0x26, 0x53, 0x59, 0, 0];
    let end_marker = [0x17, 0x72, 0x45, 0x38, 0x50, 0x90, 0, 0];
    for garbage in [
        &b"garbage"[..],
        &[0u8; 1],
        &[0u8; 100],
        b"BZ",
        &block_marker,
        &end_marker,
    ] {
        let input = [packed.clone(), garbage.to_vec()].concat();
        assert_eq!(trailing_bytes_every_way(&input), [true; 3], "{garbage:?}");
    }
}

#[test]
fn damaged_byte_is_refused() {
    let packed = made_by_bzip2(&text_of_lines(60_000), 1);
    for at in [20, packed.len() / 3, packed.len() / 2, packed.len() - 20] {
        let mut damaged = packed.clone();
        damaged[at] ^= 0x10;
        assert_decode_fails(&damaged);
    }
}

#[test]
fn truncated_data_is_refused() {
    let packed = made_by_bzip2(&text_of_lines(60_000), 1);
    for keep in [3, 4, 10, packed.len() / 2, packed.len() - 1] {
        assert_decode_fails(&packed[..keep]);
    }
}

#[test]
fn non_bzip2_data_is_refused() {
    assert_decode_fails(b"plain text, not bzip2");
    assert_decode_fails(b"BZh0");
    assert_decode_fails(&made_by_bzip2(b"hello", 9)[1..]);
}
