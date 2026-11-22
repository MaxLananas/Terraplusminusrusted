#!/usr/bin/env bash
# Runs the live provider tests (every ignored test named `live_*`) and fails unless each of them
# ran and passed: an empty selection, a filtered-out test or a skipped one is a failure, never a
# pass.
set -euo pipefail
cd "$(dirname "$0")/.."
read -r -a cargo_command <<< "${CARGO_COMMAND:-cargo}"

expected=$(grep -rhoE '^fn live_[a-z0-9_]+' crates --include='*.rs' | sort -u | wc -l)
if (( expected == 0 )); then
    echo "FAIL: no live tests are defined" >&2
    exit 1
fi
output=$(mktemp)
trap 'rm -f "$output"' EXIT
status=0
TERRA_LIVE_TESTS=1 "${cargo_command[@]}" test --locked --workspace -- --ignored --exact $(grep -rhoE '^fn live_[a-z0-9_]+' crates --include='*.rs' | sort -u | sed 's/^fn //') 2>&1 | tee "$output" || status=$?
passed=$(grep -cE '^test live_[a-z0-9_]+ \.\.\. ok$' "$output" || true)
echo "live tests: $passed of $expected passed"
if (( status != 0 || passed != expected )); then
    echo "FAIL: every live test must run and pass" >&2
    exit 1
fi
