package net.buildtheearth.terraminusminus.config;

public abstract class TypedSerializer<T> {
    protected abstract java.util.Map<Class<? extends T>, String> registry();
}
