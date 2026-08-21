use crate::error::DecodeError;

/// A bounds-checked big-endian reader over a byte slice. NSE broadcasts every
/// multi-byte field in network (big-endian) order; `nse_fo_decoder.cpp`
/// achieves the same thing by reading native-endian then byte-swapping each
/// field by hand (`bswap16`/`bswap32`/`bswap_wide`) -- `from_be_bytes` is the
/// direct Rust equivalent, applied uniformly here instead of per call site.
pub struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn at(buf: &'a [u8], pos: usize) -> Self {
        Self { buf, pos }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let available = self.buf.len().saturating_sub(self.pos);
        if n > available {
            return Err(DecodeError::BufferTooShort {
                offset: self.pos,
                needed: n,
                available,
            });
        }
        let slice = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    pub fn read_i16(&mut self) -> Result<i16, DecodeError> {
        Ok(i16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    pub fn read_i32(&mut self) -> Result<i32, DecodeError> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn read_f64(&mut self) -> Result<f64, DecodeError> {
        Ok(f64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
}

/// Reads a big-endian i16 at an absolute offset without disturbing a cursor.
/// Used for the outer framing fields (`iNoPackets`, `iCompLen`), which are
/// peeked at before deciding how much of the buffer the next structure
/// actually covers.
pub fn peek_i16_at(buf: &[u8], offset: usize) -> Result<i16, DecodeError> {
    Cursor::at(buf, offset).read_i16()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_sequential_big_endian_fields() {
        // i16(1) then i32(2) then f64(3.5), all big-endian.
        let mut buf = Vec::new();
        buf.extend_from_slice(&1i16.to_be_bytes());
        buf.extend_from_slice(&2i32.to_be_bytes());
        buf.extend_from_slice(&3.5f64.to_be_bytes());

        let mut c = Cursor::at(&buf, 0);
        assert_eq!(c.read_i16().unwrap(), 1);
        assert_eq!(c.read_i32().unwrap(), 2);
        assert_eq!(c.read_f64().unwrap(), 3.5);
    }

    #[test]
    fn reports_buffer_too_short_instead_of_panicking() {
        let buf = [0u8; 3];
        let mut c = Cursor::at(&buf, 0);
        assert!(c.read_i32().is_err());
    }
}
