// Encodes a raw corpus, one value per line, into an FSST code stream in the
// layout the sweeps and onpair's bench_needles read: one row per line, the
// codes as comma-separated decimals, escape markers and their literal bytes
// both written as codes. The symbol table goes beside it as <out>.symtab.
//
//   prefilter_corpus <raw.csv> <out.fsst.csv>

#include <cstdio>
#include <fstream>
#include <string>
#include <vector>

#include "../../compress.hpp"
#include "utils.hpp"

using namespace fsst::search::prefilter;

int main(int argc, char** argv) {
    if (argc != 3) {
        std::fprintf(stderr, "usage: %s <raw.csv> <out.fsst.csv>\n", argv[0]);
        return 2;
    }
    try {
        const std::string text = scan::bench::read_file(argv[1]);
        std::vector<std::string> rows;
        for (size_t at = 0; at < text.size();) {
            size_t end = text.find('\n', at);
            if (end == std::string::npos) end = text.size();
            rows.push_back(text.substr(at, end - at));
            at = end + 1;
        }
        size_t raw_bytes = 0;
        for (const std::string& row : rows) raw_bytes += row.size();

        const CompressedRows c = compress_rows(rows);
        std::string csv;
        csv.reserve(raw_bytes * 2);
        char digits[8];
        for (size_t i = 0; i < c.row_count(); ++i) {
            for (size_t j = c.offsets[i]; j < c.offsets[i + 1]; ++j) {
                int w = std::snprintf(digits, sizeof digits, "%u", static_cast<unsigned>(c.codes[j]));
                if (j != c.offsets[i]) csv.push_back(',');
                csv.append(digits, static_cast<size_t>(w));
            }
            csv.push_back('\n');
        }
        std::ofstream(argv[2], std::ios::binary) << csv;
        std::ofstream(std::string(argv[2]) + ".symtab", std::ios::binary)
            .write(reinterpret_cast<const char*>(c.header.data()), static_cast<std::streamsize>(c.header.size()));

        const size_t codes = c.codes.size(), escapes = c.freq.count[ESCAPE];
        std::printf("%zu rows, %zu bytes -> %zu codes (%zu escapes, %.1f%% of codes), ratio %.2f\n", rows.size(),
                    raw_bytes, codes, escapes, 100.0 * escapes / codes, double(raw_bytes) / codes);
        return 0;
    } catch (const std::exception& e) {
        std::fprintf(stderr, "%s\n", e.what());
        return 1;
    }
}
