# Author-owned build for the `fsst_like` candidate (DESIGN.md §17): the DaMoN'26
# FSST-LIKE-Matching compressed-domain matcher. The harness build invokes this
# via the `cmake` crate from build.rs and links the static library.
#
# We fetch three deps pinned for reproducibility and compile only the curated,
# LLVM/vectorscan-FREE interpreted subset (codegen/vectorscan backends land in a
# later milestone). Because this candidate statically links its own FSST copy
# (calin2110 fork) while the `fsst` candidate links cwida upstream — and both
# forks export the identical `fsst_*` / `libfsst::` symbols — we LOCALIZE every
# symbol here except the single lb_candidate_fsst_like entry point, so the two
# archives do not collide at the final harness link.
cmake_minimum_required(VERSION 3.21)
project(lb_fsst_like CXX)

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
FetchContent_Declare(fsst_like_src
    GIT_REPOSITORY https://github.com/calin2110/FSST-LIKE-Matching.git
    GIT_TAG        b1eb3ab9c63ea0199a381b92371a3154190b4406
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

# Curated interpreted subset — no codegen/*, no vectorscan/hybrid_string_search,
# no serializer/utils (file I/O the matcher does not need).
add_library(fl_objs OBJECT
    fsst_like_candidate.cpp
    ${fsst_like_src_SOURCE_DIR}/src/encoder.cpp
    ${fsst_like_src_SOURCE_DIR}/src/automata.cpp
    ${fsst_like_src_SOURCE_DIR}/src/like_pattern_automaton.cpp
    ${fsst_like_src_SOURCE_DIR}/src/pattern.cpp
    ${fsst_like_src_SOURCE_DIR}/src/percentage.cpp
    ${fsst_like_src_SOURCE_DIR}/src/common.cpp
    ${fsst_fork_SOURCE_DIR}/libfsst.cpp
    ${fsst_fork_SOURCE_DIR}/fsst_avx512.cpp
)
target_include_directories(fl_objs PRIVATE
    ${fsst_like_src_SOURCE_DIR}/include
    ${GEN_INC}
    ${fmt_ext_SOURCE_DIR}/include
    ${CMAKE_CURRENT_SOURCE_DIR}/../../../contract
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
set(COMBINED ${CMAKE_CURRENT_BINARY_DIR}/lb_fsst_like_combined${CMAKE_CXX_OUTPUT_EXTENSION})
if(APPLE)
    add_custom_command(OUTPUT ${COMBINED}
        COMMAND ld -r -exported_symbol _lb_candidate_fsst_like
                $<TARGET_OBJECTS:fl_objs> -o ${COMBINED}
        DEPENDS fl_objs COMMAND_EXPAND_LISTS VERBATIM)
else()
    add_custom_command(OUTPUT ${COMBINED}
        COMMAND ${CMAKE_LINKER} -r $<TARGET_OBJECTS:fl_objs> -o ${COMBINED}
        COMMAND objcopy --keep-global-symbol=lb_candidate_fsst_like ${COMBINED}
        DEPENDS fl_objs COMMAND_EXPAND_LISTS VERBATIM)
endif()
set_source_files_properties(${COMBINED} PROPERTIES EXTERNAL_OBJECT TRUE GENERATED TRUE)

add_library(lb_fsst_like STATIC ${COMBINED})
set_target_properties(lb_fsst_like PROPERTIES LINKER_LANGUAGE CXX)

install(TARGETS lb_fsst_like ARCHIVE DESTINATION lib)
