#pragma once
//---------------------------------------------------------------------------
#include "util/CompilerHints.hpp"
#include <cstdint>
//---------------------------------------------------------------------------
// Static Global Token Table
// (c) 2025 Tobias Schmidt
//---------------------------------------------------------------------------
struct Hash {
   /// Pseudorandom "secret" constants from wyhash
   static constexpr uint64_t k0 = 0xa0761d6478bd642full, k1 = 0xe7037ed1a0b428dbull, k2 = 0x8ebc6af09c88c6e3ull, k3 = 0x589965cc75374cc3ull, k4 = k3;
   /// Full 128 bit multiplication folded into 64bit
   constexpr static inline uint64_t mulFold(uint64_t a, uint64_t b) noexcept {
      auto m = static_cast<unsigned __int128>(a) * static_cast<unsigned __int128>(b);
      return static_cast<uint64_t>(m >> 64) ^ static_cast<uint64_t>(m);
   }
   /// Hash a two values
   static constexpr std::uint64_t hashTwo(uint64_t v1, uint64_t v2, uint64_t seed = 0) {
      uint64_t a = v1, b = v2;
      seed = seed ^ k0;
      return mulFold(k4 ^ 16, mulFold(a ^ k1, b ^ seed));
   }
   /// Hash a single value
   static constexpr uint64_t hashUInt64(uint64_t v, uint64_t seed = 0) {
      uint64_t a = v, b = v;
      seed = seed ^ k0;
      return mulFold(k4 ^ 8, mulFold(a ^ k1, b ^ seed));
   }

   /// Hash a 32-bit integer
   constexpr std::size_t operator()(const uint32_t& v) const noexcept { return hashUInt64(v); }
   /// Hash a 64-bit integer
   constexpr std::size_t operator()(const uint64_t& v) const noexcept { return hashUInt64(v); }
   /// Hash a 128-bit integer
   constexpr std::size_t operator()(const uint128_t& v) const noexcept { return hashTwo(static_cast<uint64_t>(v), v >> 64); }
};
//---------------------------------------------------------------------------
/// A fast hash function for 64bit keys
template <typename T>
static forceinline uint64_t fasthash(T key, uint64_t seed = 0) {
   if constexpr (std::is_same_v<T, uint128_t> || std::is_same_v<T, int128_t>) {
      return fasthash<uint64_t>(key >> 64, fasthash<uint64_t>(static_cast<uint64_t>(key), seed));
   } else if constexpr (std::is_same_v<T, uint64_t> || std::is_same_v<T, int64_t>) {
      auto m = static_cast<unsigned __int128>(key) * (static_cast<unsigned __int128>(0x2127'599b'f432'5c37ull) ^ seed);
      return static_cast<uint64_t>(m >> 64) ^ static_cast<uint64_t>(m);
   } else {
      return static_cast<uint64_t>(key) * (static_cast<uint64_t>(0x2127'599b'f432'5c37ull) ^ seed);
   }
}
//---------------------------------------------------------------------------
static constexpr uint32_t log2ceil(uint64_t a) noexcept {
   assert(a > 1);
   return static_cast<uint32_t>((8 * sizeof(unsigned long)) - static_cast<uint32_t>(__builtin_clzl(a - 1)));
}
//---------------------------------------------------------------------------
