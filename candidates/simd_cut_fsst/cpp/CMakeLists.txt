# Author-owned build for simd_cut_fsst + dict_simd_cut_fsst: our FSST fork in
# third_party/fsst (whole-string compression, codes written in byte order, and
# the header-only min-cut prefilter under search/prefilter) plus the Matcher
# core. A further FSST copy in the harness, so all FSST symbols are localized
# at link, leaving only the two lb_candidate_* entry points exported.
cmake_minimum_required(VERSION 3.21)
project(lb_simd_cut_fsst CXX)

set(fsst_fork_SOURCE_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../../../third_party/fsst")

include(CheckCXXCompilerFlag)
# The prefilter kernels are NEON, AVX-512BW or AVX2 by what -march defines;
# the candidate declares the set it got in cpu_features. Default `native` is
# the host that will run the result. Set LB_FSST_MARCH to pin one instead,
# e.g. `x86-64-v3` for AVX2 on a machine that also has AVX-512.
set(LB_FSST_MARCH "native" CACHE STRING "-march= value for the prefilter kernels")
check_cxx_compiler_flag("-march=${LB_FSST_MARCH}" LB_FSST_MARCH_OK)

add_library(fp_objs OBJECT
    simd_cut_fsst_candidate.cpp
    ${fsst_fork_SOURCE_DIR}/libfsst.cpp
    ${fsst_fork_SOURCE_DIR}/fsst_avx512.cpp
)
target_include_directories(fp_objs PRIVATE
    ${fsst_fork_SOURCE_DIR}
    ${fsst_fork_SOURCE_DIR}/search/prefilter
    ${CMAKE_CURRENT_SOURCE_DIR}/../../../contract
    ${CMAKE_CURRENT_SOURCE_DIR}/../../common/fsst_common
    ${CMAKE_CURRENT_SOURCE_DIR}/../../common/matcher
)
set_target_properties(fp_objs PROPERTIES
    CXX_STANDARD 20 CXX_STANDARD_REQUIRED ON POSITION_INDEPENDENT_CODE ON)
target_compile_options(fp_objs PRIVATE
    -O3 "$<$<BOOL:${LB_FSST_MARCH_OK}>:-march=${LB_FSST_MARCH}>" -fvisibility=hidden)
set_source_files_properties(${fsst_fork_SOURCE_DIR}/fsst_avx512.cpp
    PROPERTIES COMPILE_FLAGS "-O1")

set(COMBINED ${CMAKE_CURRENT_BINARY_DIR}/lb_simd_cut_fsst_combined${CMAKE_CXX_OUTPUT_EXTENSION})
# DEPENDS names the object files as well as the target: a bare target name is
# only an ordering dependency, so an edited source would recompile the objects
# and still leave this combined object -- and the archive built from it --
# stale, silently linking the previous build's code.
if(APPLE)
    add_custom_command(OUTPUT ${COMBINED}
        COMMAND ld -r -exported_symbol _lb_candidate_simd_cut_fsst
                -exported_symbol _lb_candidate_dict_simd_cut_fsst
                $<TARGET_OBJECTS:fp_objs> -o ${COMBINED}
        DEPENDS fp_objs $<TARGET_OBJECTS:fp_objs> COMMAND_EXPAND_LISTS VERBATIM)
else()
    add_custom_command(OUTPUT ${COMBINED}
        COMMAND ${CMAKE_LINKER} -r $<TARGET_OBJECTS:fp_objs> -o ${COMBINED}
        COMMAND sh ${CMAKE_CURRENT_SOURCE_DIR}/localize.sh ${COMBINED}
        DEPENDS fp_objs $<TARGET_OBJECTS:fp_objs> ${CMAKE_CURRENT_SOURCE_DIR}/localize.sh COMMAND_EXPAND_LISTS VERBATIM)
endif()
set_source_files_properties(${COMBINED} PROPERTIES EXTERNAL_OBJECT TRUE GENERATED TRUE)

add_library(lb_simd_cut_fsst STATIC ${COMBINED})
set_target_properties(lb_simd_cut_fsst PROPERTIES LINKER_LANGUAGE CXX)

install(TARGETS lb_simd_cut_fsst ARCHIVE DESTINATION lib)
