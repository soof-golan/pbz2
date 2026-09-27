# pbz2-core

The `no_std` engine of [pbz2](https://github.com/soof-golan/pbz2). It does not allocate and does no I/O: the
caller provides every buffer and thread. It builds for bare-metal targets such as
`thumbv7em-none-eabihf`.

## API

| | Decoding | Encoding |
| --- | --- | --- |
| Sequential | `Decoder` | `Encoder` |
| One block, any thread | `Scanner`, `decode_block_into` | `BlockSplitter`, `encode_block` |
| Joining blocks in order | `StreamChecker` | `StreamAssembler` |

Every buffer size is a constant or `const fn`, so buffers can be static arrays:

| | Input | Scratch |
| --- | --- | --- |
| Decoding | `MAX_COMPRESSED_BLOCK_BYTES` | `SCRATCH_WORDS` |
| Encoding | `ENCODE_BUFFER_BYTES` | `ENCODE_SCRATCH_WORDS` |

```rust
let mut input = [0u8; pbz2_core::MAX_COMPRESSED_BLOCK_BYTES];
let mut scratch = [0u32; pbz2_core::SCRATCH_WORDS];
let mut decoder = pbz2_core::Decoder::new(&mut input[..], &mut scratch[..]);
// decoder.push(bytes), decoder.pull(&mut out), decoder.end_input()
```

pbz2's `src/decode.rs` and `src/encode.rs` show the per-block API on threads.

## SIMD

`Native` is the backend chosen at compile time: SIMD with the `simd` feature, `Scalar`
without it. The only `unsafe` code is one carry-less multiply for the CRC, with `simd`.

## License

MIT, © Soof Golan. Third-party material: [NOTICE](https://github.com/soof-golan/pbz2/blob/main/NOTICE).
