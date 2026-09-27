use std::io;

/// Why data could not be compressed or decompressed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The data is damaged or is not bzip2.
    #[error(transparent)]
    Data(#[from] pbz2_core::Error),
    /// Reading or writing failed.
    #[error(transparent)]
    Io(io::Error),
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        match error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<pbz2_core::Error>())
        {
            Some(data) => Self::Data(*data),
            None => Self::Io(error),
        }
    }
}

impl From<Error> for io::Error {
    fn from(error: Error) -> Self {
        match error {
            Error::Data(data) => Self::new(io::ErrorKind::InvalidData, data),
            Error::Io(error) => error,
        }
    }
}
