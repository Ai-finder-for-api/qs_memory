//! Ordered map: key-sorted entries in an allocator-backed vector. Lookups are binary searches; inserts and
//! removals shift the tail, which suits maps that change rarely.

use crate::allocators::traits::Allocator;
use crate::containers::Vector;
use crate::foundation::error::AllocResult;

pub struct OrderedMap<'a, K: Ord, V, A: Allocator> {
    entries: Vector<'a, (K, V), A>,
}

impl<'a, K: Ord, V, A: Allocator> OrderedMap<'a, K, V, A> {
    /// An empty map whose storage comes from `alloc`.
    pub fn new(alloc: &'a A) -> AllocResult<Self> {
        Ok(OrderedMap { entries: Vector::new(alloc)? })
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn position(&self, key: &K) -> Result<usize, usize> {
        self.entries.as_slice().binary_search_by(|(k, _)| k.cmp(key))
    }

    /// Inserts `value` under `key`. Returns the previous value when the key was present.
    pub fn insert(&mut self, key: K, value: V) -> AllocResult<Option<V>> {
        match self.position(&key) {
            Ok(i) => Ok(Some(core::mem::replace(&mut self.entries.as_mut_slice()[i].1, value))),
            Err(i) => {
                self.entries.push((key, value))?;
                self.entries.as_mut_slice()[i..].rotate_right(1);
                Ok(None)
            }
        }
    }

    /// The value stored under `key`.
    pub fn get(&self, key: &K) -> Option<&V> {
        let i = self.position(key).ok()?;
        Some(&self.entries.as_slice()[i].1)
    }

    /// Mutable access to the value stored under `key`.
    pub fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        let i = self.position(key).ok()?;
        Some(&mut self.entries.as_mut_slice()[i].1)
    }

    /// True when `key` is present.
    pub fn contains_key(&self, key: &K) -> bool {
        self.position(key).is_ok()
    }

    /// Removes `key` and returns its value.
    pub fn remove(&mut self, key: &K) -> Option<V> {
        let i = self.position(key).ok()?;
        self.entries.as_mut_slice()[i..].rotate_left(1);
        let (_, value) = self.entries.pop()?;
        Some(value)
    }

    /// Entries in ascending key order.
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> + '_ {
        self.entries.as_slice().iter().map(|(k, v)| (k, v))
    }

    /// The smallest entry.
    pub fn first(&self) -> Option<(&K, &V)> {
        self.entries.as_slice().first().map(|(k, v)| (k, v))
    }

    /// The largest entry.
    pub fn last(&self) -> Option<(&K, &V)> {
        self.entries.as_slice().last().map(|(k, v)| (k, v))
    }
}
