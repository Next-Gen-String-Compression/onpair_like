//===----------------------------------------------------------------------===//
//                         DuckDB
//
// duckdb/common/samply_marker.hpp
//
//
//===----------------------------------------------------------------------===//

#pragma once

#include "duckdb/common/common.hpp"

namespace duckdb {

//! A span in samply's marker file (/tmp/marker-<pid>.txt), shown in the Firefox Profiler timeline.
//! Written only when the process runs with DUCKDB_SAMPLY_MARKERS set; a no-op otherwise.
class SamplyMarkerSpan {
public:
	explicit SamplyMarkerSpan(string name);
	~SamplyMarkerSpan();

private:
	string name;
	uint64_t start_ns;
};

} // namespace duckdb
