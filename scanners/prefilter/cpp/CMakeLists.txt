# Author-owned build for the prefilter scanner; mirrors the cpp_std_find
# pattern. prefilter.hpp (candidates/common/prefilter) is header-only, so only
# the scanner TU is compiled here.
cmake_minimum_required(VERSION 3.20)
project(prefilter_scan CXX)

add_library(prefilter_scan STATIC prefilter_scan.cpp)
target_include_directories(prefilter_scan PRIVATE
    ${CMAKE_CURRENT_SOURCE_DIR}/../../../contract
    ${CMAKE_CURRENT_SOURCE_DIR}/../../../candidates/common/prefilter)
set_target_properties(prefilter_scan PROPERTIES CXX_STANDARD 20 CXX_STANDARD_REQUIRED ON)
target_compile_options(prefilter_scan PRIVATE -O3)

install(TARGETS prefilter_scan ARCHIVE DESTINATION lib)
