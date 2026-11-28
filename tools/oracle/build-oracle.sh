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
    if [[ ! -f "$toolchain/jars/ecj.jar" || ! -f "$toolchain/jars/gson.jar" ]]; then
        rm -rf "$toolchain/jars" "$toolchain/extract"
        mkdir -p "$toolchain/jars" "$toolchain/extract"
        tar -xzf "$toolchain/jls.tgz" -C "$toolchain/extract" "$ECJ_ENTRY" "$GUAVA_ENTRY" "$FAILUREACCESS_ENTRY" "$GSON_ENTRY"
        mv "$toolchain/extract/$GUAVA_ENTRY" "$toolchain/jars/guava.jar"
        mv "$toolchain/extract/$FAILUREACCESS_ENTRY" "$toolchain/jars/failureaccess.jar"
        mv "$toolchain/extract/$GSON_ENTRY" "$toolchain/jars/gson.jar"
        mv "$toolchain/extract/$ECJ_ENTRY" "$toolchain/jars/ecj.jar"
        rm -rf "$toolchain/extract"
    fi
}

prepare_toolchain
java="$toolchain/jdk/jdk4py/java-runtime/bin/java"
classpath="$toolchain/jars/guava.jar:$toolchain/jars/failureaccess.jar:$toolchain/jars/gson.jar"

"$root/scripts/fetch-upstream.sh" terraminusminus commons-imaging
upstream="$root/.upstream/terraminusminus/src/main"
package_root="$upstream/java/net/buildtheearth/terraminusminus"

generated="$build/generated/net/buildtheearth/terraminusminus"
rm -rf "$build/generated" "$build/classes"
mkdir -p "$generated" "$build/classes"

# TerraConstants drags in Jackson configuration; only its numeric constants, the empty array and the
# Gson instance (with the block state adapter the OpenStreetMap parsers rely on) are needed.
{
    echo "package net.buildtheearth.terraminusminus;"
    echo "import com.google.gson.Gson;"
    echo "import com.google.gson.GsonBuilder;"
    echo "import net.buildtheearth.terraminusminus.dataset.osm.BlockStateParser;"
    echo "import net.buildtheearth.terraminusminus.substitutes.BlockState;"
    echo "public final class TerraConstants {"
    echo "    private TerraConstants() {}"
    grep -E "public static final double (EARTH_CIRCUMFERENCE|EARTH_POLAR_CIRCUMFERENCE) = " "$package_root/TerraConstants.java"
    grep -E "public static final double\[\] EMPTY_DOUBLE_ARRAY = " "$package_root/TerraConstants.java"
    sed -n '/public static final Gson GSON = /,/\.create();/p' "$package_root/TerraConstants.java"
    echo "    public static final UnsupportedJsonMapper JSON_MAPPER = new UnsupportedJsonMapper();"
    echo "    public static final class UnsupportedJsonMapper {"
    echo "        public <T> T readValue(String content, Class<T> type) {"
    echo "            throw new UnsupportedOperationException(\"JSON parsing is not part of the oracle\");"
    echo "        }"
    echo "    }"
    echo "}"
} > "$generated/TerraConstants.java"
if [[ "$(grep -c "EARTH_" "$generated/TerraConstants.java")" -ne 2 ]] \
    || ! grep -q "EMPTY_DOUBLE_ARRAY" "$generated/TerraConstants.java" \
    || ! grep -q "registerTypeAdapter(BlockState.class, BlockStateParser.INSTANCE)" "$generated/TerraConstants.java"; then
    echo "error: failed to extract TerraConstants members" >&2
    exit 1
fi

# Upstream sources are copied, mechanically delomboked by the recorded patch, and compiled as-is otherwise.
upstream_sources=(
    projection
    util/MathUtils.java
    util/Strings.java
    util/geo/CoordinateParseUtils.java
    util/geo/LatLng.java
    util/IntToDoubleBiFunction.java
    util/CornerBoundingBox2d.java
    util/bvh/Bounds2d.java
    util/bvh/Bounds2dImpl.java
    substitutes/ChunkPos.java
    substitutes/BlockPos.java
    dataset/BlendMode.java
    config/scalarparse/d/DoubleScalarParser.java
    config/scalarparse/d/ParseTerrariumPngDSP.java
    config/scalarparse/d/FromIntDSP.java
    config/scalarparse/d/ParseTiffDSP.java
    config/scalarparse/d/ParseTiffAutoDSP.java
    config/scalarparse/d/ParseTiffFloatingPointDSP.java
    config/scalarparse/d/ParseTiffIntDSP.java
    config/scalarparse/i
    # OpenStreetMap: GeoJSON model and parsers, the osm.json5 mapper, vector geometries and draw
    # functions, and the spatial indexes. Delomboked by patches/delombok-osm.patch (see delombok-osm.py).
    dataset/geojson/AbstractGeoJsonDeserializer.java
    dataset/geojson/GeoJson.java
    dataset/geojson/GeoJsonObject.java
    dataset/geojson/Geometry.java
    dataset/geojson/GeometryDeserializer.java
    dataset/geojson/ObjectDeserializer.java
    dataset/geojson/geometry
    dataset/geojson/object
    dataset/osm/BlockStateParser.java
    dataset/osm/JsonParser.java
    dataset/osm/OSMMapper.java
    dataset/osm/Root.java
    dataset/osm/dvalue
    dataset/osm/mapper
    dataset/osm/match
    dataset/vector/draw
    dataset/vector/geometry
    util/bvh/BVH.java
    util/bvh/EmptyBVH.java
    util/bvh/QuadtreeBVH.java
    util/bvh/SingletonBVH.java
    util/interval
    substitutes/BlockState.java
    substitutes/BlockStateBuilder.java
    substitutes/BlockPropertyValue.java
    substitutes/Identifier.java
    substitutes/exceptions
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
    find "$patched" "$oracle/stubs/src" "$oracle/harness/src" "$build/generated" "$root/.upstream/commons-imaging/src/main/java" -name '*.java' -print0 | sort -z
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
        echo "commons_imaging_commit=$COMMONS_IMAGING_COMMIT"
        echo "java_version=$("$java" -version 2>&1 | head -n 1)"
        echo "jdk_wheel_sha256=$JDK_WHEEL_SHA256"
        echo "ecj_jar_sha256=$(sha256sum "$toolchain/jars/ecj.jar" | cut -d' ' -f1)"
        echo "guava_jar_sha256=$(sha256sum "$toolchain/jars/guava.jar" | cut -d' ' -f1)"
        echo "gson_jar_sha256=$(sha256sum "$toolchain/jars/gson.jar" | cut -d' ' -f1)"
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
coordinate_fixtures="$root/fixtures/coordinates/golden"
mkdir -p "$coordinate_fixtures"
rm -f "$coordinate_fixtures"/*.tsv.gz
run_harness terraplusminus.oracle.CoordinateParseGoldenDump "$root/fixtures/coordinates/parse_inputs.txt" "$coordinate_fixtures"
record_provenance "$coordinate_fixtures"
command_fixtures="$root/fixtures/commands/golden"
mkdir -p "$command_fixtures"
rm -f "$command_fixtures"/*.tsv.gz
run_harness terraplusminus.oracle.CommandTextGoldenDump "$command_fixtures"
record_provenance "$command_fixtures"
osm_fixtures="$root/fixtures/osm/golden"
mkdir -p "$osm_fixtures"
rm -f "$osm_fixtures"/*.tsv.gz
osm_mapping="$upstream/resources/net/buildtheearth/terraminusminus/dataset/osm/osm.json5"
run_harness terraplusminus.oracle.OsmGoldenDump "$osm_mapping" "$root/fixtures/osm" "$osm_fixtures"
record_provenance "$osm_fixtures"
echo "osm_json5_sha256=$(sha256sum "$osm_mapping" | cut -d' ' -f1)" >> "$osm_fixtures/PROVENANCE"
image_fixtures="$root/fixtures/images/golden"
mkdir -p "$image_fixtures"
rm -f "$image_fixtures"/*.tsv.gz
run_harness terraplusminus.oracle.ImageGoldenDump "$root/fixtures/images/input" "$image_fixtures"
record_provenance "$image_fixtures"
(cd "$root/fixtures/images/input" && for input in *; do echo "file $(sha256sum "$input" | cut -d' ' -f1)  ../input/$input"; done) >> "$image_fixtures/PROVENANCE"
echo "fixtures written to $fixtures, $elevation_fixtures, $coordinate_fixtures, $command_fixtures, $osm_fixtures and $image_fixtures"
