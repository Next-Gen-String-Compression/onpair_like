#pragma once
// =============================================================================
// prefilter_prod.hpp — production substring prefilter, single- and multi-needle
// =============================================================================
//
// Scans a vector of rows (a DBMS string column: per-row pointer + length) for
// fixed-width byte codes and writes the indices of rows containing ANY of the
// needles into a selection vector:
//
//     col LIKE '%ab%'   OR ...              -> 2-byte needles (uint16_t)
//     col LIKE '%abcd%' OR ...              -> 4-byte needles (uint32_t)
//
// Two widths, one algorithm (the "vert_valid" kernel): per row, dispatch once
// on length to a width-specialized scan (16-byte blocks with vertical OR
// accumulators / clamped u64 windows / clamped tiny loads), then loop the
// needles over that scan with early exit on the first hit.
//
// Properties (the conservative set — this kernel assumes nothing):
//   * EXACT: no false positives, no false negatives; result equals
//     std::string_view::find != npos, OR-ed over the needles.
//   * Safe on a raw, unfiltered column: rows shorter than the needle width
//     (including empty rows) are rejected by a single guard.
//   * No out-of-bounds reads: every load stays inside [row, row + len).
//     No trailing-padding or buffer-slack contract is required.
//   * No layout assumption: rows may live anywhere (heap, inline, dictionary).
//   * Portable C++ (no intrinsics), no allocations, stateless / thread-safe.
//
// Multi-needle contract: 1 <= num_needles <= MAX_NEEDLES (8). The needle loop
// itself works for any count, but the kernel is tuned and benchmarked for up
// to 8; beyond that a shared-filter algorithm (Teddy-style) wins.
// Duplicate needles are harmless. Matching is raw byte comparison (no case
// folding, NUL bytes are fine).
//
// Performance (Apple M4 Pro, clang -O3, hits.URL, avg row 64 B, 4-byte codes):
//   1 needle  ~11 GB/s (~170 M rows/s); 8 needles cost ~4.4x one needle
//   (vs 8x for eight separate passes). Fusing needles at block level is
//   only possible with SIMD intrinsics (~1.5x more at K=8); this header
//   deliberately stays portable.
//
// CODEGEN WARNING (the price of portability): the hot loops rely on the
// auto-vectorizer. The intended shape for the block scan is, per 16 bytes,
// CODE_LEN overlapping 16-byte loads + that many vector compares + ORs, with
// accumulators in registers. The same source has compiled to 2-3x slower
// code in other translation units (ld4 interleaved loads, scalar csinv
// chains) purely from different inlining/unroll context. After integrating:
// disassemble the instantiated kernels and check for ld4/st4, stack spills
// in the inner loop, or the absence of vector compares. If codegen is
// unstable in your TU, compile this header in a dedicated .cpp and export
// wrappers, so the kernel keeps its own compilation context.
// =============================================================================
#include <algorithm>
#include <cstdint>
#include <cstring>

namespace prefilter {

// Maximum supported needle count per call (see contract above).
inline constexpr uint32_t MAX_NEEDLES = 8;

namespace detail {

// Width-dependent geometry. CodeT is uint16_t or uint32_t; everything below
// derives from its size, so both widths share one implementation.
//
//   CODE_LEN    bytes per needle (2 / 4)
//   WINDOWS_16  window starts checked per 16-byte block (16 for both)
//   MIN_LEN_16  shortest row for the block path: a block's last window ends at
//               byte 16 + CODE_LEN - 2 (17 / 19 bytes read)
//   BASES       byte alignments a window can have inside a block (2 / 4)
//   LANES       windows per aligned 16-byte vector compare (8 / 4)
//   WINDOWS_U64 window starts inside one 8-byte word (7 / 5)
//   TINY_ITERS  clamped loads covering every window of a row shorter than 8
//               bytes: last start is len - CODE_LEN <= 8 - CODE_LEN - 1 (6/4)
template <typename CodeT>
struct Geometry {
	static constexpr uint32_t CODE_LEN = sizeof(CodeT);
	static constexpr uint32_t WINDOWS_16 = 16;
	static constexpr uint32_t MIN_LEN_16 = WINDOWS_16 + CODE_LEN - 1;
	static constexpr uint32_t MIN_LEN_8 = sizeof(uint64_t);
	static constexpr uint32_t BASES = CODE_LEN;
	static constexpr uint32_t LANES = WINDOWS_16 / CODE_LEN;
	static constexpr uint32_t WINDOWS_U64 = static_cast<uint32_t>(sizeof(uint64_t)) - CODE_LEN + 1;
	static constexpr uint32_t TINY_ITERS = MIN_LEN_8 - CODE_LEN;
	static_assert(sizeof(CodeT) == 2 || sizeof(CodeT) == 4, "prefilter supports 2- and 4-byte needles");
};

// Compare the 16 window starts block[0..15] against `code`, OR-ing each
// window's mask vertically into window_masks[0..15]. The block reads
// MIN_LEN_16 bytes (window @15 is CODE_LEN wide). The 16 starts are swept as
// BASES byte alignments x LANES stride-CODE_LEN lanes, so within one base the
// lanes are 16 contiguous bytes = one vector load + lane compare for the
// auto-vectorizer.
template <typename CodeT>
inline void block16(const char *__restrict block, CodeT code, CodeT *__restrict window_masks) {
	using G = Geometry<CodeT>;
	for (uint32_t base = 0; base < G::BASES; ++base) {
		for (uint32_t lane = 0; lane < G::LANES; ++lane) {
			CodeT word;
			std::memcpy(&word, block + base + G::CODE_LEN * lane, G::CODE_LEN);
			window_masks[G::LANES * base + lane] |= word == code ? static_cast<CodeT>(~CodeT {0}) : CodeT {0};
		}
	}
}

// len >= MIN_LEN_16: full blocks, then one block re-aligned to the row end to
// cover the leftover windows (re-checking a window is harmless for
// membership). Vertical accumulators, one horizontal reduce per row.
template <typename CodeT>
bool contains_16(const char *row, uint32_t len, CodeT code) {
	using G = Geometry<CodeT>;
	const uint32_t num_windows = len - G::CODE_LEN + 1;
	CodeT window_masks[G::WINDOWS_16] = {};
	uint32_t start = 0;
	for (; start + G::WINDOWS_16 <= num_windows; start += G::WINDOWS_16)
		block16(row + start, code, window_masks);
	if (start < num_windows) {
		const uint32_t tail_start = num_windows - G::WINDOWS_16;
		block16(row + tail_start, code, window_masks);
	}
	CodeT matched = 0;
	for (uint32_t i = 0; i < G::WINDOWS_16; ++i)
		matched |= window_masks[i];
	return matched != 0;
}

// The WINDOWS_U64 window starts inside the 8-byte word at block8. The caller
// guarantees block8[0..7] is inside the row.
template <typename CodeT>
inline uint32_t windows_u64(const char *__restrict block8, CodeT code) {
	using G = Geometry<CodeT>;
	uint64_t word;
	std::memcpy(&word, block8, sizeof(uint64_t));
	uint32_t matched = 0;
	for (uint32_t i = 0; i < G::WINDOWS_U64; ++i) {
		matched |= (static_cast<CodeT>(word >> (8 * i)) == code);
	}
	return matched;
}

// len in [8, MIN_LEN_16): cover all window starts with three u64 words
// stepped WINDOWS_U64 apart, each clamped so its load ends inside the row
// (overlap re-checks windows, harmless).
template <typename CodeT>
inline bool contains_8(const char *__restrict row, uint32_t len, CodeT code) {
	using G = Geometry<CodeT>;
	const uint32_t last_load_offset = len - static_cast<uint32_t>(sizeof(uint64_t));
	const uint32_t start1 = std::min(1 * G::WINDOWS_U64, last_load_offset);
	const uint32_t start2 = std::min(2 * G::WINDOWS_U64, last_load_offset);
	uint32_t matched = windows_u64(row, code);
	matched |= windows_u64(row + start1, code);
	matched |= windows_u64(row + start2, code);
	return matched != 0;
}

// len in [CODE_LEN, 8): one window per load, fixed trip count with the start
// clamped to the last valid window (re-checks, harmless). The fixed count is
// deliberate: a length-dependent exit branch mispredicts on random-length
// rows (measured 2.4x slower).
template <typename CodeT>
inline bool contains_tiny(const char *__restrict row, uint32_t len, CodeT code) {
	using G = Geometry<CodeT>;
	const uint32_t last_offset = len - G::CODE_LEN;
	uint32_t matched = 0;
	for (uint32_t i = 0; i < G::TINY_ITERS; ++i) {
		const uint32_t start = std::min(i, last_offset);
		CodeT word;
		std::memcpy(&word, row + start, G::CODE_LEN);
		matched |= (word == code);
	}
	return matched != 0;
}

} // namespace detail

// -----------------------------------------------------------------------------
// Per-row API: does row[0..len) contain `code` anywhere? Safe for any len
// (returns false when the row cannot hold the code).
// -----------------------------------------------------------------------------
template <typename CodeT>
inline bool contains(const char *row, uint32_t len, CodeT code) {
	using G = detail::Geometry<CodeT>;
	if (len < G::CODE_LEN)
		return false;
	if (len >= G::MIN_LEN_16)
		return detail::contains_16(row, len, code);
	if (len >= G::MIN_LEN_8)
		return detail::contains_8(row, len, code);
	return detail::contains_tiny(row, len, code);
}

// -----------------------------------------------------------------------------
// Batch API: scan n rows for ANY of codes[0..num_codes), append the index of
// every matching row to `sel` (room for n entries), return the match count.
// Indices are 0..n-1, ascending. 1 <= num_codes <= MAX_NEEDLES.
//
// The length dispatch runs once per row; the needle loop wraps the
// width-specialized scan and exits on the first hit. The selection append is
// branchless (write, then bump by the hit flag).
// -----------------------------------------------------------------------------
template <typename CodeT>
inline uint32_t contains_any(const char *const *data, const uint32_t *lengths, uint32_t n, const CodeT *codes,
                             uint32_t num_codes, uint32_t *sel) {
	using G = detail::Geometry<CodeT>;
	uint32_t count = 0;
	for (uint32_t i = 0; i < n; ++i) {
		const char *row = data[i];
		const uint32_t len = lengths[i];
		bool hit = false;
		if (len >= G::MIN_LEN_16) {
			for (uint32_t k = 0; k < num_codes && !hit; ++k)
				hit = detail::contains_16(row, len, codes[k]);
		} else if (len >= G::MIN_LEN_8) {
			for (uint32_t k = 0; k < num_codes && !hit; ++k)
				hit = detail::contains_8(row, len, codes[k]);
		} else if (len >= G::CODE_LEN) {
			for (uint32_t k = 0; k < num_codes && !hit; ++k)
				hit = detail::contains_tiny(row, len, codes[k]);
		}
		sel[count] = i;
		count += hit;
	}
	return count;
}

// -----------------------------------------------------------------------------
// Byte-oriented convenience wrappers: needles passed as concatenated raw
// bytes (num_needles * 2 or * 4 bytes), e.g. straight out of a query plan.
// -----------------------------------------------------------------------------
inline uint32_t contains_any_u16(const char *const *data, const uint32_t *lengths, uint32_t n, const char *needle_bytes,
                                 uint32_t num_needles, uint32_t *sel) {
	uint16_t codes[MAX_NEEDLES];
	for (uint32_t k = 0; k < num_needles; ++k)
		std::memcpy(&codes[k], needle_bytes + 2 * k, 2);
	return contains_any(data, lengths, n, codes, num_needles, sel);
}

inline uint32_t contains_any_u32(const char *const *data, const uint32_t *lengths, uint32_t n, const char *needle_bytes,
                                 uint32_t num_needles, uint32_t *sel) {
	uint32_t codes[MAX_NEEDLES];
	for (uint32_t k = 0; k < num_needles; ++k)
		std::memcpy(&codes[k], needle_bytes + 4 * k, 4);
	return contains_any(data, lengths, n, codes, num_needles, sel);
}

} // namespace prefilter
