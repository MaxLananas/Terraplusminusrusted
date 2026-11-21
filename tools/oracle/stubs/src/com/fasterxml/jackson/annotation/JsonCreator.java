package com.fasterxml.jackson.annotation;
import java.lang.annotation.*;
@Retention(RetentionPolicy.SOURCE)
public @interface JsonCreator {
    Mode mode() default Mode.DEFAULT;
    enum Mode { DEFAULT, DELEGATING, PROPERTIES, DISABLED }
}
