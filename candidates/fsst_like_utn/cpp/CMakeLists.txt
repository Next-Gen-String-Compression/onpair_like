# Author-owned build for the `fsst_like_utn` candidate (DESIGN.md §17): the
# utndatasystems/fsst-like "Comet" compressed-domain LIKE matcher. Unlike the
# other C++ candidates, the sources are VENDORED as a git submodule at
# vendor/fsst-like (checked out locally, pinned by submodule SHA) rather than
# fetched — so we compile straight from that tree.
#
# We compile only the curated matcher subset (StateMachine / MetaStateMachine /
# FsstDecoder / Utility are header-only or trivial TUs) plus the repo's own FSST
# fork (alexandervanrenen). Because this candidate statically links its own FSST
# copy while the `fsst` (cwida) and `fsst_like_tum` (calin2110) candidates link
# theirs — and all forks export the identical fsst_* symbols — we LOCALIZE every
# symbol here except the single lb_candidate_fsst_like_utn entry point, so the
# archives do not collide at the final harness link.
cmake_minimum_required(VERSION 3.21)
project(lb_fsst_like_utn CXX)

set(VENDOR ${CMAKE_CURRENT_SOURCE_DIR}/../vendor/fsst-like)
if(NOT EXISTS ${VENDOR}/src/StateMachine.hpp)
    message(FATAL_ERROR
        "vendor/fsst-like is empty — run: git submodule update --init "
        "candidates/fsst_like_utn/vendor/fsst-like (and its nested `fsst` submodule)")
endif()

include(CheckCXXCompilerFlag)
check_cxx_compiler_flag("-march=native" LB_UTN_MARCH_NATIVE)

# Curated matcher subset. FsstWrapper.cpp carries the real FsstDecoder impl;
# StateMachine.cpp / MetaStateMachine.cpp / Utility.cpp are effectively empty
# (all logic is header-only) so we skip them. libfsst.cpp + fsst_avx512.cpp are
# the fork's compression core (used by the shared fsst_common::Build front-end).
add_library(utn_objs OBJECT
    fsst_like_utn_candidate.cpp
    ${VENDOR}/src/FsstWrapper.cpp
    ${VENDOR}/fsst/libfsst.cpp
    ${VENDOR}/fsst/fsst_avx512.cpp
)
target_include_directories(utn_objs PRIVATE
    ${VENDOR}/fsst          # FsstWrapper.hpp does #include "fsst.h"
    ${VENDOR}/src           # StateMachine.hpp / FsstWrapper.hpp / Utility.hpp
    ${CMAKE_CURRENT_SOURCE_DIR}/../../../contract
    ${CMAKE_CURRENT_SOURCE_DIR}/../../common/fsst_common
)
set_target_properties(utn_objs PROPERTIES
    CXX_STANDARD 20 CXX_STANDARD_REQUIRED ON POSITION_INDEPENDENT_CODE ON)
# -fvisibility=hidden so the localize step keeps only the default-visibility
# entry point global.
target_compile_options(utn_objs PRIVATE
    -O3 "$<$<BOOL:${LB_UTN_MARCH_NATIVE}>:-march=native>"
    -fvisibility=hidden -Wno-deprecated-declarations
)
# cwida/alexandervanrenen workaround: fsst_avx512.cpp must be -O1 (unrolling
# spills AVX-512 registers at higher -O). Trailing -O wins; -march=native is
# preserved (needed to define __AVX512F__ on x86).
set_source_files_properties(${VENDOR}/fsst/fsst_avx512.cpp
    PROPERTIES COMPILE_FLAGS "-O1")

# Combine all objects into one relocatable object, exporting ONLY the entry
# point; every FSST / UTN symbol becomes local, so this archive cannot clash
# with the other candidates' FSST symbols. Mirrors fsst_like_tum exactly: on
# macOS `ld -r -exported_symbol` keeps just the entry point; on Linux we combine
# then localize by binding via localize.sh (leaves weak COMDAT groups intact).
set(COMBINED ${CMAKE_CURRENT_BINARY_DIR}/lb_fsst_like_utn_combined${CMAKE_CXX_OUTPUT_EXTENSION})
# DEPENDS names the object files as well as the target: a bare target name is
# only an ordering dependency, so an edited source would recompile the objects
# and still leave this combined object -- and the archive built from it --
# stale, silently linking the previous build's code.
if(APPLE)
    add_custom_command(OUTPUT ${COMBINED}
        COMMAND ld -r -exported_symbol _lb_candidate_fsst_like_utn
                $<TARGET_OBJECTS:utn_objs> -o ${COMBINED}
        DEPENDS utn_objs $<TARGET_OBJECTS:utn_objs> COMMAND_EXPAND_LISTS VERBATIM)
else()
    add_custom_command(OUTPUT ${COMBINED}
        COMMAND ${CMAKE_LINKER} -r $<TARGET_OBJECTS:utn_objs> -o ${COMBINED}
        COMMAND sh ${CMAKE_CURRENT_SOURCE_DIR}/localize.sh ${COMBINED}
        DEPENDS utn_objs $<TARGET_OBJECTS:utn_objs> ${CMAKE_CURRENT_SOURCE_DIR}/localize.sh COMMAND_EXPAND_LISTS VERBATIM)
endif()
set_source_files_properties(${COMBINED} PROPERTIES EXTERNAL_OBJECT TRUE GENERATED TRUE)

add_library(lb_fsst_like_utn STATIC ${COMBINED})
set_target_properties(lb_fsst_like_utn PROPERTIES LINKER_LANGUAGE CXX)

install(TARGETS lb_fsst_like_utn ARCHIVE DESTINATION lib)
