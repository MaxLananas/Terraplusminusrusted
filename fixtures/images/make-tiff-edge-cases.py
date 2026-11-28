#!/usr/bin/env python3
"""Hand-built TIFF edge cases for the image oracle (layouts ImageMagick will not write).

Writes uncompressed classic TIFFs into ./input. Deterministic: rerunning produces identical bytes.
The other inputs come from ImageMagick (see PROVENANCE in ../golden).
"""
import os
import struct

HERE = os.path.dirname(os.path.abspath(__file__))


def tiff(name, width, height, bits, photometric, data, extra=(), little=True, rows_per_strip=None, big=False):
    """One uncompressed, strip-organised image directory. `extra` holds (tag, type, values) with
    type 2 (ASCII, values a str), 3 (SHORT) or 4 (LONG)."""
    e = "<" if little else ">"
    rows_per_strip = rows_per_strip or height
    row_bytes = (sum(bits) * width + 7) // 8
    strips = [data[y * row_bytes:min(height, y + rows_per_strip) * row_bytes] for y in range(0, height, rows_per_strip)]
    offset_type = 16 if big else 4
    entries = [(256, 3, [width]), (257, 3, [height]), (258, 3, list(bits)), (259, 3, [1]), (262, 3, [photometric]),
               (273, offset_type, [0] * len(strips)), (277, 3, [len(bits)]), (278, 3, [rows_per_strip]),
               (279, 4, [len(strip) for strip in strips])]
    entries = sorted(entries + list(extra), key=lambda entry: entry[0])
    codes = {3: "H", 4: "I", 16: "Q"}
    field = 8 if big else 4
    header = 16 if big else 8
    directory = (8 if big else 2) + (20 if big else 12) * len(entries) + field

    def blob(kind, values):
        if kind == 2:
            return values.encode("ascii") + b"\0"
        return struct.pack(e + codes[kind] * len(values), *values)

    # Lay out out-of-line values after the directory, then the strips.
    heap, where = bytearray(), {}
    for tag, kind, values in entries:
        size = len(blob(kind, values))
        if size > field:
            where[tag] = header + directory + len(heap)
            heap += b"\0" * (size + size % 2)
    data_start = header + directory + len(heap)
    offsets, payload = [], bytearray()
    for strip in strips:
        offsets.append(data_start + len(payload))
        payload += strip
    entries = [(tag, kind, offsets if tag == 273 else values) for tag, kind, values in entries]
    out = bytearray(b"II" if little else b"MM")
    out += struct.pack(e + "HHHQ", 43, 8, 0, header) if big else struct.pack(e + "HI", 42, header)
    out += struct.pack(e + ("Q" if big else "H"), len(entries))
    for tag, kind, values in entries:
        value = blob(kind, values)
        count = len(value) if kind == 2 else len(values)
        if tag in where:
            at = where[tag] - header - directory
            heap[at:at + len(value)] = value
            value = struct.pack(e + ("Q" if big else "I"), where[tag])
        out += struct.pack(e + "HH", tag, kind) + struct.pack(e + ("Q" if big else "I"), count) + value.ljust(field, b"\0")
    out += b"\0" * field
    out += heap + payload
    with open(os.path.join(HERE, "input", name), "wb") as handle:
        handle.write(bytes(out))


def pattern(count, seed):
    value = seed
    result = []
    for _ in range(count):
        value = (value * 1103515245 + 12345) & 0x7FFFFFFF
        result.append((value >> 16) & 0xFF)
    return result


W, H = 9, 5
tiff("tif_x_gray8_minwhite.tif", W, H, [8], 0, bytes(pattern(W * H, 1)))
tiff("tif_x_bilevel_minwhite.tif", W, H, [1], 0, bytes(pattern(2 * H, 2)))
tiff("tif_x_ycbcr.tif", W, H, [8, 8, 8], 6, bytes(pattern(3 * W * H, 3)))
tiff("tif_x_rgbx_extra0.tif", W, H, [8, 8, 8, 8], 2, bytes(pattern(4 * W * H, 4)), extra=[(338, 3, [0])])
tiff("tif_x_rgba_noextra.tif", W, H, [8, 8, 8, 8], 2, bytes(pattern(4 * W * H, 5)))
tiff("tif_x_rgba_assoc_be.tif", W, H, [8, 8, 8, 8], 2, bytes(pattern(4 * W * H, 6)), extra=[(338, 3, [1])], little=False)
tiff("tif_x_gray_alpha.tif", W, H, [8, 8], 1, bytes(pattern(2 * W * H, 7)), extra=[(338, 3, [2])])
tiff("tif_x_rgb16_pred.tif", W, H, [16, 16, 16], 2, bytes(pattern(6 * W * H, 8)), extra=[(317, 3, [2])])
tiff("tif_x_gray16_pred_be.tif", W, H, [16], 1, bytes(pattern(2 * W * H, 9)), extra=[(317, 3, [2])], little=False)
tiff("tif_x_palette8.tif", W, H, [8], 3, bytes(pattern(W * H, 10)), extra=[(320, 3, [(i * 2654435761) & 0xFFFF for i in range(768)])])
tiff("tif_x_signed16.tif", W, H, [16], 1, bytes(pattern(2 * W * H, 11)), extra=[(339, 3, [2])])
tiff("tif_x_gray4_odd.tif", W, H, [4], 1, bytes(pattern(5 * H, 12)), rows_per_strip=2)
tiff("tif_x_cmyk_odd_strips.tif", W, H, [8, 8, 8, 8], 5, bytes(pattern(4 * W * H, 13)), rows_per_strip=3)
tiff("tif_x_gray8_sq.tif", 8, 8, [8], 1, bytes(pattern(64, 14)))
tiff("tif_x_gray8_minwhite_sq.tif", 8, 8, [8], 0, bytes(pattern(64, 15)))

# Rasters upstream's ParseTiffDSP (parse_tiff for doubles) accepts: SampleFormat present, signed
# 16/32-bit integers (predictor 1 or 2) or 32/64-bit floats. Square, so the parsers run on them.
R = 8
signed = lambda values, size, e: b"".join(struct.pack(e + ("h" if size == 2 else "i"), v) for v in values)
i16 = [((v * 977) % 65536) - 32768 for v in pattern(R * R, 21)]
tiff("tif_r_i16.tif", R, R, [16], 1, signed(i16, 2, "<"), extra=[(339, 3, [2])])
# Big-endian BigTIFFs keep several strips: Commons Imaging reads a single-valued LONG8 through
# toInt (its first four bytes), which is the high half in big-endian files, so a one-strip file
# would point upstream at offset 0. That case is tif_k_* ("known upstream bug"), checked against
# its several-strip twin instead of the oracle.
tiff("tif_r_i16_be_bigtiff.tif", R, R, [16], 1, signed(i16, 2, ">"), extra=[(339, 3, [2])], little=False, big=True, rows_per_strip=3)
tiff("tif_k_i16_be_bigtiff_one_strip.tif", R, R, [16], 1, signed(i16, 2, ">"), extra=[(339, 3, [2])], little=False, big=True)
# Differencing on sign-extended samples in Java ints: the running sum leaves the 16-bit range.
tiff("tif_r_i16_pred_overflow.tif", R, R, [16], 1, signed([-30000 if i % R else 20000 for i in range(R * R)], 2, "<"), extra=[(317, 3, [2]), (339, 3, [2])])
tiff("tif_r_i32_pred.tif", R, R, [32], 1, signed([(v << 23) - (1 << 30) for v in pattern(R * R, 22)], 4, "<"), extra=[(317, 3, [2]), (339, 3, [2])])
tiff("tif_r_i32_nodata.tif", R, R, [32], 0, signed([(-9999 if v % 5 == 0 else v * 3) for v in pattern(R * R, 23)], 4, ">"), extra=[(339, 3, [2]), (42113, 2, "-9999")], little=False)
floats = [(v - 100) * 12.375 for v in pattern(R * R, 24)]
tiff("tif_r_f32.tif", R, R, [32], 1, struct.pack("<" + "f" * len(floats), *floats), extra=[(339, 3, [3]), (42113, 2, "-1076.625")])
tiff("tif_r_f64_bigtiff_be.tif", R, R, [64], 1, struct.pack(">" + "d" * len(floats), *[v / 7 for v in floats]), extra=[(339, 3, [3])], little=False, big=True, rows_per_strip=4)
# What upstream refuses (unsigned, no SampleFormat): the port reads these as a documented superset.
tiff("tif_r_u16_unsigned.tif", R, R, [16], 1, bytes(pattern(2 * R * R, 25)), extra=[(339, 3, [1])])
