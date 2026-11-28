package io.netty.buffer;

public final class ByteBufInputStream extends java.io.InputStream {
    private final java.nio.ByteBuffer buffer;

    public ByteBufInputStream(ByteBuf source) {
        this.buffer = source.buffer;
    }

    @Override
    public int read() {
        return this.buffer.hasRemaining() ? this.buffer.get() & 0xFF : -1;
    }

    @Override
    public int read(byte[] destination, int offset, int length) {
        if (!this.buffer.hasRemaining()) {
            return -1;
        }
        int count = Math.min(length, this.buffer.remaining());
        this.buffer.get(destination, offset, count);
        return count;
    }
}
