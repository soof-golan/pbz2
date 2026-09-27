# How pbz2 works

bzip2 blocks are independent, so each goes to its own core.

## Decoding

- Blocks start at 48-bit markers, not at byte boundaries. pbz2 scans for the markers.
- Blocks decode in parallel; checksums are checked in stream order.
- A marker can also occur inside a block. Then pbz2 retries over at least twice the
  data, so any number of false markers costs at most two decodes.

## Encoding

- Suffix sort: divsufsort on the block's least rotation. It sorts about a third
  of the suffixes one bucket at a time and places the rest in one pass, so a
  900 KB block touches far less memory than other sorts; O(n log n) on any input.
- Huffman code lengths: package-merge, optimal under the 17-bit limit.

## Checksums

- With `simd`: carry-less multiply, the only `unsafe` in pbz2.
- Otherwise, and for the tail: a 16-byte table step.

## Measuring

```sh
cargo build --release -p pbz2-cli
hyperfine -N --output=null 'target/release/pbz2 -c -n1 file' 'target/release/pbz2 -dc -n1 file.bz2'
cargo bench -p pbz2-core --features simd
```

Drop `-n1` to use every core. `PBZ2_BENCH_INPUT=file` benches one block of that file.

## Fuzz targets

| Target | Checks |
| --- | --- |
| `decode` | every decoder agrees with libbzip2 |
| `core_decode` | scalar and SIMD decode one block the same |
| `roundtrip` | both encoders write the same stream, which decodes back |
| `encode` | same as `roundtrip`, on up to 1.2 MB grown from a short recipe |
