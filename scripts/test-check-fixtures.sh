#!/usr/bin/env bash
# check-fixtures.sh must fail on every kind of damage, each tried on a disposable copy of
# fixtures/ (never on the repository's own files).
set -euo pipefail
cd "$(dirname "$0")/.."
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
passed=0
failed=0

fresh() {
    rm -rf "$work/copy"
    mkdir -p "$work/copy"
    cp -r fixtures "$work/copy/"
}

expect() {
    local outcome=$1 label=$2
    if scripts/check-fixtures.sh "$work/copy" > "$work/out" 2>&1; then actual=pass; else actual=fail; fi
    if [[ $actual == "$outcome" ]]; then
        passed=$(( passed + 1 ))
        echo "PASS $label"
    else
        failed=$(( failed + 1 ))
        echo "FAIL $label (expected $outcome, got $actual)"
        cat "$work/out"
    fi
}

flip_byte() {
    python3 - "$1" "$2" <<'PY'
import sys
path, offset = sys.argv[1], int(sys.argv[2])
data = bytearray(open(path, "rb").read())
data[offset % len(data)] ^= 0x01
open(path, "wb").write(data)
PY
}

fresh; expect pass "an intact copy passes"
fresh; flip_byte "$work/copy/fixtures/tiff/cases/f32_none_be.tif" 200; expect fail "a corrupted TIFF case fails"
fresh; flip_byte "$work/copy/fixtures/treecover/13/1519/3217.tiff" 100; expect fail "a corrupted tree cover tile fails"
fresh; flip_byte "$(find "$work/copy/fixtures/elevation/terrarium" -type f | head -n 1)" 300; expect fail "a corrupted elevation tile fails"
fresh; flip_byte "$(find "$work/copy/fixtures/osm/scenes" -type f | head -n 1)" 10; expect fail "a corrupted OSM scene fails"
fresh; rm "$work/copy/fixtures/tiff/cases/u8_lzw_large.tif"; expect fail "a missing TIFF case fails"
fresh; echo extra > "$work/copy/fixtures/tiff/cases/unlisted.tif"; expect fail "an unlisted file fails"
fresh; rm "$work/copy/fixtures/MANIFEST.sha256"; expect fail "a missing manifest fails"
fresh; sed -i '1d' "$work/copy/fixtures/MANIFEST.sha256"; expect fail "a manifest missing an entry fails"
fresh; first=$(find "$work/copy/fixtures" -name '*.gz' -path '*golden*' | head -n 1); printf 'x' | gzip > "$first"; expect fail "a changed golden fails"

echo "$passed passed, $failed failed"
(( failed == 0 ))
