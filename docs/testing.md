# Testing

Entry points (from the crate root):

    scripts\test_all.bat          unit, integration, property, negative tests, then the C ABI test
    scripts\build_release.bat     release build (qs_memory.dll)
    scripts\bench.bat             benchmarks (release mode)

Test layout (mirrors the sibling projects):

- Unit tests live beside the code (`#[cfg(test)]` modules), one group per file.
- `tests/integration/`: the public API as a downstream user sees it, one file per area.
- `tests/property/`: randomized model checks with fixed seeds. Each run compares the allocator
  against a reference model after every batch and must end with an empty heap.
- `tests/negative/`: misuse. Every allocator must refuse null, foreign, interior and repeated
  releases with a precise error and keep its state.
- `tests/c/`: the C ABI, built with MSVC against the DLL.

Sanitizers: not run yet. Miri or AddressSanitizer on the Rust port is planned; nothing has been verified under them.
runs; the Rust port has not been run under Miri or a sanitizer yet.
