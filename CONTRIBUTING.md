# Contributing

## Use of AI

All AI contributions must follow the [AI Policy](AI_POLICY.md).

## Setup

Install [Rust](https://rustup.rs/). Then fetch the test corpus once:

```sh
scripts/fetch-test-corpus.sh
```

## Testing

```sh
cargo test --workspace --release
cargo test --workspace --release --no-default-features
```

Compressed output is checked against snapshots in `pbz2/tests/snapshots`. When the bytes change on purpose, update them
with `INSTA_UPDATE=always cargo test` or `cargo insta review`.

## Formatting and lints

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo doc --workspace --no-deps
cargo check -p pbz2-core --features simd --target thumbv7em-none-eabihf
cargo check -p pbz2-core --features simd --target wasm32-unknown-unknown
```

## Benchmarks

See [docs/how-it-works.md](docs/how-it-works.md#measuring). When comparing two builds, alternate their runs and compare
cycle counts (`/usr/bin/time -l` on macOS); a machine that heats up under load makes whichever build runs later look
slower.

## Releases

Releases are made by the maintainer. Publish in dependency order: `pbz2-core`, then
`pbz2`, then `pbz2-cli`.

This guide is adapted from uv's
[CONTRIBUTING.md](https://github.com/astral-sh/uv/blob/main/CONTRIBUTING.md) (MIT or Apache-2.0).
