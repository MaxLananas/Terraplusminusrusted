package net.daporkchop.lib.common.function.io;

import java.io.IOException;

/** Signature-compatible replacement for PorkLib's IOBiFunction (only used by parsers, not run by the oracle). */
@FunctionalInterface
public interface IOBiFunction<T, U, R> {
    R applyThrowing(T t, U u) throws IOException;
}
