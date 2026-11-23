package org.apache.commons.io.build;

import java.io.ByteArrayInputStream;
import java.io.File;
import java.io.IOException;
import java.io.InputStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Arrays;

/** Oracle-only stand-in for the commons-io origins that commons-imaging's ByteSource uses. */
public abstract class AbstractOrigin<T, B extends AbstractOrigin<T, B>> {
    protected final T origin;

    protected AbstractOrigin(T origin) {
        this.origin = origin;
    }

    public abstract InputStream getInputStream() throws IOException;

    public byte[] getByteArray() throws IOException {
        try (InputStream input = getInputStream()) {
            return input.readAllBytes();
        }
    }

    public byte[] getByteArray(long position, int length) throws IOException {
        byte[] all = getByteArray();
        int start = (int) position;
        if (start < 0 || length < 0 || start + length > all.length) {
            throw new IllegalArgumentException("Couldn't read array (start: " + start + ", length: " + length + ", data length: " + all.length + ").");
        }
        return Arrays.copyOfRange(all, start, start + length);
    }

    public long size() throws IOException {
        return getByteArray().length;
    }

    public static class ByteArrayOrigin extends AbstractOrigin<byte[], ByteArrayOrigin> {
        public ByteArrayOrigin(byte[] origin) {
            super(origin);
        }

        @Override
        public InputStream getInputStream() {
            return new ByteArrayInputStream(origin);
        }

        @Override
        public byte[] getByteArray() {
            return origin;
        }
    }

    public static class FileOrigin extends AbstractOrigin<File, FileOrigin> {
        public FileOrigin(File origin) {
            super(origin);
        }

        @Override
        public InputStream getInputStream() throws IOException {
            return Files.newInputStream(origin.toPath());
        }
    }

    public static class PathOrigin extends AbstractOrigin<Path, PathOrigin> {
        public PathOrigin(Path origin) {
            super(origin);
        }

        @Override
        public InputStream getInputStream() throws IOException {
            return Files.newInputStream(origin);
        }
    }

    /**
     * Commons-io buffers the stream so that it can be read more than once (size, then contents);
     * ByteSource relies on that for its {@code size()} and positional reads.
     */
    public static class InputStreamOrigin extends AbstractOrigin<InputStream, InputStreamOrigin> {
        private byte[] bytes;

        public InputStreamOrigin(InputStream origin) {
            super(origin);
        }

        @Override
        public synchronized byte[] getByteArray() throws IOException {
            if (bytes == null) {
                bytes = origin.readAllBytes();
            }
            return bytes;
        }

        @Override
        public InputStream getInputStream() throws IOException {
            return new ByteArrayInputStream(getByteArray());
        }
    }
}
