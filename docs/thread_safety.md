# Thread safety

| Type | Guarantee |
|---|---|
| `SystemAllocator` | `Sync`: calls may come from any thread. Counters are relaxed atomics. |
| Every other allocator | not `Sync`. Shared use needs `Synchronized`. |
| `Synchronized<A>` | serialises calls to `A` behind a spin lock; `Sync` because access is locked. |
| `Tracking`, `Budget`, `Guarded`, ... | not `Sync`; wrap in `Synchronized` to share. |

Single-threaded allocators keep their bookkeeping in `Cell`s, so the compiler rejects sharing
them across threads. A move to another thread is allowed where the type is `Send`.

Recommended patterns:

1. One allocator per thread on hot paths.
2. `Synchronized` around a shared allocator when contention is low.
3. Pass allocators between threads only when no other thread uses them.

The integration test `synchronized_allocator_is_shared_across_threads` exercises the
locked path with three threads.
