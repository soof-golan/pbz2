use std::io::Read;

use pbz2_core::{
    Decoder, Encoder, Error, Level, MarkerKind, Pulled, Scanner, decode_block_into,
    decode_scratch_words, encode_scratch_words, native,
};

const SAMPLE1_BZ2: &[u8] = include_bytes!("../../testdata/sample1.bz2");
const SAMPLE1_REF: &[u8] = include_bytes!("../../testdata/sample1.ref");
const SAMPLE3_BZ2: &[u8] = include_bytes!("../../testdata/sample3.bz2");
const SAMPLE3_REF: &[u8] = include_bytes!("../../testdata/sample3.ref");

#[test]
fn decoder_on_borrowed_buffers() {
    let mut input_buffer = [0u8; 4096];
    let mut scratch = vec![0u32; decode_scratch_words(Level::BEST)];
    let mut decoder = Decoder::new(&mut input_buffer[..], &mut scratch[..]);
    let mut pending = SAMPLE3_BZ2;
    let mut out = Vec::new();
    let mut chunk = [0u8; 1000];
    loop {
        match decoder.pull(&mut chunk) {
            Ok(Pulled::Bytes(count)) => out.extend_from_slice(&chunk[..count]),
            Ok(Pulled::Finished) => break,
            Ok(Pulled::NeedInput) if pending.is_empty() => decoder.end_input(),
            Ok(Pulled::NeedInput) => {
                let taken = decoder.push(pending);
                pending = &pending[taken..];
            }
            Err(problem) => panic!("{problem}"),
        }
    }
    assert_eq!(out, SAMPLE3_REF);
}

#[test]
fn encoder_on_borrowed_buffers() {
    let mut input_buffer = vec![0u8; 50_000];
    let mut scratch = vec![0u32; encode_scratch_words(Level::BEST)];
    let mut encoder = Encoder::new(Level::BEST, &mut input_buffer[..], &mut scratch[..])
        .expect("the buffers are big enough");
    let mut pending = SAMPLE3_REF;
    let mut out = Vec::new();
    let mut chunk = [0u8; 1000];
    loop {
        match encoder.pull(&mut chunk) {
            Pulled::Bytes(count) => out.extend_from_slice(&chunk[..count]),
            Pulled::Finished => break,
            Pulled::NeedInput if pending.is_empty() => encoder.end_input(),
            Pulled::NeedInput => pending = &pending[encoder.push(pending)..],
        }
    }
    let mut unpacked = Vec::new();
    bzip2::read::BzDecoder::new(&out[..])
        .read_to_end(&mut unpacked)
        .expect("libbzip2 decodes it");
    assert_eq!(unpacked, SAMPLE3_REF);
}

#[test]
fn oversized_block_returns_buffer_too_small() {
    let mut input_buffer = [0u8; 1024];
    let mut scratch = vec![0u32; decode_scratch_words(Level::BEST)];
    let mut decoder = Decoder::new(&mut input_buffer[..], &mut scratch[..]);
    let mut pending = SAMPLE1_BZ2;
    let mut chunk = [0u8; 1000];
    let problem = loop {
        match decoder.pull(&mut chunk) {
            Ok(Pulled::NeedInput) => {
                let taken = decoder.push(pending);
                pending = &pending[taken..];
            }
            Ok(_) => {}
            Err(problem) => break problem,
        }
    };
    assert_eq!(problem, Error::BufferTooSmall);
}

#[test]
fn scanned_blocks_decode_independently() {
    let mut markers = Vec::new();
    let mut scanner = Scanner::new();
    for piece in SAMPLE1_BZ2.chunks(100) {
        scanner.scan(native(), piece, |marker| markers.push(marker));
    }
    let level = Level::new(SAMPLE1_BZ2[3] - b'0').expect("the header names a level");
    assert!(markers.len() >= 2);
    assert_eq!(
        markers.last().map(|marker| marker.kind),
        Some(MarkerKind::End)
    );

    let mut scratch = vec![0u32; decode_scratch_words(level)];
    let mut out = Vec::new();
    let mut stream_crc = 0u32;
    for pair in markers.windows(2) {
        let mut block = decode_block_into(
            native(),
            SAMPLE1_BZ2,
            pair[0].bit,
            pair[1].bit,
            level,
            &mut scratch,
        )
        .expect("the block decodes");
        let mut chunk = [0u8; 777];
        loop {
            let count = block.read(&scratch, &mut chunk);
            if count == 0 {
                break;
            }
            out.extend_from_slice(&chunk[..count]);
        }
        assert_eq!(block.end_bit(), pair[1].bit);
        stream_crc = stream_crc.rotate_left(1) ^ block.finish().expect("the checksum matches");
    }
    assert_eq!(out, SAMPLE1_REF);

    let end = markers.last().expect("there is an end marker").bit;
    let stored = (end + 48) as usize;
    let mut eight = [0u8; 8];
    let rest = &SAMPLE1_BZ2[stored / 8..];
    let count = rest.len().min(8);
    eight[..count].copy_from_slice(&rest[..count]);
    let tail = u64::from_be_bytes(eight);
    assert_eq!((tail << (stored % 8)) >> 32, u64::from(stream_crc));
}
