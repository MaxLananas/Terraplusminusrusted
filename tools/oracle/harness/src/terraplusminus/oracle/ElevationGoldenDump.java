package terraplusminus.oracle;

import io.netty.buffer.Unpooled;
import net.buildtheearth.terraminusminus.config.scalarparse.d.ParseTerrariumPngDSP;
import net.buildtheearth.terraminusminus.dataset.BlendMode;
import net.buildtheearth.terraminusminus.projection.GeographicProjection;
import net.buildtheearth.terraminusminus.projection.OutOfProjectionBoundsException;
import net.buildtheearth.terraminusminus.projection.dymaxion.BTEDymaxionProjection;
import net.buildtheearth.terraminusminus.projection.mercator.WebMercatorProjection;
import net.buildtheearth.terraminusminus.projection.transform.FlipVerticalProjectionTransform;
import net.buildtheearth.terraminusminus.projection.transform.OffsetProjectionTransform;
import net.buildtheearth.terraminusminus.projection.transform.ScaleProjectionTransform;
import net.buildtheearth.terraminusminus.substitutes.ChunkPos;
import net.buildtheearth.terraminusminus.util.CornerBoundingBox2d;
import net.buildtheearth.terraminusminus.util.IntToDoubleBiFunction;
import net.buildtheearth.terraminusminus.util.bvh.Bounds2d;

import java.io.BufferedWriter;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.HashMap;
import java.util.Map;
import java.util.SplittableRandom;

import static net.daporkchop.lib.common.math.PMath.floorI;

/**
 * Runs the upstream terrarium parser, blend modes, bounding-box classes and projections over an
 * offline tile set. The glue between them transcribes DoubleTiledDataset.getAsync(CornerBoundingBox2d,...),
 * DoubleTiledDataset.AbstractState.apply, IEarthAsyncPipelineStep.getFuture and HeightsBaker.bake.
 */
public final class ElevationGoldenDump {
    private static final double BTE_SCALE = 7318261.522857145;
    private static final int RESOLUTION = 256;
    private static final int SHIFT = 8;
    private static final int MASK = RESOLUTION - 1;

    private ElevationGoldenDump() {}

    public static void main(String[] args) throws Exception {
        if (args.length != 4) {
            System.err.println("usage: ElevationGoldenDump <tile-root> <zoom> <output-directory> <chunk-radius>");
            System.exit(2);
        }
        Path tileRoot = Path.of(args[0]);
        int zoom = Integer.parseInt(args[1]);
        Path output = Path.of(args[2]);
        int radius = Integer.parseInt(args[3]);
        Files.createDirectories(output);

        Map<Long, double[]> tiles = loadTiles(tileRoot.resolve(String.valueOf(zoom)));
        for (Map.Entry<Long, double[]> entry : tiles.entrySet()) {
            int x = (int) (entry.getKey() >>> 32);
            int z = (int) (long) entry.getKey();
            writeValues(output.resolve("terrarium_decode_z" + zoom + "_" + x + "_" + z + ".tsv"), entry.getValue());
        }
        IntToDoubleBiFunction sampler = (x, z) -> {
            double[] tile = tiles.get(pack(x >> SHIFT, z >> SHIFT));
            return tile == null ? Double.NaN : tile[(z & MASK) << SHIFT | (x & MASK)];
        };
        writeBlendSamples(output.resolve("blend_samples.tsv"), tiles, sampler);

        WebMercatorProjection datasetProjection = new WebMercatorProjection(zoom);
        GeographicProjection bte = new ScaleProjectionTransform(
                new FlipVerticalProjectionTransform(new BTEDymaxionProjection()), BTE_SCALE, BTE_SCALE);
        long firstTile = tiles.keySet().iterator().next();
        double centerPixelX = ((int) (firstTile >>> 32) + 0.5) * RESOLUTION;
        double centerPixelZ = ((int) firstTile + 0.5) * RESOLUTION;
        double[] centerGeo = datasetProjection.toGeo(centerPixelX, centerPixelZ);
        writeChunkHeights(output.resolve("chunk_heights_bte.tsv"), bte, datasetProjection, centerGeo, radius, sampler);
        GeographicProjection shifted = new OffsetProjectionTransform(bte, 10_000_000, -3_000_000);
        writeChunkHeights(output.resolve("chunk_heights_bte_offset.tsv"), shifted, datasetProjection, centerGeo, radius, sampler);
    }

    private static long pack(int x, int z) {
        return (Integer.toUnsignedLong(x) << 32) | Integer.toUnsignedLong(z);
    }

    private static Map<Long, double[]> loadTiles(Path zoomRoot) throws IOException {
        Map<Long, double[]> tiles = new HashMap<>();
        try (var columns = Files.list(zoomRoot)) {
            for (Path column : (Iterable<Path>) columns.sorted()::iterator) {
                try (var rows = Files.list(column)) {
                    for (Path row : (Iterable<Path>) rows.sorted()::iterator) {
                        int x = Integer.parseInt(column.getFileName().toString());
                        int z = Integer.parseInt(row.getFileName().toString().replace(".png", ""));
                        byte[] bytes = Files.readAllBytes(row);
                        tiles.put(pack(x, z), new ParseTerrariumPngDSP().parse(RESOLUTION, Unpooled.wrappedBuffer(bytes)));
                    }
                }
            }
        }
        return tiles;
    }

    private static void writeValues(Path path, double[] values) throws IOException {
        try (BufferedWriter writer = Files.newBufferedWriter(path, StandardCharsets.UTF_8)) {
            writer.write("# ParseTerrariumPngDSP output, row-major (z * resolution + x)\n");
            for (double value : values) {
                writer.write(bits(value));
                writer.write('\n');
            }
        }
    }

    private static void writeBlendSamples(Path path, Map<Long, double[]> tiles, IntToDoubleBiFunction sampler) throws IOException {
        long firstTile = tiles.keySet().iterator().next();
        double originX = (int) (firstTile >>> 32) * (double) RESOLUTION;
        double originZ = (int) firstTile * (double) RESOLUTION;
        SplittableRandom random = new SplittableRandom(0xB1E9DL);
        try (BufferedWriter writer = Files.newBufferedWriter(path, StandardCharsets.UTF_8)) {
            writer.write("# mode\tscaledX\tscaledZ\tresult\n");
            for (int i = 0; i < 6000; i++) {
                double x = originX + random.nextDouble(-4.0, RESOLUTION + 4.0);
                double z = originZ + random.nextDouble(-4.0, RESOLUTION + 4.0);
                if (i % 10 == 0) {
                    x = Math.floor(x);
                } else if (i % 10 == 1) {
                    z = Math.floor(z) + 0.5;
                }
                for (BlendMode mode : BlendMode.values()) {
                    writer.write(mode.name() + '\t' + bits(x) + '\t' + bits(z) + '\t' + bits(mode.get(x, z, sampler)) + '\n');
                }
            }
        }
    }

    private static void writeChunkHeights(Path path, GeographicProjection projection, WebMercatorProjection datasetProjection,
                                          double[] centerGeo, int radius, IntToDoubleBiFunction sampler) throws Exception {
        double[] centerBlock = projection.fromGeo(centerGeo[0], centerGeo[1]);
        int centerChunkX = floorI(centerBlock[0]) >> 4;
        int centerChunkZ = floorI(centerBlock[1]) >> 4;
        BlendMode blend = BlendMode.CUBIC;
        try (BufferedWriter writer = Files.newBufferedWriter(path, StandardCharsets.UTF_8)) {
            writer.write("# " + projection + " chunkX\tchunkZ\t256 heights (x * 16 + z) or OUT_OF_BOUNDS\n");
            for (int chunkX = centerChunkX - radius; chunkX <= centerChunkX + radius; chunkX++) {
                for (int chunkZ = centerChunkZ - radius; chunkZ <= centerChunkZ + radius; chunkZ++) {
                    writer.write(chunkX + "\t" + chunkZ);
                    double[] heights;
                    try {
                        int baseX = ChunkPos.cubeToMinBlock(chunkX);
                        int baseZ = ChunkPos.cubeToMinBlock(chunkZ);
                        Bounds2d chunkBounds = Bounds2d.of(baseX, baseX + 16, baseZ, baseZ + 16);
                        CornerBoundingBox2d chunkBoundsGeo = chunkBounds.toCornerBB(projection, false).toGeo();
                        CornerBoundingBox2d localBounds = chunkBoundsGeo.fromGeo(datasetProjection);
                        localBounds.axisAlign().expand(blend.size).validate(datasetProjection, false);
                        heights = sampleGrid(localBounds, 16, 16, blend, sampler);
                    } catch (OutOfProjectionBoundsException e) {
                        heights = null;
                    }
                    if (heights == null) {
                        writer.write("\tOUT_OF_BOUNDS");
                    } else {
                        for (double height : heights) {
                            writer.write('\t');
                            writer.write(bits(height));
                        }
                    }
                    writer.write('\n');
                }
            }
        }
    }

    private static double[] sampleGrid(CornerBoundingBox2d localBounds, int sizeX, int sizeZ, BlendMode blend,
                                       IntToDoubleBiFunction sampler) {
        double stepX = 1.0d / sizeX;
        double stepZ = 1.0d / sizeZ;
        double[] point = new double[2];
        double[] out = new double[sizeX * sizeZ];
        double fx = 0.0d;
        for (int i = 0, x = 0; x < sizeX; x++, fx += stepX) {
            double fz = 0.0d;
            for (int z = 0; z < sizeZ; z++, fz += stepZ) {
                point = localBounds.point(point, fx, fz);
                out[i++] = blend.get(point[0], point[1], sampler);
            }
        }
        return out;
    }

    private static String bits(double value) {
        return String.format("%016x", Double.doubleToRawLongBits(value));
    }
}
