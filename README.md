# pbz2

Parallel bzip2 compression and decompression in Rust. Output is standard bzip2.

```toml
[dependencies]
pbz2 = "0.1"
```

```rust
let packed = pbz2::compress(b"hello hello hello", pbz2::Level::BEST);
assert_eq!(pbz2::decompress(&packed)?, b"hello hello hello");
```

```rust
let mut encoder = pbz2::ParallelEncoder::new(File::create("out.bz2")?, pbz2::Level::BEST);
io::copy(&mut File::open("in")?, &mut encoder)?;
encoder.finish()?;
```

## API

| Item | Threads | Interface |
| --- | --- | --- |
| `compress`, `decompress` | all cores | in memory |
| `ParallelEncoder` | all cores | `Write` |
| `ParallelDecoder` | all cores | `Read` |
| `EncoderWriter` | one | `Write` |
| `DecoderReader` | one | `Read` |

`Level` is 1 to 9. [`pbz2-core`](https://github.com/soof-golan/pbz2/blob/main/pbz2-core/README.md) is the `no_std` engine underneath.

The default `cli` feature builds the `pbz2` command. A library without it:
`pbz2 = { version = "0.1", default-features = false, features = ["simd"] }`.

## Command line

```sh
cargo install pbz2
```

`pbz2` takes the same flags as `bzip2` for everyday use: `-z -d -t -c -k -f -q -v -1…-9
--fast --best`, files or standard input, and the same output names, file attributes and
exit codes (1 for file problems, 2 for damaged data). Linked or copied as `bunzip2` or
`bzcat`, it decompresses. `-n N` sets the number of threads; the default is one per core.

Not supported: Windows, the `BZIP2` and `BZIP` environment variables, removing a partial
output file on Ctrl-C, and `bzip2recover`. Compressed files are valid bzip2 but not byte
for byte the same as `bzip2` makes.

## SIMD

The `simd` feature is on by default and chosen at compile time: NEON on aarch64, AVX2
or SSE4.2 on x86 when the target enables them (`RUSTFLAGS="-C target-cpu=native"`).

## Speed

Level 9, fastest of five interleaved runs. "All cores" is pbz2's default; the other
parallel tools get the same thread count.

Apple M4, 10 cores:

| | bzip2 | pbz2, 1 core | pbz2, all cores | lbzip2 | pbzip2 | 7-Zip |
| --- | --- | --- | --- | --- | --- | --- |
| Compress 300 MB tarball | 10.9 s | 4.6 s | 0.73 s | 1.11 s | 1.70 s | 21.2 s |
| Decompress it | 3.9 s | 1.5 s | 0.30 s | 0.54 s | 3.8 s | 1.7 s |
| Compress 126 MB Silesia | 5.6 s | 2.4 s | 0.38 s | 0.62 s | 0.90 s | 11.5 s |
| Decompress it | 2.1 s | 0.77 s | 0.17 s | 0.31 s | 2.1 s | 0.96 s |

Intel i7-13700K, 24 threads, Linux, default build:

| | bzip2 | pbz2, 1 core | pbz2, all cores | lbzip2 | pbzip2 |
| --- | --- | --- | --- | --- | --- |
| Compress 300 MB tarball | 10.3 s | 4.5 s | 0.36 s | 0.47 s | 0.75 s |
| Decompress it | 4.0 s | 1.5 s | 0.19 s | 0.27 s | 4.0 s |
| Compress 126 MB Silesia | 5.4 s | 2.4 s | 0.20 s | 0.28 s | 0.39 s |
| Decompress it | 2.1 s | 0.76 s | 0.11 s | 0.16 s | 2.1 s |

Built with `-C target-cpu=native` (AVX2), pbz2 on one core takes 4.4 s, 1.5 s, 2.3 s
and 0.76 s. pbzip2 decodes other tools' files
on one thread. 7-Zip level 9 makes extra passes.

## Contributing

See [CONTRIBUTING.md](https://github.com/soof-golan/pbz2/blob/main/CONTRIBUTING.md) and
the [AI policy](https://github.com/soof-golan/pbz2/blob/main/AI_POLICY.md). How pbz2
works, fuzzing and benchmarks:
[docs/how-it-works.md](https://github.com/soof-golan/pbz2/blob/main/docs/how-it-works.md).

## License

MIT, © Soof Golan. Third-party material:
[NOTICE](https://github.com/soof-golan/pbz2/blob/main/NOTICE).
