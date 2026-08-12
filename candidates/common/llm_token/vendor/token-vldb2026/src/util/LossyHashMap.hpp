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
class lossy_hash_map {
   public:
   using key_type = Key;
   using value_type = std::pair<Key, Value>;
   using reference = value_type&;
   using const_reference = const value_type&;
   using pointer = value_type*;
   using const_pointer = const value_type*;

   template <bool IsConst>
   class iterator_impl {
      using iterator_type = std::conditional_t<IsConst, typename std::vector<value_type>::const_pointer, typename std::vector<value_type>::pointer>;
      using reference_type = std::conditional_t<IsConst, const_reference, reference>;
      using pointer_type = std::conditional_t<IsConst, const_pointer, pointer>;

      const lossy_hash_map* map;
      iterator_type entry = nullptr;

      template <bool B>
      friend class iterator_impl;

      public:
      iterator_impl(const lossy_hash_map* map, iterator_type entry) : map(map), entry(entry) {}
      template <bool OtherIsConst, typename = std::enable_if_t<IsConst && !OtherIsConst>>
      constexpr iterator_impl(iterator_impl<OtherIsConst> const& other) noexcept : map(other.map), entry(other.entry) {}
      template <bool OtherIsConst, typename = std::enable_if_t<IsConst && !OtherIsConst>>
      constexpr iterator_impl& operator=(iterator_impl<OtherIsConst> const& other) noexcept {
         map = other.map;
         entry = other.entry;
         return *this;
      }

      void next() {
         while (entry && entry != &map->values.back() && entry->first == Key{}) ++entry;
         if (entry == &map->values.back()) entry = nullptr;
      }
      iterator_impl& operator++() {
         ++entry;
         next();
         return *this;
      }

      template <bool O>
      bool operator==(const iterator_impl<O>& other) const { return entry == other.entry; }
      template <bool O>
      bool operator!=(const iterator_impl<O>& other) const { return entry != other.entry; }

      reference_type operator*() const { return *entry; }
      pointer_type operator->() const { return std::addressof(*entry); }
   };

   using iterator = iterator_impl<false>;
   using const_iterator = iterator_impl<true>;

   private:
   /// The hash table size
   uint32_t shift;

   /// The values
   std::vector<value_type> values;

   Value override;

   /// Hash a value and map to range
   forceinline uint64_t hash(Key key) const noexcept {
      return fasthash(key);
   }
   /// Map a hash to the range [0, size)
   forceinline uint64_t mapToRange(uint64_t hash) const noexcept { return hash >> shift; }

   public:
   /// Constructor
   lossy_hash_map() : shift(63) { values.resize(3); }
   /// Find an entry
   forceinline const_iterator find(Key key) const noexcept {
      auto slot = mapToRange(hash(key));

      assert(slot < values.size());
      if (values[slot].first == key) [[likely]]
         return const_iterator(this, &values[slot]);
      else if (values[slot + 1].first == key)
         return const_iterator(this, &values[slot + 1]);
      else
         return end();
   }
   /// Find an entry
   forceinline std::pair<Value, uint64_t> findOrZero(Key key) const noexcept {
      auto slot = mapToRange(hash(key));

      assert(slot < values.size());
      auto [k1, v1] = values[slot];
      auto [k2, v2] = values[slot + 1];
      uint64_t mask1 = -(k1 == key);
      uint64_t mask2 = -(k2 == key);
      return std::make_pair((v1 & mask1) | (v2 & mask2), mask1 | mask2);
   }
   void initialize(std::span<Key> keys);
   /// Access an entry
   Value& access(Key key) {
      auto slot = mapToRange(hash(key));

      assert(slot < values.size());
      if (values[slot].first == key) return values[slot].second;
      if (values[slot + 1].first == key) return values[slot + 1].second;
      return override;
   }
   /// Access operator
   Value& operator[](Key key) { return access(key); }

   iterator begin() {
      auto it = iterator(this, &values.front());
      it.next();
      return it;
   }
   const_iterator begin() const {
      auto it = const_iterator(this, &values.front());
      it.next();
      return it;
   }
   const_iterator cbegin() const { return begin(); }
   iterator end() { return iterator(this, nullptr); }
   const_iterator end() const { return const_iterator(this, nullptr); }
   const_iterator cend() const { return const_iterator(this, nullptr); }
};
//---------------------------------------------------------------------------
template <class Key, class Value, typename Hash>
void lossy_hash_map<Key, Value, Hash>::initialize(std::span<Key> keys)
// Constructor
{
   assert(shift == 63); // can only initialize once

   if (keys.empty()) return;

   // Compute the desired hash table size
   unsigned minSizeLog2 = log2ceil(keys.size()) + 2;
   auto hashTableSize = (1ull << minSizeLog2) + 3;
   shift = 64 - minSizeLog2;

   auto insert = [&](Key key) {
      auto slot = mapToRange(hash(key));
      if (values[slot].first == Key{}) {
         values[slot] = std::make_pair(key, Value{});
         return true;
      } else if (values[slot + 1].first == Key{}) {
         values[slot + 1] = std::make_pair(key, Value{});
         return true;
      } else {
         return false;
      }
   };

   values.clear();
   values.resize(hashTableSize);
   for (unsigned index = 0; index != keys.size(); ++index) {
      auto slot = mapToRange(hash(keys[index]));
      if (insert(keys[index])) {
      } else if (insert(values[slot + 1].first)) {
         values[slot + 1] = std::make_pair(keys[index], Value{});
      }
   }
}
//---------------------------------------------------------------------------
}
//---------------------------------------------------------------------------
