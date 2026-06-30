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

#include "sensorwatch/sensorwatch.h"

#include <cstddef>
#include <cstdint>
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
class Error : public std::runtime_error {
public:
    explicit Error(sw_error_t code)
        : std::runtime_error(sw_error_string(code)), code_(code) {}

    sw_error_t code() const noexcept { return code_; }

private:
    sw_error_t code_;
};

/* Throw an Error for any non-SW_OK result; a no-op on success. */
inline void check(sw_error_t error) {
    if (error != SW_OK) {
        throw Error(error);
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

inline bool operator==(const Reading& a, const Reading& b) {
    return a.source == b.source && a.sensor == b.sensor &&
           a.reading == b.reading && a.unit == b.unit && a.type == b.type &&
           a.value == b.value && a.minimum == b.minimum &&
           a.maximum == b.maximum && a.average == b.average;
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
 * moved-from handle is left inert so the snapshot is freed exactly once. All
 * accessors are const and safe to call concurrently for a live snapshot.
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
    }

    Snapshot(Snapshot&& other) noexcept
        : ptr_(other.ptr_), count_(other.count_),
          source_(std::move(other.source_)), source_loaded_(other.source_loaded_) {
        other.ptr_ = nullptr;
    }

    Snapshot& operator=(Snapshot&& other) noexcept {
        if (this != &other) {
            reset();
            ptr_ = other.ptr_;
            count_ = other.count_;
            source_ = std::move(other.source_);
            source_loaded_ = other.source_loaded_;
            other.ptr_ = nullptr;
        }
        return *this;
    }

    Snapshot(const Snapshot&) = delete;
    Snapshot& operator=(const Snapshot&) = delete;

    ~Snapshot() { reset(); }

    /* Number of reading entries. */
    std::uint32_t size() const noexcept { return count_; }

    /*
     * The source/backend identity (e.g. "HWiNFO"), shared by every reading.
     * Empty for a zero-entry snapshot. Queried once and cached.
     */
    std::string source() const { return cached_source(); }

    /* Build the reading at index, throwing std::out_of_range if out of bounds. */
    Reading at(std::uint32_t index) const {
        if (index >= count_) {
            throw std::out_of_range("snapshot reading index out of range");
        }
        return build_reading(index);
    }

    /* Build the reading at index without bounds checking. */
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

    /* Forward iterator yielding Reading by value (range-for friendly). */
    class const_iterator {
    public:
        using iterator_category = std::input_iterator_tag;
        using value_type        = Reading;
        using difference_type    = std::ptrdiff_t;
        using pointer           = void;
        using reference         = Reading;

        const_iterator(const Snapshot* snapshot, std::uint32_t index) noexcept
            : snapshot_(snapshot), index_(index) {}

        Reading operator*() const { return (*snapshot_)[index_]; }

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
        const Snapshot* snapshot_;
        std::uint32_t index_;
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

    const std::string& cached_source() const {
        if (!source_loaded_) {
            source_ = (count_ == 0u)
                          ? std::string()
                          : detail::query_string(sw_snapshot_get_source_name, ptr_, 0u);
            source_loaded_ = true;
        }
        return source_;
    }

    Reading build_reading(std::uint32_t index) const {
        Reading r;
        r.source  = cached_source();
        r.sensor  = detail::query_string(sw_snapshot_get_sensor_name, ptr_, index);
        r.reading = detail::query_string(sw_snapshot_get_reading_name, ptr_, index);
        r.unit    = detail::query_string(sw_snapshot_get_unit, ptr_, index);

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
        return r;
    }

    sw_snapshot_t* ptr_ = nullptr;
    std::uint32_t count_ = 0;
    mutable std::string source_;
    mutable bool source_loaded_ = false;
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
