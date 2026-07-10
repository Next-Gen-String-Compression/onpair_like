#!/usr/bin/env python3
"""Two-axis LIKE-benchmark analyzer -> analysis/summary.json (feeds report.py).

Ingests one or more results.jsonl trees and derives the parameter-space map:
  - compression:  per (candidate,config,dataset) ratio + build/decode MB/s
  - winner_map:   per (dataset,op,len-bucket,sel-band) engine ranking by ns/row
  - pareto:       per dataset, per candidate: (compression ratio, contains ns/row)
  - onpair_vs_unc: per (dataset,op) grid of onpair/uncompressed latency ratio
  - candidate_matrix: per-candidate roll-up (strength/weakness table)
Pure stdlib.
"""
import json, sys, os, glob
from collections import defaultdict

# ---------------- loading ----------------
def load_rows(dirs):
    builds, queries, manifests = [], [], {}
    skipped = []
    for d in dirs:
        rp = os.path.join(d, "results.jsonl")
        if not os.path.exists(rp):
            continue
        mp = os.path.join(d, "manifest.json")
        if os.path.exists(mp):
            try: manifests[d] = json.load(open(mp))
            except Exception: pass
        with open(rp) as f:
            for ln, line in enumerate(f, 1):
                line = line.strip()
                if not line: continue
                try:
                    r = json.loads(line)          # tolerate a truncated final row from a crashed worker
                except Exception:
                    skipped.append((d, ln)); continue
                r["_src"] = d
                if r.get("kind") == "build": builds.append(r)
                elif r.get("kind") == "query": queries.append(r)
    if skipped:
        print(f"WARN: skipped {len(skipped)} malformed line(s): " +
              ", ".join(f"{d}:{ln}" for d,ln in skipped), file=sys.stderr)
    return builds, queries, manifests

def cfg_label(candidate, config):
    try: c = json.loads(config) if config else {}
    except Exception: c = {}
    if not c: return candidate
    if "level" in c: return f"{candidate}-l{c['level']}"
    if "bits" in c: return f"{candidate}-b{c['bits']}"
    return f"{candidate}[" + ",".join(f"{k}={v}" for k,v in sorted(c.items())) + "]"

def median(xs):
    xs = sorted(xs); n = len(xs)
    if n == 0: return None
    return xs[n//2] if n % 2 else 0.5*(xs[n//2-1]+xs[n//2])

# ---------------- bucketing (map axes) ----------------
def len_bucket(n):
    n = n or 1
    for hi,lab in [(2,"1-2"),(4,"3-4"),(8,"5-8"),(16,"9-16"),(32,"17-32")]:
        if n <= hi: return lab
    return "33+"
LEN_ORDER = ["1-2","3-4","5-8","9-16","17-32","33+"]

def sel_band(s):
    if s <= 0: return "0"
    for hi,lab in [(1e-4,"<1e-4"),(1e-3,"1e-4-1e-3"),(1e-2,"1e-3-1e-2"),(1e-1,"1e-2-1e-1")]:
        if s < hi: return lab
    return ">=1e-1"
SEL_ORDER = ["0","<1e-4","1e-4-1e-3","1e-3-1e-2","1e-2-1e-1",">=1e-1"]

# ---------------- compression axis ----------------
def analyze_compression(builds, queries):
    # Group build rows by (cand,cfg,ds,chunk_rows). One build row per group per source
    # dir, so ACROSS dirs the rows are duplicates (same dataset+codec => identical
    # footprint): dedup (take the value, never sum), median the single-shot build_ns.
    # Summing across dirs would inflate raw_bytes and thus every throughput. Throughput
    # is over the RAW PAYLOAD (raw - uncompressed offsets), the metric codecs.toml defines.
    bgroups = defaultdict(list)
    for b in builds:
        bgroups[(b["candidate"], b.get("config","{}"), b["dataset"], b.get("chunk_rows",0))].append(b)
    dec = defaultdict(list)
    for q in queries:
        if q.get("strategy") == "decode":
            dn = ((q.get("prefilter") or {}).get("decode_ns") or {}).get("ns")
            if dn: dec[(q["candidate"], q.get("config","{}"), q["dataset"], q.get("chunk_rows",0))].append(dn)
    recs = {}
    for key,rows in bgroups.items():
        cand,cfg,ds,chunk = key
        raw   = max(r.get("raw_bytes",0) for r in rows)
        foot  = max(r.get("footprint_total_bytes",0) for r in rows)
        payload = max((r.get("footprint_components",{}) or {}).get("payload",0) for r in rows)
        offsets = max((r.get("footprint_components",{}) or {}).get("offsets",0) for r in rows)
        build_ns = median([r.get("build_ns",0) for r in rows if r.get("build_ns")])
        dm = median(dec.get(key,[]))
        raw_payload = raw - offsets                       # decoded/compressed bytes (offsets stored raw)
        stored_payload = payload if payload else (foot - offsets)
        rec = {
            "candidate":cand,"config":cfg,"label":cfg_label(cand,cfg),"dataset":ds,"chunk_rows":chunk,
            "raw_bytes":raw,"footprint_bytes":foot,"payload_bytes":payload,"offset_bytes":offsets,
            "ratio_total": raw/foot if foot else None,
            "ratio_payload": raw_payload/stored_payload if stored_payload else None,
            "build_mbps": (raw_payload/1e6)/(build_ns/1e9) if build_ns else None,
            "decode_ns_median": dm,
            "decode_mbps": (raw_payload/1e6)/(dm/1e9) if dm else None,
        }
        k2 = (cand,cfg,ds)
        if k2 not in recs or chunk == 0:   # one record per (cand,cfg,ds); prefer whole-column (chunk 0)
            recs[k2] = rec
    return list(recs.values())

# ---------------- query axis ----------------
def engine_id(cand_label, strat, scanner):
    return f"{cand_label}:{scanner}" if strat == "direct" else f"{cand_label}:{strat}"

def collect_points(queries):
    pts = []
    for q in queries:
        if q.get("status") != "ok": continue
        med = (q.get("latency") or {}).get("median_ns")
        if med is None: continue
        der = q.get("derived",{}) or {}
        nlt = der.get("needle_len_total") or (q.get("meta",{}).get("gen",{}) or {}).get("target_len") or 1
        sel = der.get("selectivity", 0.0) or 0.0
        cl = cfg_label(q["candidate"], q.get("config","{}"))
        pts.append({
            "dataset":q["dataset"],"op":q["op"],
            "candidate":cl,"strategy":q.get("strategy",""),"scanner":q.get("scanner",""),
            "engine":engine_id(cl,q.get("strategy",""),q.get("scanner","")),
            "needle_len":nlt,"len_bucket":len_bucket(nlt),
            "selectivity":sel,"sel_band":sel_band(sel),
            "median_ns":med,"ns_per_row":q.get("ns_per_row"),"gbps_raw":q.get("gbps_raw"),
        })
    return pts

def winner_map(points):
    cells = defaultdict(list)
    for p in points:
        if p["ns_per_row"] is not None:
            cells[(p["dataset"],p["op"],p["len_bucket"],p["sel_band"])].append(p)
    out = []
    for (ds,op,lb,sb),ps in cells.items():
        by = defaultdict(list)
        for p in ps: by[p["engine"]].append(p["ns_per_row"])
        rank = sorted(({"engine":e,"ns":median(v),"n":len(v)} for e,v in by.items()), key=lambda r:r["ns"])
        out.append({"dataset":ds,"op":op,"len_bucket":lb,"sel_band":sb,
                    "winner":rank[0]["engine"],"winner_ns":rank[0]["ns"],
                    "ranking":rank,"n_queries":len(ps)})
    return out

def onpair_vs_unc(points):
    """per (dataset,op,len,sel): median ns/row for onpair(any) and uncompressed(memmem), ratio."""
    grp = defaultdict(lambda: {"onpair":[], "unc":[]})
    for p in points:
        k = (p["dataset"],p["op"],p["len_bucket"],p["sel_band"])
        if p["candidate"].startswith("onpair") and p["strategy"] in ("compressed","interp"):
            grp[k]["onpair"].append(p["ns_per_row"])
        elif p["candidate"]=="uncompressed" and p["strategy"]=="direct":
            grp[k]["unc"].append(p["ns_per_row"])
    out = []
    for (ds,op,lb,sb),v in grp.items():
        o,u = median([x for x in v["onpair"] if x is not None]), median([x for x in v["unc"] if x is not None])
        if o and u:
            out.append({"dataset":ds,"op":op,"len_bucket":lb,"sel_band":sb,
                        "onpair_ns":o,"unc_ns":u,"ratio":o/u})
    return out

def pareto(builds, queries):
    """per dataset, per candidate: compression ratio (build) x contains latency (best applicable strategy)."""
    # ratio per (cand,cfg,ds) from builds (prefer max-config? keep each config as a point)
    comp = {(c["candidate"],c["config"],c["dataset"]): c for c in analyze_compression(builds, queries)}
    # contains latency per (cand,cfg,ds): median ns/row over contains queries, best strategy
    lat = defaultdict(lambda: defaultdict(list))  # (cand,cfg,ds) -> strat -> [ns/row]
    for q in queries:
        if q.get("op")!="contains" or q.get("status")!="ok": continue
        npr = q.get("ns_per_row")
        if npr is None: continue
        lat[(q["candidate"],q.get("config","{}"),q["dataset"])][q.get("strategy","")].append(npr)
    out = defaultdict(list)
    for key,byst in lat.items():
        cand,cfg,ds = key
        # pick best (lowest median) applicable strategy that ISN'T decode when a native path exists
        strat_med = {s:median(v) for s,v in byst.items() if v}
        if not strat_med: continue
        best_s = min(strat_med, key=strat_med.get)
        c = comp.get(key)
        ratio = c["ratio_total"] if c else None
        out[ds].append({
            "candidate":cand,"config":cfg,"label":cfg_label(cand,cfg),
            "ratio":ratio,"ns_per_row":strat_med[best_s],"strategy":best_s,
            "all_strategies":{s:m for s,m in strat_med.items()},
            "decode_mbps": c["decode_mbps"] if c else None,
            "build_mbps": c["build_mbps"] if c else None,
        })
    return out

def candidate_matrix(comp, pareto_by_ds, points):
    """per base-candidate roll-up across datasets: ratio range, decode MB/s, contains ns/row, native path."""
    cands = defaultdict(lambda: {"ratios":[], "decode":[], "build":[], "contains":[], "ops":set(), "strategies":set()})
    for c in comp:
        base = c["candidate"]
        if c["ratio_total"]: cands[base]["ratios"].append((c["dataset"],c["ratio_total"]))
        if c["decode_mbps"]: cands[base]["decode"].append(c["decode_mbps"])
        if c["build_mbps"]: cands[base]["build"].append(c["build_mbps"])
    for ds,rows in pareto_by_ds.items():
        for r in rows:
            base = r["candidate"]
            if r["ns_per_row"] is not None: cands[base]["contains"].append((ds,r["ns_per_row"],r["strategy"]))
    for p in points:
        base = p["candidate"].split("-")[0].split("[")[0]
        cands[base]["ops"].add(p["op"]); cands[base]["strategies"].add(p["strategy"])
    out = {}
    for base,v in cands.items():
        rr = [r for _,r in v["ratios"]]
        out[base] = {
            "candidate":base,
            "ratio_min":min(rr) if rr else None,"ratio_max":max(rr) if rr else None,
            "ratios_by_ds":dict(v["ratios"]),
            "decode_mbps_med": median(v["decode"]) if v["decode"] else None,
            "build_mbps_med": median(v["build"]) if v["build"] else None,
            "contains_by_ds":{ds:(ns,st) for ds,ns,st in v["contains"]},
            "ops":sorted(v["ops"]),"strategies":sorted(v["strategies"]),
        }
    return out

def main():
    if len(sys.argv) < 2:
        print("usage: analyze.py <result_dir|glob-parent> ...", file=sys.stderr); sys.exit(2)
    dirs = []
    for a in sys.argv[1:]:
        if os.path.isdir(a) and os.path.exists(os.path.join(a,"results.jsonl")): dirs.append(a)
        else: dirs.extend(sorted(d for d in glob.glob(os.path.join(a,"*")) if os.path.exists(os.path.join(d,"results.jsonl"))))
    dirs = sorted(set(dirs))
    builds, queries, manifests = load_rows(dirs)
    comp = analyze_compression(builds, queries)
    points = collect_points(queries)
    wm = winner_map(points)
    ovu = onpair_vs_unc(points)
    par = pareto(builds, queries)
    cmatrix = candidate_matrix(comp, par, points)
    env = {}
    for m in manifests.values():
        e = m.get("environment") or m.get("env") or {}
        if e: env = e; break
    summary = {
        "sources":dirs,"manifest_env":env,
        "n_build_rows":len(builds),"n_query_rows":len(queries),
        "datasets":sorted({q["dataset"] for q in queries} | {b["dataset"] for b in builds}),
        "ops":sorted({p["op"] for p in points}),
        "engines":sorted({p["engine"] for p in points}),
        "compression":comp,"winner_map":wm,"onpair_vs_unc":ovu,
        "pareto":par,"candidate_matrix":cmatrix,"points":points,
        "len_order":LEN_ORDER,"sel_order":SEL_ORDER,
    }
    os.makedirs("analysis", exist_ok=True)
    json.dump(summary, open("analysis/summary.json","w"))
    # digest
    print(f"loaded {len(builds)} build + {len(queries)} query rows from {len(dirs)} dirs")
    print("datasets:", summary["datasets"]); print("ops:", summary["ops"])
    print(f"engines ({len(summary['engines'])}):", summary["engines"])
    print("\n== CANDIDATE MATRIX ==")
    for base,m in sorted(cmatrix.items()):
        rmin,rmax = m["ratio_min"],m["ratio_max"]
        rr = f"{rmin:.2f}-{rmax:.2f}" if rmin else "  -  "
        dm = f"{m['decode_mbps_med']:.0f}" if m['decode_mbps_med'] else "-"
        print(f"  {base:<14} ratio={rr:<12} decode={dm:>7} MB/s  ops={m['ops']}  strat={m['strategies']}")
    print("\n== QUERY WINNERS (freq per dataset) ==")
    for ds in summary["datasets"]:
        cells=[w for w in wm if w["dataset"]==ds]
        if not cells: continue
        freq=defaultdict(int)
        for w in cells: freq[w["winner"]]+=1
        print(f"  [{ds}] {len(cells)} cells:", dict(sorted(freq.items(), key=lambda x:-x[1])))

if __name__ == "__main__":
    main()
