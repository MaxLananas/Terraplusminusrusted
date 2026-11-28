package net.daporkchop.lib.binary.oio;

public final class StreamUtil {
    private StreamUtil() {}

    public static byte[] toByteArray(java.io.InputStream stream) throws java.io.IOException {
        return stream.readAllBytes();
    }
}
