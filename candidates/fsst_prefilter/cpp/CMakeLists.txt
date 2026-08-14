# Author-owned build for fsst_prefilter + dict_fsst_prefilter: cwida/fsst
# upstream (the `fsst` candidate's fork) + the Matcher core + prefilter.hpp +
# the mandatory-chain port. A SECOND/THIRD cwida FSST copy in the harness, so
# all FSST symbols are localized at link, leaving only the two lb_candidate_*
# entry points exported (cf. fsst_decode_prefilter/CMakeLists.txt).
cmake_minimum_required(VERSION 3.21)
project(lb_fsst_prefilter CXX)

include(FetchContent)
FetchContent_Declare(
    fsst_upstream
    GIT_REPOSITORY https://github.com/cwida/fsst.git
    GIT_TAG        e638d4cf8c26129d73c242a4127b42b975de5b63
    SOURCE_SUBDIR  __lb_no_configure__
)
FetchContent_MakeAvailable(fsst_upstream)

include(CheckCXXCompilerFlag)
check_cxx_compiler_flag("-march=native" LB_FSST_MARCH_NATIVE)

add_library(fp_objs OBJECT
    fsst_prefilter_candidate.cpp
    ${fsst_upstream_SOURCE_DIR}/libfsst.cpp
    ${fsst_upstream_SOURCE_DIR}/fsst_avx512.cpp
)
target_include_directories(fp_objs PRIVATE
    ${fsst_upstream_SOURCE_DIR}
    ${CMAKE_CURRENT_SOURCE_DIR}/../../../contract
    ${CMAKE_CURRENT_SOURCE_DIR}/../../common/fsst_common
    ${CMAKE_CURRENT_SOURCE_DIR}/../../common/matcher
    ${CMAKE_CURRENT_SOURCE_DIR}/../../common/prefilter
)
set_target_properties(fp_objs PROPERTIES
    CXX_STANDARD 20 CXX_STANDARD_REQUIRED ON POSITION_INDEPENDENT_CODE ON)
target_compile_options(fp_objs PRIVATE
    -O3 "$<$<BOOL:${LB_FSST_MARCH_NATIVE}>:-march=native>" -fvisibility=hidden)
set_source_files_properties(${fsst_upstream_SOURCE_DIR}/fsst_avx512.cpp
    PROPERTIES COMPILE_FLAGS "-O1")

set(COMBINED ${CMAKE_CURRENT_BINARY_DIR}/lb_fsst_prefilter_combined${CMAKE_CXX_OUTPUT_EXTENSION})
# DEPENDS names the object files as well as the target: a bare target name is
# only an ordering dependency, so an edited source would recompile the objects
# and still leave this combined object -- and the archive built from it --
# stale, silently linking the previous build's code.
if(APPLE)
    add_custom_command(OUTPUT ${COMBINED}
        COMMAND ld -r -exported_symbol _lb_candidate_fsst_prefilter
                -exported_symbol _lb_candidate_dict_fsst_prefilter
                $<TARGET_OBJECTS:fp_objs> -o ${COMBINED}
        DEPENDS fp_objs $<TARGET_OBJECTS:fp_objs> COMMAND_EXPAND_LISTS VERBATIM)
else()
    add_custom_command(OUTPUT ${COMBINED}
        COMMAND ${CMAKE_LINKER} -r $<TARGET_OBJECTS:fp_objs> -o ${COMBINED}
        COMMAND sh ${CMAKE_CURRENT_SOURCE_DIR}/localize.sh ${COMBINED}
        DEPENDS fp_objs $<TARGET_OBJECTS:fp_objs> ${CMAKE_CURRENT_SOURCE_DIR}/localize.sh COMMAND_EXPAND_LISTS VERBATIM)
endif()
set_source_files_properties(${COMBINED} PROPERTIES EXTERNAL_OBJECT TRUE GENERATED TRUE)

add_library(lb_fsst_prefilter STATIC ${COMBINED})
set_target_properties(lb_fsst_prefilter PROPERTIES LINKER_LANGUAGE CXX)

install(TARGETS lb_fsst_prefilter ARCHIVE DESTINATION lib)
