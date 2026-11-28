package lombok;

import java.lang.annotation.Retention;
import java.lang.annotation.RetentionPolicy;

@Retention(RetentionPolicy.SOURCE)
public @interface NoArgsConstructor {
    AccessLevel access() default AccessLevel.PUBLIC;
}
