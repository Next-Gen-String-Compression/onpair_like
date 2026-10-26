# simd_cut_fsst_debug: simd_cut_fsst with the "sorted" and "uniform" switches, built
# from this candidate's own copy of our FSST fork (../fsst, bdaab1d plus the
# switches), not from the pin. All FSST symbols are localized at link, leaving only
# lb_candidate_simd_cut_fsst_debug exported (cf. simd_cut_fsst/cpp/CMakeLists.txt).
cmake_minimum_required(VERSION 3.21)
project(lb_simd_cut_fsst_debug CXX)

set(LB_FSST_DEBUG_SRC "${CMAKE_CURRENT_SOURCE_DIR}/../fsst")

include(CheckCXXCompilerFlag)
# The prefilter kernels are NEON, AVX-512BW or AVX2 by what -march defines, as in
# simd_cut_fsst; LB_FSST_MARCH pins one instead of the host's.
set(LB_FSST_MARCH "native" CACHE STRING "-march= value for the prefilter kernels")
check_cxx_compiler_flag("-march=${LB_FSST_MARCH}" LB_FSST_MARCH_OK)

add_library(fpd_objs OBJECT
    simd_cut_fsst_debug_candidate.cpp
    ${LB_FSST_DEBUG_SRC}/libfsst.cpp
    ${LB_FSST_DEBUG_SRC}/fsst_avx512.cpp
)
target_include_directories(fpd_objs PRIVATE
    ${LB_FSST_DEBUG_SRC}
    ${LB_FSST_DEBUG_SRC}/search/prefilter
    ${CMAKE_CURRENT_SOURCE_DIR}/../../../contract
    ${CMAKE_CURRENT_SOURCE_DIR}/../../common/fsst_common
    ${CMAKE_CURRENT_SOURCE_DIR}/../../common/matcher
)
set_target_properties(fpd_objs PROPERTIES
    CXX_STANDARD 20 CXX_STANDARD_REQUIRED ON POSITION_INDEPENDENT_CODE ON)
target_compile_options(fpd_objs PRIVATE
    -O3 "$<$<BOOL:${LB_FSST_MARCH_OK}>:-march=${LB_FSST_MARCH}>" -fvisibility=hidden)
set_source_files_properties(${LB_FSST_DEBUG_SRC}/fsst_avx512.cpp
    PROPERTIES COMPILE_FLAGS "-O1")

set(COMBINED ${CMAKE_CURRENT_BINARY_DIR}/lb_simd_cut_fsst_debug_combined${CMAKE_CXX_OUTPUT_EXTENSION})
# DEPENDS names the object files as well as the target, as in simd_cut_fsst.
if(APPLE)
    add_custom_command(OUTPUT ${COMBINED}
        COMMAND ld -r -exported_symbol _lb_candidate_simd_cut_fsst_debug
                $<TARGET_OBJECTS:fpd_objs> -o ${COMBINED}
        DEPENDS fpd_objs $<TARGET_OBJECTS:fpd_objs> COMMAND_EXPAND_LISTS VERBATIM)
else()
    # localize.sh keeps every symbol but the entry points to this candidate
    # (see there).
    add_custom_command(OUTPUT ${COMBINED}
        COMMAND ${CMAKE_LINKER} -r $<TARGET_OBJECTS:fpd_objs> -o ${COMBINED}
        COMMAND sh ${CMAKE_CURRENT_SOURCE_DIR}/localize.sh ${COMBINED}
        DEPENDS fpd_objs $<TARGET_OBJECTS:fpd_objs> ${CMAKE_CURRENT_SOURCE_DIR}/localize.sh COMMAND_EXPAND_LISTS VERBATIM)
endif()
set_source_files_properties(${COMBINED} PROPERTIES EXTERNAL_OBJECT TRUE GENERATED TRUE)

add_library(lb_simd_cut_fsst_debug STATIC ${COMBINED})
set_target_properties(lb_simd_cut_fsst_debug PROPERTIES LINKER_LANGUAGE CXX)

install(TARGETS lb_simd_cut_fsst_debug ARCHIVE DESTINATION lib)
