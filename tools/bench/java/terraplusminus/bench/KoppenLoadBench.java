package terraplusminus.bench;

import java.io.FileInputStream;
import java.io.InputStream;
import java.util.Arrays;
import java.util.zip.GZIPInputStream;

/**
 * Times the startup load of the Koppen raster exactly as upstream {@code KoppenClimateData.CACHE}
 * does it (GZIPInputStream, 4096-byte reads, per-byte RLEByteArray.Builder.append). The builder is
 * transcribed with growable primitive arrays standing in for fastutil's IntArrayList/ByteArrayList.
 */
public final class KoppenLoadBench {
    static final class Builder {
        int[] index = new int[1024];
        byte[] data = new byte[1024];
        int runs;
        int size;
        byte last;

        void append(byte val) {
            int size = this.size++;
            if (size == 0 || this.last != val) {
                if (runs == index.length) {
                    index = Arrays.copyOf(index, runs * 2);
                    data = Arrays.copyOf(data, runs * 2);
                }
                index[runs] = size;
                data[runs++] = this.last = val;
            }
        }
    }

    public static void main(String[] args) throws Exception {
        int rounds = Integer.parseInt(args[1]);
        for (int r = 0; r < rounds; r++) {
            long start = System.nanoTime();
            Builder builder = new Builder();
            try (InputStream is = new GZIPInputStream(new FileInputStream(args[0]))) {
                byte[] buffer = new byte[4096];
                int ready;
                while ((ready = is.read(buffer, 0, buffer.length)) != -1) {
                    for (int i = 0; i < ready; i++) {
                        builder.append(buffer[i]);
                    }
                }
            }
            long micros = (System.nanoTime() - start) / 1000;
            System.out.println("koppen_load\tms\t" + micros / 1000.0 + "\truns=" + builder.runs + "\tsize=" + builder.size);
        }
    }
}
