package net.daporkchop.lib.binary.oio.reader;

import java.io.FileInputStream;
import java.io.FileNotFoundException;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;

/** Only referenced by OSMMapper.load, which the oracle does not call. */
public final class UTF8FileReader extends InputStreamReader {
    public UTF8FileReader(String path) throws FileNotFoundException {
        super(new FileInputStream(path), StandardCharsets.UTF_8);
    }
}
