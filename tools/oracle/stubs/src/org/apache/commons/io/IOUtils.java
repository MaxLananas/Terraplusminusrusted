package org.apache.commons.io;

import java.io.ByteArrayOutputStream;
import java.io.Closeable;
import java.io.EOFException;
import java.io.IOException;
import java.io.InputStream;

/** The commons-io calls commons-imaging makes, with commons-io's semantics. Oracle only. */
public final class IOUtils {
    private IOUtils() {}

    public static void close(Closeable closeable) throws IOException {
        if (closeable != null) {
            closeable.close();
        }
    }

    public static long consume(InputStream input) throws IOException {
        return input.transferTo(java.io.OutputStream.nullOutputStream());
    }

    public static long skip(InputStream input, long toSkip) throws IOException {
        if (toSkip < 0) {
            throw new IllegalArgumentException("Skip count must be non-negative, actual: " + toSkip);
        }
        byte[] buffer = new byte[8192];
        long remain = toSkip;
        while (remain > 0) {
            int n = input.read(buffer, 0, (int) Math.min(remain, buffer.length));
            if (n < 0) {
                break;
            }
            remain -= n;
        }
        return toSkip - remain;
    }

    public static byte[] toByteArray(InputStream input) throws IOException {
        ByteArrayOutputStream output = new ByteArrayOutputStream();
        input.transferTo(output);
        return output.toByteArray();
    }

    public static byte[] toByteArray(InputStream input, int size) throws IOException {
        byte[] data = new byte[size];
        int offset = 0;
        while (offset < size) {
            int n = input.read(data, offset, size - offset);
            if (n < 0) {
                throw new EOFException("Unexpected read size, current: " + offset + ", expected: " + size);
            }
            offset += n;
        }
        return data;
    }
}
