package terraplusminus.oracle;

import net.buildtheearth.terraminusminus.projection.AzimuthalEquidistantProjection;
import net.buildtheearth.terraminusminus.projection.EqualEarthProjection;
import net.buildtheearth.terraminusminus.projection.EquirectangularProjection;
import net.buildtheearth.terraminusminus.projection.GeographicProjection;
import net.buildtheearth.terraminusminus.projection.LambertAzimuthalProjection;
import net.buildtheearth.terraminusminus.projection.SinusoidalProjection;
import net.buildtheearth.terraminusminus.projection.StereographicProjection;
import net.buildtheearth.terraminusminus.projection.OutOfProjectionBoundsException;
import net.buildtheearth.terraminusminus.projection.dymaxion.BTEDymaxionProjection;
import net.buildtheearth.terraminusminus.projection.dymaxion.ConformalDynmaxionProjection;
import net.buildtheearth.terraminusminus.projection.dymaxion.DymaxionProjection;
import net.buildtheearth.terraminusminus.projection.mercator.CenteredMercatorProjection;
import net.buildtheearth.terraminusminus.projection.mercator.TransverseMercatorProjection;
import net.buildtheearth.terraminusminus.projection.mercator.WebMercatorProjection;
import net.buildtheearth.terraminusminus.projection.transform.ClampProjectionTransform;
import net.buildtheearth.terraminusminus.projection.transform.FlipVerticalProjectionTransform;
import net.buildtheearth.terraminusminus.projection.transform.OffsetProjectionTransform;
import net.buildtheearth.terraminusminus.projection.transform.RotateProjectionTransform;
import net.buildtheearth.terraminusminus.projection.transform.ScaleProjectionTransform;

import java.io.BufferedWriter;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.SplittableRandom;

/**
 * Writes golden samples of the upstream projection code. Each line holds the raw IEEE-754 bits of the
 * input pair followed by either the output pair or the token OUT_OF_BOUNDS.
 */
public final class ProjectionGoldenDump {
    private static final double BTE_SCALE = 7318261.522857145;
    private static final long SEED = 0x7E22A_2026L;

    private ProjectionGoldenDump() {}

    public static void main(String[] args) throws IOException {
        if (args.length != 1) {
            System.err.println("usage: ProjectionGoldenDump <output-directory>");
            System.exit(2);
        }
        Path output = Path.of(args[0]);
        Files.createDirectories(output);

        GeographicProjection bte = new ScaleProjectionTransform(
                new FlipVerticalProjectionTransform(new BTEDymaxionProjection()), BTE_SCALE, BTE_SCALE);
        GeographicProjection bteWithOffset = new OffsetProjectionTransform(bte, 1_234_567, -7_654_321);

        List<double[]> geoInputs = geographicInputs();
        writeForward(output.resolve("bte_from_geo.tsv"), bte, geoInputs);
        writeForward(output.resolve("bte_offset_from_geo.tsv"), bteWithOffset, geoInputs);
        writeForward(output.resolve("dymaxion_from_geo.tsv"), new DymaxionProjection(), geoInputs);
        writeForward(output.resolve("conformal_dymaxion_from_geo.tsv"), new ConformalDynmaxionProjection(), geoInputs);
        writeForward(output.resolve("web_mercator_z13_from_geo.tsv"), new WebMercatorProjection(13), geoInputs);

        writeInverse(output.resolve("bte_to_geo.tsv"), bte, blockInputs(bte, geoInputs));
        writeInverse(output.resolve("bte_offset_to_geo.tsv"), bteWithOffset, blockInputs(bteWithOffset, geoInputs));
        writeInverse(output.resolve("web_mercator_z13_to_geo.tsv"), new WebMercatorProjection(13), pixelInputs());

        // The rest of the dataset projection registry (GlobalParseRegistries.PROJECTIONS).
        writeBoth(output, "centered_mercator", new CenteredMercatorProjection(), geoInputs);
        writeBoth(output, "transverse_mercator", new TransverseMercatorProjection(), geoInputs);
        writeBoth(output, "equirectangular", new EquirectangularProjection(), geoInputs);
        writeBoth(output, "sinusoidal", new SinusoidalProjection(), geoInputs);
        writeBoth(output, "equal_earth", new EqualEarthProjection(), geoInputs);
        writeBoth(output, "lambert_azimuthal", new LambertAzimuthalProjection(12.5, 48.0), geoInputs);
        writeBoth(output, "azimuthal_equidistant", new AzimuthalEquidistantProjection(-61.5, 16.25), geoInputs);
        writeBoth(output, "azimuthal_equidistant_north", new AzimuthalEquidistantProjection(0.0, 90.0), geoInputs);
        writeBoth(output, "stereographic", new StereographicProjection(10.0, 20.0, 60.0), geoInputs);
        writeBoth(output, "clamp", new ClampProjectionTransform(new EquirectangularProjection(), -10.0, 20.0, -5.0, 50.0), geoInputs);
        writeBoth(output, "rotate", new RotateProjectionTransform(new EqualEarthProjection(), 33.0), geoInputs);
    }

    /** Forward samples, then inverse samples at their images and across the bounds widened by a quarter. */
    private static void writeBoth(Path output, String name, GeographicProjection projection, List<double[]> geoInputs) throws IOException {
        writeForward(output.resolve(name + "_from_geo.tsv"), projection, geoInputs);
        List<double[]> inputs = new ArrayList<>();
        for (double[] geo : geoInputs) {
            try {
                inputs.add(projection.fromGeo(geo[0], geo[1]));
            } catch (OutOfProjectionBoundsException ignored) {
                // Nothing to invert.
            }
        }
        double[] bounds = projection.bounds();
        double padX = (bounds[2] - bounds[0]) / 4;
        double padY = (bounds[3] - bounds[1]) / 4;
        SplittableRandom random = new SplittableRandom(SEED ^ name.hashCode());
        for (int i = 0; i < 2000; i++) {
            inputs.add(new double[]{ random.nextDouble(bounds[0] - padX, bounds[2] + padX), random.nextDouble(bounds[1] - padY, bounds[3] + padY) });
        }
        writeInverse(output.resolve(name + "_to_geo.tsv"), projection, inputs);
    }

    private static List<double[]> geographicInputs() {
        List<double[]> inputs = new ArrayList<>();
        for (int lat = -90; lat <= 90; lat += 5) {
            for (int lon = -180; lon <= 180; lon += 5) {
                inputs.add(new double[]{ lon, lat });
            }
        }
        double[] edges = { -180.0, -179.99999999, 179.99999999, 180.0, Math.nextUp(180.0), -90.0, 90.0, Math.nextDown(-90.0) };
        for (double edge : edges) {
            inputs.add(new double[]{ edge, 0.0 });
            inputs.add(new double[]{ 0.0, edge });
        }
        inputs.add(new double[]{ Double.NaN, 0.0 });
        inputs.add(new double[]{ 13.4050, 52.5200 });
        inputs.add(new double[]{ -61.5333, 16.2650 });
        inputs.add(new double[]{ 151.2153, -33.8568 });
        SplittableRandom random = new SplittableRandom(SEED);
        for (int i = 0; i < 6000; i++) {
            inputs.add(new double[]{ random.nextDouble(-180.0, 180.0), random.nextDouble(-90.0, 90.0) });
        }
        for (int i = 0; i < 2000; i++) {
            inputs.add(new double[]{ random.nextDouble(5.0, 15.5), random.nextDouble(47.0, 55.5) });
        }
        return inputs;
    }

    /** Block-space inputs: forward images of geographic samples, their integer floors, and a wide random sweep. */
    private static List<double[]> blockInputs(GeographicProjection projection, List<double[]> geoInputs) {
        List<double[]> inputs = new ArrayList<>();
        for (double[] geo : geoInputs) {
            try {
                double[] block = projection.fromGeo(geo[0], geo[1]);
                inputs.add(block);
                inputs.add(new double[]{ Math.floor(block[0]), Math.floor(block[1]) });
            } catch (OutOfProjectionBoundsException ignored) {
                // Out-of-range geographic inputs have no block image to invert.
            }
        }
        SplittableRandom random = new SplittableRandom(SEED ^ 0x5EA);
        for (int i = 0; i < 4000; i++) {
            inputs.add(new double[]{ random.nextInt(-40_000_000, 40_000_000), random.nextInt(-40_000_000, 40_000_000) });
        }
        return inputs;
    }

    private static List<double[]> pixelInputs() {
        List<double[]> inputs = new ArrayList<>();
        double size = 256 << 13;
        SplittableRandom random = new SplittableRandom(SEED ^ 0x3E2C);
        for (int i = 0; i < 2000; i++) {
            inputs.add(new double[]{ random.nextDouble(-16.0, size + 16.0), random.nextDouble(-16.0, size + 16.0) });
        }
        inputs.add(new double[]{ 0.0, 0.0 });
        inputs.add(new double[]{ size, size });
        inputs.add(new double[]{ -0.0, Math.nextUp(size) });
        return inputs;
    }

    private static void writeForward(Path path, GeographicProjection projection, List<double[]> inputs) throws IOException {
        try (BufferedWriter writer = Files.newBufferedWriter(path, StandardCharsets.UTF_8)) {
            writer.write("# from_geo " + projection + '\n');
            for (double[] input : inputs) {
                double[] result;
                try {
                    result = projection.fromGeo(input[0], input[1]);
                } catch (OutOfProjectionBoundsException e) {
                    result = null;
                }
                writeLine(writer, input, result);
            }
        }
    }

    private static void writeInverse(Path path, GeographicProjection projection, List<double[]> inputs) throws IOException {
        try (BufferedWriter writer = Files.newBufferedWriter(path, StandardCharsets.UTF_8)) {
            writer.write("# to_geo " + projection + '\n');
            for (double[] input : inputs) {
                double[] result;
                try {
                    result = projection.toGeo(input[0], input[1]);
                } catch (OutOfProjectionBoundsException e) {
                    result = null;
                }
                writeLine(writer, input, result);
            }
        }
    }

    private static void writeLine(BufferedWriter writer, double[] input, double[] result) throws IOException {
        writer.write(bits(input[0]) + '\t' + bits(input[1]) + '\t');
        writer.write(result == null ? "OUT_OF_BOUNDS" : bits(result[0]) + '\t' + bits(result[1]));
        writer.write('\n');
    }

    private static String bits(double value) {
        return String.format("%016x", Double.doubleToRawLongBits(value));
    }
}
