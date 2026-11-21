package net.daporkchop.lib.common.math;

/** Method bodies copied verbatim from PorkLib commit 836334a9655fe9c5fae088db74a7830af108a2bd. */
public final class PMath {
    private PMath() {}

    public static int floorI(double d) {
        return (int) Math.floor(d);
    }

    public static int ceilI(double d) {
        return (int) Math.ceil(d);
    }

    public static double lerp(double a, double b, double t) {
        return a + (b - a) * t;
    }
}
