#pragma once

// FlatDictionary — a dictionary builder backed by its own linear-probing hash table (modelled on
// DuckDB's PrimitiveDictionary). Vendored and adapted from
// SqlPile/external/CompressionBenchmark (github.com/gropaul/CompressionBenchmark,
// src/algorithms/dict/flat_dictionary.hpp): the only change is Build(), which now consumes a raw
// (bytes, offsets, num_rows) chunk view instead of that project's StringCollector, so this header is
// self-contained (no duckdb.h). Shared verbatim by the `dict` and `dict_fsst` candidates.
//
// Deduplicating one chunk yields:
//   - one contiguous buffer of the unique strings (Blob()), padded so a 16-byte overshoot is safe,
//   - parallel offset_/length_ arrays locating each unique string in that buffer,
//   - one dictionary code per input row (Codes()).
// Because the unique strings live in a flat buffer with offsets, a row is reconstructed with a single
// memcpy out of the buffer — no hashing at decode time.

#include <vector>
#include <cstdint>
#include <cstring>
#include <algorithm>

class FlatDictionary {
public:
    // Strings are copied in fixed 16-byte units (branchless), overshooting the end by up to 15 bytes.
    static constexpr uint32_t COPY_CHUNK = 16;

    // Copy `len` bytes from src to dst in fixed 16-byte stores (one movups each). Overshoots up to
    // COPY_CHUNK-1 bytes; callers guarantee both buffers have >= COPY_CHUNK bytes of trailing slack.
    static inline void Copy16(uint8_t *dst, const uint8_t *src, uint32_t len) {
        uint32_t copied = 0;
        do {
            std::memcpy(dst + copied, src + copied, COPY_CHUNK);
            copied += COPY_CHUNK;
        } while (copied < len);
    }

    // Deduplicate a chunk view (row i is bytes[offsets[i] .. offsets[i+1])) into the flat dictionary.
    // Overwrites any previous state.
    void Build(const uint8_t *bytes, const uint64_t *offsets, uint64_t num_rows) {
        const size_t n = static_cast<size_t>(num_rows);
        const uint64_t total_bytes = offsets[n];

        // Capacity is a power of two >= n * LOAD_FACTOR, so even an all-unique chunk never fills the
        // table (short probe chains, no resizing).
        constexpr size_t LOAD_FACTOR = 2;
        table_capacity_ = NextPowerOfTwo(std::max<size_t>(n * LOAD_FACTOR, 16));
        table_mask_ = table_capacity_ - 1;
        table_.assign(table_capacity_, Entry{}); // Entry{} has index == INVALID_INDEX

        blob_.clear();
        blob_.reserve(total_bytes / 10 + 1); // assume ~10% unique
        offset_.clear();
        offset_.reserve(n / 10 + 1);
        length_.clear();
        length_.reserve(n / 10 + 1);
        codes_.resize(n);

        for (size_t i = 0; i < n; i++) {
            const uint8_t *ptr = bytes + offsets[i];
            const auto len = static_cast<uint32_t>(offsets[i + 1] - offsets[i]);
            codes_[i] = InsertOrGet(ptr, len);
        }

        // Pad the blob so the last string's 16-byte-chunk overshoot stays in-bounds; remember the
        // real (unpadded) size for size accounting.
        blob_data_size_ = blob_.size();
        blob_.resize(blob_.size() + COPY_CHUNK, 0);
    }

    // Unique strings packed contiguously, padded by COPY_CHUNK bytes.
    const uint8_t *Blob() const { return blob_.data(); }
    // Real (unpadded) byte size of the unique strings.
    size_t BlobSize() const { return blob_data_size_; }

    const std::vector<uint32_t> &Offsets() const { return offset_; }
    const std::vector<uint32_t> &Lengths() const { return length_; }
    const std::vector<uint32_t> &Codes() const { return codes_; }

    size_t NumUnique() const { return offset_.size(); }
    size_t NumRows() const { return codes_.size(); }

    void Free() {
        table_.clear();
        table_.shrink_to_fit();
        blob_.clear();
        blob_.shrink_to_fit();
        blob_data_size_ = 0;
        offset_.clear();
        length_.clear();
        codes_.clear();
    }

private:
    static constexpr uint32_t INVALID_INDEX = static_cast<uint32_t>(-1);

    struct Entry {
        uint32_t offset = 0;              // offset of the string in blob_
        uint32_t length = 0;              // length of the string
        uint32_t index = INVALID_INDEX;   // dictionary code; INVALID_INDEX == empty slot
    };

    static size_t NextPowerOfTwo(size_t v) {
        size_t p = 1;
        while (p < v) p <<= 1;
        return p;
    }

    // Avalanche finalizer (DuckDB's MurmurHash64, from nullprogram.com/blog/2018/07/31/).
    static inline uint64_t MurmurHash64(uint64_t x) {
        x ^= x >> 32;
        x *= 0xd6e8feb86659fd93ULL;
        x ^= x >> 32;
        x *= 0xd6e8feb86659fd93ULL;
        x ^= x >> 32;
        return x;
    }

    // DuckDB's HashBytes (src/common/types/hash.cpp): hash/combine in 8-byte blocks, load the
    // (<8-byte) remainder, then finalize. Reproduced inline so it inlines into the build loop.
    static inline uint64_t HashBytes(const uint8_t *ptr, uint32_t len) {
        uint64_t h = 0xe17a1465ULL ^ (static_cast<uint64_t>(len) * 0xc6a4a7935bd1e995ULL);

        const uint32_t remainder = len & 7U;
        for (const uint8_t *end = ptr + len - remainder; ptr != end; ptr += 8U) {
            uint64_t block;
            std::memcpy(&block, ptr, sizeof(block)); // unaligned 8-byte load
            h ^= block;
            h *= 0xd6e8feb86659fd93ULL;
        }

        if (remainder != 0) {
            uint64_t hr = 0;
            std::memcpy(&hr, ptr, remainder);
            h ^= hr;
            h *= 0xd6e8feb86659fd93ULL;
        }

        return MurmurHash64(h);
    }

    // Byte-equality in 8-byte blocks while >= 8 bytes remain, then a byte-wise tail (< 8). Both
    // operands always have `len` valid bytes, so the 8-byte loads never read out of bounds.
    static inline bool EqualBytes(const uint8_t *a, const uint8_t *b, uint32_t len) {
        while (len >= 8) {
            uint64_t x, y;
            std::memcpy(&x, a, 8); // unaligned 8-byte loads
            std::memcpy(&y, b, 8);
            if (x != y) return false;
            a += 8;
            b += 8;
            len -= 8;
        }
        while (len-- > 0) {
            if (*a++ != *b++) return false;
        }
        return true;
    }

    // Look the string up with linear probing; insert (copying bytes into blob_) if absent.
    // Returns the dictionary code for the string.
    inline uint32_t InsertOrGet(const uint8_t *ptr, uint32_t len) {
        size_t slot = HashBytes(ptr, len) & table_mask_;
        while (table_[slot].index != INVALID_INDEX) {
            const Entry &e = table_[slot];
            if (e.length == len && EqualBytes(blob_.data() + e.offset, ptr, len)) {
                return e.index; // already present
            }
            slot = (slot + 1) & table_mask_;
        }

        const auto offset = static_cast<uint32_t>(blob_.size());
        const auto code = static_cast<uint32_t>(offset_.size());
        blob_.insert(blob_.end(), ptr, ptr + len);
        offset_.push_back(offset);
        length_.push_back(len);
        table_[slot] = Entry{offset, len, code};
        return code;
    }

    // Linear-probing hash table.
    std::vector<Entry> table_;
    size_t table_capacity_{0};
    size_t table_mask_{0};

    // The dictionary blob and its metadata.
    std::vector<uint8_t> blob_;
    size_t blob_data_size_{0};
    std::vector<uint32_t> offset_;
    std::vector<uint32_t> length_;

    // One dictionary code per row.
    std::vector<uint32_t> codes_;
};
