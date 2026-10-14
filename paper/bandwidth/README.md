# Memory bandwidth

What one core pulls from memory at a given working-set size, so a scan's GB/s
can be read against the ceiling the machine allows for the stream it walks.
The stage-one sweep walks 1 MiB of code stream, 2^19 codes at two bytes a
code or 2^20 at one; read the row at `1024` to see whether a kernel measured
there is bound by the machine or by itself.

    paper/bandwidth/run.sh [tag]        # writes results/<tag>.csv

The tag defaults to the EC2 instance type, else `uname -m`. On Linux the run
pins itself to the last core. Both columns are the same sequential read, eight
independent accumulators, min of several tries:

| column | meaning |
| --- | --- |
| `cold_gbs` | one pass over a buffer a gigabyte sweep has just evicted |
| `seq_gbs` | the same buffer read again and again |

The gap between them is the data landing in cache. `cold_gbs` is flat across
sizes, since the read comes from memory whatever the size is and the prefetcher
covers the latency after the first few lines. `seq_gbs` sits well above it
while the set fits in a cache and falls back onto it once it does not, so the
size where the two curves meet is the last cache the stream fits in.

Three things the benchmark has to do to be honest, all learned the hard way.
The buffer is filled from a runtime seed and hidden behind an asm barrier,
because a compiler that can see a `memset` constant folds the entire loop away
and reports six orders of magnitude too much. The cold pass is preceded by a
read through a gigabyte rather than a cache maintenance instruction, because
`dc civac` cleans only to the point of coherency and on Apple silicon the line
survives in the system level cache, which reads back faster than memory can
serve. And the clock is `CLOCK_MONOTONIC_RAW`, because a cold pass over the
smallest size takes about a microsecond and `CLOCK_MONOTONIC` on Darwin ticks
at exactly that.

## Machines

Single-core numbers, so the instance size is only about which caches a core
gets to itself. `run.sh` names each file after the instance type.

Caches are in `results/caches.csv`, per core except where a column says
otherwise. They are what the seq_gbs curve steps at, so read the two together.

| tag | core | L1d | L1i | L2 | L3 | note |
| --- | --- | --- | --- | --- | --- | --- |
| `m4pro` | Apple M4 Pro, P-core | 128 KiB | 192 KiB | 16 MiB, 4 cores | none | the dev MacBook, NEON |
| `c8g.xlarge` | Neoverse V2, Graviton 4 | 64 KiB | 64 KiB | 2 MiB | 36 MiB | has SVE2 the kernels do not use |
| `c7a.xlarge` | AMD EPYC 9R14, Zen 4 | 32 KiB | 32 KiB | 1 MiB | 16 MiB per complex | AVX-512 on a 256-bit datapath |
| `c7i.xlarge` | Xeon Platinum 8488C, Sapphire Rapids | 48 KiB | 32 KiB | 2 MiB | 105 MiB | native AVX-512 |
| `c8id.xlarge` | Xeon 6975P-C, Granite Rapids | 48 KiB | 64 KiB | 2 MiB | 480 MiB | the x86 benchmark box |
| `c8id.4xlarge` | the same, 8 cores | 48 KiB | 64 KiB | 2 MiB | 480 MiB | four times the cores, same per-core rate |

The M4 Pro also has four E-cores, 64 KiB L1d and 4 MiB of L2 between them, but
a release build pinned to nothing lands on a P-core.

Only `m4pro` has been rerun since the cold column was added; the other five
files still carry the old schema and need a run on their box.

On the M4 Pro the two curves say the whole story: 188 GB/s warm at 64 KiB in
L1, 109 to 113 anywhere inside the 16 MiB L2, and at 64 MiB the warm number
collapses onto the roughly 92 GB/s the cold pass reads at every size.

The sweep used to walk 8 MiB. There sequential read was 100 GB/s on the EPYC,
46 on Graviton 4 and 30 to 34 on both Intel generations, and a matcher
measured on the Intel cores read 30.2 GB/s whatever it computed. The
level-three a core reports says nothing about this, since the 480 MiB part is
the slowest of the three x86 cores at that size and the 16 MiB EPYC the
fastest, because its L3 is local to the core complex rather than across a
mesh. At the 1 MiB the sweep walks now, sequential read is 113 GB/s on the
M4 Pro, 100 on the EPYC, 76 on Graviton 4, 135 on Sapphire Rapids and 177 on
Granite Rapids. The EPYC reads the same at both sizes: its L2 is exactly
1 MiB, so the stream does not fit in it, and its local L3 already serves
100 GB/s.
