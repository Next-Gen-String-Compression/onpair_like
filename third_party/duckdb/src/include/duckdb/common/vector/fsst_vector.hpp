//===----------------------------------------------------------------------===//
//                         DuckDB
//
// duckdb/common/vector/fsst_vector.hpp
//
//
//===----------------------------------------------------------------------===//

#pragma once

#include "duckdb/common/vector/string_vector.hpp"
#include "duckdb/common/vector/variable_binary_buffer.hpp"

namespace duckdb {

class FSSTEncoder;

class VectorFSSTStringBuffer : public VariableBinaryBuffer {
public:
	VectorFSSTStringBuffer(capacity_t capacity, idx_t auxiliary_size);
	//! Slice view sharing source's byte buffer, with fresh offsets/lengths arrays for the caller to gather into.
	//! The caller must keep source alive via auxiliary data.
	VectorFSSTStringBuffer(const VectorFSSTStringBuffer &source, count_t count);
	//! Range view sharing source's byte buffer and offsets/lengths (shifted by offset), fully zero-copy.
	//! The caller must keep source alive via auxiliary data.
	VectorFSSTStringBuffer(const VectorFSSTStringBuffer &source, count_t count, idx_t offset);
	~VectorFSSTStringBuffer() override;

public:
	void AddDecoder(buffer_ptr<void> &duckdb_fsst_decoder_p, shared_ptr<FSSTEncoder> encoder,
	                const idx_t string_block_limit, const idx_t symbol_count_p, const idx_t row_group_id_p) {
		duckdb_fsst_decoder = duckdb_fsst_decoder_p;
		fsst_encoder = std::move(encoder);
		decompress_buffer.resize(string_block_limit + 1);
		symbol_count = symbol_count_p;
		row_group_id = row_group_id_p;
	}
	void *GetDecoder() const {
		return duckdb_fsst_decoder.get();
	}
	//! Codes 0..symbol_count-1 name symbols; the rest are the filler fsst_import writes
	idx_t GetSymbolCount() const {
		return symbol_count;
	}
	//! The scan's number for the row group these strings came from, 0 when not from a table scan.
	//! All segments of one row group share a symbol table, so it identifies the table as well.
	idx_t GetRowGroupId() const {
		return row_group_id;
	}
	//! The encoder shared with all vectors created from the same decoder.
	FSSTEncoder &GetEncoder() const;
	vector<unsigned char> &GetDecompressBuffer() const {
		return decompress_buffer;
	}
	void SetVectorType(VectorType vector_type) override;

public:
	Value GetValue(const LogicalType &type, idx_t index) const override;

protected:
	buffer_ptr<VectorBuffer> FlattenSliceInternal(const LogicalType &type, const SelectionVector &sel,
	                                              idx_t count) const override;
	buffer_ptr<VectorBuffer> SliceInternal(const LogicalType &type, idx_t offset, idx_t end) override;
	buffer_ptr<VectorBuffer> SliceInternal(const LogicalType &type, const SelectionVector &sel, idx_t count) override;
	void VerifyInternal(const LogicalType &type, const SelectionVector &sel, idx_t count) const override;

private:
	template <bool SEL_IS_IDENTITY, bool SRC_HAS_INVALIDS>
	buffer_ptr<VectorBuffer> FlattenSliceTemplated(const SelectionVector &sel, idx_t count) const;

private:
	buffer_ptr<void> duckdb_fsst_decoder;
	idx_t symbol_count = 0;
	idx_t row_group_id = 0;
	mutable vector<unsigned char> decompress_buffer;
	shared_ptr<FSSTEncoder> fsst_encoder;
};

struct FSSTVector {
	static inline const ValidityMask &Validity(const Vector &vector) {
		D_ASSERT(vector.GetVectorType() == VectorType::FSST_VECTOR);
		return vector.Buffer().GetValidityMask();
	}
	static inline ValidityMask &Validity(Vector &vector) {
		D_ASSERT(vector.GetVectorType() == VectorType::FSST_VECTOR);
		return vector.BufferMutable().GetValidityMask();
	}
	static inline void SetValidity(Vector &vector, ValidityMask &new_validity) {
		D_ASSERT(vector.GetVectorType() == VectorType::FSST_VECTOR);
		auto &validity = vector.BufferMutable().GetValidityMask();
		validity.Initialize(new_validity);
	}
	static inline const string_t *GetCompressedData(const Vector &vector) {
		D_ASSERT(vector.GetVectorType() == VectorType::FSST_VECTOR);
		return reinterpret_cast<const string_t *>(vector.GetBufferRef()->GetData());
	}
	static inline string_t *GetCompressedData(Vector &vector) {
		D_ASSERT(vector.GetVectorType() == VectorType::FSST_VECTOR);
		return reinterpret_cast<string_t *>(vector.BufferMutable().GetData());
	}

	DUCKDB_API static void Create(Vector &vector, buffer_ptr<void> &duckdb_fsst_decoder,
	                              shared_ptr<FSSTEncoder> encoder, const idx_t string_block_limit, idx_t capacity,
	                              idx_t auxiliary_size, idx_t symbol_count, idx_t row_group_id = 0);
	//! Grow an existing FSST vector to hold added_count more values / added_bytes more payload (append across scans)
	DUCKDB_API static void Grow(Vector &vector, idx_t added_count, idx_t added_bytes);
	DUCKDB_API static void *GetDecoder(const Vector &vector);
	//! Number of codes in the vector's symbol table (codes 0..count-1)
	DUCKDB_API static idx_t GetSymbolCount(const Vector &vector);
	//! The scan's row group number, see VectorFSSTStringBuffer::GetRowGroupId
	DUCKDB_API static idx_t GetRowGroupId(const Vector &vector);
	DUCKDB_API static vector<unsigned char> &GetDecompressBuffer(const Vector &vector);
	//! Format the FSST symbol table (decoder) of this vector as a human-readable string
	DUCKDB_API static string SymbolTableToString(const Vector &vector);
	//! Print the FSST symbol table (decoder) of this vector for debugging
	DUCKDB_API static void PrintSymbolTable(const Vector &vector);
	//! Raw compressed bytes + length of value index (points into the byte buffer; no copy)
	DUCKDB_API static var_binary_t GetCompressedString(const Vector &vector, idx_t index);
	//! Positional view over the compressed data (base + offsets + lengths); fetch once, then read
	//! values with view.GetVarBinary(i). Valid for sliced and unsliced vectors alike.
	DUCKDB_API static var_binary_view_t GetDataView(const Vector &vector);
	//! The FSST buffer of the vector
	DUCKDB_API static VectorFSSTStringBuffer &GetFSSTBuffer(const Vector &vector);
	//! Compress a string using the vector's symbol table, returning the compressed bytes.
	DUCKDB_API static string CompressValue(const Vector &vector, const char *input, idx_t input_len);
};

} // namespace duckdb
