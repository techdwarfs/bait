#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 2 ]]; then
  echo "usage: $0 <bait-repo> <git-repo> [runs] [query ...]" >&2
  exit 1
fi

BAIT_REPO=$1
GIT_REPO=$2
RUNS=${3:-10}
shift 3 || true

ROOT=/Users/siddartha/source/bait
BAIT_BIN=${BAIT_BIN:-$ROOT/target/release/bait}

if [[ $# -gt 0 ]]; then
  QUERIES=("$@")
else
  QUERIES=("EchoRunner")
fi

cd "$ROOT"
cargo build --release -p bait >/dev/null

ensure_bait_repo() {
  local repo=$1
  if [[ ! -d "$repo/.bait" ]]; then
    "$BAIT_BIN" init "$repo" >/dev/null
    (cd "$repo" && "$BAIT_BIN" config user.name 'Bench User' >/dev/null)
    (cd "$repo" && "$BAIT_BIN" config user.email 'bench@example.com' >/dev/null)
  fi
}

measure_cmd() {
  local cmd=$1
  local cwd=$2
  local runs=$3

  (
    cd "$cwd"
    local out_file
    out_file=$(mktemp)
    trap 'rm -f "$out_file"' RETURN

    for _ in $(seq 1 "$runs"); do
      : >"$out_file"
      local start
      local end
      start=$(perl -MTime::HiRes=time -e 'printf "%.9f", time()')
      sh -c "$cmd" >"$out_file"
      end=$(perl -MTime::HiRes=time -e 'printf "%.9f", time()')
      local real
      local bytes
      real=$(awk -v s="$start" -v e="$end" 'BEGIN {printf "%.6f", e - s}')
      bytes=$(wc -c <"$out_file" | tr -d ' ')
      echo "$real,$bytes"
    done
  ) | awk -F, '{ts+=$1; bs+=$2; n+=1} END {if (n==0) {print "0.0000,0"} else {printf "%.4f,%d", ts/n, int(bs/n)}}'
}

estimate_tokens() {
  local bytes=$1
  awk -v b="$bytes" 'BEGIN {printf "%d", int((b + 3) / 4)}'
}

ensure_bait_repo "$BAIT_REPO"
(cd "$BAIT_REPO" && "$BAIT_BIN" ai index >/dev/null)

echo "query,runs,bait_avg_s,git_avg_s,time_speedup_x,bait_avg_bytes,git_avg_bytes,bait_est_tokens,git_est_tokens,token_reduction_pct"

sum_bait_s=0
sum_git_s=0
sum_bait_tokens=0
sum_git_tokens=0
count=0

for query in "${QUERIES[@]}"; do
  BAIT_CMD="$BAIT_BIN ai find '$query'"
  GIT_CMD="git grep -n -F '$query'"

  if ! (cd "$BAIT_REPO" && sh -c "$BAIT_CMD" >/dev/null 2>&1); then
    echo "warning: skipping query '$query' (no BAIT results)" >&2
    continue
  fi
  if ! (cd "$GIT_REPO" && sh -c "$GIT_CMD" >/dev/null 2>&1); then
    echo "warning: skipping query '$query' (no Git results)" >&2
    continue
  fi

  bait_metrics=$(measure_cmd "$BAIT_CMD" "$BAIT_REPO" "$RUNS")
  git_metrics=$(measure_cmd "$GIT_CMD" "$GIT_REPO" "$RUNS")

  bait_avg_s=${bait_metrics%%,*}
  bait_avg_bytes=${bait_metrics##*,}
  git_avg_s=${git_metrics%%,*}
  git_avg_bytes=${git_metrics##*,}

  bait_tokens=$(estimate_tokens "$bait_avg_bytes")
  git_tokens=$(estimate_tokens "$git_avg_bytes")

  speedup=$(awk -v g="$git_avg_s" -v b="$bait_avg_s" 'BEGIN {if (b<=0) print "inf"; else printf "%.2f", g/b}')
  token_reduction=$(awk -v g="$git_tokens" -v b="$bait_tokens" 'BEGIN {if (g<=0) print "0.00"; else printf "%.2f", ((g-b)/g)*100}')

  echo "$query,$RUNS,$bait_avg_s,$git_avg_s,$speedup,$bait_avg_bytes,$git_avg_bytes,$bait_tokens,$git_tokens,$token_reduction"

  sum_bait_s=$(awk -v s="$sum_bait_s" -v v="$bait_avg_s" 'BEGIN {printf "%.6f", s+v}')
  sum_git_s=$(awk -v s="$sum_git_s" -v v="$git_avg_s" 'BEGIN {printf "%.6f", s+v}')
  sum_bait_tokens=$((sum_bait_tokens + bait_tokens))
  sum_git_tokens=$((sum_git_tokens + git_tokens))
  count=$((count + 1))
done

if [[ $count -gt 0 ]]; then
  avg_bait_s=$(awk -v s="$sum_bait_s" -v n="$count" 'BEGIN {printf "%.4f", s/n}')
  avg_git_s=$(awk -v s="$sum_git_s" -v n="$count" 'BEGIN {printf "%.4f", s/n}')
  avg_bait_tokens=$((sum_bait_tokens / count))
  avg_git_tokens=$((sum_git_tokens / count))
  avg_speedup=$(awk -v g="$avg_git_s" -v b="$avg_bait_s" 'BEGIN {if (b<=0) print "inf"; else printf "%.2f", g/b}')
  avg_token_reduction=$(awk -v g="$avg_git_tokens" -v b="$avg_bait_tokens" 'BEGIN {if (g<=0) print "0.00"; else printf "%.2f", ((g-b)/g)*100}')
  echo "SUMMARY,$count,$avg_bait_s,$avg_git_s,$avg_speedup,NA,NA,$avg_bait_tokens,$avg_git_tokens,$avg_token_reduction"
else
  echo "warning: no valid queries were benchmarked" >&2
  exit 2
fi