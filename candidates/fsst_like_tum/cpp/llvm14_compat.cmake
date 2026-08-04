# FetchContent PATCH_COMMAND script for FSST-LIKE-Matching: the ONLY source
# deviation from the pinned upstream (b1eb3ab9). Upstream targets LLVM 16,
# where `LLJIT::lookup()` yields an `ExecutorAddr` with `toPtr<T>()`; on LLVM 14
# that accessor does not exist (`JITEvaluatedSymbol::getAddress()` + cast is the
# spelling). Wrap that one line in an LLVM_VERSION_MAJOR guard so the pin
# builds on LLVM 14–16 unchanged. Runs in the populated source dir; idempotent
# (FetchContent re-runs it if the declaration changes).
set(FILE src/codegen/llvmcodegen.cpp)
file(READ ${FILE} content)
if(content MATCHES "LLVM_VERSION_MAJOR >= 15")
    return()
endif()
set(ORIG "    parseFunction = cantFail(this->JIT->lookup(Compiler::getParseFunctionName())).toPtr<bool (*) (const uint8_t*, size_t)>();")
set(REPL "#if LLVM_VERSION_MAJOR >= 15
${ORIG}
#else
    parseFunction = reinterpret_cast<bool (*) (const uint8_t*, size_t)>(
        cantFail(this->JIT->lookup(Compiler::getParseFunctionName())).getAddress());
#endif")
string(FIND "${content}" "${ORIG}" pos)
if(pos EQUAL -1)
    message(FATAL_ERROR "llvm14_compat.cmake: expected toPtr line not found in ${FILE} — upstream pin changed; re-verify the patch")
endif()
string(REPLACE "${ORIG}" "${REPL}" content "${content}")
file(WRITE ${FILE} "${content}")
