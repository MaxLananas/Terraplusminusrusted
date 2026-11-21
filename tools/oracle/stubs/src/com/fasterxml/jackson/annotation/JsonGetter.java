package com.fasterxml.jackson.annotation;
import java.lang.annotation.*;
@Retention(RetentionPolicy.SOURCE)
public @interface JsonGetter {
    String value() default "";
}
