#pragma once
//---------------------------------------------------------------------------
#include "compressor/Compressor.hpp"
#include <cstddef>
#include <span>
#include <string>
#include <vector>
//---------------------------------------------------------------------------
// Static Global Token Table
// (c) 2025 Tobias Schmidt
//---------------------------------------------------------------------------
namespace sgtt::compressor {
//---------------------------------------------------------------------------
/// Tokenizer-only compressor (OpenAI100kPatched).
///
/// Hard-wired configuration:
/// - tokenizer: OpenAI100kPatched
/// - short_token_optimization: map_short_tokens4
/// - hash_map: lossy_map
/// - token_id_encoding: sparse
///
/// Format: uint32 token IDs + uint64 uncompressed size footer.
class TokenizerCompressor final : public Compressor {
   public:
   /// Create tokenizer-only compressor.
   TokenizerCompressor();

   /// Get the uncompressed size
   size_t getUncompressedSize(std::span<std::byte> input) const override;
   /// Prepare the compressor
   void prepare(size_t inputSize) override;

   /// Compress the data
   void compress(std::span<std::byte> input, std::vector<std::byte>& output, bool exportDictionary = false, std::optional<std::span<std::byte>> dictionary = std::nullopt) override;
   /// Decompress the data
   void decompress(std::span<std::byte> input, std::vector<std::byte>& output, std::vector<std::vector<std::byte>>& temporaryBuffers) override;

   /// Get the name of the compressor
   std::string getName() override;
};
//---------------------------------------------------------------------------
}
//---------------------------------------------------------------------------
