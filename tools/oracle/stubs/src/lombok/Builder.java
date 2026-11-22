package lombok;

import java.lang.annotation.Retention;
import java.lang.annotation.RetentionPolicy;

@Retention(RetentionPolicy.SOURCE)
public @interface Builder {
    @Retention(RetentionPolicy.SOURCE)
    @interface Default {}
}
