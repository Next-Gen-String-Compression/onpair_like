# Author-owned build for the dict_uncompressed_prefilter candidate; header-only Matcher core
# (candidates/common/matcher) + prefilter.hpp, one TU compiled here.
cmake_minimum_required(VERSION 3.20)
project(lb_dict_uncompressed_prefilter CXX)

add_library(lb_dict_uncompressed_prefilter STATIC dict_uncompressed_prefilter_candidate.cpp)
target_include_directories(lb_dict_uncompressed_prefilter PRIVATE
    ${CMAKE_CURRENT_SOURCE_DIR}/../../../contract
    ${CMAKE_CURRENT_SOURCE_DIR}/../../common/matcher
    ${CMAKE_CURRENT_SOURCE_DIR}/../../common/prefilter)
set_target_properties(lb_dict_uncompressed_prefilter PROPERTIES CXX_STANDARD 20 CXX_STANDARD_REQUIRED ON)
target_compile_options(lb_dict_uncompressed_prefilter PRIVATE -O3)

install(TARGETS lb_dict_uncompressed_prefilter ARCHIVE DESTINATION lib)
