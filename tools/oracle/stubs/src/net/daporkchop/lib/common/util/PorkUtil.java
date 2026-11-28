package net.daporkchop.lib.common.util;

/** Method bodies copied verbatim from PorkLib commit 836334a9655fe9c5fae088db74a7830af108a2bd. */
public final class PorkUtil {
    private PorkUtil() {}

    @SuppressWarnings("unchecked")
    public static <T> T uncheckedCast(Object value) {
        return (T) value;
    }

    public static String className(Object obj) {
        return obj == null ? "null" : obj.getClass().getTypeName();
    }
}
