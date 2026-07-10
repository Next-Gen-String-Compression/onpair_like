#!/bin/bash
cd "$(dirname "$0")/.."
until [ -f results/cx-shootout-rest/results.jsonl ]; do sleep 10; done
sleep 3
echo "=== cx-shootout-rest landed ==="
python3 - <<'PY'
import json
from collections import defaultdict
rows=[json.loads(l) for l in open("results/cx-shootout-rest/results.jsonl")]
q=[r for r in rows if r.get("kind")=="query" and r.get("status")=="ok" and (r.get("latency") or {}).get("median_ns")]
print("total rows:",len(rows),"ok-query:",len(q))
print("gate_fail:",sum(1 for r in rows if r.get("kind")=="query" and (r.get("gate") or {}).get("hash_ok") is False))
print("datasets:",sorted({r['dataset'] for r in q}))
# anomaly scan: median/max ns_per_row and max_ns outliers. A sleep-poisoned sample
# would show max_ns in the 100s of ms..minutes; ns/row wildly above the column norm.
for ds in sorted({r['dataset'] for r in q}):
    qq=[r for r in q if r['dataset']==ds]
    nspr=sorted(r['ns_per_row'] for r in qq if r.get('ns_per_row'))
    maxns=sorted(((r['latency']['max_ns'],r['latency']['median_ns'],r.get('scanner'),r.get('op'),r.get('query_id')) for r in qq))
    med=nspr[len(nspr)//2]
    p99=nspr[int(len(nspr)*0.99)]
    hi=maxns[-3:]
    print(f"\n[{ds}] n={len(qq)} ns/row median={med:.2f} p99={p99:.2f} max={nspr[-1]:.2f}")
    print("  top max_ns (ns):")
    for mx,mdn,sc,op,qid in hi:
        print(f"    max={mx/1e6:.1f}ms median={mdn/1e6:.2f}ms  {sc}/{op}  {qid}")
    # flag: any max_ns > 5s is almost certainly sleep-poisoned
    poisoned=[m for m in maxns if m[0]>5e9]
    print(f"  >>> samples with max_ns>5s (sleep-suspect): {len(poisoned)}")
PY
