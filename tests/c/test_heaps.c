/* test_heaps.c - C ABI checks for the buddy, TLSF, ring, slab, stack, byte buffer, error callback,
 * system memory and feature mask. Built and run by build_c_abi.bat. Returns the number of failures. */
#include <stdio.h>
#include <string.h>
#include "qs_memory.h"

static int failures = 0;
static int callbacks = 0;

#define CHECK(cond)                                                    \
    do {                                                               \
        if (!(cond)) {                                                 \
            printf("FAILED line %d: %s\n", __LINE__, #cond);           \
            failures++;                                                \
        }                                                              \
    } while (0)

static void count_error(uint8_t code) {
    (void)code;
    callbacks++;
}

int main(void) {
    /* Buddy: allocate, free, verify. */
    qs_buddy *buddy = NULL;
    uint8_t *p = NULL;
    CHECK(qs_buddy_create(4096, &buddy) == QS_OK);
    CHECK(qs_buddy_alloc(buddy, 100, 1, &p) == QS_OK && p != NULL);
    CHECK(qs_buddy_verify(buddy) == QS_OK);
    CHECK(qs_buddy_free(buddy, p, 100, 1) == QS_OK);
    CHECK(qs_buddy_free(buddy, p, 100, 1) != QS_OK); /* double free is refused */
    CHECK(qs_buddy_verify(buddy) == QS_OK);
    qs_buddy_destroy(buddy);

    /* TLSF: alignment beyond 16 bytes, usage counters, verify, misuse classification. */
    qs_tlsf *tlsf = NULL;
    uint8_t *a = NULL, *b = NULL;
    qs_tlsf_stats usage;
    uint8_t local = 0;
    CHECK(qs_tlsf_create(8192, &tlsf) == QS_OK);
    CHECK(qs_tlsf_alloc(tlsf, 100, 256, &a) == QS_OK);
    CHECK(((size_t)a % 256) == 0);
    CHECK(qs_tlsf_alloc(tlsf, 64, 16, &b) == QS_OK);
    CHECK(qs_tlsf_usage(tlsf, &usage) == QS_OK);
    CHECK(usage.used_blocks == 2);
    CHECK(qs_tlsf_free(tlsf, &local, 16, 16) == QS_NOT_OWNED);
    CHECK(qs_tlsf_free(tlsf, a, 100, 256) == QS_OK);
    CHECK(qs_tlsf_free(tlsf, a, 100, 256) == QS_DOUBLE_FREE); /* isolated block: exact */
    CHECK(qs_tlsf_free(tlsf, b, 64, 16) == QS_OK);
    CHECK(qs_tlsf_verify(tlsf) == QS_OK);
    CHECK(qs_tlsf_usage(tlsf, &usage) == QS_OK);
    CHECK(usage.free_blocks == 1 && usage.used_blocks == 0);
    qs_tlsf_destroy(tlsf);

    /* Ring: oldest-first release. */
    qs_ring *ring = NULL;
    uint8_t *r1 = NULL, *r2 = NULL;
    CHECK(qs_ring_create(256, &ring) == QS_OK);
    CHECK(qs_ring_alloc(ring, 32, 16, &r1) == QS_OK);
    CHECK(qs_ring_alloc(ring, 32, 16, &r2) == QS_OK);
    CHECK(qs_ring_free(ring, r2, 32, 16) == QS_OUT_OF_ORDER_FREE);
    CHECK(qs_ring_free(ring, r1, 32, 16) == QS_OK);
    CHECK(qs_ring_free(ring, r2, 32, 16) == QS_OK);
    qs_ring_destroy(ring);

    /* Slab: fixed-size objects. */
    qs_slab *slab = NULL;
    uint8_t *s1 = NULL;
    CHECK(qs_slab_create(64, 16, &slab) == QS_OK);
    CHECK(qs_slab_alloc(slab, 64, 16, &s1) == QS_OK);
    CHECK(qs_slab_free(slab, s1, 64, 16) == QS_OK);
    qs_slab_destroy(slab);

    /* Stack: LIFO release. */
    qs_stack *stack = NULL;
    uint8_t *k1 = NULL, *k2 = NULL;
    CHECK(qs_stack_create(1024, &stack) == QS_OK);
    CHECK(qs_stack_alloc(stack, 32, 8, &k1) == QS_OK);
    CHECK(qs_stack_alloc(stack, 32, 8, &k2) == QS_OK);
    CHECK(qs_stack_free(stack, k1, 32, 8) == QS_OUT_OF_ORDER_FREE);
    CHECK(qs_stack_free(stack, k2, 32, 8) == QS_OK);
    CHECK(qs_stack_free(stack, k1, 32, 8) == QS_OK);
    qs_stack_destroy(stack);

    /* Byte buffer: growth across several reallocations. */
    qs_bytes *bytes = NULL;
    size_t i;
    CHECK(qs_bytes_create(&bytes) == QS_OK);
    for (i = 0; i < 1000; i++) {
        CHECK(qs_bytes_push(bytes, (uint8_t)(i & 0xFF)) == QS_OK);
    }
    CHECK(qs_bytes_len(bytes) == 1000);
    CHECK(qs_bytes_data(bytes)[999] == (uint8_t)(999 & 0xFF));
    qs_bytes_destroy(bytes);

    /* Error callback: a foreign free reports through the hook. */
    CHECK(qs_set_error_hook(count_error) == QS_OK);
    CHECK(qs_tlsf_create(1024, &tlsf) == QS_OK);
    int before = callbacks;
    CHECK(qs_tlsf_free(tlsf, &local, 16, 16) == QS_NOT_OWNED);
    CHECK(callbacks > before);
    qs_tlsf_destroy(tlsf);
    CHECK(qs_set_error_hook(NULL) == QS_OK);

    /* System memory and features. */
    qs_memory_info info;
    CHECK(qs_system_memory(&info) == QS_OK);
    CHECK(info.total_physical > 0);
    CHECK((qs_features() & QS_FEATURE_TLSF) != 0);

    printf("C heap ABI: %s (%d failure(s))\n", failures ? "FAILED" : "all checks passed", failures);
    return failures;
}
