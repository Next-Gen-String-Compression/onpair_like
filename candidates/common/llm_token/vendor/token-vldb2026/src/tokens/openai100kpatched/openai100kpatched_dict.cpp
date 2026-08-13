#include "tokens/openai100kpatched/openai100kpatched.hpp"
//---------------------------------------------------------------------------
// Static Global Token Table
// (c) 2025 Tobias Schmidt
//---------------------------------------------------------------------------
namespace sgtt::tokenizer::openai100kpatched {
//---------------------------------------------------------------------------
// Token sizes for OpenAI100kPatched tokens
const std::array<uint8_t, 65536> token_lengths = {
#embed "openai100kpatched_lengths.bin"
};
//---------------------------------------------------------------------------
// Token dictionary for OpenAI100kPatched tokens
alignas(64) const std::array<uint8_t, 1048576> token_dict = {
#embed "openai100kpatched_dict.bin"
};
//---------------------------------------------------------------------------
} // namespace sgtt::tokenizer::openai100kpatched
//---------------------------------------------------------------------------
