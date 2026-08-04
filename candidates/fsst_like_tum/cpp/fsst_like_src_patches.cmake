# FetchContent PATCH_COMMAND script for FSST-LIKE-Matching: the ONLY source
# deviations from the pinned upstream (b1eb3ab9). Every patch below is applied
# via an explicit, independent string(FIND)/string(REPLACE) pair — deliberately
# NOT via a CMake list + foreach(), because these match strings contain literal
# `;` (end of a C++ statement); CMake lists are `;`-joined strings, so
# foreach() over a list silently re-splits any element containing one,
# truncating it. (Discovered the hard way: a 6-tab template fragment produced
# by that split re-matched inside an already-patched 7-tab line, since 6 tabs
# is a shifted substring of 7 tabs — a double "ull" suffix on one line only.)
# Each block below is self-contained and naturally idempotent: once applied,
# its unsuffixed match text no longer exists to re-match on a later reconfigure.

# --- Patch 1: LLVM 14 compat -------------------------------------------------
# Upstream targets LLVM 16, where `LLJIT::lookup()` yields an `ExecutorAddr`
# with `toPtr<T>()`; on LLVM 14 that accessor does not exist
# (`JITEvaluatedSymbol::getAddress()` + cast is the spelling). Wrap that one
# line in an LLVM_VERSION_MAJOR guard so the pin builds on LLVM 14–16 unchanged.
set(LLVM_FILE src/codegen/llvmcodegen.cpp)
file(READ ${LLVM_FILE} llvm_content)
# Check the "already patched" marker FIRST: LLVM_ORIG stays present verbatim
# INSIDE the #if branch after patching, so searching for it directly (without
# checking this guard first) would match forever and re-wrap on every
# reconfigure — CMake reruns PATCH_COMMAND on every reconfigure of an
# already-populated FetchContent source, not just on first clone.
string(FIND "${llvm_content}" "LLVM_VERSION_MAJOR >= 15" llvm_already)
if(llvm_already EQUAL -1)
    set(LLVM_ORIG "    parseFunction = cantFail(this->JIT->lookup(Compiler::getParseFunctionName())).toPtr<bool (*) (const uint8_t*, size_t)>();")
    string(FIND "${llvm_content}" "${LLVM_ORIG}" llvm_pos)
    if(NOT llvm_pos EQUAL -1)
        set(LLVM_REPL "#if LLVM_VERSION_MAJOR >= 15
${LLVM_ORIG}
#else
    parseFunction = reinterpret_cast<bool (*) (const uint8_t*, size_t)>(
        cantFail(this->JIT->lookup(Compiler::getParseFunctionName())).getAddress());
#endif")
        string(REPLACE "${LLVM_ORIG}" "${LLVM_REPL}" llvm_content "${llvm_content}")
        file(WRITE ${LLVM_FILE} "${llvm_content}")
    else()
        message(FATAL_ERROR "fsst_like_src_patches.cmake: expected toPtr line not found in ${LLVM_FILE} — upstream pin changed; re-verify patch 1")
    endif()
endif()

# --- Patch 2: unsuffixed uint64_t literals in generated kernels -------------
# CppCompiler emits automaton State::level (uint64_t; SIZE_MAX is its "no
# level" sentinel) into generated .cpp kernels via fmt::format("{}", ...) with
# NO integer-literal suffix — unlike the prefix/suffix sites in this same file,
# which correctly use "{}ull". A sentinel-valued level then emits the bare
# decimal for 2^64-1 into the generated kernel, which clang flags as
# -Wimplicitly-unsigned-literal on every compile (cosmetic only: the value is
# correct, just untyped). Add the missing "ull" suffix at every such site,
# matching the style the correct sites already use.
# NOTE: \\t / \\n below are literal backslash-letter pairs (the source file's
# on-disk escape sequences inside a C++ string literal), not control chars.
set(CPP_FILE src/codegen/cppcodegen.cpp)
file(READ ${CPP_FILE} cpp_content)
set(cpp_patched_any FALSE)

set(CPP_S1 "\\tuint64_t level = {};\\n")
string(FIND "${cpp_content}" "${CPP_S1}" cpp_p1)
if(NOT cpp_p1 EQUAL -1)
    set(cpp_patched_any TRUE)
    string(REPLACE "${CPP_S1}" "\\tuint64_t level = {}ull;\\n" cpp_content "${cpp_content}")
endif()

set(CPP_S2 "\\t\\t\\t\\t\\t\\t\\tlevel = {};\\n")
string(FIND "${cpp_content}" "${CPP_S2}" cpp_p2)
if(NOT cpp_p2 EQUAL -1)
    set(cpp_patched_any TRUE)
    string(REPLACE "${CPP_S2}" "\\t\\t\\t\\t\\t\\t\\tlevel = {}ull;\\n" cpp_content "${cpp_content}")
endif()

set(CPP_S3 "\\t\\t\\t\\t\\t\\tlevel = {};\\n")
string(FIND "${cpp_content}" "${CPP_S3}" cpp_p3)
if(NOT cpp_p3 EQUAL -1)
    set(cpp_patched_any TRUE)
    string(REPLACE "${CPP_S3}" "\\t\\t\\t\\t\\t\\tlevel = {}ull;\\n" cpp_content "${cpp_content}")
endif()

set(CPP_S4 "\\tif (len < {})\\n")
string(FIND "${cpp_content}" "${CPP_S4}" cpp_p4)
if(NOT cpp_p4 EQUAL -1)
    set(cpp_patched_any TRUE)
    string(REPLACE "${CPP_S4}" "\\tif (len < {}ull)\\n" cpp_content "${cpp_content}")
endif()

set(CPP_S5 "\\tif (!canParse || strIdx + 1 < {})\\n")
string(FIND "${cpp_content}" "${CPP_S5}" cpp_p5)
if(NOT cpp_p5 EQUAL -1)
    set(cpp_patched_any TRUE)
    string(REPLACE "${CPP_S5}" "\\tif (!canParse || strIdx + 1 < {}ull)\\n" cpp_content "${cpp_content}")
endif()

if(cpp_patched_any)
    file(WRITE ${CPP_FILE} "${cpp_content}")
else()
    string(FIND "${cpp_content}" "\\tuint64_t level = {}ull;\\n" cpp_already)
    if(cpp_already EQUAL -1)
        message(FATAL_ERROR "fsst_like_src_patches.cmake: none of the expected level/len format sites found in ${CPP_FILE} — upstream pin changed; re-verify patch 2")
    endif()
endif()
