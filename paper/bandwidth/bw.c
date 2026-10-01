// SPDX-License-Identifier: Apache-2.0
//
// What one core can pull from memory at a given working-set size, so a scan's
// GB/s can be read against the ceiling the machine allows for the stream it
// walks. Two numbers per size, both sequential reads with eight independent
// accumulators, so one chain never measures its own latency:
//
//   cold_gbs  one pass over a buffer nothing has touched since a sweep of a
//             gigabyte pushed it out of every cache, so the read comes from
//             memory whatever the size is.
//   seq_gbs   the same buffer read again and again. Where the set fits in a
//             cache this is the rate that cache gives; where it does not, it
//             falls back to what the cold pass reads.
//
// The gap between the two is the data landing in cache: wide while the set
// fits, closed once it does not.
//
// The buffer is filled from a runtime seed and hidden behind an asm barrier,
// because a compiler that can see it is constant folds the whole loop away.
// A cold pass over the smallest size takes microseconds, which is why the
// clock is the raw one: `CLOCK_MONOTONIC` ticks at 1 us on Darwin.
//
// Writes `kib,cold_gbs,seq_gbs` to stdout. See README.md.

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#if defined(__aarch64__)
#include <arm_neon.h>
typedef uint16x8_t V;
#define VLOAD(p) vld1q_u16(p)
#define VADD(a, b) vaddq_u16(a, b)
#define VZERO vdupq_n_u16(0)
#define VSUM(a) ((int)vaddvq_u16(a))
#define LANES 8
#elif defined(__AVX512F__)
#include <immintrin.h>
typedef __m512i V;
#define VLOAD(p) _mm512_load_si512((const void *)(p))
#define VADD(a, b) _mm512_add_epi16(a, b)
#define VZERO _mm512_setzero_si512()
#define VSUM(a) ((int)_mm512_reduce_add_epi64(a))
#define LANES 32
#else
#include <immintrin.h>
typedef __m256i V;
#define VLOAD(p) _mm256_load_si256((const __m256i *)(p))
#define VADD(a, b) _mm256_add_epi16(a, b)
#define VZERO _mm256_setzero_si256()
#define VSUM(a) ((int)_mm256_extract_epi16(a, 0))
#define LANES 16
#endif

// The compiler may not assume anything about what `p` points at.
#define OPAQUE(p) __asm__ volatile("" : "+r"(p) : : "memory")
#define CONSUME(x) __asm__ volatile("" : : "r"(x) : "memory")

static double now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC_RAW, &t);
    return t.tv_sec + t.tv_nsec * 1e-9;
}

static uint64_t next(uint64_t *state) {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    return *state;
}

// Read through this much to evict a working set, rather than a cache
// maintenance instruction: `dc civac` cleans only to the point of coherency,
// which on Apple silicon leaves the line in the system level cache, and a
// buffer read back from there is faster than memory can serve.
static const size_t SCRUB = 1ull << 30;
// Bytes to move per warm measurement, so a small set is not timed on a
// handful of passes.
static const unsigned long long WORK = 2ull << 30;
static const int PASSES = 5;
// A cold pass is one traversal, microseconds at the smallest size, so it needs
// more tries than a warm one before the best of them settles.
static const int COLD_PASSES = 25;

static double read_pass(uint16_t *buf, size_t vecs, size_t reps) {
    V a0 = VZERO, a1 = VZERO, a2 = VZERO, a3 = VZERO;
    V a4 = VZERO, a5 = VZERO, a6 = VZERO, a7 = VZERO;
    uint16_t *p0 = buf;
    double t0 = now();
    for (size_t r = 0; r < reps; r++) {
        OPAQUE(p0);
        for (size_t i = 0; i < vecs; i += 8) {
            const uint16_t *p = p0 + i * LANES;
            a0 = VADD(a0, VLOAD(p + 0 * LANES));
            a1 = VADD(a1, VLOAD(p + 1 * LANES));
            a2 = VADD(a2, VLOAD(p + 2 * LANES));
            a3 = VADD(a3, VLOAD(p + 3 * LANES));
            a4 = VADD(a4, VLOAD(p + 4 * LANES));
            a5 = VADD(a5, VLOAD(p + 5 * LANES));
            a6 = VADD(a6, VLOAD(p + 6 * LANES));
            a7 = VADD(a7, VLOAD(p + 7 * LANES));
        }
    }
    double dt = now() - t0;
    int sink = VSUM(VADD(VADD(VADD(a0, a1), VADD(a2, a3)),
                         VADD(VADD(a4, a5), VADD(a6, a7))));
    CONSUME(sink);
    return dt;
}

static void sequential(uint16_t *buf, size_t n, uint16_t *scrub, double *cold,
                       double *warm) {
    size_t vecs = n / (LANES * 2), reps = (size_t)(WORK / n) + 2;
    double best_cold = 1e9, best_warm = 1e9;
    for (int t = 0; t < COLD_PASSES; t++) {
        read_pass(scrub, SCRUB / (LANES * 2), 1);
        double dt = read_pass(buf, vecs, 1);
        if (dt < best_cold) best_cold = dt;
    }
    // Those passes left the set as resident as it is going to get.
    for (int t = 0; t < PASSES; t++) {
        double dt = read_pass(buf, vecs, reps);
        if (dt < best_warm) best_warm = dt;
    }
    *cold = (double)n / best_cold / 1e9;
    *warm = (double)n * reps / best_warm / 1e9;
}

int main(void) {
    static const size_t KIB[] = {64,    256,   1024,   2048,   4096,  8192,
                                 16384, 65536, 262144, 524288, 1048576};
    uint64_t seed = 0x9e3779b97f4a7c15ull ^ (uint64_t)time(NULL);

    uint16_t *scrub = aligned_alloc(64, SCRUB);
    if (!scrub) return 1;
    memset(scrub, 1, SCRUB);

    printf("kib,cold_gbs,seq_gbs\n");
    for (unsigned s = 0; s < sizeof KIB / sizeof *KIB; s++) {
        size_t n = KIB[s] << 10;
        if ((n / (LANES * 2)) % 8) continue;
        uint16_t *buf = aligned_alloc(64, n);
        if (!buf) continue;
        for (size_t i = 0; i < n / 2; i++) buf[i] = (uint16_t)next(&seed);

        double cold, warm;
        sequential(buf, n, scrub, &cold, &warm);
        printf("%zu,%.1f,%.1f\n", KIB[s], cold, warm);
        fflush(stdout);
        free(buf);
    }
    free(scrub);
    return 0;
}
