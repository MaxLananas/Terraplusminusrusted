#!/usr/bin/env bash
# Fetches the pinned upstream revisions into .upstream/<name> and verifies the checked-out commit.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=upstream-revisions.env
source "$root/scripts/upstream-revisions.env"

fetch() {
    local name="$1" repository="$2" commit="$3"
    local target="$root/.upstream/$name"
    if [[ -d "$target/.git" ]] && [[ "$(git -C "$target" rev-parse HEAD)" == "$commit" ]]; then
        echo "$name: $commit (cached)"
        return
    fi
    rm -rf "$target"
    mkdir -p "$target"
    git -C "$target" init --quiet
    git -C "$target" fetch --quiet --depth 1 "$repository" "$commit"
    git -C "$target" checkout --quiet FETCH_HEAD
    local actual
    actual="$(git -C "$target" rev-parse HEAD)"
    if [[ "$actual" != "$commit" ]]; then
        echo "error: $name resolved to $actual, expected $commit" >&2
        exit 1
    fi
    echo "$name: $commit"
}

selection=("$@")
if [[ ${#selection[@]} -eq 0 ]]; then
    selection=(terraplusminus terraminusminus horizon)
fi
for name in "${selection[@]}"; do
    case "$name" in
        terraplusminus) fetch terraplusminus "$TERRAPLUSMINUS_REPOSITORY" "$TERRAPLUSMINUS_COMMIT" ;;
        terraminusminus) fetch terraminusminus "$TERRAMINUSMINUS_REPOSITORY" "$TERRAMINUSMINUS_COMMIT" ;;
        horizon) fetch horizon "$HORIZON_REPOSITORY" "$HORIZON_COMMIT" ;;
        *) echo "error: unknown upstream '$name'" >&2; exit 2 ;;
    esac
done
