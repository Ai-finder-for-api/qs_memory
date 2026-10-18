//! qs_memory: memory-management foundation for Quantum Suite.
//!
//! Zero dependencies. Builds as a dynamic library (`qs_memory.dll` on Windows) for C
//! callers, and as an ordinary Rust library. Layout:
//!
//! - `foundation`: error codes, alignment math, blocks, capabilities, counters.
//! - `allocators`: storage strategies, each implementing the `Allocator` contract.
//! - `decorators`: behaviour added around any allocator.
//! - `containers`: data structures whose storage comes from an allocator.
//! - `structures`: bitmaps and intrusive lists.
//! - `util`: diagnostics (histogram, telemetry, reports), hashing, `defer`.
//! - `ffi`: the C ABI declared in `include/qs_memory/qs_memory.h`.
//! - `platform`: the only code that talks to the operating system.
//! - `config`: build-time constants.

#![deny(unsafe_op_in_unsafe_fn)]

pub mod allocators;
pub mod config;
pub mod containers;
pub mod decorators;
pub mod ffi;
pub mod foundation;
pub mod structures;
pub mod util;
mod platform;

pub use allocators::arena::{Arena, ArenaScope, Checkpoint};
pub use allocators::buddy::{BuddyAllocator, BuddyStats};
pub use allocators::chunked::{ChunkedArena, ChunkedPool};
pub use allocators::double_ended::{DoubleEndedArena, Mark};
pub use allocators::freelist::{FreeListAllocator, FreeListFragmentation};
pub use allocators::page::{PageAllocator, VirtualRegion};
pub use allocators::pool::PoolAllocator;
pub use allocators::size_class::SizeClassAllocator;
pub use allocators::slab::SlabAllocator;
pub use allocators::stack::{StackAllocator, StackCheckpoint};
pub use allocators::system::SystemAllocator;
pub use allocators::traits::{make_owned, Allocator, OwnedBlock};
pub use containers::{FnvHasher, HashMap, OwnedPtr, RingBuffer, SmallVector, Vector};
pub use decorators::{Budget, FaultInjecting, Fallback, GuardReport, Guarded, Ownership, Synchronized};
pub use decorators::tracking::{TagSummary, TrackRecord, Tracking};
pub use foundation::block::Block;
pub use foundation::capability::Capabilities;
pub use foundation::error::{AllocError, AllocResult};
pub use foundation::stats::{AtomicStats, Stats};
pub use structures::bitmap::Bitmap;
pub use structures::intrusive::{IntrusiveList, Link, Linked};
pub use util::{defer, Defer, SizeHistogram, Telemetry};pub use allocators::bulk::BulkAllocator;
pub use allocators::resize::Resize;
pub use allocators::typed::AllocatorExt;
pub use decorators::leak::LeakCheck;
pub use decorators::location::{Located, SiteSummary};
pub use decorators::quarantine::Quarantine;
pub use decorators::watermark::{Peaks, Watermark};
pub use allocators::ring::RingAllocator;
pub use allocators::tlsf::Tlsf;
pub use containers::slot_map::{Handle, SlotMap};
pub use decorators::zeroing::Zeroing;
pub use allocators::introspect::LiveBlocks;
pub mod diagnostics;
pub use allocators::huge::HugeArena;
pub use allocators::sharded::Sharded;
pub use allocators::shared::SharedArena;
pub use allocators::thread_cache::ThreadCached;
pub use decorators::locked::PageLocked;
pub use decorators::sampled::{Sample, Sampled};
pub use decorators::timeline::{Event, Op, Timeline};
pub use foundation::hooks::{clear_error_hook, notify, set_error_hook, Reporting};
pub use diagnostics::scrubber::Scrubber;
pub use config::env::Settings;
pub use platform::memory::{system_memory, MemoryInfo};
pub use platform::huge::PageKind;
pub use platform::lock::{lock_pages, unlock_pages};

pub mod concurrency;

pub use allocators::small_cache::SmallCache;
pub use concurrency::epoch::{Epoch, Guard as EpochGuard};
pub use concurrency::queue::BoundedQueue;
pub use concurrency::rwlocked::RwLocked;
pub use concurrency::treiber::LockFreeStack;
pub use containers::bitset::BitSet;
pub use containers::byte_builder::{ByteBuilder, Cursor, ReadError};
pub use containers::deque::Deque;
pub use containers::hash_set::HashSet;
pub use containers::lru::LruCache;
pub use containers::ordered_map::OrderedMap;
pub use containers::priority_queue::PriorityQueue;
pub use containers::small_string::SmallString;
pub use util::checksum::{crc32, crc32_update, hash64};
pub use util::mem;
