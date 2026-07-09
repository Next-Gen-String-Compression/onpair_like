# Author-owned build for the cpp_identity candidate. Flags chosen here are
# part of the candidate's identity; the harness build invokes this via the
# `cmake` crate from build.rs and links the static library.
cmake_minimum_required(VERSION 3.20)
project(cpp_identity CXX)

add_library(cpp_identity STATIC identity.cpp)
target_include_directories(cpp_identity PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/../../../contract)
set_target_properties(cpp_identity PROPERTIES CXX_STANDARD 20 CXX_STANDARD_REQUIRED ON)
target_compile_options(cpp_identity PRIVATE -O3)

install(TARGETS cpp_identity ARCHIVE DESTINATION lib)
