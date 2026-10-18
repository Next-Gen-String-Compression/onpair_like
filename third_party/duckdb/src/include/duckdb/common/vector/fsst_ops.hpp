//===----------------------------------------------------------------------===//
//                         DuckDB
//
// duckdb/common/vector/fsst_ops.hpp
//
//
//===----------------------------------------------------------------------===//

#pragma once

#include "duckdb/common/types/vector.hpp"

namespace duckdb {

//! Comparison operations evaluated directly on FSST-compressed data, without decompressing.
//! TryEquals/TryNotEquals have the contract of VectorOperations::Equals: the vectors are already sliced to the
//! selected rows and sel only names the output rows. SelectEquals is the table filter shape over the unsliced
//! scan vector.
struct FSSTOps {
	//! Try to evaluate an equality select between an FSST vector and a non-NULL constant VARCHAR by
	//! compressing the constant and comparing in the compressed domain.
	//! Returns false if the input shape does not match; result is only set when returning true.
	DUCKDB_API static bool TryEquals(const Vector &left, const Vector &right, optional_ptr<const SelectionVector> sel,
	                                 idx_t count, optional_ptr<SelectionVector> true_sel,
	                                 optional_ptr<SelectionVector> false_sel, optional_ptr<ValidityMask> null_mask,
	                                 idx_t &result);
	//! TryEquals for not-equals.
	DUCKDB_API static bool TryNotEquals(const Vector &left, const Vector &right,
	                                    optional_ptr<const SelectionVector> sel, idx_t count,
	                                    optional_ptr<SelectionVector> true_sel, optional_ptr<SelectionVector> false_sel,
	                                    optional_ptr<ValidityMask> null_mask, idx_t &result);
	//! The rows of sel (all rows when null) of an FSST vector that hold the needle, or do not hold it for
	//! equality = false, written to result_sel in order. NULL rows never qualify. Returns their count.
	DUCKDB_API static idx_t SelectEquals(const Vector &fsst_vec, const string &needle, bool equality,
	                                     optional_ptr<const SelectionVector> sel, idx_t count,
	                                     SelectionVector &result_sel);
};

} // namespace duckdb
