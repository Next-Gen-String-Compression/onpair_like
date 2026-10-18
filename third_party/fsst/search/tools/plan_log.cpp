// Logs the prefilter's alignment graph and min cut for every needle in every
// row group of a column, as three CSVs appended to --out:
//
//   plan_log --col C.col --needles N.ndl --dataset NAME --chunk-rows 524288 --out DIR
//
// queries.csv: uuid, dataset, row_group, needle, algorithm, rows,
//   needle_selectivity, cut_selectivity_estimated, cut_selectivity_measured.
// symbols.csv: dataset, row_group, algorithm, token_id, content, frequency;
//   the escape code is a row of its own with empty content.
// graph.csv:   uuid, from, to, label_type, label, cut.
// Needles and symbols are base64. The uuid is the hash onpair's
// examples/plan_log.rs writes, over (dataset, row_group, needle, algorithm).
// Each row group is trained and compressed as the fsst_prefilter candidate
// does: fsst_create, fsst_sort_codes, one fsst_compress over every row.
//
// .col is u64 num_rows, u64 payload_len, num_rows+1 u64 offsets, payload.
// .ndl is u64 count, then u64 len and the bytes per needle. Little-endian.

#include <cstdio>
#include <cstring>
#include <stdexcept>
#include <string>
#include <string_view>
#include <sys/stat.h>
#include <vector>

#include "prefilter/compress.hpp"
#include "prefilter/scan/bench/utils.hpp"
#include "tools/trace.hpp"

namespace pf = fsst::search::prefilter;
using pf::scan::bench::read_file;

static uint64_t u64_at(const std::string& b, size_t& at) {
    uint64_t v;
    std::memcpy(&v, b.data() + at, 8);
    at += 8;
    return v;
}

static std::string base64(const uint8_t* p, size_t n) {
    static const char* A = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    std::string out;
    for (size_t i = 0; i < n; i += 3) {
        uint32_t v = uint32_t(p[i]) << 16;
        if (i + 1 < n) v |= uint32_t(p[i + 1]) << 8;
        if (i + 2 < n) v |= p[i + 2];
        for (size_t k = 0; k < 4; ++k) out += (k <= n - i) ? A[(v >> (18 - 6 * k)) & 63] : '=';
    }
    return out;
}

static uint64_t fnv1a(const std::string& s, uint64_t h) {
    for (unsigned char c : s) h = (h ^ c) * 0x100000001b3ull;
    return h;
}

static std::string uuid(const std::string& dataset, size_t row_group, const std::string& needle_b64,
                        const std::string& algorithm) {
    const std::string key = dataset + '\x1f' + std::to_string(row_group) + '\x1f' + needle_b64 + '\x1f' + algorithm;
    const uint64_t hi = fnv1a(key, 0xcbf29ce484222325ull), lo = fnv1a(key, hi);
    char h[33];
    std::snprintf(h, sizeof h, "%016llx%016llx", (unsigned long long)hi, (unsigned long long)lo);
    const std::string s(h);
    return s.substr(0, 8) + '-' + s.substr(8, 4) + '-' + s.substr(12, 4) + '-' + s.substr(16, 4) + '-' + s.substr(20);
}

static FILE* csv(const std::string& dir, const char* name, const char* header) {
    const std::string path = dir + "/" + name;
    struct stat st;
    const bool fresh = stat(path.c_str(), &st) != 0 || st.st_size == 0;
    FILE* f = std::fopen(path.c_str(), "a");
    if (!f) throw std::runtime_error(path + ": cannot open");
    if (fresh) std::fprintf(f, "%s\n", header);
    return f;
}

template <typename C>
static std::string list(const C& codes) {
    std::string s = "\"[";
    for (size_t i = 0; i < codes.size(); ++i) s += (i ? "," : "") + std::to_string(unsigned(codes[i]));
    return s + "]\"";
}

int main(int argc, char** argv) {
    std::string col, needles_path, dataset, out;
    size_t chunk_rows = 0;
    for (int i = 1; i + 1 < argc; i += 2) {
        const std::string flag = argv[i], value = argv[i + 1];
        if (flag == "--col") col = value;
        else if (flag == "--needles") needles_path = value;
        else if (flag == "--dataset") dataset = value;
        else if (flag == "--out") out = value;
        else if (flag == "--chunk-rows") chunk_rows = std::stoull(value);
        else throw std::runtime_error("unknown argument " + flag);
    }
    if (col.empty() || needles_path.empty() || dataset.empty() || out.empty())
        throw std::runtime_error("--col, --needles, --dataset and --out are required");
    const std::string algorithm = "fsst";

    const std::string raw = read_file(col);
    size_t at = 0;
    const size_t total_rows = u64_at(raw, at), payload_len = u64_at(raw, at);
    std::vector<uint64_t> offsets(total_rows + 1);
    for (auto& o : offsets) o = u64_at(raw, at);
    const uint8_t* payload = reinterpret_cast<const uint8_t*>(raw.data()) + at;
    if (at + payload_len != raw.size()) throw std::runtime_error(col + ": payload length mismatch");

    const std::string nraw = read_file(needles_path);
    at = 0;
    std::vector<std::string> needles(u64_at(nraw, at));
    for (auto& n : needles) {
        const size_t len = u64_at(nraw, at);
        n.assign(reinterpret_cast<const char*>(nraw.data() + at), len);
        at += len;
    }

    mkdir(out.c_str(), 0755);
    FILE* queries = csv(out, "queries.csv",
                        "uuid,dataset,row_group,needle,algorithm,rows,needle_selectivity,"
                        "cut_selectivity_estimated,cut_selectivity_measured");
    FILE* symbols = csv(out, "symbols.csv", "dataset,row_group,algorithm,token_id,content,frequency");
    FILE* graph = csv(out, "graph.csv", "uuid,from,to,label_type,label,cut");

    const size_t step = chunk_rows == 0 ? total_rows : chunk_rows;
    for (size_t start = 0, row_group = 0; start < total_rows; start += step, ++row_group) {
        const size_t rows = std::min(step, total_rows - start);
        std::vector<size_t> len_in(rows);
        std::vector<const unsigned char*> str_in(rows);
        size_t bytes = 0;
        for (size_t r = 0; r < rows; ++r) {
            len_in[r] = offsets[start + r + 1] - offsets[start + r];
            str_in[r] = payload + offsets[start + r];
            bytes += len_in[r];
        }
        const pf::CompressedRows c = pf::compress_rows(rows, len_in.data(), str_in.data());
        const pf::Dictionary& dict = c.dict;
        const pf::Frequency& freq = c.freq;

        for (size_t c = 0; c < dict.count; ++c)
            std::fprintf(symbols, "%s,%zu,%s,%zu,%s,%u\n", dataset.c_str(), row_group, algorithm.c_str(), c,
                         base64(dict.symbol(c), dict.length(c)).c_str(), freq.count[c]);
        // The escape code is no symbol, but escape probes are weighed by it.
        std::fprintf(symbols, "%s,%zu,%s,%u,,%u\n", dataset.c_str(), row_group, algorithm.c_str(), unsigned(pf::ESCAPE),
                     freq.count[pf::ESCAPE]);

        for (const std::string& needle : needles) {
            if (needle.empty()) throw std::runtime_error("empty needle");
            const auto* np = reinterpret_cast<const uint8_t*>(needle.data());
            std::vector<size_t> matching;
            for (size_t r = 0; r < rows; ++r) {
                const std::string_view row(reinterpret_cast<const char*>(str_in[r]), len_in[r]);
                if (row.find(needle) != std::string_view::npos) matching.push_back(r);
            }

            const pf::Trace t = pf::trace(np, needle.size(), dict, freq, rows);
            std::vector<size_t> admitted;
            pf::superset_rows(c.stream(), t.analysis, admitted);
            if (!std::includes(admitted.begin(), admitted.end(), matching.begin(), matching.end()))
                throw std::runtime_error(dataset + " row group " + std::to_string(row_group) +
                                         ": cover misses a match");

            const std::string needle_b64 = base64(np, needle.size());
            const std::string id = uuid(dataset, row_group, needle_b64, algorithm);
            const double estimated = std::min(1.0, double(t.analysis.covered_frequency) / double(rows));
            std::fprintf(queries, "%s,%s,%zu,%s,%s,%zu,%.17g,%.17g,%.17g\n", id.c_str(), dataset.c_str(), row_group,
                         needle_b64.c_str(), algorithm.c_str(), rows, double(matching.size()) / double(rows), estimated,
                         double(admitted.size()) / double(rows));

            for (size_t e = 0; e < t.graph.edges.size(); ++e) {
                const pf::Edge& edge = t.graph.edges[e];
                std::string kind, label;
                switch (edge.probe) {
                    case pf::Probe::Point: kind = "code", label = std::to_string(edge.codes[0]); break;
                    case pf::Probe::Escape: kind = "escape-code", label = std::to_string(edge.byte); break;
                    case pf::Probe::Range:
                        kind = "code-range", label = list(std::vector<uint8_t>{edge.range.begin, edge.range.last});
                        break;
                    case pf::Probe::Set: kind = "code-set", label = list(edge.codes); break;
                    case pf::Probe::SetTooBig: kind = "none"; break;
                    case pf::Probe::Pair: throw std::runtime_error("pair edge in the alignment graph");
                }
                std::string cut = "no";
                if (t.cut[e]) {
                    cut = "yes";
                } else if (!t.paired[e].empty()) {
                    cut = "paired:";
                    for (size_t i = 0; i < t.paired[e].size(); ++i)
                        cut += (i ? ";" : "") + std::to_string(t.paired[e][i]);
                }
                std::fprintf(graph, "%s,%u,%u,%s,%s,%s\n", id.c_str(), edge.from, edge.to, kind.c_str(),
                             label.c_str(), cut.c_str());
            }
        }
        std::fprintf(stderr, "%s fsst: row group %zu, %zu rows, %zu needles\n", dataset.c_str(), row_group, rows,
                     needles.size());
    }
    std::fclose(queries);
    std::fclose(symbols);
    std::fclose(graph);
}
