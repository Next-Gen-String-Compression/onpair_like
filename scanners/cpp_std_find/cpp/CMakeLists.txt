# Author-owned build for the cpp_std_find scanner; see the candidate
# CMakeLists for the pattern.
cmake_minimum_required(VERSION 3.20)
project(cpp_std_find CXX)

add_library(cpp_std_find STATIC std_find.cpp)
target_include_directories(cpp_std_find PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/../../../contract)
set_target_properties(cpp_std_find PROPERTIES CXX_STANDARD 20 CXX_STANDARD_REQUIRED ON)
target_compile_options(cpp_std_find PRIVATE -O3)

install(TARGETS cpp_std_find ARCHIVE DESTINATION lib)
