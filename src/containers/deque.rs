//! Double-ended queue over an allocator-backed ring buffer. Grows by doubling when full.

use crate::allocators::traits::Allocator;
use crate::containers::Vector;
use crate::foundation::error::AllocResult;

pub struct Deque<'a, T, A: Allocator> {
    alloc: &'a A,
    buf: Vector<'a, Option<T>, A>,
    head: usize,
    len: usize,
}

impl<'a, T, A: Allocator> Deque<'a, T, A> {
    /// An empty deque whose storage comes from `alloc`.
    pub fn new(alloc: &'a A) -> AllocResult<Self> {
        Ok(Deque { alloc, buf: Vector::new(alloc)?, head: 0, len: 0 })
    }

    /// Number of values.
    pub fn len(&self) -> usize {
        self.len
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn cap(&self) -> usize {
        self.buf.len()
    }

    fn grow(&mut self) -> AllocResult<()> {
        let old_cap = self.cap();
        let new_cap = (old_cap * 2).max(4);
        let mut next: Vector<'a, Option<T>, A> = Vector::new(self.alloc)?;
        next.reserve(new_cap)?;
        for _ in 0..new_cap {
            next.push(None)?;
        }
        for i in 0..self.len {
            let idx = (self.head + i) % old_cap;
            let value = self.buf.as_mut_slice()[idx].take();
            next.as_mut_slice()[i] = value;
        }
        self.buf = next;
        self.head = 0;
        Ok(())
    }

    /// Adds `value` at the back.
    pub fn push_back(&mut self, value: T) -> AllocResult<()> {
        if self.len == self.cap() {
            self.grow()?;
        }
        let idx = (self.head + self.len) % self.cap();
        self.buf.as_mut_slice()[idx] = Some(value);
        self.len += 1;
        Ok(())
    }

    /// Adds `value` at the front.
    pub fn push_front(&mut self, value: T) -> AllocResult<()> {
        if self.len == self.cap() {
            self.grow()?;
        }
        let cap = self.cap();
        self.head = (self.head + cap - 1) % cap;
        self.buf.as_mut_slice()[self.head] = Some(value);
        self.len += 1;
        Ok(())
    }

    /// Removes and returns the front value.
    pub fn pop_front(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        let value = self.buf.as_mut_slice()[self.head].take();
        self.head = (self.head + 1) % self.cap();
        self.len -= 1;
        value
    }

    /// Removes and returns the back value.
    pub fn pop_back(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        let idx = (self.head + self.len - 1) % self.cap();
        let value = self.buf.as_mut_slice()[idx].take();
        self.len -= 1;
        value
    }

    /// The value at position `i` (0 is the front).
    pub fn get(&self, i: usize) -> Option<&T> {
        if i >= self.len {
            return None;
        }
        self.buf.as_slice()[(self.head + i) % self.cap()].as_ref()
    }

    /// The front value.
    pub fn front(&self) -> Option<&T> {
        self.get(0)
    }

    /// The back value.
    pub fn back(&self) -> Option<&T> {
        if self.len == 0 {
            None
        } else {
            self.get(self.len - 1)
        }
    }

    /// Values from front to back.
    pub fn iter(&self) -> impl Iterator<Item = &T> + '_ {
        (0..self.len).filter_map(move |i| self.get(i))
    }
}
