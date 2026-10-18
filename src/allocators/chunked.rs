//! Growable allocators built from chunks obtained on demand.
//!
//! `ChunkedArena`: bump allocation inside chunks. A request that does not fit starts a
//! new chunk (sized `max(chunk_bytes, request + alignment)`). Individual frees are not
//! supported. `reset` keeps the first chunk and releases the rest.
//!
//! `ChunkedPool`: fixed-size slots inside chunks. A full pool adds a chunk with the same
//! slot count. Individual frees are supported and validated by the owning chunk.
//!
//! Not `Sync`.

use core::cell::Cell;
use core::mem::{align_of, size_of};
use core::ptr;

use crate::foundation::align::{align_up, is_valid_alignment};
use crate::allocators::traits::Allocator;
use crate::foundation::block::Block;
use crate::foundation::capability::Capabilities;
use crate::config::{DEFAULT_ALIGNMENT, MAX_BLOCK_SIZE};
use crate::foundation::error::{AllocError, AllocResult};
use crate::allocators::pool::PoolAllocator;
use crate::foundation::stats::Stats;
use crate::platform;

/// Offset of the first usable byte inside a chunk (header rounded to 16).
const CHUNK_DATA_OFFSET: usize = 32;

#[repr(C)]
struct Chunk {
    next: *mut Chunk,
    capacity: usize,
    top: usize,
}

const _: () = assert!(size_of::<Chunk>() <= CHUNK_DATA_OFFSET);

/// Bump allocator that grows by chunks.
pub struct ChunkedArena {
    chunk_bytes: usize,
    head: Cell<*mut Chunk>,
    first: Cell<*mut Chunk>,
    chunk_count: Cell<usize>,
    reserved: Cell<usize>,
    used: Cell<usize>,
    stats: Cell<Stats>,
}

// SAFETY: the arena owns its chunks and has no thread affinity; it is not `Sync`.
unsafe impl Send for ChunkedArena {}

impl ChunkedArena {
    /// An arena whose chunks hold at least `chunk_bytes` each. Allocates the first chunk.
    pub fn create(chunk_bytes: usize) -> AllocResult<Self> {
        if chunk_bytes == 0 || chunk_bytes > MAX_BLOCK_SIZE {
            return Err(AllocError::InvalidArgument);
        }
        let arena = ChunkedArena {
            chunk_bytes,
            head: Cell::new(ptr::null_mut()),
            first: Cell::new(ptr::null_mut()),
            chunk_count: Cell::new(0),
            reserved: Cell::new(0),
            used: Cell::new(0),
            stats: Cell::new(Stats::default()),
        };
        let c = arena.new_chunk(chunk_bytes)?;
        arena.head.set(c);
        arena.first.set(c);
        arena.chunk_count.set(1);
        arena.reserved.set(chunk_bytes);
        Ok(arena)
    }

    /// Chunks currently held.
    pub fn chunk_count(&self) -> usize {
        self.chunk_count.get()
    }

    /// Bytes reserved across all chunks (usable area only).
    pub fn reserved_bytes(&self) -> usize {
        self.reserved.get()
    }

    /// Bytes handed out (requested sizes, excluding padding).
    pub fn used_bytes(&self) -> usize {
        self.used.get()
    }

    /// Current counters.
    pub fn stats(&self) -> Stats {
        self.stats.get()
    }

    fn with_stats(&self, f: impl FnOnce(&mut Stats)) {
        let mut s = self.stats.get();
        f(&mut s);
        self.stats.set(s);
    }

    fn new_chunk(&self, usable: usize) -> AllocResult<*mut Chunk> {
        let total = CHUNK_DATA_OFFSET.checked_add(usable).ok_or(AllocError::Overflow)?;
        let raw = platform::raw_allocate(total, DEFAULT_ALIGNMENT);
        if raw.is_null() {
            return Err(AllocError::OutOfMemory);
        }
        let c = raw as *mut Chunk;
        // SAFETY: `raw` holds at least a Chunk header.
        unsafe { ptr::write(c, Chunk { next: ptr::null_mut(), capacity: usable, top: 0 }) };
        Ok(c)
    }

    /// Tries to bump inside chunk `c`.
    fn bump(c: *mut Chunk, size: usize, align: usize) -> Option<*mut u8> {
        // SAFETY: `c` is a live chunk of this arena.
        unsafe {
            let base = (c as usize) + CHUNK_DATA_OFFSET;
            let aligned = align_up(base.checked_add((*c).top)?, align)?;
            let off = aligned - base;
            if off > (*c).capacity || size > (*c).capacity - off {
                return None;
            }
            (*c).top = off + size;
            Some(aligned as *mut u8)
        }
    }

    /// Releases every chunk except the first and empties the first.
    pub fn reset(&self) {
        let first = self.first.get();
        let mut c = self.head.get();
        while !c.is_null() && c != first {
            // SAFETY: each chunk after `first` was allocated in `new_chunk` and is released once.
            unsafe {
                let next = (*c).next;
                platform::raw_deallocate(c as *mut u8);
                c = next;
            }
        }
        if !first.is_null() {
            // SAFETY: `first` is the live first chunk.
            unsafe {
                (*first).next = ptr::null_mut();
                (*first).top = 0;
            }
            self.head.set(first);
            self.chunk_count.set(1);
            self.reserved.set(self.chunk_bytes);
        } else {
            self.chunk_count.set(0);
            self.reserved.set(0);
        }
        self.used.set(0);
        self.with_stats(|s| {
            s.deallocations += s.live_allocations();
            s.bytes_live = 0;
        });
    }
}

impl Drop for ChunkedArena {
    fn drop(&mut self) {
        let mut c = self.head.get();
        while !c.is_null() {
            // SAFETY: every chunk is released exactly once here.
            unsafe {
                let next = (*c).next;
                platform::raw_deallocate(c as *mut u8);
                c = next;
            }
        }
    }
}

impl Allocator for ChunkedArena {
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        if !is_valid_alignment(align) {
            self.with_stats(|s| s.on_failure());
            return Err(AllocError::InvalidAlignment);
        }
        if size == 0 {
            return Ok(Block::empty(align));
        }
        if size > MAX_BLOCK_SIZE {
            self.with_stats(|s| s.on_failure());
            return Err(AllocError::Overflow);
        }
        let head = self.head.get();
        if !head.is_null() {
            if let Some(p) = Self::bump(head, size, align) {
                self.used.set(self.used.get() + size);
                self.with_stats(|s| s.on_alloc(size));
                return Ok(Block::new(p, size, align));
            }
        }
        // Start a chunk large enough for this request and its alignment slack.
        let need = size.checked_add(align).ok_or_else(|| {
            self.with_stats(|s| s.on_failure());
            AllocError::Overflow
        })?;
        let usable = need.max(self.chunk_bytes);
        let c = match self.new_chunk(usable) {
            Ok(c) => c,
            Err(e) => {
                self.with_stats(|s| s.on_failure());
                return Err(e);
            }
        };
        // SAFETY: `c` is a live chunk we just created.
        unsafe { (*c).next = self.head.get() };
        self.head.set(c);
        self.chunk_count.set(self.chunk_count.get() + 1);
        self.reserved.set(self.reserved.get() + usable);
        let p = Self::bump(c, size, align).ok_or(AllocError::CapacityExhausted)?;
        self.used.set(self.used.get() + size);
        self.with_stats(|s| s.on_alloc(size));
        Ok(Block::new(p, size, align))
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        if block.is_empty() {
            return if block.ptr().is_null() { Ok(()) } else { Err(AllocError::InvalidArgument) };
        }
        if block.ptr().is_null() {
            return Err(AllocError::InvalidArgument);
        }
        Err(AllocError::NotSupported)
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::ALLOCATE | Capabilities::RESET | Capabilities::STATISTICS
    }
}

#[repr(C)]
struct PoolNode {
    next: *mut PoolNode,
    pool: PoolAllocator,
}

/// Fixed-size pool that adds chunks when full.
pub struct ChunkedPool {
    object_size: usize,
    object_align: usize,
    slots_per_chunk: usize,
    head: Cell<*mut PoolNode>,
    chunk_count: Cell<usize>,
    stats: Cell<Stats>,
}

// SAFETY: the pool owns its chunks and has no thread affinity; it is not `Sync`.
unsafe impl Send for ChunkedPool {}

impl ChunkedPool {
    /// A pool whose chunks each hold `slots_per_chunk` objects. Creates the first chunk.
    pub fn create(object_size: usize, object_align: usize, slots_per_chunk: usize) -> AllocResult<Self> {
        if object_size == 0 || slots_per_chunk == 0 {
            return Err(AllocError::InvalidArgument);
        }
        if !is_valid_alignment(object_align) {
            return Err(AllocError::InvalidAlignment);
        }
        let pool = ChunkedPool {
            object_size,
            object_align,
            slots_per_chunk,
            head: Cell::new(ptr::null_mut()),
            chunk_count: Cell::new(0),
            stats: Cell::new(Stats::default()),
        };
        pool.add_chunk()?;
        Ok(pool)
    }

    /// Chunks currently held.
    pub fn chunk_count(&self) -> usize {
        self.chunk_count.get()
    }

    /// Objects in use across all chunks.
    pub fn slots_in_use(&self) -> usize {
        let mut total = 0;
        let mut n = self.head.get();
        while !n.is_null() {
            // SAFETY: `n` is a live node of this pool.
            unsafe {
                total += (*n).pool.slots_in_use();
                n = (*n).next;
            }
        }
        total
    }

    /// Total slots across all chunks.
    pub fn slots_total(&self) -> usize {
        self.chunk_count.get() * self.slots_per_chunk
    }

    /// Current counters.
    pub fn stats(&self) -> Stats {
        self.stats.get()
    }

    fn with_stats(&self, f: impl FnOnce(&mut Stats)) {
        let mut s = self.stats.get();
        f(&mut s);
        self.stats.set(s);
    }

    fn add_chunk(&self) -> AllocResult<*mut PoolNode> {
        let pool = PoolAllocator::create(self.object_size, self.object_align, self.slots_per_chunk)?;
        let raw = platform::raw_allocate(size_of::<PoolNode>(), align_of::<PoolNode>());
        if raw.is_null() {
            return Err(AllocError::OutOfMemory);
        }
        let n = raw as *mut PoolNode;
        // SAFETY: `raw` is a fresh allocation sized and aligned for a PoolNode.
        unsafe { ptr::write(n, PoolNode { next: self.head.get(), pool }) };
        self.head.set(n);
        self.chunk_count.set(self.chunk_count.get() + 1);
        self.with_stats(|s| s.capacity_bytes += (self.object_size * self.slots_per_chunk) as u64);
        Ok(n)
    }
}

impl Drop for ChunkedPool {
    fn drop(&mut self) {
        let mut n = self.head.get();
        while !n.is_null() {
            // SAFETY: each node was written by `add_chunk`; its pool is dropped in place
            // exactly once, then the node storage is released.
            unsafe {
                let next = (*n).next;
                ptr::drop_in_place(ptr::addr_of_mut!((*n).pool));
                platform::raw_deallocate(n as *mut u8);
                n = next;
            }
        }
    }
}

impl Allocator for ChunkedPool {
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        let mut n = self.head.get();
        while !n.is_null() {
            // SAFETY: `n` is a live node of this pool.
            let node = unsafe { &*n };
            if node.pool.slots_free() > 0 {
                if let Ok(b) = node.pool.allocate(size, align) {
                    self.with_stats(|s| s.on_alloc(self.object_size));
                    return Ok(b);
                }
            }
            n = node.next;
        }
        let fresh = match self.add_chunk() {
            Ok(f) => f,
            Err(e) => {
                self.with_stats(|s| s.on_failure());
                return Err(e);
            }
        };
        // SAFETY: `fresh` is the live node just added.
        let node = unsafe { &*fresh };
        match node.pool.allocate(size, align) {
            Ok(b) => {
                self.with_stats(|s| s.on_alloc(self.object_size));
                Ok(b)
            }
            Err(e) => {
                self.with_stats(|s| s.on_failure());
                Err(e)
            }
        }
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        if block.is_empty() {
            return if block.ptr().is_null() { Ok(()) } else { Err(AllocError::InvalidArgument) };
        }
        if block.ptr().is_null() {
            return Err(AllocError::InvalidArgument);
        }
        let mut n = self.head.get();
        while !n.is_null() {
            // SAFETY: `n` is a live node of this pool.
            let node = unsafe { &*n };
            if node.pool.owns(block.ptr(), 1) {
                // SAFETY: the pointer lies in this chunk; the chunk validates it.
                let r = unsafe { node.pool.deallocate(block) };
                if r.is_ok() {
                    self.with_stats(|s| s.on_free(self.object_size));
                }
                return r;
            }
            n = node.next;
        }
        Err(AllocError::NotOwned)
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::ALLOCATE
            .union(Capabilities::INDIVIDUAL_FREE)
            .union(Capabilities::OWNERSHIP)
            .union(Capabilities::STATISTICS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn arena_grows_and_resets() {
        let a = ChunkedArena::create(256).unwrap();
        assert_eq!(a.chunk_count(), 1);
        let mut seen = HashSet::new();
        for _ in 0..40 {
            let b = a.allocate(50, 8).unwrap();
            assert_eq!(b.ptr() as usize & 7, 0);
            seen.insert(b.ptr() as usize);
        }
        assert_eq!(seen.len(), 40);
        assert!(a.chunk_count() > 1);
        a.reset();
        assert_eq!(a.chunk_count(), 1);
        assert_eq!(a.used_bytes(), 0);
        assert_eq!(a.stats().live_allocations(), 0);
    }

    #[test]
    fn arena_dedicated_chunk_for_large_request() {
        let a = ChunkedArena::create(128).unwrap();
        let big = a.allocate(5000, 64).unwrap();
        assert_eq!(big.ptr() as usize & 63, 0);
        assert_eq!(a.chunk_count(), 2);
        // SAFETY: the arena refuses individual frees by design; nothing is touched.
        unsafe { assert_eq!(a.deallocate(big), Err(AllocError::NotSupported)) };
    }

    #[test]
    fn pool_adds_chunks_and_frees_exactly() {
        let p = ChunkedPool::create(16, 8, 4).unwrap();
        assert_eq!(p.chunk_count(), 1);
        let mut held = Vec::new();
        for _ in 0..10 {
            held.push(p.allocate(16, 8).unwrap());
        }
        assert_eq!(p.chunk_count(), 3);
        assert_eq!(p.slots_in_use(), 10);
        assert_eq!(p.slots_total(), 12);
        // SAFETY: each block is released once; the repeat and foreign calls are refused.
        unsafe {
            p.deallocate(held[0]).unwrap();
            assert_eq!(p.deallocate(held[0]), Err(AllocError::DoubleFree));
            let mut local = 0u8;
            assert_eq!(
                p.deallocate(Block::from_raw_parts(&mut local as *mut u8, 16, 8)),
                Err(AllocError::NotOwned)
            );
            for b in held.iter().skip(1) {
                p.deallocate(*b).unwrap();
            }
        }
        assert_eq!(p.slots_in_use(), 0);
        assert_eq!(p.stats().live_allocations(), 0);
    }
}
