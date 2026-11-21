package io.netty.buffer;

/** Big-endian read cursor matching the subset of Netty's ByteBuf used by the projection code. */
public final class ByteBuf {
    private final java.nio.ByteBuffer buffer;

    ByteBuf(byte[] bytes) {
        this.buffer = java.nio.ByteBuffer.wrap(bytes).order(java.nio.ByteOrder.BIG_ENDIAN);
    }

    public double readDouble() {
        return this.buffer.getDouble();
    }
}
