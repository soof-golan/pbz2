#![no_main]

use libfuzzer_sys::fuzz_target;
use pbz2::pbz2_core::{
    Backend, Error, Level, Scalar, decode_block_into, decode_scratch_words, native,
};

fn decoded<B: Backend>(bytes: &[u8], start_bit: u64, backend: B) -> Result<(Vec<u8>, u32), Error> {
    let mut scratch = vec![0u32; decode_scratch_words(Level::BEST)];
    let end_bit = bytes.len() as u64 * 8;
    let mut output = decode_block_into(
        backend,
        bytes,
        start_bit,
        end_bit,
        Level::BEST,
        &mut scratch,
    )?;
    let mut out = Vec::new();
    let mut piece = [0u8; 4096];
    loop {
        let count = output.read(&scratch, &mut piece);
        if count == 0 {
            break;
        }
        out.extend_from_slice(&piece[..count]);
    }
    Ok((out, output.finish()?))
}

fuzz_target!(|input: &[u8]| {
    let Some((first, bytes)) = input.split_first() else {
        return;
    };
    let start_bit = u64::from(first % 8);
    assert_eq!(
        decoded(bytes, start_bit, Scalar),
        decoded(bytes, start_bit, native()),
        "the scalar and native backends agree"
    );
});
