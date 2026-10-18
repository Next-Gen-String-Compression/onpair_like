#include "duckdb/planner/filter/fsst_contains.hpp"

#include "duckdb/common/numeric_utils.hpp"
#include "duckdb/common/vector/flat_vector.hpp"
#include "duckdb/common/vector/fsst_vector.hpp"
#include "duckdb/main/settings.hpp"
#include "fsst.h"

// The kernels of search/prefilter need NEON, AVX-512BW or AVX2 and refuse to compile without one.
// A build below that baseline evaluates contains on decompressed strings rather than failing to build;
// on x86 that means configuring with -march=native (or -mavx2) to get the compressed scan at all.
#if defined(__ARM_NEON) || defined(__AVX512BW__) || defined(__AVX2__)
#define DUCKDB_FSST_SEARCH_AVAILABLE
#include "prefilter/prefilter.hpp"
#endif

namespace duckdb {

#ifdef DUCKDB_FSST_SEARCH_AVAILABLE

namespace search = fsst::search::prefilter;

namespace {

class ContainsFilterExecutor final : public ExpressionFilterExecutor {
public:
	ContainsFilterExecutor(ClientContext &context, FSSTPattern pattern_p, unique_ptr<ExpressionFilterExecutor> exact_p)
	    : pattern(std::move(pattern_p)), enabled(Settings::Get<EnableFSSTContainsSetting>(context)),
	      exact(std::move(exact_p)) {
		D_ASSERT(!(pattern.anchor_start && pattern.anchor_end));
		if (pattern.anchor_start) {
			anchor = search::Anchor::Start;
		} else if (pattern.anchor_end) {
			anchor = search::Anchor::End;
		}
	}

	idx_t FilterSelectionInternal(SelectionVector &sel, Vector &vector, idx_t scan_count,
	                              idx_t &approved_tuple_count) override {
		if (approved_tuple_count == 0) {
			return 0;
		}
		if ((enabled && FilterFSST(sel, vector, approved_tuple_count)) || !exact) {
			return approved_tuple_count;
		}
		return exact->FilterSelection(sel, vector, scan_count, approved_tuple_count);
	}

private:
	bool FilterFSST(SelectionVector &sel, Vector &vector, idx_t &count) {
		if (vector.GetVectorType() != VectorType::FSST_VECTOR) {
			return false;
		}
		const idx_t row_count = FSSTVector::GetFSSTBuffer(vector).Capacity();
		auto &input_sel = sel.IsSet() ? sel : *FlatVector::IncrementalSelectionVector();
		if (row_count == 0 || !Prepare(vector, row_count) ||
		    !MarkMatches(FSSTVector::GetDataView(vector), input_sel, count, row_count)) {
			return false;
		}
		// A NULL row is stored as an empty string, which a negated pattern would keep, so validity decides too.
		// The incoming selection is ascending, since every filter compacts in order, so result_sel is too.
		auto &validity = FSSTVector::Validity(vector);
		if (result_capacity < count) {
			result_sel.Initialize(count);
			result_capacity = count;
		}
		const uint8_t negated = pattern.negated;
		idx_t result_count = 0;
		for (idx_t i = 0; i < count; i++) {
			const idx_t row = input_sel.get_index(i);
			result_sel.set_index(result_count, row);
			result_count += (matched[row] ^ negated) & uint8_t(validity.RowIsValid(row));
		}
		sel.Initialize(result_sel);
		count = result_count;
		return true;
	}

	//! Plan the literal over the vector's symbol table, weighted by the code counts stored with it. Every segment
	//! of a row group shares the table, so the plan is kept per row group; 0 means unknown and plans every time.
	bool Prepare(const Vector &vector, idx_t row_count) {
		const idx_t row_group_id = FSSTVector::GetRowGroupId(vector);
		if (row_group_id != 0 && row_group_id == planned_row_group) {
			return usable;
		}
		planned_row_group = row_group_id;
		usable = false;
		const auto decoder = static_cast<const fsst_decoder_t *>(FSSTVector::GetDecoder(vector));
		const idx_t symbol_count = FSSTVector::GetSymbolCount(vector);
		if (!decoder || symbol_count == 0) {
			return false;
		}
		dict = search::Dictionary::from(*decoder, symbol_count);
		freq = search::Frequency::of_counts(decoder->count);
		// the planner needs codes in byte order of their symbols, and the counts; segments written before
		// fsst_sort_codes or without counts have neither
		if (!dict.sorted() || freq.total == 0) {
			return false;
		}
		auto &literal = pattern.literal;
		analysis = search::analyze(const_data_ptr_cast(literal.data()), literal.size(), dict, freq, row_count, anchor);
		// every row, or a literal this symbol table cannot spell: left to the decompressing path
		usable = !analysis.matches_all && (anchor != search::Anchor::None || !analysis.cover.empty());
		return usable;
	}

	//! Sets matched[row] for the rows of sel that hold the literal. False when an unanchored plan gets a gathered
	//! slice rather than one scanned block; an anchored plan reads each row on its own and takes any layout.
	bool MarkMatches(const var_binary_view_t &view, const SelectionVector &sel, idx_t count, idx_t row_count) {
		const auto codes = const_data_ptr_cast(view.base);
		matched.assign(row_count, 0);
		if (anchor == search::Anchor::Start) {
			MarkAnchored<search::Anchor::Start>(view, sel, count);
			return true;
		}
		if (anchor == search::Anchor::End) {
			MarkAnchored<search::Anchor::End>(view, sel, count);
			return true;
		}
		// The block holds the rows back to front, so the offsets read backwards are the ascending row offsets the
		// scan wants; stream row r is vector row row_count-1-r.
		const idx_t stream_size = UnsafeNumericCast<idx_t>(view.offsets[0]) + view.lengths[0];
		stream_offsets.resize(row_count + 1);
		for (idx_t row = 0; row < row_count; row++) {
			const idx_t source = row_count - 1 - row;
			const idx_t offset = UnsafeNumericCast<idx_t>(view.offsets[source]);
			if (offset + view.lengths[source] !=
			    (source == 0 ? stream_size : UnsafeNumericCast<idx_t>(view.offsets[source - 1]))) {
				return false;
			}
			stream_offsets[row] = UnsafeNumericCast<uint32_t>(offset);
		}
		stream_offsets[row_count] = UnsafeNumericCast<uint32_t>(stream_size);
		const search::scan::Stream stream {codes, stream_size, stream_offsets.data(), row_count + 1};
		found.clear();
		if (count < row_count) {
			stream_sel.resize(count);
			for (idx_t i = 0; i < count; i++) {
				stream_sel[count - 1 - i] = UnsafeNumericCast<uint32_t>(row_count - 1 - sel.get_index(i));
			}
			finder.find(stream, dict, analysis, stream_sel.data(), count, found);
		} else {
			search::candidate_rows(stream, dict, analysis, found);
		}
		for (auto stream_row : found) {
			matched[row_count - 1 - stream_row] = 1;
		}
		return true;
	}

	//! Each selected row walked on its own from its first code (Start) or back from its last (End).
	template <search::Anchor A>
	void MarkAnchored(const var_binary_view_t &view, const SelectionVector &sel, idx_t count) {
		const auto codes = const_data_ptr_cast(view.base);
		for (idx_t i = 0; i < count; i++) {
			const idx_t row = sel.get_index(i);
			const idx_t start = UnsafeNumericCast<idx_t>(view.offsets[row]);
			matched[row] = analysis.walk.check_anchored<A>(dict, codes, start, start + view.lengths[row]);
		}
	}

	FSSTPattern pattern;
	search::Anchor anchor = search::Anchor::None;
	bool enabled;
	unique_ptr<ExpressionFilterExecutor> exact;

	idx_t planned_row_group = 0;
	bool usable = false;
	search::Dictionary dict;
	search::Frequency freq;
	search::Analysis analysis;

	unsafe_vector<uint8_t> matched;
	unsafe_vector<uint32_t> stream_offsets;
	std::vector<uint32_t> stream_sel;
	std::vector<size_t> found;
	search::Finder finder;
	SelectionVector result_sel;
	idx_t result_capacity = 0;
};

} // namespace

unique_ptr<ExpressionFilterExecutor> CreateContainsExecutor(ClientContext &context, FSSTPattern pattern,
                                                            unique_ptr<ExpressionFilterExecutor> exact) {
	return make_uniq<ContainsFilterExecutor>(context, std::move(pattern), std::move(exact));
}

#else

unique_ptr<ExpressionFilterExecutor> CreateContainsExecutor(ClientContext &, FSSTPattern,
                                                            unique_ptr<ExpressionFilterExecutor> exact) {
	return exact;
}

#endif

} // namespace duckdb
