//! Hash set built on the allocator-backed hash map.

use crate::allocators::traits::Allocator;
use crate::containers::HashMap;
use crate::foundation::error::AllocResult;
use core::hash::Hash;

pub struct HashSet<'a, K: Hash + Eq, A: Allocator> {
    map: HashMap<'a, K, u8, A>,
}

impl<'a, K: Hash + Eq, A: Allocator> HashSet<'a, K, A> {
    /// An empty set whose storage comes from `alloc`.
    pub fn new(alloc: &'a A) -> AllocResult<Self> {
        Ok(HashSet { map: HashMap::new(alloc)? })
    }

    /// Number of values.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Adds `key`. Returns true when it was not already present.
    pub fn insert(&mut self, key: K) -> AllocResult<bool> {
        Ok(self.map.insert(key, 0u8)?.is_none())
    }

    /// True when `key` is present.
    pub fn contains(&self, key: &K) -> bool {
        self.map.contains_key(key)
    }

    /// Removes `key`. Returns true when it was present.
    pub fn remove(&mut self, key: &K) -> bool {
        self.map.remove(key).is_some()
    }

    /// Values in unspecified order.
    pub fn iter(&self) -> impl Iterator<Item = &K> + '_ {
        self.map.iter().map(|(k, _)| k)
    }
}
