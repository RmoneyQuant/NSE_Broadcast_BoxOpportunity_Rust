use thiserror::Error;

#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("buffer too short: needed at least {needed} bytes at offset {offset}, got {available}")]
    BufferTooShort {
        offset: usize,
        needed: usize,
        available: usize,
    },

    #[error("LZO1Z decompression failed (liblzo2 returned code {0})")]
    LzoDecompressFailed(i32),

    #[error("failed to load {path}: {source}")]
    LzoLibraryLoad {
        path: String,
        #[source]
        source: libloading::Error,
    },
}
