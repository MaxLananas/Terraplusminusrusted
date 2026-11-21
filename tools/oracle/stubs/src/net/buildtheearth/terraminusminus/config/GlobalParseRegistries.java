package net.buildtheearth.terraminusminus.config;

import com.google.common.collect.BiMap;
import com.google.common.collect.ImmutableBiMap;
import net.buildtheearth.terraminusminus.projection.GeographicProjection;

/** JSON parsing is out of scope for the numeric oracle; projections are constructed directly. */
public final class GlobalParseRegistries {
    private GlobalParseRegistries() {}

    public static final BiMap<String, Class<? extends GeographicProjection>> PROJECTIONS = ImmutableBiMap.of();
}
