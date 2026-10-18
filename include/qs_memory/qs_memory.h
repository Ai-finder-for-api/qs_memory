/* qs_memory.h - C interface to the Quantum Suite memory library.
 *
 * The library is built as qs_memory.dll (Windows) with the import library qs_memory.dll.lib.
 * Define QS_MEMORY_BUILD only when compiling the library itself.
 *
 * Every function returns a status code as uint8_t: 0 means success, anything else is an error code
 * described by qs_error_str(). Handles are opaque: create them with the *_create functions and release
 * them with the matching *_destroy function. Pointers from *_alloc are released with *_free, passing the
 * same size and alignment.
 *
 * The C++ wrapper with RAII handles and exceptions is in qs_memory.hpp.
 */
#ifndef QS_MEMORY_H
#define QS_MEMORY_H

#include <stddef.h>
#include <stdint.h>

#if defined(_WIN32)
#  ifdef QS_MEMORY_BUILD
#    define QS_API __declspec(dllexport)
#  else
#    define QS_API __declspec(dllimport)
#  endif
#else
#  define QS_API
#endif

#ifdef __cplusplus
extern "C" {
#endif

typedef struct qs_arena qs_arena;
typedef struct qs_pool qs_pool;
typedef struct qs_freelist qs_freelist;
typedef struct qs_buddy qs_buddy;
typedef struct qs_tlsf qs_tlsf;
typedef struct qs_ring qs_ring;
typedef struct qs_slab qs_slab;
typedef struct qs_stack qs_stack;
typedef struct qs_bytes qs_bytes;

/* Status codes (match AllocError in the Rust source). */
enum qs_status {
    QS_OK = 0,
    QS_INVALID_ARGUMENT = 1,
    QS_INVALID_ALIGNMENT = 2,
    QS_OVERFLOW = 3,
    QS_OUT_OF_MEMORY = 4,
    QS_CAPACITY_EXHAUSTED = 5,
    QS_INVALID_FREE = 6,
    QS_DOUBLE_FREE = 7,
    QS_INVALID_CHECKPOINT = 8,
    QS_STALE_CHECKPOINT = 9,
    QS_OUT_OF_ORDER_FREE = 10,
    QS_NOT_SUPPORTED = 11,
    QS_CORRUPTION = 12,
    QS_NOT_OWNED = 13
};

/* Feature bits returned by qs_features(). */
enum qs_feature {
    QS_FEATURE_TLSF = 1u << 0,
    QS_FEATURE_RING = 1u << 1,
    QS_FEATURE_BUDDY = 1u << 2,
    QS_FEATURE_SLAB = 1u << 3,
    QS_FEATURE_STACK = 1u << 4,
    QS_FEATURE_SHARED_MEMORY = 1u << 5,
    QS_FEATURE_LARGE_PAGES = 1u << 6,
    QS_FEATURE_PAGE_LOCK = 1u << 7,
    QS_FEATURE_THREAD_CACHE = 1u << 8,
    QS_FEATURE_SHARDED = 1u << 9,
    QS_FEATURE_ZEROING = 1u << 10,
    QS_FEATURE_ERROR_HOOK = 1u << 11,
    QS_FEATURE_BYTES = 1u << 12,
    QS_FEATURE_SYSTEM_MEMORY = 1u << 13
};

/* Usage counters for a TLSF heap. */
typedef struct qs_tlsf_stats {
    size_t free_blocks;
    size_t used_blocks;
    size_t free_bytes;
    size_t used_bytes;
    size_t largest_free;
} qs_tlsf_stats;

/* System memory figures. */
typedef struct qs_memory_info {
    uint64_t total_physical;
    uint64_t available_physical;
    size_t page_size;
    size_t large_page_size;
} qs_memory_info;

/* Error callback: receives the status code of each error reported by the library. */
typedef void (*qs_error_callback)(uint8_t code);

/* Library version and error text. Both strings are NUL-terminated and must not be freed. */
QS_API const uint8_t *qs_memory_version(void);
QS_API const uint8_t *qs_error_str(uint8_t code);

/* Feature bits this build provides. */
QS_API uint32_t qs_features(void);

/* System heap. Release with qs_free using the same size and alignment. */
QS_API uint8_t qs_alloc(size_t size, size_t align, uint8_t **out);
QS_API uint8_t qs_free(uint8_t *p, size_t size, size_t align);

/* Bump arena owning its storage. Individual frees are not supported. */
QS_API uint8_t qs_arena_create(size_t capacity, qs_arena **out);
QS_API uint8_t qs_arena_alloc(qs_arena *arena, size_t size, size_t align, uint8_t **out);
QS_API uint8_t qs_arena_reset(qs_arena *arena);
QS_API uint8_t qs_arena_destroy(qs_arena *arena);

/* Fixed-size object pool. */
QS_API uint8_t qs_pool_create(size_t object_size, size_t object_align, size_t count, qs_pool **out);
QS_API uint8_t qs_pool_alloc(qs_pool *pool, size_t size, size_t align, uint8_t **out);
QS_API uint8_t qs_pool_free(qs_pool *pool, uint8_t *p, size_t size, size_t align);
QS_API uint8_t qs_pool_destroy(qs_pool *pool);

/* Coalescing free-list heap. */
QS_API uint8_t qs_freelist_create(size_t capacity, qs_freelist **out);
QS_API uint8_t qs_freelist_alloc(qs_freelist *heap, size_t size, size_t align, uint8_t **out);
QS_API uint8_t qs_freelist_free(qs_freelist *heap, uint8_t *p, size_t size, size_t align);
QS_API uint8_t qs_freelist_verify(const qs_freelist *heap);
QS_API uint8_t qs_freelist_destroy(qs_freelist *heap);

/* Buddy heap (power-of-two blocks). */
QS_API uint8_t qs_buddy_create(size_t capacity, qs_buddy **out);
QS_API uint8_t qs_buddy_alloc(qs_buddy *heap, size_t size, size_t align, uint8_t **out);
QS_API uint8_t qs_buddy_free(qs_buddy *heap, uint8_t *p, size_t size, size_t align);
QS_API uint8_t qs_buddy_verify(const qs_buddy *heap);
QS_API void qs_buddy_destroy(qs_buddy *heap);

/* TLSF heap (O(1) allocate and free). Misuse is classified exactly for blocks that are still block starts
 * or were freed and absorbed; see the Rust documentation for the full rules. */
QS_API uint8_t qs_tlsf_create(size_t capacity, qs_tlsf **out);
QS_API uint8_t qs_tlsf_alloc(qs_tlsf *heap, size_t size, size_t align, uint8_t **out);
QS_API uint8_t qs_tlsf_free(qs_tlsf *heap, uint8_t *p, size_t size, size_t align);
QS_API uint8_t qs_tlsf_usage(const qs_tlsf *heap, qs_tlsf_stats *out);
QS_API uint8_t qs_tlsf_verify(const qs_tlsf *heap);
QS_API uint8_t qs_tlsf_set_zero_on_free(const qs_tlsf *heap, uint8_t on);
QS_API void qs_tlsf_destroy(qs_tlsf *heap);

/* Ring buffer: blocks are released oldest first. */
QS_API uint8_t qs_ring_create(size_t capacity, qs_ring **out);
QS_API uint8_t qs_ring_alloc(qs_ring *ring, size_t size, size_t align, uint8_t **out);
QS_API uint8_t qs_ring_free(qs_ring *ring, uint8_t *p, size_t size, size_t align);
QS_API void qs_ring_destroy(qs_ring *ring);

/* Slab cache for fixed-size objects. */
QS_API uint8_t qs_slab_create(size_t object_size, size_t object_align, qs_slab **out);
QS_API uint8_t qs_slab_alloc(qs_slab *slab, size_t size, size_t align, uint8_t **out);
QS_API uint8_t qs_slab_free(qs_slab *slab, uint8_t *p, size_t size, size_t align);
QS_API void qs_slab_destroy(qs_slab *slab);

/* LIFO stack: blocks are released newest first. */
QS_API uint8_t qs_stack_create(size_t capacity, qs_stack **out);
QS_API uint8_t qs_stack_alloc(qs_stack *stack, size_t size, size_t align, uint8_t **out);
QS_API uint8_t qs_stack_free(qs_stack *stack, uint8_t *p, size_t size, size_t align);
QS_API void qs_stack_destroy(qs_stack *stack);

/* Growable byte buffer on the system heap. */
QS_API uint8_t qs_bytes_create(qs_bytes **out);
QS_API uint8_t qs_bytes_push(qs_bytes *bytes, uint8_t byte);
QS_API size_t qs_bytes_len(const qs_bytes *bytes);
QS_API const uint8_t *qs_bytes_data(const qs_bytes *bytes);
QS_API void qs_bytes_destroy(qs_bytes *bytes);

/* Error callback. Pass NULL to remove it. */
QS_API uint8_t qs_set_error_hook(qs_error_callback callback);

/* System memory. Returns QS_NOT_SUPPORTED when the OS does not answer. */
QS_API uint8_t qs_system_memory(qs_memory_info *out);

#ifdef __cplusplus
}
#endif

#endif /* QS_MEMORY_H */
