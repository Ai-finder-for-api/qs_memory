# Architecture

qs_memory is organised by subsystem, the same way as the other Quantum Suite projects.

    include/qs_memory/qs_memory.h   C interface (the only public header for C callers)
    src/foundation/                 errors, alignment math, blocks, capabilities, counters
    src/allocators/                 one module per storage strategy + the Allocator contract
    src/decorators/                 behaviour added around any allocator
    src/containers/                 data structures whose storage comes from an allocator
    src/structures/                 bitmap and intrusive list
    src/util/                       diagnostics (histogram, telemetry, reports), hashing, defer
    src/ffi/                        the C ABI (status codes, handles, exported functions)
    src/platform/                   the only code that calls the operating system
    src/config/                     build-time constants
    tests/                          integration, property (randomized), negative (misuse), c (ABI)
    examples/                       runnable programs, one idea each
    benches/                        timing programs (run in release mode)
    docs/                           this documentation
    scripts/                        build, test and benchmark entry points

## Layering

`platform` is the bottom: raw aligned memory and reserved address space. `foundation` is
pure logic with no OS calls. `allocators` builds on both. `decorators` and `containers`
build on the `Allocator` trait only, so any allocator can be wrapped or used as storage.
`ffi` sits on top and exposes a flat C interface.

## Build artifacts

`cargo build --release` produces `qs_memory.dll` and its import library `qs_memory.dll.lib`
(Windows), plus an ordinary Rust library. The C header is `include/qs_memory/qs_memory.h`.
