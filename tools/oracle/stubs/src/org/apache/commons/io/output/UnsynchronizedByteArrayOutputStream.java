package org.apache.commons.io.output;

import java.io.ByteArrayOutputStream;

/** Oracle-only stand-in: an unsynchronised byte array stream with commons-io's builder. */
public class UnsynchronizedByteArrayOutputStream extends ByteArrayOutputStream {
    public UnsynchronizedByteArrayOutputStream(int size) {
        super(size);
    }

    public static Builder builder() {
        return new Builder();
    }

    public static final class Builder {
        private int bufferSize = 1024;

        public Builder setBufferSize(int bufferSize) {
            this.bufferSize = bufferSize;
            return this;
        }

        public UnsynchronizedByteArrayOutputStream get() {
            return new UnsynchronizedByteArrayOutputStream(bufferSize);
        }
    }
}
