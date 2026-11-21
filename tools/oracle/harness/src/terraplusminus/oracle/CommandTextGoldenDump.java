package terraplusminus.oracle;

import java.io.IOException;
import java.io.PrintWriter;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;
import java.util.SplittableRandom;
import net.buildtheearth.terraminusminus.projection.GeographicProjection;
import net.buildtheearth.terraminusminus.projection.OutOfProjectionBoundsException;
import net.buildtheearth.terraminusminus.projection.dymaxion.BTEDymaxionProjection;
import net.buildtheearth.terraminusminus.projection.transform.FlipVerticalProjectionTransform;
import net.buildtheearth.terraminusminus.projection.transform.ScaleProjectionTransform;

/**
 * What the chat commands print: {@code Double.toString} (string concatenation in /tpll and
 * /where), {@code String.format} with {@code %.6f} and {@code %.4f} in the root locale
 * (/distortion), and {@code GeographicProjection.tissot} of the BTE projection.
 */
public final class CommandTextGoldenDump {
    private static final double BTE_SCALE = 7318261.522857145;

    private CommandTextGoldenDump() {}

    public static void main(String[] args) throws IOException {
        Path output = Path.of(args[0]);
        SplittableRandom random = new SplittableRandom(20261122L);
        List<Double> values = new ArrayList<>(List.of(
                0.0, -0.0, 1.0, -1.0, 50.0, 0.1, 0.001, 0.00099999, 1.0E-4, 1.0E7, 9999999.999, 1.0E-300, Double.MIN_VALUE,
                Double.MAX_VALUE, 123456.789, 35.995, -113.24694444444444, 48.858222, 2.2945, 0.5, 1.5, 2.5, 0.125, 0.0000005,
                0.00000049999, 1.23456785, 89.99999999999999, -180.0, 1.0E21, 1.0E22, 12345678901234567890.0, 3.0E-5,
                Double.NaN, Double.POSITIVE_INFINITY, Double.NEGATIVE_INFINITY));
        for (int i = 0; i < 300; i++) {
            double magnitude = Math.pow(10, random.nextInt(-9, 12));
            values.add((random.nextDouble() * 2 - 1) * magnitude);
        }
        for (int i = 0; i < 200; i++) {
            values.add(random.nextDouble(-90, 90));
            values.add(random.nextDouble(-180, 180));
        }
        try (PrintWriter out = new PrintWriter(Files.newBufferedWriter(output.resolve("double_text.tsv"), StandardCharsets.UTF_8))) {
            out.print("bits\tto_string\tformat_6\tformat_4\n");
            for (double value : values) {
                out.print(Long.toHexString(Double.doubleToRawLongBits(value)) + "\t" + value + "\t"
                        + String.format(Locale.ROOT, "%.6f", value) + "\t" + String.format(Locale.ROOT, "%.4f", value) + "\n");
            }
        }

        GeographicProjection bte = new ScaleProjectionTransform(
                new FlipVerticalProjectionTransform(new BTEDymaxionProjection()), BTE_SCALE, BTE_SCALE);
        try (PrintWriter out = new PrintWriter(Files.newBufferedWriter(output.resolve("tissot_bte.tsv"), StandardCharsets.UTF_8))) {
            out.print("lon_bits\tlat_bits\toutcome\tarea_bits\tangular_bits\tmax_scale_bits\tmin_scale_bits\n");
            for (int i = 0; i < 400; i++) {
                double lon = random.nextDouble(-180, 180);
                double lat = random.nextDouble(-89.9, 89.9);
                String prefix = Long.toHexString(Double.doubleToRawLongBits(lon)) + "\t" + Long.toHexString(Double.doubleToRawLongBits(lat));
                try {
                    double[] tissot = bte.tissot(lon, lat);
                    StringBuilder line = new StringBuilder(prefix).append("\tok");
                    for (double component : tissot) {
                        line.append('\t').append(Long.toHexString(Double.doubleToRawLongBits(component)));
                    }
                    out.print(line.append('\n'));
                } catch (OutOfProjectionBoundsException e) {
                    out.print(prefix + "\tout_of_bounds\t\t\t\t\n");
                }
            }
        }
    }
}
