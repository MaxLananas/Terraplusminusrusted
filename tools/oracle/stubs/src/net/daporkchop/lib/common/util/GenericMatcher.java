package net.daporkchop.lib.common.util;

import java.lang.reflect.Array;
import java.lang.reflect.GenericArrayType;
import java.lang.reflect.ParameterizedType;
import java.lang.reflect.Type;
import java.lang.reflect.TypeVariable;
import java.util.ArrayList;
import java.util.List;

/**
 * Generic-parameter lookup copied from PorkLib commit 836334a9655fe9c5fae088db74a7830af108a2bd
 * without its cache (PArrays.linearSearch inlined). The mapper parsers use it to find their class.
 */
public final class GenericMatcher {
    private GenericMatcher() {}

    public static <C> Class<?> find(Class<? extends C> thisClass, Class<C> superClass, String name) {
        List<Class<?>> hierarchy = getHierarchy(thisClass, superClass);
        if (hierarchy == null) {
            throw new IllegalArgumentException(String.format("%s does not inherit from %s!", thisClass, superClass));
        }
        return lookup(hierarchy, name);
    }

    @SuppressWarnings("unchecked")
    public static <T, C> Class<T> uncheckedFind(Class<? extends C> thisClass, Class<C> superClass, String name) {
        return (Class<T>) find(thisClass, superClass, name);
    }

    private static List<Class<?>> getHierarchy(Class<?> current, Class<?> superClass) {
        List<Class<?>> list = null;
        if (current == superClass) {
            list = new ArrayList<>();
        } else if (current.getSuperclass() != null) {
            list = getHierarchy(current.getSuperclass(), superClass);
        }
        if (list == null && superClass.isInterface()) {
            for (Class<?> interfaz : current.getInterfaces()) {
                if ((list = getHierarchy(interfaz, superClass)) != null) {
                    break;
                }
            }
        }
        if (list != null) {
            list.add(current);
        }
        return list;
    }

    private static Class<?> lookup(List<Class<?>> hierarchy, String name) {
        for (int i = 0; i < hierarchy.size() - 1; i++) {
            Class<?> clazz = hierarchy.get(i);
            Class<?> next = hierarchy.get(i + 1);

            int index = -1;
            Type[] genericTypes = clazz.getTypeParameters();
            for (int j = genericTypes.length - 1; j >= 0; j--) {
                if (genericTypes[j].getTypeName().equals(name)) {
                    index = j;
                    break;
                }
            }
            if (index == -1) {
                throw new IllegalArgumentException(String.format("Unknown generic name: \"%s\"", name));
            }

            Type genericSuperType;
            if (clazz.isInterface()) {
                Class<?>[] interfaces = next.getInterfaces();
                int position = -1;
                for (int j = 0; j < interfaces.length; j++) {
                    if (interfaces[j] == clazz) {
                        position = j;
                        break;
                    }
                }
                genericSuperType = next.getGenericInterfaces()[position];
            } else {
                genericSuperType = next.getGenericSuperclass();
            }
            if (!(genericSuperType instanceof ParameterizedType)) {
                return Object.class;
            }

            Type actualTypeParam = ((ParameterizedType) genericSuperType).getActualTypeArguments()[index];
            if (actualTypeParam instanceof ParameterizedType) {
                actualTypeParam = ((ParameterizedType) actualTypeParam).getRawType();
            }
            if (actualTypeParam instanceof Class) {
                return (Class<?>) actualTypeParam;
            }
            if (actualTypeParam instanceof GenericArrayType) {
                Type componentType = ((GenericArrayType) actualTypeParam).getGenericComponentType();
                if (componentType instanceof ParameterizedType) {
                    componentType = ((ParameterizedType) componentType).getRawType();
                }
                if (componentType instanceof Class) {
                    return Array.newInstance((Class<?>) componentType, 0).getClass();
                }
            }
            if (actualTypeParam instanceof TypeVariable) {
                TypeVariable<?> v = (TypeVariable<?>) actualTypeParam;
                if (!(v.getGenericDeclaration() instanceof Class)) {
                    return Object.class;
                }
                name = v.getName();
            }
        }
        return null;
    }
}
