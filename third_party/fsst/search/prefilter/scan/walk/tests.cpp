// The walk against byte containment over hand-built symbol tables, trying
// every code of a row as the hit. The tokenizer here is the encoder's parse:
// longest symbol that matches, else marker and literal.

#include <algorithm>
#include <cstring>
#include <string>
#include <vector>

#include "../../graph.hpp"
#include "../tests.hpp"
#include "walk.hpp"

using namespace fsst::search::prefilter;
using scan::Walk;

struct Column {
    Dictionary dict;
    std::vector<uint8_t> codes;
    std::vector<uint32_t> offsets{0};
};

// `singles` get one-byte symbols; other bytes escape. Codes in byte order,
// as fsst_sort_codes writes them.
static Dictionary dictionary(const std::string& singles, const std::vector<std::string>& extra) {
    std::vector<std::string> symbols;
    for (char c : singles) symbols.emplace_back(1, c);
    for (const std::string& s : extra) symbols.push_back(s);
    std::sort(symbols.begin(), symbols.end());
    Dictionary d;
    CHECK(symbols.size() <= 255);
    d.count = symbols.size();
    for (size_t c = 0; c < symbols.size(); ++c) {
        d.len[c] = static_cast<uint8_t>(symbols[c].size());
        std::memcpy(d.bytes[c].data(), symbols[c].data(), symbols[c].size());
    }
    CHECK(d.sorted());
    return d;
}

static uint8_t code_of(const Dictionary& d, const std::string& symbol) {
    for (size_t c = 0; c < d.count; ++c)
        if (d.length(c) == symbol.size() && std::memcmp(d.symbol(c), symbol.data(), symbol.size()) == 0)
            return static_cast<uint8_t>(c);
    CHECK_MSG(false, "no symbol %s", symbol.c_str());
    return 0;
}

static void tokenize(const Dictionary& d, const std::string& row, std::vector<uint8_t>& codes) {
    size_t at = 0;
    while (at < row.size()) {
        size_t best = 0, code = 0;
        for (size_t c = 0; c < d.count; ++c) {
            size_t l = d.length(c);
            if (l > best && at + l <= row.size() && std::memcmp(d.symbol(c), row.data() + at, l) == 0) {
                best = l;
                code = c;
            }
        }
        if (best == 0) {
            codes.push_back(ESCAPE);
            codes.push_back(static_cast<uint8_t>(row[at]));
            at += 1;
        } else {
            codes.push_back(static_cast<uint8_t>(code));
            at += best;
        }
    }
}

static Column column(const std::string& singles, const std::vector<std::string>& extra,
                     const std::vector<std::string>& rows) {
    Column col;
    col.dict = dictionary(singles, extra);
    for (const std::string& row : rows) {
        tokenize(col.dict, row, col.codes);
        col.offsets.push_back(static_cast<uint32_t>(col.codes.size()));
    }
    return col;
}

// Every printable byte as a one-byte symbol: the rows of the ported tests
// then never escape, and 255 codes leave room for the extras.
static std::string all_bytes() {
    std::string s;
    for (int b = 32; b < 127; ++b) s.push_back(static_cast<char>(b));
    return s;
}

static const std::string LETTERS = "abcdefghijklmnopqrstuvwxyz ";

// Byte span of the unit at each code index; literals get no span.
struct Unit {
    size_t start, end;
    bool head;
};

static std::vector<Unit> units(const Dictionary& d, const uint8_t* codes, size_t len) {
    std::vector<Unit> u(len, Unit{0, 0, false});
    size_t at = 0;
    for (size_t i = 0; i < len; ++i) {
        if (codes[i] == ESCAPE) {
            u[i] = Unit{at, at + 1, true};
            ++i;
            at += 1;
        } else {
            u[i] = Unit{at, at + d.length(codes[i]), true};
            at += d.length(codes[i]);
        }
    }
    return u;
}

// Every unit of every row as the hit. A unit passes iff it is a parse step
// of some occurrence: it overlaps the occurrence and is not an entry token
// the planner never enumerated, which no cover can name.
static void walk_decides(const std::string& singles, const std::vector<std::string>& extra,
                         const std::vector<std::string>& rows, const std::string& needle) {
    Column col = column(singles, extra, rows);
    const uint8_t* pat = reinterpret_cast<const uint8_t*>(needle.data());
    const size_t needle_len = needle.size();
    Frequency freq = Frequency::of(col.codes.data(), col.codes.size());
    AlignmentGraph graph = build_alignment_graph(col.dict, pat, needle_len, freq);
    Candidates cand = alignment_candidates(col.dict, pat, needle_len);
    Walk walk(graph, pat);
    for (size_t row = 0; row < rows.size(); ++row) {
        size_t start = col.offsets[row], end = col.offsets[row + 1];
        std::vector<Unit> u = units(col.dict, col.codes.data() + start, end - start);
        std::vector<bool> expected(end - start, false);
        for (size_t s = 0; s + needle_len <= rows[row].size(); ++s) {
            if (rows[row].compare(s, needle_len, needle) != 0) continue;
            for (size_t i = 0; i < u.size(); ++i) {
                if (!u[i].head || u[i].end <= s || u[i].start >= s + needle_len) continue;
                bool entry = u[i].start < s && u[i].end < s + needle_len;
                if (entry && cand.count[u[i].end - s] > PROBE_SET_SIZE_LIMIT) continue;
                expected[i] = true;
            }
        }
        bool walked = false;
        for (size_t hit = start; hit < end; ++hit) {
            bool got = walk.check(col.dict, col.codes.data(), start, end, hit);
            walked |= got;
            CHECK_MSG(got == expected[hit - start], "needle %s row %zu hit %zu", needle.c_str(), row, hit - start);
        }
        bool contains = rows[row].find(needle) != std::string::npos;
        CHECK_MSG(walked == contains, "needle %s row %zu", needle.c_str(), row);
    }
}

// goo|gl|e is the alignment-0 parse; agoo enters at node 3 with the needle's
// head as its tail; es ends it inside a longer symbol; gooxgle and gogle hit
// the same symbols and must fail.
static void interior_entry_and_exit_layouts() {
    walk_decides(all_bytes(), {"goo", "gl", "es", "agoo", "oo"},
                 {"google", "xagoogle", "googles", "gooxgle", "gogle", "agoo gl e", ""}, "google");
}

static void whole_needle_inside_one_token() {
    walk_decides(all_bytes(), {"xgooglex", "googlez", "goo", "gl"}, {"xgooglex", "googlez", "xgoogle", "goog"},
                 "google");
}

// ab is a point step out of the source and the terminal set out of node 2.
static void a_token_on_two_edges() {
    walk_decides(all_bytes(), {"ab"}, {"abab", "xabab", "ababx", "abxab", "ab", "aabb"}, "abab");
}

static void walk_stops_at_the_row() {
    walk_decides(all_bytes(), {"goo", "gl"}, {"goo", "gle", "google"}, "google");
}

static void overlapping_occurrence_needs_no_back_edge() {
    std::vector<std::string> rows = {"appappapple", "appapple", "appappappapple", "xappappapplex", "appappaple",
                                     "appapp",      "apple"};
    walk_decides(all_bytes(), {"app", "le"}, rows, "appapple");
    walk_decides(all_bytes(), {"app", "apple", "le"}, rows, "appapple");
    walk_decides(all_bytes(), {"appa", "pp", "pple", "le"}, rows, "appapple");
    walk_decides(all_bytes(), {"aa", "aab", "aabaaabaa"}, {"aabaaabaa", "aabaab", "aaa", "baaab"}, "aaa");
}

// More than 16 symbols end with g, so alignment 1's set is never enumerated
// and the entry is answered off the dictionary.
static void entry_through_a_set() {
    std::vector<std::string> extra;
    for (char b = 'a'; b <= 'z'; ++b) extra.push_back(std::string(1, b) + "g");
    extra.push_back("oo");
    extra.push_back("gl");
    walk_decides(all_bytes(), extra, {"zgoogle", "zgoogl", "google", "zg oogle"}, "google");
}

// # has no symbol: the chain runs through an escape edge, at the start, in
// the middle and at the end of the needle.
static void escapes_inside_and_around_the_occurrence() {
    std::vector<std::string> rows = {"go#gle", "xgo#glex", "go##gle", "gogle", "go#gl", "#google", "google#",
                                     "#go#gle#", "g#o#gle", "o#gle", "#", "##", "go#", "#gle"};
    walk_decides(LETTERS, {"goo", "gl", "go", "le"}, rows, "go#gle");
    walk_decides(LETTERS, {"goo", "gl", "go", "le"}, rows, "#go");
    walk_decides(LETTERS, {"goo", "gl", "go", "le"}, rows, "gle#");
    walk_decides(LETTERS, {"goo", "gl", "go", "le"}, rows, "#");
    walk_decides(LETTERS, {"goo", "gl", "go", "le"}, rows, "##");
    walk_decides(LETTERS, {"goo", "gl", "go", "le"}, rows, "#go#gle#");
}

// A literal byte whose value is a covered code is a false hit the matcher
// cannot see through; the walk must.
static void a_literal_equal_to_a_covered_code() {
    Dictionary d = dictionary(LETTERS, {"goo", "gl"});
    std::string literal(1, static_cast<char>(code_of(d, "goo")));
    std::vector<std::string> rows = {literal + "gle", "x" + literal + "y", "google", literal + literal,
                                     "go" + literal + "gle", literal};
    walk_decides(LETTERS, {"goo", "gl"}, rows, "google");
    walk_decides(LETTERS, {"goo", "gl"}, rows, literal + "gle");
    walk_decides(LETTERS, {"goo", "gl"}, rows, literal);
}

// Runs of 255: literal 0xff bytes are 255 255, so the parity of the run
// decides what every position is.
static void runs_of_escape_markers() {
    std::string ff(1, '\xff');
    std::vector<std::string> rows = {"a" + ff + ff + "b", "a" + ff + "b", ff + ff + ff + ff, ff, "a" + ff, ff + "b",
                                     ff + ff + ff, "ab"};
    walk_decides(LETTERS, {"ab"}, rows, "a" + ff + ff + "b");
    walk_decides(LETTERS, {"ab"}, rows, ff);
    walk_decides(LETTERS, {"ab"}, rows, ff + ff);
    walk_decides(LETTERS, {"ab"}, rows, "a" + ff);
    walk_decides(LETTERS, {"ab"}, rows, ff + "b");
    walk_decides(LETTERS, {"ab"}, rows, ff + ff + ff);
}

// The prefix walk from the row's first code over runs of 255: a row starting
// with 0xff starts with the escape pair 255 255.
static void prefix_through_escaped_escapes() {
    const std::string ff(1, '\xff');
    const std::vector<std::string> rows = {ff, ff + ff, ff + ff + ff, ff + "b", ff + ff + "b", "a" + ff,
                                           "a" + ff + ff, "ab" + ff + "b", "b" + ff, "ab", ff + "ab" + ff};
    const std::vector<std::string> needles = {ff,       ff + ff,  ff + ff + ff, ff + "b",       ff + ff + "b",
                                              "a" + ff, "ab" + ff, ff + "ab",    "ab" + ff + "b"};
    Column col = column(LETTERS, {"ab"}, rows);
    Frequency freq = Frequency::of(col.codes.data(), col.codes.size());
    for (const std::string& needle : needles) {
        const uint8_t* pat = reinterpret_cast<const uint8_t*>(needle.data());
        AlignmentGraph graph = build_alignment_graph(col.dict, pat, needle.size(), freq, Anchor::Start);
        Walk walk(graph, pat);
        for (size_t row_index = 0; row_index < rows.size(); ++row_index) {
            const size_t row_start_index = col.offsets[row_index], row_end_index = col.offsets[row_index + 1];
            const bool got = walk.check_anchored<Anchor::Start>(col.dict, col.codes.data(), row_start_index,
                                                                row_end_index);
            const bool expected = rows[row_index].compare(0, needle.size(), needle) == 0;
            CHECK_MSG(got == expected, "needle %zu bytes row %zu", needle.size(), row_index);
        }
    }
}

// The suffix walk from the row's last code against the rows that end with
// the needle.
static void ends_decides(const std::string& singles, const std::vector<std::string>& extra,
                         const std::vector<std::string>& rows, const std::string& needle) {
    Column col = column(singles, extra, rows);
    const uint8_t* pat = reinterpret_cast<const uint8_t*>(needle.data());
    Frequency freq = Frequency::of(col.codes.data(), col.codes.size());
    AlignmentGraph graph = build_alignment_graph(col.dict, pat, needle.size(), freq, Anchor::End);
    Walk walk(graph, pat);
    for (size_t row_index = 0; row_index < rows.size(); ++row_index) {
        const size_t row_start_index = col.offsets[row_index], row_end_index = col.offsets[row_index + 1];
        const std::string& row = rows[row_index];
        const bool expected =
            row.size() >= needle.size() && row.compare(row.size() - needle.size(), needle.size(), needle) == 0;
        const bool got = walk.check_anchored<Anchor::End>(col.dict, col.codes.data(), row_start_index, row_end_index);
        CHECK_MSG(got == expected, "needle %s row %zu", needle.c_str(), row_index);
    }
}

// The needle starts at a symbol boundary, inside a symbol (agoo), or wholly
// inside the last symbol (xgoogle); symbols running past the needle (googlez,
// gles) must not end it.
static void suffix_through_entries_and_contained_symbols() {
    const std::vector<std::string> extra = {"goo", "gl", "le", "gle", "gles", "agoo", "xgoogle", "googlez", "oogle"};
    const std::vector<std::string> rows = {"google", "xgoogle", "agoogle", "googlez", "googles", "goo gle",
                                           "zgoogle", "googl", "oogle", "le", "e", "gle", "googlegoogle",
                                           "googlezgoogle", "xgooglex", ""};
    for (const std::string& needle : {"google", "oogle", "gle", "le", "e", "goo", "googlez", "xgoogle", "googlegoogle"})
        ends_decides(all_bytes(), extra, rows, needle);
}

// Escapes as the row's last code, in the middle of the needle, and 0xff
// escaped as 255 255 at the end.
static void suffix_through_escapes() {
    const std::string ff(1, '\xff');
    const std::vector<std::string> rows = {"go#gle", "xgo#gle", "go#gle#", "#", "##", "ab#", "a#b", ff,
                                           ff + ff, "a" + ff, "a" + ff + ff, ff + "b", "ab" + ff + "b", ""};
    for (const std::string& needle :
         {std::string("#"), std::string("##"), std::string("go#gle"), std::string("gle#"), ff, ff + ff, "a" + ff,
          ff + "b", "b" + ff, "ab" + ff + "b"})
        ends_decides(LETTERS, {"goo", "gl", "go", "le", "ab"}, rows, needle);
}

static void random_rows_against_suffix() {
    Rng rng{23};
    std::vector<std::string> extra = {"ab", "abc", "bca", "cab", "aab", "abcabc", "ca", "bb"};
    for (size_t round = 0; round < 40; ++round) {
        std::vector<std::string> rows;
        for (size_t r = 0; r < 30; ++r) {
            std::string row(rng.below(12), 'a');
            for (char& c : row) c = "abc#\xff"[rng.below(5)];
            rows.push_back(row);
        }
        std::string needle(1 + rng.below(5), 'a');
        for (char& c : needle) c = "abc#\xff"[rng.below(5)];
        // a needle that is some row's tail, so the true case is exercised
        const std::string& row = rows[rng.below(rows.size())];
        std::string tail = row.substr(row.size() - std::min<size_t>(row.size(), 1 + rng.below(5)));
        ends_decides("abc", extra, rows, needle);
        if (!tail.empty()) ends_decides("abc", extra, rows, tail);
    }
}

static void random_rows_against_containment() {
    Rng rng{19};
    std::vector<std::string> extra = {"ab", "abc", "bca", "cab", "aab", "abcabc", "ca", "bb"};
    for (size_t round = 0; round < 40; ++round) {
        std::vector<std::string> rows;
        for (size_t r = 0; r < 30; ++r) {
            std::string row(rng.below(12), 'a');
            for (char& c : row) c = "abc#\xff"[rng.below(5)];
            rows.push_back(row);
        }
        std::string needle(1 + rng.below(5), 'a');
        for (char& c : needle) c = "abc#\xff"[rng.below(5)];
        walk_decides("abc", extra, rows, needle);
    }
}

int main() {
    interior_entry_and_exit_layouts();
    whole_needle_inside_one_token();
    a_token_on_two_edges();
    walk_stops_at_the_row();
    overlapping_occurrence_needs_no_back_edge();
    entry_through_a_set();
    escapes_inside_and_around_the_occurrence();
    a_literal_equal_to_a_covered_code();
    runs_of_escape_markers();
    prefix_through_escaped_escapes();
    random_rows_against_containment();
    suffix_through_entries_and_contained_symbols();
    suffix_through_escapes();
    random_rows_against_suffix();
    return finish("prefilter walk tests");
}
