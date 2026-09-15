# Author-owned build for fsst_prefilter + dict_fsst_prefilter: the gropaul/fsst
# fork (branch search/prefilter: whole-string compression, codes written in
# byte order, and the header-only min-cut prefilter under search/prefilter)
# plus the Matcher core. A further FSST copy in the harness, so all FSST
# symbols are localized at link, leaving only the two lb_candidate_* entry
# points exported (cf. fsst_decode_prefilter/CMakeLists.txt).
#
# The fork is fetched pinned. To develop against a local checkout instead:
#   FSST_SOURCE_DIR=/path/to/fsst cargo build --release
cmake_minimum_required(VERSION 3.21)
project(lb_fsst_prefilter CXX)

include(FetchContent)
if(DEFINED ENV{FSST_SOURCE_DIR})
    set(FETCHCONTENT_SOURCE_DIR_FSST_FORK "$ENV{FSST_SOURCE_DIR}")
    message(STATUS "lb_fsst_prefilter: using local FSST checkout $ENV{FSST_SOURCE_DIR}")
endif()
FetchContent_Declare(
    fsst_fork
    GIT_REPOSITORY https://github.com/gropaul/fsst.git
    GIT_TAG        abb24db31b02ef242946f974ead2ce401733578c
    SOURCE_SUBDIR  __lb_no_configure__
)
FetchContent_MakeAvailable(fsst_fork)

include(CheckCXXCompilerFlag)
check_cxx_compiler_flag("-march=native" LB_FSST_MARCH_NATIVE)
# The prefilter kernels are AVX-512BW on x86-64, compiled in regardless of the
# build host; the candidate declares avx512f,avx512bw and the harness gates.
if(CMAKE_SYSTEM_PROCESSOR MATCHES "x86_64|AMD64")
    set(LB_FSST_ISA_FLAGS -mavx512f -mavx512bw)
else()
    set(LB_FSST_ISA_FLAGS "")
endif()

add_library(fp_objs OBJECT
    fsst_prefilter_candidate.cpp
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
    -O3 "$<$<BOOL:${LB_FSST_MARCH_NATIVE}>:-march=native>" ${LB_FSST_ISA_FLAGS} -fvisibility=hidden)
set_source_files_properties(${fsst_fork_SOURCE_DIR}/fsst_avx512.cpp
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
