# Build for the `llm_token_tum` candidate (SGTT LLM-token-table compression,
# decode-only; adapted from TUM's token-vldb2026, vendored in ../vendor with
# provenance). The harness build invokes this via the `cmake` crate from
# build.rs and links the static library.
cmake_minimum_required(VERSION 3.21)
project(lb_llm_token_tum CXX ASM)

set(LB_TOKEN_DIR
    ${CMAKE_CURRENT_SOURCE_DIR}/../vendor/token-vldb2026/src/tokens/openai100kpatched)

# The candidate declares fixed-size extern arrays and the encoder assumes the
# 16-byte dict stride — refuse to build against a table of any other shape.
file(SIZE ${LB_TOKEN_DIR}/openai100kpatched_dict.bin LB_DICT_SIZE)
file(SIZE ${LB_TOKEN_DIR}/openai100kpatched_lengths.bin LB_LENGTHS_SIZE)
if(NOT LB_DICT_SIZE EQUAL 1048576 OR NOT LB_LENGTHS_SIZE EQUAL 65536)
  message(FATAL_ERROR "openai100kpatched token table has unexpected size: "
                      "dict=${LB_DICT_SIZE} lengths=${LB_LENGTHS_SIZE} "
                      "(expected 1048576 / 65536)")
endif()

configure_file(tokens.S.in tokens.S @ONLY)

include(CheckCXXCompilerFlag)
# Host-native codegen (the measured artifact runs on this host), matching the
# fsst/onpair candidates.
check_cxx_compiler_flag("-march=native" LB_LLM_TOKEN_MARCH_NATIVE)

add_library(lb_llm_token_tum STATIC
    llm_token_tum_candidate.cpp
    ${CMAKE_CURRENT_BINARY_DIR}/tokens.S
)
target_include_directories(lb_llm_token_tum PRIVATE
    ${CMAKE_CURRENT_SOURCE_DIR}/../../../contract
    ${CMAKE_CURRENT_SOURCE_DIR}/../vendor/token-vldb2026/src
)
set_target_properties(lb_llm_token_tum PROPERTIES
    CXX_STANDARD 20 CXX_STANDARD_REQUIRED ON)
target_compile_options(lb_llm_token_tum PRIVATE
    $<$<COMPILE_LANGUAGE:CXX>:-O3>
    "$<$<AND:$<COMPILE_LANGUAGE:CXX>,$<BOOL:${LB_LLM_TOKEN_MARCH_NATIVE}>>:-march=native>"
)

install(TARGETS lb_llm_token_tum ARCHIVE DESTINATION lib)
