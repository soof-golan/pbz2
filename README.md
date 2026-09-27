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

## Command line

```sh
cargo install pbz2-cli
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
| Compress 300 MB tarball | 10.5 s | 6.1 s | 0.94 s | 1.04 s | 1.59 s | 19.1 s |
| Decompress it | 3.9 s | 2.0 s | 0.33 s | 0.51 s | 3.8 s | 1.7 s |
| Compress 126 MB Silesia | 5.6 s | 3.4 s | 0.51 s | 0.61 s | 0.81 s | 10.4 s |
| Decompress it | 2.1 s | 0.96 s | 0.16 s | 0.29 s | 2.1 s | 0.97 s |

Intel i7-13700K, 24 threads, Linux, default build:

| | bzip2 | pbz2, 1 core | pbz2, all cores | lbzip2 |
| --- | --- | --- | --- | --- |
| Compress 300 MB tarball | 10.3 s | 6.2 s | 0.47 s | 0.48 s |
| Decompress it | 4.0 s | 2.0 s | 0.22 s | 0.27 s |
| Compress 126 MB Silesia | 5.4 s | 3.4 s | 0.26 s | 0.28 s |
| Decompress it | 2.1 s | 0.95 s | 0.12 s | 0.16 s |

Built with `-C target-cpu=native` (AVX2), pbz2 on one core takes 6.1 s, 1.9 s, 3.3 s
and 0.92 s. pbzip2 decodes other tools' files
on one thread. 7-Zip level 9 makes extra passes.

## Contributing

See [CONTRIBUTING.md](https://github.com/soof-golan/pbz2/blob/main/CONTRIBUTING.md) and
the [AI policy](https://github.com/soof-golan/pbz2/blob/main/AI_POLICY.md). How pbz2
works, fuzzing and benchmarks:
[docs/how-it-works.md](https://github.com/soof-golan/pbz2/blob/main/docs/how-it-works.md).

## License

MIT, © Soof Golan. Third-party material:
[NOTICE](https://github.com/soof-golan/pbz2/blob/main/NOTICE).
