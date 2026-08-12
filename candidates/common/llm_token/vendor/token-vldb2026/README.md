# Vendored: token-vldb2026 (SGTT — Static Global Token Table)

Source: https://gitlab.db.in.tum.de/schmidt/token-vldb2026
Commit: `4b993419be2e638800ff3584ac52139d624a8d55`
Author: (c) 2025 Tobias Schmidt (TUM). No LICENSE file is present upstream;
files are vendored verbatim for research/benchmarking with attribution.

What is vendored (verbatim, unmodified):

| path | role |
|---|---|
| `src/util/CompilerHints.hpp` | `unalignedLoad/Store`, `uint128_t`, inline hints |
| `src/util/Hash.hpp` | `fasthash`, `log2ceil` (used by both hash maps) |
| `src/util/LossyHashMap.hpp` | lossy static hash map (3-byte token lookup) |
| `src/util/MultiHashMap.hpp` | static bucketed multimap (4..7-byte token lookup) |
| `src/compressor/TokenizerCompressor.{cpp,hpp}` | REFERENCE ONLY — not compiled; the shared core in `../../llm_token_table.hpp` adapts its logic (see header comment there for the deltas) |
| `src/tokens/openai100kpatched/*_dict.bin`, `*_lengths.bin` | the token table: 65536 tokens, 16-byte stride, lengths 1..7 |
| `src/tokens/openai100kpatched/openai100kpatched.{hpp,cpp}` | REFERENCE ONLY — upstream embeds the .bin via `#embed` (needs GCC 15+/Clang 19+); the candidates embed them via `.incbin` in `../../tokens.S.in` instead |

Token-table invariants (verified against the vendored .bin files; the
candidates re-check them at init and fail build() if violated):

- 65536 tokens, dict stride 16 bytes, lengths in [1, 7]
- token IDs 0..255 are the identity single-byte tokens (`dict[16*i] == i`),
  which the encoder's 1-byte fallback and the tail path rely on
- no multi-byte token contains a 0x00 byte, which makes the zero-padded
  end-of-row tail matching safe (a token can never match past the row end)
