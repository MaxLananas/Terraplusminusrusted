package net.daporkchop.lib.common.reference.cache;

import net.daporkchop.lib.common.function.io.IOSupplier;
import net.daporkchop.lib.common.reference.ReferenceStrength;

/** Strongly memoizing replacement; the oracle is short-lived, so soft-reference eviction is irrelevant. */
public final class Cached<T> {
    private final IOSupplier<T> supplier;
    private T value;

    private Cached(IOSupplier<T> supplier) {
        this.supplier = supplier;
    }

    public static <T> Cached<T> global(IOSupplier<T> supplier, ReferenceStrength strength) {
        return new Cached<>(supplier);
    }

    public synchronized T get() {
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
