package org.apache.commons.io;

import java.io.IOException;
import java.io.RandomAccessFile;

/** Oracle-only stand-in for commons-io's helper. */
public final class RandomAccessFiles {
    private RandomAccessFiles() {}

    public static byte[] read(RandomAccessFile input, long position, int length) throws IOException {
        input.seek(position);
        byte[] data = new byte[length];
        input.readFully(data);
        return data;
    }
}
