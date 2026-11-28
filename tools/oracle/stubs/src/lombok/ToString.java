package lombok;
import java.lang.annotation.*;
@Retention(RetentionPolicy.SOURCE)
public @interface ToString {
    @Retention(RetentionPolicy.SOURCE)
    @interface Exclude {}
}
