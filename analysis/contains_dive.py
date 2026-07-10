#!/usr/bin/env python3
"""Deep dive: when is onpair-compressed CONTAINS strong/weak?"""
import json, glob
from collections import defaultdict

def load(p):
    o=[]
    for l in open(p):
        l=l.strip()
        if not l: continue
        try: o.append(json.loads(l))
        except: pass
    return o

rows=[]
for f in glob.glob("results/cx-query/*/results.jsonl")+glob.glob("results/cx-shootout-rest/results.jsonl")+glob.glob("results/cx-probe/results.jsonl"):
    rows+=load(f)
def med(x):
    x=sorted(v for v in x if v is not None); return x[len(x)//2] if x else None

build=[r for r in rows if r.get("kind")=="build"]
ratio={}; rowlen={}
for r in build:
    if r["candidate"]=="onpair":
        ratio[r["dataset"]]=r["raw_bytes"]/r["footprint_total_bytes"]
        off=(r.get("footprint_components",{}) or {}).get("offsets",0)
        if off: rowlen[r["dataset"]]=(r["raw_bytes"]-off)/(off/8)

q=[r for r in rows if r.get("kind")=="query" and r.get("op")=="contains"
   and r.get("status")=="ok" and r.get("ns_per_row") is not None]

def sel(r): return (r.get("derived",{}) or {}).get("selectivity",0) or 0
def nlen(r): return (r.get("derived",{}) or {}).get("needle_len_total") or 1
def onp_rows(ds=None):
    return [r for r in q if r['candidate']=='onpair' and r['strategy']=='compressed' and (ds is None or r['dataset']==ds)]
def scan_rows(ds=None,scanner=None):
    return [r for r in q if r['candidate']=='uncompressed' and r.get('strategy')=='direct'
            and (ds is None or r['dataset']==ds) and (scanner is None or r.get('scanner')==scanner)]

def best_scanner(ds):
    by=defaultdict(list)
    for r in scan_rows(ds): by[r['scanner']].append(r['ns_per_row'])
    by={k:v for k,v in by.items()}
    if len(by)<2: return None
    bs=min(by,key=lambda s:med(by[s])); return bs,med(by[bs])

print("="*78)
print("CONTAINS per column — onpair·compressed vs memmem vs best scanner")
print("="*78)
print(f"{'column':<20}{'rowlen':>7}{'ratio':>6}{'onpair':>8}{'memmem':>8}{'onp/mm':>7}{'best-scanner':>19}{'onp/best':>9}")
for ds in sorted({r['dataset'] for r in q}):
    o=med([r['ns_per_row'] for r in onp_rows(ds)])
    mm=med([r['ns_per_row'] for r in scan_rows(ds,'memmem')])
    b=best_scanner(ds)
    rl=rowlen.get(ds,0); rt=ratio.get(ds,0)
    bt=f"{b[1]:.2f} ({b[0]})" if b else "(memmem only)"
    ob=f"{o/b[1]:.2f}x" if b and o else "-"
    print(f"{ds:<20}{rl:>7.0f}{rt:>6.2f}{o:>8.2f}{mm:>8.2f}{o/mm:>6.2f}x{bt:>19}{ob:>9}")

print("\n"+"="*78)
print("CONTAINS vs BEST scanner, by NEEDLE LENGTH (clickbench+dbpedia, full roster)")
print("="*78)
LB=[(1,2,"1-2"),(3,4,"3-4"),(5,8,"5-8"),(9,16,"9-16"),(17,999,"17+")]
for ds in ["clickbench-url-1m","dbpedia-abstract","msmarco-query"]:
    b=best_scanner(ds)
    if not b:
        print(f"\n[{ds}] no full roster (shootout data missing)"); continue
    print(f"\n[{ds}]  (best scanner overall: {b[0]})")
    print(f"  {'needle_len':<12}{'onpair':>9}{'best-scan':>11}{'onp/best':>9}{'n':>4}")
    for lo,hi,lab in LB:
        orows=[r['ns_per_row'] for r in onp_rows(ds) if lo<=nlen(r)<=hi]
        # best scanner per this bucket
        by=defaultdict(list)
        for r in scan_rows(ds):
            if lo<=nlen(r)<=hi: by[r['scanner']].append(r['ns_per_row'])
        if not orows or not by: continue
        o=med(orows); bs=min(by,key=lambda s:med(by[s])); bm=med(by[bs])
        print(f"  {lab:<12}{o:>9.2f}{bm:>9.2f}({bs[:3]}){o/bm:>8.2f}x{len(orows):>4}")

print("\n"+"="*78)
print("CONTAINS vs BEST scanner, by SELECTIVITY (clickbench+dbpedia)")
print("="*78)
SB=[(0,0,"zero"),(1e-9,1e-4,"<1e-4"),(1e-4,1e-3,"1e-4..3"),(1e-3,1e-2,"1e-3..2"),(1e-2,1e-1,"1e-2..1"),(1e-1,1,">=1e-1")]
for ds in ["clickbench-url-1m","dbpedia-abstract"]:
    b=best_scanner(ds)
    if not b: continue
    print(f"\n[{ds}]")
    print(f"  {'selectivity':<12}{'onpair':>9}{'best-scan':>13}{'onp/best':>9}{'n':>4}")
    for lo,hi,lab in SB:
        def inb(r):
            s=sel(r); return (s==0 and lab=="zero") or (lo<s<=hi and lab!="zero")
        orows=[r['ns_per_row'] for r in onp_rows(ds) if inb(r)]
        by=defaultdict(list)
        for r in scan_rows(ds):
            if inb(r): by[r['scanner']].append(r['ns_per_row'])
        if not orows or not by: continue
        o=med(orows); bs=min(by,key=lambda s:med(by[s])); bm=med(by[bs])
        print(f"  {lab:<12}{o:>9.2f}{bm:>9.2f}({bs[:4]}){o/bm:>8.2f}x{len(orows):>4}")
