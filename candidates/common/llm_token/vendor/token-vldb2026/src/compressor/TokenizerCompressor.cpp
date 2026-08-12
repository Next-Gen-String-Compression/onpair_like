#include "compressor/TokenizerCompressor.hpp"
#include "tokens/openai100kpatched/openai100kpatched.hpp"
#include "util/CompilerHints.hpp"
#include "util/LossyHashMap.hpp"
#include "util/MultiHashMap.hpp"
#include <cstdint>
#include <cstring>
#include <mutex>
#include <stdexcept>
#include <vector>
//---------------------------------------------------------------------------
// Static Global Token Table
// (c) 2025 Tobias Schmidt
//---------------------------------------------------------------------------
using namespace std;
//---------------------------------------------------------------------------
namespace sgtt::compressor {
//---------------------------------------------------------------------------
namespace {
//---------------------------------------------------------------------------
// Minimal footer: only the uncompressed size (bytes).
// Tokenizer/dictionary + encoding parameters are hard-wired in this compressor.
struct [[gnu::packed]] Footer {
   uint64_t uncompressedSize;
};
static_assert(sizeof(Footer) == 8);
//---------------------------------------------------------------------------
struct ShortTokenEntry {
   uint64_t text;
   uint64_t mask;
   uint32_t id;
   uint32_t length;
};
static_assert(sizeof(ShortTokenEntry) == 24);
//---------------------------------------------------------------------------
struct Dict {
   vector<uint8_t> lengths;
   vector<uint64_t> tokens;

   // <=2 byte lookup
   vector<uint16_t> shortTokenArray2;
   // 3-byte lookup (lossy hash map)
   lossy_hash_map<uint32_t, uint32_t> tokenMap3;
   // 4..maxTokenSizeBytes lookup keyed by first 4 bytes
   multi_hash_map<uint32_t, ShortTokenEntry> shortTokenMap;

   /// Initialize the dictionary
   void init() {
      static constexpr size_t maxTokenSize = 7;
      static constexpr size_t tokenCount = tokenizer::openai100kpatched::token_dict.size() / sizeof(uint128_t);
      static_assert(maxTokenSize <= 8, "TokenizerCompressor only supports tokens up to 8 bytes");
      static_assert(tokenCount <= 65536, "TokenizerCompressor only supports up to 65536 tokens");
      static_assert(tokenCount == tokenizer::openai100kpatched::token_lengths.size(), "Token count mismatch between token dict and token lengths");

      lengths.resize(tokenCount);
      tokens.resize(tokenCount);
      for (size_t i = 0; i < tokenCount; ++i) {
         if (tokenizer::openai100kpatched::token_lengths[i] == 0 || tokenizer::openai100kpatched::token_lengths[i] > maxTokenSize) {
            throw std::runtime_error("Invalid token length in OpenAI100kPatched dictionary");
         }
         lengths[i] = tokenizer::openai100kpatched::token_lengths[i];
         tokens[i] = unalignedLoad<uint64_t>(tokenizer::openai100kpatched::token_dict.data() + i * sizeof(uint128_t));
      }

      // Build 2-byte array (fallback to 1-byte tokens)
      shortTokenArray2.resize(65536);
      for (size_t i = 0; i < shortTokenArray2.size(); ++i) {
         shortTokenArray2[i] = static_cast<uint16_t>(i % 256);
      }
      for (uint32_t id = 0; id < tokenCount; ++id) {
         if (lengths[id] == 2) {
            shortTokenArray2[static_cast<uint16_t>(tokens[id])] = static_cast<uint16_t>(id);
         }
      }

      // Build lossy map for 3-byte tokens
      {
         vector<uint32_t> keys;
         keys.reserve(tokenCount);
         for (uint32_t id = 0; id < tokenCount; ++id) {
            if (lengths[id] != 3) continue;

            keys.push_back(tokens[id] << 8); // align to match lookup logic
         }
         tokenMap3.initialize(keys);

         for (uint32_t id = 0; id < tokenCount; ++id) {
            if (lengths[id] != 3) continue;

            tokenMap3[tokens[id] << 8] = id;
         }
      }

      // Build shortTokenMap for tokens with length 4..8
      {
         vector<uint32_t> keys;
         keys.reserve(tokenCount);
         for (uint32_t id = 0; id < tokenCount; ++id) {
            const uint8_t len = lengths[id];
            if (len < 4 || len > maxTokenSize) continue;

            keys.push_back(tokens[id]);
         }

         shortTokenMap.initialize(keys);

         for (uint32_t id = tokenCount; id-- > 0;) {
            const uint8_t len = lengths[id];
            if (len < 4 || len > maxTokenSize) continue;

            const uint64_t mask = (~static_cast<uint64_t>(0) >> ((8 - len) * 8));
            const uint64_t text = tokens[id] & mask;
            const uint32_t key = tokens[id];
            shortTokenMap[key] = ShortTokenEntry{.text = text, .mask = mask, .id = static_cast<uint16_t>(id), .length = len};
         }

         shortTokenMap.setDefault(ShortTokenEntry{.text = 0, .mask = 0xFFull, .id = 0, .length = 1});
      }
   }
};
//---------------------------------------------------------------------------
Dict g_dict_openai100kpatched;
once_flag g_dictOnce_openai100kpatched;
//---------------------------------------------------------------------------
const Dict& dict() {
   call_once(g_dictOnce_openai100kpatched, [] { g_dict_openai100kpatched.init(); });
   return g_dict_openai100kpatched;
}

} // namespace
//---------------------------------------------------------------------------
TokenizerCompressor::TokenizerCompressor()
// Constructor
{
   // Ensure dictionary is ready
   dict();
}
//---------------------------------------------------------------------------
size_t TokenizerCompressor::getUncompressedSize(std::span<std::byte> input) const
// Get the uncompressed size
{
   if (input.size() < sizeof(Footer)) throw std::runtime_error("TokenizerCompressor: input too small");
   const auto footer = unalignedLoad<Footer>(input.data() + input.size() - sizeof(Footer));
   return footer.uncompressedSize;
}
//---------------------------------------------------------------------------
void TokenizerCompressor::prepare(size_t /*inputSize*/)
// Prepare the compressor
{
   // No per-input preparation needed; dictionary is static.
   (void) dict();
}
//---------------------------------------------------------------------------
void TokenizerCompressor::compress(std::span<std::byte> input, std::vector<std::byte>& output, bool /*exportDictionary*/, std::optional<std::span<std::byte>> /*dictionary*/)
// Compress the data
{
   const auto& d = dict();

   // Worst case: 1-byte tokens => input.size() ids.
   output.clear();
   output.resize(input.size() * sizeof(uint32_t) + sizeof(Footer));

   auto* writer = reinterpret_cast<uint16_t*>(output.data());
   const std::byte* reader = input.data();
   const std::byte* limit = input.data() + input.size();

   auto& shortTokenMap = d.shortTokenMap;
   auto& tokenMap3 = d.tokenMap3;
   auto* shortTokenArray2 = d.shortTokenArray2.data();

   auto match = [&](uint64_t text) {
      // 2-byte token array
      uint32_t id = shortTokenArray2[static_cast<uint16_t>(text)];
      uint32_t len = 1 + static_cast<int>(id > 255);

      // ShortTokenMap (length 4..8)
      {
         const uint32_t prefix = static_cast<uint32_t>(text);
         auto [begin, end] = shortTokenMap.find(prefix);

         // Fast path: unroll first 4 entries.
         const auto& e1 = *(begin + 0);
         const auto& e2 = *(begin + 1);
         const auto& e3 = *(begin + 2);
         const auto& e4 = *(begin + 3);

         if ((text & e1.mask) == e1.text) {
            id = e1.id;
            len = e1.length;
            goto next;
         }
         if ((text & e2.mask) == e2.text) {
            id = e2.id;
            len = e2.length;
            goto next;
         }
         if ((text & e3.mask) == e3.text) {
            id = e3.id;
            len = e3.length;
            goto next;
         }
         if ((text & e4.mask) == e4.text) {
            id = e4.id;
            len = e4.length;
            goto next;
         }

         // Remaining bucket entries.
         for (begin = begin + 4; begin < end; ++begin) {
            const auto& e = *begin;
            if ((text & e.mask) == e.text) {
               id = e.id;
               len = e.length;
               goto next;
            }
         }
      }

      // 3-byte token map
      {
         const uint32_t key = (static_cast<uint32_t>(text) << 8);
         auto it = tokenMap3.find(key);
         if (it != tokenMap3.end()) {
            id = it->second;
            len = 3;
            goto next;
         }
      }

   next:
      *(writer++) = static_cast<uint16_t>(id);
      reader += len;
   };

   while (reader < limit - 8) {
      const uint64_t text = unalignedLoad<uint64_t>(reader);
      match(text);
   }

   while (reader < limit) {
      auto remaining = limit - reader;
      uint64_t text = unalignedLoad<uint64_t>(reader - 8 + remaining) >> (64 - remaining * 8);
      if (remaining == 1) {
         // 1-byte fallback
         *(writer++) = static_cast<uint16_t>(text);
         reader += 1;
      } else {
         match(text);
      }
   }

   const size_t tokenBytes = reinterpret_cast<std::byte*>(writer) - output.data();
   output.resize(tokenBytes + sizeof(Footer));
   reinterpret_cast<Footer*>(output.data() + tokenBytes)->uncompressedSize = input.size();
}
//---------------------------------------------------------------------------
void TokenizerCompressor::decompress(std::span<std::byte> input, std::vector<std::byte>& output, std::vector<std::vector<std::byte>>& /*temporaryBuffers*/)
// Decompress the data
{
   const auto& d = dict();

   if (input.size() < sizeof(Footer)) throw std::runtime_error("TokenizerCompressor: input too small");

   const auto footer = unalignedLoad<Footer>(input.data() + input.size() - sizeof(Footer));

   const size_t tokenBytes = input.size() - sizeof(Footer);
   if ((tokenBytes % sizeof(uint16_t)) != 0) {
      throw std::runtime_error("TokenizerCompressor: token stream size mismatch");
   }
   const uint64_t tokenCount = tokenBytes / sizeof(uint16_t);

   output.resize(footer.uncompressedSize + sizeof(uint64_t)); // Pad by 8 bytes to simplify token loading logic

   const auto* lengths = d.lengths.data();
   const auto* tokens = d.tokens.data();

   const auto* reader = reinterpret_cast<const uint16_t*>(input.data());
   const auto* limit = reader + tokenCount;
   std::byte* writer = output.data();

   for (; reader + 4 <= limit; reader += 4) {
      auto token0 = reader[0];
      auto token1 = reader[1];
      auto token2 = reader[2];
      auto token3 = reader[3];

      unalignedStore(writer, tokens[token0]);
      writer += lengths[token0];
      unalignedStore(writer, tokens[token1]);
      writer += lengths[token1];
      unalignedStore(writer, tokens[token2]);
      writer += lengths[token2];
      unalignedStore(writer, tokens[token3]);
      writer += lengths[token3];
   }
   for (; reader + 1 <= limit; reader += 1) {
      auto token = reader[0];
      unalignedStore(writer, tokens[token]);
      writer += lengths[token];
   }

   output.resize(writer - output.data());
}
//---------------------------------------------------------------------------
std::string TokenizerCompressor::getName()
// Get the name of the compressor
{
   return "tokenizer";
}
//---------------------------------------------------------------------------
} // namespace sgtt::compressor
//---------------------------------------------------------------------------
