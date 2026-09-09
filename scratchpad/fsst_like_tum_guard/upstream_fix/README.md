# Upstream fix for calin2110/FSST-LIKE-Matching (pin b1eb3ab9)

Kernel-level fixes for the escape look-back of DESIGN.md §17.7, submitted as a
PR against https://github.com/calin2110/FSST-LIKE-Matching (main == b1eb3ab9,
no other issues or PRs as of 2026-09-02). **Opened as
https://github.com/calin2110/FSST-LIKE-Matching/pull/1 on 2026-09-02** from the
fork `Hedi-Chehaidar/FSST-LIKE-Matching`, branch `fix/backward-scan-row-start`. Instead of padding the stream (what
`fsst_common::GuardedStream` does here), commit 1 stops the backward scan at the
row start in all three kernels (`*strIdx >= 0 &&` in the loop condition), and
commit 2 decides escapes by the parity of the 255 run: the suffix automaton's
pseudo-ends alternate with an "odd" state on 255 (`src/percentage.cpp`), and the
middle-start scan loops emitted by the C++/LLVM generators track the escape
state seeded by a parity walk-back (`isEscapedLiteral`). The candidate's CMake
pins the fork branch carrying both commits.

- `0001-*.patch`, `0002-*.patch` — `git am`-able series against b1eb3ab9.
- `PR_BODY.md` — pull-request description.

Validated with `../guard_test` built against the patched sources (synthetic
corpus + `gen_fixture.py 1 8000`, which plants the in-row rows): no read before
the row, no `0xFF` flip, no false negative from escaped `0xFF` on any backend;
only the trailing-backslash patterns and the benign LLVM after-row read remain. To open the PR:

```sh
gh repo fork calin2110/FSST-LIKE-Matching --clone=false
git clone https://github.com/<you>/FSST-LIKE-Matching && cd FSST-LIKE-Matching
git checkout -b fix/backward-scan-row-start && git am <path>/0001-fix-backward-scan-row-start.patch
git push -u origin fix/backward-scan-row-start
gh pr create -R calin2110/FSST-LIKE-Matching --title "fix: stop the backward scan at the row start instead of reading data[-1]" --body-file <path>/PR_BODY.md
```
