#pragma once

// The corpora and needle catalogs under bench/data: prefilter_corpus writes
// the streams, onpair's bench_needles the catalog beside each.

#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <string>
#include <utility>
#include <vector>

#include "utils.hpp"

namespace fsst::search::prefilter::scan::bench {

namespace fs = std::filesystem;

constexpr const char* STREAMS[] = {"ch/hits/URL_1m"};
constexpr const char* ENCODING = "fsst";
// 1 MiB of code stream, the same working set the u16 sweep walks at two bytes
// a code, so the two are read against one memory-bandwidth ceiling. At 8 MiB
// the compare kernel at K = 1 measured the c7a's 99.8 GB/s sequential read
// rather than itself; 1 MiB stays under every fitted machine's ceiling.
constexpr size_t CODES = 1u << 20;
constexpr size_t CHECK_CODES = CODES / 16;
constexpr size_t SAMPLE = 0;

struct Corpus {
    std::vector<uint8_t> codes;
    std::vector<uint32_t> row_offsets;
};

struct NeedleSet {
    size_t count;
    double target;
    double achieved;
    size_t sample;
    std::vector<uint8_t> needles;
};

inline fs::path data_dir() { return fs::path(BENCH_DIR) / "data"; }

inline std::pair<fs::path, fs::path> paths(const std::string& stream, const std::string& encoding) {
    fs::path base = data_dir() / stream;
    return {base.string() + "." + encoding + ".csv", base.string() + "." + encoding + ".needles.csv"};
}

inline std::string read_file(const fs::path& path, const char* producer) {
    if (!fs::exists(path)) {
        std::fprintf(stderr, "missing %s; run %s first\n", path.c_str(), producer);
        std::exit(1);
    }
    return read_file(path);
}

inline uint8_t code(unsigned value, const fs::path& path) {
    if (value > 255) {
        std::fprintf(stderr, "%s: code %u is wider than a byte\n", path.c_str(), value);
        std::exit(1);
    }
    return static_cast<uint8_t>(value);
}

// `1,2,3\n4,5\n`: one line is one row, one field is one code.
inline Corpus load_corpus(const fs::path& path) {
    std::string text = read_file(path, "prefilter_corpus");
    Corpus corpus;
    corpus.codes.reserve(text.size() / 2);
    corpus.row_offsets.push_back(0);
    unsigned value = 0;
    bool digits = false;
    for (char ch : text) {
        if (ch >= '0' && ch <= '9') {
            value = value * 10 + static_cast<unsigned>(ch - '0');
            digits = true;
        } else if (ch == ',' || ch == '\n') {
            if (digits) corpus.codes.push_back(code(value, path));
            if (ch == '\n') corpus.row_offsets.push_back(static_cast<uint32_t>(corpus.codes.size()));
            value = 0;
            digits = false;
        } else {
            std::fprintf(stderr, "unexpected byte in %s\n", path.c_str());
            std::exit(1);
        }
    }
    return corpus;
}

// `l,k,target,achieved,rows,sample,codes`, needles separated by `|` and their
// codes by spaces. Only the one-code sets.
inline std::vector<NeedleSet> load_needles(const fs::path& path) {
    std::string text = read_file(path, "onpair's bench_needles example");
    std::vector<NeedleSet> sets;
    bool header = true;
    for (const std::string& line : split(text, '\n')) {
        if (header) {
            header = false;
            continue;
        }
        if (line.rfind("1,", 0) != 0) continue;
        std::vector<std::string> field = split(line, ',');
        NeedleSet set;
        set.count = std::stoul(field[1]);
        set.target = std::stod(field[2]);
        set.achieved = std::stod(field[3]);
        set.sample = std::stoul(field[5]);
        for (const std::string& needle : split(field[6], '|'))
            for (const std::string& c : split(needle, ' '))
                if (!c.empty()) set.needles.push_back(code(std::stoul(c), path));
        sets.push_back(std::move(set));
    }
    return sets;
}

// The row layer cut to the codes being scanned, closed over the row the cut
// fell inside, so every code a bit can be set for has a row.
inline std::vector<uint32_t> row_layer(const std::vector<uint32_t>& row_offsets, size_t codes) {
    std::vector<uint32_t> layer;
    for (uint32_t offset : row_offsets) {
        if (offset > codes) break;
        layer.push_back(offset);
    }
    if (layer.back() < codes) layer.push_back(static_cast<uint32_t>(codes));
    return layer;
}

}  // namespace fsst::search::prefilter::scan::bench
