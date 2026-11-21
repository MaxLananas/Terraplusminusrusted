package com.fasterxml.jackson.databind.annotation;
import java.lang.annotation.*;
@Retention(RetentionPolicy.SOURCE)
public @interface JsonSerialize {
    Class<?> using() default Void.class;
}
