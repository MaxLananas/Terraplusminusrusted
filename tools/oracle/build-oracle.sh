#!/usr/bin/env bash
# Builds the development-only Java oracle from the pinned, unmodified TerraMinusMinus projection sources
# and regenerates the projection golden fixtures. Never used by production builds.
set -euo pipefail
oracle="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$oracle/../.." && pwd)"
build="$oracle/build"
toolchain="$build/toolchain"
# shellcheck source=oracle-toolchain.env
source "$oracle/oracle-toolchain.env"
# shellcheck source=../../scripts/upstream-revisions.env
source "$root/scripts/upstream-revisions.env"

fixtures="${1:-$root/fixtures/projection}"

download() {
    local url="$1" destination="$2"
    [[ -f "$destination" ]] || curl --fail --silent --show-error --location --retry 3 --output "$destination.partial" "$url"
    [[ -f "$destination.partial" ]] && mv "$destination.partial" "$destination"
    return 0
}

prepare_toolchain() {
    mkdir -p "$toolchain"
    download "$JDK_WHEEL_URL" "$toolchain/jdk.whl"
    echo "$JDK_WHEEL_SHA256  $toolchain/jdk.whl" | sha256sum --check --quiet
    download "$JLS_TARBALL_URL" "$toolchain/jls.tgz"
    local actual
    actual="$(openssl dgst -sha512 -binary "$toolchain/jls.tgz" | base64 -w0)"
    if [[ "$actual" != "$JLS_TARBALL_SHA512_BASE64" ]]; then
        echo "error: java-language-server tarball digest mismatch" >&2
        exit 1
    fi
    if [[ ! -x "$toolchain/jdk/jdk4py/java-runtime/bin/java" ]]; then
        rm -rf "$toolchain/jdk"
        python3 -m zipfile -e "$toolchain/jdk.whl" "$toolchain/jdk"
        chmod +x "$toolchain/jdk/jdk4py/java-runtime/bin/"*
    fi
    if [[ ! -f "$toolchain/jars/ecj.jar" ]]; then
        rm -rf "$toolchain/jars" "$toolchain/extract"
        mkdir -p "$toolchain/jars" "$toolchain/extract"
        tar -xzf "$toolchain/jls.tgz" -C "$toolchain/extract" "$ECJ_ENTRY" "$GUAVA_ENTRY" "$FAILUREACCESS_ENTRY"
        mv "$toolchain/extract/$GUAVA_ENTRY" "$toolchain/jars/guava.jar"
        mv "$toolchain/extract/$FAILUREACCESS_ENTRY" "$toolchain/jars/failureaccess.jar"
        mv "$toolchain/extract/$ECJ_ENTRY" "$toolchain/jars/ecj.jar"
        rm -rf "$toolchain/extract"
    fi
}

prepare_toolchain
java="$toolchain/jdk/jdk4py/java-runtime/bin/java"
classpath="$toolchain/jars/guava.jar:$toolchain/jars/failureaccess.jar"

"$root/scripts/fetch-upstream.sh" terraminusminus
upstream="$root/.upstream/terraminusminus/src/main"
package_root="$upstream/java/net/buildtheearth/terraminusminus"

generated="$build/generated/net/buildtheearth/terraminusminus"
rm -rf "$build/generated" "$build/classes"
mkdir -p "$generated" "$build/classes"

# TerraConstants drags in Jackson and Gson configuration; only its numeric constants are needed.
{
    echo "package net.buildtheearth.terraminusminus;"
    echo "public final class TerraConstants {"
    echo "    private TerraConstants() {}"
    grep -E "public static final double (EARTH_CIRCUMFERENCE|EARTH_POLAR_CIRCUMFERENCE) = " "$package_root/TerraConstants.java"
    echo "    public static final UnsupportedJsonMapper JSON_MAPPER = new UnsupportedJsonMapper();"
    echo "    public static final class UnsupportedJsonMapper {"
    echo "        public <T> T readValue(String content, Class<T> type) {"
    echo "            throw new UnsupportedOperationException(\"JSON parsing is not part of the oracle\");"
    echo "        }"
    echo "    }"
    echo "}"
} > "$generated/TerraConstants.java"
if [[ "$(grep -c "EARTH_" "$generated/TerraConstants.java")" -ne 2 ]]; then
    echo "error: failed to extract TerraConstants numeric constants" >&2
    exit 1
fi

# Upstream sources are copied, mechanically delomboked by the recorded patch, and compiled as-is otherwise.
upstream_sources=(
    projection
    util/MathUtils.java
    util/IntToDoubleBiFunction.java
    util/CornerBoundingBox2d.java
    util/bvh/Bounds2d.java
    util/bvh/Bounds2dImpl.java
    substitutes/ChunkPos.java
    substitutes/BlockPos.java
    dataset/BlendMode.java
    config/scalarparse/d/DoubleScalarParser.java
    config/scalarparse/d/ParseTerrariumPngDSP.java
)
patched="$build/patched-src"
rm -rf "$patched"
for relative in "${upstream_sources[@]}"; do
    mkdir -p "$patched/net/buildtheearth/terraminusminus/$(dirname "$relative")"
    cp -R "$package_root/$relative" "$patched/net/buildtheearth/terraminusminus/$relative"
done
for patch_file in "$oracle"/patches/*.patch; do
    patch --quiet --directory "$patched" --strip 1 --forward < "$patch_file"
done

sources=()
while IFS= read -r -d '' file; do sources+=("$file"); done < <(
    find "$patched" "$oracle/stubs/src" "$oracle/harness/src" "$build/generated" -name '*.java' -print0 | sort -z
)

"$java" -jar "$toolchain/jars/ecj.jar" -21 -encoding UTF-8 -nowarn -proc:none \
    -classpath "$classpath" -d "$build/classes" "${sources[@]}"

resource_dir="$build/classes/net/buildtheearth/terraminusminus/projection/dymaxion"
cp "$upstream/resources/net/buildtheearth/terraminusminus/projection/dymaxion/conformal.gz" "$resource_dir/"

run_harness() {
    "$java" -Djava.awt.headless=true -cp "$build/classes:$classpath" "$@"
}

record_provenance() {
    local directory="$1"
    {
        echo "terraminusminus_commit=$TERRAMINUSMINUS_COMMIT"
        echo "java_version=$("$java" -version 2>&1 | head -n 1)"
        echo "jdk_wheel_sha256=$JDK_WHEEL_SHA256"
        echo "ecj_jar_sha256=$(sha256sum "$toolchain/jars/ecj.jar" | cut -d' ' -f1)"
        echo "guava_jar_sha256=$(sha256sum "$toolchain/jars/guava.jar" | cut -d' ' -f1)"
        echo "conformal_gz_sha256=$(sha256sum "$resource_dir/conformal.gz" | cut -d' ' -f1)"
        echo "harness_sha256=$(cat "$oracle/harness/src/terraplusminus/oracle/"*.java | sha256sum | cut -d' ' -f1)"
        echo "stubs_sha256=$(find "$oracle/stubs/src" -name '*.java' -print0 | sort -z | xargs -0 cat | sha256sum | cut -d' ' -f1)"
        echo "patches_sha256=$(cat "$oracle"/patches/*.patch | sha256sum | cut -d' ' -f1)"
        (cd "$directory" && sha256sum ./*.tsv)
    } > "$directory/PROVENANCE"
    gzip -9 -n -f "$directory"/*.tsv
}

mkdir -p "$fixtures"
rm -f "$fixtures"/*.tsv.gz
run_harness terraplusminus.oracle.ProjectionGoldenDump "$fixtures"
record_provenance "$fixtures"

elevation_fixtures="$root/fixtures/elevation/golden"
mkdir -p "$elevation_fixtures"
rm -f "$elevation_fixtures"/*.tsv.gz
run_harness terraplusminus.oracle.ElevationGoldenDump "$root/fixtures/elevation/terrarium" 12 "$elevation_fixtures" 12
record_provenance "$elevation_fixtures"
echo "fixtures written to $fixtures and $elevation_fixtures"
