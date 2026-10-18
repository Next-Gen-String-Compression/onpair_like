#include "duckdb/common/fsst.hpp"

#include "duckdb/common/local_file_system.hpp"
#include "duckdb/common/string_util.hpp"
#include "duckdb/common/types/hash.hpp"

#include <cctype>
#include <cstdio>

namespace duckdb {

string FSSTPrimitives::DecompressValue(void *duckdb_fsst_decoder, const char *compressed_string,
                                       const idx_t compressed_string_len, vector<unsigned char> &decompress_buffer) {
	auto compressed_string_ptr = reinterpret_cast<const unsigned char *>(compressed_string);
	auto fsst_decoder = static_cast<fsst_decoder_t *>(duckdb_fsst_decoder);
	auto decompressed_string_size = fsst_decompress(fsst_decoder, compressed_string_len, compressed_string_ptr,
	                                                decompress_buffer.size(), decompress_buffer.data());

	D_ASSERT(!decompress_buffer.empty());
	D_ASSERT(decompressed_string_size <= decompress_buffer.size() - 1);
	return string(char_ptr_cast(decompress_buffer.data()), decompressed_string_size);
}

string FSSTPrimitives::DecoderToString(void *duckdb_fsst_decoder) {
	if (!duckdb_fsst_decoder) {
		return "FSST decoder: nullptr\n";
	}
	auto decoder = static_cast<fsst_decoder_t *>(duckdb_fsst_decoder);
	string result;
	result += StringUtil::Format("FSST symbol table (version=%llu, zeroTerminated=%u)\n",
	                             (unsigned long long)decoder->version, (unsigned)decoder->zeroTerminated);
	// symbol[x] holds the byte sequence for code x in little-endian, len[x] gives its valid byte length
	for (idx_t code = 0; code < 255; code++) {
		auto len = decoder->len[code];
		if (len == 0) {
			continue;
		}
		auto symbol = decoder->symbol[code];
		string literal;
		string hex;
		for (idx_t b = 0; b < len; b++) {
			auto byte = static_cast<unsigned char>((symbol >> (8 * b)) & 0xFF);
			hex += StringUtil::Format("%02X ", (unsigned)byte);
			literal += std::isprint(byte) ? static_cast<char>(byte) : '.';
		}
		result += StringUtil::Format("  [%3llu] len=%u  bytes=%-24s '%s'\n", (unsigned long long)code, (unsigned)len,
		                             hex.c_str(), literal.c_str());
	}
	return result;
}

void FSSTPrimitives::PrintDecoder(void *duckdb_fsst_decoder) {
	auto str = DecoderToString(duckdb_fsst_decoder);
	fprintf(stderr, "%s", str.c_str());
	fflush(stderr);
}

void FSSTPrimitives::SaveDecoder(void *duckdb_fsst_decoder, const string &directory) {
	auto str = DecoderToString(duckdb_fsst_decoder);
	auto hash = Hash(str.c_str(), str.size());
	auto file_name = StringUtil::Format("%016llx.txt", (unsigned long long)hash);

	LocalFileSystem fs;
	fs.CreateDirectoriesRecursive(directory);
	auto path = fs.JoinPath(directory, file_name);
	if (fs.FileExists(path)) {
		return;
	}
	auto handle = fs.OpenFile(path, FileFlags::FILE_FLAGS_WRITE | FileFlags::FILE_FLAGS_FILE_CREATE_NEW);
	handle->Write(const_cast<char *>(str.c_str()), str.size());
}

} // namespace duckdb
