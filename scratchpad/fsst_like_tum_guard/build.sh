#!/bin/sh
# Builds guard_test against the FSST-LIKE / calin-fsst / fmt sources that the
# fsst_like_tum candidate's CMake build already fetched (run `cargo build
# --release` first). LLVM backends are compiled in when llvm-config-14 exists.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
build=$(ls -dt "$root"/target/release/build/lb-cand-fsst-like-tum-*/out/build | head -1)
src="$build/_deps/fsst_like_src-src"; fork="$build/_deps/fsst_fork-src"; fmt="$build/_deps/fmt_ext-src"
[ -d "$src" ] || { echo "upstream sources not found under $build — run cargo build --release" >&2; exit 1; }
CXX=${CXX:-g++}
FLAGS="-std=gnu++20 -O2 -g -march=native -DFMT_HEADER_ONLY -DNDEBUG -D_GNU_SOURCE -I$src/include -I$build/gen_include -I$fmt/include -include unistd.h -w ${EXTRA:-}"
LLVM_SRC=""; LLVM_LD=""
if command -v llvm-config-14 >/dev/null 2>&1; then
  FLAGS="$FLAGS -DHAVE_LLVM $(llvm-config-14 --cppflags)"
  LLVM_SRC="$src/src/codegen/llvmcodegen.cpp"
  LLVM_LD="$(llvm-config-14 --ldflags) -lLLVM-14"
fi
out=${OUT:-"$here/build"}; mkdir -p "$out"; cd "$out"
$CXX $FLAGS -O1 -c "$fork/fsst_avx512.cpp" -o fsst_avx512.o   # upstream: must be -O1
objs="fsst_avx512.o"
for f in "$fork/libfsst.cpp" "$src"/src/encoder.cpp "$src"/src/automata.cpp "$src"/src/like_pattern_automaton.cpp \
         "$src"/src/pattern.cpp "$src"/src/percentage.cpp "$src"/src/common.cpp \
         "$src"/src/codegen/codegen.cpp "$src"/src/codegen/cppcodegen.cpp $LLVM_SRC "$here/guard_test.cpp"; do
  o=$(basename "$f" .cpp).o; $CXX $FLAGS -c "$f" -o "$o" & objs="$objs $o"
done
wait
$CXX $FLAGS -o guard_test $objs $LLVM_LD -ldl ${EXTRA:-}
echo "built $out/guard_test"
