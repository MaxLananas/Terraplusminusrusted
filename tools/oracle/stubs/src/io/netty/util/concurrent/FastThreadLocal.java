package io.netty.util.concurrent;

import io.netty.util.internal.InternalThreadLocalMap;

/** Per-thread value with Netty's FastThreadLocal semantics (initial value null). */
public class FastThreadLocal<V> {
    private final ThreadLocal<V> values = new ThreadLocal<>();

    public final V get(InternalThreadLocalMap map) {
        return this.values.get();
    }

    public final void set(InternalThreadLocalMap map, V value) {
        this.values.set(value);
    }
}
