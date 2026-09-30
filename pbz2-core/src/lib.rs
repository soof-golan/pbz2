//! Parallel bzip2 compression and decompression that is `no_std`, does not allocate, and
//! does no I/O.
//!
//! The caller owns every buffer, thread and file: this crate only turns bytes into bytes
//! in memory it is given. Fixed-size working state lives on the stack.
//!
//! bzip2 compresses data in independent blocks, so both directions can use every core:
//!
//! - Sequential: [`Decoder`] and [`Encoder`].
//! - Parallel decoding: [`Scanner`] finds block markers, [`decode_block_into`] decodes a
//!   block on any thread, and [`StreamChecker`] checks the results in order.
//! - Parallel encoding: [`BlockSplitter`] splits data into blocks, [`encode_block`]
//!   compresses one on any thread, and [`StreamAssembler`] joins them into a stream.
//!
//! Functions that run inner loops take a [`Backend`]; [`native`] is the best one the
//! build target enables.
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
    BlockSplitter, EncodedBlock, InputBlock, RawBlock, StreamAssembler, code_runs, encode_block,
    encoded_block_bytes,
};
#[cfg(feature = "simd")]
pub use backend::Vectorized;
pub use backend::{Backend, Scalar};
pub use block::{BlockOutput, decode_block_into, decode_scratch_words};
pub use checker::StreamChecker;
pub use encoder::{Encoder, encode_scratch_words};
pub use error::Error;
#[cfg(feature = "simd")]
pub use fearless_simd;
pub use level::Level;
pub use native::{Native, native};
pub use scan::{Marker, MarkerKind, Scanner};
pub use stream::{Decoder, MAX_COMPRESSED_BLOCK_BYTES, Pulled};
