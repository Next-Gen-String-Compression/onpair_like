#pragma once

// Rows through the fork's encoder as the tests and tools read them: codes in
// byte order, one stream with ascending row offsets, the counts stored in
// the symbol table, and the dictionary and frequency the planner takes.

#include <stdexcept>
#include <string>
#include <vector>

#include "fsst.h"
#include "prefilter.hpp"

namespace fsst::search::prefilter {

struct CompressedRows {
    std::vector<uint8_t> codes;
    std::vector<uint32_t> offsets;  // row_count + 1, the last equal to codes.size()
    std::vector<unsigned char> header;  // fsst_export, counts included
    fsst_decoder_t decoder;
    Dictionary dict;
    Frequency freq;

    size_t row_count() const { return offsets.size() - 1; }
    scan::Stream stream() const { return {codes.data(), codes.size(), offsets.data(), offsets.size()}; }
};

// `len` and `str` as fsst_create takes them: the encoder's API is not const.
inline CompressedRows compress_rows(size_t n, size_t* len, const unsigned char** str) {
    size_t raw_bytes = 0;
    for (size_t i = 0; i < n; ++i) raw_bytes += len[i];
    fsst_encoder_t* encoder = fsst_create(n, len, str, 0);
    fsst_sort_codes(encoder);
    std::vector<unsigned char> out(2 * raw_bytes + 8 * n + 8);
    std::vector<size_t> len_out(n);
    std::vector<unsigned char*> str_out(n);
    unsigned char* cursor = out.data();
    for (size_t done = 0; done < n;) {
        const size_t count = fsst_compress(encoder, n - done, len + done, str + done,
                                           static_cast<size_t>(out.data() + out.size() - cursor), cursor,
                                           len_out.data() + done, str_out.data() + done);
        if (count == 0) throw std::runtime_error("fsst_compress made no progress at row " + std::to_string(done));
        done += count;
        cursor = str_out[done - 1] + len_out[done - 1];
    }
    CompressedRows c;
    c.codes.assign(out.data(), cursor);
    for (size_t i = 0; i < n; ++i) c.offsets.push_back(static_cast<uint32_t>(str_out[i] - out.data()));
    c.offsets.push_back(static_cast<uint32_t>(c.codes.size()));

    unsigned int counts[256] = {};
    for (uint8_t code : c.codes) ++counts[code];
    fsst_set_frequency(encoder, counts);
    c.header.resize(FSST_MAXHEADER);
    c.header.resize(fsst_export(encoder, c.header.data()));
    fsst_destroy(encoder);
    fsst_import(&c.decoder, c.header.data());
    c.dict = Dictionary::from(c.decoder, symbol_count(c.header.data()));
    c.freq = Frequency::of_counts(c.decoder.count);
    return c;
}

inline CompressedRows compress_rows(const std::vector<std::string>& rows) {
    std::vector<size_t> len(rows.size());
    std::vector<const unsigned char*> str(rows.size());
    for (size_t i = 0; i < rows.size(); ++i) {
        len[i] = rows[i].size();
        str[i] = reinterpret_cast<const unsigned char*>(rows[i].data());
    }
    return compress_rows(rows.size(), len.data(), str.data());
}

}  // namespace fsst::search::prefilter
