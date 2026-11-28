package terraplusminus.oracle;

import java.io.IOException;
import java.io.PrintWriter;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;
import net.buildtheearth.terraminusminus.util.geo.CoordinateParseUtils;
import net.buildtheearth.terraminusminus.util.geo.LatLng;

/** Runs every input line through {@code CoordinateParseUtils.parseVerbatimCoordinates}. */
public final class CoordinateParseGoldenDump {
    private CoordinateParseGoldenDump() {}

    public static void main(String[] args) throws IOException {
        List<String> inputs = Files.readAllLines(Path.of(args[0]), StandardCharsets.UTF_8);
        try (PrintWriter out = new PrintWriter(Files.newBufferedWriter(Path.of(args[1], "coordinate_parse.tsv"), StandardCharsets.UTF_8))) {
            out.print("index\toutcome\tlat_bits\tlng_bits\n");
            for (int i = 0; i < inputs.size(); i++) {
                String outcome;
                String lat = "";
                String lng = "";
                try {
                    LatLng parsed = CoordinateParseUtils.parseVerbatimCoordinates(inputs.get(i));
                    if (parsed == null) {
                        outcome = "null";
                    } else {
                        outcome = "ok";
                        lat = Long.toHexString(Double.doubleToRawLongBits(parsed.getLat()));
                        lng = Long.toHexString(Double.doubleToRawLongBits(parsed.getLng()));
                    }
                } catch (RuntimeException e) {
                    outcome = "throws:" + e.getClass().getSimpleName();
                }
                out.print(i + "\t" + outcome + "\t" + lat + "\t" + lng + "\n");
            }
        }
    }
}
