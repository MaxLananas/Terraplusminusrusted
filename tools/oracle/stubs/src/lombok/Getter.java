package lombok;
import java.lang.annotation.*;
@Retention(RetentionPolicy.SOURCE)
public @interface Getter {
    AccessLevel value() default AccessLevel.PUBLIC;

    com.fasterxml.jackson.annotation.JsonGetter[] onMethod_() default {};
}
