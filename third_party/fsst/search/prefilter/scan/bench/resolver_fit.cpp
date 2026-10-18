// Stage two: both resolvers against one mask per needle set, built once from
// the cover so no matcher runs while the clock does, then the four constants
// of the stage-two cost model fitted to the rows. Mirrors onpair's
// resolver_fit sweep; the paper's plot_resolver.py reads the CSV unchanged.
//
//   prefilter_resolver_sweep                 sweep, write the CSV, fit
//   prefilter_resolver_sweep --refit [csv]   fit the newest CSV, or the one named
//
// Axes: the mask's hit density (needle sets of K = 1 and 16 at every target
// selectivity) against the row length (the row layer with every `factor`
// rows fused into one). Rates are over the code stream.

#include <algorithm>
#include <array>
#include <cmath>
#include <cstdio>
#include <map>
#include <string>
#include <vector>

#include "../resolver/gallop_seek.hpp"
#include "../resolver/linear_seek.hpp"
#include "../scan.hpp"
#include "../tests.hpp"
#include "loader.hpp"
#include "utils.hpp"

using namespace fsst::search::prefilter;
using namespace fsst::search::prefilter::scan;
using namespace fsst::search::prefilter::scan::bench;

// Short rows and long rows; a stream whose files are missing is skipped.
constexpr const char* RESOLVER_STREAMS[] = {"imdb/name/name_1m", "ch/hits/URL_1m"};
constexpr size_t MERGES[] = {1, 4, 16, 64};
constexpr size_t COUNTS[] = {1, 16};
// 8 MiB at one byte a code, the working set the matcher sweep walks, so both
// stages are read against one memory ceiling.
constexpr size_t RESOLVER_CODES = 8u << 20;
constexpr size_t WORDS = BLOCK / 64;

struct Row {
    std::string stream;
    size_t codes, rows, codes_per_row;
    std::string resolver;
    size_t length, count;
    double target, achieved, density, blocks_hit, selectivity, gbs;
    std::string machine;

    bool linear() const { return resolver == "linear_seek"; }
    double ns() const { return static_cast<double>(codes) / gbs; }
    double words() const { return 64.0 * blocks_hit * static_cast<double>(codes) / BLOCK; }
    double emitted() const { return selectivity * static_cast<double>(rows); }
    // The cursor stops at the last hit rather than the last row.
    double crossed() const { return static_cast<double>(rows) * emitted() / (emitted() + 1.0); }
    bool has_work() const { return density * static_cast<double>(codes) >= 1.0 && emitted() >= 1.0; }

    std::array<double, 4> terms() const {
        double w = words(), e = emitted(), c = crossed();
        if (linear()) return {w, e, c, 0.0};
        return {w, 0.0, 0.0, e * std::log2(1.0 + c / e)};
    }

    static Row parse(const std::map<std::string, std::string>& f) {
        return Row{f.at("stream"),          std::stoul(f.at("codes")),    std::stoul(f.at("rows")),
                   std::stoul(f.at("codes_per_row")), f.at("resolver"),  std::stoul(f.at("length")),
                   std::stoul(f.at("count")), std::stod(f.at("target")), std::stod(f.at("achieved")),
                   std::stod(f.at("density")), std::stod(f.at("blocks_hit")), std::stod(f.at("selectivity")),
                   std::stod(f.at("gbs")),   f.at("machine")};
    }

    std::string csv() const {
        return stream + "," + std::to_string(codes) + "," + std::to_string(rows) + "," +
               std::to_string(codes_per_row) + "," + resolver + "," + std::to_string(length) + "," +
               std::to_string(count) + "," + number(target) + "," + number(achieved) + "," + number(density) + "," +
               number(blocks_hit) + "," + number(selectivity) + "," + number(gbs) + "," + machine;
    }
};

// One bit per covered code for the whole stream, laid out as the blocks the
// seam hands over.
static std::vector<uint64_t> mask_of(const NeedleSet& set, const uint8_t* codes, size_t len) {
    ProbeCover cover{set.needles, {}};
    std::array<bool, 256> admits{};
    for (uint8_t c : set.needles) admits[c] = true;
    std::vector<uint64_t> mask;
    mask.reserve(len / 64);
    blocks<BLOCK / 64>(codes, len, [&](const uint8_t* block, size_t, size_t valid) {
        Mask bits{};
        for (size_t at = 0; at < BLOCK; ++at)
            if (admits[block[at]]) bits[at / 64] |= uint64_t{1} << (at % 64);
        clear_from(bits, valid);
        mask.insert(mask.end(), bits.begin(), bits.end());
    });
    return mask;
}

static std::vector<size_t> hit_blocks(const std::vector<uint64_t>& mask) {
    std::vector<size_t> hit;
    for (size_t block = 0; block * WORDS < mask.size(); ++block)
        if (std::any_of(mask.begin() + block * WORDS, mask.begin() + (block + 1) * WORDS,
                        [](uint64_t w) { return w != 0; }))
            hit.push_back(block);
    return hit;
}

static std::vector<uint32_t> merge_rows(const std::vector<uint32_t>& layer, size_t factor) {
    std::vector<uint32_t> merged;
    for (size_t i = 0; i < layer.size(); i += factor) merged.push_back(layer[i]);
    if (merged.back() < layer.back()) merged.push_back(layer.back());
    return merged;
}

// Stage two over the stream from a mask built once: the blocks a matcher
// would report non-empty, in order, into a buffer the passes reuse.
template <typename R>
static void resolve_stream(const std::vector<uint64_t>& mask, const std::vector<size_t>& hit,
                           const std::vector<uint32_t>& offsets, std::vector<size_t>& out) {
    out.clear();
    R resolver(offsets.data(), offsets.size());
    Mask bits;
    for (size_t block : hit) {
        std::copy_n(mask.begin() + block * WORDS, WORDS, bits.begin());
        resolver.rows(bits, block * BLOCK, AcceptAll{}, out);
    }
}

template <typename R>
static double resolve(const char* name, const std::vector<uint64_t>& mask, const std::vector<size_t>& hit,
                      const std::vector<uint32_t>& offsets, const std::vector<size_t>& expected) {
    std::vector<size_t> out;
    double seconds = best([&] { resolve_stream<R>(mask, hit, offsets, out); });
    if (out != expected) {
        std::fprintf(stderr, "%s differs from the rows the mask names\n", name);
        std::exit(1);
    }
    return seconds;
}

static void measure(const std::string& stream, const std::string& machine, std::vector<Row>& out) {
    auto [corpus_path, needles_path] = paths(stream, ENCODING);
    if (!fs::exists(corpus_path) || !fs::exists(needles_path)) {
        std::printf("\n%s %s: skipped, no stream or catalog under %s\n", stream.c_str(), ENCODING,
                    data_dir().c_str());
        return;
    }
    Corpus corpus = load_corpus(corpus_path);
    size_t len = std::min(RESOLVER_CODES, corpus.codes.size()) / BLOCK * BLOCK;
    std::vector<uint32_t> layer = row_layer(corpus.row_offsets, len);
    std::vector<NeedleSet> sets;
    for (NeedleSet& set : load_needles(needles_path))
        if (set.sample == SAMPLE && std::find(std::begin(COUNTS), std::end(COUNTS), set.count) != std::end(COUNTS))
            sets.push_back(std::move(set));
    std::printf("\n%s %s: %zu codes, %zu rows, %zu needle sets\n", stream.c_str(), ENCODING, len, layer.size() - 1,
                sets.size());

    size_t empty = 0;
    for (const NeedleSet& set : sets) {
        std::vector<uint64_t> mask = mask_of(set, corpus.codes.data(), len);
        std::vector<size_t> hit = hit_blocks(mask);
        if (hit.empty()) {
            ++empty;
            continue;
        }
        size_t bits = 0;
        for (uint64_t w : mask) bits += static_cast<size_t>(__builtin_popcountll(w));
        for (size_t factor : MERGES) {
            std::vector<uint32_t> offsets = merge_rows(layer, factor);
            size_t rows = offsets.size() - 1;
            std::vector<size_t> expected = rows_of_mask(mask, offsets, AcceptAll{});
            std::pair<const char*, double> timed[] = {
                {"linear_seek", resolve<resolver::LinearSeek>("linear_seek", mask, hit, offsets, expected)},
                {"gallop_seek", resolve<resolver::GallopSeek>("gallop_seek", mask, hit, offsets, expected)},
            };
            for (auto& [name, seconds] : timed)
                out.push_back(Row{stream, len, rows, len / rows, name, 1, set.count, set.target, set.achieved,
                                  static_cast<double>(bits) / len, static_cast<double>(hit.size()) / (mask.size() / WORDS),
                                  static_cast<double>(expected.size()) / rows, static_cast<double>(len) / seconds / 1e9,
                                  machine});
        }
    }
    if (empty) std::printf("  %zu of %zu sets never hit the prefix\n", empty, sets.size());
}

// Least squares of y ~ sum b_i x_i on relative error, by elimination.
static std::array<double, 4> lstsq(const std::vector<std::pair<std::array<double, 4>, double>>& rows) {
    constexpr size_t N = 4;
    double a[N][N] = {}, b[N] = {};
    for (auto& [x, y] : rows)
        for (size_t i = 0; i < N; ++i) {
            b[i] += x[i] / y;
            for (size_t j = 0; j < N; ++j) a[i][j] += x[i] * x[j] / (y * y);
        }
    for (size_t col = 0; col < N; ++col) {
        size_t pivot = col;
        for (size_t i = col + 1; i < N; ++i)
            if (std::fabs(a[i][col]) > std::fabs(a[pivot][col])) pivot = i;
        std::swap(a[col], a[pivot]);
        std::swap(b[col], b[pivot]);
        if (a[col][col] == 0.0) continue;
        for (size_t i = 0; i < N; ++i) {
            if (i == col) continue;
            double f = a[i][col] / a[col][col];
            for (size_t j = 0; j < N; ++j) a[i][j] -= f * a[col][j];
            b[i] -= f * b[col];
        }
    }
    std::array<double, 4> beta{};
    for (size_t i = 0; i < N; ++i) beta[i] = a[i][i] == 0.0 ? 0.0 : b[i] / a[i][i];
    return beta;
}

static double error(const std::vector<Row*>& rows, bool linear, const std::array<double, 4>& beta) {
    double sum = 0.0;
    size_t n = 0;
    for (Row* row : rows) {
        if (row->linear() != linear) continue;
        auto t = row->terms();
        double predicted = t[0] * beta[0] + t[1] * beta[1] + t[2] * beta[2] + t[3] * beta[3];
        sum += std::fabs(predicted / row->ns() - 1.0);
        ++n;
    }
    return n ? 100.0 * sum / n : NAN;
}

// Rows crossed per emitted row above which the search beats the walk.
static double crossover(const std::array<double, 4>& beta) {
    for (double g = 2.0; g < 1e7; g *= 1.001)
        if (beta[3] * std::log2(1.0 + g) < beta[1] + beta[2] * g) return g;
    return INFINITY;
}

static void fit(std::vector<Row>& all) {
    std::map<std::string, std::vector<Row*>> group;
    for (Row& row : all)
        if (row.has_work()) group[row.machine].push_back(&row);
    for (auto& [machine, rows] : group) {
        std::vector<std::pair<std::array<double, 4>, double>> points;
        for (Row* row : rows) points.push_back({row->terms(), row->ns()});
        std::array<double, 4> beta = lstsq(points);
        std::printf("\n%s, %zu rows with work in them\n", machine.c_str(), rows.size());
        std::printf("  both         %.2f per word\n", beta[0]);
        std::printf("  linear_seek  %.2f per row + %.2f per crossed   fit %.1f%%\n", beta[1], beta[2],
                    error(rows, true, beta));
        std::printf("  gallop_seek  %.2f per halving                  fit %.1f%%\n", beta[3], error(rows, false, beta));
        double g = crossover(beta);
        std::printf("  gallop_seek above g = %.0f rows crossed per emitted row, one hit row in %.3f\n", g, 1.0 / g);
        std::printf("\nWORD_NS = %.2f; LINEAR_SEEK_ROW_NS = %.2f; LINEAR_SEEK_CROSS_NS = %.2f; GALLOP_SEEK_STEP_NS = %.2f\n",
                    beta[0], beta[1], beta[2], beta[3]);
    }
}

int main(int argc, char** argv) {
    if (argc >= 2 && std::string(argv[1]) == "--refit") {
        auto path = csv_source("novel_resolve", argc >= 3 ? argv[2] : "");
        std::vector<Row> rows;
        for (auto& fields : read_csv(path)) rows.push_back(Row::parse(fields));
        std::printf("%s\n%zu rows\n", path.c_str(), rows.size());
        fit(rows);
        return 0;
    }
    std::string name = machine();
    std::vector<Row> rows;
    for (const char* stream : RESOLVER_STREAMS) measure(stream, name, rows);
    std::vector<std::string> lines;
    for (const Row& row : rows) lines.push_back(row.csv());
    auto path = write_csv("novel_resolve",
                          "stream,codes,rows,codes_per_row,resolver,length,count,target,achieved,density,blocks_hit,"
                          "selectivity,gbs,machine",
                          lines);
    std::printf("%zu rows -> %s\n", rows.size(), path.c_str());
    fit(rows);
    return 0;
}
