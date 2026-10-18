#pragma once

// What the planner needs to know about nibble_n8 without the kernel: tokens
// per batch and the batches the u8 planner admits.

#include <cstddef>

namespace fsst::search::prefilter::scan::matcher {

constexpr size_t PER_BATCH = 8;
constexpr size_t MAX_BATCHES = 1;

}  // namespace fsst::search::prefilter::scan::matcher
