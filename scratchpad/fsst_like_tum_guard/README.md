# fsst_like_tum guard-page driver

Standalone probe behind DESIGN.md §17.7: drives every FSST-LIKE backend
(`interp`, `cpp`, `cpp-simd`, `llvm`, `llvm-simd`) over rows placed flush
against `PROT_NONE` guard pages and checks each answer against a SEMANTICS.md
oracle. It is how the one-byte out-of-row reads (and the `0xFF` false negative)
were found and how the candidate's padded layout was sized.

```sh
cargo build --release                      # fetches the upstream sources it links
scratchpad/fsst_like_tum_guard/build.sh    # -> scratchpad/fsst_like_tum_guard/build/guard_test
cd scratchpad/fsst_like_tum_guard/build    # generated kernels (gen_*.cpp/.so) land in the CWD
./guard_test | tee run.log                 # synthetic corpus, ~2 min (clang++ per cpp cell)
grep -E '^\[| BAD ' run.log                # non-clean cells
```

`gen_fixture.py SEED N_SHORT out.bin [out.csv]` reproduces the regression
corpus of `harness/tests/fsst_like_tum_guard.rs` (same LCG, byte-identical);
`./guard_test --rows out.bin` runs the suffix queries over it, and
`--probe-only` just reports whether FSST escaped `0xFF` (the precondition for
the row-boundary hazard). Expected residual non-clean cells with the current
upstream pin: the `*_bs` trailing-backslash patterns (refused by the candidate)
and `suffix_ff*` on rows ending in ≥2 escaped `0xFF`.
