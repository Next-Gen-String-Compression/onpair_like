"""Deterministic fixture generator (portable LCG so the Rust test can reproduce it byte-for-byte)."""
import struct, sys
WORDS = ("the of and to in a is that for it as was with be by on not he this are or his from at which but "
 "have an had they you were their one all we can her has there been if more when will would who so no "
 "http https www com org index html query search user customer comment abstract wikipedia database "
 "compression symbol table string match pattern automaton fast static carefully regular requests special "
 "packages ironic furiously slyly blithely daringly pending final deposits accounts platelets foxes "
 "instructions theodolites excuses dolphins cat dog goose mouse the the").split()
MASK = (1 << 64) - 1
class Lcg:
    def __init__(self, seed): self.x = (seed * 2654435761 + 1) & MASK
    def next(self):
        self.x = (self.x * 6364136223846793005 + 1442695040888963407) & MASK
        return self.x >> 33
    def below(self, k): return self.next() % k
def rows_for(seed, n_short):
    rng = Lcg(seed)
    def sentence(k): return " ".join(WORDS[rng.below(len(WORDS))] for _ in range(k)).encode()
    def long_row(tail):
        r = sentence(90)
        while len(r) < 530: r += b" " + sentence(5)
        return r + tail
    rows = [sentence(1 + rng.below(14)) for _ in range(n_short)]
    common = [b"the", b"cat", b"dog", b"goose", b"mouse"]
    for _ in range(40): rows.extend(common)
    rows += [long_row(b"\xff"), b"the", long_row(b"abc\xff"), b"cat", long_row(b"\xffz\xff"), b"goose",
             long_row(b"x\xffy"), b"mouse", b"", b"the", long_row(b"\xff"), b"e", long_row(b"\xff"), b"",
             long_row(b"the\xff"), b"the", long_row(b""), b"the",
             b"back\\slash", b"trailing\\", b"\\", b"50% off_now\\here", b"a\\%b", long_row(b"\xff"),
             b"\xffe", b"q\xffe", b"\xff\xffe", b"\xff\xff\xffthe"]  # in-row escaped 0xFF before a match
    return rows
def write_csv(rows, path):
    with open(path, "wb") as f:
        f.write(b"data\n")
        for r in rows:
            if r == b"": f.write(b'""\n')
            elif any(c in r for c in b',"\n\r'): f.write(b'"' + r.replace(b'"', b'""') + b'"\n')
            else: f.write(r + b"\n")
def write_bin(rows, path):
    with open(path, "wb") as f:
        f.write(struct.pack("<I", len(rows)))
        for r in rows: f.write(struct.pack("<I", len(r)) + r)
if __name__ == "__main__":
    seed, n_short = int(sys.argv[1]), int(sys.argv[2])
    rows = rows_for(seed, n_short)
    write_bin(rows, sys.argv[3])
    if len(sys.argv) > 4: write_csv(rows, sys.argv[4])
    print("rows", len(rows), "bytes", sum(len(r) for r in rows))
