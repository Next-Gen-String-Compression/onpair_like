/* lb_candidate.h — the LIKE-benchmark candidate & scanner contract.
 *
 * This header is the single cross-language source of truth. Rust code uses
 * the `lb-abi` crate, which mirrors these definitions exactly; C/C++ code
 * includes this header directly. Semantics (operation definitions, edge
 * cases, candidate hygiene rules) live in SEMANTICS.md next to this file.
 *
 * Versioning: any layout or semantic change bumps LB_ABI_VERSION. The
 * harness refuses to register a module whose abi_version does not match.
 */
#ifndef LB_CANDIDATE_H
#define LB_CANDIDATE_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define LB_ABI_VERSION 4u

/* Guaranteed writable headroom past the decoded payload in every decode()
 * output buffer (see lb_candidate.decode). Lets fixed-stride over-copying
 * decoders (FSST- and OnPair-style: emit a constant-size copy per
 * token/symbol, advance by true length) run at full speed with no
 * defensive tail handling and no extra memcpy inside the timed path. */
#define LB_DECODE_PAD 64u

/* ------------------------------------------------------------------ data */

typedef struct lb_bytes {
  const uint8_t* ptr;
  uint64_t       len;
} lb_bytes;

/* One chunk of the canonical column. Row i occupies
 * bytes[offsets[i] .. offsets[i+1]). offsets has num_rows + 1 entries and
 * is rebased so offsets[0] == 0. The view is read-only; writing through it
 * is undefined behaviour (the harness maps data read-only where it can). */
typedef struct lb_chunk_view {
  const uint8_t*  bytes;
  const uint64_t* offsets;
  uint64_t        num_rows;
} lb_chunk_view;

/* --------------------------------------------------------------- queries */

/* Operation codes. Stored as uint32_t in structs so the ABI does not
 * depend on the C compiler's enum sizing. */
enum {
  LB_PREFIX         = 0, /* row starts with needle            LIKE 'n%'   */
  LB_SUFFIX         = 1, /* row ends with needle              LIKE '%n'   */
  LB_CONTAINS       = 2, /* needle occurs anywhere            LIKE '%n%'  */
  LB_MULTI_CONTAINS = 3, /* needles in order, non-overlapping LIKE '%a%b%'*/
  LB_CONTAINS_ANY   = 4, /* any needle occurs                 OR of LIKEs */
  LB_OP_COUNT       = 5
};

/* Bit for op `o` in a supported_ops bitmask. */
#define LB_OP_BIT(o) (1u << (o))
#define LB_ALL_OPS   ((1u << LB_OP_COUNT) - 1u)

typedef struct lb_query {
  uint32_t        op;           /* one of the LB_* op codes            */
  const lb_bytes* needles;
  uint32_t        needle_count; /* per-op arity rules in SEMANTICS.md  */
} lb_query;

/* ------------------------------------------------------------- reporting */

/* Resident-size components, e.g. "payload", "offsets", "prefilter".
 * Named so prefilter storage cost is attributable on the compression axis. */
typedef struct lb_footprint_component {
  char     name[32];  /* NUL-terminated                                  */
  uint64_t bytes;
} lb_footprint_component;

/* Instrumented-mode statistics. The stats pointer passed to run()/scan()
 * is NULL in timing mode (do no bookkeeping) and non-NULL in instrumented
 * mode. The harness pre-fills every field with LB_STAT_UNSET; fill only
 * what you actually measure. Self-timed *_ns fields are labelled
 * self-reported in results and never mixed into headline latency. */
#define LB_STAT_UNSET UINT64_MAX
typedef struct lb_run_stats {
  uint64_t prefilter_candidates; /* rows surviving the prefilter        */
  uint64_t decode_ns;            /* self-timed phase breakdown          */
  uint64_t prefilter_ns;
  uint64_t verify_ns;
  uint64_t setup_ns;             /* per-query setup: pattern/automaton
                                    compilation before any row or token
                                    is examined (ABI v3)               */
} lb_run_stats;

/* ------------------------------------------------------------ strategies */

/* A candidate-implemented way of answering queries (e.g. "compressed",
 * or a custom fused/hybrid path). The names "direct" and "decode" are
 * reserved for harness-composed strategies and must never be declared. */
typedef struct lb_strategy {
  const char* name;
  uint32_t    supported_ops;   /* bitmask of LB_OP_BIT(op)              */
} lb_strategy;

/* ------------------------------------------------------------- candidate */

/* Return-code convention for all int-returning entry points:
 * 0 = success, nonzero = failure (the cell is recorded as errored). */
typedef struct lb_candidate {
  uint32_t           abi_version;   /* must equal LB_ABI_VERSION         */
  const char*        name;
  const char*        version;
  /* Required host CPU features, comma-separated, lower case, e.g.
   * "avx2,bmi2" / "avx512f,avx512bw" / "neon". NULL or "" = portable.
   * Unmet requirements mean the module never runs on this host; its
   * cells are recorded as unavailable (DESIGN.md §9). There is no
   * scalar-fallback mechanism by design.                                */
  const char*        cpu_features;

  const lb_strategy* strategies;    /* candidate-implemented; may be 0   */
  uint32_t           strategy_count;

  /* Build the internal representation for one chunk. This IS the
   * compression step; it is harness-timed. config_json is an opaque
   * candidate-defined configuration string (always valid UTF-8 JSON).
   * Returns an opaque handle, or NULL on failure with a NUL-terminated
   * message in err_buf.                                                 */
  void* (*build)(const lb_chunk_view* view, const char* config_json,
                 char* err_buf, uint64_t err_cap);

  /* Report resident bytes as named components. Writes up to `capacity`
   * components into `out` and returns the total component count (which
   * may exceed capacity; the harness then retries with a larger buffer). */
  uint32_t (*footprint)(void* self, lb_footprint_component* out,
                        uint32_t capacity);

  /* Answer one query under declared strategy `strategy_index`, setting
   * bit i of out_bitmap_words iff row i of this chunk matches. The bitmap
   * slice is harness-owned, pre-zeroed, LSB-first within little-endian
   * 64-bit words, and sized to ceil(num_rows/64) words. NULL iff
   * strategy_count == 0. Must not memoize work across calls
   * (SEMANTICS.md).                                                     */
  int (*run)(void* self, uint32_t strategy_index, const lb_query* query,
             uint64_t* out_bitmap_words, lb_run_stats* stats_or_null);

  /* Optional (NULL if absent): zero-copy view of the stored data, valid
   * until destroy(). Only for schemes whose stored form already is the
   * canonical layout; enables the harness-composed "direct" strategy.   */
  int (*view)(void* self, lb_chunk_view* out);

  /* Optional (NULL if absent): decompress this chunk into caller-provided
   * buffers in canonical layout. bytes_out has bytes_cap writable bytes;
   * the harness guarantees bytes_cap >= chunk payload + LB_DECODE_PAD, so
   * decoders may over-write up to bytes_cap as scratch — only
   * [0, payload) is meaningful afterwards. offsets_out has num_rows + 1
   * slots. Enables the harness-composed "decode" strategy. Pays full
   * decode cost on every call — no memoization.                          */
  int (*decode)(void* self, uint8_t* bytes_out, uint64_t bytes_cap,
                uint64_t* offsets_out);

  void (*destroy)(void* self);
} lb_candidate;
/* A candidate must offer at least one of: run (with strategies), view,
 * decode. */

/* --------------------------------------------------------------- scanner */

/* A pluggable uncompressed prefilter+eval kernel. Scanners are benchmark
 * subjects: each registered scanner is composed with every candidate's
 * "direct" (over view()) and "decode" (over decode() output) strategies. */
typedef struct lb_scanner {
  uint32_t    abi_version;      /* must equal LB_ABI_VERSION             */
  const char* name;
  const char* version;
  const char* cpu_features;     /* same hard gating as candidates        */
  uint32_t    supported_ops;    /* bitmask of LB_OP_BIT(op)              */

  /* Compile the needles (masks, tables, automata). Chunk-independent;
   * called once per query inside each timed sample. Returns an opaque
   * handle, NULL on failure.                                            */
  void* (*prepare)(const lb_query* query);

  /* Per-chunk prefilter + eval into the pre-zeroed bitmap slice; same
   * bitmap and stats conventions as lb_candidate.run.                   */
  int (*scan)(void* prepared, const lb_chunk_view* view,
              uint64_t* out_bitmap_words, lb_run_stats* stats_or_null);

  void (*release)(void* prepared);

  /* Optional (NULL if absent): per-query capability probe (ABI v4). When
   * non-NULL the harness calls it after supported_ops passes and before
   * prepare(); returning 0 marks the cell Unsupported — a *declared
   * capability gap* (e.g. a needle longer than a bit-parallel scanner's
   * word, or a multi-literal engine's pattern-count ceiling), reported
   * distinctly from an error. Nonzero => the scanner will handle this
   * query. Pure predicate: no allocation, no side effects, and it must
   * agree with prepare() (prepare must not return NULL for an accepted
   * query except on genuine resource failure).                          */
  int (*supports_query)(const lb_query* query);
} lb_scanner;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* LB_CANDIDATE_H */
