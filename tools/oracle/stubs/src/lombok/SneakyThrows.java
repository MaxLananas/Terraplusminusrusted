package lombok;
import java.lang.annotation.*;
@Retention(RetentionPolicy.SOURCE)
public @interface SneakyThrows {
    Class<? extends Throwable>[] value() default {};
}
