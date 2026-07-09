// cpp_identity — the C++ smoke candidate (DESIGN.md §11 step 7).
//
// A "compression" scheme at ratio ~1: build() copies the chunk into private
// buffers, decode() memcpy's it back out. Deliberately tiny, yet it
// exercises exactly the shape every real compressed candidate will use:
// C++ built under cargo, named footprint components, and the
// harness-composed "decode" strategy across every scanner.

#include "lb_candidate.h"

#include <cstdio>
#include <cstring>
#include <new>
#include <vector>

namespace {

struct Handle {
  std::vector<uint8_t> bytes;
  std::vector<uint64_t> offsets;
  uint64_t num_rows;
};

void* identity_build(const lb_chunk_view* view, const char* /*config_json*/,
                     char* err_buf, uint64_t err_cap) {
  auto* h = new (std::nothrow) Handle;
  if (h == nullptr) {
    if (err_cap > 0) std::snprintf(err_buf, err_cap, "allocation failed");
    return nullptr;
  }
  const uint64_t payload = view->offsets[view->num_rows];
  h->bytes.assign(view->bytes, view->bytes + payload);
  h->offsets.assign(view->offsets, view->offsets + view->num_rows + 1);
  h->num_rows = view->num_rows;
  return h;
}

uint32_t identity_footprint(void* self, lb_footprint_component* out,
                            uint32_t capacity) {
  auto* h = static_cast<Handle*>(self);
  const lb_footprint_component components[] = {
      {"payload", h->bytes.size()},
      {"offsets", h->offsets.size() * sizeof(uint64_t)},
  };
  const uint32_t count = 2;
  for (uint32_t i = 0; i < count && i < capacity; i++) out[i] = components[i];
  return count;
}

int identity_decode(void* self, uint8_t* bytes_out, uint64_t bytes_cap,
                    uint64_t* offsets_out) {
  auto* h = static_cast<Handle*>(self);
  if (bytes_cap < h->bytes.size()) return 1;
  // Full decode cost on every call — no memoization (SEMANTICS.md rule 1).
  if (!h->bytes.empty()) std::memcpy(bytes_out, h->bytes.data(), h->bytes.size());
  std::memcpy(offsets_out, h->offsets.data(),
              h->offsets.size() * sizeof(uint64_t));
  return 0;
}

void identity_destroy(void* self) { delete static_cast<Handle*>(self); }

const lb_candidate kVtable = {
    /*abi_version=*/LB_ABI_VERSION,
    /*name=*/"cpp_identity",
    /*version=*/"0.1.0",
    /*cpu_features=*/nullptr,
    /*strategies=*/nullptr,
    /*strategy_count=*/0,
    /*build=*/identity_build,
    /*footprint=*/identity_footprint,
    /*run=*/nullptr,
    /*view=*/nullptr,
    /*decode=*/identity_decode,
    /*destroy=*/identity_destroy,
};

}  // namespace

extern "C" const lb_candidate* lb_candidate_cpp_identity(void) {
  return &kVtable;
}
