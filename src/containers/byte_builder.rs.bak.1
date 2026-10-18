//! Byte builder and cursor (feature 23). The builder appends values in little-endian order into an
//! allocator-backed buffer. The cursor reads them back with bounds checks and never panics on short input.

use crate::allocators::traits::Allocator;
use crate::containers::Vector;
use crate::foundation::error::AllocResult;

/// Appends bytes and fixed-width integers (little-endian) to an allocator-backed buffer.
pub struct ByteBuilder<'a, A: Allocator> {
    bytes: Vector<'a, u8, A>,
}

impl<'a, A: Allocator> ByteBuilder<'a, A> {
    /// An empty builder whose storage comes from `alloc`.
    pub fn new(alloc: &'a A) -> AllocResult<Self> {
        Ok(ByteBuilder { bytes: Vector::new(alloc)? })
    }

    /// Number of bytes written.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// True when nothing has been written.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// The bytes written so far.
    pub fn as_slice(&self) -> &[u8] {
        self.bytes.as_slice()
    }

    /// Appends one byte.
    pub fn put_u8(&mut self, v: u8) -> AllocResult<()> {
        self.bytes.push(v)
    }

    /// Appends a `u16` in little-endian order.
    pub fn put_u16_le(&mut self, v: u16) -> AllocResult<()> {
        self.put_bytes(&v.to_le_bytes())
    }

    /// Appends a `u32` in little-endian order.
    pub fn put_u32_le(&mut self, v: u32) -> AllocResult<()> {
        self.put_bytes(&v.to_le_bytes())
    }

    /// Appends a `u64` in little-endian order.
    pub fn put_u64_le(&mut self, v: u64) -> AllocResult<()> {
        self.put_bytes(&v.to_le_bytes())
    }

    /// Appends raw bytes.
    pub fn put_bytes(&mut self, data: &[u8]) -> AllocResult<()> {
        self.bytes.reserve(data.len())?;
        for &b in data {
            self.bytes.push(b)?;
        }
        Ok(())
    }
}

/// A read error: the input ended before the requested value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadError {
    OutOfData,
}

/// Reads values written by [`ByteBuilder`] back out of a byte slice.
pub struct Cursor<'b> {
    data: &'b [u8],
    pos: usize,
}

impl<'b> Cursor<'b> {
    /// A cursor at the start of `data`.
    pub fn new(data: &'b [u8]) -> Self {
        Cursor { data, pos: 0 }
    }

    /// Bytes already read.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Bytes not yet read.
    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    /// Reads `n` bytes.
    pub fn read_bytes(&mut self, n: usize) -> Result<&'b [u8], ReadError> {
        if n > self.remaining() {
            return Err(ReadError::OutOfData);
        }
        let out = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N], ReadError> {
        let bytes = self.read_bytes(N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(bytes);
        Ok(out)
    }

    /// Reads one byte.
    pub fn read_u8(&mut self) -> Result<u8, ReadError> {
        Ok(self.read_array::<1>()?[0])
    }

    /// Reads a little-endian `u16`.
    pub fn read_u16_le(&mut self) -> Result<u16, ReadError> {
        Ok(u16::from_le_bytes(self.read_array()?))
    }

    /// Reads a little-endian `u32`.
    pub fn read_u32_le(&mut self) -> Result<u32, ReadError> {
        Ok(u32::from_le_bytes(self.read_array()?))
    }

    /// Reads a little-endian `u64`.
    pub fn read_u64_le(&mut self) -> Result<u64, ReadError> {
        Ok(u64::from_le_bytes(self.read_array()?))
    }
}
