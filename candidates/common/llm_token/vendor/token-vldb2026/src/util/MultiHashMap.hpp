#pragma once
//---------------------------------------------------------------------------
#include "util/CompilerHints.hpp"
#include "util/Hash.hpp"
#include <algorithm>
#include <cstdint>
#include <iostream>
#include <span>
#include <stdexcept>
#include <vector>
//---------------------------------------------------------------------------
// Static Global Token Table
// (c) 2025 Tobias Schmidt
//---------------------------------------------------------------------------
namespace sgtt {
//---------------------------------------------------------------------------
/// A static hash map for small key sets (up to 65k elements)
template <class Key, class Value, typename Hash = void>
class multi_hash_map {
   /// The hash table size
   uint32_t shift;

   /// The directory
   std::vector<uint32_t> directory;
   /// The values
   std::vector<Value> values;
   /// The insert counter
   std::vector<uint32_t> insertCounter;

   /// Hash a value and map to range
   forceinline uint64_t hash(Key key) const noexcept {
      return fasthash(key);
   }
   /// Map a hash to the range [0, size)
   forceinline uint64_t mapToRange(uint64_t hash) const noexcept { return hash >> shift; }

   public:
   /// Constructor
   multi_hash_map() : shift(63) {
      directory.resize(2, 0);
      values.resize(0);
   }
   /// Find an entry
   forceinline std::pair<const Value*, const Value*> find(Key key) const noexcept {
      const auto slot = mapToRange(hash(key));
      const auto* data = values.data();
      assert(slot < directory.size());
      return std::make_pair(data + directory[slot], data + directory[slot + 1]);
   }
   void initialize(std::span<Key> keys);
   /// Access an entry
   Value& access(Key key) {
      const auto slot = mapToRange(hash(key));
      assert(slot < directory.size());

      const auto begin = directory[slot];
      auto offset = insertCounter[slot];
      assert(begin + offset < directory[slot + 1]);

      ++insertCounter[slot];
      return values[begin + offset];
   }
   void setDefault(const Value& value) {
      values[values.size() - 1] = value;
      values[values.size() - 2] = value;
      values[values.size() - 3] = value;
      values[values.size() - 4] = value;
   }
   /// Access operator
   Value& operator[](Key key) { return access(key); }
};
//---------------------------------------------------------------------------
template <class Key, class Value, typename Hash>
void multi_hash_map<Key, Value, Hash>::initialize(std::span<Key> keys)
// Constructor
{
   assert(shift == 63); // can only initialize once

   if (keys.empty()) return;

   // Compute the desired hash table size
   unsigned minSizeLog2 = log2ceil(keys.size()) + 2;
   auto hashTableSize = (1ull << minSizeLog2) + 3;
   shift = 64 - minSizeLog2;

   directory.clear();
   values.clear();
   insertCounter.clear();

   directory.resize(hashTableSize + 1, 0);
   values.resize(keys.size() + 4);
   insertCounter.resize(hashTableSize, 0);

   for (auto& key : keys) {
      auto slot = mapToRange(hash(key));
      ++insertCounter[slot];
   }
   for (unsigned i = 1; i <= hashTableSize; ++i) {
      directory[i] = directory[i - 1] + insertCounter[i - 1];
      insertCounter[i - 1] = 0;
   }
}
//---------------------------------------------------------------------------
}
//---------------------------------------------------------------------------
