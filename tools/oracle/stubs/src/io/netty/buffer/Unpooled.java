package io.netty.buffer;

public final class Unpooled {
    private Unpooled() {}

    public static ByteBuf wrappedBuffer(byte[] bytes) {
        return new ByteBuf(bytes);
    }
}
