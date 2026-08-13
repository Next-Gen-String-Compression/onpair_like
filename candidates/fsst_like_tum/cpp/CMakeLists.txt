# Author-owned build for the `fsst_like_tum` candidate (DESIGN.md §17): the DaMoN'26
# FSST-LIKE-Matching compressed-domain matcher. The harness build invokes this
# via the `cmake` crate from build.rs and links the static library.
#
# We fetch three deps pinned for reproducibility and compile only the curated,
# LLVM/vectorscan-FREE interpreted subset (codegen/vectorscan backends land in a
# later milestone). Because this candidate statically links its own FSST copy
# (calin2110 fork) while the `fsst` candidate links cwida upstream — and both
# forks export the identical `fsst_*` / `libfsst::` symbols — we LOCALIZE every
# symbol here except the single lb_candidate_fsst_like_tum entry point, so the two
# archives do not collide at the final harness link.
cmake_minimum_required(VERSION 3.21)
# C is enabled solely because LLVM's CMake config (FindFFI/FindTerminfo)
# runs check_c_source_compiles; the candidate itself is C++ only.
project(lb_fsst_like_tum C CXX)

include(FetchContent)
# All three populated source-only (SOURCE_SUBDIR points at a nonexistent dir so
# MakeAvailable does not add_subdirectory their CMakeLists); we pick the sources.
FetchContent_Declare(fmt_ext
    GIT_REPOSITORY https://github.com/fmtlib/fmt.git
    GIT_TAG        12.1.0
    SOURCE_SUBDIR  __lb_no_configure__)
FetchContent_Declare(fsst_fork
    GIT_REPOSITORY https://github.com/calin2110/fsst.git
    GIT_TAG        1755328b61f4e48ab7f53b315bbb20e8130059f2
    SOURCE_SUBDIR  __lb_no_configure__)
# PATCH_COMMAND: two small source deviations from the pin — see
# fsst_like_src_patches.cmake (LLVM 14 compat + a cosmetic codegen-kernel
# warning fix); a no-op for anything neither patch touches.
FetchContent_Declare(fsst_like_src
    GIT_REPOSITORY https://github.com/calin2110/FSST-LIKE-Matching.git
    GIT_TAG        b1eb3ab9c63ea0199a381b92371a3154190b4406
    PATCH_COMMAND  ${CMAKE_COMMAND} -P ${CMAKE_CURRENT_SOURCE_DIR}/fsst_like_src_patches.cmake
    SOURCE_SUBDIR  __lb_no_configure__)
FetchContent_MakeAvailable(fmt_ext fsst_fork fsst_like_src)

# encoder.hpp does #include <fsst/libfsst.hpp>; create an include dir with an
# `fsst` symlink into the fsst fork so that resolves (mirrors the repo's CMake).
set(GEN_INC ${CMAKE_CURRENT_BINARY_DIR}/gen_include)
file(MAKE_DIRECTORY ${GEN_INC})
if(NOT EXISTS ${GEN_INC}/fsst)
    file(CREATE_LINK ${fsst_fork_SOURCE_DIR} ${GEN_INC}/fsst SYMBOLIC)
endif()

include(CheckCXXCompilerFlag)
check_cxx_compiler_flag("-march=native" LB_FL_MARCH_NATIVE)

# LLVM for the llvm/llvm-simd ORC-JIT backends. Upstream pins LLVM 16, but the
# codegen source only uses APIs present in 14–16 and REMOVED in 17+
# (PassManagerBuilder.h, Type::getInt8PtrTy, Support/Host.h), so accept that
# range and reject anything newer (a 17+ install would fail to compile).
# Absent/too-new LLVM ⇒ the llvm* strategies are absent, not a build error.
if(NOT DEFINED LLVM_DIR AND EXISTS /usr/lib/llvm-14/lib/cmake/llvm)
    set(LLVM_DIR /usr/lib/llvm-14/lib/cmake/llvm)
endif()
find_package(LLVM CONFIG QUIET)
set(LB_FL_HAVE_LLVM FALSE)
if(LLVM_FOUND)
    if(LLVM_VERSION_MAJOR GREATER_EQUAL 14 AND LLVM_VERSION_MAJOR LESS_EQUAL 16)
        set(LB_FL_HAVE_LLVM TRUE)
        message(STATUS "fsst_like_tum: LLVM ${LLVM_PACKAGE_VERSION} found — llvm/llvm-simd backends enabled")
    else()
        message(STATUS "fsst_like_tum: LLVM ${LLVM_PACKAGE_VERSION} found but outside the supported 14–16 range — llvm backends disabled")
    endif()
else()
    message(STATUS "fsst_like_tum: no LLVM 14–16 — llvm backends disabled")
endif()

# Curated subset: the interpreted matcher plus the cpp codegen backend (and the
# LLVM backend when available) — no vectorscan/hybrid_string_search, no
# serializer/utils (file I/O the matcher does not need; cppcodegen uses only the
# header-only loadUnaligned<T> from utils.hpp).
add_library(fl_objs OBJECT
    fsst_like_tum_candidate.cpp
    ${fsst_like_src_SOURCE_DIR}/src/encoder.cpp
    ${fsst_like_src_SOURCE_DIR}/src/automata.cpp
    ${fsst_like_src_SOURCE_DIR}/src/like_pattern_automaton.cpp
    ${fsst_like_src_SOURCE_DIR}/src/pattern.cpp
    ${fsst_like_src_SOURCE_DIR}/src/percentage.cpp
    ${fsst_like_src_SOURCE_DIR}/src/common.cpp
    ${fsst_like_src_SOURCE_DIR}/src/codegen/codegen.cpp
    ${fsst_like_src_SOURCE_DIR}/src/codegen/cppcodegen.cpp
    ${fsst_fork_SOURCE_DIR}/libfsst.cpp
    ${fsst_fork_SOURCE_DIR}/fsst_avx512.cpp
)
if(LB_FL_HAVE_LLVM)
    target_sources(fl_objs PRIVATE
        ${fsst_like_src_SOURCE_DIR}/src/codegen/llvmcodegen.cpp)
    target_compile_definitions(fl_objs PRIVATE HAVE_LLVM ${LLVM_DEFINITIONS})
    target_include_directories(fl_objs PRIVATE ${LLVM_INCLUDE_DIRS})
endif()
target_include_directories(fl_objs PRIVATE
    ${fsst_like_src_SOURCE_DIR}/include
    ${GEN_INC}
    ${fmt_ext_SOURCE_DIR}/include
    ${CMAKE_CURRENT_SOURCE_DIR}/../../../contract
    ${CMAKE_CURRENT_SOURCE_DIR}/../../common/fsst_common
)
target_compile_definitions(fl_objs PRIVATE FMT_HEADER_ONLY)
set_target_properties(fl_objs PROPERTIES
    CXX_STANDARD 20 CXX_STANDARD_REQUIRED ON POSITION_INDEPENDENT_CODE ON)
# -fvisibility=hidden so the localize step keeps only the default-visibility
# entry point global. FSST-LIKE forgot <unistd.h> in encoder.cpp (compiles on
# their Linux GCC via transitive includes); force-include it. Deprecation is the
# basic_string<uint8_t> warning; return-type is the always-covered shift switch.
target_compile_options(fl_objs PRIVATE
    -O3 "$<$<BOOL:${LB_FL_MARCH_NATIVE}>:-march=native>"
    -fvisibility=hidden -Wno-deprecated-declarations -Wno-return-type
    "SHELL:-include unistd.h"
)
# cwida/calin workaround: fsst_avx512.cpp must be -O1 (unrolling spills AVX-512
# registers at higher -O). Trailing -O wins; -march=native is preserved.
set_source_files_properties(${fsst_fork_SOURCE_DIR}/fsst_avx512.cpp
    PROPERTIES COMPILE_FLAGS "-O1")

# Combine all objects into one relocatable object, exporting ONLY the entry
# point; every FSST / FSST-LIKE symbol becomes local, so this archive cannot
# clash with the `fsst` candidate's cwida FSST symbols.
set(COMBINED ${CMAKE_CURRENT_BINARY_DIR}/lb_fsst_like_tum_combined${CMAKE_CXX_OUTPUT_EXTENSION})
if(APPLE)
    add_custom_command(OUTPUT ${COMBINED}
        COMMAND ld -r -exported_symbol _lb_candidate_fsst_like_tum
                $<TARGET_OBJECTS:fl_objs> -o ${COMBINED}
        DEPENDS fl_objs COMMAND_EXPAND_LISTS VERBATIM)
else()
    # Combine all objects into one relocatable object, then localize the
    # calin2110 FSST-fork symbols so they can't clash with the `fsst` candidate's
    # cwida FSST symbols at the final harness link. localize.sh localizes only
    # strong-global (non-COMDAT) defs and leaves weak COMDAT symbols (C++
    # templates, vtables, typeinfo, inline fsst_decompress, libstdc++ machinery)
    # intact — a blanket `objcopy --keep-global-symbol=<entry>` demotes those weak
    # COMDAT symbols to local, corrupting their group sections (lld tolerates it
    # but crashes the co-linked fsst candidate; bfd refuses). See localize.sh.
    add_custom_command(OUTPUT ${COMBINED}
        COMMAND ${CMAKE_LINKER} -r $<TARGET_OBJECTS:fl_objs> -o ${COMBINED}
        COMMAND sh ${CMAKE_CURRENT_SOURCE_DIR}/localize.sh ${COMBINED}
        DEPENDS fl_objs ${CMAKE_CURRENT_SOURCE_DIR}/localize.sh COMMAND_EXPAND_LISTS VERBATIM)
endif()
set_source_files_properties(${COMBINED} PROPERTIES EXTERNAL_OBJECT TRUE GENERATED TRUE)

add_library(lb_fsst_like_tum STATIC ${COMBINED})
set_target_properties(lb_fsst_like_tum PROPERTIES LINKER_LANGUAGE CXX)

install(TARGETS lb_fsst_like_tum ARCHIVE DESTINATION lib)

# Tell build.rs how to link LLVM into the final harness binary (the archive
# above only *references* LLVM symbols; the library itself is linked at the
# Rust level). Line 1: link-search dir; remaining lines: dylib names. The
# shared libLLVM-<major>.so keeps the harness link simple and symbol-clash-free
# (candidate objects are localized; LLVM stays a normal shared dep). build.rs
# links LLVM only when this file exists, so compile-time HAVE_LLVM and the
# final link always agree.
if(LB_FL_HAVE_LLVM)
    file(WRITE ${CMAKE_CURRENT_BINARY_DIR}/lb_llvm_link.txt
        "${LLVM_LIBRARY_DIRS}\nLLVM-${LLVM_VERSION_MAJOR}\n")
    install(FILES ${CMAKE_CURRENT_BINARY_DIR}/lb_llvm_link.txt DESTINATION lib)
endif()
