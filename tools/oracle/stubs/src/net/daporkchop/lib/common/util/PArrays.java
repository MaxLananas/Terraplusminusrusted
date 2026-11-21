package net.daporkchop.lib.common.util;

import java.util.function.IntFunction;

public final class PArrays {
    private PArrays() {}

    public static <T> T[] filledBy(int length, IntFunction<T[]> arrayFactory, IntFunction<T> elementFactory) {
        T[] array = arrayFactory.apply(length);
        for (int i = 0; i < length; i++) {
            array[i] = elementFactory.apply(i);
        }
        return array;
    }
}
