#!/usr/bin/env bash
# Fixture integrity, against the repository root or the directory given as $1:
#  1. every golden `<digest>  <name>` in a PROVENANCE matches its decompressed .gz file;
#  2. every `file <digest>  <name>` in a PROVENANCE matches that file's bytes;
#  3. fixtures/MANIFEST.sha256 lists every file under fixtures/ (nothing unlisted, nothing
#     missing) with its digest.
# Any mismatch, missing or unlisted file fails.
set -euo pipefail
root=${1:-"$(dirname "$0")/.."}
cd "$root"
[[ -d fixtures ]] || { echo "no fixtures directory in $root"; exit 1; }
failures=0
fail() { echo "FAIL: $*"; failures=$(( failures + 1 )); }
digest_of() { sha256sum "$1" | cut -d' ' -f1; }

goldens=0
files=0
while IFS= read -r provenance; do
    directory=$(dirname "$provenance")
    while read -r digest name; do
        file="$directory/${name#./}.gz"
        if [[ ! -f $file ]]; then fail "missing golden $file"; continue; fi
        [[ $(gzip -dc "$file" | sha256sum | cut -d' ' -f1) == "$digest" ]] || fail "golden digest mismatch: $file"
        goldens=$(( goldens + 1 ))
    done < <(grep -E '^[0-9a-f]{64}  ' "$provenance" || true)
    while read -r _ digest name; do
        file="$directory/$name"
        if [[ ! -f $file ]]; then fail "missing $file (listed in $provenance)"; continue; fi
        [[ $(digest_of "$file") == "$digest" ]] || fail "digest mismatch: $file (per $provenance)"
        files=$(( files + 1 ))
    done < <(grep -E '^file [0-9a-f]{64}  ' "$provenance" || true)
    if grep -vE '^(#|[0-9a-f]{64}  |file [0-9a-f]{64}  |[A-Za-z_]+=|$)' "$provenance" | grep -qE '^[0-9a-f]{64}|^file '; then
        fail "malformed digest line in $provenance"
    fi
done < <(find fixtures -name PROVENANCE | sort)

manifest=fixtures/MANIFEST.sha256
listed=0
if [[ ! -f $manifest ]]; then
    fail "missing $manifest"
else
    while IFS= read -r line; do
        [[ $line =~ ^([0-9a-f]{64})\ \ (fixtures/.+)$ ]] || { fail "malformed manifest line: $line"; continue; }
        digest=${BASH_REMATCH[1]} file=${BASH_REMATCH[2]}
        if [[ ! -f $file ]]; then fail "missing $file (listed in the manifest)"; continue; fi
        [[ $(digest_of "$file") == "$digest" ]] || fail "digest mismatch: $file (per the manifest)"
        listed=$(( listed + 1 ))
    done < "$manifest"
    unlisted=$(comm -23 <(find fixtures -type f ! -path "$manifest" | LC_ALL=C sort) <(sed -E 's/^[0-9a-f]{64}  //' "$manifest" | LC_ALL=C sort))
    [[ -z $unlisted ]] || fail "files not in the manifest: $(echo "$unlisted" | tr '\n' ' ')"
fi

(( goldens > 0 )) || fail "no golden files checked"
(( files > 0 )) || fail "no PROVENANCE file digests checked"
(( failures == 0 )) || { echo "$failures fixture integrity failure(s)"; exit 1; }
echo "$goldens golden files, $files provenance file digests and $listed manifest entries match"
