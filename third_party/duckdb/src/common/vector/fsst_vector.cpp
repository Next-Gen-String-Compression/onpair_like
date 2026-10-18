#include "duckdb/common/vector/fsst_vector.hpp"
#include "duckdb/common/vector/flat_vector.hpp"
#include "duckdb/common/vector_operations/vector_operations.hpp"
#include "duckdb/common/fsst.hpp"
#include "duckdb/common/printer.hpp"

namespace duckdb {

VectorFSSTStringBuffer::VectorFSSTStringBuffer(capacity_t capacity, idx_t auxiliary_size)
    : VariableBinaryBuffer(capacity, auxiliary_size) {
	buffer_type = VectorBufferType::FSST_BUFFER;
	vector_type = VectorType::FSST_VECTOR;
}

VectorFSSTStringBuffer::VectorFSSTStringBuffer(const VectorFSSTStringBuffer &source, count_t count)
    : VariableBinaryBuffer(source, count) {
	buffer_type = VectorBufferType::FSST_BUFFER;
	vector_type = VectorType::FSST_VECTOR;
	duckdb_fsst_decoder = source.duckdb_fsst_decoder;
	fsst_encoder = source.fsst_encoder;
	symbol_count = source.symbol_count;
	row_group_id = source.row_group_id;
	decompress_buffer.resize(source.decompress_buffer.size());
}

VectorFSSTStringBuffer::VectorFSSTStringBuffer(const VectorFSSTStringBuffer &source, count_t count, idx_t offset)
    : VariableBinaryBuffer(source, count, offset) {
	buffer_type = VectorBufferType::FSST_BUFFER;
	vector_type = VectorType::FSST_VECTOR;
	duckdb_fsst_decoder = source.duckdb_fsst_decoder;
	fsst_encoder = source.fsst_encoder;
	symbol_count = source.symbol_count;
	row_group_id = source.row_group_id;
	decompress_buffer.resize(source.decompress_buffer.size());
}

VectorFSSTStringBuffer::~VectorFSSTStringBuffer() {
}

FSSTEncoder &VectorFSSTStringBuffer::GetEncoder() const {
	if (!fsst_encoder) {
		throw InternalException("FSST vector has no encoder");
	}
	return *fsst_encoder;
}

void VectorFSSTStringBuffer::SetVectorType(VectorType new_vector_type) {
	throw InternalException("SetVectorType not supported for FSST vector");
}

void VectorFSSTStringBuffer::VerifyInternal(const LogicalType &type, const SelectionVector &sel, idx_t count) const {
	D_ASSERT(type.InternalType() == PhysicalType::VARCHAR);
	D_ASSERT(vector_type == VectorType::FSST_VECTOR);
}

Value VectorFSSTStringBuffer::GetValue(const LogicalType &type, idx_t index) const {
	if (!GetValidityMask().RowIsValid(index)) {
		return Value(type);
	}
	auto str_compressed = GetString(index);
	auto decoder = GetDecoder();
	auto string_val =
	    FSSTPrimitives::DecompressValue(decoder, str_compressed.GetData(), str_compressed.GetSize(), decompress_buffer);
	switch (type.id()) {
	case LogicalTypeId::VARCHAR:
		return Value(std::move(string_val));
	case LogicalTypeId::BLOB:
		return Value::BLOB_RAW(string_val);
	default:
		throw InternalException("Unsupported type for FSST vector GetValue");
	}
}

template <bool SEL_IS_IDENTITY, bool SRC_HAS_INVALIDS>
buffer_ptr<VectorBuffer> VectorFSSTStringBuffer::FlattenSliceTemplated(const SelectionVector &sel, idx_t count) const {
	auto result = make_buffer<VectorStringBuffer>(capacity_t(count));

	auto result_data = reinterpret_cast<string_t *>(result->GetData());
	auto &str_allocator = result->GetStringAllocator();
	const auto decoder = GetDecoder();
	auto &src_mask = GetValidityMask();
	auto &dst_mask = result->GetValidityMask();
	for (idx_t idx = 0; idx < count; idx++) {
		const idx_t sel_idx = SEL_IS_IDENTITY ? idx : sel.get_index_unsafe(idx);
		if (SRC_HAS_INVALIDS && !src_mask.RowIsValid(sel_idx)) {
			// NULL value
			dst_mask.SetInvalid(idx);
			continue;
		}
		auto compressed_string = GetVarBinary(sel_idx);
		if (compressed_string.length > 0) {
			result_data[idx] = FSSTPrimitives::DecompressValue(decoder, str_allocator, compressed_string.ptr,
			                                                   compressed_string.length);
		} else {
			// empty string
			result_data[idx] = string_t(nullptr, 0);
		}
	}
	result->SetVectorSize(count);
	return result;
}

buffer_ptr<VectorBuffer> VectorFSSTStringBuffer::FlattenSliceInternal(const LogicalType &type,
                                                                      const SelectionVector &sel, idx_t count) const {
	const bool sel_is_identity = !sel.IsSet();
	const bool src_has_invalids = GetValidityMask().CanHaveNull();
	if (sel_is_identity) {
		if (src_has_invalids) {
			return FlattenSliceTemplated<true, true>(sel, count);
		}
		return FlattenSliceTemplated<true, false>(sel, count);
	}
	if (src_has_invalids) {
		return FlattenSliceTemplated<false, true>(sel, count);
	}
	return FlattenSliceTemplated<false, false>(sel, count);
}

buffer_ptr<VectorBuffer> VectorFSSTStringBuffer::SliceInternal(const LogicalType &type, idx_t offset, idx_t end) {
	D_ASSERT(end <= Size());
	if (offset == 0 && end == Size()) {
		// full-range slice: keep the current buffer
		return nullptr;
	}
	// zero-copy range view: shares the byte buffer and the shifted offsets/lengths sub-arrays
	auto count = end - offset;
	auto result = make_buffer<VectorFSSTStringBuffer>(*this, count_t(count), offset);
	result->GetValidityMask().Slice(GetValidityMask(), offset, count);
	result->AddAuxiliaryData(make_uniq<VectorBufferHolder>(shared_from_this()));
	return result;
}

buffer_ptr<VectorBuffer> VectorFSSTStringBuffer::SliceInternal(const LogicalType &type, const SelectionVector &sel,
                                                               idx_t count) {
	if (!sel.IsSet()) {
		// incremental selection: slice the range [0, count) directly
		return SliceInternal(type, idx_t(0), count);
	}

	// slice view: share the byte buffer, gather the selected offsets/lengths into the fresh arrays
	auto result = make_buffer<VectorFSSTStringBuffer>(*this, count_t(count));
	auto new_offsets = result->GetOffsets();
	auto new_lengths = result->GetLengths();
	const auto old_offsets = GetOffsets();
	const auto old_lengths = GetLengths();
	for (idx_t i = 0; i < count; i++) {
		const auto source_idx = sel.get_index(i);
		new_offsets[i] = old_offsets[source_idx];
		new_lengths[i] = old_lengths[source_idx];
	}
	// only remap validity when the source has nulls, and only write bits for the null rows: an all-valid
	// selection (e.g. survivors of an equality/IN filter, which never match NULL) leaves the mask unset
	auto &src_mask = GetValidityMask();
	if (src_mask.CanHaveNull()) {
		auto &dst_mask = result->GetValidityMask();
		for (idx_t i = 0; i < count; i++) {
			if (!src_mask.RowIsValid(sel.get_index(i))) {
				dst_mask.SetInvalid(i);
			}
		}
	}
	result->AddAuxiliaryData(make_uniq<VectorBufferHolder>(shared_from_this()));
	return result;
}

VectorFSSTStringBuffer &FSSTVector::GetFSSTBuffer(const Vector &vector) {
	D_ASSERT(vector.GetType().InternalType() == PhysicalType::VARCHAR);
	if (vector.GetVectorType() != VectorType::FSST_VECTOR) {
		throw InternalException("FSSTVector::GetFSSTBuffer called on a non-FSST vector");
	}
	if (!vector.GetBufferRef() || vector.Buffer().GetBufferType() != VectorBufferType::FSST_BUFFER) {
		throw InternalException("FSSTVector has a non-FSST buffer");
	}
	return vector.GetBufferRef()->Cast<VectorFSSTStringBuffer>();
}

void *FSSTVector::GetDecoder(const Vector &vector) {
	auto &fsst_string_buffer = GetFSSTBuffer(vector);
	return fsst_string_buffer.GetDecoder();
}

idx_t FSSTVector::GetSymbolCount(const Vector &vector) {
	auto &fsst_string_buffer = GetFSSTBuffer(vector);
	return fsst_string_buffer.GetSymbolCount();
}

idx_t FSSTVector::GetRowGroupId(const Vector &vector) {
	auto &fsst_string_buffer = GetFSSTBuffer(vector);
	return fsst_string_buffer.GetRowGroupId();
}

vector<unsigned char> &FSSTVector::GetDecompressBuffer(const Vector &vector) {
	auto &fsst_string_buffer = GetFSSTBuffer(vector);
	return fsst_string_buffer.GetDecompressBuffer();
}

string FSSTVector::SymbolTableToString(const Vector &vector) {
	return FSSTPrimitives::DecoderToString(GetDecoder(vector));
}

void FSSTVector::PrintSymbolTable(const Vector &vector) {
	printf("%s", SymbolTableToString(vector).c_str());
}

var_binary_t FSSTVector::GetCompressedString(const Vector &vector, idx_t index) {
	return GetFSSTBuffer(vector).GetVarBinary(index);
}

var_binary_view_t FSSTVector::GetDataView(const Vector &vector) {
	return GetFSSTBuffer(vector).GetView();
}

string FSSTVector::CompressValue(const Vector &vector, const char *input, idx_t input_len) {
	auto &fsst_string_buffer = GetFSSTBuffer(vector);
	return fsst_string_buffer.GetEncoder().Compress(input, input_len);
}

void FSSTVector::Create(Vector &vector, buffer_ptr<void> &duckdb_fsst_decoder, shared_ptr<FSSTEncoder> encoder,
                        const idx_t string_block_limit, idx_t capacity, idx_t auxiliary_size, idx_t symbol_count,
                        idx_t row_group_id) {
	vector.SetBuffer(make_buffer<VectorFSSTStringBuffer>(capacity_t(capacity), auxiliary_size));
	auto &fsst_string_buffer = vector.BufferMutable().Cast<VectorFSSTStringBuffer>();
	fsst_string_buffer.AddDecoder(duckdb_fsst_decoder, std::move(encoder), string_block_limit, symbol_count,
	                              row_group_id);
}

void FSSTVector::Grow(Vector &vector, idx_t added_count, idx_t added_bytes) {
	GetFSSTBuffer(vector).Grow(added_count, added_bytes);
}

} // namespace duckdb
