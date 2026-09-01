// guard_test.cpp — memory-safety + semantics probe for the FSST-LIKE-Matching
// (calin2110, DaMoN'26) matcher backends, driven exactly the way
// candidates/fsst_like_tum builds them: calin fsst fork fsst_create /
// fsst_compress -> escaped-byte bitmap clobber into symbols[255] -> Encoder.
// Findings and the resulting candidate fix: DESIGN.md §17.7.
//
// For every (pattern, backend, row) the compressed row is parsed in four
// placements: contiguous heap (as the harness lays rows out), END (row ends
// exactly at a PROT_NONE page: catches over-reads), START (row begins right
// after a PROT_NONE page: catches under-reads) and MID (surrounded by fill
// bytes 0x00 / 0xFF / 0x41: does the answer depend on out-of-row bytes?). On a
// fault the slack needed to avoid it is probed, and the single neighbouring
// byte is swept over all 256 values. Every answer is checked against a
// SEMANTICS.md oracle on the raw rows.
//
//   ./guard_test                      synthetic corpus + adversarial patterns
//   ./guard_test --rows FILE          rows from gen_fixture.py's .bin dump
//   ./guard_test --probe-only [...]   stop after reporting whether 0xFF is escaped
//   ./guard_test --no-guard [...]     heap placement only (for ASAN builds)
#include "codegen/cppcodegen.hpp"
#include "encoder.hpp"
#include "like_pattern_automaton.hpp"
#include <fsst/fsst.h>
#ifdef HAVE_LLVM
#include "codegen/llvmcodegen.hpp"
#include "llvm/Support/TargetSelect.h"
#endif

#include <algorithm>
#include <csetjmp>
#include <csignal>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <functional>
#include <memory>
#include <random>
#include <span>
#include <string>
#include <sys/mman.h>
#include <unistd.h>
#include <vector>

using Bytes = std::vector<uint8_t>;
constexpr size_t kFillPad = 64;        // fill bytes on each side in MID placement
constexpr size_t kMaxSlackProbe = 64;  // largest slack tried after a fault
constexpr int kExitFault = 2, kExitNotEscaped = 3;

// ------------------------------------------------------------- guard pages
static sigjmp_buf g_jmp;
static volatile sig_atomic_t g_in_guard = 0;
static void on_fault(int) {
  if (g_in_guard) siglongjmp(g_jmp, 1);
  _exit(139);
}
static void install_fault_trap() {
  struct sigaction sa {};
  sa.sa_handler = on_fault;
  sa.sa_flags = SA_NODEFER;
  sigaction(SIGSEGV, &sa, nullptr);
  sigaction(SIGBUS, &sa, nullptr);
}
// Runs f() with faults trapped; false if it faulted.
template <class F> static bool guarded(F&& f) {
  g_in_guard = 1;
  if (sigsetjmp(g_jmp, 1) != 0) { g_in_guard = 0; return false; }
  f();
  g_in_guard = 0;
  return true;
}
// [PROT_NONE page][RW pages][PROT_NONE page]
struct GuardRegion {
  uint8_t* base = nullptr;
  size_t page = 0, rw = 0;
  explicit GuardRegion(size_t rw_bytes) {
    page = size_t(sysconf(_SC_PAGESIZE));
    rw = std::max<size_t>(page, (rw_bytes + page - 1) / page * page);
    void* m = mmap(nullptr, rw + 2 * page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (m == MAP_FAILED) { perror("mmap"); std::exit(kExitFault); }
    base = static_cast<uint8_t*>(m);
    mprotect(base, page, PROT_NONE);
    mprotect(base + page + rw, page, PROT_NONE);
  }
  ~GuardRegion() { munmap(base, rw + 2 * page); }
  uint8_t* rw_begin() const { return base + page; }
  uint8_t* rw_end() const { return base + page + rw; }
  // `n` bytes ending `slack` bytes before the top guard page (slack zeroed).
  uint8_t* place_end(const uint8_t* p, size_t n, size_t slack = 0) {
    uint8_t* d = rw_end() - n - slack;
    std::memcpy(d, p, n);
    std::memset(d + n, 0, slack);
    return d;
  }
  // `n` bytes starting `slack` bytes after the bottom guard page (slack zeroed).
  uint8_t* place_start(const uint8_t* p, size_t n, size_t slack = 0) {
    std::memset(rw_begin(), 0, slack);
    uint8_t* d = rw_begin() + slack;
    std::memcpy(d, p, n);
    return d;
  }
  // `n` bytes with kFillPad bytes of `fill` on both sides.
  uint8_t* place_mid(const uint8_t* p, size_t n, uint8_t fill) {
    std::memset(rw_begin(), fill, kFillPad);
    uint8_t* d = rw_begin() + kFillPad;
    std::memcpy(d, p, n);
    std::memset(d + n, fill, kFillPad);
    return d;
  }
};

// ------------------------------------------------------------- oracle
enum Op { PREFIX, SUFFIX, CONTAINS, MULTI };
struct Query { Op op; std::vector<Bytes> needles; std::string label; };

static bool find_at(const Bytes& row, const Bytes& n, size_t from, size_t* pos) {
  if (from > row.size()) return false;
  if (n.empty()) { *pos = from; return true; }
  if (n.size() > row.size() - from) return false;
  const void* r = memmem(row.data() + from, row.size() - from, n.data(), n.size());
  if (!r) return false;
  *pos = size_t(static_cast<const uint8_t*>(r) - row.data());
  return true;
}
static bool oracle(const Bytes& row, const Query& q) {
  const Bytes& n0 = q.needles[0];
  switch (q.op) {
    case PREFIX: return n0.size() <= row.size() && std::equal(n0.begin(), n0.end(), row.begin());
    case SUFFIX: return n0.size() <= row.size() && std::equal(n0.begin(), n0.end(), row.end() - n0.size());
    case CONTAINS: { size_t p; return find_at(row, n0, 0, &p); }
    case MULTI: {
      size_t from = 0;
      for (const Bytes& n : q.needles) {
        size_t p;
        if (!find_at(row, n, from, &p)) return false;
        from = p + n.size();
      }
      return true;
    }
  }
  return false;
}
// Identical to the candidate's to_like_pattern (escape % _ \ then wrap).
static void escape_append(Bytes& out, const Bytes& nd) {
  for (uint8_t c : nd) {
    if (c == '%' || c == '_' || c == '\\') out.push_back('\\');
    out.push_back(c);
  }
}
static Bytes to_like(const Query& q) {
  Bytes p;
  if (q.op != PREFIX) p.push_back('%');
  for (size_t i = 0; i < q.needles.size(); i++) {
    escape_append(p, q.needles[i]);
    if (q.op == MULTI || (q.op != SUFFIX && i + 1 == q.needles.size())) p.push_back('%');
  }
  return p;
}
static bool all_empty(const Query& q) {
  return std::all_of(q.needles.begin(), q.needles.end(), [](const Bytes& n) { return n.empty(); });
}

// ------------------------------------------------------------- corpus
static Bytes B(const char* s) { return Bytes(s, s + std::strlen(s)); }
static const char* kWords[] = {
    "the","of","and","to","in","a","is","that","for","it","as","was","with","be","by","on","not",
    "he","this","are","or","his","from","at","which","but","have","an","had","they","you","were",
    "their","one","all","we","can","her","has","there","been","if","more","when","will","would",
    "who","so","no","http","https","www","com","org","index","html","query","search","user",
    "customer","comment","abstract","wikipedia","database","compression","symbol","table",
    "string","match","pattern","automaton","fast","static","carefully","regular","requests",
    "special","packages","ironic","furiously","slyly","blithely","daringly","pending","final",
    "deposits","accounts","platelets","foxes","instructions","theodolites","excuses","dolphins",
    "München","naïve","日本語","😀","ß","Ø",
};
struct Gen {
  std::mt19937_64 rng{20260901};
  Bytes word() { return B(kWords[rng() % (sizeof(kWords) / sizeof(kWords[0]))]); }
  Bytes sentence(size_t n) {
    Bytes r;
    for (size_t i = 0; i < n; i++) { if (i) r.push_back(' '); Bytes w = word(); r.insert(r.end(), w.begin(), w.end()); }
    return r;
  }
};
static void add_adversarial_rows(std::vector<Bytes>& rows, Gen& g) {
  for (const char* s : {"", "a", "ab", "the", " ", "%", "_", "\\", "50% off_now\\here", "trailing\\", "\\%", "a\\%b"})
    rows.push_back(B(s));
  { Bytes r = B("ff:"); r.insert(r.end(), 3, 0xFF); rows.push_back(r); }
  rows.push_back(Bytes(5, 0xFF));
  for (int k = 1; k <= 4; k++) { Bytes r = B("run-end:"); r.insert(r.end(), k, 0xFF); rows.push_back(r); }
  for (int k = 1; k <= 4; k++) { Bytes r(k, 0xFF); Bytes t = B(":run-start"); r.insert(r.end(), t.begin(), t.end()); rows.push_back(r); }
  for (int k = 1; k <= 4; k++) rows.push_back(Bytes(k, 0xFF));
  { Bytes r = B("nul"); r.push_back(0); r.push_back(0); r.push_back('x'); rows.push_back(r); }
  { Bytes r; for (int b = 0; b < 256; b++) r.push_back(uint8_t(b)); rows.push_back(r); }
  { Bytes r; for (int b = 255; b >= 0; b--) r.push_back(uint8_t(b)); rows.push_back(r); }
  for (int b : {0x01, 0x02, 0x7f, 0x80, 0xfe, 0xff}) {
    Bytes r = B("x"); r.push_back(uint8_t(b)); r.push_back('y'); rows.push_back(r);
    rows.push_back(Bytes(1, uint8_t(b)));
  }
  for (size_t L : {510, 511, 512, 513, 1021, 1022, 1023, 1024, 1533, 2048}) {  // FSST 511-byte chunk edges
    Bytes r;
    while (r.size() < L) { Bytes w = g.sentence(1); r.insert(r.end(), w.begin(), w.end()); r.push_back(' '); }
    r.resize(L);
    rows.push_back(r);
  }
  for (size_t off : {500, 505, 508, 509, 510, 511, 512, 515}) {  // needle straddling the 511 boundary
    Bytes r = g.sentence(120); r.resize(700);
    Bytes n = B("theodolites"); std::copy(n.begin(), n.end(), r.begin() + off);
    rows.push_back(r);
  }
}
// Bulk rows first: a big, diverse corpus so the rare 0xFF rows stay out of
// FSST's ~16 KB training sample and 0xFF is ESCAPED (the interesting case).
static std::vector<Bytes> make_corpus(Gen& g) {
  std::vector<Bytes> rows;
  for (int i = 0; i < 12000; i++) rows.push_back(g.sentence(g.rng() % 24));
  for (int i = 0; i < 300; i++) rows.push_back(g.sentence(90 + g.rng() % 200));
  add_adversarial_rows(rows, g);
  return rows;
}
static std::vector<Query> make_queries(const std::vector<Bytes>& rows, Gen& g) {
  std::vector<Query> qs;
  auto add = [&](Op op, std::vector<Bytes> ns, std::string label) { qs.push_back({op, std::move(ns), std::move(label)}); };
  auto slice = [&](size_t len, bool from_start, bool from_end) {
    for (int tries = 0; tries < 1000; tries++) {
      const Bytes& r = rows[g.rng() % 3000];
      if (r.size() < len || len == 0) continue;
      size_t off = from_start ? 0 : from_end ? r.size() - len : g.rng() % (r.size() - len + 1);
      return Bytes(r.begin() + off, r.begin() + off + len);
    }
    return B("the");
  };
  for (size_t L : {1, 2, 3, 4, 5, 7, 8, 9, 12, 15, 16, 17, 20, 31, 33}) add(PREFIX, {slice(L, true, false)}, "prefix" + std::to_string(L));
  for (size_t L : {1, 2, 3, 4, 7, 8, 9, 15, 16, 17, 24, 33}) add(SUFFIX, {slice(L, false, true)}, "suffix" + std::to_string(L));
  for (size_t L : {1, 2, 3, 4, 5, 8, 9, 16, 17, 33}) add(CONTAINS, {slice(L, false, false)}, "contains" + std::to_string(L));
  for (const char* s : {"theodolites", "the ", "%", "_", "\\", "off_now\\", "\\%", "😀", "zzzzqqqq", "\\a", "a\\%", "w\\h"})
    add(CONTAINS, {B(s)}, std::string("contains:") + s);
  add(CONTAINS, {Bytes(1, 0xFF)}, "contains_ff"); add(CONTAINS, {Bytes(2, 0xFF)}, "contains_ffff");
  add(CONTAINS, {Bytes(1, 0x01)}, "contains_01"); add(CONTAINS, {Bytes(1, 0x00)}, "contains_nul");
  add(CONTAINS, {Bytes(600, 'a')}, "contains_longer_than_rows");
  add(SUFFIX, {Bytes(1, 0xFF)}, "suffix_ff"); add(SUFFIX, {Bytes(2, 0xFF)}, "suffix_ffff"); add(SUFFIX, {Bytes(3, 0xFF)}, "suffix_ff3");
  add(PREFIX, {Bytes(1, 0xFF)}, "prefix_ff"); add(PREFIX, {Bytes(2, 0xFF)}, "prefix_ffff");
  { Bytes n = B("end:"); n.push_back(0xFF); add(SUFFIX, {n}, "suffix_end_colon_ff"); n.push_back(0xFF); add(SUFFIX, {n}, "suffix_end_colon_ffff"); }
  add(PREFIX, {B("trailing\\")}, "prefix_trailing_bs_KNOWN_LIMIT"); add(PREFIX, {B("\\")}, "prefix_bs_only"); add(PREFIX, {B("%")}, "prefix_pct");
  add(SUFFIX, {B("\\")}, "suffix_bs"); add(SUFFIX, {B("\\%")}, "suffix_bs_pct"); add(SUFFIX, {B("g\\")}, "suffix_trailing_bs");
  add(MULTI, {slice(3, false, false), slice(2, false, false)}, "multi2");
  add(MULTI, {slice(4, false, false), slice(1, false, false), slice(5, false, false)}, "multi3");
  add(MULTI, {B("the"), B("of")}, "multi_the_of"); add(MULTI, {B("of"), B("the")}, "multi_of_the");
  add(MULTI, {B("the"), B("the")}, "multi_dup"); add(MULTI, {B("th"), B("he")}, "multi_overlap");
  add(MULTI, {B("a"), B("a"), B("a"), B("a")}, "multi_a4"); add(MULTI, {B("the"), B("%"), B("of")}, "multi_pct_mid");
  add(MULTI, {B("off"), B("now\\")}, "multi_last_trailing_bs"); add(MULTI, {B("now\\"), B("here")}, "multi_inner_trailing_bs");
  add(CONTAINS, {Bytes{}}, "contains_EMPTY"); add(MULTI, {Bytes{}, Bytes{}}, "multi_EMPTY_EMPTY");
  add(MULTI, {B("the"), Bytes{}, B("of")}, "multi_the_EMPTY_of"); add(PREFIX, {Bytes{}}, "prefix_EMPTY"); add(SUFFIX, {Bytes{}}, "suffix_EMPTY");
  return qs;
}
static std::vector<Bytes> load_rows(const char* path) {
  std::vector<Bytes> rows;
  FILE* f = std::fopen(path, "rb");
  if (!f) { perror(path); std::exit(kExitFault); }
  uint32_t cnt = 0;
  if (std::fread(&cnt, 4, 1, f) != 1) std::exit(kExitFault);
  for (uint32_t i = 0; i < cnt; i++) {
    uint32_t l = 0;
    if (std::fread(&l, 4, 1, f) != 1) std::exit(kExitFault);
    Bytes r(l);
    if (l && std::fread(r.data(), 1, l, f) != l) std::exit(kExitFault);
    rows.push_back(std::move(r));
  }
  std::fclose(f);
  return rows;
}
static std::vector<Query> fixture_queries() {
  std::vector<Query> qs;
  auto add = [&](Op op, const char* n, const char* label) { qs.push_back({op, {B(n)}, label}); };
  for (const char* n : {"e", "the", "he", "t", "cat", "goose", "se"}) add(SUFFIX, n, (std::string("suffix.") + n).c_str());
  add(PREFIX, "the", "prefix.the"); add(CONTAINS, "he", "contains.he");
  return qs;
}

// ------------------------------------------------------------- compression
struct Corpus {
  std::vector<Bytes> rows;
  Bytes comp;                    // concatenated compressed rows (heap, as the harness)
  std::vector<uint64_t> coff;    // rows+1 offsets into comp
  std::vector<size_t> clen;      // per-row compressed length
  std::unique_ptr<Encoder> encoder;
  bool ff_escaped = false;
  size_t maxlen = 0;
  const uint8_t* ptr(size_t i) const { return comp.data() + coff[i]; }
};
// fsst_create + fsst_compress with the raw rows flush against a guard page
// (END, then START): does the compressor itself read outside its input?
static fsst_encoder_t* compress_guarded(Corpus& c, std::vector<size_t>& len_out, std::vector<unsigned char*>& str_out, Bytes& outbuf) {
  const size_t n = c.rows.size();
  size_t payload = 0;
  for (const Bytes& r : c.rows) payload += r.size();
  Bytes concat; concat.reserve(payload);
  std::vector<size_t> roff(n + 1, 0);
  for (size_t i = 0; i < n; i++) { concat.insert(concat.end(), c.rows[i].begin(), c.rows[i].end()); roff[i + 1] = concat.size(); }
  GuardRegion in_end(payload), in_start(payload);
  std::vector<size_t> len_in(n); std::vector<const unsigned char*> str_in(n);
  outbuf.assign(7 * n + 2 * payload + 64, 0); len_out.assign(n, 0); str_out.assign(n, nullptr);
  fsst_encoder_t* keep = nullptr;
  for (int layout = 0; layout < 2; layout++) {
    uint8_t* base = layout == 0 ? in_end.place_end(concat.data(), payload) : in_start.place_start(concat.data(), payload);
    for (size_t i = 0; i < n; i++) { len_in[i] = c.rows[i].size(); str_in[i] = base + roff[i]; }
    fsst_encoder_t* e = nullptr; size_t nc = 0;
    bool ok = guarded([&] {
      e = fsst_create(n, len_in.data(), str_in.data(), 0);
      nc = fsst_compress(e, n, len_in.data(), str_in.data(), outbuf.size(), outbuf.data(), len_out.data(), str_out.data());
    });
    std::printf("input-guard layout=%s: %s (compressed %zu/%zu rows)\n", layout == 0 ? "END" : "START", ok ? "clean" : "FAULT", nc, n);
    if (!ok || nc != n) std::exit(kExitFault);
    if (layout == 0) fsst_destroy(e); else keep = e;
  }
  std::printf("payload=%zu\n", payload);
  return keep;
}
static void check_decode_roundtrip(const Corpus& c, fsst_encoder_t* enc) {
  fsst_decoder_t dec = fsst_decoder(enc);
  size_t bad = 0; Bytes tmp;
  for (size_t i = 0; i < c.rows.size(); i++) {
    tmp.assign(8 * c.clen[i] + 64, 0);
    size_t k = fsst_decompress(&dec, c.clen[i], c.ptr(i), tmp.size(), tmp.data());
    if (k != c.rows[i].size() || !std::equal(c.rows[i].begin(), c.rows[i].end(), tmp.begin())) bad++;
  }
  std::printf("decode round-trip mismatches: %zu\n", bad);
}
// Escaped-byte bitmap clobber + FSST-LIKE Encoder, as cand_build does it.
static void build_encoder(Corpus& c, fsst_encoder_t* enc) {
  std::shared_ptr<libfsst::SymbolTable> sym = reinterpret_cast<libfsst::Encoder*>(enc)->symbolTable;
  bool bitmap[256] = {false};
  for (size_t i = 0; i < c.rows.size(); i++) {
    const uint8_t* p = c.ptr(i);
    for (size_t j = 0; j < c.clen[i]; j++) if (p[j] == 255) bitmap[p[++j]] = true;
  }
  std::printf("nSymbols=%u escaped-bytes=%d\n", unsigned(sym->nSymbols), int(std::count(bitmap, bitmap + 256, true)));
  std::memcpy(&sym->symbols[255], bitmap, 256 * sizeof(bool));
  c.ff_escaped = bitmap[255];
  std::printf("0xFF escaped: %s\n", c.ff_escaped ? "yes" : "NO");
  SymbolTable st(sym);
  c.encoder = std::make_unique<Encoder>(st);
  fsst_destroy(enc);
  size_t invalid = 0;
  for (size_t i = 0; i < c.rows.size(); i++) if (!c.encoder->isEncodingValid(c.ptr(i), c.clen[i])) invalid++;
  std::printf("isEncodingValid failures: %zu\n", invalid);
}
static void print_ff_rows(const Corpus& c) {
  for (size_t i = 0; i < c.rows.size(); i++) {
    if (c.rows[i].empty() || c.rows[i].back() != 0xFF) continue;
    std::printf("row %zu rawlen=%zu comp-tail[", i, c.rows[i].size());
    for (size_t k = c.clen[i] > 4 ? c.clen[i] - 4 : 0; k < c.clen[i]; k++) std::printf(" %02x", c.ptr(i)[k]);
    std::printf(" ]  next row rawlen=%zu\n", i + 1 < c.rows.size() ? c.rows[i + 1].size() : 0);
  }
}
static Corpus compress_corpus(std::vector<Bytes> rows) {
  Corpus c; c.rows = std::move(rows);
  std::vector<size_t> len_out; std::vector<unsigned char*> str_out; Bytes outbuf;
  fsst_encoder_t* enc = compress_guarded(c, len_out, str_out, outbuf);
  const size_t n = c.rows.size();
  c.coff.assign(n + 1, 0); c.clen = len_out;
  for (size_t i = 0; i < n; i++) { c.coff[i + 1] = c.coff[i] + len_out[i]; c.maxlen = std::max(c.maxlen, len_out[i]); }
  c.comp.resize(c.coff[n]);
  for (size_t i = 0; i < n; i++) std::memcpy(c.comp.data() + c.coff[i], str_out[i], len_out[i]);
  std::printf("compressed=%zu\n", c.comp.size());
  check_decode_roundtrip(c, enc);
  build_encoder(c, enc);
  print_ff_rows(c);
  return c;
}

// ------------------------------------------------------------- backends
struct Backend { const char* name; int kind; bool simd; };  // kind: 0 interp, 1 cpp, 2 llvm
using ParseFn = std::function<bool(const uint8_t*, size_t)>;
struct Matcher {
  ParseFn parse;
  std::string setup_error;
  bool short_circuit_all = false;  // the candidate's all-empty-needles guard
  std::unique_ptr<automata::parsing::LikePatternAutomatonParser> interp;
  std::unique_ptr<automata::codegen::cpp::CppCompiler> cc;
#ifdef HAVE_LLVM
  std::unique_ptr<automata::codegen::llvmir::LLVMCompiler> lc;
#endif
  std::unique_ptr<automata::codegen::Parser> gen;
};
static std::unique_ptr<Matcher> build_matcher(const Backend& b, const Query& q, const Bytes& pat, const Encoder& encoder) {
  auto m = std::make_unique<Matcher>();
  try {
    auto span = std::span<const uint8_t>(pat.data(), pat.size());
    if (b.kind == 0) {
      m->interp = std::make_unique<automata::parsing::LikePatternAutomatonParser>(span, encoder);
      auto* ip = m->interp.get();
      m->parse = [ip](const uint8_t* p, size_t l) { return ip->parse(std::span<const uint8_t>(p, l)); };
    } else if (all_empty(q)) {
      m->short_circuit_all = true;
    } else {
      auto automaton = automata::parsing::LikePatternAutomaton::build(span, encoder);
      if (b.kind == 1) {
        static int counter = 0;
        std::string stem = "gen_" + std::to_string(getpid()) + "_" + std::to_string(++counter);
        m->cc = std::make_unique<automata::codegen::cpp::CppCompiler>(stem + ".cpp", stem + ".so", b.simd);
        m->gen = m->cc->compile(automaton);
      }
#ifdef HAVE_LLVM
      else { m->lc = std::make_unique<automata::codegen::llvmir::LLVMCompiler>(b.simd); m->gen = m->lc->compile(automaton); }
#endif
      auto* gp = m->gen.get();
      m->parse = [gp](const uint8_t* p, size_t l) { return gp->parse(p, l); };
    }
  } catch (const std::exception& e) { m->setup_error = e.what(); } catch (...) { m->setup_error = "unknown exception"; }
  return m;
}

// ------------------------------------------------------------- evaluation
struct CellResult {
  size_t fn = 0, fp = 0, faults_end = 0, faults_start = 0, faults_heap = 0;
  size_t need_after = 0, need_before = 0;  // slack that makes the fault go away
  size_t fill_dep = 0;                     // rows whose answer depends on out-of-row bytes
  std::string first_bad, notes;
  bool clean() const { return fn + fp + faults_end + faults_start + faults_heap + fill_dep == 0; }
};
struct Placements { GuardRegion end, start; };
static std::string row_preview(const Bytes& r) {
  std::string s;
  for (size_t k = 0; k < std::min<size_t>(r.size(), 40); k++) {
    uint8_t c = r[k];
    if (c >= 32 && c < 127) s.push_back(char(c)); else { char b[8]; std::snprintf(b, sizeof b, "\\x%02x", c); s += b; }
  }
  return s;
}
static void note_first(CellResult& res, const Corpus& c, size_t i, const char* kind) {
  if (!res.first_bad.empty()) return;
  char buf[160];
  std::snprintf(buf, sizeof buf, "first=%s row=%zu rawlen=%zu complen=%zu raw=\"", kind, i, c.rows[i].size(), c.clen[i]);
  res.first_bad = std::string(buf) + row_preview(c.rows[i]) + "\"";
}
// Smallest slack in [1, kMaxSlackProbe] at which the placement stops faulting.
template <class Place> static size_t probe_slack(const ParseFn& parse, size_t len, Place&& place) {
  for (size_t k = 1; k <= kMaxSlackProbe; k++) {
    uint8_t* pk = place(k);
    if (guarded([&] { (void)parse(pk, len); })) return k;
  }
  return kMaxSlackProbe + 1;
}
// Which values of the single byte before/after the row flip the answer?
static std::string sweep_neighbour(const ParseFn& parse, GuardRegion& g, const uint8_t* p, size_t len, bool expect, bool before) {
  uint8_t* pm = g.place_mid(p, len, 0);
  std::string flips;
  for (int v = 0; v < 256; v++) {
    if (before) pm[-1] = uint8_t(v); else { pm[len] = uint8_t(v); pm[len + 1] = uint8_t(v); }
    bool got = false;
    if (guarded([&] { got = parse(pm, len); }) && got != expect) { char b[8]; std::snprintf(b, sizeof b, " %02x", v); flips += b; }
  }
  return flips;
}
static void evaluate_row(const Corpus& c, size_t i, const ParseFn& parse, bool expect, Placements& pl, bool do_guard, CellResult& res) {
  const uint8_t* p = c.ptr(i); const size_t len = c.clen[i];
  bool got = false;
  if (!guarded([&] { got = parse(p, len); })) { res.faults_heap++; note_first(res, c, i, "FAULT-heap"); return; }
  if (do_guard) {
    uint8_t* pe = pl.end.place_end(p, len); bool ge = false;
    if (!guarded([&] { ge = parse(pe, len); })) {
      res.faults_end++; note_first(res, c, i, "FAULT-over-read(END)");
      res.need_after = std::max(res.need_after, probe_slack(parse, len, [&](size_t k) { return pl.end.place_end(p, len, k); }));
      if (res.notes.find("after-flips") == std::string::npos) res.notes += " after-flips:[" + sweep_neighbour(parse, pl.end, p, len, expect, false) + " ]";
    } else if (ge != got) res.notes += " NONDETERMINISTIC(END)";
    uint8_t* ps = pl.start.place_start(p, len); bool gs = false;
    if (!guarded([&] { gs = parse(ps, len); })) {
      res.faults_start++; note_first(res, c, i, "FAULT-under-read(START)");
      res.need_before = std::max(res.need_before, probe_slack(parse, len, [&](size_t k) { return pl.start.place_start(p, len, k); }));
      if (res.notes.find("before-flips") == std::string::npos) res.notes += " before-flips:[" + sweep_neighbour(parse, pl.end, p, len, expect, true) + " ]";
    } else if (gs != got) res.notes += " NONDETERMINISTIC(START)";
    for (uint8_t fill : {uint8_t(0x00), uint8_t(0xFF), uint8_t(0x41)}) {
      uint8_t* pm = pl.end.place_mid(p, len, fill); bool gm = false;
      if (guarded([&] { gm = parse(pm, len); }) && gm != expect) { res.fill_dep++; break; }
    }
  }
  if (got && !expect) { res.fp++; note_first(res, c, i, "false-positive"); }
  if (!got && expect) { res.fn++; note_first(res, c, i, "false-negative"); }
}
static CellResult evaluate_cell(const Corpus& c, const Matcher& m, const std::vector<char>& expect, Placements& pl, bool do_guard) {
  CellResult res;
  if (m.short_circuit_all) {
    for (size_t i = 0; i < c.rows.size(); i++) if (!expect[i]) res.fp++;
    return res;
  }
  for (size_t i = 0; i < c.rows.size(); i++) evaluate_row(c, i, m.parse, expect[i], pl, do_guard, res);
  return res;
}
static void print_cell(const Backend& b, const Matcher& m, const CellResult& r) {
  std::printf("  %-9s %s  fn=%zu fp=%zu faults(end=%zu,start=%zu,heap=%zu) slack(after=%zu,before=%zu) fill_dep=%zu%s%s %s\n",
              b.name, r.clean() ? "ok " : "BAD", r.fn, r.fp, r.faults_end, r.faults_start, r.faults_heap,
              r.need_after, r.need_before, r.fill_dep, r.notes.c_str(), m.short_circuit_all ? " [short-circuit]" : "", r.first_bad.c_str());
}

// ------------------------------------------------------------- main
int main(int argc, char** argv) {
  bool do_guard = true, probe_only = false; const char* rows_file = nullptr;
  for (int a = 1; a < argc; a++) {
    std::string arg = argv[a];
    if (arg == "--no-guard") do_guard = false;
    else if (arg == "--probe-only") probe_only = true;
    else if (arg == "--rows" && a + 1 < argc) rows_file = argv[++a];
  }
  install_fault_trap();
  Gen gen;
  std::vector<Bytes> rows = rows_file ? load_rows(rows_file) : make_corpus(gen);
  std::vector<Query> queries = rows_file ? fixture_queries() : make_queries(rows, gen);
  std::printf("rows=%zu queries=%zu sizeof(Symbol)=%zu\n", rows.size(), queries.size(), sizeof(libfsst::Symbol));
  Corpus corpus = compress_corpus(std::move(rows));
  if (probe_only) return corpus.ff_escaped ? 0 : kExitNotEscaped;

#ifdef HAVE_LLVM
  llvm::InitializeNativeTarget(); llvm::InitializeNativeTargetAsmPrinter();
#endif
  std::vector<Backend> backends = {{"interp", 0, false}, {"cpp", 1, false}, {"cpp-simd", 1, true}};
#ifdef HAVE_LLVM
  backends.push_back({"llvm", 2, false}); backends.push_back({"llvm-simd", 2, true});
#endif
  Placements pl{GuardRegion(corpus.maxlen + 2 * kFillPad + kMaxSlackProbe), GuardRegion(corpus.maxlen + kMaxSlackProbe)};
  std::vector<char> expect(corpus.rows.size());
  int problem_cells = 0;
  for (const Query& q : queries) {
    for (size_t i = 0; i < corpus.rows.size(); i++) expect[i] = oracle(corpus.rows[i], q);
    Bytes pat = to_like(q);
    std::printf("\n[%s] pattern=\"%s\" expected=%zu\n", q.label.c_str(), std::string(pat.begin(), pat.end()).c_str(), size_t(std::count(expect.begin(), expect.end(), 1)));
    for (const Backend& b : backends) {
      std::unique_ptr<Matcher> m = build_matcher(b, q, pat, *corpus.encoder);
      if (!m->setup_error.empty()) { std::printf("  %-9s SETUP-EXCEPTION: %s\n", b.name, m->setup_error.c_str()); problem_cells++; continue; }
      CellResult r = evaluate_cell(corpus, *m, expect, pl, do_guard);
      if (!r.clean()) problem_cells++;
      print_cell(b, *m, r);
    }
  }
  std::printf("\nTOTAL PROBLEM CELLS: %d\n", problem_cells);
  return problem_cells ? 1 : 0;
}
