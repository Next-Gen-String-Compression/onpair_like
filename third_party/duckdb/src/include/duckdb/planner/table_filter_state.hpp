//===----------------------------------------------------------------------===//
//                         DuckDB
//
// duckdb/planner/table_filter_state.hpp
//
//
//===----------------------------------------------------------------------===//

#pragma once

#include "duckdb/planner/table_filter.hpp"
#include "duckdb/execution/expression_executor.hpp"
#include "duckdb/common/vector_size.hpp"

namespace duckdb {

struct SelectionVector;
class Vector;

struct ExpressionFilterExecutor {
	virtual ~ExpressionFilterExecutor() = default;

	//! Driver over one vector. Storage dictionaries are handled here once per dictionary: the
	//! filter runs over the unique entries and the per-entry verdicts are cached by dictionary id;
	//! rows then map through the dictionary selection. Everything else forwards to
	//! FilterSelectionInternal. Requires the verdict to be a deterministic function of the value.
	idx_t FilterSelection(SelectionVector &sel, Vector &vector, idx_t scan_count, idx_t &approved_tuple_count);

protected:
	virtual idx_t FilterSelectionInternal(SelectionVector &sel, Vector &vector, idx_t scan_count,
	                                      idx_t &approved_tuple_count) = 0;

private:
	void ComputeDictionaryVerdicts(Vector &dictionary_vector, idx_t dictionary_size);

private:
	//! Per-entry filter verdicts for the current storage dictionary
	string cached_dictionary_id;
	vector<uint8_t> dictionary_verdicts;
	SelectionVector dictionary_result_sel;
	idx_t dictionary_result_capacity = 0;
};

//! The expression executor decompresses what it reads, and it reads a slice per conjunct, so several predicates
//! on one column would decompress the survivors once each. Flattening the scan vector while every row is still
//! selected costs that same one pass and leaves the column flat for all of them.
void FlattenForExpressionExecutor(Vector &vector, const SelectionVector &sel, idx_t scan_count,
                                  idx_t approved_tuple_count);

//! Thread-local state for executing a table filter
struct TableFilterState {
public:
	virtual ~TableFilterState() = default;

public:
	static unique_ptr<TableFilterState> Initialize(ClientContext &context, const TableFilter &filter);

public:
	template <class TARGET>
	TARGET &Cast() {
		DynamicCastCheck<TARGET>(this);
		return reinterpret_cast<TARGET &>(*this);
	}
	template <class TARGET>
	const TARGET &Cast() const {
		DynamicCastCheck<TARGET>(this);
		return reinterpret_cast<const TARGET &>(*this);
	}
};

struct ExpressionFilterState : public TableFilterState {
public:
	ExpressionFilterState(ClientContext &context, const Expression &expression);
	~ExpressionFilterState() override;

	ClientContext &GetContext() {
		D_ASSERT(executor);
		return executor->GetContext();
	}

	unique_ptr<ExpressionExecutor> executor;
	unique_ptr<ExpressionFilterExecutor> fast_executor;
};

} // namespace duckdb
