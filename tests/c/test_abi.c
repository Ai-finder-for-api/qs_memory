#include <stdio.h>
#include "qs_memory.h"

int main(void) {
    int failures = 0;
    uint8_t *p = NULL, *q = NULL, *o = NULL, *f = NULL;
    qs_arena *a = NULL;
    qs_pool *pool = NULL;
    qs_freelist *heap = NULL;

    if (qs_alloc(100, 16, &p) != QS_OK || p == NULL) failures++;
    if (qs_free(p, 100, 16) != QS_OK) failures++;

    if (qs_arena_create(256, &a) != QS_OK) failures++;
    if (qs_arena_alloc(a, 64, 16, &q) != QS_OK) failures++;
    if (qs_arena_alloc(a, 4096, 1, &q) != QS_CAPACITY_EXHAUSTED) failures++;
    if (qs_arena_destroy(a) != QS_OK) failures++;

    if (qs_pool_create(32, 16, 2, &pool) != QS_OK) failures++;
    if (qs_pool_alloc(pool, 32, 16, &o) != QS_OK) failures++;
    if (qs_pool_free(pool, o, 32, 16) != QS_OK) failures++;
    if (qs_pool_free(pool, o, 32, 16) != QS_DOUBLE_FREE) failures++;
    if (qs_pool_destroy(pool) != QS_OK) failures++;

    if (qs_freelist_create(4096, &heap) != QS_OK) failures++;
    if (qs_freelist_alloc(heap, 200, 16, &f) != QS_OK) failures++;
    if (qs_freelist_verify(heap) != QS_OK) failures++;
    if (qs_freelist_free(heap, f, 200, 16) != QS_OK) failures++;
    if (qs_freelist_verify(heap) != QS_OK) failures++;
    if (qs_freelist_destroy(heap) != QS_OK) failures++;

    printf("qs_memory %s: %s\n", (const char *)qs_memory_version(), (const char *)qs_error_str(QS_DOUBLE_FREE));
    if (failures) {
        printf("C ABI: %d failure(s)\n", failures);
    } else {
        printf("C ABI: all checks passed\n");
    }
    return failures;
}
