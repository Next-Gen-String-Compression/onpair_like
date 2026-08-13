#pragma once
//---------------------------------------------------------------------------
#include <array>
#include <unordered_map>
#include "util/CompilerHints.hpp"
//---------------------------------------------------------------------------
// Static Global Token Table
// (c) 2025 Tobias Schmidt
//---------------------------------------------------------------------------
namespace sgtt::tokenizer::openai100kpatched {
//---------------------------------------------------------------------------
/// Token sizes for OpenAI100kPatched tokens
extern const std::array<uint8_t, 65536> token_lengths;
/// Token dictionary for OpenAI100kPatched tokens
extern const std::array<uint8_t, 1048576> token_dict;
//---------------------------------------------------------------------------
} // namespace sgtt::tokenizer::openai100kpatched
//---------------------------------------------------------------------------
