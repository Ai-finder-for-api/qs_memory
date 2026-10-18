# Decorators and containers

## Decorators (`src/decorators/`)

Each decorator borrows an upstream allocator and implements `Allocator` itself.

- `Guarded`: canary bytes before and after every block; `check` verifies them, and a release
  over an overwritten canary is refused with `Corruption`, leaving the block in place.
- `Tracking`: records live blocks with a tag; `summarize` groups them for leak reports. Refuses
  allocations when its table is full, so every block it hands out can be released.
- `Budget`: refuses requests that would exceed a live-byte limit.
- `FaultInjecting`: fails chosen calls on purpose, for testing error paths.
- `Fallback`: serves from a primary allocator (which must implement `Ownership`), then a
  secondary one. Releases are routed by ownership.
- `Synchronized`: serialises a single-threaded allocator behind a spin lock.

## Containers (`src/containers/`)

Each container takes its storage from any allocator.

- `Vector`: geometric growth, `Deref` to a slice.
- `SmallVector`: inline storage for `N` elements, spills to the allocator.
- `HashMap`: open addressing with linear probing and tombstones; FNV-1a hashing.
- `RingBuffer`: fixed capacity; `push` refuses when full instead of growing.
- `OwnedPtr`: a single owned value that releases its storage on drop.

Zero-sized element types are rejected with `InvalidArgument`.
