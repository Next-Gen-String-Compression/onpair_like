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

# OnPair links Boost::unordered (header-only boost::unordered_flat_map, Boost
# >= 1.81). Resolve it here, before OnPair's own find_package runs, so the
# target exists when OnPair links it. This is onpair_like's wrapper — the OnPair
# library itself is fetched unmodified.
#   * Homebrew/macOS (and any Boost whose CMake package exports the component):
#     this find_package creates the real target; the shim below is skipped.
#   * Debian/Ubuntu: the package ships no per-config for header-only libs, so
#     find_package reports Boost_FOUND but creates no target — we synthesize an
#     INTERFACE target from the installed headers.
#   * No system Boost at all: neither branch fires and OnPair's own vendored-Boost
#     FetchContent fallback still triggers.
find_package(Boost 1.81 QUIET CONFIG COMPONENTS unordered)
if(NOT TARGET Boost::unordered)
    find_path(LB_BOOST_UNORDERED_INCLUDE boost/unordered/unordered_flat_map.hpp)
    if(LB_BOOST_UNORDERED_INCLUDE)
        message(STATUS "lb_onpair: synthesizing header-only Boost::unordered from ${LB_BOOST_UNORDERED_INCLUDE}")
        add_library(Boost::unordered INTERFACE IMPORTED GLOBAL)
        set_target_properties(Boost::unordered PROPERTIES
            INTERFACE_INCLUDE_DIRECTORIES "${LB_BOOST_UNORDERED_INCLUDE}")
    endif()
endif()

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
