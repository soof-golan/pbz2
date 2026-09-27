#![no_main]

use std::io::{Read, Write};

use libfuzzer_sys::fuzz_target;
use pbz2::{EncoderWriter, Level, ParallelEncoder, decompress};

fuzz_target!(|input: &[u8]| {
    let Some((first, data)) = input.split_first() else {
        return;
    };
    let Some(level) = Level::new(1 + first % 9) else {
        return;
    };

    let mut sequential = EncoderWriter::new(Vec::new(), level);
    sequential.write_all(data).unwrap();
    let sequential = sequential.finish().unwrap();

    let mut parallel = ParallelEncoder::with_threads(Vec::new(), level, 2);
    parallel.write_all(data).unwrap();
    let parallel = parallel.finish().unwrap();
    assert_eq!(
        sequential, parallel,
        "the parallel encoder writes the same stream"
    );

    let mut by_libbzip2 = Vec::new();
    bzip2::read::BzDecoder::new(sequential.as_slice())
        .read_to_end(&mut by_libbzip2)
        .unwrap();
    assert_eq!(by_libbzip2, data, "libbzip2 decodes it back");
    assert_eq!(
        decompress(&sequential).unwrap(),
        data,
        "pbz2 decodes it back"
    );
});
