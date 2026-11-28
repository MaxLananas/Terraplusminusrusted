package net.daporkchop.lib.common.function.io;

@FunctionalInterface
public interface IOSupplier<T> {
    T getThrowing() throws java.io.IOException;
}
