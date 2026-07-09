# Author-owned build for the ac-cpp scanner; see the candidate CMakeLists for
# the pattern. Vendors the author's Aho-Corasick headers under cpp/include so
# the scanner is self-contained (no dependency on the onpair source tree).
cmake_minimum_required(VERSION 3.20)
project(ac_cpp CXX)

add_library(ac_cpp STATIC ac_cpp.cpp)
target_include_directories(ac_cpp PRIVATE
    ${CMAKE_CURRENT_SOURCE_DIR}/../../../contract
    ${CMAKE_CURRENT_SOURCE_DIR}/include)
set_target_properties(ac_cpp PROPERTIES CXX_STANDARD 20 CXX_STANDARD_REQUIRED ON)
target_compile_options(ac_cpp PRIVATE -O3)

install(TARGETS ac_cpp ARCHIVE DESTINATION lib)
