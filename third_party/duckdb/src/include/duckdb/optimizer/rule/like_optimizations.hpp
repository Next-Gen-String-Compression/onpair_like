//===----------------------------------------------------------------------===//
//                         DuckDB
//
// duckdb/optimizer/rule/like_optimizations.hpp
//
//
//===----------------------------------------------------------------------===//

#pragma once

#include "duckdb/optimizer/rule.hpp"
#include "duckdb/function/scalar/string_functions.hpp"

namespace duckdb {

enum class PatternMatchType : uint8_t { LIKE, GLOB };

//! A pattern as the literals between its wildcards. One literal decides the match on its own: both anchors is
//! equality, one is prefix or suffix, none is contains. Several only rule rows out.
struct LikePatternSplit {
	vector<string> literals;
	bool anchor_start = false;
	bool anchor_end = false;

	bool IsExact() const {
		return literals.size() == 1;
	}
};

//! False for a pattern with a single character wildcard or an escape, or without a literal
bool TrySplitLikePattern(const string &pattern, PatternMatchType match_type, LikePatternSplit &result);

// The Like Optimization rule rewrites LIKE to optimized scalar functions (e.g.: prefix, suffix, and contains)
class LikeOptimizationRule : public Rule {
public:
	explicit LikeOptimizationRule(ExpressionRewriter &rewriter);

	unique_ptr<Expression> Apply(LogicalOperator &op, vector<reference<Expression>> &bindings, bool &changes_made,
	                             bool is_root) override;

	unique_ptr<Expression> ApplyRule(BoundFunctionExpression &expr, const ScalarFunction &function, string literal,
	                                 bool is_not_like) const;
};

} // namespace duckdb
