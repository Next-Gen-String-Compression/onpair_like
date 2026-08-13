# Build for llm_token_prefilter + dict_llm_token_prefilter (compressed-domain
# prefilter over the SGTT LLM-token stream): the shared llm_token core +
# Matcher core + prefilter.hpp + the token mandatory-chain port. A SECOND copy
# of the token table is embedded under this candidate's own symbol prefix, so
# both llm_token candidates link into one harness binary without collisions
# (cf. fsst_prefilter's second FSST copy).
cmake_minimum_required(VERSION 3.21)
project(lb_llm_token_prefilter CXX ASM)

set(LB_LLM_TOKEN_COMMON ${CMAKE_CURRENT_SOURCE_DIR}/../../common/llm_token)
set(LB_TOKEN_DIR
    ${LB_LLM_TOKEN_COMMON}/vendor/token-vldb2026/src/tokens/openai100kpatched)

# Same table-shape guard as llm_token_tum's build.
file(SIZE ${LB_TOKEN_DIR}/openai100kpatched_dict.bin LB_DICT_SIZE)
file(SIZE ${LB_TOKEN_DIR}/openai100kpatched_lengths.bin LB_LENGTHS_SIZE)
if(NOT LB_DICT_SIZE EQUAL 1048576 OR NOT LB_LENGTHS_SIZE EQUAL 65536)
  message(FATAL_ERROR "openai100kpatched token table has unexpected size: "
                      "dict=${LB_DICT_SIZE} lengths=${LB_LENGTHS_SIZE} "
                      "(expected 1048576 / 65536)")
endif()

set(LB_TOKEN_PREFIX lb_llm_token_pf)
configure_file(${LB_LLM_TOKEN_COMMON}/tokens.S.in tokens.S @ONLY)

include(CheckCXXCompilerFlag)
check_cxx_compiler_flag("-march=native" LB_LLM_TOKEN_MARCH_NATIVE)

add_library(lb_llm_token_prefilter STATIC
    llm_token_prefilter_candidate.cpp
    ${CMAKE_CURRENT_BINARY_DIR}/tokens.S
)
target_include_directories(lb_llm_token_prefilter PRIVATE
    ${CMAKE_CURRENT_SOURCE_DIR}/../../../contract
    ${LB_LLM_TOKEN_COMMON}
    ${LB_LLM_TOKEN_COMMON}/vendor/token-vldb2026/src
    ${CMAKE_CURRENT_SOURCE_DIR}/../../common/matcher
    ${CMAKE_CURRENT_SOURCE_DIR}/../../common/prefilter
)
set_target_properties(lb_llm_token_prefilter PROPERTIES
    CXX_STANDARD 20 CXX_STANDARD_REQUIRED ON)
target_compile_options(lb_llm_token_prefilter PRIVATE
    $<$<COMPILE_LANGUAGE:CXX>:-O3>
    "$<$<AND:$<COMPILE_LANGUAGE:CXX>,$<BOOL:${LB_LLM_TOKEN_MARCH_NATIVE}>>:-march=native>"
)

install(TARGETS lb_llm_token_prefilter ARCHIVE DESTINATION lib)
