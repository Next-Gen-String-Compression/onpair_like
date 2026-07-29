#pragma once

// Bitpacked-size accounting, vendored and adapted from
// SqlPile/external/CompressionBenchmark (src/utils/bitpacking_utils.hpp): the
// same formula, made standalone (no duckdb.h). Computes the number of bytes a
// fixed-width bitpacked array WOULD occupy. The codes/lengths themselves stay
// full-width (u32) in memory and decode() reads them directly at full speed;
// only the reported footprint reflects bitpacking — exactly as the reference's
// BitPackingUtils does. Dictionary codes are the load-bearing case: at N unique
// values a code needs ceil(log2(N+1)) bits, not a whole u32.

#include <cstdint>
#include <cmath>
#include <vector>
#include <algorithm>

namespace bitpack {

// Bits to store a value in [0, range]. Matches BitPackingUtils::GetBitsPerValue.
inline uint8_t BitsPerValue(uint64_t range) {
    if (range == 0) return 1;
    return static_cast<uint8_t>(std::ceil(std::log2(static_cast<double>(range) + 1.0)));
}

// Bytes to bitpack `n_values` values each in [0, range].
inline uint64_t CompressedSize(uint64_t range, uint64_t n_values) {
    const uint8_t bits = BitsPerValue(range);
    return (static_cast<uint64_t>(bits) * n_values + 7) / 8;
}

// Bytes to bitpack a concrete vector (width from its min..max range), as the
// reference's vector overload does.
template <typename T>
inline uint64_t CompressedSize(const std::vector<T>& values) {
    if (values.empty()) return 0;
    const auto mm = std::minmax_element(values.begin(), values.end());
    const uint64_t range = static_cast<uint64_t>(*mm.second - *mm.first);
    return CompressedSize(range, values.size());
}

}  // namespace bitpack
