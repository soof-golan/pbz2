//! Fast parallel bzip2 compression and decompression.
//!
//! In memory:
//!
//! ```
//! let data = b"hello hello hello hello";
//! let packed = pbz2::compress(data, pbz2::Level::BEST);
//! assert_eq!(pbz2::decompress(&packed)?, data);
//! # Ok::<(), pbz2::Error>(())
//! ```
//!
//! Streaming, with any [`std::io::Write`] and [`std::io::Read`], such as files or
//! sockets:
//!
//! ```
//! use std::io::{self, Cursor};
//!
//! let original = b"one block of text, two blocks of text".repeat(1000);
//!
//! let mut encoder = pbz2::ParallelEncoder::new(Vec::new(), pbz2::Level::BEST);
//! io::copy(&mut original.as_slice(), &mut encoder)?;
//! let packed = encoder.finish()?;
//!
//! let mut decoder = pbz2::ParallelDecoder::new(Cursor::new(packed));
//! let mut unpacked = Vec::new();
//! io::copy(&mut decoder, &mut unpacked)?;
//! assert_eq!(unpacked, original);
//! # Ok::<(), io::Error>(())
//! ```
//!
//! [`ParallelDecoder`] and [`ParallelEncoder`] use every core, and decode data while it
//! is still arriving. [`DecoderReader`] and [`EncoderWriter`] use the calling thread only.
//! With the `simd` feature, which is on by default, all of them use the SIMD instructions
//! the build targets; see [`pbz2_core::Native`]. On x86-64 builds that do not target
//! SSE4.2, each one checks the CPU once when it is created and uses SSE4.2 if it is there.
//!
//! Output is a standard bzip2 stream that any bzip2 tool reads, and any bzip2 stream,
//! including several in a row, decodes.
//!
//! Everything here is built on [`pbz2_core`], which does not allocate, does no I/O, and
//! runs without `std`, for embedded targets or your own threads.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod decode;
mod encode;
mod engine;
mod error;
mod pool;
mod split;
mod workers;

pub use decode::{DecoderReader, ParallelDecoder, decompress};
pub use encode::{EncoderWriter, ParallelEncoder, compress};
pub use error::Error;
pub use pbz2_core;
pub use pbz2_core::Level;
