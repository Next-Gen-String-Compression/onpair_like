// The graph, the cut and the planned cover against byte containment on
// FSST-compressed rows, plus the min cut on synthetic graphs.

#include <algorithm>
#include <cstring>
#include <string>
#include <vector>

#include "compress.hpp"
#include "scan/tests.hpp"

using namespace fsst::search::prefilter;
using scan::cover_matches;

struct Column : CompressedRows {
    std::vector<std::string> rows;
};

static Column column(const std::vector<std::string>& rows) {
    Column col;
    static_cast<CompressedRows&>(col) = ::fsst::search::prefilter::compress_rows(rows);
    col.rows = rows;
    CHECK(col.dict.sorted());
    std::vector<unsigned char> plain(8);
    for (size_t i = 0; i < rows.size(); ++i) {
        plain.resize(rows[i].size() + 8);
        const size_t len = col.offsets[i + 1] - col.offsets[i];
        size_t got = fsst_decompress(&col.decoder, len, col.codes.data() + col.offsets[i], plain.size(), plain.data());
        CHECK_MSG(got == rows[i].size() && std::memcmp(plain.data(), rows[i].data(), got) == 0,
                  "row %zu does not round trip", i);
    }
    return col;
}

static bool byte_contains(const std::string& hay, const std::string& needle) {
    return hay.find(needle) != std::string::npos;
}

static std::vector<size_t> rows_containing(const Column& col, const std::string& pat) {
    std::vector<size_t> want;
    for (size_t r = 0; r < col.row_count(); ++r)
        if (byte_contains(col.rows[r], pat)) want.push_back(r);
    return want;
}

static const uint8_t* bytes(const std::string& s) { return reinterpret_cast<const uint8_t*>(s.data()); }

static CutGraph plain(const AlignmentGraph& g) { return CutGraph{g.edges, g.node_count(), g.source(), g.sink()}; }

template <typename Weight>
static std::vector<const Edge*> cut_edges(const CutGraph& graph, Weight weight) {
    std::vector<const Edge*> cut;
    for (uint32_t at : min_cut(graph, weight)) cut.push_back(&graph.edges[at]);
    return cut;
}

template <typename Blocked>
static bool sink_reachable_avoiding(const CutGraph& g, Blocked blocked) {
    std::vector<std::vector<const Edge*>> adjacency(g.node_count);
    for (const Edge& e : g.edges) adjacency[e.from].push_back(&e);
    std::vector<bool> seen(g.node_count, false);
    std::vector<size_t> stack{g.source};
    seen[g.source] = true;
    while (!stack.empty()) {
        size_t node = stack.back();
        stack.pop_back();
        if (node == g.sink) return true;
        for (const Edge* e : adjacency[node])
            if (!seen[e->to] && !blocked(*e)) {
                seen[e->to] = true;
                stack.push_back(e->to);
            }
    }
    return false;
}

static bool selected(const std::vector<const Edge*>& selection, const Edge& e) {
    return std::find(selection.begin(), selection.end(), &e) != selection.end();
}

// Pairs are read within the row: stricter than the scan, which also pairs
// across a row boundary and lets the walk reject that.
static bool row_holds(const Column& col, const ProbeCover& cover, size_t row) {
    for (size_t i = col.offsets[row]; i < col.offsets[row + 1]; ++i)
        if (cover_matches(cover, col.codes.data(), i, col.offsets[row + 1])) return true;
    return false;
}

static bool covers_every_match(const Column& col, const std::vector<const Edge*>& selection,
                               const std::vector<size_t>& want) {
    ProbeCover cover = from_edge_cut(selection);
    for (size_t row : want)
        if (!row_holds(col, cover, row)) return false;
    return true;
}

static uint64_t by_frequency(const Edge& e) { return e.frequency; }

static size_t g_escape_edges = 0;

// The cut graph's invariants: no unprobed path, honest weights, the weakest
// and the minimum cut both sound, the minimum cut optimal while small.
static void check_cut_graph(const Column& col, const std::string& pat, const std::vector<size_t>& want,
                            const CutGraph& g) {
    std::vector<const Edge*> probes;
    for (const Edge& e : g.edges) {
        CHECK_MSG(e.from != e.to && e.from < g.node_count && e.to < g.node_count && e.from != g.sink &&
                      e.to != g.source,
                  "%s: cut edge %u -> %u", pat.c_str(), e.from, e.to);
        if (e.probe == Probe::Pair) {
            CHECK(e.codes.empty() && e.points == 0 && e.ranges == 0 && e.pairs == 1);
            // a pair's weight is the independence estimate, not a count: the counting index holds
            // single codes only, so how often two codes follow one another is not recorded
            CHECK_MSG(e.frequency == col.freq.pair_estimate(e.pair), "%s: pair %u %u reports %u, estimates %u",
                      pat.c_str(), e.pair.first, e.pair.second, e.frequency, col.freq.pair_estimate(e.pair));
        }
        if (e.cuttable()) probes.push_back(&e);
    }
    CHECK_MSG(!sink_reachable_avoiding(g, [](const Edge& e) { return e.cuttable(); }),
              "%s: a source-to-sink path carries no probe", pat.c_str());
    for (const Edge* e : probes) {
        if (e->probe == Probe::Pair) continue;
        ProbeCover cover = from_edge_cut({e});
        size_t matched = 0;
        for (uint8_t c : col.codes) matched += cover.contains(c);
        CHECK_MSG(e->frequency == matched, "%s: probe %u -> %u reports %u, matches %zu", pat.c_str(), e->from, e->to,
                  e->frequency, matched);
    }
    CHECK_MSG(covers_every_match(col, probes, want), "%s: a matching row holds no probe at all", pat.c_str());

    std::vector<const Edge*> cut = cut_edges(g, by_frequency);
    CHECK_MSG(!sink_reachable_avoiding(g, [&](const Edge& e) { return selected(cut, e); }),
              "%s: the minimum cut leaves a path open", pat.c_str());
    CHECK_MSG(covers_every_match(col, cut, want), "%s: the minimum cut misses a matching row", pat.c_str());

    if (probes.size() <= 12) {
        uint64_t best = 0;
        for (const Edge* e : cut) best += e->frequency;
        for (uint32_t mask = 0; mask < (1u << probes.size()); ++mask) {
            std::vector<const Edge*> subset;
            uint64_t weight = 0;
            for (size_t bit = 0; bit < probes.size(); ++bit)
                if ((mask >> bit) & 1) {
                    subset.push_back(probes[bit]);
                    weight += probes[bit]->frequency;
                }
            if (weight < best)
                CHECK_MSG(sink_reachable_avoiding(g, [&](const Edge& e) { return selected(subset, e); }),
                          "%s: a cover of weight %llu beats the cut's %llu", pat.c_str(),
                          static_cast<unsigned long long>(weight), static_cast<unsigned long long>(best));
        }
    }
}

static size_t g_pair_edges = 0;

static void check_graph(const Column& col, const std::string& pat, const std::vector<size_t>& want) {
    const size_t needle_len = pat.size();
    AlignmentGraph g = build_alignment_graph(col.dict, bytes(pat), needle_len, col.freq);
    for (const Edge& e : g.edges) {
        CHECK_MSG(e.from < e.to && e.to <= needle_len, "%s: edge %u -> %u", pat.c_str(), e.from, e.to);
        CHECK(e.probe != Probe::Pair);
        if (e.probe == Probe::Escape) {
            ++g_escape_edges;
            CHECK(e.to == e.from + 1 && e.codes.size() == 1 && e.codes[0] == ESCAPE && e.byte == bytes(pat)[e.from]);
        }
        if (e.probe == Probe::Range) {
            CHECK(e.codes.empty() && e.points == 0 && e.ranges == 1 && e.range.begin <= e.range.last);
            // Exactly the symbols the suffix is a prefix of, contiguous because the codes are sorted.
            size_t tail_len = needle_len - e.from;
            for (size_t c = 0; c < col.dict.count; ++c) {
                bool prefixed = col.dict.length(c) >= tail_len &&
                                std::memcmp(col.dict.symbol(c), bytes(pat) + e.from, tail_len) == 0;
                CHECK_MSG(prefixed == e.range.contains(static_cast<uint8_t>(c)), "%s: range %u-%u at code %zu",
                          pat.c_str(), e.range.begin, e.range.last, c);
            }
        } else {
            ProbeCover shape = ProbeCover::from_runs([&] {
                std::vector<CodeRange> runs;
                for (uint8_t c : e.codes) runs.push_back({c, c});
                return runs;
            }());
            CHECK(e.points == shape.points.size() && e.ranges == shape.ranges.size());
        }
    }
    CHECK_MSG(g.edges.size() <= 2 * needle_len + 16, "%s: %zu edges", pat.c_str(), g.edges.size());
    check_cut_graph(col, pat, want, plain(g));
    CutGraph paired = CutGraph::with_pairs(g, col.freq);
    for (const Edge& e : paired.edges) g_pair_edges += e.probe == Probe::Pair;
    // Unrolling keeps every edge of the plain graph at least once.
    CHECK(paired.edges.size() >= g.edges.size());
    check_cut_graph(col, pat, want, paired);
}

// Every pattern against byte containment: the graph's invariants, the
// cover holding a code of every matching row, then the walk's exact rows.
static void check(const Column& col, const std::vector<std::string>& patterns) {
    for (const std::string& pat : patterns) {
        std::vector<size_t> want = rows_containing(col, pat);
        if (!pat.empty()) check_graph(col, pat, want);
        Analysis a = analyze(bytes(pat), pat.size(), col.dict, col.freq, col.row_count());
        std::vector<size_t> exact;
        candidate_rows(col.stream(), col.dict, a, exact);
        CHECK_MSG(exact == want, "%s: the walk's rows differ from byte containment (%zu against %zu)", pat.c_str(),
                  exact.size(), want.size());
        if (pat.empty()) {
            CHECK(a.matches_all);
            continue;
        }
        CHECK(a.covered_frequency == col.freq.of_cover(a.cover));
        CHECK(a.total_frequency == col.codes.size());
        for (size_t row : want) {
            bool holds = false;
            for (size_t i = col.offsets[row]; i < col.offsets[row + 1]; ++i)
                holds |= cover_matches(a.cover, col.codes.data(), i, col.codes.size());
            CHECK_MSG(holds, "%s: matching row %zu holds no covered code or pair", pat.c_str(), row);
        }
    }
}

static std::vector<std::string> url_rows(size_t count, Rng& rng) {
    static const char* hosts[] = {"www.example.com", "shop.example.org", "cdn.static.net", "api.service.io"};
    static const char* paths[] = {"page", "user", "item", "search", "img", "docs"};
    std::vector<std::string> rows;
    for (size_t i = 0; i < count; ++i) {
        std::string r = rng.below(4) == 0 ? "http://" : "https://";
        r += hosts[rng.below(4)];
        size_t depth = rng.below(4);
        for (size_t d = 0; d < depth; ++d) r += "/" + std::string(paths[rng.below(6)]) + std::to_string(rng.below(1000));
        if (rng.below(2)) r += "?user=" + std::to_string(rng.below(50)) + "&q=" + std::string(paths[rng.below(6)]);
        rows.push_back(r);
    }
    return rows;
}

static void sound_on_url_text() {
    Rng rng{3};
    std::vector<std::string> rows = url_rows(400, rng);
    rows.push_back("");
    rows.push_back(std::string(300, 'x') + "https://www.example.com/page1" + std::string(20, 'y'));
    Column col = column(rows);
    std::vector<std::string> patterns = {"",
                                         "e",
                                         "/",
                                         "://",
                                         "http",
                                         "https://",
                                         ".com/page",
                                         "https://www.example.com",
                                         "user=1",
                                         "q=docs",
                                         "zzz absent",
                                         "example.org/item",
                                         rows[5],
                                         rows.back(),
                                         rows.back().substr(1)};
    check(col, patterns);
}

static void sound_on_repetitive_text() {
    std::vector<std::string> rows = {std::string(16, 'a'),   "aaaaaaaa aaaaaaaa", "xaaaaay", "aaa",  "aa",
                                     "zzaazz",               "abababababab",      "ba",     "aab",  "baa",
                                     std::string(600, 'a'),  "appappapple",       "apple",  "app",  "papp",
                                     "abcabcabcabc",         "cab",               "bca",    "abcab"};
    Column col = column(rows);
    check(col, {"a", "aa", "aaa", "aaaa", "aaaaaaaaa", "ab", "ba", "aba", "bab", "zz", "za", "app", "apple", "pp",
                "ppa", "abc", "cabc", "bcab", std::string(64, 'a'), std::string(300, 'a'), std::string(601, 'a')});
}

static void sound_on_random_bytes() {
    Rng rng{7};
    std::vector<std::string> rows;
    for (size_t i = 0; i < 300; ++i) {
        std::string r(1 + rng.below(40), '\0');
        for (char& c : r) c = static_cast<char>(rng.below(256));
        rows.push_back(r);
    }
    rows.push_back(std::string("\xff\xff\xff\xff", 4));
    rows.push_back(std::string("\x00\xff\x00\xff", 4));
    Column col = column(rows);
    CHECK_MSG(col.freq.count[ESCAPE] > 0, "random bytes should escape");
    std::vector<std::string> patterns = {std::string(1, '\xff'), std::string("\xff\xff", 2), std::string(1, '\0'),
                                         std::string("\x00\xff", 2)};
    for (size_t i = 0; i < 40; ++i) {
        const std::string& row = rows[rng.below(300)];
        size_t len = 1 + rng.below(std::min<size_t>(row.size(), 12));
        size_t at = rng.below(row.size() - len + 1);
        patterns.push_back(row.substr(at, len));
    }
    for (size_t i = 0; i < 8; ++i) patterns.push_back(std::string(1, static_cast<char>(rng.below(256))));
    check(col, patterns);
}

// Text with a handful of bytes the table has no symbol for, so occurrences
// begin, end and continue through escapes.
static void sound_with_escapes_inside_the_needle() {
    Rng rng{11};
    std::vector<std::string> rows = url_rows(300, rng);
    std::string rare;
    for (int b = 0x80; b < 0x100; ++b) rare.push_back(static_cast<char>(b));
    rows.push_back(rare);
    rows.push_back("page\x80\x81page");
    rows.push_back("\x90user=1\x91");
    rows.push_back("x\xff\xffy");
    rows.push_back("\x80");
    rows.push_back("ab\x80");
    rows.push_back("\x80\x80\x80");
    Column col = column(rows);
    CHECK_MSG(col.freq.count[ESCAPE] > 0, "rare bytes should escape");
    size_t before = g_escape_edges;
    check(col, {std::string(1, '\x80'), "page\x80", "\x80\x81page", "e\x80\x81p", "\x90user", "user=1\x91",
                std::string("\xff\xff", 2), "x\xff", "\xffy", "\x80\x80", "b\x80", rare.substr(3, 9)});
    CHECK_MSG(g_escape_edges > before, "no pattern built an escape edge");
    CHECK_MSG(g_pair_edges > 0, "no pattern built a pair edge");
}

// The anchored walks against byte prefix and suffix on rows the real encoder
// compressed: the end anchor relies on its parse at the end of a string.
static bool anchored_at(const std::string& row, const std::string& pat, Anchor anchor) {
    if (row.size() < pat.size()) return false;
    return row.compare(anchor == Anchor::Start ? 0 : row.size() - pat.size(), pat.size(), pat) == 0;
}

template <Anchor A>
static std::vector<uint32_t> anchored_rows(const Column& col, const scan::Walk& walk) {
    std::vector<uint32_t> got;
    for (uint32_t r = 0; r < col.row_count(); ++r)
        if (walk.check_anchored<A>(col.dict, col.codes.data(), col.offsets[r], col.offsets[r + 1])) got.push_back(r);
    return got;
}

static void check_anchored(const Column& col, const std::vector<std::string>& patterns) {
    for (const std::string& pat : patterns) {
        if (pat.empty()) continue;
        for (const Anchor anchor : {Anchor::Start, Anchor::End}) {
            std::vector<uint32_t> want;
            for (uint32_t r = 0; r < col.row_count(); ++r)
                if (anchored_at(col.rows[r], pat, anchor)) want.push_back(r);
            Analysis a = analyze(bytes(pat), pat.size(), col.dict, col.freq, col.row_count(), anchor);
            std::vector<uint32_t> got = anchor == Anchor::Start ? anchored_rows<Anchor::Start>(col, a.walk)
                                                                : anchored_rows<Anchor::End>(col, a.walk);
            CHECK_MSG(got == want, "%s anchored at the %s: %zu rows against %zu", pat.c_str(),
                      anchor == Anchor::Start ? "start" : "end", got.size(), want.size());
        }
    }
}

// Patterns taken from the rows' heads and tails, so both anchors match, plus
// whole rows and bytes the table escapes.
static void anchored_rows_match_prefix_and_suffix() {
    Rng rng{29};
    std::vector<std::string> rows = url_rows(300, rng);
    for (const char* r : {"aaaaaaaa", "aaa", "aa", "a", "appappapple", "apple", "app", "abcabcabc", "cab", ""})
        rows.push_back(r);
    std::string rare;
    for (int b = 0x80; b < 0x100; ++b) rare.push_back(static_cast<char>(b));
    for (const std::string& r : {rare, std::string("page\x80"), std::string("\x80page"), std::string("x\xff\xff"),
                                 std::string("\xff\xffy"), std::string("\xff"), std::string("ab\x80\x81")})
        rows.push_back(r);
    Column col = column(rows);
    std::vector<std::string> patterns = {"a", "aa", "aaa", "e", "le", "pple", "apple", "app", "abc", "cab",
                                         "http", "https://", "https://www.example.com", "docs", "\x80",
                                         "\xff", "\xff\xff", "\x80\x81", "page\x80", "zzz absent"};
    for (size_t i = 0; i < 60; ++i) {
        const std::string& row = rows[rng.below(rows.size())];
        if (row.empty()) continue;
        const size_t len = 1 + rng.below(std::min<size_t>(row.size(), 24));
        patterns.push_back(row.substr(0, len));
        patterns.push_back(row.substr(row.size() - len));
    }
    patterns.push_back(rows[7]);
    patterns.push_back(rare);
    check_anchored(col, patterns);
}

static void empty_probe_cover_appends_nothing() {
    Column col = column({"alpha", "beta", "gamma"});
    Analysis a = analyze(bytes("\x01\x02\x03"), 3, col.dict, col.freq, col.row_count());
    std::vector<size_t> got;
    candidate_rows(col.stream(), col.dict, a, got);
    CHECK(rows_containing(col, "\x01\x02\x03").empty());
    CHECK(got.empty());
}

static void false_zero_frequencies_cannot_hide_a_true_match() {
    std::vector<std::string> rows = {"alpha beta", "gamma", "alphabet soup", "delta"};
    Column col = column(rows);
    Frequency zeros;
    zeros.total = col.freq.total;
    std::string pat = "alpha";
    Analysis a = analyze(bytes(pat), pat.size(), col.dict, zeros, col.row_count());
    CHECK(!a.cover.empty());
    CHECK(a.covered_frequency == 0);
    std::vector<size_t> want = rows_containing(col, pat);
    std::vector<size_t> exact;
    candidate_rows(col.stream(), col.dict, a, exact);
    CHECK(exact == want);
}

static void cover_probes_the_maximal_runs_of_its_membership() {
    ProbeCover pf = ProbeCover::from_runs({{7, 8}, {0, 0}, {3, 3}, {6, 7}, {1, 1}, {8, 8}});
    CHECK(pf.points == std::vector<uint8_t>{3});
    CHECK(pf.ranges.size() == 2 && pf.ranges[0].begin == 0 && pf.ranges[0].last == 1 && pf.ranges[1].begin == 6 &&
          pf.ranges[1].last == 8);
    bool members[] = {true, true, false, true, false, false, true, true, true};
    for (uint8_t code = 0; code < 9; ++code) CHECK_MSG(pf.contains(code) == members[code], "membership at %u", code);
    CHECK(ProbeCover::from_runs({}).empty());
    ProbeCover top = ProbeCover::from_runs({{254, 254}, {255, 255}});
    CHECK(top.points.empty() && top.ranges.size() == 1 && top.ranges[0].last == 255);
}

static void planned_cover_prices_at_or_below_the_frequency_cut() {
    Rng rng{5};
    Column col = column(url_rows(500, rng));
    const size_t codes = col.codes.size();
    for (std::string pat : {"e", "://", ".com/page", "https://www.example.com", "user=1"}) {
        AlignmentGraph g = build_alignment_graph(col.dict, bytes(pat), pat.size(), col.freq);
        CutGraph cg = CutGraph::with_pairs(g, col.freq);
        ProbeCover baseline = from_edge_cut(cut_edges(cg, by_frequency));
        double baseline_ns = scan::policy::scan_ns(baseline, col.freq.of_cover(baseline), codes);
        Priced p = cheapest_cover(cg, col.freq);
        CHECK(p.covered == col.freq.of_cover(p.cover));
        CHECK(p.scan_ns == scan::policy::scan_ns(p.cover, p.covered, codes));
        CHECK_MSG(p.scan_ns <= baseline_ns, "%s: sweep %f against %f", pat.c_str(), p.scan_ns, baseline_ns);
    }
}

// Synthetic graphs for the cut alone. Uncuttable steps are SetTooBig.
static Edge synthetic(uint32_t from, uint32_t to, int64_t frequency) {
    if (frequency < 0) return Edge{from, to, Probe::SetTooBig, 0, {}, CodeRange{0, 0}, CodePair{0, 0}, 0, 0, 0, 0};
    return Edge{from, to, Probe::Point, 0, {0}, CodeRange{0, 0}, CodePair{0, 0}, static_cast<uint32_t>(frequency), 1, 0, 0};
}

using Steps = std::vector<std::pair<uint32_t, uint32_t>>;

static Steps steps(const std::vector<const Edge*>& cut) {
    Steps s;
    for (const Edge* e : cut) s.emplace_back(e->from, e->to);
    return s;
}

static void shared_suffix_beats_two_local_choices() {
    CutGraph g{{synthetic(0, 1, -1), synthetic(0, 2, -1), synthetic(1, 3, 4), synthetic(2, 3, 4), synthetic(3, 4, 6)},
               5, 0, 4};
    CHECK(steps(cut_edges(g, by_frequency)) == Steps({{3, 4}}));
}

static void disjoint_paths_are_cut_separately() {
    CutGraph g{{synthetic(0, 1, -1), synthetic(1, 4, 5), synthetic(0, 2, -1), synthetic(2, 3, 9), synthetic(3, 4, 0)},
               5, 0, 4};
    CHECK(steps(cut_edges(g, by_frequency)) == Steps({{1, 4}, {3, 4}}));
}

static void deep_chain_does_not_exhaust_the_stack() {
    const uint32_t len = 100000;
    CutGraph g{{}, len, 0, len - 1};
    for (uint32_t v = 0; v + 1 < len; ++v) g.edges.push_back(synthetic(v, v + 1, 7));
    g.edges[len / 2] = synthetic(len / 2, len / 2 + 1, 3);
    CHECK(steps(cut_edges(g, by_frequency)) == Steps({{len / 2, len / 2 + 1}}));
}

// One Finder for every call, so its scratch is reused with stale entries.
static void selected_scan_is_the_full_scan_filtered() {
    Rng rng{5};
    Column col = column(url_rows(3000, rng));
    const uint32_t n = static_cast<uint32_t>(col.row_count());
    std::vector<std::vector<uint32_t>> selections = {{}, {0}, {n - 1}};
    std::vector<uint32_t> all, step7, runs, sparse;
    for (uint32_t r = 0; r < n; ++r) {
        all.push_back(r);
        if (r % 7 == 0) step7.push_back(r);
        if ((r / 40) % 3 == 0) runs.push_back(r);
        if (r * 2654435761u % 97 < 3) sparse.push_back(r);
    }
    selections.insert(selections.end(), {all, step7, runs, sparse});
    Finder finder;
    for (std::string pat : {"example", ".com", "://", "page", "user", "", "zzzzqq"}) {
        Analysis a = analyze(bytes(pat), pat.size(), col.dict, col.freq, col.row_count());
        std::vector<size_t> full;
        candidate_rows(col.stream(), col.dict, a, full);
        for (const auto& sel : selections) {
            std::vector<size_t> want, got;
            for (size_t r : full)
                if (std::binary_search(sel.begin(), sel.end(), static_cast<uint32_t>(r))) want.push_back(r);
            finder.find(col.stream(), col.dict, a, sel.data(), sel.size(), got);
            CHECK_MSG(got == want, "%s, %zu selected: %zu rows against %zu", pat.c_str(), sel.size(), got.size(),
                      want.size());
        }
    }
}

// The per-row scan of a sparse selection over rows of several 64-code blocks,
// with the match in a late block, empty rows, and the last row selected.
static void sparse_selection_over_long_rows() {
    std::vector<std::string> rows;
    for (uint32_t i = 0; i < 600; ++i) {
        std::string row;
        if (i % 7 != 0) {
            for (uint32_t j = 0; j < 60 + i % 50; ++j) row += "w" + std::to_string((i * 31 + j) % 997) + " ";
            if (i % 3 == 0) row += "sunflower";
        }
        rows.push_back(row);
    }
    Column col = column(rows);
    const uint32_t n = static_cast<uint32_t>(col.row_count());
    std::vector<uint32_t> sel;
    for (uint32_t r = 0; r < n; ++r)
        if (r % 41 == 0 || r % 41 == 3 || r == n - 1) sel.push_back(r);
    Finder finder;
    for (std::string pat : {"sunflower", "w1", "zzzzqq"}) {
        Analysis a = analyze(bytes(pat), pat.size(), col.dict, col.freq, col.row_count());
        std::vector<size_t> full, want, got;
        candidate_rows(col.stream(), col.dict, a, full);
        for (size_t r : full)
            if (std::binary_search(sel.begin(), sel.end(), static_cast<uint32_t>(r))) want.push_back(r);
        finder.find(col.stream(), col.dict, a, sel.data(), sel.size(), got);
        CHECK_MSG(got == want, "%s: %zu rows against %zu", pat.c_str(), got.size(), want.size());
    }
}

int main() {
    shared_suffix_beats_two_local_choices();
    disjoint_paths_are_cut_separately();
    deep_chain_does_not_exhaust_the_stack();
    cover_probes_the_maximal_runs_of_its_membership();
    sound_on_url_text();
    sound_on_repetitive_text();
    sound_on_random_bytes();
    sound_with_escapes_inside_the_needle();
    anchored_rows_match_prefix_and_suffix();
    empty_probe_cover_appends_nothing();
    false_zero_frequencies_cannot_hide_a_true_match();
    planned_cover_prices_at_or_below_the_frequency_cut();
    selected_scan_is_the_full_scan_filtered();
    sparse_selection_over_long_rows();
    return finish("prefilter tests");
}
