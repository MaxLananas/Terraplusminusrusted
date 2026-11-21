package com.fasterxml.jackson.databind.annotation;
import java.lang.annotation.*;
@Retention(RetentionPolicy.SOURCE)
public @interface JsonDeserialize {
    Class<?> using() default Void.class;
    Class<?> as() default Void.class;
}
