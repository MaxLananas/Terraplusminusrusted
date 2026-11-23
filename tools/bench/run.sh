#!/usr/bin/env bash
# Times the same workloads in upstream's Java classes (the oracle build) and in this port:
# terrarium PNG, JPEG and GeoTIFF tiles through their heights-config parsers, and the BTE
# projection both ways. Needs tools/oracle/build-oracle.sh to have run.
# Usage: tools/bench/run.sh [warm-up seconds] [measured seconds]   (default 3 5)
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
warm="${1:-3}" run="${2:-5}"
out="$root/artifacts/bench"
toolchain="$root/tools/oracle/build/toolchain"
java="$toolchain/jdk/jdk4py/java-runtime/bin/java"
classpath="$root/tools/oracle/build/classes:$toolchain/jars/guava.jar:$toolchain/jars/failureaccess.jar:$toolchain/jars/gson.jar"
[[ -x "$java" && -d "$root/tools/oracle/build/classes" ]] || { echo "error: run tools/oracle/build-oracle.sh first" >&2; exit 1; }
mkdir -p "$out/classes"
python3 "$root/tools/bench/make-inputs.py" "$out"
cp "$root/fixtures/elevation/terrarium/12/759/1608.png" "$out/terrarium_256.png"
cp "$root/fixtures/images/input/jpeg_tile256.jpg" "$out/tile_256.jpg"
"$java" -jar "$toolchain/jars/ecj.jar" -21 -nowarn -proc:none -cp "$classpath" -d "$out/classes" "$root/tools/bench/Bench.java"
${CARGO_COMMAND:-cargo} build --offline --release -q -p terra-elevation --example bench_vs_java
cd "$out"
echo "== Java: $("$java" -version 2>&1 | head -n 1)"
"$java" -Djava.awt.headless=true -cp "$out/classes:$classpath" bench.Bench "$warm" "$run"
echo "== Rust: $(${RUSTC:-rustc} --version 2>/dev/null || echo rustc)"
"$root/target/release/examples/bench_vs_java" "$warm" "$run"
