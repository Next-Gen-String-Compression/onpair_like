#include "duckdb/optimizer/rule/like_optimizations.hpp"

#include "duckdb/execution/expression_executor.hpp"
#include "duckdb/function/scalar/string_functions.hpp"
#include "duckdb/function/scalar/string_common.hpp"
#include "duckdb/planner/expression/bound_function_expression.hpp"
#include "duckdb/planner/expression/bound_constant_expression.hpp"
#include "duckdb/planner/expression/bound_operator_expression.hpp"
#include "duckdb/planner/expression/bound_comparison_expression.hpp"

namespace duckdb {

LikeOptimizationRule::LikeOptimizationRule(ExpressionRewriter &rewriter) : Rule(rewriter) {
	// match on a FunctionExpression that has a foldable ConstantExpression
	auto func = make_uniq<FunctionExpressionMatcher>();
	func->matchers.push_back(make_uniq<ExpressionMatcher>());
	func->matchers.push_back(make_uniq<ConstantExpressionMatcher>());
	func->policy = SetMatcher::Policy::ORDERED;
	// we match on LIKE ("~~"), NOT LIKE ("!~~"), GLOB ("~~~"), and NOT GLOB ("!~~~")
	func->function = make_uniq<ManyFunctionMatcher>(
	    identifier_set_t {Identifier("!~~"), Identifier("~~"), Identifier("!~~~"), Identifier("~~~")});
	root = std::move(func);
}

bool TrySplitLikePattern(const string &pattern, PatternMatchType match_type, LikePatternSplit &result) {
	const bool glob = match_type == PatternMatchType::GLOB;
	const char wildcard_any = glob ? '*' : '%';
	result.literals.clear();
	string run;
	for (auto c : pattern) {
		if (c == wildcard_any) {
			if (!run.empty()) {
				result.literals.push_back(std::move(run));
				run.clear();
			}
			continue;
		}
		if (glob ? (c == '?' || c == '[' || c == '\\') : c == '_') {
			return false;
		}
		run += c;
	}
	if (!run.empty()) {
		result.literals.push_back(std::move(run));
	}
	if (result.literals.empty()) {
		// only wildcards, or the empty pattern, which matches only the empty string
		if (!pattern.empty()) {
			return false;
		}
		result.literals.emplace_back();
	}
	result.anchor_start = pattern.empty() || pattern.front() != wildcard_any;
	result.anchor_end = pattern.empty() || pattern.back() != wildcard_any;
	return true;
}

unique_ptr<Expression> LikeOptimizationRule::Apply(LogicalOperator &op, vector<reference<Expression>> &bindings,
                                                   bool &changes_made, bool is_root) {
	auto &root = bindings[0].get().Cast<BoundFunctionExpression>();
	auto &constant_expr = bindings[2].get().Cast<BoundConstantExpression>();
	D_ASSERT(root.GetChildren().size() == 2);

	if (constant_expr.GetValue().IsNull()) {
		return make_uniq<BoundConstantExpression>(Value(root.GetReturnType()));
	}

	// the constant_expr is a scalar expression that we have to fold
	if (!constant_expr.IsFoldable()) {
		return nullptr;
	}

	auto constant_value = ExpressionExecutor::EvaluateScalar(GetContext(), constant_expr);
	D_ASSERT(constant_value.type() == constant_expr.GetReturnType());
	auto &patt_str = StringValue::Get(constant_value);

	bool is_not_like = root.Function().GetName() == "!~~" || root.Function().GetName() == "!~~~";
	PatternMatchType match_type = (root.Function().GetName() == "~~~" || root.Function().GetName() == "!~~~")
	                                  ? PatternMatchType::GLOB
	                                  : PatternMatchType::LIKE;
	LikePatternSplit split;
	if (!TrySplitLikePattern(patt_str, match_type, split) || !split.IsExact()) {
		return nullptr;
	}
	if (split.anchor_start && split.anchor_end) {
		return BoundComparisonExpression::Create(
		    is_not_like ? ExpressionType::COMPARE_NOTEQUAL : ExpressionType::COMPARE_EQUAL,
		    std::move(root.GetChildrenMutable()[0]), std::move(root.GetChildrenMutable()[1]));
	}
	if (split.anchor_start) {
		return ApplyRule(root, PrefixFun::GetFunction(), std::move(split.literals[0]), is_not_like);
	}
	if (split.anchor_end) {
		return ApplyRule(root, SuffixFun::GetFunction(), std::move(split.literals[0]), is_not_like);
	}
	return ApplyRule(root, GetStringContains(), std::move(split.literals[0]), is_not_like);
}

unique_ptr<Expression> LikeOptimizationRule::ApplyRule(BoundFunctionExpression &expr, const ScalarFunction &function,
                                                       string literal, bool is_not_like) const {
	// replace LIKE by an optimized function
	auto result = function.Bind(GetContext(), std::move(expr.GetChildrenMutable()));
	result->GetChildrenMutable()[1] = make_uniq<BoundConstantExpression>(Value(std::move(literal)));

	if (!is_not_like) {
		return std::move(result);
	}

	auto negation = make_uniq<BoundOperatorExpression>(ExpressionType::OPERATOR_NOT, LogicalType::BOOLEAN);
	negation->GetChildrenMutable().push_back(std::move(result));
	return std::move(negation);
}

} // namespace duckdb
