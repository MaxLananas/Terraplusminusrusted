#!/usr/bin/env bash
# Stages the Horizon mod with the development API stub (tools/horizon-api-stub) and runs
# `cargo clippy` on it. Proves the mod's own code is well-typed against the API signatures it
# relies on; it is not a build against the real Horizon SDK (see the stub's README).
set -euo pipefail
source "$(dirname "$0")/common.sh"
staging="$repository_root/artifacts/horizon-mod-check/terraplusminus"
python3 scripts/stage-horizon-mod.py "$staging" > /dev/null
mkdir -p "$staging/.horizon/sdk/api/crates"
cp -r tools/horizon-api-stub/api "$staging/.horizon/sdk/api/crates/horizon-mod-api"
cp -r tools/horizon-api-stub/macros "$staging/.horizon/sdk/api/crates/horizon-api-stub-macros"
sed -i 's|path = "../macros"|path = "../horizon-api-stub-macros"|' "$staging/.horizon/sdk/api/crates/horizon-mod-api/Cargo.toml"
cd "$staging"
"${cargo_command[@]}" clippy --offline --all-targets -- -D warnings
echo "horizon mod type-checks against the API stub: $staging"
