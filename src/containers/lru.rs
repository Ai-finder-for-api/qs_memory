//! Least-recently-used cache with a fixed capacity. Entries are kept in allocator-backed storage with an
//! index-linked list for recency order and a hash map for lookups. When full, the least recently used
//! entry is evicted and returned to the caller.

use crate::allocators::traits::Allocator;
use crate::containers::{HashMap, Vector};
use crate::foundation::error::{AllocError, AllocResult};
use core::hash::Hash;

const NIL: usize = usize::MAX;

struct Node<K, V> {
    key: K,
    value: V,
    prev: usize,
    next: usize,
}

pub struct LruCache<'a, K: Hash + Eq + Clone, V, A: Allocator> {
    index: HashMap<'a, K, usize, A>,
    nodes: Vector<'a, Node<K, V>, A>,
    head: usize,
    tail: usize,
    capacity: usize,
}

impl<'a, K: Hash + Eq + Clone, V, A: Allocator> LruCache<'a, K, V, A> {
    /// An empty cache holding at most `capacity` entries.
    pub fn new(alloc: &'a A, capacity: usize) -> AllocResult<Self> {
        if capacity == 0 {
            return Err(AllocError::InvalidArgument);
        }
        Ok(LruCache {
            index: HashMap::new(alloc)?,
            nodes: Vector::new(alloc)?,
            head: NIL,
            tail: NIL,
            capacity,
        })
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// True when `key` is cached. Does not change recency.
    pub fn contains(&self, key: &K) -> bool {
        self.index.contains_key(key)
    }

    fn unlink(&mut self, i: usize) {
        let (prev, next) = {
            let n = &self.nodes.as_slice()[i];
            (n.prev, n.next)
        };
        let s = self.nodes.as_mut_slice();
        if prev != NIL {
            s[prev].next = next;
        } else {
            self.head = next;
        }
        if next != NIL {
            s[next].prev = prev;
        } else {
            self.tail = prev;
        }
    }

    fn push_front(&mut self, i: usize) {
        let head = self.head;
        {
            let s = self.nodes.as_mut_slice();
            s[i].prev = NIL;
            s[i].next = head;
            if head != NIL {
                s[head].prev = i;
            }
        }
        self.head = i;
        if self.tail == NIL {
            self.tail = i;
        }
    }

    /// The value for `key`, marking it as most recently used.
    pub fn get(&mut self, key: &K) -> Option<&V> {
        let i = *self.index.get(key)?;
        self.unlink(i);
        self.push_front(i);
        Some(&self.nodes.as_slice()[i].value)
    }

    /// The value for `key` without changing recency.
    pub fn peek(&self, key: &K) -> Option<&V> {
        let i = *self.index.get(key)?;
        Some(&self.nodes.as_slice()[i].value)
    }

    /// Inserts or updates `key`. Returns the evicted `(key, value)` when a new key displaced the least
    /// recently used entry. Updating an existing key replaces its value and returns `None`.
    pub fn put(&mut self, key: K, value: V) -> AllocResult<Option<(K, V)>> {
        if let Some(&i) = self.index.get(&key) {
            self.nodes.as_mut_slice()[i].value = value;
            self.unlink(i);
            self.push_front(i);
            return Ok(None);
        }
        if self.nodes.len() < self.capacity {
            let i = self.nodes.len();
            self.nodes.push(Node { key: key.clone(), value, prev: NIL, next: NIL })?;
            if let Err(e) = self.index.insert(key, i) {
                self.nodes.pop();
                return Err(e);
            }
            self.push_front(i);
            return Ok(None);
        }
        let i = self.tail;
        self.unlink(i);
        let old_key = self.nodes.as_slice()[i].key.clone();
        self.index.remove(&old_key);
        self.index.insert(key.clone(), i)?;
        let old = core::mem::replace(
            &mut self.nodes.as_mut_slice()[i],
            Node { key: key.clone(), value, prev: NIL, next: NIL },
        );
        self.push_front(i);
        Ok(Some((old.key, old.value)))
    }
}
