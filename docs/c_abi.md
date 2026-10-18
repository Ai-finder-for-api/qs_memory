# C ABI

Header: `include/qs_memory/qs_memory.h`. Library: `qs_memory.dll` with import library
`qs_memory.dll.lib`. Every function returns a `uint8_t` status: `0` is success, anything else is
an `AllocError` code, and `qs_error_str(code)` gives its text.

Handles are opaque. Each `*_create` has a matching `*_destroy`. Pointers from `qs_alloc`
and the handle allocators are released with their matching free function.

Functions exported (checked with `dumpbin /exports`):

- `qs_memory_version`, `qs_error_str`
- `qs_alloc`, `qs_free` (system heap)
- `qs_arena_create`, `qs_arena_alloc`, `qs_arena_reset`, `qs_arena_destroy`
- `qs_pool_create`, `qs_pool_alloc`, `qs_pool_free`, `qs_pool_destroy`
- `qs_freelist_create`, `qs_freelist_alloc`, `qs_freelist_free`, `qs_freelist_verify`,
  `qs_freelist_destroy`

Every function checks its pointer arguments for null before use. A C caller that passes a
pointer the library did not hand out has undefined behaviour, as with any allocator API.

The C test in `tests/c/test_abi.c` covers the round trips and the misuse refusals. Run it
with `tests\c\build_c_abi.bat`.
