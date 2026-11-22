package net.buildtheearth.terraminusminus.generator;

import static net.daporkchop.lib.common.math.PMath.clamp;

import java.util.Arrays;
import java.util.HashMap;
import java.util.Map;

import net.buildtheearth.terraminusminus.substitutes.BlockState;

/**
 * The part of TerraMinusMinus' CachedChunkData that OpenStreetMap drawing touches: the builder's
 * water depth, surface blocks and custom attributes. Constants and method bodies are copied
 * verbatim from the pinned commit; heights, biomes and the immutable result are not needed.
 */
public class CachedChunkData {
    public static final int WATERDEPTH_DEFAULT = (byte) 0x80;

    public static final int WATERDEPTH_TYPE_MASK = (byte) 0xC0;
    public static final int WATERDEPTH_TYPE_DEFAULT = (byte) 0x80;
    public static final int WATERDEPTH_TYPE_WATER = (byte) 0x00;
    public static final int WATERDEPTH_TYPE_OCEAN = (byte) 0x40;

    private CachedChunkData() {}

    public static final class Builder {
        private final byte[] waterDepth = new byte[16 * 16];

        protected final BlockState[] surfaceBlocks = new BlockState[16 * 16];

        protected final Map<String, Object> custom = new HashMap<>();

        public Builder() {
            Arrays.fill(this.waterDepth, (byte) WATERDEPTH_DEFAULT);
        }

        public Builder updateWaterDepth(int x, int z, int depth) {
            depth = clamp(depth + 32, 0, 0x3F) | WATERDEPTH_TYPE_WATER;
            if (depth > this.waterDepth[x * 16 + z]) {
                this.waterDepth[x * 16 + z] = (byte) depth;
            }
            return this;
        }

        public Builder updateOceanDepth(int x, int z, int depth) {
            depth = clamp(depth + 32, 0, 0x3F) | WATERDEPTH_TYPE_OCEAN;
            if (depth > this.waterDepth[x * 16 + z]) {
                this.waterDepth[x * 16 + z] = (byte) depth;
            }
            return this;
        }

        public void putCustom(String key, Object value) {
            this.custom.put(key, value);
        }

        @SuppressWarnings("unchecked")
        public <T> T getCustom(String key, T fallback) {
            return (T) this.custom.getOrDefault(key, fallback);
        }

        public byte[] waterDepth() {
            return this.waterDepth;
        }

        public BlockState[] surfaceBlocks() {
            return this.surfaceBlocks;
        }
    }
}
