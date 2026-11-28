package terraplusminus.bench;

import io.netty.buffer.Unpooled;
import net.buildtheearth.terraminusminus.config.scalarparse.d.ParseTerrariumPngDSP;
import net.buildtheearth.terraminusminus.dataset.BlendMode;
import net.buildtheearth.terraminusminus.projection.GeographicProjection;
import net.buildtheearth.terraminusminus.projection.OutOfProjectionBoundsException;
import net.buildtheearth.terraminusminus.projection.dymaxion.BTEDymaxionProjection;
import net.buildtheearth.terraminusminus.projection.mercator.WebMercatorProjection;
import net.buildtheearth.terraminusminus.projection.transform.FlipVerticalProjectionTransform;
import net.buildtheearth.terraminusminus.projection.transform.ScaleProjectionTransform;
import net.buildtheearth.terraminusminus.substitutes.ChunkPos;
import net.buildtheearth.terraminusminus.util.CornerBoundingBox2d;
import net.buildtheearth.terraminusminus.util.IntToDoubleBiFunction;
import net.buildtheearth.terraminusminus.util.bvh.Bounds2d;

import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Arrays;
import java.util.SplittableRandom;

import static net.daporkchop.lib.common.math.PMath.floorI;

/**
 * Single-threaded micro-benchmarks of upstream terraminusminus code (compiled by
 * tools/oracle/build-oracle.sh), mirrored workload for workload by
 * crates/terra-elevation/examples/microbench.rs:
 *   decode  - ParseTerrariumPngDSP on the fixture tile (ImageIO PNG decode + Terrarium maths)
 *   from    - BTE projection fromGeo on 1,000,000 fixed random points
 *   to      - BTE projection toGeo on the projected points
 *   chunk   - heights of 625 chunks (radius 12) from the decoded tile, cubic blend, as in
 *             ElevationGoldenDump (DoubleTiledDataset glue transcribed; no async pipeline, no cache)
 * Each benchmark runs warm-up rounds (JIT) then timed rounds; the median round is reported.
 * Output: one "name<TAB>unit<TAB>median<TAB>min<TAB>max" line per benchmark.
 */
public final class ElevationBench {
    private static final double BTE_SCALE = 7318261.522857145;
    private static final int RESOLUTION = 256;
    private static final int SHIFT = 8;
    private static final int MASK = RESOLUTION - 1;
    private static volatile double sink;

    public static void main(String[] args) throws Exception {
        Path tile = Path.of(args[0]);
        int tileX = Integer.parseInt(args[1]);
        int tileZ = Integer.parseInt(args[2]);
        int warmup = Integer.parseInt(args[3]);
        int rounds = Integer.parseInt(args[4]);
        byte[] png = Files.readAllBytes(tile);

        report("decode", "us/tile", measure(warmup, rounds, () -> {
            int n = 20;
            long start = System.nanoTime();
            for (int i = 0; i < n; i++) {
                sink += new ParseTerrariumPngDSP().parse(RESOLUTION, Unpooled.wrappedBuffer(png))[1234];
            }
            return (System.nanoTime() - start) / 1e3 / n;
        }));

        GeographicProjection bte = new ScaleProjectionTransform(new FlipVerticalProjectionTransform(new BTEDymaxionProjection()), BTE_SCALE, BTE_SCALE);
        int points = 1_000_000;
        double[] lon = new double[points];
        double[] lat = new double[points];
        SplittableRandom random = new SplittableRandom(0x5EED);
        for (int i = 0; i < points; i++) {
            lon[i] = random.nextDouble(-180.0, 180.0);
            lat[i] = random.nextDouble(-85.0, 85.0);
        }
        double[] px = new double[points];
        double[] pz = new double[points];
        report("from", "ns/call", measure(warmup, rounds, () -> {
            long start = System.nanoTime();
            for (int i = 0; i < points; i++) {
                double[] p = bte.fromGeo(lon[i], lat[i]);
                px[i] = p[0];
                pz[i] = p[1];
            }
            return (System.nanoTime() - start) / (double) points;
        }));
        report("to", "ns/call", measure(warmup, rounds, () -> {
            long start = System.nanoTime();
            double acc = 0;
            for (int i = 0; i < points; i++) {
                acc += bte.toGeo(px[i], pz[i])[0];
            }
            sink += acc;
            return (System.nanoTime() - start) / (double) points;
        }));

        double[] heights = new ParseTerrariumPngDSP().parse(RESOLUTION, Unpooled.wrappedBuffer(png));
        IntToDoubleBiFunction sampler = (x, z) -> (x >> SHIFT) == tileX && (z >> SHIFT) == tileZ ? heights[(z & MASK) << SHIFT | (x & MASK)] : Double.NaN;
        WebMercatorProjection datasetProjection = new WebMercatorProjection(12);
        double[] centerGeo = datasetProjection.toGeo((tileX + 0.5) * RESOLUTION, (tileZ + 0.5) * RESOLUTION);
        double[] centerBlock = bte.fromGeo(centerGeo[0], centerGeo[1]);
        int centerChunkX = floorI(centerBlock[0]) >> 4;
        int centerChunkZ = floorI(centerBlock[1]) >> 4;
        int radius = 12;
        report("chunk", "us/chunk", measure(warmup, rounds, () -> {
            long start = System.nanoTime();
            int chunks = 0;
            for (int chunkX = centerChunkX - radius; chunkX <= centerChunkX + radius; chunkX++) {
                for (int chunkZ = centerChunkZ - radius; chunkZ <= centerChunkZ + radius; chunkZ++) {
                    sink += chunkHeights(bte, datasetProjection, chunkX, chunkZ, sampler)[17];
                    chunks++;
                }
            }
            return (System.nanoTime() - start) / 1e3 / chunks;
        }));
    }

    private static double[] chunkHeights(GeographicProjection projection, WebMercatorProjection datasetProjection, int chunkX, int chunkZ, IntToDoubleBiFunction sampler) {
        try {
            int baseX = ChunkPos.cubeToMinBlock(chunkX);
            int baseZ = ChunkPos.cubeToMinBlock(chunkZ);
            Bounds2d chunkBounds = Bounds2d.of(baseX, baseX + 16, baseZ, baseZ + 16);
            CornerBoundingBox2d localBounds = chunkBounds.toCornerBB(projection, false).toGeo().fromGeo(datasetProjection);
            localBounds.axisAlign().expand(BlendMode.CUBIC.size).validate(datasetProjection, false);
            double[] point = new double[2];
            double[] out = new double[256];
            double fx = 0.0d;
            for (int i = 0, x = 0; x < 16; x++, fx += 1.0d / 16) {
                double fz = 0.0d;
                for (int z = 0; z < 16; z++, fz += 1.0d / 16) {
                    point = localBounds.point(point, fx, fz);
                    out[i++] = BlendMode.CUBIC.get(point[0], point[1], sampler);
                }
            }
            return out;
        } catch (OutOfProjectionBoundsException e) {
            return new double[256];
        }
    }

    interface Round {
        double run() throws Exception;
    }

    private static double[] measure(int warmup, int rounds, Round round) throws Exception {
        for (int i = 0; i < warmup; i++) {
            round.run();
        }
        double[] results = new double[rounds];
        for (int i = 0; i < rounds; i++) {
            results[i] = round.run();
        }
        Arrays.sort(results);
        return results;
    }

    private static void report(String name, String unit, double[] sorted) {
        System.out.printf("%s\t%s\t%.3f\t%.3f\t%.3f%n", name, unit, sorted[sorted.length / 2], sorted[0], sorted[sorted.length - 1]);
    }
}
