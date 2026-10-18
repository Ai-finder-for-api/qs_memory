//! Containers whose storage comes from an allocator.
//!
//! Each container borrows its allocator (`&'a A`), so the allocator must outlive it.
//! Growth and insertion report failures as `AllocResult`; nothing panics on exhaustion.
//! Zero-sized element types are rejected with `InvalidArgument`.
//!
//! Storage released from an arena-style allocator that cannot free individually stays
//! allocated until that allocator is reset.

use core::borrow::Borrow;
use core::fmt;
use core::hash::{Hash, Hasher};
use core::mem::{align_of, size_of, MaybeUninit};
use core::ops::{Deref, DerefMut};
use core::ptr::{self, NonNull};

use crate::allocators::traits::Allocator;
use crate::foundation::block::Block;
use crate::foundation::error::{AllocError, AllocResult};

fn check_element<T>() -> AllocResult<()> {
    if size_of::<T>() == 0 {
        Err(AllocError::InvalidArgument)
    } else {
        Ok(())
    }
}

// ----------------------------------------------------------------- Vector

/// Growable array with storage from `A`. Growth is geometric (minimum capacity 4).
pub struct Vector<'a, T, A: Allocator> {
    alloc: &'a A,
    ptr: *mut T,
    len: usize,
    cap: usize,
}

impl<'a, T, A: Allocator> Vector<'a, T, A> {
    /// An empty vector. Allocates nothing until the first push.
    pub fn new(alloc: &'a A) -> AllocResult<Self> {
        check_element::<T>()?;
        Ok(Vector { alloc, ptr: ptr::null_mut(), len: 0, cap: 0 })
    }

    /// Number of elements.
    pub fn len(&self) -> usize {
        self.len
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Allocated capacity in elements.
    pub fn capacity(&self) -> usize {
        self.cap
    }

    fn grow_to(&mut self, new_cap: usize) -> AllocResult<()> {
        let bytes = new_cap.checked_mul(size_of::<T>()).ok_or(AllocError::Overflow)?;
        let nb = self.alloc.allocate(bytes, align_of::<T>())?;
        let np = nb.ptr() as *mut T;
        if self.cap > 0 {
            // SAFETY: the old buffer holds `len` initialized elements and is distinct from `np`.
            unsafe {
                ptr::copy_nonoverlapping(self.ptr, np, self.len);
                let old = Block::from_raw_parts(self.ptr as *mut u8, self.cap * size_of::<T>(), align_of::<T>());
                // A refused release only leaks the old buffer; the elements are already moved.
                let _ = self.alloc.deallocate(old);
            }
        }
        self.ptr = np;
        self.cap = new_cap;
        Ok(())
    }

    /// Ensures room for at least `additional` more elements.
    pub fn reserve(&mut self, additional: usize) -> AllocResult<()> {
        let needed = self.len.checked_add(additional).ok_or(AllocError::Overflow)?;
        if needed <= self.cap {
            return Ok(());
        }
        self.grow_to(needed)
    }

    /// Appends `value`.
    pub fn push(&mut self, value: T) -> AllocResult<()> {
        if self.len == self.cap {
            let next = if self.cap == 0 { 4 } else { self.cap * 2 };
            self.grow_to(next)?;
        }
        // SAFETY: `len < cap`, so the slot is inside the buffer and uninitialized.
        unsafe { ptr::write(self.ptr.add(self.len), value) };
        self.len += 1;
        Ok(())
    }

    /// Removes and returns the last element.
    pub fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        // SAFETY: the element at `len` was initialized and is now logically removed.
        Some(unsafe { ptr::read(self.ptr.add(self.len)) })
    }

    /// The elements as a slice.
    pub fn as_slice(&self) -> &[T] {
        if self.len == 0 {
            &[]
        } else {
            // SAFETY: the first `len` elements are initialized and owned by `self`.
            unsafe { core::slice::from_raw_parts(self.ptr, self.len) }
        }
    }

    /// The elements as a mutable slice.
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        if self.len == 0 {
            &mut []
        } else {
            // SAFETY: as in `as_slice`, with exclusive access through `&mut self`.
            unsafe { core::slice::from_raw_parts_mut(self.ptr, self.len) }
        }
    }
}

impl<T, A: Allocator> Deref for Vector<'_, T, A> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T, A: Allocator> DerefMut for Vector<'_, T, A> {
    fn deref_mut(&mut self) -> &mut [T] {
        self.as_mut_slice()
    }
}

impl<T, A: Allocator> Drop for Vector<'_, T, A> {
    fn drop(&mut self) {
        // SAFETY: the first `len` elements are initialized; the buffer came from `alloc`.
        unsafe {
            ptr::drop_in_place(ptr::slice_from_raw_parts_mut(self.ptr, self.len));
            if self.cap > 0 {
                let _ = self.alloc.deallocate(Block::from_raw_parts(
                    self.ptr as *mut u8,
                    self.cap * size_of::<T>(),
                    align_of::<T>(),
                ));
            }
        }
    }
}

impl<T: fmt::Debug, A: Allocator> fmt::Debug for Vector<'_, T, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.as_slice().iter()).finish()
    }
}

// ------------------------------------------------------------- SmallVector

/// Vector with inline storage for `N` elements. Spills to `A` past `N`.
pub struct SmallVector<'a, T, const N: usize, A: Allocator> {
    alloc: &'a A,
    inline: [MaybeUninit<T>; N],
    heap: *mut T,
    len: usize,
    cap: usize,
}

impl<'a, T, const N: usize, A: Allocator> SmallVector<'a, T, N, A> {
    /// An empty vector using its inline storage.
    pub fn new(alloc: &'a A) -> AllocResult<Self> {
        check_element::<T>()?;
        // SAFETY: an array of `MaybeUninit` needs no initialization.
        let inline = unsafe { MaybeUninit::<[MaybeUninit<T>; N]>::uninit().assume_init() };
        Ok(SmallVector { alloc, inline, heap: ptr::null_mut(), len: 0, cap: N })
    }

    fn data(&self) -> *mut T {
        if self.heap.is_null() {
            self.inline.as_ptr() as *mut T
        } else {
            self.heap
        }
    }

    /// Number of elements.
    pub fn len(&self) -> usize {
        self.len
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// True while the elements still live in the inline buffer.
    pub fn is_inline(&self) -> bool {
        self.heap.is_null()
    }

    /// Appends `value`, spilling to the allocator when the inline buffer is full.
    pub fn push(&mut self, value: T) -> AllocResult<()> {
        if self.len == self.cap {
            let new_cap = self.cap.checked_mul(2).ok_or(AllocError::Overflow)?.max(4);
            let bytes = new_cap.checked_mul(size_of::<T>()).ok_or(AllocError::Overflow)?;
            let nb = self.alloc.allocate(bytes, align_of::<T>())?;
            let np = nb.ptr() as *mut T;
            // SAFETY: `np` holds `new_cap > len` slots; the old elements are moved, not copied twice.
            unsafe {
                ptr::copy_nonoverlapping(self.data(), np, self.len);
                if !self.heap.is_null() {
                    let old = Block::from_raw_parts(self.heap as *mut u8, self.cap * size_of::<T>(), align_of::<T>());
                    let _ = self.alloc.deallocate(old);
                }
            }
            self.heap = np;
            self.cap = new_cap;
        }
        // SAFETY: `len < cap`, so the slot is inside the active buffer and uninitialized.
        unsafe { ptr::write(self.data().add(self.len), value) };
        self.len += 1;
        Ok(())
    }

    /// Removes and returns the last element.
    pub fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        // SAFETY: the element at `len` was initialized and is now logically removed.
        Some(unsafe { ptr::read(self.data().add(self.len)) })
    }

    /// The elements as a slice.
    pub fn as_slice(&self) -> &[T] {
        if self.len == 0 {
            &[]
        } else {
            // SAFETY: the first `len` elements are initialized and owned by `self`.
            unsafe { core::slice::from_raw_parts(self.data(), self.len) }
        }
    }
}

impl<T, const N: usize, A: Allocator> Deref for SmallVector<'_, T, N, A> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T, const N: usize, A: Allocator> Drop for SmallVector<'_, T, N, A> {
    fn drop(&mut self) {
        // SAFETY: the first `len` elements are initialized; a heap buffer came from `alloc`.
        unsafe {
            ptr::drop_in_place(ptr::slice_from_raw_parts_mut(self.data(), self.len));
            if !self.heap.is_null() {
                let _ = self.alloc.deallocate(Block::from_raw_parts(
                    self.heap as *mut u8,
                    self.cap * size_of::<T>(),
                    align_of::<T>(),
                ));
            }
        }
    }
}

// ----------------------------------------------------------------- HashMap

/// FNV-1a hasher used by `HashMap`. Deterministic across runs.
#[derive(Clone, Copy)]
pub struct FnvHasher(u64);

impl Default for FnvHasher {
    fn default() -> Self {
        FnvHasher(0xCBF2_9CE4_8422_2325)
    }
}

impl Hasher for FnvHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01B3);
        }
    }
}

enum Slot<K, V> {
    Empty,
    Tombstone,
    Full(K, V),
}

fn hash_of<Q: Hash + ?Sized>(key: &Q) -> usize {
    let mut h = FnvHasher::default();
    key.hash(&mut h);
    h.finish() as usize
}

/// Open-addressed hash map with storage from `A`. Linear probing with tombstones; grows
/// when the load (live plus tombstones) passes 3/4. Keys and values must be `Hash`/`Eq`
/// and not zero-sized.
pub struct HashMap<'a, K, V, A: Allocator> {
    alloc: &'a A,
    slots: *mut Slot<K, V>,
    cap: usize,
    used: usize,
    live: usize,
}

impl<'a, K: Hash + Eq, V, A: Allocator> HashMap<'a, K, V, A> {
    /// An empty map. Allocates on first insert.
    pub fn new(alloc: &'a A) -> AllocResult<Self> {
        check_element::<K>()?;
        check_element::<V>()?;
        Ok(HashMap { alloc, slots: ptr::null_mut(), cap: 0, used: 0, live: 0 })
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.live
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// Slots in the table.
    pub fn capacity(&self) -> usize {
        self.cap
    }

    fn slot(&self, i: usize) -> &Slot<K, V> {
        // SAFETY: `i < cap` and the table holds `cap` initialized slots.
        unsafe { &*self.slots.add(i) }
    }

    fn slot_mut(&mut self, i: usize) -> &mut Slot<K, V> {
        // SAFETY: as in `slot`, with exclusive access through `&mut self`.
        unsafe { &mut *self.slots.add(i) }
    }

    fn find<Q>(&self, key: &Q) -> Option<usize>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        if self.cap == 0 {
            return None;
        }
        let mut i = hash_of(key) & (self.cap - 1);
        for _ in 0..self.cap {
            match self.slot(i) {
                Slot::Empty => return None,
                Slot::Full(k, _) if k.borrow() == key => return Some(i),
                _ => {}
            }
            i = (i + 1) & (self.cap - 1);
        }
        None
    }

    /// The value for `key`, if present.
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        match self.find(key) {
            Some(i) => match self.slot(i) {
                Slot::Full(_, v) => Some(v),
                _ => None,
            },
            None => None,
        }
    }

    /// Mutable access to the value for `key`, if present.
    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let i = self.find(key)?;
        match self.slot_mut(i) {
            Slot::Full(_, v) => Some(v),
            _ => None,
        }
    }

    /// True when `key` is present.
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.find(key).is_some()
    }

    fn rebuild(&mut self, new_cap: usize) -> AllocResult<()> {
        let bytes = new_cap.checked_mul(size_of::<Slot<K, V>>()).ok_or(AllocError::Overflow)?;
        let nb = self.alloc.allocate(bytes, align_of::<Slot<K, V>>())?;
        let fresh = nb.ptr() as *mut Slot<K, V>;
        // SAFETY: `fresh` holds `new_cap` slots; writing `Empty` initializes each one.
        unsafe {
            for i in 0..new_cap {
                ptr::write(fresh.add(i), Slot::Empty);
            }
        }
        let old = self.slots;
        let old_cap = self.cap;
        self.slots = fresh;
        self.cap = new_cap;
        self.used = 0;
        self.live = 0;
        if !old.is_null() {
            for i in 0..old_cap {
                // SAFETY: each old slot is initialized and read exactly once here.
                let s = unsafe { ptr::read(old.add(i)) };
                if let Slot::Full(k, v) = s {
                    self.place_new(k, v);
                }
            }
            // SAFETY: every old slot was moved out above; release the old block.
            let _ = unsafe {
                self.alloc.deallocate(Block::from_raw_parts(
                    old as *mut u8,
                    old_cap * size_of::<Slot<K, V>>(),
                    align_of::<Slot<K, V>>(),
                ))
            };
        }
        Ok(())
    }

    /// Places an entry known to be absent into the first empty slot of its probe path.
    fn place_new(&mut self, k: K, v: V) {
        let mut i = hash_of(&k) & (self.cap - 1);
        while !matches!(self.slot(i), Slot::Empty) {
            i = (i + 1) & (self.cap - 1);
        }
        *self.slot_mut(i) = Slot::Full(k, v);
        self.used += 1;
        self.live += 1;
    }

    /// Inserts or replaces. Returns the previous value when `key` was present.
    pub fn insert(&mut self, key: K, value: V) -> AllocResult<Option<V>> {
        if let Some(i) = self.find(&key) {
            return Ok(match self.slot_mut(i) {
                Slot::Full(_, v) => Some(core::mem::replace(v, value)),
                _ => None,
            });
        }
        if self.cap == 0 || (self.used + 1) * 4 > self.cap * 3 {
            let target = if self.cap == 0 {
                8
            } else if (self.live + 1) * 2 > self.cap {
                self.cap * 2
            } else {
                self.cap
            };
            self.rebuild(target)?;
        }
        self.place_new(key, value);
        Ok(None)
    }

    /// Removes `key`, returning its value.
    pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let i = self.find(key)?;
        match core::mem::replace(self.slot_mut(i), Slot::Tombstone) {
            Slot::Full(_, v) => {
                self.live -= 1;
                Some(v)
            }
            _ => None,
        }
    }

    /// Iterates over the entries in table order.
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        let slots: &[Slot<K, V>] = if self.cap == 0 {
            &[]
        } else {
            // SAFETY: the table holds `cap` initialized slots.
            unsafe { core::slice::from_raw_parts(self.slots, self.cap) }
        };
        slots.iter().filter_map(|s| match s {
            Slot::Full(k, v) => Some((k, v)),
            _ => None,
        })
    }
}

impl<K, V, A: Allocator> Drop for HashMap<'_, K, V, A> {
    fn drop(&mut self) {
        if self.slots.is_null() {
            return;
        }
        // SAFETY: every slot is initialized; the table came from `alloc`.
        unsafe {
            for i in 0..self.cap {
                ptr::drop_in_place(self.slots.add(i));
            }
            let _ = self.alloc.deallocate(Block::from_raw_parts(
                self.slots as *mut u8,
                self.cap * size_of::<Slot<K, V>>(),
                align_of::<Slot<K, V>>(),
            ));
        }
    }
}

// --------------------------------------------------------------- RingBuffer

/// Fixed-capacity FIFO queue. `push` refuses when full instead of growing, which suits
/// real-time producers.
pub struct RingBuffer<'a, T, A: Allocator> {
    alloc: &'a A,
    data: *mut MaybeUninit<T>,
    cap: usize,
    head: usize,
    len: usize,
}

impl<'a, T, A: Allocator> RingBuffer<'a, T, A> {
    /// A ring holding up to `capacity` elements.
    pub fn new(alloc: &'a A, capacity: usize) -> AllocResult<Self> {
        check_element::<T>()?;
        if capacity == 0 {
            return Err(AllocError::InvalidArgument);
        }
        let bytes = capacity.checked_mul(size_of::<T>()).ok_or(AllocError::Overflow)?;
        let nb = alloc.allocate(bytes, align_of::<T>())?;
        Ok(RingBuffer { alloc, data: nb.ptr() as *mut MaybeUninit<T>, cap: capacity, head: 0, len: 0 })
    }

    /// Elements currently held.
    pub fn len(&self) -> usize {
        self.len
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// True when full.
    pub fn is_full(&self) -> bool {
        self.len == self.cap
    }

    /// Appends `value`, or returns it back when the ring is full.
    pub fn push(&mut self, value: T) -> Result<(), T> {
        if self.is_full() {
            return Err(value);
        }
        let idx = (self.head + self.len) % self.cap;
        // SAFETY: `idx < cap` and the slot is currently unused.
        unsafe { (*self.data.add(idx)).write(value) };
        self.len += 1;
        Ok(())
    }

    /// Removes and returns the oldest element.
    pub fn pop(&mut self) -> Option<T> {
        if self.is_empty() {
            return None;
        }
        // SAFETY: the head slot holds an initialized element.
        let v = unsafe { (*self.data.add(self.head)).assume_init_read() };
        self.head = (self.head + 1) % self.cap;
        self.len -= 1;
        Some(v)
    }
}

impl<T, A: Allocator> Drop for RingBuffer<'_, T, A> {
    fn drop(&mut self) {
        while self.pop().is_some() {}
        // SAFETY: the buffer came from `alloc` with this capacity; all elements are gone.
        unsafe {
            let _ = self.alloc.deallocate(Block::from_raw_parts(
                self.data as *mut u8,
                self.cap * size_of::<T>(),
                align_of::<T>(),
            ));
        }
    }
}

// ----------------------------------------------------------------- OwnedPtr

/// A uniquely owned value stored in memory from `A`. Drops the value and returns the
/// storage when it goes out of scope.
pub struct OwnedPtr<'a, T, A: Allocator> {
    alloc: &'a A,
    ptr: NonNull<T>,
    block: Block,
}

impl<'a, T, A: Allocator> OwnedPtr<'a, T, A> {
    /// Moves `value` into storage from `alloc`.
    pub fn new(alloc: &'a A, value: T) -> AllocResult<Self> {
        check_element::<T>()?;
        let block = alloc.allocate(size_of::<T>(), align_of::<T>())?;
        let p = block.ptr() as *mut T;
        // SAFETY: the block is `size_of::<T>()` bytes with `align_of::<T>()` alignment.
        unsafe { ptr::write(p, value) };
        Ok(OwnedPtr { alloc, ptr: NonNull::new(p).ok_or(AllocError::OutOfMemory)?, block })
    }

    /// Moves the value out, releasing the storage.
    pub fn into_inner(self) -> T {
        let this = core::mem::ManuallyDrop::new(self);
        // SAFETY: the value is initialized and is moved out exactly once; the storage is
        // then released through the recorded block.
        unsafe {
            let v = ptr::read(this.ptr.as_ptr());
            let _ = this.alloc.deallocate(this.block);
            v
        }
    }
}

impl<T, A: Allocator> Deref for OwnedPtr<'_, T, A> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: the pointer refers to the initialized value owned by `self`.
        unsafe { self.ptr.as_ref() }
    }
}

impl<T, A: Allocator> DerefMut for OwnedPtr<'_, T, A> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: as in `deref`, with exclusive access through `&mut self`.
        unsafe { self.ptr.as_mut() }
    }
}

impl<T, A: Allocator> Drop for OwnedPtr<'_, T, A> {
    fn drop(&mut self) {
        // SAFETY: the value is initialized and dropped once; the storage is released once.
        unsafe {
            ptr::drop_in_place(self.ptr.as_ptr());
            let _ = self.alloc.deallocate(self.block);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::allocators::arena::Arena;
    use crate::allocators::system::SystemAllocator;

    #[test]
    fn vector_grows_and_drops_elements() {
        let sys = SystemAllocator::new();
        {
            let mut v: Vector<String, SystemAllocator> = Vector::new(&sys).unwrap();
            for i in 0..100 {
                v.push(format!("item{i}")).unwrap();
            }
            assert_eq!(v.len(), 100);
            assert_eq!(v[99], "item99");
            assert_eq!(v.pop().unwrap(), "item99");
            assert_eq!(v.len(), 99);
        }
        assert_eq!(sys.stats().live_allocations(), 0);
    }

    #[test]
    fn vector_on_arena_and_reserve() {
        let mut buf = [MaybeUninit::<u8>::uninit(); 4096];
        let arena = Arena::from_slice(&mut buf);
        let mut v: Vector<i32, Arena> = Vector::new(&arena).unwrap();
        v.reserve(10).unwrap();
        for i in 0..10 {
            v.push(i * i).unwrap();
        }
        assert_eq!(v.iter().sum::<i32>(), 285);
    }

    #[test]
    fn small_vector_stays_inline_then_spills() {
        let sys = SystemAllocator::new();
        let mut v: SmallVector<u32, 4, SystemAllocator> = SmallVector::new(&sys).unwrap();
        for i in 0..4 {
            v.push(i).unwrap();
        }
        assert!(v.is_inline());
        assert_eq!(sys.stats().live_allocations(), 0);
        v.push(4).unwrap();
        assert!(!v.is_inline());
        assert_eq!(v.iter().sum::<u32>(), 10);
        assert_eq!(v.pop(), Some(4));
        drop(v);
        assert_eq!(sys.stats().live_allocations(), 0);
    }

    #[test]
    fn hashmap_insert_get_replace_remove() {
        let sys = SystemAllocator::new();
        {
            let mut m: HashMap<String, i32, SystemAllocator> = HashMap::new(&sys).unwrap();
            assert_eq!(m.insert("a".to_string(), 1).unwrap(), None);
            assert_eq!(m.insert("a".to_string(), 2).unwrap(), Some(1));
            assert_eq!(m.get("a"), Some(&2));
            assert_eq!(m.len(), 1);
            assert_eq!(m.remove("a"), Some(2));
            assert_eq!(m.get("a"), None);
            assert!(m.is_empty());
        }
        assert_eq!(sys.stats().live_allocations(), 0);
    }

    #[test]
    fn hashmap_grows_and_keeps_everything() {
        let sys = SystemAllocator::new();
        let mut m: HashMap<u64, u64, SystemAllocator> = HashMap::new(&sys).unwrap();
        for i in 0..5000u64 {
            m.insert(i, i * 3).unwrap();
        }
        assert_eq!(m.len(), 5000);
        assert!(m.capacity() >= 5000);
        for i in 0..5000u64 {
            assert_eq!(m.get(&i), Some(&(i * 3)));
        }
        for i in (0..5000u64).step_by(2) {
            assert!(m.remove(&i).is_some());
        }
        assert_eq!(m.len(), 2500);
        assert_eq!(m.iter().count(), 2500);
        assert!(m.iter().all(|(k, v)| *v == *k * 3 && k % 2 == 1));
    }

    #[test]
    fn ring_buffer_is_fifo_and_refuses_when_full() {
        let sys = SystemAllocator::new();
        let mut rb: RingBuffer<i32, SystemAllocator> = RingBuffer::new(&sys, 4).unwrap();
        for i in 1..=4 {
            rb.push(i).unwrap();
        }
        assert!(rb.is_full());
        assert_eq!(rb.push(5), Err(5));
        assert_eq!(rb.pop(), Some(1));
        rb.push(5).unwrap();
        assert_eq!((rb.pop(), rb.pop(), rb.pop(), rb.pop()), (Some(2), Some(3), Some(4), Some(5)));
        assert_eq!(rb.pop(), None);
        assert_eq!(RingBuffer::<i32, SystemAllocator>::new(&sys, 0).err(), Some(AllocError::InvalidArgument));
    }

    #[test]
    fn owned_ptr_drops_value_and_releases_storage() {
        let sys = SystemAllocator::new();
        {
            let mut p = OwnedPtr::new(&sys, String::from("hello")).unwrap();
            p.push_str(" world");
            assert_eq!(p.len(), 11);
        }
        assert_eq!(sys.stats().live_allocations(), 0);
        let p = OwnedPtr::new(&sys, 42u32).unwrap();
        assert_eq!(p.into_inner(), 42);
        assert_eq!(sys.stats().live_allocations(), 0);
    }

    #[test]
    fn zero_sized_types_are_rejected() {
        let sys = SystemAllocator::new();
        assert_eq!(Vector::<(), SystemAllocator>::new(&sys).err(), Some(AllocError::InvalidArgument));
        assert_eq!(OwnedPtr::new(&sys, ()).err(), Some(AllocError::InvalidArgument));
    }
}
pub mod slot_map;

pub mod bitset;
pub mod byte_builder;
pub mod deque;
pub mod hash_set;
pub mod lru;
pub mod ordered_map;
pub mod priority_queue;
pub mod small_string;
