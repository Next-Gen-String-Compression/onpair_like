#pragma once

// Exact verification of a hit in the compressed domain. The graph lists the
// steps the hit code can be; forward compares the row with the greedy codes
// from a node to the sink until a terminal set, backward takes the one step
// into each node until the source or an entry the planner did not enumerate. An escape is two codes,
// marker then literal; a position is a literal iff the run of 255s before it
// has odd length.

#include <algorithm>
#include <array>
#include <vector>

#include "../../dictionary.hpp"
#include "../../graph.hpp"

namespace fsst::search::prefilter::scan {

class Walk {
  public:
    Walk() = default;

    Walk(const AlignmentGraph& graph, const uint8_t* needle)
        : entry_len_(graph.node_count()),
          sink_(graph.sink()),
          head_word_(load_word(needle, std::min(graph.needle_len, MAX_TOKEN_SIZE))) {
        nodes_.resize(graph.node_count());
        struct KeyedTransition {
            uint16_t key;
            uint32_t from;
            uint32_t to;
        };
        std::vector<KeyedTransition> transitions;
        for (const Edge& edge : graph.edges) {
            const uint32_t from = edge.from, to = edge.to;
            switch (edge.probe) {
                case Probe::Point:
                    nodes_[from].has_step = true;
                    nodes_[from].step = edge.codes[0];
                    nodes_[from].next = to;
                    transitions.push_back({edge.codes[0], from, to});
                    break;
                case Probe::Escape:
                    nodes_[from].has_step = true;
                    nodes_[from].escape = true;
                    nodes_[from].step = edge.byte;
                    nodes_[from].next = to;
                    transitions.push_back({escape_key(edge.byte), from, to});
                    break;
                case Probe::Range:
                    nodes_[from].terminal = edge.range;
                    for (size_t code = edge.range.begin; code <= edge.range.last; ++code)
                        transitions.push_back({static_cast<uint16_t>(code), from, to});
                    break;
                case Probe::Set:
                    for (const uint8_t code : edge.codes) transitions.push_back({code, from, to});
                    break;
                case Probe::SetTooBig: entry_len_[to] = static_cast<uint8_t>(to); break;
                case Probe::Pair: break;  // the cut graph's; the walk reads the alignment graph
            }
        }
        std::sort(transitions.begin(), transitions.end(), [](const KeyedTransition& a, const KeyedTransition& b) {
            return std::tie(a.key, a.from, a.to) < std::tie(b.key, b.from, b.to);
        });
        transitions.erase(std::unique(transitions.begin(), transitions.end(),
                                      [](const KeyedTransition& a, const KeyedTransition& b) {
                                          return a.key == b.key && a.from == b.from && a.to == b.to;
                                      }),
                          transitions.end());
        for (const KeyedTransition& transition : transitions) {
            ++head_[transition.key + 1];
            transitions_.push_back({transition.from, transition.to});
        }
        for (size_t key = 0; key < KEY_COUNT; ++key) head_[key + 1] += head_[key];
    }

    // Whether an occurrence inside codes[row_start_index..row_end_index) has the
    // code at `hit_index` as one of its parse steps.
    bool check(const Dictionary& dict, const uint8_t* codes, const size_t row_start_index, const size_t row_end_index,
               const size_t hit_index) const {
        const size_t hit_end_index = hit_index + (codes[hit_index] == ESCAPE ? 2 : 1);
        for (const CodeTransition& transition : possible_transitions(codes, row_start_index, row_end_index, hit_index))
            if (check_placement(dict, codes, row_start_index, row_end_index,
                                Placement{transition.from, transition.to, hit_index, hit_end_index}))
                return true;
        return false;
    }

    // Whether codes[row_start_index..row_end_index) starts (Anchor::Start) or ends (Anchor::End) with the
    // needle, for a graph built with that anchor: a prefix walks forward from the source at the row's first
    // code, a suffix backward from the sink at its last.
    template <Anchor A>
    bool check_anchored(const Dictionary& dict, const uint8_t* codes, const size_t row_start_index,
                        const size_t row_end_index) const {
        if constexpr (A == Anchor::End) {
            return backward(sink_, row_end_index, codes, row_start_index, dict);
        } else {
            return forward(0, row_start_index, codes, row_end_index);
        }
    }

  private:
    // A cut through an occurrence at a hit: needle[..from] lies before
    // before_code_index, needle[to..] from after_code_index on, and the hit's
    // step spans the two.
    struct Placement {
        uint32_t from;
        uint32_t to;
        size_t before_code_index;
        size_t after_code_index;
    };

    // Whether the needle extends from the placement back to the source and on
    // to the sink inside codes[row_start_index..row_end_index).
    bool check_placement(const Dictionary& dict, const uint8_t* codes, const size_t row_start_index,
                         const size_t row_end_index, const Placement& placement) const {
        return forward(placement.to, placement.after_code_index, codes, row_end_index) &&
               backward(placement.from, placement.before_code_index, codes, row_start_index, dict);
    }

    // A node has one step, the greedy symbol or else an escape, and may have a
    // terminal range of the symbols holding the whole tail. Their codes are
    // disjoint, so forward never has to choose.
    struct Node {
        bool has_step = false;
        bool escape = false;  // the step is the marker with literal `step`
        uint8_t step = 0;     // Point: the code; Escape: the literal byte
        uint32_t next = 0;
        CodeRange terminal = CodeRange::none();
    };

    // A graph edge as the walk keeps it: the needle offsets one key moves
    // the parse between, grouped by key in transitions_.
    struct CodeTransition {
        uint32_t from;
        uint32_t to;
    };

    struct CodeTransitions {
        const CodeTransition* first = nullptr;
        const CodeTransition* last = nullptr;

        const CodeTransition* begin() const { return first; }
        const CodeTransition* end() const { return last; }
    };

    static constexpr size_t KEY_COUNT = 511;

    static uint16_t escape_key(const uint8_t literal) { return static_cast<uint16_t>(ESCAPE + literal); }

    // Keys 0..254 are the symbol codes, 255 + byte an escape with that literal.
    CodeTransitions transitions_of(const uint16_t key) const {
        return {transitions_.data() + head_[key], transitions_.data() + head_[key + 1]};
    }

    // The transitions the code at hit_index can be. None when it is the
    // literal of an escape, or a marker the row cuts off.
    CodeTransitions possible_transitions(const uint8_t* codes, const size_t row_start_index,
                                         const size_t row_end_index, const size_t hit_index) const {
        if (escapes_before(codes, row_start_index, hit_index) & 1) return {};
        const uint8_t code = codes[hit_index];
        if (code != ESCAPE) return transitions_of(code);
        if (hit_index + 1 >= row_end_index) return {};
        return transitions_of(escape_key(codes[hit_index + 1]));
    }

    // 255s in codes[row_start_index..code_index), counted back from code_index - 1.
    static size_t escapes_before(const uint8_t* codes, const size_t row_start_index, size_t code_index) {
        size_t run = 0;
        while (code_index > row_start_index && codes[code_index - 1] == ESCAPE) {
            ++run;
            --code_index;
        }
        return run;
    }

    // The row from code_index on follows the greedy steps from `needle_index` to the sink, or up to the first
    // code that differs, which matches when it is a symbol holding the whole rest of the needle. A step's code
    // is never in its own node's terminal range, so a code that equals it decides nothing yet.
    bool forward(const uint32_t needle_index, size_t code_index, const uint8_t* codes,
                 const size_t row_end_index) const {
        size_t node = needle_index;
        for (; code_index < row_end_index; ++code_index) {
            // past the sink every code matches: the whole needle lies before it
            if (node == sink_) return true;
            const Node& at = nodes_[node];
            const uint8_t code = codes[code_index];
            if (!at.has_step || code != (at.escape ? ESCAPE : at.step)) return at.terminal.contains(code);
            // an escape is the marker, then its literal, which no terminal holds
            if (at.escape && (++code_index == row_end_index || codes[code_index] != at.step)) return false;
            node = at.next;
        }
        // the row ended: a match only when the needle ended with it
        return node == sink_;
    }

    // Some path from the source reaches `needle_index` with the code ending at
    // code_index - 1 as its last step. At most one step of a code enters a node.
    bool backward(uint32_t needle_index, size_t code_index, const uint8_t* codes, const size_t row_start_index,
                  const Dictionary& dict) const {
        while (needle_index != 0)
            if (!step_backward(needle_index, code_index, codes, row_start_index, dict)) return false;
        return true;
    }

    // Takes the step into `needle_index` that ends at codes[code_index - 1].
    // That code is a literal iff the run of 255s before it is odd. An entry
    // symbol the planner did not enumerate is a step out of the source.
    bool step_backward(uint32_t& needle_index, size_t& code_index, const uint8_t* codes, const size_t row_start_index,
                       const Dictionary& dict) const {
        if (code_index <= row_start_index) return false;
        const uint8_t code = codes[code_index - 1];
        if (escapes_before(codes, row_start_index, code_index - 1) & 1) {
            const CodeTransition* transition = transition_into(escape_key(code), needle_index);
            if (transition == nullptr) return false;
            needle_index = transition->from;
            code_index -= 2;
            return true;
        }
        if (code == ESCAPE) return false;
        const CodeTransition* transition = transition_into(code, needle_index);
        if (transition != nullptr) {
            needle_index = transition->from;
        } else {
            if (!entry_matches(dict, code, needle_index)) return false;
            needle_index = 0;
        }
        code_index -= 1;
        return true;
    }

    // Whether symbol `code` ends with the needle's head up to `needle_index`,
    // for a node whose entry set was too big to enumerate.
    bool entry_matches(const Dictionary& dict, const uint8_t code, const uint32_t needle_index) const {
        const size_t entry_len = entry_len_[needle_index];
        const size_t symbol_len = dict.length(code);
        return entry_len > 0 && symbol_len >= entry_len &&
               same_prefix(dict.word(code) >> (8 * (symbol_len - entry_len)), head_word_, entry_len);
    }

    const CodeTransition* transition_into(const uint16_t key, const uint32_t needle_index) const {
        for (const CodeTransition& transition : transitions_of(key))
            if (transition.to == needle_index) return &transition;
        return nullptr;
    }

    std::vector<uint8_t> entry_len_;  // > 0 where the set into the node was too big
    std::vector<Node> nodes_;         // per needle offset, its greedy step and terminal range
    uint32_t sink_ = 0;
    std::vector<CodeTransition> transitions_;
    std::array<uint32_t, KEY_COUNT + 1> head_{};
    uint64_t head_word_ = 0;  // needle[..8] as a word, what an entry test compares
};

// The walk as stage two's check over one stream.
struct WalkCheck {
    const Walk& walk;
    const Dictionary& dict;
    const uint8_t* codes;

    bool passes(const size_t hit_index, const size_t row_start_index, const size_t row_end_index) const {
        return walk.check(dict, codes, row_start_index, row_end_index, hit_index);
    }
};

}  // namespace fsst::search::prefilter::scan
