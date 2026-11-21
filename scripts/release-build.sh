#!/usr/bin/env bash
# Host release build of the workspace binaries (profile.release: ThinLTO, 16 codegen units,
# line tables). Records toolchain and revisions, copies binaries plus SHA-256 sums into
# artifacts/release/<timestamp>/. No target-cpu=native, panic=abort, static linking or PGO:
# the output runs on any x86_64 Linux host with the same glibc baseline as the build host.
set -uo pipefail
source "$(dirname "$0")/common.sh"

export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-$(bounded_jobs)}
start_log_directory release
record_environment
git diff --quiet HEAD -- || echo "WARNING: building from a dirty working tree" | tee -a "$summary"

run_step build "${cargo_command[@]}" build --locked --release --workspace --bins
if (( failures == 0 )); then
    mkdir -p "$log_directory/bin"
    for binary in terraplusminus; do
        cp "target/release/$binary" "$log_directory/bin/"
    done
    (cd "$log_directory/bin" && sha256sum -- * > SHA256SUMS)
    run_step smoke-test "$log_directory/bin/terraplusminus" column examples/offline-grand-canyon/terraplusminus.json5 35.995 -113.247
    sed 's/^/binary: /' "$log_directory/bin/SHA256SUMS" >> "$summary"
fi
finish
