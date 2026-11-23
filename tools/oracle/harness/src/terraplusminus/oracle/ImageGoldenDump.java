package terraplusminus.oracle;

import io.netty.buffer.Unpooled;
import net.buildtheearth.terraminusminus.config.scalarparse.d.DoubleScalarParser;
import net.buildtheearth.terraminusminus.config.scalarparse.d.FromIntDSP;
import net.buildtheearth.terraminusminus.config.scalarparse.d.ParseTiffDSP;
import net.buildtheearth.terraminusminus.config.scalarparse.i.AddISP;
import net.buildtheearth.terraminusminus.config.scalarparse.i.AndISP;
import net.buildtheearth.terraminusminus.config.scalarparse.i.FlipXISP;
import net.buildtheearth.terraminusminus.config.scalarparse.i.FlipZISP;
import net.buildtheearth.terraminusminus.config.scalarparse.i.GrayscaleExtractISP;
import net.buildtheearth.terraminusminus.config.scalarparse.i.IntScalarParser;
import net.buildtheearth.terraminusminus.config.scalarparse.i.ParseJpgISP;
import net.buildtheearth.terraminusminus.config.scalarparse.i.ParsePngISP;
import net.buildtheearth.terraminusminus.config.scalarparse.i.ParseTiffISP;
import net.buildtheearth.terraminusminus.config.scalarparse.i.RGBExtractISP;
import net.buildtheearth.terraminusminus.config.scalarparse.i.RequireOpaqueISP;
import net.buildtheearth.terraminusminus.config.scalarparse.i.SwapAxesISP;
import org.apache.commons.imaging.bytesource.ByteSource;
import org.apache.commons.imaging.formats.tiff.TiffImageParser;
import org.apache.commons.imaging.formats.tiff.TiffImagingParameters;

import javax.imageio.ImageIO;
import java.awt.image.BufferedImage;
import java.io.BufferedWriter;
import java.io.ByteArrayInputStream;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.function.Function;
import java.util.stream.Stream;

/**
 * Image decoding as terraminusminus' parsers see it.
 *
 * <ul>
 *   <li>{@code argb.tsv}: every input decoded to ARGB the way {@code ParsePngISP} / {@code ParseJpgISP}
 *   ({@code ImageIO.read}) or {@code ParseTiffISP} (Commons Imaging {@code TiffImageParser}) does, then
 *   {@code getRGB}; non-square images included. Name, width, height and pixels, or UNREADABLE.</li>
 *   <li>{@code parsers.tsv}: upstream's own {@code IntScalarParser} chains, and {@code from_int} on top,
 *   over every square input of at most 32 pixels (plus one non-square image, which must fail). Columns: file, the parser as
 *   it is written in a heights config, resolution, then INT and the ints in hex, DOUBLE and the doubles'
 *   raw bits in hex, or ERROR and the exception.</li>
 *   <li>{@code raster.tsv}: upstream's {@code ParseTiffDSP} ({@code parse_tiff} for doubles) on every
 *   square TIFF, as raw double bits, or ERROR.</li>
 * </ul>
 */
public final class ImageGoldenDump {
    private ImageGoldenDump() {}

    public static void main(String[] args) throws IOException {
        if (args.length != 2) {
            System.err.println("usage: ImageGoldenDump <input-directory> <output-directory>");
            System.exit(2);
        }
        List<Path> inputs;
        try (Stream<Path> files = Files.list(Path.of(args[0]))) {
            inputs = files.filter(Files::isRegularFile).filter(path -> !path.getFileName().toString().endsWith(".py")).sorted().toList();
        }
        Path output = Path.of(args[1]);
        Files.createDirectories(output);
        try (BufferedWriter argb = writer(output, "argb.tsv");
             BufferedWriter parsers = writer(output, "parsers.tsv");
             BufferedWriter raster = writer(output, "raster.tsv")) {
            for (Path input : inputs) {
                String name = input.getFileName().toString();
                byte[] bytes = Files.readAllBytes(input);
                boolean tiff = name.endsWith(".tif");
                BufferedImage image = decode(bytes, tiff);
                argb.write(name);
                if (image == null) {
                    argb.write("\tUNREADABLE\n");
                    continue;
                }
                int width = image.getWidth();
                int height = image.getHeight();
                argb.write("\t" + width + "\t" + height);
                for (int pixel : image.getRGB(0, 0, width, height, null, 0, width)) {
                    argb.write('\t');
                    argb.write(String.format("%08x", pixel));
                }
                argb.write('\n');
                boolean square = width == height;
                // Chains run on small squares only (fixture size), plus one non-square image that must fail.
                if ((!square || width > 32) && !name.equals("png_wide.png")) {
                    continue;
                }
                int resolution = square ? width : height * 2;
                for (Map.Entry<String, Function<IntScalarParser, IntScalarParser>> chain : chains().entrySet()) {
                    String base = tiff ? "parse_tiff" : name.endsWith(".jpg") ? "parse_jpg" : "parse_png";
                    IntScalarParser parser = chain.getValue().apply(tiff ? new ParseTiffISP() : name.endsWith(".jpg") ? new ParseJpgISP() : new ParsePngISP());
                    String spec = chain.getKey().replace("BASE", "{\"" + base + "\": {}}");
                    parsers.write(name + "\t" + spec + "\t" + resolution);
                    try {
                        int[] values = parser.parse(resolution, Unpooled.wrappedBuffer(bytes));
                        parsers.write("\tINT");
                        for (int value : values) {
                            parsers.write('\t');
                            parsers.write(String.format("%08x", value));
                        }
                    } catch (Exception e) {
                        parsers.write("\tERROR\t" + e.getClass().getSimpleName());
                    }
                    parsers.write('\n');
                    DoubleScalarParser fromInt = new FromIntDSP(parser);
                    parsers.write(name + "\t{\"from_int\": {\"delegate\": " + spec + "}}\t" + resolution);
                    writeDoubles(parsers, () -> fromInt.parse(resolution, Unpooled.wrappedBuffer(bytes)));
                }
                if (tiff && square) {
                    raster.write(name + "\t{\"parse_tiff\": {}}\t" + resolution);
                    writeDoubles(raster, () -> new ParseTiffDSP().parse(resolution, Unpooled.wrappedBuffer(bytes)));
                }
            }
        }
    }

    private interface DoubleSource {
        double[] get() throws IOException;
    }

    private static void writeDoubles(BufferedWriter writer, DoubleSource source) throws IOException {
        try {
            double[] values = source.get();
            writer.write("\tDOUBLE");
            for (double value : values) {
                writer.write('\t');
                writer.write(Long.toHexString(Double.doubleToRawLongBits(value)));
            }
        } catch (Exception e) {
            writer.write("\tERROR\t" + e.getClass().getSimpleName());
        }
        writer.write('\n');
    }

    /** Every integer operator once over the base decoder, and a few compositions. */
    private static Map<String, Function<IntScalarParser, IntScalarParser>> chains() {
        Map<String, Function<IntScalarParser, IntScalarParser>> chains = new LinkedHashMap<>();
        chains.put("BASE", base -> base);
        chains.put("{\"rgb_extract\": {\"delegate\": BASE}}", RGBExtractISP::new);
        chains.put("{\"grayscale_extract\": {\"delegate\": BASE}}", GrayscaleExtractISP::new);
        chains.put("{\"require_opaque\": {\"delegate\": BASE}}", RequireOpaqueISP::new);
        chains.put("{\"and\": {\"mask\": 15790320, \"delegate\": BASE}}", base -> new AndISP(base, 0x00F0F0F0));
        chains.put("{\"add\": {\"value\": -123456, \"delegate\": BASE}}", base -> new AddISP(base, -123456));
        chains.put("{\"flip_x\": {\"delegate\": BASE}}", FlipXISP::new);
        chains.put("{\"flip_z\": {\"delegate\": BASE}}", FlipZISP::new);
        chains.put("{\"swap_axes\": {\"delegate\": BASE}}", SwapAxesISP::new);
        chains.put("{\"and\": {\"mask\": 65280, \"delegate\": {\"require_opaque\": {\"delegate\": BASE}}}}", base -> new AndISP(new RequireOpaqueISP(base), 0xFF00));
        chains.put("{\"add\": {\"value\": 1, \"delegate\": {\"grayscale_extract\": {\"delegate\": {\"swap_axes\": {\"delegate\": BASE}}}}}}",
                base -> new AddISP(new GrayscaleExtractISP(new SwapAxesISP(base)), 1));
        return chains;
    }

    private static BufferedWriter writer(Path directory, String name) throws IOException {
        return Files.newBufferedWriter(directory.resolve(name), StandardCharsets.UTF_8);
    }

    private static BufferedImage decode(byte[] bytes, boolean tiff) {
        try {
            if (tiff) {
                ByteSource source = ByteSource.inputStream(new ByteArrayInputStream(bytes), "");
                return new TiffImageParser().getBufferedImage(source, new TiffImagingParameters());
            }
            return ImageIO.read(new ByteArrayInputStream(bytes));
        } catch (IOException | RuntimeException e) {
            return null;
        }
    }
}
