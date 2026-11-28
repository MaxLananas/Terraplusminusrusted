package net.buildtheearth.terraminusminus.config;

public abstract class TypedDeserializer<T> {
    protected abstract java.util.Map<String, Class<? extends T>> registry();
}
