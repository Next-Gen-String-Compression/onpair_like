//===----------------------------------------------------------------------===//
//                         DuckDB
//
// duckdb/planner/filter/fsst_contains.hpp
//
//
//===----------------------------------------------------------------------===//

#pragma once

#include "duckdb/common/string.hpp"
#include "duckdb/common/unique_ptr.hpp"
#include "duckdb/planner/table_filter_state.hpp"

namespace duckdb {

class ClientContext;

//! One literal of a LIKE pattern and its anchors: contains is unanchored, prefix anchored at the start, suffix
//! at the end. Both anchors would be equality, which the comparison filter takes.
struct FSSTPattern {
	string literal;
	bool anchor_start = false;
	bool anchor_end = false;
	//! NOT LIKE / NOT contains(...)
	bool negated = false;
};

//! A filter for a literal that answers FSST vectors on their codes, with the substring search of
//! third_party/fsst/upstream/search. Other vectors, and those its planner declines, go to `exact`; without one
//! it keeps them, as a prefilter in front of a pattern checked after it. A build without the search returns
//! `exact` itself.
unique_ptr<ExpressionFilterExecutor> CreateContainsExecutor(ClientContext &context, FSSTPattern pattern,
                                                            unique_ptr<ExpressionFilterExecutor> exact);

} // namespace duckdb
