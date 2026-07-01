#ifndef SENSORWATCH_SENSORWATCH_HPP
#define SENSORWATCH_SENSORWATCH_HPP

/*
 * sensorwatch C++ binding (header-only, C++17) over the C ABI in
 * include/sensorwatch/sensorwatch.h.
 *
 * This is a thin RAII layer for C/C++ consumers; it ships no compiled artifact
 * and defines no ABI of its own. Include this header and link the C core --
 * either the static library built with SW_STATIC (so SW_API is undecorated) or
 * the DLL. The flat extern "C" ABI stays the boundary; this wrapper only *calls*
 * those functions, so its exceptions never cross back into C.
 *
 * It mirrors the shipped Python binding (sensorwatch.native, v0.2.0): move-only
 * Session / Snapshot handles closed/freed by RAII, a Reading value type, a
 * ReadingType enum that folds unrecognized categories to Unknown, and string
 * fields read through the ABI's length-query-then-copy contract. Every non-SW_OK
 * result becomes a sensorwatch::Error carrying the sw_error_t code and the
 * library's sw_error_string() text.
 */

/*
 * C++17 floor, enforced at the header so it holds for every consumer regardless
 * of build system or how the package was installed (the CMake sensorwatch::hpp
 * target's cxx_std_17 feature is only present when the *installing* toolchain had
 * a C++ compiler). _MSVC_LANG is checked because MSVC reports __cplusplus as
 * 199711L unless /Zc:__cplusplus is set.
 */
#if __cplusplus < 201703L && !(defined(_MSVC_LANG) && _MSVC_LANG >= 201703L)
#  error "sensorwatch.hpp requires C++17 or later (e.g. -std=c++17 or /std:c++17)"
#endif

#include "sensorwatch/sensorwatch.h"

#include <cmath>
#include <cstddef>
#include <cstdint>
#include <exception>
#include <iterator>
#include <stdexcept>
#include <string>
#include <utility>
#include <vector>

namespace sensorwatch {

/*
 * Source-neutral reading category, mirroring sw_reading_type_t. Unknown (255) is
 * the ABI's sentinel for a source category this binding version does not
 * recognize; see to_reading_type().
 */
enum class ReadingType : int {
    None        = SW_READING_TYPE_NONE,
    Temperature = SW_READING_TYPE_TEMPERATURE,
    Voltage     = SW_READING_TYPE_VOLTAGE,
    Fan         = SW_READING_TYPE_FAN,
    Current     = SW_READING_TYPE_CURRENT,
    Power       = SW_READING_TYPE_POWER,
    Clock       = SW_READING_TYPE_CLOCK,
    Usage       = SW_READING_TYPE_USAGE,
    Other       = SW_READING_TYPE_OTHER,
    Unknown     = SW_READING_TYPE_UNKNOWN,
};

/*
 * Map a raw sw_reading_type_t onto ReadingType. The C core already folds
 * unrecognized source categories to SW_READING_TYPE_UNKNOWN; this also keeps any
 * value the running ABI might add ahead of this binding inside the enum (never an
 * out-of-range cast), mirroring the Python binding's ReadingType._missing_.
 */
inline ReadingType to_reading_type(sw_reading_type_t type) noexcept {
    switch (type) {
        case SW_READING_TYPE_NONE:        return ReadingType::None;
        case SW_READING_TYPE_TEMPERATURE: return ReadingType::Temperature;
        case SW_READING_TYPE_VOLTAGE:     return ReadingType::Voltage;
        case SW_READING_TYPE_FAN:         return ReadingType::Fan;
        case SW_READING_TYPE_CURRENT:     return ReadingType::Current;
        case SW_READING_TYPE_POWER:       return ReadingType::Power;
        case SW_READING_TYPE_CLOCK:       return ReadingType::Clock;
        case SW_READING_TYPE_USAGE:       return ReadingType::Usage;
        case SW_READING_TYPE_OTHER:       return ReadingType::Other;
        case SW_READING_TYPE_UNKNOWN:     return ReadingType::Unknown;
        default:                          return ReadingType::Unknown;
    }
}

/*
 * A native sw_error_t surfaced as a C++ exception. Carries the numeric code() and
 * uses the library's own sw_error_string() text as what(). SW_OK is never thrown
 * (see check()).
 */
class Error : public std::exception {
public:
    explicit Error(sw_error_t code) noexcept
        : code_(code), what_(sw_error_string(code)) {}

    sw_error_t code() const noexcept { return code_; }

    // sw_error_string returns a static, process-lifetime string (never NULL), so
    // Error holds the pointer directly -- no allocation. Construction is therefore
    // noexcept, and even an out-of-memory error can be reported without check()
    // itself throwing std::bad_alloc instead of the Error.
    const char* what() const noexcept override { return what_; }

private:
    sw_error_t code_;
    const char* what_;
};

/* Throw an Error for any non-SW_OK result; a no-op on success. */
inline void check(sw_error_t error) {
    if (error != SW_OK) {
        throw Error(error);
    }
}

/*
 * Verify the loaded C core's ABI is compatible with the version this header was
 * compiled against, throwing Error(SW_ERR_VERSION_MISMATCH) otherwise. Pre-1.0 a
 * minor bump is breaking, so major.minor must match; from 1.0 on only the major
 * gates compatibility (mirrors the Python binding's load-time guard). It matters
 * most for a consumer linking the DLL, whose loaded core can drift from this
 * header; a static link is already pinned at link time. Session() calls it before
 * opening, and a DLL consumer can call it explicitly up front.
 */
inline void check_abi_compatibility() {
    const std::uint32_t runtime = sw_api_version();
    const std::uint32_t major = runtime / 10000u;
    const std::uint32_t minor = (runtime / 100u) % 100u;
    const bool compatible =
        major == SW_API_VERSION_MAJOR && (major >= 1u || minor == SW_API_VERSION_MINOR);
    if (!compatible) {
        throw Error(SW_ERR_VERSION_MISMATCH);
    }
}

/*
 * One reading entry, a snapshot-independent value copy (the strings are owned, so
 * a Reading outlives the Snapshot it came from). Fields and order mirror the
 * Python binding's Reading dataclass.
 */
struct Reading {
    std::string source;
    std::string sensor;
    std::string reading;
    std::string unit;
    ReadingType type = ReadingType::Unknown;
    double value   = 0.0;
    double minimum = 0.0;
    double maximum = 0.0;
    double average = 0.0;
};

namespace detail {
/*
 * Equality for the scalar fields that stays reflexive for NaN sensor values. Raw
 * == makes NaN != NaN, so a Reading carrying a NaN (the C core copies raw doubles
 * out of the source buffer) would not compare equal to itself.
 */
inline bool scalar_equal(double a, double b) noexcept {
    return a == b || (std::isnan(a) && std::isnan(b));
}
}  // namespace detail

inline bool operator==(const Reading& a, const Reading& b) {
    return a.source == b.source && a.sensor == b.sensor &&
           a.reading == b.reading && a.unit == b.unit && a.type == b.type &&
           detail::scalar_equal(a.value, b.value) &&
           detail::scalar_equal(a.minimum, b.minimum) &&
           detail::scalar_equal(a.maximum, b.maximum) &&
           detail::scalar_equal(a.average, b.average);
}

inline bool operator!=(const Reading& a, const Reading& b) { return !(a == b); }

namespace detail {

/*
 * Defensive upper bound on a single ABI string (matches the Python binding's
 * 64 KiB cap). ABI strings are bounded, sanitized UTF-8, so a larger required
 * length signals a fault rather than a value worth allocating for.
 */
constexpr std::size_t k_max_string_bytes = 64u * 1024u;

/* Pointer to any of the sw_snapshot_get_*_name / _unit accessors. */
using string_accessor = sw_error_t(SW_CALL*)(const sw_snapshot_t*, std::uint32_t,
                                             char*, std::size_t, std::size_t*);

/*
 * Read one string field via the ABI's length-query-then-copy contract. A first
 * call with (NULL, 0, &needed) reports the byte count including the NUL (always
 * >= 1) and returns SW_ERR_BUFFER_TOO_SMALL; a second call fills a buffer of that
 * size. Any other code from the length query is a real error.
 */
inline std::string query_string(string_accessor accessor,
                                const sw_snapshot_t* snapshot,
                                std::uint32_t index) {
    std::size_t needed = 0;
    sw_error_t err = accessor(snapshot, index, nullptr, 0, &needed);
    if (err != SW_ERR_BUFFER_TOO_SMALL) {
        // Surfaces NULL snapshot, index out of range, etc.; the length query
        // never legitimately returns SW_OK.
        check(err);
        throw Error(SW_ERR_INTERNAL);
    }
    if (needed > k_max_string_bytes) {
        throw Error(SW_ERR_CORRUPT_DATA);
    }
    std::string buffer(needed, '\0');
    check(accessor(snapshot, index, buffer.data(), buffer.size(), &needed));
    buffer.resize(needed - 1);  // drop the trailing NUL (needed includes it, >= 1)
    return buffer;
}

}  // namespace detail

/*
 * An immutable snapshot of all readings, owning its own data copy. Move-only: the
 * moved-from handle is left inert (and empty) so the snapshot is freed exactly
 * once. All accessors are const and hold no shared mutable state, so they are safe
 * to call concurrently for a live snapshot.
 */
class Snapshot {
public:
    /* Takes ownership of a snapshot returned by sw_snapshot_take(). */
    explicit Snapshot(sw_snapshot_t* snapshot) : ptr_(snapshot) {
        std::uint32_t count = 0;
        sw_error_t err = sw_snapshot_entry_count(ptr_, &count);
        if (err != SW_OK) {
            sw_snapshot_free(ptr_);  // do not leak on a failed construction
            ptr_ = nullptr;
            throw Error(err);
        }
        count_ = count;
        // The source is snapshot-wide; query it once here, eagerly. Doing it in the
        // constructor (rather than a lazy mutable cache) keeps every accessor
        // genuinely const and free of shared mutable state, so they are race-free
        // under concurrent use. Empty for a zero-entry snapshot (the source
        // accessor is index-gated, so there is no index to query).
        if (count_ > 0u) {
            try {
                source_ = detail::query_string(sw_snapshot_get_source_name, ptr_, 0u);
            } catch (...) {
                sw_snapshot_free(ptr_);  // still no leak if the source query fails
                ptr_ = nullptr;
                throw;
            }
        }
    }

    Snapshot(Snapshot&& other) noexcept
        : ptr_(other.ptr_), count_(other.count_), source_(std::move(other.source_)) {
        other.ptr_ = nullptr;
        other.count_ = 0;
        other.source_.clear();  // moved-from is observably empty: size()==0, source()==""
    }

    Snapshot& operator=(Snapshot&& other) noexcept {
        if (this != &other) {
            reset();
            ptr_ = other.ptr_;
            count_ = other.count_;
            source_ = std::move(other.source_);
            other.ptr_ = nullptr;
            other.count_ = 0;
            other.source_.clear();  // a moved-from std::string is valid but unspecified
        }
        return *this;
    }

    Snapshot(const Snapshot&) = delete;
    Snapshot& operator=(const Snapshot&) = delete;

    ~Snapshot() { reset(); }

    /* Number of reading entries. */
    std::uint32_t size() const noexcept { return count_; }

    /*
     * The source/backend identity (e.g. "HWiNFO"), shared by every reading. Empty
     * for a zero-entry snapshot, and queried once in the constructor. On an lvalue
     * Snapshot it is returned by reference (valid for the snapshot's lifetime); on
     * an rvalue -- e.g. session.snapshot().source() -- it is returned by value, so
     * the result can never dangle off a temporary.
     */
    const std::string& source() const& noexcept { return source_; }
    std::string source() const&& { return source_; }

    /* Build the reading at index, throwing std::out_of_range if out of bounds. */
    Reading at(std::uint32_t index) const {
        if (index >= count_) {
            throw std::out_of_range("snapshot reading index out of range");
        }
        return build_reading(index);
    }

    /*
     * The reading at index, without the std::out_of_range pre-check that at()
     * performs. Unlike std::vector::operator[], a past-the-end index is not
     * undefined behavior: the C accessors validate it, so an out-of-range access
     * surfaces as Error(SW_ERR_INDEX_OUT_OF_RANGE).
     */
    Reading operator[](std::uint32_t index) const { return build_reading(index); }

    /* Materialize all readings at once. */
    std::vector<Reading> readings() const {
        std::vector<Reading> out;
        out.reserve(count_);
        for (std::uint32_t i = 0; i < count_; ++i) {
            out.push_back(build_reading(i));
        }
        return out;
    }

    /*
     * Input iterator over the snapshot's Readings (range-for friendly). operator*
     * yields a Reading by value and operator-> returns a small proxy so `it->field`
     * works. The category is input, not forward: a by-value operator* cannot model
     * the LegacyForwardIterator reference requirements. It is default-constructible
     * (a singular iterator) and provides operator->, so it satisfies
     * LegacyInputIterator.
     */
    class const_iterator {
    public:
        // operator* is a prvalue, so operator-> hands back a proxy that owns the
        // Reading for the duration of the `it->member` access.
        struct arrow_proxy {
            Reading value;
            const Reading* operator->() const noexcept { return &value; }
        };

        using iterator_category = std::input_iterator_tag;
        using value_type        = Reading;
        using difference_type   = std::ptrdiff_t;
        using pointer           = arrow_proxy;
        using reference         = Reading;

        const_iterator() noexcept = default;
        const_iterator(const Snapshot* snapshot, std::uint32_t index) noexcept
            : snapshot_(snapshot), index_(index) {}

        Reading operator*() const { return (*snapshot_)[index_]; }
        arrow_proxy operator->() const { return arrow_proxy{(*snapshot_)[index_]}; }

        const_iterator& operator++() noexcept {
            ++index_;
            return *this;
        }
        const_iterator operator++(int) noexcept {
            const_iterator copy = *this;
            ++index_;
            return copy;
        }

        bool operator==(const const_iterator& other) const noexcept {
            return snapshot_ == other.snapshot_ && index_ == other.index_;
        }
        bool operator!=(const const_iterator& other) const noexcept {
            return !(*this == other);
        }

    private:
        const Snapshot* snapshot_ = nullptr;
        std::uint32_t index_ = 0;
    };

    const_iterator begin() const noexcept { return const_iterator(this, 0u); }
    const_iterator end() const noexcept { return const_iterator(this, count_); }

private:
    void reset() noexcept {
        if (ptr_ != nullptr) {
            sw_snapshot_free(ptr_);
            ptr_ = nullptr;
        }
    }

    Reading build_reading(std::uint32_t index) const {
        Reading r;
        // Read the scalars first: these validate `index` without allocating, so an
        // out-of-range operator[] throws Error(SW_ERR_INDEX_OUT_OF_RANGE) before any
        // string is built -- a deterministic error with no wasted allocation.
        sw_reading_type_t type = SW_READING_TYPE_UNKNOWN;
        check(sw_snapshot_get_reading_type(ptr_, index, &type));
        r.type = to_reading_type(type);

        double scalar = 0.0;
        check(sw_snapshot_get_value(ptr_, index, &scalar));
        r.value = scalar;
        check(sw_snapshot_get_minimum(ptr_, index, &scalar));
        r.minimum = scalar;
        check(sw_snapshot_get_maximum(ptr_, index, &scalar));
        r.maximum = scalar;
        check(sw_snapshot_get_average(ptr_, index, &scalar));
        r.average = scalar;

        r.source  = source_;
        r.sensor  = detail::query_string(sw_snapshot_get_sensor_name, ptr_, index);
        r.reading = detail::query_string(sw_snapshot_get_reading_name, ptr_, index);
        r.unit    = detail::query_string(sw_snapshot_get_unit, ptr_, index);
        return r;
    }

    sw_snapshot_t* ptr_ = nullptr;
    std::uint32_t count_ = 0;
    std::string source_;
};

/*
 * An open sensorwatch session for the default sensor source. Move-only; the
 * moved-from handle is left inert so the session is closed exactly once.
 * Construction opens the session and throws Error on failure -- including
 * Error(SW_ERR_UNSUPPORTED_PLATFORM) on non-Windows.
 */
class Session {
public:
    Session() {
        check_abi_compatibility();
        sw_session_t* session = nullptr;
        check(sw_session_open(&session));
        session_ = session;
    }

    Session(Session&& other) noexcept : session_(other.session_) {
        other.session_ = nullptr;
    }

    Session& operator=(Session&& other) noexcept {
        if (this != &other) {
            reset();
            session_ = other.session_;
            other.session_ = nullptr;
        }
        return *this;
    }

    Session(const Session&) = delete;
    Session& operator=(const Session&) = delete;

    ~Session() { reset(); }

    /* Take an immutable snapshot of all currently available readings. */
    Snapshot snapshot() {
        sw_snapshot_t* taken = nullptr;
        check(sw_snapshot_take(session_, &taken));
        return Snapshot(taken);
    }

private:
    void reset() noexcept {
        if (session_ != nullptr) {
            sw_session_close(session_);
            session_ = nullptr;
        }
    }

    sw_session_t* session_ = nullptr;
};

}  // namespace sensorwatch

#endif /* SENSORWATCH_SENSORWATCH_HPP */
