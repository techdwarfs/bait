#!/usr/bin/env bash
set -euo pipefail

ROOT=/Users/siddartha/source/bait
BAIT_BIN=${BAIT_BIN:-$ROOT/target/release/bait}
SUITE_ROOT=${SUITE_ROOT:-/tmp/bait-ai-suite}
RUNS=${1:-5}

cd "$ROOT"
cargo build --release -p bait >/dev/null

mkdir -p "$SUITE_ROOT"

create_repo_pair() {
  local name=$1
  local seed=$2
  local file_count=$3
  local symbol_prefix=$4
  local query_symbol=$5

  local bait_repo="$SUITE_ROOT/$name-bait"
  local git_repo="$SUITE_ROOT/$name-git"

  rm -rf "$bait_repo" "$git_repo"
  mkdir -p "$bait_repo" "$git_repo"

  "$BAIT_BIN" init "$bait_repo" >/dev/null
  (cd "$bait_repo" && "$BAIT_BIN" config user.name 'Bench User' >/dev/null)
  (cd "$bait_repo" && "$BAIT_BIN" config user.email 'bench@example.com' >/dev/null)

  (cd "$git_repo" && git init >/dev/null)
  (cd "$git_repo" && git config user.name 'Bench User')
  (cd "$git_repo" && git config user.email 'bench@example.com')

  for i in $(seq 1 "$file_count"); do
    local ext
    case $(( (i + seed) % 3 )) in
      0) ext=ts ;;
      1) ext=rs ;;
      *) ext=md ;;
    esac

    local file_name="$symbol_prefix-$i.$ext"
    local symbol_name
    if [[ $i -eq 1 ]]; then
      symbol_name=$query_symbol
    else
      symbol_name="${symbol_prefix}${i}Runner"
    fi
    cat >"$bait_repo/$file_name" <<EOF
export function ${symbol_name}() {
  return "${name}-${i}";
}

export const ${symbol_prefix}${i}Repository = {
  name: "${name}",
  symbol: "${symbol_name}"
};
EOF
    cp "$bait_repo/$file_name" "$git_repo/$file_name"
  done

  (cd "$bait_repo" && "$BAIT_BIN" add . >/dev/null)
  (cd "$bait_repo" && "$BAIT_BIN" save --message "seed ${name}" >/dev/null)

  (cd "$git_repo" && git add . >/dev/null)
  (cd "$git_repo" && git commit -m "seed ${name}" >/dev/null)

  echo "$bait_repo|$git_repo"
}

run_case() {
  local name=$1
  local repo_pair=$2
  local query=$3

  local bait_repo=${repo_pair%%|*}
  local git_repo=${repo_pair##*|}
  local result

  result=$(
    "$ROOT/scripts/bench_ai_agent_savings.sh" "$bait_repo" "$git_repo" "$RUNS" "$query"
  )

  echo "## ${name}"
  echo "$result"
  echo
}

echo "BAIT AI multi-repo savings suite"
echo "suite_root,$SUITE_ROOT"
echo "runs,$RUNS"
echo

small_pair=$(create_repo_pair "small" 1 12 "Echo" "EchoRunner")
docs_pair=$(create_repo_pair "docs" 2 18 "Repo" "Repository")
large_pair=$(create_repo_pair "large" 3 30 "Trace" "TraceRunner")

run_case "Small code repo" "$small_pair" "EchoRunner"
run_case "Docs-heavy repo" "$docs_pair" "Repository"
run_case "Large mixed repo" "$large_pair" "TraceRunner"