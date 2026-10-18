//! Small string: text up to `N` bytes lives inline in the value. Longer text spills into a buffer from the
//! allocator. Only valid UTF-8 is ever stored, so `as_str` is always valid.

use crate::allocators::traits::Allocator;
use crate::containers::Vector;
use crate::foundation::error::{AllocError, AllocResult};

pub struct SmallString<'a, const N: usize, A: Allocator> {
    alloc: &'a A,
    inline: [u8; N],
    len: usize,
    heap: Option<Vector<'a, u8, A>>,
}

impl<'a, const N: usize, A: Allocator> SmallString<'a, N, A> {
    /// An empty string whose spill buffer, if needed, comes from `alloc`.
    pub fn new(alloc: &'a A) -> Self {
        SmallString { alloc, inline: [0; N], len: 0, heap: None }
    }

    /// Length in bytes.
    pub fn len(&self) -> usize {
        self.len
    }

    /// True when the string holds no bytes.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// True while the text fits inline (no allocation has happened).
    pub fn is_inline(&self) -> bool {
        self.heap.is_none()
    }

    /// Appends `s`, spilling to the allocator when the inline buffer is full.
    pub fn push_str(&mut self, s: &str) -> AllocResult<()> {
        if self.heap.is_none() && self.len + s.len() <= N {
            self.inline[self.len..self.len + s.len()].copy_from_slice(s.as_bytes());
            self.len += s.len();
            return Ok(());
        }
        if self.heap.is_none() {
            let mut spill = Vector::new(self.alloc)?;
            spill.reserve(self.len + s.len())?;
            for &b in &self.inline[..self.len] {
                spill.push(b)?;
            }
            self.heap = Some(spill);
        }
        let heap = self.heap.as_mut().ok_or(AllocError::Corruption)?;
        for &b in s.as_bytes() {
            heap.push(b)?;
        }
        self.len += s.len();
        Ok(())
    }

    /// Appends one character.
    pub fn push(&mut self, c: char) -> AllocResult<()> {
        let mut buf = [0u8; 4];
        self.push_str(c.encode_utf8(&mut buf))
    }

    /// The contents as text.
    pub fn as_str(&self) -> &str {
        let bytes: &[u8] = match &self.heap {
            Some(heap) => heap.as_slice(),
            None => &self.inline[..self.len],
        };
        // SAFETY: only valid UTF-8 is ever appended (it comes from `&str` and `char`), and a spill copies those
        // bytes exactly, so the stored bytes are always valid UTF-8.
        unsafe { core::str::from_utf8_unchecked(bytes) }
    }

    /// Removes all text. A spill buffer, once allocated, is kept for reuse.
    pub fn clear(&mut self) {
        self.len = 0;
        if let Some(heap) = self.heap.as_mut() {
            while heap.pop().is_some() {}
        }
    }
}
