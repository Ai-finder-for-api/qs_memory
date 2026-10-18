// qs_memory.hpp - C++ RAII wrapper over the qs_memory C ABI.
//
// Every handle is owned by exactly one C++ object. Objects are movable, not copyable, and release their
// handle in the destructor. Any failure throws qs::Error, which carries the status code and its message.
#ifndef QS_MEMORY_HPP
#define QS_MEMORY_HPP

#include "qs_memory.h"

#include <cstddef>
#include <cstdint>
#include <stdexcept>
#include <string>
#include <utility>

namespace qs {

class Error : public std::runtime_error {
public:
    explicit Error(uint8_t code)
        : std::runtime_error(reinterpret_cast<const char *>(qs_error_str(code))), code_(code) {}
    uint8_t code() const noexcept { return code_; }

private:
    uint8_t code_;
};

inline void check(uint8_t status) {
    if (status != QS_OK) {
        throw Error(status);
    }
}

// Common move-only behaviour for a handle of type H, released by `destroy`.
template <typename H, void (*destroy)(H *)>
class Handle {
public:
    Handle() noexcept = default;
    Handle(const Handle &) = delete;
    Handle &operator=(const Handle &) = delete;
    Handle(Handle &&other) noexcept : h_(std::exchange(other.h_, nullptr)) {}
    Handle &operator=(Handle &&other) noexcept {
        if (this != &other) {
            reset();
            h_ = std::exchange(other.h_, nullptr);
        }
        return *this;
    }
    ~Handle() { reset(); }

    H *get() const noexcept { return h_; }
    explicit operator bool() const noexcept { return h_ != nullptr; }

protected:
    H *h_ = nullptr;

    void reset() noexcept {
        if (h_ != nullptr) {
            destroy(h_);
            h_ = nullptr;
        }
    }
};

inline void destroy_arena(qs_arena *h) { qs_arena_destroy(h); }
inline void destroy_pool(qs_pool *h) { qs_pool_destroy(h); }
inline void destroy_freelist(qs_freelist *h) { qs_freelist_destroy(h); }
inline void destroy_buddy(qs_buddy *h) { qs_buddy_destroy(h); }
inline void destroy_tlsf(qs_tlsf *h) { qs_tlsf_destroy(h); }
inline void destroy_ring(qs_ring *h) { qs_ring_destroy(h); }
inline void destroy_slab(qs_slab *h) { qs_slab_destroy(h); }
inline void destroy_stack(qs_stack *h) { qs_stack_destroy(h); }
inline void destroy_bytes(qs_bytes *h) { qs_bytes_destroy(h); }

// Bump arena that owns its storage.
class Arena : public Handle<qs_arena, destroy_arena> {
public:
    explicit Arena(size_t capacity) { check(qs_arena_create(capacity, &h_)); }
    void *allocate(size_t size, size_t align) {
        uint8_t *p = nullptr;
        check(qs_arena_alloc(h_, size, align, &p));
        return p;
    }
    void reset() { check(qs_arena_reset(h_)); }
};

// Fixed-size object pool.
class Pool : public Handle<qs_pool, destroy_pool> {
public:
    Pool(size_t object_size, size_t object_align, size_t count) {
        check(qs_pool_create(object_size, object_align, count, &h_));
    }
    void *allocate(size_t size, size_t align) {
        uint8_t *p = nullptr;
        check(qs_pool_alloc(h_, size, align, &p));
        return p;
    }
    void release(void *p, size_t size, size_t align) {
        check(qs_pool_free(h_, static_cast<uint8_t *>(p), size, align));
    }
};

// Coalescing free-list heap.
class FreeList : public Handle<qs_freelist, destroy_freelist> {
public:
    explicit FreeList(size_t capacity) { check(qs_freelist_create(capacity, &h_)); }
    void *allocate(size_t size, size_t align) {
        uint8_t *p = nullptr;
        check(qs_freelist_alloc(h_, size, align, &p));
        return p;
    }
    void release(void *p, size_t size, size_t align) {
        check(qs_freelist_free(h_, static_cast<uint8_t *>(p), size, align));
    }
    void verify() const { check(qs_freelist_verify(h_)); }
};

// Buddy heap.
class Buddy : public Handle<qs_buddy, destroy_buddy> {
public:
    explicit Buddy(size_t capacity) { check(qs_buddy_create(capacity, &h_)); }
    void *allocate(size_t size, size_t align) {
        uint8_t *p = nullptr;
        check(qs_buddy_alloc(h_, size, align, &p));
        return p;
    }
    void release(void *p, size_t size, size_t align) {
        check(qs_buddy_free(h_, static_cast<uint8_t *>(p), size, align));
    }
    void verify() const { check(qs_buddy_verify(h_)); }
};

// TLSF heap.
class Tlsf : public Handle<qs_tlsf, destroy_tlsf> {
public:
    explicit Tlsf(size_t capacity) { check(qs_tlsf_create(capacity, &h_)); }
    void *allocate(size_t size, size_t align) {
        uint8_t *p = nullptr;
        check(qs_tlsf_alloc(h_, size, align, &p));
        return p;
    }
    void release(void *p, size_t size, size_t align) {
        check(qs_tlsf_free(h_, static_cast<uint8_t *>(p), size, align));
    }
    qs_tlsf_stats usage() const {
        qs_tlsf_stats u{};
        check(qs_tlsf_usage(h_, &u));
        return u;
    }
    void verify() const { check(qs_tlsf_verify(h_)); }
};

// Ring buffer, released oldest first.
class Ring : public Handle<qs_ring, destroy_ring> {
public:
    explicit Ring(size_t capacity) { check(qs_ring_create(capacity, &h_)); }
    void *allocate(size_t size, size_t align) {
        uint8_t *p = nullptr;
        check(qs_ring_alloc(h_, size, align, &p));
        return p;
    }
    void release(void *p, size_t size, size_t align) {
        check(qs_ring_free(h_, static_cast<uint8_t *>(p), size, align));
    }
};

// Slab cache.
class Slab : public Handle<qs_slab, destroy_slab> {
public:
    Slab(size_t object_size, size_t object_align) {
        check(qs_slab_create(object_size, object_align, &h_));
    }
    void *allocate(size_t size, size_t align) {
        uint8_t *p = nullptr;
        check(qs_slab_alloc(h_, size, align, &p));
        return p;
    }
    void release(void *p, size_t size, size_t align) {
        check(qs_slab_free(h_, static_cast<uint8_t *>(p), size, align));
    }
};

// LIFO stack, released newest first.
class Stack : public Handle<qs_stack, destroy_stack> {
public:
    explicit Stack(size_t capacity) { check(qs_stack_create(capacity, &h_)); }
    void *allocate(size_t size, size_t align) {
        uint8_t *p = nullptr;
        check(qs_stack_alloc(h_, size, align, &p));
        return p;
    }
    void release(void *p, size_t size, size_t align) {
        check(qs_stack_free(h_, static_cast<uint8_t *>(p), size, align));
    }
};

// Growable byte buffer on the system heap.
class Bytes : public Handle<qs_bytes, destroy_bytes> {
public:
    Bytes() { check(qs_bytes_create(&h_)); }
    void push_back(uint8_t byte) { check(qs_bytes_push(h_, byte)); }
    size_t size() const { return qs_bytes_len(h_); }
    const uint8_t *data() const { return qs_bytes_data(h_); }
    std::string str() const {
        return std::string(reinterpret_cast<const char *>(data()), size());
    }
};

// Process-wide error callback; pass nullptr to remove it.
inline void set_error_callback(qs_error_callback callback) { check(qs_set_error_hook(callback)); }

// System memory figures.
inline qs_memory_info system_memory() {
    qs_memory_info info{};
    check(qs_system_memory(&info));
    return info;
}

}  // namespace qs

#endif  // QS_MEMORY_HPP
