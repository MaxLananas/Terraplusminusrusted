package it.unimi.dsi.fastutil.objects;

import java.util.HashMap;

/** Only used as a plain mutable map of parser types; iteration order is never relied on. */
public class Object2ObjectOpenHashMap<K, V> extends HashMap<K, V> {
    public Object2ObjectOpenHashMap() {}
}
