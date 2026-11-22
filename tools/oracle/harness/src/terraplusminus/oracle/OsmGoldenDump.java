package terraplusminus.oracle;

import com.google.gson.stream.JsonReader;
import java.io.BufferedReader;
import java.io.IOException;
import java.io.PrintWriter;
import java.lang.reflect.Type;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collection;
import java.util.Collections;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.TreeSet;
import java.util.stream.Stream;
import java.util.stream.StreamSupport;
import net.buildtheearth.terraminusminus.TerraConstants;
import net.buildtheearth.terraminusminus.dataset.geojson.GeoJson;
import net.buildtheearth.terraminusminus.dataset.geojson.GeoJsonObject;
import net.buildtheearth.terraminusminus.dataset.geojson.Geometry;
import net.buildtheearth.terraminusminus.dataset.geojson.geometry.Point;
import net.buildtheearth.terraminusminus.dataset.geojson.object.Feature;
import net.buildtheearth.terraminusminus.dataset.geojson.object.Reference;
import net.buildtheearth.terraminusminus.dataset.osm.OSMMapper;
import net.buildtheearth.terraminusminus.dataset.vector.geometry.VectorGeometry;
import net.buildtheearth.terraminusminus.generator.CachedChunkData;
import net.buildtheearth.terraminusminus.generator.EarthGeneratorPipelines;
import net.buildtheearth.terraminusminus.projection.OutOfProjectionBoundsException;
import net.buildtheearth.terraminusminus.projection.ProjectionFunction;
import net.buildtheearth.terraminusminus.substitutes.BlockPropertyValue;
import net.buildtheearth.terraminusminus.substitutes.BlockState;
import net.buildtheearth.terraminusminus.util.bvh.BVH;
import net.buildtheearth.terraminusminus.util.bvh.Bounds2d;

/**
 * OpenStreetMap features drawn onto chunk data by upstream code: the osm.json5 mapping parsed by
 * {@code Root}'s Gson adapters, tiles decoded like {@code ParsingGeoJsonDataset}, references
 * resolved like {@code AbstractReferenceResolvingGeoJsonDataset}, elements produced like
 * {@code GeoJsonToVectorDataset.convertToElements}, indexed with {@code BVH.of} and drawn like
 * {@code OSMBaker.bake}. Scenes use the bundled osm.json5 unless they name another mapping.
 * Only the projection is a test stand-in (linear, see make-scenes.py), and
 * every tile of a scene holds the same objects, as neighbouring tiles share the features crossing
 * them.
 */
public final class OsmGoldenDump {
    /** A distinctive tree-cover chance, so that NoTrees' zeroing is visible. */
    private static final byte TREE_COVER = (byte) 0x5A;
    private static final double OUT_OF_RANGE_LONGITUDE = 180.0;

    private OsmGoldenDump() {}

    public static void main(String[] args) throws Exception {
        Path mapping = Path.of(args[0]);
        Path fixtures = Path.of(args[1]);
        Path output = Path.of(args[2]);
        OSMMapper<Geometry> defaultMapper = loadMapper(mapping);

        try (PrintWriter elementsOut = new PrintWriter(Files.newBufferedWriter(output.resolve("elements.tsv"), StandardCharsets.UTF_8));
             PrintWriter chunksOut = new PrintWriter(Files.newBufferedWriter(output.resolve("chunks.tsv"), StandardCharsets.UTF_8))) {
            elementsOut.print("scene\tindex\tkind\tid\tlayer_bits\tmin_x_bits\tmax_x_bits\tmin_z_bits\tmax_z_bits\n");
            chunksOut.print("scene\tchunk_x\tchunk_z\twater_depth\ttree_cover\tsurface_blocks\n");
            for (String row : Files.readAllLines(fixtures.resolve("scenes.tsv"), StandardCharsets.UTF_8)) {
                if (row.isEmpty() || row.startsWith("#")) {
                    continue;
                }
                String[] fields = row.split("\t");
                String name = fields[0];
                Path tile = fixtures.resolve(fields[1]);
                double lon0 = Double.parseDouble(fields[2]);
                double lat0 = Double.parseDouble(fields[3]);
                double scale = Double.parseDouble(fields[4]);
                int minChunkX = Integer.parseInt(fields[5]);
                int maxChunkX = Integer.parseInt(fields[6]);
                int minChunkZ = Integer.parseInt(fields[7]);
                int maxChunkZ = Integer.parseInt(fields[8]);
                OSMMapper<Geometry> mapper = "default".equals(fields[9]) ? defaultMapper : loadMapper(fixtures.resolve(fields[9]));

                ProjectionFunction projection = (lon, lat) -> {
                    if (Math.abs(lon) > OUT_OF_RANGE_LONGITUDE) {
                        throw OutOfProjectionBoundsException.get();
                    }
                    return new double[]{ (lon - lon0) * scale, (lat0 - lat) * scale };
                };
                VectorGeometry[] elements = load(mapper, projection, tile.getParent(), tile);
                for (int i = 0; i < elements.length; i++) {
                    VectorGeometry element = elements[i];
                    elementsOut.print(name + '\t' + i + '\t' + element.getClass().getSimpleName() + '\t' + element.id() + '\t'
                            + bits(element.layer()) + '\t' + bits(element.minX()) + '\t' + bits(element.maxX()) + '\t'
                            + bits(element.minZ()) + '\t' + bits(element.maxZ()) + '\n');
                }

                BVH<VectorGeometry> index = BVH.of(elements);
                for (int chunkX = minChunkX; chunkX <= maxChunkX; chunkX++) {
                    for (int chunkZ = minChunkZ; chunkZ <= maxChunkZ; chunkZ++) {
                        CachedChunkData.Builder builder = new CachedChunkData.Builder();
                        byte[] treeCover = new byte[16 * 16];
                        Arrays.fill(treeCover, TREE_COVER);
                        builder.putCustom(EarthGeneratorPipelines.KEY_DATA_TREE_COVER, treeCover);

                        int baseX = chunkX << 4;
                        int baseZ = chunkZ << 4;
                        Bounds2d chunkBounds = Bounds2d.of(baseX, baseX + 16, baseZ, baseZ + 16);
                        Set<VectorGeometry> drawn = new TreeSet<>();
                        index.forEachIntersecting(chunkBounds, drawn::add);
                        int x = chunkX;
                        int z = chunkZ;
                        drawn.forEach(element -> element.apply(builder, x, z, chunkBounds));

                        StringBuilder blocks = new StringBuilder();
                        for (BlockState state : builder.surfaceBlocks()) {
                            if (blocks.length() > 0) {
                                blocks.append(' ');
                            }
                            blocks.append(state == null ? "-" : format(state));
                        }
                        chunksOut.print(name + '\t' + chunkX + '\t' + chunkZ + '\t' + hex(builder.waterDepth()) + '\t' + hex(treeCover) + '\t'
                                + blocks + '\n');
                    }
                }
            }
        }
    }

    @SuppressWarnings("unchecked")
    private static OSMMapper<Geometry> loadMapper(Path mapping) throws IOException, ClassNotFoundException {
        // OSMMapper.load reads its file the same way: a JsonReader made lenient by Gson.fromJson.
        Type root = Class.forName("net.buildtheearth.terraminusminus.dataset.osm.Root");
        try (JsonReader reader = new JsonReader(Files.newBufferedReader(mapping, StandardCharsets.UTF_8))) {
            return (OSMMapper<Geometry>) TerraConstants.GSON.fromJson(reader, root);
        }
    }

    /** AbstractReferenceResolvingGeoJsonDataset.load: referenced resources first, then the other objects. */
    private static VectorGeometry[] load(OSMMapper<Geometry> mapper, ProjectionFunction projection, Path base, Path file) throws IOException {
        GeoJsonObject[] objects;
        try (BufferedReader reader = Files.newBufferedReader(file, StandardCharsets.UTF_8)) {
            objects = reader.lines().map(GeoJson::parse).toArray(GeoJsonObject[]::new);
        }
        List<VectorGeometry> out = new ArrayList<>();
        List<GeoJsonObject> local = new ArrayList<>();
        for (GeoJsonObject object : objects) {
            if (object instanceof Reference) {
                out.addAll(Arrays.asList(load(mapper, projection, base, base.resolve(((Reference) object).location()))));
            } else {
                local.add(object);
            }
        }
        local.stream().flatMap(object -> convertToElements(mapper, projection, null, Collections.emptyMap(), object)).forEach(out::add);
        return out.toArray(new VectorGeometry[0]);
    }

    /** GeoJsonToVectorDataset.convertToElements, with the scene's projection in place of the earth projection. */
    @SuppressWarnings("unchecked")
    private static Stream<VectorGeometry> convertToElements(OSMMapper<Geometry> mapper, ProjectionFunction projection, String id,
                                                            Map<String, String> tags, GeoJsonObject object) {
        if (object instanceof Iterable) {
            return StreamSupport.stream(((Iterable<? extends GeoJsonObject>) object).spliterator(), false)
                    .flatMap(child -> convertToElements(mapper, projection, id, tags, child));
        } else if (object instanceof Feature) {
            Feature feature = (Feature) object;
            return convertToElements(mapper, projection, feature.id() != null ? feature.id() : id,
                    feature.properties() != null ? feature.properties() : tags, feature.geometry());
        } else if (object instanceof Point) {
            return Stream.empty();
        } else if (object instanceof Reference) {
            return Stream.empty();
        } else {
            try {
                Geometry geometry = (Geometry) object;
                Collection<VectorGeometry> elements = mapper.apply(id, tags, geometry, geometry.project(projection));
                return elements != null ? elements.stream() : Stream.empty();
            } catch (OutOfProjectionBoundsException e) {
                return Stream.empty();
            }
        }
    }

    /** Identifier plus properties sorted by name; upstream's toString iterates a HashMap. */
    private static String format(BlockState state) {
        StringBuilder out = new StringBuilder(state.getBlock().toString());
        Map<String, BlockPropertyValue> properties = state.getProperties();
        if (!properties.isEmpty()) {
            out.append('[');
            String[] names = properties.keySet().toArray(new String[0]);
            Arrays.sort(names);
            for (int i = 0; i < names.length; i++) {
                out.append(i == 0 ? "" : ",").append(names[i]).append('=').append(properties.get(names[i]).getAsString());
            }
            out.append(']');
        }
        return out.toString();
    }

    private static String bits(double value) {
        return String.format("%016x", Double.doubleToRawLongBits(value));
    }

    private static String hex(byte[] values) {
        StringBuilder out = new StringBuilder(values.length * 2);
        for (byte value : values) {
            out.append(String.format("%02x", value & 0xFF));
        }
        return out.toString();
    }
}
