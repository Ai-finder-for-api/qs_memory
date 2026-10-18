//! Priority queue: a binary max-heap stored in an allocator-backed vector.

use crate::allocators::traits::Allocator;
use crate::containers::Vector;
use crate::foundation::error::AllocResult;

pub struct PriorityQueue<'a, T: Ord, A: Allocator> {
    heap: Vector<'a, T, A>,
}

impl<'a, T: Ord, A: Allocator> PriorityQueue<'a, T, A> {
    /// An empty queue whose storage comes from `alloc`.
    pub fn new(alloc: &'a A) -> AllocResult<Self> {
        Ok(PriorityQueue { heap: Vector::new(alloc)? })
    }

    /// Number of values.
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    /// The largest value, without removing it.
    pub fn peek(&self) -> Option<&T> {
        self.heap.as_slice().first()
    }

    /// Adds `value`.
    pub fn push(&mut self, value: T) -> AllocResult<()> {
        self.heap.push(value)?;
        let last = self.heap.len() - 1;
        self.sift_up(last);
        Ok(())
    }

    /// Removes and returns the largest value.
    pub fn pop(&mut self) -> Option<T> {
        let n = self.heap.len();
        if n == 0 {
            return None;
        }
        self.heap.as_mut_slice().swap(0, n - 1);
        let top = self.heap.pop();
        if !self.heap.is_empty() {
            self.sift_down(0);
        }
        top
    }

    fn sift_up(&mut self, mut i: usize) {
        let s = self.heap.as_mut_slice();
        while i > 0 {
            let parent = (i - 1) / 2;
            if s[i] > s[parent] {
                s.swap(i, parent);
                i = parent;
            } else {
                break;
            }
        }
    }

    fn sift_down(&mut self, mut i: usize) {
        let s = self.heap.as_mut_slice();
        let n = s.len();
        loop {
            let left = 2 * i + 1;
            let right = left + 1;
            let mut largest = i;
            if left < n && s[left] > s[largest] {
                largest = left;
            }
            if right < n && s[right] > s[largest] {
                largest = right;
            }
            if largest == i {
                break;
            }
            s.swap(i, largest);
            i = largest;
        }
    }
}
