# Allocators

Every allocator implements `Allocator`:

- `allocate(size, align) -> AllocResult<Block>`. A zero size succeeds with an empty block. A
  non-power-of-two alignment fails with `InvalidAlignment`.
- `unsafe deallocate(block) -> AllocResult<()>`. The block must be one the allocator returned,
  still live, and released once. The allocator refuses what it can check safely.
- `capabilities() -> Capabilities`. Callers check this instead of guessing.

| Allocator | Frees | Rollback / reset | Ownership check | Notes |
|---|---|---|---|---|
| `SystemAllocator` | yes | no | no | aligned heap via the C runtime; thread-safe counters |
| `Arena` | no | checkpoints, reset | yes | bump pointer; `ArenaScope` rolls back on drop |
| `StackAllocator` | LIFO only | exact checkpoints | yes | serial numbers reject stale checkpoints |
| `PoolAllocator` | yes | no | yes | fixed slots; occupancy bitmap catches double frees |
| `FreeListAllocator` | yes | no | yes | coalescing first fit; `try_grow`; `verify()` |
| `BuddyAllocator` | yes | no | no | power-of-two blocks; O(1) validation table |
| `SizeClassAllocator` | yes | no | yes | nine pool classes plus an aligned fallback |
| `SlabAllocator` | yes | `release_empty` | yes | page-aligned slabs with inline bitmaps |
| `ChunkedArena` | no | reset | no | grows by chunks |
| `ChunkedPool` | yes | no | yes | adds a chunk when full |
| `PageAllocator` | no | reset (decommits) | no | lazily committed pages over reserved address space |
| `DoubleEndedArena` | rollback per end | marks, reset | yes | scratch memory used from both ends |

## Choosing

- Per-frame or per-request scratch: `Arena` with `ArenaScope`, or `DoubleEndedArena`.
- Many objects of one size: `PoolAllocator`, or `ChunkedPool` when the count is unknown.
- Mixed sizes, long-running: `FreeListAllocator`. Inspect it with `fragmentation()`.
- Power-of-two sizes and bounded work per call: `BuddyAllocator`.
- Small objects of many sizes: `SizeClassAllocator`.
- Kernel-style object caches: `SlabAllocator`.
- Large reserved regions committed on demand: `PageAllocator`.
