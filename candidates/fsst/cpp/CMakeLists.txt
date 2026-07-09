# Author-owned build for the `fsst` candidate (standard FSST, decode-only).
# Wraps cwida/fsst upstream (the canonical reference implementation), pinned to
# a commit for reproducibility. The harness build invokes this via the `cmake`
# crate from build.rs and links the static library.
cmake_minimum_required(VERSION 3.21)
project(lb_fsst CXX)

include(FetchContent)
# Populate-only: point SOURCE_SUBDIR at a nonexistent dir so MakeAvailable does
# NOT add_subdirectory() cwida's CMakeLists (which would also build its `fsst`
# CLI binaries + the fsst12 variant). We compile only the library sources we
# need directly into lb_fsst.
FetchContent_Declare(
    fsst_upstream
    GIT_REPOSITORY https://github.com/cwida/fsst.git
    GIT_TAG        e638d4cf8c26129d73c242a4127b42b975de5b63
    SOURCE_SUBDIR  __lb_no_configure__
)
FetchContent_MakeAvailable(fsst_upstream)

include(CheckCXXCompilerFlag)
# Host-native codegen (the measured artifact runs on this host) — this is also
# how cwida's own CMake and the onpair candidate build. On Apple clang/arm64
# `-march=native` is unsupported, so the guard drops it and FSST's arm64 scalar
# path (fsst_hasAVX512()==false) is used; on x86 with AVX-512 the real SIMD
# compress path (guarded by __AVX512F__) is compiled, matching the runtime probe.
check_cxx_compiler_flag("-march=native" LB_FSST_MARCH_NATIVE)

add_library(lb_fsst STATIC
    fsst_candidate.cpp
    ${fsst_upstream_SOURCE_DIR}/libfsst.cpp
    ${fsst_upstream_SOURCE_DIR}/fsst_avx512.cpp
)
target_include_directories(lb_fsst PRIVATE
    ${fsst_upstream_SOURCE_DIR}
    ${CMAKE_CURRENT_SOURCE_DIR}/../../../contract
)
set_target_properties(lb_fsst PROPERTIES CXX_STANDARD 20 CXX_STANDARD_REQUIRED ON)
target_compile_options(lb_fsst PRIVATE
    -O3 "$<$<BOOL:${LB_FSST_MARCH_NATIVE}>:-march=native>"
)
# cwida's own workaround: fsst_avx512.cpp must be built at -O1 (aggressive
# unrolling at -O2/-O3 spills the AVX-512 registers and slows the loop). The
# per-source flag appends after the target flags, so the trailing -O1 wins while
# -march=native (needed to define __AVX512F__) is preserved.
set_source_files_properties(${fsst_upstream_SOURCE_DIR}/fsst_avx512.cpp
    PROPERTIES COMPILE_FLAGS "-O1")

install(TARGETS lb_fsst ARCHIVE DESTINATION lib)
