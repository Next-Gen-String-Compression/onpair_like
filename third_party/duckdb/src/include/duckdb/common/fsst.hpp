//===----------------------------------------------------------------------===//
//                         DuckDB
//
// duckdb/common/fsst.hpp
//
//
//===----------------------------------------------------------------------===//

#pragma once

#include "duckdb/common/typedefs.hpp"
#include "duckdb/common/vector.hpp"
#include "duckdb/common/types/value.hpp"
#include "duckdb/common/fsst.hpp"
#include "fsst.h"
#include "duckdb/common/vector/string_vector.hpp"

#include <algorithm>
#include <cstring>

namespace duckdb {

class Value;
class Vector;
struct string_t;

typedef struct {
	uint32_t dict_size;
	uint32_t dict_end;
	uint32_t bitpacking_width;
	uint32_t fsst_symbol_table_offset;
} fsst_compression_header_t;

class FSSTPrimitives {
private:
	// This allows us to decode FSST strings efficiently directly into a string_t (if inlined)
	// The decode will overflow a bit into "extra_space", but "str" will contain the full string
	struct StringWithExtraSpace {
		string_t str;
		// INLINE_BYTES instead of INLINE_LENGTH so this isn't 0-length array when building with DUCKDB_DEBUG_NO_INLINE
		uint64_t extra_space[string_t::INLINE_BYTES];
	};

public:
	static string_t DecompressValue(void *duckdb_fsst_decoder, ArenaAllocator &str_allocator,
	                                const char *compressed_string, const idx_t compressed_string_len) {
		// todo: Maybe we can make the trailing path of fsst with the 3 symbols faster
		// as we can still use the four byte symbol mask if we have space in the input buffer
		// so we can still do the four byte symbol table load. The tail path of
		// fsst is called often!
		const auto max_uncompressed_length = compressed_string_len * 8 + 32;
		const auto fsst_decoder = static_cast<fsst_decoder_t *>(duckdb_fsst_decoder);
		const auto compressed_string_ptr = (const unsigned char *)compressed_string; // NOLINT
		const auto target_ptr = StringVector::AllocateShrinkableBuffer(str_allocator, max_uncompressed_length);
		const auto decompressed_string_size = fsst_decompress(
		    fsst_decoder, compressed_string_len, compressed_string_ptr, max_uncompressed_length, target_ptr);
		return StringVector::FinalizeShrinkableBuffer(str_allocator, target_ptr, max_uncompressed_length,
		                                              decompressed_string_size);
	}
	static string_t DecompressInlinedValue(void *duckdb_fsst_decoder, const char *compressed_string,
	                                       const idx_t compressed_string_len) {
		const auto fsst_decoder = static_cast<fsst_decoder_t *>(duckdb_fsst_decoder);
		const auto compressed_string_ptr = (const unsigned char *)compressed_string; // NOLINT
		StringWithExtraSpace result;
		const auto target_ptr = (unsigned char *)result.str.GetPrefixWriteable(); // NOLINT
		const auto decompressed_string_size =
		    fsst_decompress(fsst_decoder, compressed_string_len, compressed_string_ptr,
		                    string_t::INLINE_LENGTH + sizeof(StringWithExtraSpace::extra_space), target_ptr);
		if (decompressed_string_size > string_t::INLINE_LENGTH) {
			throw DataCorruptionException(
			    "Corrupt database file: decoded FSST string of >=%llu bytes (should be <=%llu bytes)",
			    decompressed_string_size, string_t::INLINE_LENGTH);
		}
		D_ASSERT(decompressed_string_size <= string_t::INLINE_LENGTH);
		result.str.SetSizeAndFinalize(UnsafeNumericCast<uint32_t>(decompressed_string_size), string_t::INLINE_LENGTH);
		return result.str;
	}
	static string DecompressValue(void *duckdb_fsst_decoder, const char *compressed_string,
	                              const idx_t compressed_string_len, vector<unsigned char> &decompress_buffer);
	//! Format the FSST symbol table (decoder) as a human-readable string for debugging.
	static string DecoderToString(void *duckdb_fsst_decoder);
	//! Print the FSST symbol table (decoder) to stderr for debugging.
	static void PrintDecoder(void *duckdb_fsst_decoder);
	//! Save the FSST symbol table (decoder) to a file inside `directory`, named by a hash of its contents.
	//! One file per distinct decoder; existing files are left untouched.
	static void SaveDecoder(void *duckdb_fsst_decoder, const string &directory);
};

//! A self-contained FSST encoder reconstructed from a decoder's symbol table.
//! Compresses strings via greedy longest-match. Intended for compressing small constants
//! (e.g. filter predicates), not bulk data. Assumes a little-endian host (as does the FSST format).
class FSSTEncoder {
public:
	//! The tables are built on the first Compress: most scans never compress a needle
	explicit FSSTEncoder(buffer_ptr<void> decoder_p) : decoder(std::move(decoder_p)) {
	}

	//! Compress the input via greedy longest-match, returning the compressed bytes.
	string Compress(const char *input_p, idx_t input_len) const {
		if (!built) {
			Build();
		}
		const auto input = reinterpret_cast<const unsigned char *>(input_p);
		string result;
		result.reserve(input_len);
		idx_t pos = 0;
		while (pos < input_len) {
			const idx_t remaining = input_len - pos;
			uint64_t word = 0;
			memcpy(&word, input + pos, remaining >= 8 ? 8 : remaining);
			const auto &bucket = buckets[word & 0xFF];
			bool matched = false;
			for (const auto &symbol : bucket) {
				if (symbol.len <= remaining && (word & symbol.mask) == symbol.value) {
					result.push_back(static_cast<char>(symbol.code));
					pos += symbol.len;
					matched = true;
					break;
				}
			}
			if (!matched) {
				result.push_back(static_cast<char>(FSST_ESC));
				result.push_back(static_cast<char>(input[pos]));
				pos++;
			}
		}
		return result;
	}

private:
	struct Symbol {
		uint64_t value; //! symbol bytes, little-endian, masked to len
		uint64_t mask;  //! low (len * 8) bits set
		uint8_t len;    //! symbol byte-length (1-8)
		uint8_t code;   //! FSST code for this symbol
	};

	void Build() const {
		const auto &table = *static_cast<const fsst_decoder_t *>(decoder.get());
		for (uint16_t code = 0; code < 255; code++) {
			const idx_t len = table.len[code];
			if (len == 0 || len > 8) {
				continue;
			}
			const uint64_t mask = len >= 8 ? ~0ULL : ((1ULL << (8 * len)) - 1);
			const uint64_t value = table.symbol[code] & mask;
			buckets[value & 0xFF].push_back({value, mask, static_cast<uint8_t>(len), static_cast<uint8_t>(code)});
		}
		// sort each bucket by descending length so the first match is the longest
		for (auto &bucket : buckets) {
			std::sort(bucket.begin(), bucket.end(), [](const Symbol &a, const Symbol &b) { return a.len > b.len; });
		}
		built = true;
	}

	//! The fsst_decoder_t the tables come from, kept alive here
	buffer_ptr<void> decoder;
	mutable bool built = false;
	//! Symbols grouped by first byte, each bucket sorted by descending length.
	mutable vector<Symbol> buckets[256];
};

} // namespace duckdb
