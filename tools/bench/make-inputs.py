"""Writes the benchmark's synthetic GeoTIFFs: a smooth heightfield as 1024x1024 float32 Deflate
and 512x512 signed int16 Deflate with predictor 2, strips of 16 rows, little-endian."""
import math
import random
import struct
import sys
import zlib


def tiff(path, n, fmt, sample_format, bits, predictor, rows_per_strip=16):
    random.seed(7)
    values = []
    for y in range(n):
        for x in range(n):
            h = 800 + 300 * math.sin(x / 97.0) * math.cos(y / 71.0) + 40 * math.sin(x / 9.0 + y / 13.0) + random.random() * 2
            values.append(h if fmt == "f" else int(h))
    strips = []
    for start in range(0, n, rows_per_strip):
        raw = bytearray()
        for y in range(start, min(n, start + rows_per_strip)):
            row = values[y * n:(y + 1) * n]
            if predictor == 2:
                row = [row[0]] + [((row[i] - row[i - 1] + 32768) % 65536) - 32768 for i in range(1, n)]
            raw += struct.pack("<%d%s" % (n, fmt), *row)
        strips.append(zlib.compress(bytes(raw), 6))
    data = bytearray(b"II*\0\0\0\0\0")
    offsets = []
    for strip in strips:
        offsets.append(len(data))
        data += strip
        if len(data) % 2:
            data += b"\0"
    offsets_at = len(data)
    data += struct.pack("<%dI" % len(offsets), *offsets)
    counts_at = len(data)
    data += struct.pack("<%dI" % len(strips), *[len(s) for s in strips])
    tags = [(256, 4, 1, n), (257, 4, 1, n), (258, 3, 1, bits), (259, 3, 1, 8), (262, 3, 1, 1), (273, 4, len(strips), offsets_at),
            (277, 3, 1, 1), (278, 4, 1, rows_per_strip), (279, 4, len(strips), counts_at), (317, 3, 1, predictor), (339, 3, 1, sample_format)]
    data[4:8] = struct.pack("<I", len(data))
    data += struct.pack("<H", len(tags))
    for tag, kind, count, value in tags:
        data += struct.pack("<HHIHH", tag, kind, count, value, 0) if kind == 3 and count == 1 else struct.pack("<HHII", tag, kind, count, value)
    data += struct.pack("<I", 0)
    with open(path, "wb") as out:
        out.write(data)


directory = sys.argv[1]
tiff(f"{directory}/dem_f32_deflate_1024.tif", 1024, "f", 3, 32, 1)
tiff(f"{directory}/dem_i16_deflate_pred2_512.tif", 512, "h", 2, 16, 2)
