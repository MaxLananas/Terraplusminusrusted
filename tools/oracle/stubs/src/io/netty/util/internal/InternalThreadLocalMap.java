package io.netty.util.internal;

/** Placeholder handle: the FastThreadLocal stub keeps its values in a plain ThreadLocal. */
public final class InternalThreadLocalMap {
    private static final InternalThreadLocalMap INSTANCE = new InternalThreadLocalMap();

    private InternalThreadLocalMap() {}

    public static InternalThreadLocalMap get() {
        return INSTANCE;
    }
}
