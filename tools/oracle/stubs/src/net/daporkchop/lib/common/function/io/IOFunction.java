package net.daporkchop.lib.common.function.io;

import java.io.IOException;

/** Signature-compatible replacement for PorkLib's IOFunction. */
@FunctionalInterface
public interface IOFunction<T, R> {
    R applyThrowing(T t) throws IOException;
}
