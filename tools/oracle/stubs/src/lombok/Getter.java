package lombok;
import java.lang.annotation.*;
@Retention(RetentionPolicy.SOURCE)
public @interface Getter {
    com.fasterxml.jackson.annotation.JsonGetter[] onMethod_() default {};
}
