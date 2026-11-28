package net.buildtheearth.terraminusminus;

/** Replaces the log4j logger with standard error; the oracle only logs parser warnings. */
public final class TerraMinusMinus {
    private TerraMinusMinus() {}

    public static final Logger LOGGER = new Logger();

    public static final class Logger {
        private Logger() {}

        public void warn(String format, Object... arguments) {
            String message = format;
            for (Object argument : arguments) {
                message = message.replaceFirst("\\{}", java.util.regex.Matcher.quoteReplacement(String.valueOf(argument)));
            }
            System.err.println("WARN " + message);
        }
    }
}
