package bench;

import io.netty.buffer.Unpooled;
import net.buildtheearth.terraminusminus.config.scalarparse.d.ParseTerrariumPngDSP;
import net.buildtheearth.terraminusminus.config.scalarparse.d.ParseTiffDSP;
import net.buildtheearth.terraminusminus.config.scalarparse.i.ParseJpgISP;
import net.buildtheearth.terraminusminus.projection.GeographicProjection;
import net.buildtheearth.terraminusminus.projection.dymaxion.BTEDymaxionProjection;
import net.buildtheearth.terraminusminus.projection.transform.FlipVerticalProjectionTransform;
import net.buildtheearth.terraminusminus.projection.transform.ScaleProjectionTransform;

import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Arrays;
import java.util.SplittableRandom;

public class Bench {
    interface Op { double run() throws Exception; }
    static volatile double sink;

    static void measure(String name, Op op, double warmSeconds, double runSeconds) throws Exception {
        long end = System.nanoTime() + (long) (warmSeconds * 1e9);
        while (System.nanoTime() < end) sink += op.run();
        double[] samples = new double[4096];
        int n = 0;
        end = System.nanoTime() + (long) (runSeconds * 1e9);
        while (System.nanoTime() < end && n < samples.length) {
            long t = System.nanoTime();
            sink += op.run();
            samples[n++] = (System.nanoTime() - t) / 1e3;
        }
        double[] s = Arrays.copyOf(samples, n);
        Arrays.sort(s);
        System.out.printf("%s\t%d\t%.1f\t%.1f%n", name, n, s[n / 2], s[(int) (n * 0.99)]);
    }

    public static void main(String[] args) throws Exception {
        double warm = Double.parseDouble(args[0]), run = Double.parseDouble(args[1]);
        byte[] png = Files.readAllBytes(Path.of("terrarium_256.png"));
        byte[] jpg = Files.readAllBytes(Path.of("tile_256.jpg"));
        byte[] f32 = Files.readAllBytes(Path.of("dem_f32_deflate_1024.tif"));
        byte[] i16 = Files.readAllBytes(Path.of("dem_i16_deflate_pred2_512.tif"));
        System.out.println("name\titerations\tmedian_us\tp99_us");
        measure("terrarium_png_256", () -> new ParseTerrariumPngDSP().parse(256, Unpooled.wrappedBuffer(png))[12345], warm, run);
        measure("jpeg_256", () -> new ParseJpgISP().parse(256, Unpooled.wrappedBuffer(jpg))[12345], warm, run);
        measure("tiff_f32_deflate_1024", () -> new ParseTiffDSP().parse(1024, Unpooled.wrappedBuffer(f32))[12345], warm, run);
        measure("tiff_i16_pred2_512", () -> new ParseTiffDSP().parse(512, Unpooled.wrappedBuffer(i16))[12345], warm, run);

        GeographicProjection bte = new ScaleProjectionTransform(new FlipVerticalProjectionTransform(new BTEDymaxionProjection()), 7318261.522857145, 7318261.522857145);
        SplittableRandom random = new SplittableRandom(42);
        int points = 100_000;
        double[] lon = new double[points], lat = new double[points];
        for (int i = 0; i < points; i++) { lon[i] = random.nextDouble(-180, 180); lat[i] = random.nextDouble(-85, 85); }
        measure("bte_fromGeo_100k", () -> { double acc = 0; for (int i = 0; i < points; i++) acc += bte.fromGeo(lon[i], lat[i])[0]; return acc; }, warm, run);
        double[][] xy = new double[points][];
        for (int i = 0; i < points; i++) xy[i] = bte.fromGeo(lon[i], lat[i]);
        measure("bte_toGeo_100k", () -> { double acc = 0; for (int i = 0; i < points; i++) acc += bte.toGeo(xy[i][0], xy[i][1])[0]; return acc; }, warm, run);
    }
}
