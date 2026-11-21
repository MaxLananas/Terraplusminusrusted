#!/usr/bin/env bash
# Every golden file must match the digest its PROVENANCE recorded when the oracle produced it.
set -euo pipefail
cd "$(dirname "$0")/.."
checked=0
while IFS= read -r provenance; do
    directory=$(dirname "$provenance")
    while read -r digest name; do
        file="$directory/${name#./}.gz"
        [[ -f $file ]] || { echo "missing $file"; exit 1; }
        actual=$(gzip -dc "$file" | sha256sum | cut -d' ' -f1)
        [[ $actual == "$digest" ]] || { echo "digest mismatch: $file"; exit 1; }
        checked=$(( checked + 1 ))
    done < <(grep -E '^[0-9a-f]{64}  ' "$provenance")
done < <(find fixtures -name PROVENANCE | sort)
(( checked > 0 )) || { echo "no fixtures checked"; exit 1; }
echo "$checked golden files match their provenance"
