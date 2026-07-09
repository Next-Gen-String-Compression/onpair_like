# Author-owned build for the onpair candidate. Flags chosen here are part
# of the candidate's identity; the harness build invokes this via the
# `cmake` crate from build.rs and links the static libraries.
#
# OnPair is fetched pinned to a commit (the library is pre-1.0 and asks
# embedders to pin). To develop against a local checkout instead:
#   ONPAIR_SOURCE_DIR=/path/to/onpair_cpp cargo build --release
cmake_minimum_required(VERSION 3.21)
project(lb_onpair CXX)

include(FetchContent)
if(DEFINED ENV{ONPAIR_SOURCE_DIR})
    set(FETCHCONTENT_SOURCE_DIR_ONPAIR "$ENV{ONPAIR_SOURCE_DIR}")
    message(STATUS "lb_onpair: using local OnPair checkout $ENV{ONPAIR_SOURCE_DIR}")
endif()
FetchContent_Declare(
    onpair
    GIT_REPOSITORY https://github.com/gargiulofrancesco/onpair_cpp.git
    GIT_TAG        f8ecc64fa7786cfa6fc46d46564135ee99a8ba49
)
# OnPair's own .cpp files (trainer/parser — the build()-time hot path) get
# host-native codegen; the query-time hot path is header-only and is
# compiled into THIS target (see below).
set(ONPAIR_NATIVE_ARCH ON)
FetchContent_MakeAvailable(onpair)

add_library(lb_onpair STATIC onpair_candidate.cpp)
target_include_directories(lb_onpair PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/../../../contract)
target_link_libraries(lb_onpair PRIVATE OnPair::onpair)
set_target_properties(lb_onpair PROPERTIES CXX_STANDARD 20 CXX_STANDARD_REQUIRED ON)
target_compile_options(lb_onpair PRIVATE -O3)
# OnPair's decoder, scan loop, and automata are templated headers
# instantiated in this translation unit — per the library's benchmarking
# guidance they must be codegen'd host-native here, not just in libonpair.
onpair_enable_native_codegen(lb_onpair)

install(TARGETS lb_onpair onpair ARCHIVE DESTINATION lib)
