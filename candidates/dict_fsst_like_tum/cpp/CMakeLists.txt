# Author-owned build for the `dict_fsst_like_tum` candidate: the dictionary
# front-end (candidates/common/matcher) over the FSST-LIKE compressed-domain
# matcher. Mirrors fsst_like_tum/cpp/CMakeLists.txt (same pinned deps, same
# curated interpreted subset, same symbol localization) with the matcher include
# dirs added and this candidate's entry point exported.
cmake_minimum_required(VERSION 3.21)
project(lb_dict_fsst_like_tum CXX)

include(FetchContent)
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

add_library(fl_objs OBJECT
    dict_fsst_like_tum_candidate.cpp
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
    ${CMAKE_CURRENT_SOURCE_DIR}/../../common/fsst_common
    ${CMAKE_CURRENT_SOURCE_DIR}/../../common/matcher
)
target_compile_definitions(fl_objs PRIVATE FMT_HEADER_ONLY)
set_target_properties(fl_objs PROPERTIES
    CXX_STANDARD 20 CXX_STANDARD_REQUIRED ON POSITION_INDEPENDENT_CODE ON)
target_compile_options(fl_objs PRIVATE
    -O3 "$<$<BOOL:${LB_FL_MARCH_NATIVE}>:-march=native>"
    -fvisibility=hidden -Wno-deprecated-declarations -Wno-return-type
    "SHELL:-include unistd.h"
)
set_source_files_properties(${fsst_fork_SOURCE_DIR}/fsst_avx512.cpp
    PROPERTIES COMPILE_FLAGS "-O1")

set(COMBINED ${CMAKE_CURRENT_BINARY_DIR}/lb_dict_fsst_like_tum_combined${CMAKE_CXX_OUTPUT_EXTENSION})
if(APPLE)
    add_custom_command(OUTPUT ${COMBINED}
        COMMAND ld -r -exported_symbol _lb_candidate_dict_fsst_like_tum
                $<TARGET_OBJECTS:fl_objs> -o ${COMBINED}
        DEPENDS fl_objs COMMAND_EXPAND_LISTS VERBATIM)
else()
    add_custom_command(OUTPUT ${COMBINED}
        COMMAND ${CMAKE_LINKER} -r $<TARGET_OBJECTS:fl_objs> -o ${COMBINED}
        COMMAND sh ${CMAKE_CURRENT_SOURCE_DIR}/localize.sh ${COMBINED}
        DEPENDS fl_objs ${CMAKE_CURRENT_SOURCE_DIR}/localize.sh COMMAND_EXPAND_LISTS VERBATIM)
endif()
set_source_files_properties(${COMBINED} PROPERTIES EXTERNAL_OBJECT TRUE GENERATED TRUE)

add_library(lb_dict_fsst_like_tum STATIC ${COMBINED})
set_target_properties(lb_dict_fsst_like_tum PROPERTIES LINKER_LANGUAGE CXX)

install(TARGETS lb_dict_fsst_like_tum ARCHIVE DESTINATION lib)
