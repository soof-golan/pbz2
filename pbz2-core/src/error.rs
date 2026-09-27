/// Why bzip2 data could not be decoded or encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The data does not start with a bzip2 stream header.
    #[error("the data is not bzip2")]
    NotBzip2,
    /// The data ends in the middle of a stream or block.
    #[error("the bzip2 data ends early")]
    Truncated,
    /// A block or end-of-stream marker was expected but something else was found.
    #[error("a bzip2 block marker is missing")]
    BadBlockMagic,
    /// The block's table of used byte values is empty.
    #[error("a bzip2 block uses no byte values")]
    BadSymbolMap,
    /// The block uses fewer than 2 or more than 6 Huffman tables.
    #[error("a bzip2 block has a bad number of Huffman tables")]
    BadGroupCount,
    /// The block's table selectors are missing or out of range.
    #[error("a bzip2 block has bad Huffman table selectors")]
    BadSelectors,
    /// A Huffman code length is outside 1 to 20.
    #[error("a bzip2 block has a bad Huffman code length")]
    BadCodeLengths,
    /// The data contains a bit pattern that is not a Huffman code.
    #[error("the bzip2 data has a bad Huffman code")]
    BadHuffmanCode,
    /// A run of repeated bytes is longer than bzip2 allows.
    #[error("a bzip2 block has a run that is too long")]
    RunTooLong,
    /// A block holds more bytes than its stream's block size allows.
    #[error("a bzip2 block is larger than its stream allows")]
    BlockTooLarge,
    /// The block's start pointer is outside the block.
    #[error("a bzip2 block has a bad start pointer")]
    BadOriginPointer,
    /// A block's checksum does not match its contents.
    #[error("a bzip2 block checksum does not match")]
    BlockCrcMismatch,
    /// A stream's checksum does not match its blocks.
    #[error("a bzip2 stream checksum does not match")]
    StreamCrcMismatch,
    /// The scratch space is smaller than the block size needs.
    #[error("the scratch space is too small for this block size")]
    ScratchTooSmall,
    /// When decoding, the input buffer cannot hold one whole compressed block. When
    /// encoding, a buffer is smaller than the block needs.
    #[error("a buffer is too small")]
    BufferTooSmall,
    /// Raw bytes given for a block are not the bytes [`crate::BlockSplitter::scan`] took
    /// for it.
    #[error("the bytes are not the ones the block splitter scanned")]
    NotScanned,
    /// A number given as a [`crate::Level`] is not 1 to 9.
    #[error("the compression level is not 1 to 9")]
    BadLevel,
}
