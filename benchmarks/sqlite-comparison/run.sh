#!/usr/bin/env bash
# Repeats the comparison so run-to-run noise is visible. Usage: ./run.sh <machine-label>
# Environment: REPEATS (default 3), RECORDS (default "10000 100000"), DIR (database location).
set -euo pipefail
cd "$(dirname "$0")"
label="${1:?usage: ./run.sh <machine-label>}"
cargo build --release
mkdir -p results
for records in ${RECORDS:-10000 100000}; do
  for run in $(seq 1 "${REPEATS:-3}"); do
    base="results/$(date +%F)-${label}-${records}-run${run}"
    echo "records=${records} run=${run}" >&2
    ./target/release/cedrodb-sqlite-comparison --records "$records" --iterations 20 ${DIR:+--dir "$DIR"} > "${base}.json" 2> "${base}.md"
  done
done
