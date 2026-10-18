# Design decisions

1. **Rust, not C++.** The C++ prototype was a useful reference (its structure, error policy and
   test cases carried over), but the Quantum Suite direction is Rust.
2. **`std`, not `no_std`.** The crate first targeted `no_std`. A `no_std` DLL must define its
   own panic handler, and that handler conflicts with any Rust program or test that links the
   library. The crate now uses `std` only for the panic runtime and the test harness; allocation
   code is written against `core` and the platform calls.
3. **`deallocate` is `unsafe`.** Releasing a pointer the allocator never produced is undefined
   behaviour in Rust terms. Allocators still refuse what they can check safely (range,
   alignment, occupancy, tags) and report it as an error.
4. **Single-threaded by default.** Allocators hold their bookkeeping in `Cell`s, so they are
   not `Sync`. Thread-safe use goes through `Synchronized` or the system allocator.
5. **No third-party crates.** Operating-system calls are declared directly in `platform/`.
6. **Errors are values.** `AllocError` with stable numeric codes that match the C ABI. No
   panics on misuse that can be detected.
