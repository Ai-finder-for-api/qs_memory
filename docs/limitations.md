# Limitations

- Windows (MSVC) is the only platform built and tested. The Linux branch of `platform/` is
  written but not compiled yet.
- Arenas, chunked arenas and page allocators cannot free single blocks; they reclaim memory in
  bulk with `reset` or checkpoints.
- The free list is first fit with a linear scan, so allocation time grows with the number of
  free blocks. The benchmark numbers show this clearly under churn.
- The buddy heap rounds requests up to powers of two and refuses requests a tighter policy
  could satisfy.
- Pointer validation for the size-class fallback and for the guard reads memory just before
  the pointer. Passing a pointer the allocator never returned is undefined behaviour when that
  memory is unreadable.
- `HashMap` uses FNV-1a and linear probing; it is not hardened against adversarial keys.
- The Rust port has not yet been run under Miri or a sanitizer.
- Benchmarks are indicative: one machine, one run per configuration.
