#!/usr/bin/env bash
# Dependency policy: every package in Cargo.lock is either a workspace crate or carries an
# allowed license, and production crates never start a JVM or ship Java.
set -euo pipefail
cd "$(dirname "$0")/.."
allowed='^(MIT|Apache-2.0|MIT OR Apache-2.0|Apache-2.0 OR MIT|BSD-3-Clause|ISC|Zlib|Unicode-3.0)$'
workspace_crates=$(sed -n 's|^ *"crates/\([^"]*\)".*|\1|p' Cargo.toml)
status=0
while read -r name; do
    if grep -qx "$name" <<< "$workspace_crates"; then
        manifest="crates/$name/Cargo.toml"
        grep -q 'license.workspace = true' "$manifest" || { echo "$name: license not inherited"; status=1; }
        continue
    fi
    license=$(find "${CARGO_HOME:-$HOME/.cargo}/registry/src" -maxdepth 2 -type d -name "$name-*" -exec sed -n 's/^license = "\(.*\)"/\1/p' {}/Cargo.toml \; 2>/dev/null | head -n1)
    if [[ ! $license =~ $allowed ]]; then
        echo "$name: license '${license:-unknown}' not in the allowed list"
        status=1
    fi
done < <(sed -n 's/^name = "\(.*\)"/\1/p' Cargo.lock)
if grep -rnE 'Command::new\("java|\.jar"|libjvm|jni::' crates; then
    echo "production crates must not run or embed Java"
    status=1
fi
grep -q '^license = "MIT"' Cargo.toml || { echo "workspace license must be MIT"; status=1; }
[[ $status == 0 ]] && echo "dependency policy satisfied ($(grep -c '^name = ' Cargo.lock) packages)"
exit $status
