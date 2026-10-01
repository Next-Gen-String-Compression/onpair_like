#!/bin/sh
# Builds and runs bw.c, writing results/<tag>.csv with the machine named in
# every row so the files concatenate.
#
#   paper/bandwidth/run.sh [tag]      default: the instance type, else uname -m
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)

# Instance type from IMDSv2, so an AWS box names itself.
instance() {
    token=$(curl -sf -X PUT "http://169.254.169.254/latest/api/token" \
        -H "X-aws-ec2-metadata-token-ttl-seconds: 60" 2>/dev/null) || return 1
    curl -sf -H "X-aws-ec2-metadata-token: $token" \
        "http://169.254.169.254/latest/meta-data/instance-type" 2>/dev/null
}

TAG=${1:-$(instance || uname -m)}
case $(uname -s) in
Darwin) CPU=$(sysctl -n machdep.cpu.brand_string); PIN="" ;;
*)      CPU=$(awk -F': ' '/model name/ {print $2; exit}' /proc/cpuinfo)
        # Arm Linux has no model name; the MIDR part number identifies the core.
        [ -n "$CPU" ] || CPU="$(uname -m) part $(awk -F': ' '/CPU part/ {print $2; exit}' /proc/cpuinfo)"
        # A free core, since one busy core halves nothing but noise costs runs.
        PIN="taskset -c $(($(nproc) - 1))" ;;
esac

# Caches, appended to results/caches.csv, since the curve steps at their sizes.
caches() {
    case $(uname -s) in
    Darwin)
        printf '%s,%s,%s,%s,%s,%s,,none\n' "$TAG" "$CPU P-core" \
            "$(sysctl -n hw.perflevel0.l1dcachesize)" \
            "$(sysctl -n hw.perflevel0.l1icachesize)" \
            "$(sysctl -n hw.perflevel0.l2cachesize)" \
            "$(sysctl -n hw.perflevel0.cpusperl2)" ;;
    *)
        one() { lscpu -C=NAME,ONE-SIZE --bytes | awk -v n="$1" '$1 == n {print $2}'; }
        printf '%s,%s,%s,%s,%s,1,%s,socket\n' "$TAG" "$CPU" \
            "$(one L1d)" "$(one L1i)" "$(one L2)" "$(one L3)" ;;
    esac
}
CACHES="$HERE/results/caches.csv"
[ -s "$CACHES" ] || echo "machine,core,l1d,l1i,l2,l2_shared_by,l3,l3_scope" > "$CACHES"
grep -q "^$TAG," "$CACHES" || caches >> "$CACHES"

cc -O3 -march=native -o "$HERE/bw" "$HERE/bw.c" 2>/dev/null ||
    cc -O3 -o "$HERE/bw" "$HERE/bw.c"

OUT="$HERE/results/$TAG.csv"
mkdir -p "$HERE/results"
{
    echo "machine,cpu,kib,cold_gbs,seq_gbs"
    $PIN "$HERE/bw" | tail -n +2 | while IFS= read -r row; do
        echo "$TAG,$CPU,$row"
    done
} > "$OUT"
rm -f "$HERE/bw"
echo "wrote $OUT"
