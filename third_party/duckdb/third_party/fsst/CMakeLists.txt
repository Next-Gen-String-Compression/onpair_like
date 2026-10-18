if(POLICY CMP0063)
    cmake_policy(SET CMP0063 NEW)
endif()

find_package(Threads REQUIRED)

set(CMAKE_CXX_VISIBILITY_PRESET hidden)

# upstream/ is our FSST fork, third_party/fsst of this repository. NONOPT_FSST drops the
# AVX-512 encoder, which parses strings in 511-byte chunks the prefilter cannot follow.
add_library(duckdb_fsst STATIC upstream/libfsst.cpp)
target_compile_definitions(duckdb_fsst PRIVATE NONOPT_FSST)

target_include_directories(duckdb_fsst PUBLIC $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/upstream>)
set_target_properties(duckdb_fsst PROPERTIES EXPORT_NAME duckdb_fsst)

install(TARGETS duckdb_fsst
        EXPORT "${DUCKDB_EXPORT_SET}"
        LIBRARY DESTINATION "${INSTALL_LIB_DIR}"
        ARCHIVE DESTINATION "${INSTALL_LIB_DIR}")

disable_target_warnings(duckdb_fsst)

# upstream/search: the header-only substring prefilter over FSST code streams
add_library(fsst_search INTERFACE)
target_include_directories(fsst_search INTERFACE $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/upstream/search>)
