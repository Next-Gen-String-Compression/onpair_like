#pragma once

// 64 codes, the width of one mask word, as a GNU vector type: one register
// on AVX-512BW, two on AVX2, four on NEON, split by the compiler. Compare,
// AND, OR and shift are operators on it, and the compiler picks the per-set
// instruction, including the byte tricks x86 needs for an unsigned compare
// and a per-byte shift. Four things it cannot express stay per set below:
// the table lookup, the OR-reduce, the movemask, and the table broadcast the
// lookup reads. The build's flags decide the set, -march=native on the
// machine that will run it.

#if defined(__ARM_NEON)
#include <arm_neon.h>
#elif defined(__AVX512BW__) || defined(__AVX2__)
#include <immintrin.h>
#else
#error "the vector matchers need NEON, AVX-512BW or AVX2: build with -march=native"
#endif

#include <cstring>

#include "../scan.hpp"

namespace fsst::search::prefilter::scan::matcher {

using V = uint8_t __attribute__((vector_size(64)));

inline V load(const uint8_t* at) {
    V v;
    std::memcpy(&v, at, sizeof v);
    return v;
}

// 1 << (lane % 8) per lane of a 16-byte register.
alignas(16) constexpr uint8_t LANE_BIT[16] = {1, 2, 4, 8, 16, 32, 64, 128, 1, 2, 4, 8, 16, 32, 64, 128};

#if defined(__ARM_NEON)

union Lanes {
    V v;
    uint8x16_t q[4];
};

using Reg = uint8x16_t;

inline Reg splat(uint8_t code) { return vdupq_n_u8(code); }

inline Reg table(const uint8_t* bytes) { return vld1q_u8(bytes); }

inline V spread(Reg r) {
    Lanes out;
    out.q[0] = out.q[1] = out.q[2] = out.q[3] = r;
    return out.v;
}

// The same 16-byte table in every 128-bit lane; an index with bit 7 set
// reads zero, as it does under vpshufb.
inline V lookup(Reg t, V index) {
    Lanes x{index}, out;
    for (size_t i = 0; i < 4; ++i) out.q[i] = vqtbl1q_u8(t, x.q[i]);
    return out.v;
}

inline bool any(V a, V b) {
    Lanes u{a | b};
    return vmaxvq_u8(vorrq_u8(vorrq_u8(u.q[0], u.q[1]), vorrq_u8(u.q[2], u.q[3]))) != 0;
}

// One bit per lane, then three pairwise adds; both words leave in one store.
inline void movemask(uint64_t* bits, V a, V b) {
    const uint8x16_t lane_bit = vld1q_u8(LANE_BIT);
    auto half = [lane_bit](V h) {
        Lanes u{h};
        uint8x16_t lo = vpaddq_u8(vandq_u8(u.q[0], lane_bit), vandq_u8(u.q[1], lane_bit));
        uint8x16_t hi = vpaddq_u8(vandq_u8(u.q[2], lane_bit), vandq_u8(u.q[3], lane_bit));
        return vpaddq_u8(lo, hi);
    };
    vst1q_u64(bits, vreinterpretq_u64_u8(vpaddq_u8(half(a), half(b))));
}

#elif defined(__AVX512BW__)

using Reg = __m512i;

inline Reg splat(uint8_t code) { return _mm512_set1_epi8(static_cast<char>(code)); }

inline Reg table(const uint8_t* bytes) {
    return _mm512_broadcast_i32x4(_mm_loadu_si128(reinterpret_cast<const __m128i*>(bytes)));
}

inline V spread(Reg r) { return (V)r; }

inline V lookup(Reg t, V index) { return (V)_mm512_shuffle_epi8(t, (__m512i)index); }

inline bool any(V a, V b) {
    __m512i x = (__m512i)(a | b);
    return _mm512_test_epi8_mask(x, x) != 0;
}

inline void movemask(uint64_t* bits, V a, V b) {
    bits[0] = _mm512_movepi8_mask((__m512i)a);
    bits[1] = _mm512_movepi8_mask((__m512i)b);
}

#elif defined(__AVX2__)

union Lanes {
    V v;
    __m256i q[2];
};

using Reg = __m256i;

inline Reg splat(uint8_t code) { return _mm256_set1_epi8(static_cast<char>(code)); }

inline Reg table(const uint8_t* bytes) {
    return _mm256_broadcastsi128_si256(_mm_loadu_si128(reinterpret_cast<const __m128i*>(bytes)));
}

inline V spread(Reg r) {
    Lanes out;
    out.q[0] = out.q[1] = r;
    return out.v;
}

// The same 16-byte table in both 128-bit lanes; an index with bit 7 set
// reads zero.
inline V lookup(Reg t, V index) {
    Lanes x{index}, out;
    for (size_t i = 0; i < 2; ++i) out.q[i] = _mm256_shuffle_epi8(t, x.q[i]);
    return out.v;
}

inline bool any(V a, V b) {
    Lanes u{a | b};
    __m256i x = _mm256_or_si256(u.q[0], u.q[1]);
    return _mm256_testz_si256(x, x) == 0;
}

inline void movemask(uint64_t* bits, V a, V b) {
    auto word = [](V h) {
        Lanes u{h};
        return static_cast<uint32_t>(_mm256_movemask_epi8(u.q[0])) |
               static_cast<uint64_t>(static_cast<uint32_t>(_mm256_movemask_epi8(u.q[1]))) << 32;
    };
    bits[0] = word(a);
    bits[1] = word(b);
}

#endif

// Per pair of mask words: load 128 codes, hits on each 64, movemask both.
// An odd W ends on one word alone. Returns whether any word was written; a
// skipped one is zeroed. `hits` also gets the pointer its vector came from,
// for a lagged load.
template <bool SKIP_MOVEMASK_IF_NO_MATCH, size_t W, typename HitsOf>
bool words(const uint8_t* codes, Bits<W>& bits, HitsOf hits) {
    bool written = false;
    for (size_t pair = 0; pair < W / 2; ++pair) {
        const uint8_t* at = codes + pair * 128;
        V a = hits(load(at), at);
        V b = hits(load(at + 64), at + 64);
        if (SKIP_MOVEMASK_IF_NO_MATCH && !any(a, b)) {
            bits[2 * pair] = 0;
            bits[2 * pair + 1] = 0;
            continue;
        }
        written = true;
        movemask(&bits[2 * pair], a, b);
    }
    if constexpr (W % 2 == 1) {
        const uint8_t* at = codes + (W - 1) * 64;
        V a = hits(load(at), at);
        if (SKIP_MOVEMASK_IF_NO_MATCH && !any(a, a)) {
            bits[W - 1] = 0;
        } else {
            written = true;
            uint64_t pair[2];
            movemask(pair, a, a);
            bits[W - 1] = pair[0];
        }
    }
    return written;
}

}  // namespace fsst::search::prefilter::scan::matcher
