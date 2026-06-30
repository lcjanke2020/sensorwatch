/*
 * Unit test for the header-only C++ binding (include/sensorwatch/sensorwatch.hpp).
 *
 * Dependency-free on purpose: a plain main() that returns non-zero on failure and
 * is registered with ctest, so the C++ wrapper does not pull a C++ test framework
 * into the build. It does NOT use <cassert>, because the project's default build
 * type (RelWithDebInfo) defines NDEBUG and would compile asserts out -- failures
 * are counted explicitly instead.
 *
 * Coverage:
 *   - Error translation (code() + what() == sw_error_string) and check().
 *   - ReadingType folding of unrecognized codes to Unknown.
 *   - Move-only semantics, asserted structurally at compile time and exercised at
 *     runtime (under ASan locally) on the live path.
 *   - Non-Windows: Session construction throws Error(SW_ERR_UNSUPPORTED_PLATFORM).
 *   - Windows: a live snapshot (skipped cleanly when no sensor source is running)
 *     exercises size/source/at/operator[]/iteration/readings and out-of-range.
 */

#include "sensorwatch/sensorwatch.hpp"

#include <cmath>
#include <cstdint>
#include <cstdio>
#include <stdexcept>
#include <string>
#include <type_traits>
#include <utility>
#include <vector>

namespace sw = sensorwatch;

static int g_failures = 0;

static void report_failure(const char* expr, const char* file, int line) {
    std::fprintf(stderr, "FAIL %s:%d: %s\n", file, line, expr);
    ++g_failures;
}

#define CHECK(cond) ((cond) ? (void)0 : report_failure(#cond, __FILE__, __LINE__))
#define FAILURE(msg) report_failure((msg), __FILE__, __LINE__)

/* Structural move-only guarantees, independent of any platform/runtime state. */
static_assert(!std::is_copy_constructible<sw::Session>::value,
              "Session must be move-only");
static_assert(!std::is_copy_assignable<sw::Session>::value,
              "Session must be move-only");
static_assert(std::is_nothrow_move_constructible<sw::Session>::value,
              "Session must be nothrow move-constructible");
static_assert(std::is_nothrow_move_assignable<sw::Session>::value,
              "Session must be nothrow move-assignable");
static_assert(!std::is_copy_constructible<sw::Snapshot>::value,
              "Snapshot must be move-only");
static_assert(!std::is_copy_assignable<sw::Snapshot>::value,
              "Snapshot must be move-only");
static_assert(std::is_nothrow_move_constructible<sw::Snapshot>::value,
              "Snapshot must be nothrow move-constructible");
static_assert(std::is_nothrow_move_assignable<sw::Snapshot>::value,
              "Snapshot must be nothrow move-assignable");
static_assert(std::is_nothrow_constructible<sw::Error, sw_error_t>::value,
              "Error construction must be noexcept (holds the static error string)");
// source() is ref-qualified: a reference from an lvalue Snapshot, a by-value copy
// from an rvalue so `session.snapshot().source()` cannot dangle off the temporary.
static_assert(
    std::is_same<decltype(std::declval<sw::Snapshot&>().source()), const std::string&>::value,
    "source() on an lvalue Snapshot must return const std::string&");
static_assert(
    std::is_same<decltype(std::declval<sw::Snapshot>().source()), std::string>::value,
    "source() on an rvalue Snapshot must return std::string by value (no dangling)");

static void test_error_translation() {
    const sw::Error err(SW_ERR_SOURCE_UNAVAILABLE);
    CHECK(err.code() == SW_ERR_SOURCE_UNAVAILABLE);
    CHECK(std::string(err.what()) ==
          std::string(sw_error_string(SW_ERR_SOURCE_UNAVAILABLE)));

    // check() throws an Error carrying the code on failure...
    bool threw = false;
    try {
        sw::check(SW_ERR_INDEX_OUT_OF_RANGE);
    } catch (const sw::Error& caught) {
        threw = true;
        CHECK(caught.code() == SW_ERR_INDEX_OUT_OF_RANGE);
    }
    CHECK(threw);

    // ...and is a no-op on SW_OK.
    sw::check(SW_OK);
}

static void test_reading_type_folding() {
    CHECK(sw::to_reading_type(SW_READING_TYPE_NONE) == sw::ReadingType::None);
    CHECK(sw::to_reading_type(SW_READING_TYPE_TEMPERATURE) ==
          sw::ReadingType::Temperature);
    CHECK(sw::to_reading_type(SW_READING_TYPE_OTHER) == sw::ReadingType::Other);
    CHECK(sw::to_reading_type(SW_READING_TYPE_UNKNOWN) == sw::ReadingType::Unknown);
    // Codes this binding does not name fold to Unknown rather than escaping as a
    // raw out-of-enum cast (mirrors the Python binding's _missing_).
    CHECK(sw::to_reading_type(static_cast<sw_reading_type_t>(9)) ==
          sw::ReadingType::Unknown);
    CHECK(sw::to_reading_type(static_cast<sw_reading_type_t>(200)) ==
          sw::ReadingType::Unknown);
}

static void test_reading_equality() {
    // Reading equality stays reflexive even when a scalar is NaN (a raw double ==
    // would make a NaN-carrying Reading compare unequal to itself).
    sw::Reading a;
    a.sensor = "S";
    a.value = std::nan("");
    a.minimum = std::nan("");
    const sw::Reading b = a;
    CHECK(a == b);
    CHECK(a == a);
    CHECK(!(a != b));

    sw::Reading c = a;
    c.value = 1.0;  // a finite value differs from the NaN one
    CHECK(a != c);
}

#if defined(_WIN32)
// Run the live-snapshot assertions on an already-open session (taken by value).
// Once the session is open the source is present, so ANY sw::Error here is a real
// failure, not a skip -- this function swallows its own sw::Error as a FAILURE so a
// later SW_ERR_SOURCE_UNAVAILABLE can never masquerade as a skip in the caller.
static void run_live_checks(sw::Session session) {
    try {
        // Move-construct the session; the moved-from handle must be inert so the
        // session is closed exactly once (verified under ASan).
        sw::Session moved_session(std::move(session));
        sw::Snapshot snapshot = moved_session.snapshot();

        const std::uint32_t count = snapshot.size();
        std::printf("[test] live snapshot: %u readings from \"%s\"\n",
                    static_cast<unsigned>(count), snapshot.source().c_str());

        // Move-construct the snapshot; the moved-from handle must be inert too.
        sw::Snapshot moved_snapshot(std::move(snapshot));
        CHECK(moved_snapshot.size() == count);

        if (count == 0u) {
            std::printf("[test] zero readings; skipping per-entry checks\n");
            return;
        }

        CHECK(!moved_snapshot.source().empty());

        // at(0) is populated; operator[] agrees with at(); source is snapshot-wide.
        const sw::Reading first = moved_snapshot.at(0u);
        CHECK(first.source == moved_snapshot.source());
        CHECK(first == moved_snapshot[0u]);

        // Range/iteration visits exactly size() readings; exercise operator-> too.
        std::uint32_t iterated = 0;
        for (sw::Snapshot::const_iterator it = moved_snapshot.begin();
             it != moved_snapshot.end(); ++it) {
            CHECK(it->source == moved_snapshot.source());  // operator->
            ++iterated;
        }
        CHECK(iterated == count);

        // readings() materializes the same set.
        const std::vector<sw::Reading> all = moved_snapshot.readings();
        CHECK(static_cast<std::uint32_t>(all.size()) == count);
        CHECK(all.front() == first);

        // Out-of-bounds at() throws std::out_of_range; operator[] stays unchecked.
        bool out_of_range_threw = false;
        try {
            (void)moved_snapshot.at(count);
        } catch (const std::out_of_range&) {
            out_of_range_threw = true;
        }
        CHECK(out_of_range_threw);

        // Move-assignment frees the left-hand handle and adopts the right-hand one.
        sw::Snapshot reassigned = moved_session.snapshot();
        reassigned = std::move(moved_snapshot);
        CHECK(reassigned.size() == count);

        // The moved-from snapshot is observably empty, not a stale view over null.
        CHECK(moved_snapshot.size() == 0u);
        CHECK(moved_snapshot.source().empty());
        CHECK(moved_snapshot.begin() == moved_snapshot.end());
    } catch (const sw::Error& err) {
        FAILURE("live snapshot path threw an unexpected sw::Error");
        std::fprintf(stderr, "  unexpected code %d: %s\n",
                     static_cast<int>(err.code()), err.what());
    }
}

static void test_windows_live() {
    // The only legitimate skip is a genuinely absent source, and it surfaces
    // exactly at Session construction: the constructor maps the HWiNFO shared
    // memory eagerly and reports SW_ERR_SOURCE_UNAVAILABLE when it is not running
    // (src/sw_session.c). If that construction (evaluated as the argument below)
    // throws SOURCE_UNAVAILABLE we skip; once it succeeds, run_live_checks() treats
    // every later error as a failure, so nothing past acquisition can hide a bug.
    try {
        run_live_checks(sw::Session{});
    } catch (const sw::Error& err) {
        if (err.code() == SW_ERR_SOURCE_UNAVAILABLE) {
            std::printf("[test] SKIP live snapshot (source unavailable)\n");
        } else {
            FAILURE("Session construction failed with an unexpected sw::Error");
            std::fprintf(stderr, "  unexpected code %d: %s\n",
                         static_cast<int>(err.code()), err.what());
        }
    }
}
#else
static void test_unsupported_platform() {
    bool threw = false;
    try {
        sw::Session session;
        (void)session;  // only constructed for its (throwing) side effect
        FAILURE("Session construction should throw on a non-Windows platform");
    } catch (const sw::Error& err) {
        threw = true;
        CHECK(err.code() == SW_ERR_UNSUPPORTED_PLATFORM);
    }
    CHECK(threw);
}
#endif

int main() {
    test_error_translation();
    test_reading_type_folding();
    test_reading_equality();
#if defined(_WIN32)
    test_windows_live();
#else
    test_unsupported_platform();
#endif

    if (g_failures != 0) {
        std::fprintf(stderr, "%d check(s) failed\n", g_failures);
        return 1;
    }
    std::printf("all checks passed\n");
    return 0;
}
