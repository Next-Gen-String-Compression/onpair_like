//===----------------------------------------------------------------------===//
//                         DuckDB
//
// duckdb/common/vector/variable_binary_buffer.hpp
//
//
//===----------------------------------------------------------------------===//

#pragma once

#include "duckdb/common/types/vector.hpp"

namespace duckdb {

//! A non-owning view of a value: raw bytes + length, pointing into the byte buffer (no copy, never inlined).
struct var_binary_t {
	const char *ptr;
	idx_t length;

	//! Wrap a string_t as a view. Note: for inlined string_t the pointer is into the string_t itself,
	//! so the source must outlive the view.
	static var_binary_t FromString(const string_t &str) {
		return {str.GetData(), str.GetSize()};
	}

	bool operator==(const var_binary_t &other) const {
		// the empty needle is a common filter and memcmp is an out-of-line call even for length 0
		return length == other.length && (length == 0 || memcmp(ptr, other.ptr, length) == 0);
	}
};

//! A non-owning view of a whole VariableBinaryBuffer: fetch once, then read values positionally.
//! Valid for sliced and unsliced buffers alike - logical row i is always base[offsets[i] : offsets[i] + lengths[i]].
//! The source buffer must outlive the view.
struct var_binary_view_t {
	const char *base;
	const int32_t *offsets;
	const uint32_t *lengths;

	inline var_binary_t GetVarBinary(idx_t index) const {
		return {base + offsets[index], idx_t(lengths[index])};
	}
};

//! Stores variable-length binary values as a contiguous byte buffer plus per-value int32 offsets and
//! uint32 lengths arrays (capacity entries each): value i occupies byte_data[offsets[i] : offsets[i] + lengths[i]].
//! Freshly scanned buffers are REVERSE-packed (value 0 at the end, the last value at the front) because that is
//! how they arrive from the dictionary block, but nothing may rely on adjacency: a slice shares the byte buffer
//! and simply gathers new offsets/lengths arrays. Pre-sized at construction.
class VariableBinaryBuffer : public VectorBuffer {
public:
	//! element_count: number of values this buffer will hold (also the vector size - pre-sized, fully filled)
	//! auxiliary_size: total number of payload bytes across all values
	VariableBinaryBuffer(idx_t element_count, idx_t auxiliary_size,
	                     Allocator &allocator = Allocator::DefaultAllocator())
	    : VectorBuffer(VectorType::FLAT_VECTOR, VectorBufferType::VARIABLE_BINARY_BUFFER, count_t(element_count)),
	      offset_data(allocator.Allocate(sizeof(int32_t) * (element_count + 1))),
	      length_data(allocator.Allocate(sizeof(uint32_t) * (element_count + 1))),
	      byte_data(allocator.Allocate(auxiliary_size == 0 ? 1 : auxiliary_size)), capacity(element_count) {
		offsets = reinterpret_cast<int32_t *>(offset_data.get());
		lengths = reinterpret_cast<uint32_t *>(length_data.get());
		byte_data_ptr = byte_data.get();
		validity.Resize(element_count);
	}

	//! Slice view constructor: shares the byte buffer of source without copying the payload.
	//! Allocates fresh offsets/lengths arrays for count values; the caller fills them (or uses the
	//! range variant below) and must keep source alive via auxiliary data.
	VariableBinaryBuffer(const VariableBinaryBuffer &source, count_t count,
	                     Allocator &allocator = Allocator::DefaultAllocator())
	    : VectorBuffer(VectorType::FLAT_VECTOR, VectorBufferType::VARIABLE_BINARY_BUFFER, count),
	      offset_data(allocator.Allocate(sizeof(int32_t) * (count + 1))),
	      length_data(allocator.Allocate(sizeof(uint32_t) * (count + 1))), byte_data_ptr(source.byte_data_ptr),
	      pin(source.pin), capacity(count), is_view(true) {
		offsets = reinterpret_cast<int32_t *>(offset_data.get());
		lengths = reinterpret_cast<uint32_t *>(length_data.get());
		validity.Resize(count);
	}

	//! Range view constructor: shares the byte buffer AND the offsets/lengths of source (shifted by offset).
	//! Fully zero-copy; the caller must keep source alive via auxiliary data.
	VariableBinaryBuffer(const VariableBinaryBuffer &source, count_t count, idx_t offset)
	    : VectorBuffer(VectorType::FLAT_VECTOR, VectorBufferType::VARIABLE_BINARY_BUFFER, count),
	      offsets(source.offsets + offset), lengths(source.lengths + offset), byte_data_ptr(source.byte_data_ptr),
	      pin(source.pin), capacity(count), is_view(true) {
		validity.Resize(count);
	}

public:
	ValidityMask &GetValidityMask() override {
		return validity;
	}
	const ValidityMask &GetValidityMask() const override {
		return validity;
	}
	idx_t Capacity() const override {
		return capacity;
	}
	//! Payload bytes; not exposed via GetData() so FlatVector::GetData<string_t> cannot misread it
	data_ptr_t GetBytes() {
		return byte_data_ptr;
	}
	const_data_ptr_t GetBytes() const {
		return byte_data_ptr;
	}
	//! Per-value byte positions into the byte buffer; populators fill this
	int32_t *GetOffsets() {
		return offsets;
	}
	const int32_t *GetOffsets() const {
		return offsets;
	}
	//! Per-value byte lengths; populators fill this
	uint32_t *GetLengths() {
		return lengths;
	}
	const uint32_t *GetLengths() const {
		return lengths;
	}
	//! Positional view over the whole buffer: fetch once, then read values without per-value buffer lookups
	var_binary_view_t GetView() const {
		return {const_char_ptr_cast(byte_data_ptr), offsets, lengths};
	}
	//! Raw bytes + length of value index (points into the byte buffer; safe for any length)
	var_binary_t GetVarBinary(idx_t index) const {
		return {const_char_ptr_cast(byte_data_ptr) + offsets[index], idx_t(lengths[index])};
	}
	//! View of value index as a string_t into the byte buffer (no copy)
	string_t GetString(idx_t index) const {
		auto view = GetVarBinary(index);
		return string_t(view.ptr, UnsafeNumericCast<uint32_t>(view.length));
	}

	//! Use the bytes at `base` inside a pinned block instead of an owned copy. The pin keeps the block in
	//! memory for as long as this buffer or any slice of it lives; Grow copies the bytes out and drops it.
	void ReferenceBytes(data_ptr_t base, shared_ptr<BufferHandle> pin_p) {
		byte_data_ptr = base;
		pin = std::move(pin_p);
	}

	//! Total payload bytes currently held (reverse layout: value 0 sits at the top of the byte buffer)
	idx_t ByteSize() const {
		if (is_view) {
			throw InternalException("ByteSize is not defined for a slice view of a VariableBinaryBuffer");
		}
		return capacity == 0 ? 0 : idx_t(offsets[0]) + lengths[0];
	}

	//! Grow to hold added_count more values / added_bytes more payload, preserving existing contents.
	//! The new (later-scanned) block goes at the FRONT of the byte buffer, so existing bytes shift UP by added_bytes
	//! and existing offsets shift up by the same amount (lengths are unaffected). Leaves the offset/length slots
	//! [old capacity ..] and byte region [0 .. added_bytes) for the caller to fill.
	void Grow(idx_t added_count, idx_t added_bytes) {
		if (is_view) {
			throw InternalException("Cannot Grow a slice view of a VariableBinaryBuffer");
		}
		auto &alloc = *byte_data.GetAllocator();
		const idx_t old_capacity = capacity;
		const idx_t old_bytes = ByteSize();
		const idx_t new_capacity = old_capacity + added_count;
		const idx_t new_bytes = old_bytes + added_bytes;

		AllocatedData new_offset_data = alloc.Allocate(sizeof(int32_t) * (new_capacity + 1));
		auto new_offsets = reinterpret_cast<int32_t *>(new_offset_data.get());
		for (idx_t i = 0; i < old_capacity; i++) {
			new_offsets[i] = offsets[i] + UnsafeNumericCast<int32_t>(added_bytes);
		}
		AllocatedData new_length_data = alloc.Allocate(sizeof(uint32_t) * (new_capacity + 1));
		auto new_lengths = reinterpret_cast<uint32_t *>(new_length_data.get());
		memcpy(new_lengths, lengths, sizeof(uint32_t) * old_capacity);

		AllocatedData new_byte_data = alloc.Allocate(new_bytes == 0 ? 1 : new_bytes);
		memcpy(new_byte_data.get() + added_bytes, byte_data_ptr, old_bytes);

		offset_data = std::move(new_offset_data);
		length_data = std::move(new_length_data);
		byte_data = std::move(new_byte_data);
		offsets = new_offsets;
		lengths = new_lengths;
		byte_data_ptr = byte_data.get();
		pin.reset();
		capacity = new_capacity;
		validity.Resize(new_capacity);
		SetVectorSizeOnly(new_capacity);
	}

private:
	ValidityMask validity;
	//! per-value byte positions into the byte buffer, capacity entries
	AllocatedData offset_data;
	int32_t *offsets;
	//! per-value byte lengths, capacity entries
	AllocatedData length_data;
	uint32_t *lengths;

	//! contiguous payload bytes
	AllocatedData byte_data;
	data_ptr_t byte_data_ptr;
	//! Set when byte_data_ptr points into a buffer-manager block rather than byte_data
	shared_ptr<BufferHandle> pin;
	//! number of values this buffer holds
	idx_t capacity;
	//! true when this buffer shares another buffer's byte data (must not Grow)
	bool is_view = false;
};

} // namespace duckdb
