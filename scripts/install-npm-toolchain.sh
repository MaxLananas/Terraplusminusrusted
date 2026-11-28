#!/usr/bin/env bash
# Installs the pinned nightly (rustc 1.92.0-nightly 2025-09-24, cargo, rust-std, clippy) from the
# @rusttonpm npm mirror into PREFIX, for hosts where rustup and static.rust-lang.org are
# unreachable. Each tarball is checked against its pinned npm sha512 integrity before use.
# Usage: scripts/install-npm-toolchain.sh [PREFIX]   (default: ~/.local/rust-nightly-2025-09-25)
set -euo pipefail
prefix=${1:-$HOME/.local/rust-nightly-2025-09-25}
version=0.92.20250925
declare -A integrity=(
    [rustc]=5xmxVbYey+G0NntypLfycMqf1Kqvah17PEO0xLIUuxEFcRo43JZbQJmUFknfn4G6p5sQIcNQqedv1SGgTI1DYg==
    [cargo]=o1ESTLh3ESiTGm7nYe2JTUx+2oXzkvWC1Lle0DNkAIN5KNHuzTQOaZ9d2xqlNOIdsuo7i5mUQxRnGj8Ensh9xg==
    [rust-std]=WoPTC8z0E6pR7MehL446Qb8jALdaKvnI0Qnoc7sCsTQxVv5PNesteFxN0g7eFoQhxM6uf6OApZu5ibQ/foZdLA==
    [clippy]=LazcjauGu9ItDct0Aq8rhHxh3gAotmFQ0k+XoJcf/s/xT9pjnpQl3xzqiXUEbo74kA5WUQPhcnOVBGdZcFCruA==
)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
for component in rustc cargo rust-std clippy; do
    tarball="$work/$component.tgz"
    curl --fail --silent --show-error --location -o "$tarball" \
        "https://registry.npmjs.org/@rusttonpm/$component-linux-x64/-/$component-linux-x64-$version.tgz"
    actual=$(openssl dgst -sha512 -binary "$tarball" | base64 -w0)
    [[ $actual == "${integrity[$component]}" ]] || { echo "integrity mismatch for $component" >&2; exit 1; }
    mkdir -p "$work/$component"
    tar -xzf "$tarball" -C "$work/$component"
    installer=$(find "$work/$component" -name install.sh -print -quit)
    [[ -n $installer ]] || { echo "no install.sh in $component" >&2; exit 1; }
    sh "$installer" --prefix="$prefix" --disable-ldconfig > /dev/null
done
"$prefix/bin/rustc" --version
echo "add to PATH: $prefix/bin"
