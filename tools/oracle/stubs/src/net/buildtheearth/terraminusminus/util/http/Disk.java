package net.buildtheearth.terraminusminus.util.http;

import java.nio.file.Path;

/** Only referenced by OSMMapper.load, which the oracle does not call: it reads its mapping explicitly. */
public final class Disk {
    private Disk() {}

    public static Path configFile(String name) {
        throw new UnsupportedOperationException("the oracle passes its osm.json5 explicitly");
    }
}
