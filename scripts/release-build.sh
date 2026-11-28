#!/usr/bin/env bash
# Host release build of the workspace binaries (profile.release: ThinLTO, 16 codegen units,
# line tables). Records toolchain and revisions, copies binaries plus SHA-256 sums into
# artifacts/release/<timestamp>/. No target-cpu=native, panic=abort, static linking or PGO:
# the output runs on any x86_64 Linux host with the same glibc baseline as the build host.
# Strict gate: fails on a dirty or untracked tree, on unverified formatting (rustfmt missing
# counts as a failure), on a failed copy, and finds the binaries through `cargo metadata`.
set -uo pipefail
source "$(dirname "$0")/common.sh"

export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-$(bounded_jobs)}
start_log_directory release
record_environment

# Strict gate: a release needs a clean tree and verified formatting; neither is skipped.
run_step clean-tree git diff --quiet HEAD --
run_step untracked-files bash -c '[[ -z $(git status --porcelain --untracked-files=normal) ]]'
if "${cargo_command[@]}" fmt --version > /dev/null 2>&1; then
    run_step format "${cargo_command[@]}" fmt --all --check
else
    echo "FAIL format: rustfmt is not installed for this toolchain; formatting is unverified" >> "$summary"
    echo "==> format: rustfmt is not installed (strict release gate: FAIL)"
    failures=$(( failures + 1 ))
fi

run_step build "${cargo_command[@]}" build --locked --release --workspace --bins
# Wherever cargo actually put the binaries (CARGO_TARGET_DIR, build.target-dir, ...).
target_directory=$("${cargo_command[@]}" metadata --locked --format-version 1 --no-deps 2>/dev/null | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])') || target_directory=
copy_binaries() {
    [[ -n $target_directory ]] || { echo "cargo metadata gave no target directory"; return 1; }
    mkdir -p "$log_directory/bin" || return 1
    local binary
    for binary in terraplusminus; do
        cp -- "$target_directory/release/$binary" "$log_directory/bin/" || return 1
    done
    (cd "$log_directory/bin" && sha256sum -- terraplusminus > SHA256SUMS)
}
if run_step copy-binaries copy_binaries && [[ -f $log_directory/bin/SHA256SUMS ]]; then
    run_step smoke-test "$log_directory/bin/terraplusminus" column examples/offline-grand-canyon/terraplusminus.json5 35.995 -113.247
    sed 's/^/binary: /' "$log_directory/bin/SHA256SUMS" >> "$summary"
fi
finish
