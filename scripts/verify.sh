#!/usr/bin/env bash
# Local verification: formatting, lints, tests, fixture integrity, dependency licenses and the
# no-Java production rule. Optional: --oracle regenerates the Java goldens and requires them
# to be byte-identical; --live runs network tests against real providers.
set -uo pipefail
source "$(dirname "$0")/common.sh"

with_oracle=false
with_live=false
for argument in "$@"; do
    case $argument in
        --oracle) with_oracle=true ;;
        --live) with_live=true ;;
        *) echo "usage: $0 [--oracle] [--live]" >&2; exit 2 ;;
    esac
done

export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-$(bounded_jobs)}
start_log_directory verify
record_environment

if "${cargo_command[@]}" fmt --version > /dev/null 2>&1; then
    run_step format "${cargo_command[@]}" fmt --all --check
else
    skip_step format "rustfmt is not installed for this toolchain"
fi
run_step lockfile "${cargo_command[@]}" metadata --locked --format-version 1 --no-deps
run_step clippy "${cargo_command[@]}" clippy --locked --workspace --all-targets -- -D warnings
run_step tests "${cargo_command[@]}" test --locked --workspace
run_step fixture-integrity scripts/check-fixtures.sh
run_step fixture-integrity-negative scripts/test-check-fixtures.sh
run_step dependency-licenses scripts/check-dependencies.sh
run_step staging-safety python3 scripts/test-stage-horizon-mod.py
run_step horizon-mod-against-stub scripts/check-horizon-mod.sh

if $with_oracle; then
    run_step oracle tools/oracle/build-oracle.sh
    run_step oracle-fixtures-unchanged git diff --exit-code -- fixtures
else
    skip_step oracle "pass --oracle (needs the pinned JDK/ECJ toolchain and upstream checkouts)"
fi
if $with_live; then
    run_step live-tests scripts/run-live-tests.sh
else
    skip_step live-tests "pass --live (needs access to elevation providers)"
fi
finish
