mod common;

use common::{assert_decode_fails, assert_decodes_to, decoded_by_bzip2, sequential_in_pieces};
use pbz2::Level;
use pbz2::pbz2_core::{Decoder, Pulled, decode_scratch_words};

const SAMPLE1_BZ2: &[u8] = include_bytes!("../../testdata/sample1.bz2");
const SAMPLE1_REF: &[u8] = include_bytes!("../../testdata/sample1.ref");
const SAMPLE2_BZ2: &[u8] = include_bytes!("../../testdata/sample2.bz2");
const SAMPLE2_REF: &[u8] = include_bytes!("../../testdata/sample2.ref");
const SAMPLE3_BZ2: &[u8] = include_bytes!("../../testdata/sample3.bz2");
const SAMPLE3_REF: &[u8] = include_bytes!("../../testdata/sample3.ref");
const RANDOMIZED_BLOCKS: &[u8] = include_bytes!("../../testdata/randomized-blocks.bin");

const HELLO_WORLD: &[u8] = &[
    0x42, 0x5a, 0x68, 0x31, 0x31, 0x41, 0x59, 0x26, 0x53, 0x59, 0x44, 0xf7, 0x13, 0x78, 0x00, 0x00,
    0x01, 0x91, 0x80, 0x40, 0x00, 0x06, 0x44, 0x90, 0x80, 0x20, 0x00, 0x22, 0x03, 0x34, 0x84, 0x30,
    0x21, 0xb6, 0x81, 0x54, 0x27, 0x8b, 0xb9, 0x22, 0x9c, 0x28, 0x48, 0x22, 0x7b, 0x89, 0xbc, 0x00,
];

#[test]
fn decompress_sample1() {
    assert_decodes_to(SAMPLE1_BZ2, SAMPLE1_REF);
}

#[test]
fn decompress_sample2() {
    assert_decodes_to(SAMPLE2_BZ2, SAMPLE2_REF);
}

#[test]
fn decompress_sample3() {
    assert_decodes_to(SAMPLE3_BZ2, SAMPLE3_REF);
}

#[test]
fn decode_in_small_pieces() {
    for piece_bytes in [1, 2, 3, 5, 64, 1000] {
        assert_eq!(
            sequential_in_pieces(SAMPLE1_BZ2, piece_bytes, pbz2::pbz2_core::native()).as_deref(),
            Ok(SAMPLE1_REF),
            "pieces of {piece_bytes} bytes"
        );
    }
}

#[test]
fn decode_randomized_blocks() {
    let expected = decoded_by_bzip2(RANDOMIZED_BLOCKS).expect("libbzip2 decodes it");
    assert_decodes_to(RANDOMIZED_BLOCKS, &expected);
}

#[test]
fn origin_pointer_past_block_is_refused() {
    let source: &[u8] = &[
        0x42, 0x5a, 0x68, 0x32, 0x31, 0x41, 0x59, 0x26, 0x53, 0x59, 0x03, 0x4f, 0x7e, 0x01, 0x01,
        0x86, 0xa5, 0x00, 0x00,
    ];
    assert!(decoded_by_bzip2(source).is_err());
    assert_decode_fails(source);
}

#[test]
fn decompress_hello_world() {
    assert_decodes_to(HELLO_WORLD, b"hello world");
}

#[test]
fn pull_with_empty_output_returns_zero() {
    let mut decoder = Decoder::new(
        vec![0u8; 1024],
        vec![0u32; decode_scratch_words(Level::BEST)],
    );
    assert_eq!(decoder.push(HELLO_WORLD), HELLO_WORLD.len());
    decoder.end_input();
    let mut nothing = [0u8; 0];
    for _ in 0..3 {
        assert_eq!(decoder.pull(&mut nothing), Ok(Pulled::Bytes(0)));
    }
    let mut out = [0u8; 64];
    assert_eq!(decoder.pull(&mut out), Ok(Pulled::Bytes(11)));
    assert_eq!(&out[..11], b"hello world");
    assert_eq!(decoder.pull(&mut out), Ok(Pulled::Finished));
}

#[test]
fn empty_input_is_refused() {
    assert_decode_fails(&[]);
}
