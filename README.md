# qs_memory

Memory-management foundation for Quantum Suite, written in Rust. Zero dependencies.

The crate always builds as a dynamic library, `qs_memory.dll` (with the import library
`qs_memory.dll.lib`) on Windows, and also as an ordinary Rust library.

- Allocation failures are values (`AllocError`); nothing panics on misuse it can detect.
- `unsafe` is confined to allocator internals and `src/sys.rs`; each block states its invariant.
- The crate uses `std` only for the panic runtime and test harness. The `no_std` decision
  was reversed because a `no_std` DLL must define its own panic handler, which conflicts
  with Rust programs and tests that link the library.

## Allocators

| Type | Notes |
|---|---|
| `SystemAllocator` | Aligned heap through the C runtime; thread-safe counters. |
| `Arena` / `ArenaScope` | Bump allocation, reset, LIFO checkpoints. Owned or borrowed storage. |
| `StackAllocator` | LIFO frees and exact checkpoint validation (serial numbers). |
| `PoolAllocator` | Fixed-size slots with occupancy bitmap (exact double-free detection). |
| `FreeListAllocator` | Coalescing first-fit heap, in-place `try_grow`, `verify()` invariant checker. |
| `BuddyAllocator` | Power-of-two blocks with an O(1) validation side table. |
| `SizeClassAllocator` | Nine pool classes plus an aligned fallback. |
| `SlabAllocator` | Page-aligned slabs with inline occupancy bitmaps. |
| `ChunkedArena` / `ChunkedPool` | Growable by chunks. |
| `PageAllocator` | Lazily committed pages over reserved virtual memory. |
| `DoubleEndedArena` | Scratch memory used from both ends. |

Decorators (`Guarded`, `Tracking`, `Budget`, `FaultInjecting`, `Fallback`, `Synchronized`)
wrap any allocator. Containers (`Vector`, `SmallVector`, `HashMap`, `RingBuffer`,
`OwnedPtr`) take their storage from any allocator. `IntrusiveList` links elements without
allocating. `util` provides histograms, telemetry, text reports, hex dumps, hashing and
`defer`.

## C ABI and the DLL

`include/qs_memory.h` declares the C interface (system heap, arenas, pools, free lists).
`cargo build --release` produces `target\release\qs_memory.dll`, and the 17 `qs_*`
functions are exported (checked with `dumpbin /exports`).

`tests/c/test_abi.c` exercises the ABI through the DLL. `tests/c/build_c_abi.bat` builds
and runs it with MSVC (`vcvarsall` is not used: the script sets the toolchain and SDK
paths explicitly, because the default SDK version was not the one installed).

## Testing

    cargo test                          # 97 unit tests and 5 integration tests
    cargo build --release               # qs_memory.dll
    tests\c\build_c_abi.bat             # C ABI through the DLL

Status: everything above passes on Windows (MSVC). The Linux branch of `src/sys.rs` is
written but not compiled yet.

## Not yet done

- Benchmarks and examples in Rust.
- A Linux build check.
- The full documentation set beyond this README.
