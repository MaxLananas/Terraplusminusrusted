package com.fasterxml.jackson.annotation;
import java.lang.annotation.*;
@Retention(RetentionPolicy.SOURCE)
public @interface JsonProperty {
    String value() default "";
    boolean required() default false;
}
