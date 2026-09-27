use std::hint::black_box;
use std::io::Write;

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use pbz2_core::{
    Backend, BlockSplitter, ENCODE_SCRATCH_WORDS, InputBlock, Level, MarkerKind, SCRATCH_WORDS,
    Scalar, Scanner, decode_block_into_with, encode_block_with, encoded_block_bytes,
};

fn sample() -> Vec<u8> {
    let data = match std::env::var_os("PBZ2_BENCH_INPUT") {
        Some(path) => std::fs::read(path).expect("PBZ2_BENCH_INPUT can be read"),
        None => [
            &include_bytes!("../../testdata/sample1.ref")[..],
            include_bytes!("../../testdata/sample2.ref"),
            include_bytes!("../../testdata/sample3.ref"),
            include_bytes!("../../testdata/issue_137.bin"),
        ]
        .concat(),
    };
    let offset = std::env::var("PBZ2_BENCH_OFFSET")
        .ok()
        .and_then(|offset| offset.parse().ok())
        .unwrap_or(0usize)
        .min(data.len());
    data[offset..].to_vec()
}

fn first_block(data: &[u8]) -> (Vec<u8>, InputBlock) {
    let mut splitter = BlockSplitter::new(Level::BEST);
    let mut block = vec![0u8; splitter.block_bytes()];
    let (_, full) = splitter
        .fill(data, &mut block)
        .expect("the block buffer fits");
    let input = match full {
        Some(input) => input,
        None => splitter
            .finish(&mut block)
            .expect("the block buffer fits")
            .expect("the data is not empty"),
    };
    (block, input)
}

fn one_block_of_bzip2(original: &[u8]) -> (Vec<u8>, u64, u64) {
    let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::new(9));
    encoder.write_all(original).expect("bzip2 compresses");
    let packed = encoder.finish().expect("bzip2 finishes");
    let mut markers = Vec::new();
    Scanner::new().scan(&packed, |marker| markers.push(marker));
    let start = markers
        .iter()
        .find(|marker| marker.kind == MarkerKind::Block)
        .expect("there is a block")
        .bit;
    let end = markers
        .iter()
        .find(|marker| marker.bit > start)
        .expect("the block ends")
        .bit;
    (packed, start, end)
}

fn encode_one<B: Backend>(criterion: &mut Criterion, name: &str, backend: B) {
    let (block, input) = first_block(&sample());
    let mut scratch = vec![0u32; ENCODE_SCRATCH_WORDS];
    let mut out = vec![0u8; encoded_block_bytes(input.length())];
    let mut group = criterion.benchmark_group("encode a block");
    group.throughput(Throughput::Bytes(input.length() as u64));
    group.bench_function(name, |bencher| {
        bencher.iter_batched_ref(
            || block.clone(),
            |block| {
                black_box(
                    encode_block_with(block, input, &mut scratch, &mut out, backend)
                        .expect("the buffers fit"),
                )
            },
            BatchSize::LargeInput,
        );
    });
    group.finish();
}

fn decode_one<B: Backend>(criterion: &mut Criterion, name: &str, backend: B) {
    let data = sample();
    let original = &data[..data.len().min(Level::BEST.block_bytes() - 19)];
    let (packed, start, end) = one_block_of_bzip2(original);
    let mut scratch = vec![0u32; SCRATCH_WORDS];
    let mut out = vec![0u8; 1 << 16];
    let mut group = criterion.benchmark_group("decode a block");
    group.throughput(Throughput::Bytes(original.len() as u64));
    group.bench_function(name, |bencher| {
        bencher.iter(|| {
            let mut output =
                decode_block_into_with(&packed, start, end, Level::BEST, &mut scratch, backend)
                    .expect("the block decodes");
            while output.read(&scratch, &mut out) > 0 {}
            black_box(output.finish().expect("the checksum matches"))
        });
    });
    group.finish();
}

fn blocks(criterion: &mut Criterion) {
    encode_one(criterion, "scalar", Scalar);
    decode_one(criterion, "scalar", Scalar);
    #[cfg(all(feature = "simd", target_arch = "aarch64"))]
    if let Some(neon) = fearless_simd::Level::new().as_neon() {
        encode_one(criterion, "neon", pbz2_core::Vectorized(neon));
        decode_one(criterion, "neon", pbz2_core::Vectorized(neon));
    }
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if let Some(avx2) = fearless_simd::Level::new().as_avx2() {
        encode_one(criterion, "avx2", pbz2_core::Vectorized(avx2));
        decode_one(criterion, "avx2", pbz2_core::Vectorized(avx2));
    }
}

criterion_group!(benches, blocks);
criterion_main!(benches);
