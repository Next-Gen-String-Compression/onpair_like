#pragma once

// Minimum-weight cut of the alignment graph by Dinic max flow over a CSR
// residual graph, built per cut. See docs/prefilter.md section 5.

#include <algorithm>
#include <cassert>
#include <cstdint>
#include <cstdlib>
#include <vector>

#include "graph.hpp"

namespace fsst::search::prefilter {

namespace detail {

class Dinic {
  public:
    // capacity(edge) for each edge; twins start empty.
    template <typename Capacity>
    Dinic(size_t num_nodes, const std::vector<Edge>& edges, Capacity capacity) {
        head_.assign(num_nodes + 1, 0);
        for (const Edge& e : edges) {
            ++head_[e.from + 1];
            ++head_[e.to + 1];
        }
        for (size_t v = 0; v < num_nodes; ++v) head_[v + 1] += head_[v];
        size_t slots = edges.size() * 2;
        std::vector<uint32_t> cursor(head_);
        to_.assign(slots, 0);
        twin_.assign(slots, 0);
        cap_.assign(slots, 0);
        for (size_t at = 0; at < edges.size(); ++at) {
            uint32_t fwd = cursor[edges[at].from]++;
            uint32_t rev = cursor[edges[at].to]++;
            to_[fwd] = edges[at].to;
            twin_[fwd] = rev;
            to_[rev] = edges[at].from;
            twin_[rev] = fwd;
            cap_[fwd] = capacity(edges[at]);
        }
        level_.assign(num_nodes, -1);
        next_.assign(num_nodes, 0);
    }

    uint64_t max_flow(size_t source, size_t sink) {
        uint64_t total = 0;
        while (build_levels(source, sink))
            if (__builtin_add_overflow(total, blocking_flow(source, sink), &total)) std::abort();
        return total;
    }

    // After max_flow: the source side of the residual graph is level >= 0.
    int32_t level(size_t node) const { return level_[node]; }

  private:
    bool build_levels(size_t source, size_t sink) {
        std::fill(level_.begin(), level_.end(), -1);
        level_[source] = 0;
        queue_.clear();
        queue_.push_back(static_cast<uint32_t>(source));
        for (size_t head = 0; head < queue_.size(); ++head) {
            size_t v = queue_[head];
            for (uint32_t arc = head_[v]; arc < head_[v + 1]; ++arc) {
                size_t to = to_[arc];
                if (cap_[arc] > 0 && level_[to] < 0) {
                    level_[to] = level_[v] + 1;
                    queue_.push_back(static_cast<uint32_t>(to));
                }
            }
        }
        return level_[sink] >= 0;
    }

    // Iterative: the path lives in path_, not on the call stack.
    uint64_t blocking_flow(size_t source, size_t sink) {
        assert(source != sink);
        std::copy(head_.begin(), head_.begin() + static_cast<std::ptrdiff_t>(level_.size()), next_.begin());
        uint64_t total = 0;
        path_.clear();
        size_t node = source;
        for (;;) {
            if (node == sink) {
                uint64_t bottleneck = UINT64_MAX;
                for (uint32_t arc : path_) bottleneck = std::min(bottleneck, cap_[arc]);
                for (uint32_t arc : path_) {
                    cap_[arc] -= bottleneck;
                    cap_[twin_[arc]] += bottleneck;
                }
                total += bottleneck;
                size_t saturated = 0;
                while (cap_[path_[saturated]] != 0) ++saturated;
                node = to_[twin_[path_[saturated]]];
                path_.resize(saturated);
                continue;
            }
            uint32_t end = head_[node + 1];
            while (next_[node] < end) {
                uint32_t arc = next_[node];
                if (cap_[arc] > 0 && level_[to_[arc]] == level_[node] + 1) break;
                ++next_[node];
            }
            if (next_[node] < end) {
                uint32_t arc = next_[node];
                path_.push_back(arc);
                node = to_[arc];
            } else if (!path_.empty()) {
                uint32_t arc = path_.back();
                path_.pop_back();
                level_[node] = -1;
                node = to_[twin_[arc]];
            } else {
                return total;
            }
        }
    }

    std::vector<uint32_t> head_, to_, twin_, next_, path_;
    std::vector<uint64_t> cap_;
    std::vector<int32_t> level_;
    std::vector<uint32_t> queue_;  // the BFS queue, read from the front by index
};

}  // namespace detail

// The cheapest set of cuttable edges disconnecting source from sink, as
// ascending indices into graph.edges. `weight(edge)` is read for cuttable
// edges only; the others get one more than every finite cut. Aborts, in every
// build, if the weights overflow or a source-to-sink path runs entirely
// through uncuttable edges: the cut would then not block it, an unsound cover.
template <typename Weight>
std::vector<uint32_t> min_cut(const CutGraph& graph, Weight weight) {
    uint64_t finite = 0;
    for (const Edge& e : graph.edges)
        if (e.cuttable() && __builtin_add_overflow(finite, weight(e), &finite)) std::abort();
    const uint64_t infinite = finite + 1;
    detail::Dinic flow(graph.node_count, graph.edges,
                       [&](const Edge& e) { return e.cuttable() ? weight(e) : infinite; });
    if (flow.max_flow(graph.source, graph.sink) >= infinite) std::abort();
    std::vector<uint32_t> cut;
    for (size_t at = 0; at < graph.edges.size(); ++at) {
        const Edge& e = graph.edges[at];
        if (e.cuttable() && flow.level(e.from) >= 0 && flow.level(e.to) < 0) cut.push_back(static_cast<uint32_t>(at));
    }
    return cut;
}

}  // namespace fsst::search::prefilter
