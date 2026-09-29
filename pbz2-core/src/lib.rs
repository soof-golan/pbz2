//! Parallel bzip2 compression and decompression that is `no_std`, does not allocate, and
//! does no I/O.
//!
//! The caller owns every buffer, thread and file: this crate only turns bytes into bytes
//! in memory it is given. Fixed-size working state lives on the stack.
//!
//! bzip2 compresses data in independent blocks, so both directions can use every core:
//!
//! - Sequential: [`Decoder`] and [`Encoder`] are complete decoders and encoders you push
//!   bytes into and pull bytes out of.
//! - Parallel decoding: [`Scanner`] finds the 48-bit markers where blocks start,
//!   [`decode_block_into`] decodes one block on any thread, and [`StreamChecker`] checks
//!   the results in order. A false marker inside compressed data is detected, and the
//!   block is decoded again over more data.
//! - Parallel encoding: [`BlockSplitter`] splits data into blocks, [`encode_block`] or
//!   [`encode_raw_block`] compresses one block on any thread, and [`StreamAssembler`]
//!   joins them into one standard bzip2 stream.
//!
//! The inner loops run in a [`Backend`] picked at compile time: [`Native`], which is SIMD
//! with the `simd` feature and [`Scalar`] without it. The `_with` functions and
//! `with_backend` constructors take another backend, for tests and benchmarks.
#![no_std]
#![deny(unsafe_code)]
#![warn(missing_docs)]

#[cfg(test)]
extern crate std;

mod assemble;
mod backend;
mod bits;
mod block;
mod bwt;
mod checker;
mod compress;
mod crc;
mod divsufsort;
mod encoder;
mod error;
mod huffman;
mod level;
mod native;
mod randomized;
mod scan;
mod stream;
mod tables;

pub use assemble::{
    BlockSplitter, EncodedBlock, InputBlock, RawBlock, StreamAssembler, code_runs_with,
    encode_block, encode_block_with, encode_raw_block, encode_raw_block_with, encoded_block_bytes,
};
#[cfg(feature = "simd")]
pub use backend::Vectorized;
pub use backend::{Backend, Scalar};
pub use block::{
    BLOCK_MAGIC, BLOCK_SIZE_STEP, BlockOutput, END_MAGIC, SCRATCH_WORDS, decode_block_into,
    decode_block_into_with, decode_scratch_words,
};
pub use checker::StreamChecker;
pub use crc::combine as combine_crc;
pub use encoder::{ENCODE_BUFFER_BYTES, ENCODE_SCRATCH_WORDS, Encoder, encode_scratch_words};
pub use error::Error;
#[cfg(feature = "simd")]
pub use fearless_simd;
pub use level::Level;
pub use native::{Native, native};
pub use scan::{Marker, MarkerKind, Scanner};
pub use stream::{Decoder, MAX_COMPRESSED_BLOCK_BYTES, Pulled};
