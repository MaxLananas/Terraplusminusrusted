package net.daporkchop.lib.common.util;

public final class PValidation {
    private PValidation() {}

    public static void checkArg(boolean condition, String message) {
        if (!condition) throw new IllegalArgumentException(message);
    }

    public static void checkArg(boolean condition, String format, Object... arguments) {
        if (!condition) throw new IllegalArgumentException(String.format(format, arguments));
    }

    public static int positive(int value) {
        checkArg(value > 0, "argument must be positive (given: %d)", value);
        return value;
    }

    public static int positive(int value, Object name) {
        checkArg(value > 0, "%2$s must be positive (given: %1$d)", value, name);
        return value;
    }

    public static int notNegative(int value, String name) {
        if (value < 0) throw new IllegalArgumentException(name + " cannot be negative! (" + value + ')');
        return value;
    }

    public static void checkState(boolean condition, String message) {
        if (!condition) throw new IllegalStateException(message);
    }

    public static void checkState(boolean condition, String format, Object... arguments) {
        if (!condition) throw new IllegalStateException(String.format(format, arguments));
    }
}
