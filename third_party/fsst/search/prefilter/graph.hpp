#pragma once

// The alignment graph: every layout of the needle across symbol boundaries
// as one DAG, whose source-to-sink cuts are the sound covers. Node offset is
// the parse position at needle[offset..], node 0 the source, node needle_len
// the sink.

#include <algorithm>
#include <array>
#include <cassert>
#include <cstring>
#include <initializer_list>
#include <vector>

#include "cover.hpp"
#include "dictionary.hpp"
#include "frequency.hpp"

namespace fsst::search::prefilter {

constexpr size_t PROBE_SET_SIZE_LIMIT = 512;
constexpr size_t PROBE_SET_SIZE_LIMIT_ALIGNMENT_1 = 16;

// Point: the greedy symbol at an interior offset. Escape: the marker 255
// with the literal needle byte behind it. Range: the symbols a needle suffix
// is a prefix of, into the sink, one code range since the codes are in byte
// order. Set: an alignment's first symbols out of the source, or the
// symbols holding the whole needle past their start. SetTooBig: an
// alignment whose set was not enumerated; a cut may not select it. Pair:
// two consecutive greedy symbols, only in the cut graph of CutGraph::with_pairs.
enum class Probe : uint8_t { Point, Escape, Range, Set, SetTooBig, Pair };

// Where the pattern must sit in the row. None is a substring anywhere. Start is a prefix: the pattern
// begins at the row's first byte, so the alignments that begin it inside a symbol are not layouts it can
// have. End is a suffix: the pattern ends at the row's last byte, and no symbol runs past it, so every
// step is the greedy one and the last symbol ends exactly on the needle's end.
enum class Anchor : uint8_t { None, Start, End };

// An edge's codes, up to INLINE of them in place: a point or an escape is one
// code, so only a large set reaches the heap.
class EdgeCodes {
   public:
    EdgeCodes() = default;
    EdgeCodes(std::initializer_list<uint8_t> codes) { assign(codes.begin(), codes.size()); }
    EdgeCodes(const std::vector<uint8_t>& codes) { assign(codes.data(), codes.size()); }

    size_t size() const { return size_; }
    bool empty() const { return size_ == 0; }
    const uint8_t* data() const { return size_ <= INLINE ? inline_ : heap_.data(); }
    const uint8_t* begin() const { return data(); }
    const uint8_t* end() const { return data() + size_; }
    uint8_t operator[](size_t i) const { return data()[i]; }

   private:
    static constexpr size_t INLINE = 14;

    void assign(const uint8_t* codes, size_t n) {
        size_ = static_cast<uint16_t>(n);
        if (n <= INLINE)
            std::memcpy(inline_, codes, n);
        else
            heap_.assign(codes, codes + n);
    }

    uint16_t size_ = 0;
    uint8_t inline_[INLINE] = {};
    std::vector<uint8_t> heap_;  // used past INLINE codes; empty, it holds no memory
};

struct Edge {
    uint32_t from;
    uint32_t to;
    Probe probe;
    uint8_t byte;                // Escape only: the literal after the marker
    EdgeCodes codes;             // Point, Escape, Set: ascending, one for Point and Escape
    CodeRange range;             // Range only
    CodePair pair;               // Pair only
    uint32_t frequency;          // codes the probe matches in the stream; an estimate for a pair
    uint32_t points;             // the lambda weight's terms: runs the codes merge to, one range, one pair
    uint32_t ranges;
    uint32_t pairs;

    bool cuttable() const { return probe != Probe::SetTooBig; }
};

struct AlignmentGraph {
    std::vector<Edge> edges;
    size_t needle_len;
    Anchor anchor;

    size_t node_count() const { return needle_len + 1; }
    uint32_t source() const { return 0; }
    uint32_t sink() const { return static_cast<uint32_t>(needle_len); }
};

// The cover a cut's probes form: every code a run of one, merged by from_runs.
inline ProbeCover from_edge_cut(const std::vector<const Edge*>& cut) {
    std::vector<CodeRange> runs;
    std::vector<CodePair> pairs;
    for (const Edge* e : cut) {
        assert(e->cuttable());
        if (e->probe == Probe::Range) runs.push_back(e->range);
        if (e->probe == Probe::Pair) pairs.push_back(e->pair);
        for (uint8_t c : e->codes) runs.push_back({c, c});
    }
    ProbeCover cover = ProbeCover::from_runs(std::move(runs));
    std::sort(pairs.begin(), pairs.end());
    pairs.erase(std::unique(pairs.begin(), pairs.end()), pairs.end());
    cover.pairs = std::move(pairs);
    return cover;
}

// What the min cut runs on: the alignment graph unrolled one step, so a
// greedy symbol may be probed together with the symbol before it.
struct CutGraph {
    std::vector<Edge> edges;
    size_t node_count;
    uint32_t source;
    uint32_t sink;

    // Every node except source and sink is copied once per edge entering it,
    // so a copy knows the symbol that led to it. A Point edge b, or a
    // terminal Range of the one symbol b, out of a copy entered by Point a
    // becomes two edges in series, the edge itself then Pair(a, b): a path
    // through them is blocked by either, and the cut takes the cheaper.
    // Paths are otherwise those of g, so every cut is still a sound cover.
    // `origin`, when given, receives per edge the alignment edge it came from.
    static CutGraph with_pairs(const AlignmentGraph& g, const Frequency& freq,
                               std::vector<uint32_t>* origin = nullptr) {
        const size_t needle_len = g.needle_len, edge_count = g.edges.size();
        // The edges into node v, in edge order, are into[start[v] .. start[v + 1]);
        // the copy of node v entered by into[s] is node 1 + s - start[1].
        std::vector<uint32_t> start(needle_len + 2, 0), into(edge_count), slot(edge_count), fill(needle_len + 1);
        for (const Edge& e : g.edges) ++start[e.to + 1];
        for (size_t v = 0; v <= needle_len; ++v) start[v + 1] += start[v];
        std::copy(start.begin(), start.begin() + static_cast<std::ptrdiff_t>(needle_len + 1), fill.begin());
        size_t copies = 0;
        for (uint32_t at = 0; at < edge_count; ++at) {
            slot[at] = fill[g.edges[at].to]++;
            into[slot[at]] = at;
            copies += g.edges[at].from == 0 ? 1 : start[g.edges[at].from + 1] - start[g.edges[at].from];
        }
        auto copy_of = [&](uint32_t s) { return 1 + s - start[1]; };
        uint32_t next = copy_of(start[needle_len]);
        const uint32_t sink = next++;
        CutGraph cg{{}, 0, 0, sink};
        cg.edges.reserve(2 * copies);
        for (uint32_t at = 0; at < edge_count; ++at) {
            const Edge& e = g.edges[at];
            const uint32_t target = e.to != needle_len ? copy_of(slot[at]) : sink;
            const uint32_t first = e.from == 0 ? 0 : start[e.from], last = e.from == 0 ? 1 : start[e.from + 1];
            for (uint32_t s = first; s < last; ++s) {
                const uint32_t cu = e.from == 0 ? 0 : copy_of(s);
                const Edge* entered_by = e.from == 0 ? nullptr : &g.edges[into[s]];
                bool one_symbol = e.probe == Probe::Point || (e.probe == Probe::Range && e.range.begin == e.range.last);
                bool pairable = one_symbol && entered_by != nullptr && entered_by->probe == Probe::Point;
                if (!pairable) {
                    Edge copy = e;
                    copy.from = cu;
                    copy.to = target;
                    cg.edges.push_back(std::move(copy));
                    if (origin) origin->push_back(at);
                    continue;
                }
                uint32_t mid = next++;
                Edge point = e;
                point.from = cu;
                point.to = mid;
                cg.edges.push_back(std::move(point));
                CodePair pair{entered_by->codes[0], e.probe == Probe::Point ? e.codes[0] : e.range.begin};
                cg.edges.push_back(Edge{mid, target, Probe::Pair, 0, {}, CodeRange{0, 0}, pair, freq.pair_estimate(pair),
                                        0, 0, 1});
                if (origin) origin->insert(origin->end(), {at, at});
            }
        }
        cg.node_count = next;
        return cg;
    }
};

// For each alignment_index >= 1, the symbols whose last alignment_index bytes
// are needle[..alignment_index] and are longer than that, and the symbols
// holding the whole needle at an offset past their start. Counts saturate one
// past the limit.
struct Candidates {
    std::array<size_t, MAX_TOKEN_SIZE> count{};
    std::array<std::vector<uint8_t>, MAX_TOKEN_SIZE> first;
    std::vector<uint8_t> contained;

    void record(size_t alignment_index, uint8_t code) {
        size_t limit = alignment_index == 1 ? PROBE_SET_SIZE_LIMIT_ALIGNMENT_1 : PROBE_SET_SIZE_LIMIT;
        if (count[alignment_index] < limit) {
            first[alignment_index].push_back(code);
            ++count[alignment_index];
        } else {
            count[alignment_index] = PROBE_SET_SIZE_LIMIT + 1;
        }
    }
};

inline Candidates alignment_candidates(const Dictionary& dict, const uint8_t* needle, size_t needle_len,
                                       Anchor anchor = Anchor::None) {
    Candidates c;
    size_t alignment_count = std::min(needle_len, MAX_TOKEN_SIZE);
    const uint64_t key = load_word(needle, needle_len);
    for (size_t code = 0; code < dict.count; ++code) {
        const uint64_t w = dict.word(code);
        size_t len = dict.length(code);
        // The symbol's last alignment_index bytes against needle[..alignment_index].
        for (size_t alignment_index = 1; alignment_index < alignment_count && alignment_index < len; ++alignment_index)
            if (same_prefix(w >> (8 * (len - alignment_index)), key, alignment_index))
                c.record(alignment_index, static_cast<uint8_t>(code));
        if (anchor == Anchor::End) {
            // the row's last symbol, so it holds the needle as its tail
            if (len > needle_len && same_prefix(w >> (8 * (len - needle_len)), key, needle_len))
                c.contained.push_back(static_cast<uint8_t>(code));
            continue;
        }
        // A symbol starting with the needle is the terminal set's at offset 0.
        if (len >= needle_len && same_prefix(w, key, needle_len)) continue;
        for (size_t p = 1; p + needle_len <= len; ++p)
            if (same_prefix(w >> (8 * p), key, needle_len)) {
                c.contained.push_back(static_cast<uint8_t>(code));
                break;
            }
    }
    return c;
}

namespace detail {

struct Builder {
    const Dictionary& dict;
    const uint8_t* needle;
    size_t needle_len;
    Anchor anchor;
    const Frequency& freq;
    std::vector<Edge> edges;
    std::vector<bool> built;
    // first[b] .. first[b + 1]: the codes whose symbol starts with byte b, a
    // run of the sorted table.
    std::array<uint16_t, 257> first{};

    Builder(const Dictionary& dict, const uint8_t* needle, size_t needle_len, Anchor anchor, const Frequency& freq)
        : dict(dict), needle(needle), needle_len(needle_len), anchor(anchor), freq(freq), built(needle_len, false) {
        edges.reserve(2 * needle_len + 16);
        for (size_t c = 0; c < dict.count; ++c) ++first[dict.symbol(c)[0] + 1];
        for (size_t b = 0; b < 256; ++b) first[b + 1] += first[b];
    }

    void add(uint32_t from, uint32_t to, Probe probe, EdgeCodes codes, uint8_t byte = 0) {
        // The points and ranges ProbeCover::from_runs would merge the codes into,
        // counted in place: a run is codes each at most one past the one before.
        assert(std::is_sorted(codes.begin(), codes.end()));
        uint32_t points = 0, ranges = 0;
        for (size_t i = 0; i < codes.size();) {
            size_t j = i + 1;
            while (j < codes.size() && codes[j] <= codes[j - 1] + 1) ++j;
            ++(codes[j - 1] == codes[i] ? points : ranges);
            i = j;
        }
        Edge e{from, to, probe, byte, std::move(codes), CodeRange{0, 0}, CodePair{0, 0}, 0, points, ranges, 0};
        e.frequency = freq.of_codes(e.codes);
        edges.push_back(std::move(e));
    }

    void add_range(uint32_t from, uint32_t to, CodeRange range) {
        edges.push_back(Edge{from, to, Probe::Range, 0, {}, range, CodePair{0, 0}, freq.of_range(range), 0, 1, 0});
    }

    // The symbols needle[offset..] is a prefix of, the exact one included.
    CodeRange terminal_range(size_t offset) const {
        size_t tail_len = needle_len - offset;
        return tail_len <= MAX_TOKEN_SIZE ? dict.prefix_range(needle + offset, tail_len) : CodeRange::none();
    }

    // The encoder's longest match restricted to the needle: the longest
    // symbol that is a prefix of needle[offset..]. Symbols of three or more
    // bytes have distinct three-byte prefixes, so at most one can match and
    // the encoder takes it before any shorter one.
    bool greedy(size_t offset, uint8_t& code, size_t& len) const {
        const size_t room = std::min(needle_len - offset, MAX_TOKEN_SIZE);
        const uint64_t word = load_word(needle + offset, room);
        const uint8_t lead = needle[offset];
        len = 0;
        // only the symbols starting with the needle's byte here can be a prefix of it
        for (size_t c = first[lead]; c < first[lead + 1]; ++c) {
            const size_t l = dict.length(c);
            if (l <= room && l > len && same_prefix(word, dict.word(c), l)) {
                code = static_cast<uint8_t>(c);
                len = l;
            }
        }
        return len > 0;
    }

    // The steps out of offset; returns where the parse lands. No symbol
    // matching means the encoder escapes needle[offset] and continues at
    // offset + 1.
    size_t build_state(size_t offset) {
        uint32_t state = static_cast<uint32_t>(offset);
        // at the row's end nothing follows the needle, so only the greedy step can end it
        CodeRange terminal = anchor == Anchor::End ? CodeRange::none() : terminal_range(offset);
        if (!terminal.empty()) add_range(state, sink(), terminal);
        uint8_t code;
        size_t len;
        if (greedy(offset, code, len)) {
            size_t next = offset + len;
            if (next < needle_len || anchor == Anchor::End)
                add(state, static_cast<uint32_t>(next), Probe::Point, {code});
            else
                assert(terminal.contains(code));
            return next;
        }
        add(state, static_cast<uint32_t>(offset + 1), Probe::Escape, {ESCAPE}, needle[offset]);
        return offset + 1;
    }

    void ensure_chain(size_t start) {
        size_t offset = start;
        while (!built[offset]) {
            built[offset] = true;
            size_t next = build_state(offset);
            if (next >= needle_len) break;
            offset = next;
        }
    }

    uint32_t sink() const { return static_cast<uint32_t>(needle_len); }
};

}  // namespace detail

inline AlignmentGraph build_alignment_graph(const Dictionary& dict, const uint8_t* needle, size_t needle_len,
                                            const Frequency& freq, Anchor anchor = Anchor::None) {
    assert(needle_len > 0);
    assert(dict.sorted());
    detail::Builder b(dict, needle, needle_len, anchor, freq);
    if (anchor == Anchor::Start) {
        // a prefix begins at the row's first byte: the greedy chain from offset 0 is its only layout
        b.ensure_chain(0);
        return AlignmentGraph{std::move(b.edges), needle_len, anchor};
    }

    Candidates cand = alignment_candidates(dict, needle, needle_len, anchor);
    size_t alignment_count = std::min(needle_len, MAX_TOKEN_SIZE);
    for (size_t alignment_index = 0; alignment_index < alignment_count; ++alignment_index) {
        if (alignment_index != 0 && cand.count[alignment_index] == 0) continue;
        b.ensure_chain(alignment_index);
        if (alignment_index != 0) {
            if (cand.count[alignment_index] <= PROBE_SET_SIZE_LIMIT)
                b.add(0, static_cast<uint32_t>(alignment_index), Probe::Set, cand.first[alignment_index]);
            else
                b.add(0, static_cast<uint32_t>(alignment_index), Probe::SetTooBig, {});
        }
    }
    if (!cand.contained.empty()) b.add(0, b.sink(), Probe::Set, cand.contained);
    assert(b.edges.size() <= 2 * needle_len + 16);
    return AlignmentGraph{std::move(b.edges), needle_len, anchor};
}

}  // namespace fsst::search::prefilter
