# Author-owned build for the `dict` candidate (plain dictionary, decode-only).
# Self-contained: no external fetch (the FlatDictionary kernel is vendored in
# flat_dictionary.hpp). Same shape as the `fsst` candidate's CMake, minus the
# FetchContent of cwida/fsst. The harness build invokes this via the `cmake`
# crate from build.rs and links the static library.
cmake_minimum_required(VERSION 3.21)
project(lb_dict CXX)

include(CheckCXXCompilerFlag)
# Host-native codegen (the measured artifact runs on this host), matching the
# fsst/onpair candidates. On Apple clang/arm64 `-march=native` is unsupported,
# so the guard drops it.
check_cxx_compiler_flag("-march=native" LB_DICT_MARCH_NATIVE)

add_library(lb_dict STATIC
    dict_candidate.cpp
)
target_include_directories(lb_dict PRIVATE
    ${CMAKE_CURRENT_SOURCE_DIR}/../../../contract
    ${CMAKE_CURRENT_SOURCE_DIR}/../../common/dict_common
)
set_target_properties(lb_dict PROPERTIES CXX_STANDARD 20 CXX_STANDARD_REQUIRED ON)
target_compile_options(lb_dict PRIVATE
    -O3 "$<$<BOOL:${LB_DICT_MARCH_NATIVE}>:-march=native>"
)

install(TARGETS lb_dict ARCHIVE DESTINATION lib)
