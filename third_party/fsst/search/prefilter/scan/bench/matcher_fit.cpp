// Stage one: every kernel against every probe shape, timed over a prefix of
// the stream, one CSV row per pair, then the matcher rows of the cost model
// fitted to what that measured. Mirrors onpair's matcher_fit sweep so the
// paper's plot_matcher.py reads the file unchanged.
//
//   prefilter_matcher_sweep                 sweep, write the CSV, fit
//   prefilter_matcher_sweep --refit [csv]   fit the newest CSV, or the one named
//
// A probe is K codes from the catalog, R ranges spread over the code space,
// or both. Only R is swept for ranges; the codes per range are the control.

#include <algorithm>
#include <cmath>
#include <cstdio>
#include <functional>
#include <map>
#include <optional>
#include <string>
#include <utility>
#include <vector>

#include "../matcher/eq_or.hpp"
#include "../matcher/nibble.hpp"
#include "../matcher/nibble_n8.hpp"
#include "../matcher/range.hpp"
#include "../scan.hpp"
#include "../tests.hpp"
#include "loader.hpp"
#include "utils.hpp"

using namespace fsst::search::prefilter;
using namespace fsst::search::prefilter::scan;
using namespace fsst::search::prefilter::scan::bench;

constexpr size_t RANGE_COUNTS[] = {1, 2, 3, 4, 6, 8, 12, 16, 24, 32};
// Width 8 so the full R grid fits: disjoint ranges want width <= 256 / R,
// and R = 32 leaves eight codes each. A range costs two compares whatever it
// spans, so this measures the same kernel as u16's width 16 does.
constexpr size_t RANGE_WIDTHS[] = {1, 8, 16, 256};
constexpr size_t RANGED_TOKEN_COUNTS[] = {1, 8, 16};
// Pairs alone, made of consecutive needles of the K = 16 set at RANGED_TARGET.
constexpr size_t PAIR_COUNTS[] = {1, 2, 4, 8};
constexpr size_t PAIR_SOURCE_COUNT = 16;
constexpr double RANGED_TARGET = 0.01;
constexpr size_t RANGED_WIDTH = 16;
constexpr size_t CODE_SPACE = 256;
constexpr size_t MAX_MEASURED_BATCHES = 32;
constexpr double BYTES_PER_CODE = 1.0;

// One measurement. `count` is K and `length` 1, both zero where a probe is
// ranges alone; `ranges` is R and `width` the codes in each, zero without.
struct Row {
    std::string stream, encoding;
    size_t codes, rows;
    std::string matcher;
    size_t length, count, ranges, width;
    double target, achieved, prefix_selectivity, gbs, gcodes;
    std::string machine;
    size_t pairs = 0;  // P, pairs alone: count, length and ranges are zero

    double ns() const { return BYTES_PER_CODE / gbs; }
    size_t batches() const { return (std::max<size_t>(count, 1) + matcher::PER_BATCH - 1) / matcher::PER_BATCH; }

    // What each kernel's cost is linear in, and the axis name.
    std::optional<std::pair<double, const char*>> regressor() const {
        if (pairs > 0) return std::nullopt;
        if (matcher == "eq_or") return std::pair{static_cast<double>(count), "K"};
        if (matcher == "nibble_n8k") return std::pair{static_cast<double>(batches()), "B"};
        if (matcher == "range") return std::pair{static_cast<double>(ranges), "R"};
        if (matcher == "nibble") return std::pair{0.0, ""};
        return std::nullopt;
    }

    std::string csv() const {
        return stream + "," + encoding + "," + std::to_string(codes) + "," + std::to_string(rows) + "," + matcher +
               "," + std::to_string(length) + "," + std::to_string(count) + "," + std::to_string(ranges) + "," +
               std::to_string(width) + "," + number(target) + "," + number(achieved) + "," +
               number(prefix_selectivity) + "," + number(gbs) + "," + number(gcodes) + "," + machine + "," +
               std::to_string(pairs);
    }

    static Row parse(const std::map<std::string, std::string>& f) {
        return Row{f.at("stream"),           f.at("encoding"),          std::stoul(f.at("codes")),
                   std::stoul(f.at("rows")), f.at("matcher"),           std::stoul(f.at("length")),
                   std::stoul(f.at("count")), std::stoul(f.at("ranges")), std::stoul(f.at("width")),
                   std::stod(f.at("target")), std::stod(f.at("achieved")), std::stod(f.at("prefix_selectivity")),
                   std::stod(f.at("gbs")),   std::stod(f.at("gcodes")), f.at("machine"),
                   f.count("pairs") ? std::stoul(f.at("pairs")) : 0};
    }
};

constexpr const char* HEADER =
    "stream,encoding,codes,rows,matcher,length,count,ranges,width,target,achieved,prefix_selectivity,gbs,gcodes,"
    "machine,pairs";

struct Probe {
    const NeedleSet* set;
    std::vector<CodeRange> ranges;
    size_t width;
    std::vector<CodePair> pairs;

    ProbeCover cover() const { return ProbeCover{set ? set->needles : std::vector<uint8_t>{}, ranges, pairs}; }
};

// R disjoint ranges of `width` codes spread over the code space, or none
// where they do not fit.
static std::optional<std::vector<CodeRange>> spread_ranges(size_t count, size_t width) {
    size_t stride = CODE_SPACE / count;
    if (width > stride) return std::nullopt;
    std::vector<CodeRange> ranges;
    for (size_t at = 0; at < count; ++at) {
        size_t lo = at * stride;
        ranges.push_back({static_cast<uint8_t>(lo), static_cast<uint8_t>(lo + width - 1)});
    }
    return ranges;
}

static std::vector<Probe> probes(const std::vector<NeedleSet>& sets) {
    std::vector<Probe> out;
    for (const NeedleSet& set : sets) out.push_back({&set, {}, 0, {}});
    for (size_t count : RANGE_COUNTS)
        for (size_t width : RANGE_WIDTHS)
            if (auto ranges = spread_ranges(count, width)) out.push_back({nullptr, *ranges, width, {}});
    for (const NeedleSet& set : sets) {
        bool beside = std::find(std::begin(RANGED_TOKEN_COUNTS), std::end(RANGED_TOKEN_COUNTS), set.count) !=
                          std::end(RANGED_TOKEN_COUNTS) &&
                      std::abs(set.target - RANGED_TARGET) < 1e-9;
        if (!beside) continue;
        for (size_t count : RANGE_COUNTS)
            if (auto ranges = spread_ranges(count, RANGED_WIDTH)) out.push_back({&set, *ranges, RANGED_WIDTH, {}});
    }
    for (const NeedleSet& set : sets) {
        if (set.count != PAIR_SOURCE_COUNT || std::abs(set.target - RANGED_TARGET) > 1e-9) continue;
        for (size_t count : PAIR_COUNTS) {
            std::vector<CodePair> pairs;
            for (size_t i = 0; i < count && 2 * i + 1 < set.needles.size(); ++i)
                pairs.push_back({set.needles[2 * i], set.needles[2 * i + 1]});
            if (pairs.size() == count) out.push_back({nullptr, {}, 0, pairs});
        }
        break;
    }
    return out;
}

struct Timing {
    const uint8_t* codes;  // the prefix timed
    size_t len;
    Stream checked;  // the shorter prefix every kernel's rows are checked on
};

struct Timed {
    double seconds;
    std::vector<size_t> found;
};

template <typename M>
static Timed time_kernel(const Probe& probe, const Timing& s) {
    ProbeCover cover = probe.cover();
    M matcher(cover);
    Mask bits{};
    double seconds = best([&] {
        blocks<BLOCK / 64>(s.codes, s.len, [&](const uint8_t* block, size_t, size_t) {
            matcher.check(block, bits);
            // The mask is thrown away; keep the compiler from throwing away the kernel with it.
            asm volatile("" : : "r"(bits.data()) : "memory");
        });
    });
    std::vector<size_t> found;
    both_stages<M>(cover, s.checked, AcceptAll{}, found);
    return {seconds, std::move(found)};
}

using Run = std::function<std::optional<Timed>(const Probe&, const Timing&)>;

struct Kernel {
    const char* name;
    Run run;
};

template <typename M>
static Run only_if(std::function<bool(size_t, size_t, size_t)> takes) {
    return [takes](const Probe& probe, const Timing& s) -> std::optional<Timed> {
        ProbeCover cover = probe.cover();
        if (!takes(cover.points.size(), cover.ranges.size(), cover.pairs.size())) return std::nullopt;
        return time_kernel<M>(probe, s);
    };
}

// NibbleN8 at ceil(K / 8) batches, measured past the one batch the planner
// picks at u8 so the figure shows where the batches stop paying.
template <bool SKIP, size_t... B>
static std::optional<Timed> n8k_at(size_t batches, const Probe& probe, const Timing& s,
                                   std::index_sequence<B...>) {
    std::optional<Timed> out;
    (void)((batches == B + 1 ? (out = time_kernel<matcher::NibbleN8<B + 1, SKIP>>(probe, s), true) : false) ||
           ...);
    return out;
}

template <bool SKIP>
static Run nibble_n8k() {
    return [](const Probe& probe, const Timing& s) -> std::optional<Timed> {
        size_t k = probe.cover().points.size();
        if (k == 0 || !probe.pairs.empty()) return std::nullopt;
        size_t batches = (k + matcher::PER_BATCH - 1) / matcher::PER_BATCH;
        return n8k_at<SKIP>(batches, probe, s, std::make_index_sequence<MAX_MEASURED_BATCHES>{});
    };
}

static std::vector<Kernel> kernels() {
    auto any = [](size_t, size_t, size_t p) { return p == 0; };
    auto tokens = [](size_t k, size_t, size_t p) { return k > 0 || p > 0; };
    auto ranges_alone = [](size_t k, size_t r, size_t p) { return k == 0 && r > 0 && p == 0; };
    return {
        {"eq_or", only_if<matcher::EqOr<false>>(tokens)},
        {"eq_or_skip", only_if<matcher::EqOr<true>>(tokens)},
        {"range", only_if<matcher::Range<false>>(ranges_alone)},
        {"range_skip", only_if<matcher::Range<true>>(ranges_alone)},
        {"nibble_n8k", nibble_n8k<false>()},
        {"nibble_n8k_skip", nibble_n8k<true>()},
        {"nibble", only_if<matcher::Nibble<false>>(any)},
        {"nibble_skip", only_if<matcher::Nibble<true>>(any)},
    };
}

static void measure(const std::string& stream, const std::vector<Kernel>& kernels, const std::string& machine,
                    std::vector<Row>& out) {
    auto [corpus_path, needles_path] = paths(stream, ENCODING);
    Corpus corpus = load_corpus(corpus_path);
    size_t len = std::min(CODES, corpus.codes.size());
    std::vector<uint32_t> row_offsets = row_layer(corpus.row_offsets, len);
    std::vector<NeedleSet> sets;
    for (NeedleSet& set : load_needles(needles_path))
        if (set.sample == SAMPLE) sets.push_back(std::move(set));
    // The catalog stops at 192 one-code needles, and is already at selectivity
    // one there: a 256-symbol dictionary holds no 256 codes that miss
    // anything. K = 256 is the whole dictionary, so name it outright. What it
    // measures is the kernel at 256 compares, not a cover a planner would cut.
    NeedleSet whole;
    whole.count = CODE_SPACE;
    whole.target = 1.0;
    whole.achieved = 1.0;
    whole.sample = SAMPLE;
    for (size_t c = 0; c < CODE_SPACE; ++c) whole.needles.push_back(static_cast<uint8_t>(c));
    sets.push_back(std::move(whole));
    std::vector<Probe> all = probes(sets);
    std::printf("\n%s %s: %zu codes, %zu rows, %zu probes over %zu needle sets\n", stream.c_str(), ENCODING,
                len, row_offsets.size() - 1, all.size(), sets.size());

    size_t checked_len = std::min(CHECK_CODES, len);
    std::vector<uint32_t> check_rows = row_layer(corpus.row_offsets, checked_len);
    Timing s{corpus.codes.data(), len, Stream{corpus.codes.data(), checked_len, check_rows.data(), check_rows.size()}};

    for (const Probe& probe : all) {
        ProbeCover cover = probe.cover();
        std::vector<size_t> expected = rows_oracle(cover, s.checked);
        for (const Kernel& kernel : kernels) {
            std::optional<Timed> timed = kernel.run(probe, s);
            if (!timed) continue;
            if (timed->found != expected) {
                std::fprintf(stderr, "%s differs from the cover on K=%zu R=%zu P=%zu\n", kernel.name,
                             cover.points.size(), cover.ranges.size(), cover.pairs.size());
                std::exit(1);
            }
            double gbs = static_cast<double>(len) * BYTES_PER_CODE / timed->seconds / 1e9;
            out.push_back(Row{stream, ENCODING, len, row_offsets.size() - 1, kernel.name, probe.set ? 1u : 0u,
                              probe.set ? probe.set->count : 0, probe.ranges.size(), probe.width,
                              probe.set ? probe.set->target : 0.0, probe.set ? probe.set->achieved : 0.0,
                              static_cast<double>(timed->found.size()) / (check_rows.size() - 1), gbs,
                              gbs / BYTES_PER_CODE, machine, probe.pairs.size()});
        }
    }
}

// One machine's coefficients: an intercept and a slope per kernel, and what
// a range adds to the kernels that fold them in.
struct Fit {
    std::optional<std::pair<double, double>> eq_or, range, nibble_n8k;
    std::optional<double> nibble;
    double beside = 0, beside_n8k = 0;
    double per_pair = 0;  // what a pair adds to eq_or, over its intercept
    double ceiling = 0;
};

// The rows of one kernel with no ranges beside its tokens, which is the line
// the ranges are then fitted on top of. The model only prices the batches
// the planner can pick.
static std::vector<const Row*> alone(const std::vector<const Row*>& rows, const std::string& kernel) {
    std::vector<const Row*> out;
    for (const Row* row : rows)
        if (row->matcher == kernel && (row->ranges == 0 || kernel == "range") &&
            (kernel != "nibble_n8k" || row->batches() <= matcher::MAX_BATCHES))
            out.push_back(row);
    return out;
}

static std::vector<std::pair<double, double>> points(const std::vector<const Row*>& rows) {
    std::vector<std::pair<double, double>> out;
    for (const Row* row : rows)
        if (auto reg = row->regressor()) out.push_back({reg->first, row->ns()});
    return out;
}

// The fit as the policy's cost table will take it, to paste under this
// machine's instruction set.
static void snippet(const std::string& machine, const std::string& source, const Fit& fit) {
    std::printf("\n// Fitted on %s, from %s. ns per code; k tokens, r ranges, b = ceil(k / 8).\n", machine.c_str(),
                source.c_str());
    if (fit.eq_or)
        std::printf("case Match::EqOr:      return %.5f + %.5f * k + %.5f * r + %.5f * p;\n", fit.eq_or->first,
                    fit.eq_or->second, fit.beside, fit.per_pair);
    if (fit.nibble_n8k) {
        std::string batches = fit.nibble_n8k->second == 0.0 ? "" : " + " + number(fit.nibble_n8k->second) + " * b";
        std::printf("case Match::NibbleN8K: return %.5f%s + %.5f * r;\n", fit.nibble_n8k->first, batches.c_str(),
                    fit.beside_n8k);
    }
    if (fit.nibble) std::printf("case Match::Nibble:    return %.5f;\n", *fit.nibble);
    if (fit.range) std::printf("case Match::Range:     return %.5f + %.5f * r;\n", fit.range->first, fit.range->second);
    std::printf("// nothing above %.1f GB/s was reachable, so clamp with max(%.5f) if that binds.\n",
                BYTES_PER_CODE / fit.ceiling, fit.ceiling);
}

static void fit(const std::vector<Row>& all, const std::string& source) {
    std::map<std::string, std::vector<const Row*>> group;
    for (const Row& row : all) group[row.machine].push_back(&row);

    std::map<std::string, Fit> fitted;
    for (auto& [machine, rows] : group) {
        size_t codes = 0;
        for (const Row* row : rows) codes = std::max(codes, row->codes);
        Fit fit;
        std::printf("\n%s  up to %zu codes\n", machine.c_str(), codes);
        std::printf("  %-13s %-26s %6s\n", "kernel", "fitted ns/code", "fit%");
        for (const char* kernel : {"eq_or", "range", "nibble_n8k", "nibble"}) {
            std::vector<const Row*> own = alone(rows, kernel);
            std::vector<std::pair<double, double>> point = points(own);
            if (point.empty()) continue;
            const char* axis = own[0]->regressor()->second;
            auto ab = line_fit(point);
            double a = ab.first, b = ab.second;
            bool flat = std::all_of(point.begin(), point.end(), [&](auto p) { return p.first == point[0].first; });
            std::string kernel_name = kernel;
            if (kernel_name == "eq_or") fit.eq_or = {a, b};
            if (kernel_name == "range") fit.range = {a, b};
            if (kernel_name == "nibble_n8k") fit.nibble_n8k = {a, flat ? 0.0 : b};
            if (kernel_name == "nibble") fit.nibble = a;
            std::string shown = flat ? number(a).substr(0, 7) : number(a).substr(0, 7) + " + " + number(b).substr(0, 7) + "*" + axis;
            std::printf("  %-13s %-26s %6.1f\n", kernel, shown.c_str(),
                        rel_error(point, [&](double x) { return a + b * x; }));
        }

        // What a range adds to a token kernel: the residual over that
        // kernel's own line, per range, on the rows carrying both.
        for (const char* kernel : {"eq_or", "nibble_n8k"}) {
            std::vector<std::pair<double, double>> own = points(alone(rows, kernel));
            if (own.empty()) continue;
            auto ab = line_fit(own);
            double a = ab.first, b = ab.second;
            std::vector<std::pair<double, double>> point;
            for (const Row* row : rows) {
                if (row->matcher != kernel || row->ranges == 0 || row->count == 0) continue;
                auto reg = row->regressor();
                if (!reg) continue;
                double residual = row->ns() - (a + b * reg->first);
                if (residual > 0.0) point.push_back({static_cast<double>(row->ranges), residual});
            }
            if (point.size() > 1) {
                double per_range = slope_fit(point);
                std::printf("  %s with ranges: %.5f per range\n", kernel, per_range);
                (std::string(kernel) == "eq_or" ? fit.beside : fit.beside_n8k) = per_range;
            }
        }

        // What a pair adds to eq_or: the residual over its intercept, per
        // pair, on the pairs-alone rows.
        if (fit.eq_or) {
            std::vector<std::pair<double, double>> point;
            for (const Row* row : rows)
                if (row->matcher == "eq_or" && row->pairs > 0)
                    point.push_back({static_cast<double>(row->pairs), row->ns() - fit.eq_or->first});
            if (point.size() > 1) {
                fit.per_pair = slope_fit(point);
                std::printf("  eq_or with pairs: %.5f per pair\n", fit.per_pair);
            }
        }

        // The fastest any kernel ran at the largest stream: no model term
        // describes it, and nothing predicted above it is reachable.
        fit.ceiling = 1e300;
        for (const Row* row : rows)
            if (row->codes == codes) fit.ceiling = std::min(fit.ceiling, row->ns());
        std::printf("  ceiling at %zu codes: %.2f GB/s, so no ns/code below %.5f\n", codes, BYTES_PER_CODE / fit.ceiling,
                    fit.ceiling);

        // The skip flag as the two ends the model is fitted to: what it costs
        // where every group matches, what it saves where none does.
        for (const char* kernel : {"eq_or", "nibble_n8k", "nibble"}) {
            std::map<std::string, std::pair<double, size_t>> delta;
            std::string skip_name = std::string(kernel) + "_skip";
            for (const Row* skip : rows) {
                if (skip->matcher != skip_name) continue;
                for (const Row* plain : rows) {
                    if (plain->matcher != kernel || plain->length != skip->length || plain->count != skip->count ||
                        plain->ranges != skip->ranges || plain->pairs != skip->pairs || plain->target != skip->target)
                        continue;
                    auto& cell = delta[number(skip->target)];
                    cell.first += skip->ns() - plain->ns();
                    cell.second += 1;
                    break;
                }
            }
            if (delta.empty()) continue;
            std::printf("  %s skip flag, ns/code by selectivity:", kernel);
            for (auto& [target, cell] : delta) std::printf("  %s %+.5f", target.c_str(), cell.first / cell.second);
            std::printf("\n");
        }
        fitted[machine] = fit;
    }
    for (auto& [machine, fit] : fitted) snippet(machine, source, fit);
}

int main(int argc, char** argv) {
    if (argc >= 2 && std::string(argv[1]) == "--refit") {
        auto path = csv_source("novel_mask", argc >= 3 ? argv[2] : "");
        std::vector<Row> rows;
        for (auto& fields : read_csv(path)) rows.push_back(Row::parse(fields));
        std::printf("%s\n%zu rows\n", path.c_str(), rows.size());
        fit(rows, path.filename().string());
        return 0;
    }
    std::string name = machine();
    std::vector<Row> rows;
    std::vector<Kernel> all = kernels();
    for (const char* stream : STREAMS) measure(stream, all, name, rows);
    std::vector<std::string> lines;
    for (const Row& row : rows) lines.push_back(row.csv());
    auto path = write_csv("novel_mask", HEADER, lines);
    std::printf("%zu rows -> %s\n", rows.size(), path.c_str());
    fit(rows, path.filename().string());
    return 0;
}
