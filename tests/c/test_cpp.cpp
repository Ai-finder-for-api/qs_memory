// test_cpp.cpp - checks for the qs_memory C++ wrapper: RAII release, move semantics, exceptions carrying
// status codes, and the error callback. Built and run by build_c_abi.bat. Returns the number of failures.
#include <cstdio>
#include <utility>

#include "qs_memory.hpp"

static int failures = 0;
static int callbacks = 0;

#define CHECK(cond)                                                    \
    do {                                                               \
        if (!(cond)) {                                                 \
            std::printf("FAILED line %d: %s\n", __LINE__, #cond);      \
            failures++;                                                \
        }                                                              \
    } while (0)

static void count_error(uint8_t) { callbacks++; }

int main() {
    // RAII: a TLSF heap is released when it leaves scope, and moves transfer ownership.
    {
        qs::Tlsf a(4096);
        void *p = a.allocate(128, 64);
        CHECK(reinterpret_cast<uintptr_t>(p) % 64 == 0);
        qs::Tlsf b(std::move(a));
        CHECK(!a);
        CHECK(static_cast<bool>(b));
        b.release(p, 128, 64);
        b.verify();
        CHECK(b.usage().used_blocks == 0);
    }

    // Exceptions carry the status code and its message.
    {
        qs::Ring ring(256);
        void *first = ring.allocate(32, 16);
        void *second = ring.allocate(32, 16);
        try {
            ring.release(second, 32, 16);  // out of order
            CHECK(false);
        } catch (const qs::Error &e) {
            CHECK(e.code() == QS_OUT_OF_ORDER_FREE);
            CHECK(std::string(e.what()).size() > 0);
        }
        ring.release(first, 32, 16);
        ring.release(second, 32, 16);
    }

    // Constructor failure throws instead of leaving a half-built object.
    try {
        qs::Pool bad(16, 3, 4);  // alignment 3 is not a power of two
        CHECK(false);
    } catch (const qs::Error &e) {
        CHECK(e.code() == QS_INVALID_ALIGNMENT);
    }

    // Byte buffer.
    {
        qs::Bytes bytes;
        for (int i = 0; i < 300; i++) {
            bytes.push_back(static_cast<uint8_t>('a' + (i % 26)));
        }
        CHECK(bytes.size() == 300);
        CHECK(bytes.str().substr(0, 3) == "abc");
    }

    // Error callback fires for a foreign release routed through the C ABI.
    qs::set_error_callback(count_error);
    {
        qs::Tlsf heap(1024);
        uint8_t local = 0;
        try {
            heap.release(&local, 16, 16);
            CHECK(false);
        } catch (const qs::Error &e) {
            CHECK(e.code() == QS_NOT_OWNED);
        }
    }
    CHECK(callbacks >= 1);
    qs::set_error_callback(nullptr);

    CHECK(qs::system_memory().total_physical > 0);

    std::printf("C++ wrapper: %s (%d failure(s))\n", failures ? "FAILED" : "all checks passed", failures);
    return failures;
}
