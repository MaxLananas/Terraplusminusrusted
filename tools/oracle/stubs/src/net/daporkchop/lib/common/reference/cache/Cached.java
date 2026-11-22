package net.daporkchop.lib.common.reference.cache;

import net.daporkchop.lib.common.function.io.IOSupplier;
import net.daporkchop.lib.common.reference.ReferenceStrength;

/** Strongly memoizing replacement; the oracle is short-lived, so soft-reference eviction is irrelevant. */
public final class Cached<T> {
    private final IOSupplier<T> supplier;
    private final ThreadLocal<T> perThread;
    private T value;

    private Cached(IOSupplier<T> supplier, boolean threadLocal) {
        this.supplier = supplier;
        this.perThread = threadLocal ? new ThreadLocal<>() : null;
    }

    public static <T> Cached<T> global(IOSupplier<T> supplier, ReferenceStrength strength) {
        return new Cached<>(supplier, false);
    }

    public static <T> Cached<T> threadLocal(IOSupplier<T> supplier, ReferenceStrength strength) {
        return new Cached<>(supplier, true);
    }

    public T get() {
        if (this.perThread == null) {
            return this.getGlobal();
        }
        T current = this.perThread.get();
        if (current == null) {
            try {
                current = this.supplier.getThrowing();
            } catch (java.io.IOException e) {
                throw new java.io.UncheckedIOException(e);
            }
            this.perThread.set(current);
        }
        return current;
    }

    private synchronized T getGlobal() {
        if (this.value == null) {
            try {
                this.value = this.supplier.getThrowing();
            } catch (java.io.IOException e) {
                throw new java.io.UncheckedIOException(e);
            }
        }
        return this.value;
    }
}
