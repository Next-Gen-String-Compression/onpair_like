# FetchContent PATCH_COMMAND script for FSST-LIKE-Matching: the ONLY source
# deviations from the pinned upstream (b1eb3ab9). Every patch below is applied
# via an explicit, independent string(FIND)/string(REPLACE) pair — deliberately
# NOT via a CMake list + foreach(), because these match strings contain literal
# `;` (end of a C++ statement); CMake lists are `;`-joined strings, so
# foreach() over a list silently re-splits any element containing one,
# truncating it. (Discovered the hard way: a 6-tab template fragment produced
# by that split re-matched inside an already-patched 7-tab line, since 6 tabs
# is a shifted substring of 7 tabs — a double "ull" suffix on one line only.)
# Each block below is self-contained and naturally idempotent: once applied,
# its unsuffixed match text no longer exists to re-match on a later reconfigure.

# --- Patch 1: LLVM 14 compat -------------------------------------------------
# Upstream targets LLVM 16, where `LLJIT::lookup()` yields an `ExecutorAddr`
# with `toPtr<T>()`; on LLVM 14 that accessor does not exist
# (`JITEvaluatedSymbol::getAddress()` + cast is the spelling). Wrap that one
# line in an LLVM_VERSION_MAJOR guard so the pin builds on LLVM 14–16 unchanged.
set(LLVM_FILE src/codegen/llvmcodegen.cpp)
file(READ ${LLVM_FILE} llvm_content)
# Check the "already patched" marker FIRST: LLVM_ORIG stays present verbatim
# INSIDE the #if branch after patching, so searching for it directly (without
# checking this guard first) would match forever and re-wrap on every
# reconfigure — CMake reruns PATCH_COMMAND on every reconfigure of an
# already-populated FetchContent source, not just on first clone.
string(FIND "${llvm_content}" "LLVM_VERSION_MAJOR >= 15" llvm_already)
if(llvm_already EQUAL -1)
    set(LLVM_ORIG "    parseFunction = cantFail(this->JIT->lookup(Compiler::getParseFunctionName())).toPtr<bool (*) (const uint8_t*, size_t)>();")
    string(FIND "${llvm_content}" "${LLVM_ORIG}" llvm_pos)
    if(NOT llvm_pos EQUAL -1)
        set(LLVM_REPL "#if LLVM_VERSION_MAJOR >= 15
${LLVM_ORIG}
#else
    parseFunction = reinterpret_cast<bool (*) (const uint8_t*, size_t)>(
        cantFail(this->JIT->lookup(Compiler::getParseFunctionName())).getAddress());
#endif")
        string(REPLACE "${LLVM_ORIG}" "${LLVM_REPL}" llvm_content "${llvm_content}")
        file(WRITE ${LLVM_FILE} "${llvm_content}")
    else()
        message(FATAL_ERROR "fsst_like_src_patches.cmake: expected toPtr line not found in ${LLVM_FILE} — upstream pin changed; re-verify patch 1")
    endif()
endif()

# --- Patch 2: unsuffixed uint64_t literals in generated kernels -------------
# CppCompiler emits automaton State::level (uint64_t; SIZE_MAX is its "no
# level" sentinel) into generated .cpp kernels via fmt::format("{}", ...) with
# NO integer-literal suffix — unlike the prefix/suffix sites in this same file,
# which correctly use "{}ull". A sentinel-valued level then emits the bare
# decimal for 2^64-1 into the generated kernel, which clang flags as
# -Wimplicitly-unsigned-literal on every compile (cosmetic only: the value is
# correct, just untyped). Add the missing "ull" suffix at every such site,
# matching the style the correct sites already use.
# NOTE: \\t / \\n below are literal backslash-letter pairs (the source file's
# on-disk escape sequences inside a C++ string literal), not control chars.
set(CPP_FILE src/codegen/cppcodegen.cpp)
file(READ ${CPP_FILE} cpp_content)
set(cpp_patched_any FALSE)

set(CPP_S1 "\\tuint64_t level = {};\\n")
string(FIND "${cpp_content}" "${CPP_S1}" cpp_p1)
if(NOT cpp_p1 EQUAL -1)
    set(cpp_patched_any TRUE)
    string(REPLACE "${CPP_S1}" "\\tuint64_t level = {}ull;\\n" cpp_content "${cpp_content}")
endif()

set(CPP_S2 "\\t\\t\\t\\t\\t\\t\\tlevel = {};\\n")
string(FIND "${cpp_content}" "${CPP_S2}" cpp_p2)
if(NOT cpp_p2 EQUAL -1)
    set(cpp_patched_any TRUE)
    string(REPLACE "${CPP_S2}" "\\t\\t\\t\\t\\t\\t\\tlevel = {}ull;\\n" cpp_content "${cpp_content}")
endif()

set(CPP_S3 "\\t\\t\\t\\t\\t\\tlevel = {};\\n")
string(FIND "${cpp_content}" "${CPP_S3}" cpp_p3)
if(NOT cpp_p3 EQUAL -1)
    set(cpp_patched_any TRUE)
    string(REPLACE "${CPP_S3}" "\\t\\t\\t\\t\\t\\tlevel = {}ull;\\n" cpp_content "${cpp_content}")
endif()

set(CPP_S4 "\\tif (len < {})\\n")
string(FIND "${cpp_content}" "${CPP_S4}" cpp_p4)
if(NOT cpp_p4 EQUAL -1)
    set(cpp_patched_any TRUE)
    string(REPLACE "${CPP_S4}" "\\tif (len < {}ull)\\n" cpp_content "${cpp_content}")
endif()

set(CPP_S5 "\\tif (!canParse || strIdx + 1 < {})\\n")
string(FIND "${cpp_content}" "${CPP_S5}" cpp_p5)
if(NOT cpp_p5 EQUAL -1)
    set(cpp_patched_any TRUE)
    string(REPLACE "${CPP_S5}" "\\tif (!canParse || strIdx + 1 < {}ull)\\n" cpp_content "${cpp_content}")
endif()

if(cpp_patched_any)
    file(WRITE ${CPP_FILE} "${cpp_content}")
else()
    string(FIND "${cpp_content}" "\\tuint64_t level = {}ull;\\n" cpp_already)
    if(cpp_already EQUAL -1)
        message(FATAL_ERROR "fsst_like_src_patches.cmake: none of the expected level/len format sites found in ${CPP_FILE} — upstream pin changed; re-verify patch 2")
    endif()
endif()

# --- Patch 3: SIMD skips in the interpreter --------------------------------
# AutomatonTable::parse gets an `int Simd` template parameter for the loop that
# skips bytes which leave the current state alone. 1 (interp-simd): a state
# whose bitmap holds at most 16 bytes first runs the loop cppcodegen.cpp emits
# for its -simd kernels (_mm_cmpestrm for 6-16 bytes, one _mm_cmpeq_epi8 per
# byte below 6). 2 (interp-bits): every state runs simd_cut_fsst's u8 bit
# table (search/prefilter/scan/matcher/nibble.hpp), two 16-byte tables split by
# high nibble and three shuffles per vector, 64 bytes per step on AVX-512BW
# and 16 on NEON, then 16-byte steps. Either way the scalar loop resumes where
# the vector steps stop, and the sets are read off the state's own bitmap, so
# the first index found is the one the scalar loop finds. 0 is upstream.
set(AUT_H include/like_pattern_automaton.hpp)
set(AUT_CPP src/like_pattern_automaton.cpp)
file(READ ${AUT_H} aut_h)
file(READ ${AUT_CPP} aut_cpp)
string(FIND "${aut_h}" "onpair_like patch 3" aut_already)
if(aut_already EQUAL -1)
    set(H1 [=[#include <optional>
]=])
    set(H1R [=[#include <optional>
#include <array>
#include <bit>
#if defined(__SSE4_2__) || defined(__AVX512BW__)
#include <immintrin.h>
#endif
#if defined(__ARM_NEON)
#include <arm_neon.h>
#endif
]=])
    set(H2 [=[        size_t N;

        AutomatonTable(]=])
    set(H2R [=[        size_t N;

        // onpair_like patch 3. Per skip state: the bytes that leave it when
        // there are at most 16 (simdCount 0xFF: more), and the same set as a
        // 16x16 bit table, rows by low nibble, for high nibbles 0..7 (nibLow)
        // and 8..15 (nibHigh).
        std::vector<std::array<uint8_t, 16>> simdSet;
        std::vector<uint8_t> simdCount;
        std::vector<std::array<uint8_t, 16>> nibLow;
        std::vector<std::array<uint8_t, 16>> nibHigh;

        void enableSimd() {
            simdSet.assign(N, {});
            simdCount.assign(N, 0xFF);
            nibLow.assign(N, {});
            nibHigh.assign(N, {});
            for (size_t s = 0; s < N; s++) {
                const uint8_t* bits = table + (s << (8 + shift)) + s * kTransitionArraySize;
                unsigned n = 0;
                for (unsigned b = 0; b < 256; b++) {
                    if (!((bits[b / 8] >> (b % 8)) & 1u)) continue;
                    if (n < 16) simdSet[s][n] = uint8_t(b);
                    n++;
                    if (b < 128) nibLow[s][b & 15] |= uint8_t(1u << (b >> 4));
                    else nibHigh[s][b & 15] |= uint8_t(1u << ((b >> 4) - 8));
                }
                if (n <= 16) simdCount[s] = uint8_t(n);
            }
        }

#ifdef __SSE4_2__
        // From `i`, 16 bytes per step while all of them lie at or before
        // maxIdx: the first byte in the state's set, else where the steps end.
        int64_t simdSkip(size_t state, const uint8_t* str, int64_t i, int64_t maxIdx, size_t len) const {
            const uint8_t n = simdCount[state];
            if (n == 0xFF) return i;
            const __m128i set = _mm_loadu_si128(reinterpret_cast<const __m128i*>(simdSet[state].data()));
            __m128i one[6];
            for (uint8_t k = 0; k < n && k < 6; k++) one[k] = _mm_set1_epi8(char(simdSet[state][k]));
            while (i + 15 <= maxIdx && size_t(i) + 16 <= len) {
                const __m128i v = _mm_loadu_si128(reinterpret_cast<const __m128i*>(str + i));
                uint32_t mask = 0;
                if (n >= 6) {
                    mask = uint16_t(_mm_extract_epi16(_mm_cmpestrm(set, n, v, 16,
                        _SIDD_UBYTE_OPS | _SIDD_CMP_EQUAL_ANY | _SIDD_BIT_MASK), 0));
                } else {
                    for (uint8_t k = 0; k < n; k++) mask |= uint32_t(_mm_movemask_epi8(_mm_cmpeq_epi8(one[k], v)));
                }
                if (mask) return i + std::countr_zero(mask);
                i += 16;
            }
            return i;
        }
#endif

#if defined(__AVX512BW__) || defined(__ARM_NEON)
        // As simdSkip, for any set size: a byte is in the set when its row,
        // looked up by low nibble in the half its high nibble selects, has the
        // high nibble's bit. The index keeps bit 7, so the wrong half reads 0.
        int64_t bitSkip(size_t state, const uint8_t* str, int64_t i, int64_t maxIdx, size_t len) const {
            alignas(16) static constexpr uint8_t BIT[16] = {1, 2, 4, 8, 16, 32, 64, 128, 1, 2, 4, 8, 16, 32, 64, 128};
#if defined(__AVX512BW__)
            const __m128i lo1 = _mm_loadu_si128(reinterpret_cast<const __m128i*>(nibLow[state].data()));
            const __m128i hi1 = _mm_loadu_si128(reinterpret_cast<const __m128i*>(nibHigh[state].data()));
            const __m128i bit1 = _mm_load_si128(reinterpret_cast<const __m128i*>(BIT));
            const __m512i lo = _mm512_broadcast_i32x4(lo1), hi = _mm512_broadcast_i32x4(hi1), bit = _mm512_broadcast_i32x4(bit1);
            const __m512i k8f = _mm512_set1_epi8(char(0x8f)), k80 = _mm512_set1_epi8(char(0x80)), k0f = _mm512_set1_epi8(0x0f);
            while (i + 63 <= maxIdx && size_t(i) + 64 <= len) {
                const __m512i v = _mm512_loadu_si512(str + i);
                const __m512i idx = _mm512_and_si512(v, k8f);
                const __m512i rows = _mm512_or_si512(_mm512_shuffle_epi8(lo, idx), _mm512_shuffle_epi8(hi, _mm512_xor_si512(idx, k80)));
                const __m512i b = _mm512_shuffle_epi8(bit, _mm512_and_si512(_mm512_srli_epi16(v, 4), k0f));
                const uint64_t mask = _mm512_test_epi8_mask(rows, b);
                if (mask) return i + std::countr_zero(mask);
                i += 64;
            }
            const __m128i s8f = _mm_set1_epi8(char(0x8f)), s80 = _mm_set1_epi8(char(0x80)), s0f = _mm_set1_epi8(0x0f);
            while (i + 15 <= maxIdx && size_t(i) + 16 <= len) {
                const __m128i v = _mm_loadu_si128(reinterpret_cast<const __m128i*>(str + i));
                const __m128i idx = _mm_and_si128(v, s8f);
                const __m128i rows = _mm_or_si128(_mm_shuffle_epi8(lo1, idx), _mm_shuffle_epi8(hi1, _mm_xor_si128(idx, s80)));
                const __m128i b = _mm_shuffle_epi8(bit1, _mm_and_si128(_mm_srli_epi16(v, 4), s0f));
                const uint32_t mask = _mm_test_epi8_mask(rows, b);
                if (mask) return i + std::countr_zero(mask);
                i += 16;
            }
#else
            const uint8x16_t lo = vld1q_u8(nibLow[state].data()), hi = vld1q_u8(nibHigh[state].data()), bit = vld1q_u8(BIT);
            const uint8x16_t k8f = vdupq_n_u8(0x8f), k80 = vdupq_n_u8(0x80);
            while (i + 15 <= maxIdx && size_t(i) + 16 <= len) {
                const uint8x16_t v = vld1q_u8(str + i);
                const uint8x16_t idx = vandq_u8(v, k8f);
                const uint8x16_t rows = vorrq_u8(vqtbl1q_u8(lo, idx), vqtbl1q_u8(hi, veorq_u8(idx, k80)));
                const uint8x16_t hit = vtstq_u8(rows, vqtbl1q_u8(bit, vshrq_n_u8(v, 4)));
                const uint64_t mask = vget_lane_u64(vreinterpret_u64_u8(vshrn_n_u16(vreinterpretq_u16_u8(hit), 4)), 0);
                if (mask) return i + (std::countr_zero(mask) >> 2);
                i += 16;
            }
#endif
            return i;
        }
#endif

        AutomatonTable(]=])
    set(H3 [=[        template <bool Forwards>
        bool parse(int64_t* strIdx,]=])
    set(H3R [=[        template <bool Forwards, int Simd = 0>
        bool parse(int64_t* strIdx,]=])
    set(H4 [=[                            uint8_t* transitionArray = table + (currentState << (8 + shift)) + currentState * kTransitionArraySize;
]=])
    set(H4R [=[                            uint8_t* transitionArray = table + (currentState << (8 + shift)) + currentState * kTransitionArraySize;
#ifdef __SSE4_2__
                            if constexpr (Simd == 1)
                                currentIdx = simdSkip(currentState, str, currentIdx, int64_t(len) - int64_t(level), len);
#endif
#if defined(__AVX512BW__) || defined(__ARM_NEON)
                            if constexpr (Simd == 2)
                                currentIdx = bitSkip(currentState, str, currentIdx, int64_t(len) - int64_t(level), len);
#endif
]=])
    set(H5 [=[        LikePatternAutomatonParser(const std::span<const uint8_t>& match, const Encoder& encoder);
]=])
    set(H5R [=[        LikePatternAutomatonParser(const std::span<const uint8_t>& match, const Encoder& encoder);
        int simd = 0;
        void enableSimd(int mode) {
            if (forwardTable.has_value()) forwardTable->enableSimd();
            simd = mode;
        }
]=])
    set(C1 [=[        if (!forwardTable->parse<true>(&forwardStrIdx, &forwardSymbolIdx, match.data(), backwardsStrIdx + 1))]=])
    set(C1R [=[        const uint8_t* data = match.data();
        const size_t n = backwardsStrIdx + 1;
        if (!(simd == 1   ? forwardTable->parse<true, 1>(&forwardStrIdx, &forwardSymbolIdx, data, n)
              : simd == 2 ? forwardTable->parse<true, 2>(&forwardStrIdx, &forwardSymbolIdx, data, n)
                          : forwardTable->parse<true>(&forwardStrIdx, &forwardSymbolIdx, data, n)))]=])
    foreach(k H1 H2 H3 H4 H5)
        string(FIND "${aut_h}" "${${k}}" pos)
        if(pos EQUAL -1)
            message(FATAL_ERROR "fsst_like_src_patches.cmake: patch 3 site ${k} not found in ${AUT_H} — upstream pin changed")
        endif()
    endforeach()
    string(FIND "${aut_cpp}" "${C1}" pos)
    if(pos EQUAL -1)
        message(FATAL_ERROR "fsst_like_src_patches.cmake: patch 3 site C1 not found in ${AUT_CPP} — upstream pin changed")
    endif()
    string(REPLACE "${H1}" "${H1R}" aut_h "${aut_h}")
    string(REPLACE "${H2}" "${H2R}" aut_h "${aut_h}")
    string(REPLACE "${H3}" "${H3R}" aut_h "${aut_h}")
    string(REPLACE "${H4}" "${H4R}" aut_h "${aut_h}")
    string(REPLACE "${H5}" "${H5R}" aut_h "${aut_h}")
    string(REPLACE "${C1}" "${C1R}" aut_cpp "${aut_cpp}")
    file(WRITE ${AUT_H} "${aut_h}")
    file(WRITE ${AUT_CPP} "${aut_cpp}")
endif()
