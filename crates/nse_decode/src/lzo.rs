use crate::error::DecodeError;
use libloading::{Library, Symbol};
use std::ffi::c_void;
use std::path::Path;

// lzo1z_decompress's public signature (same for every lzo1{x,y,z} variant):
//   int lzo1z_decompress(const lzo_bytep src, lzo_uint src_len,
//                         lzo_bytep dst, lzo_uintp dst_len, lzo_voidp wrkmem);
// `lzo_uint` is `unsigned long`, which is 32 bits on Windows (LLP64) whether
// the process is 32- or 64-bit -- so it maps to `u32` here, not `usize`.
// Decompression needs no work buffer, so `wrkmem` is always null.
type Lzo1zDecompressFn = unsafe extern "C" fn(
    src: *const u8,
    src_len: u32,
    dst: *mut u8,
    dst_len: *mut u32,
    wrkmem: *mut c_void,
) -> i32;

const LZO_E_OK: i32 = 0;

/// Owns a loaded `liblzo2` and calls its `lzo1z_decompress` export. This is
/// the one piece of `nse_fo_decoder.cpp` that isn't reimplemented natively --
/// the C++ source doesn't implement LZO1Z itself either, it links the same
/// library. Binding to it dynamically keeps the Rust decoder's behavior
/// identical without redistributing or reimplementing the codec.
pub struct LzoDecompressor {
    _lib: Library,
    decompress: Lzo1zDecompressFn,
}

impl LzoDecompressor {
    pub fn load(dll_path: impl AsRef<Path>) -> Result<Self, DecodeError> {
        let path = dll_path.as_ref();
        // SAFETY: loading an arbitrary DLL is inherently unsafe (its static
        // initializers run immediately) -- acceptable here because the path
        // is a known-good, locally-provided library, not untrusted input.
        let lib = unsafe { Library::new(path) }.map_err(|source| DecodeError::LzoLibraryLoad {
            path: path.display().to_string(),
            source,
        })?;

        // SAFETY: the symbol's signature is guaranteed by liblzo2's public
        // API contract (lzo1z.h), which this crate does not link against
        // directly but mirrors exactly.
        let decompress: Lzo1zDecompressFn = unsafe {
            let symbol: Symbol<Lzo1zDecompressFn> =
                lib.get(b"lzo1z_decompress\0")
                    .map_err(|source| DecodeError::LzoLibraryLoad {
                        path: path.display().to_string(),
                        source,
                    })?;
            *symbol
        };

        Ok(Self {
            _lib: lib,
            decompress,
        })
    }

    /// Decompresses one LZO1Z-compressed message body. `max_output` bounds
    /// the output buffer; `nse_fo_decoder.cpp` uses a fixed 2048-byte scratch
    /// buffer for the same purpose.
    pub fn decompress(&self, compressed: &[u8], max_output: usize) -> Result<Vec<u8>, DecodeError> {
        let mut dst = vec![0u8; max_output];
        let mut dst_len: u32 = max_output as u32;

        // SAFETY: `dst` is a valid, exclusively-owned buffer of `max_output`
        // bytes; `dst_len` starts at that same capacity so the callee cannot
        // be told to write more than we allocated.
        let ret = unsafe {
            (self.decompress)(
                compressed.as_ptr(),
                compressed.len() as u32,
                dst.as_mut_ptr(),
                &mut dst_len,
                std::ptr::null_mut(),
            )
        };

        if ret != LZO_E_OK {
            return Err(DecodeError::LzoDecompressFailed(ret));
        }

        dst.truncate(dst_len as usize);
        Ok(dst)
    }
}
