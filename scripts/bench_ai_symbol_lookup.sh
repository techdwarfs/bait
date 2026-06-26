#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 3 ]]; then
  echo "usage: $0 <bait-repo> <git-repo> <symbol-query> [runs]" >&2
  exit 1
fi

BAIT_REPO=$1
GIT_REPO=$2
QUERY=$3
RUNS=${4:-10}
ROOT=/Users/siddartha/source/bait
BAIT_BIN=${BAIT_BIN:-$ROOT/target/release/bait}

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

avg_time() {
  local cmd=$1
  local cwd=$2
  local runs=$3
  (
    cd "$cwd"
    for _ in $(seq 1 "$runs"); do
      /usr/bin/time -p sh -c "$cmd" 2>&1 | awk '/^real /{print $2}'
    done | awk '{s+=$1} END {printf "%.4f", s/NR}'
  )
}

ensure_bait_repo "$BAIT_REPO"
(
  cd "$BAIT_REPO"
  "$BAIT_BIN" ai index >/dev/null
)

BAIT_CMD="$BAIT_BIN ai find '$QUERY' >/dev/null"
GIT_CMD="git grep -n -F '$QUERY' >/dev/null"

(
  cd "$BAIT_REPO"
  sh -c "$BAIT_CMD"
)

(
  cd "$GIT_REPO"
  sh -c "$GIT_CMD"
)

BAIT_AVG=$(avg_time "$BAIT_CMD" "$BAIT_REPO" "$RUNS")
GIT_AVG=$(avg_time "$GIT_CMD" "$GIT_REPO" "$RUNS")

echo "dataset,bait_repo,git_repo,query,runs,bait_ai_find_avg_s,git_grep_avg_s"
echo "symbol_lookup,$BAIT_REPO,$GIT_REPO,$QUERY,$RUNS,$BAIT_AVG,$GIT_AVG"
