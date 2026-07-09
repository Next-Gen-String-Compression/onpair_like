#pragma once

#include <onpair/search/aho_corasick_trie.h>

#include <array>
#include <cstdint>
#include <span>
#include <string_view>
#include <vector>

/// Byte-level Aho-Corasick DFA for "contains-any" multi-pattern substring search.
struct ByteAhoCorasick {
    std::vector<std::array<uint16_t, 256>> delta;  ///< Full DFA goto table
    std::vector<uint8_t>                   accepting; ///< 1 if state is accepting
    bool                                   all_match = false; ///< empty pattern given

    static ByteAhoCorasick build(std::span<const std::string_view> patterns) {
        using State = onpair::search::AhoCorasickTrie::State;
        onpair::search::AhoCorasickTrie trie(patterns);

        ByteAhoCorasick ac;
        ac.all_match = trie.is_accepting(0);

        const size_t num_states = trie.num_states();
        ac.delta.resize(num_states);
        ac.accepting.resize(num_states, 0);

        // -- Build full 256-wide DFA via BFS (O(1) per cell) ----------------
        // Root: direct children from trie, missing transitions loop to root.
        ac.delta[0].fill(0);
        ac.accepting[0] = trie.is_accepting(0) ? 1u : 0u;
        {
            auto labels  = trie.edge_labels(0);
            auto targets = trie.edge_targets(0);
            for (size_t i = 0; i < labels.size(); ++i)
                ac.delta[0][labels[i]] = targets[i];
        }

        std::vector<State> fail(num_states, 0);
        std::vector<State> bfs;
        bfs.reserve(num_states);

        // Seed BFS with root's direct children.
        for (int c = 0; c < 256; ++c) {
            State s = ac.delta[0][c];
            if (s != 0) bfs.push_back(s);
        }

        for (size_t qi = 0; qi < bfs.size(); ++qi) {
            const State u = bfs[qi];

            // Propagate accepting through failure links.
            if (trie.is_accepting(fail[u]))
                ac.accepting[u] = 1;
            else
                ac.accepting[u] = trie.is_accepting(u) ? 1u : 0u;

            // Start with failure state's row (handles all missing transitions).
            ac.delta[u] = ac.delta[fail[u]];

            // Override with direct trie children.
            auto labels  = trie.edge_labels(u);
            auto targets = trie.edge_targets(u);
            for (size_t i = 0; i < labels.size(); ++i) {
                State v = targets[i];
                fail[v] = ac.delta[fail[u]][labels[i]];
                ac.delta[u][labels[i]] = v;
                bfs.push_back(v);
            }
        }

        return ac;
    }

    /// Returns true if `data[0..len)` contains any of the patterns.
    bool scan(const char* data, size_t len) const noexcept {
        if (all_match) return true;
        uint16_t state = 0;
        for (size_t i = 0; i < len; ++i) {
            state = delta[state][static_cast<uint8_t>(data[i])];
            if (accepting[state]) return true;
        }
        return false;
    }
};
