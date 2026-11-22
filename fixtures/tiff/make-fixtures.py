#!/usr/bin/env python3
"""Writes the TIFF decoder fixtures with tifffile + imagecodecs, an independent implementation.

Each case is a TIFF file and the raster tifffile reads back from it, converted to float32 as
Commons Imaging's getRasterData does, stored as gzipped little-endian f32 (`<name>.f32.gz`).
`cases.tsv` lists name, width, height and the GDAL no-data text ("-" when absent).

Run with tifffile, imagecodecs and numpy importable, for example:
    pip install --target /tmp/pyref tifffile imagecodecs numpy
    PYTHONPATH=/tmp/pyref python3 fixtures/tiff/make-fixtures.py
Versions used are recorded in PROVENANCE.
"""

import gzip
import hashlib
import os
import sys

import imagecodecs
import numpy as np
import tifffile

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "cases")


def raster(dtype, shape, seed):
    rng = np.random.default_rng(seed)
    height, width = shape
    y, x = np.mgrid[0:height, 0:width]
    dtype = np.dtype(dtype)
    if dtype.kind == "f":
        values = np.sin(x * 0.37) * 900.0 + np.cos(y * 0.21) * 350.0 + rng.normal(0, 3, shape)
        values[0, 0] = -9999.0
        values[1, 2] = 1e-30
        return values.astype(dtype)
    info = np.iinfo(dtype)
    span = float(info.max) - float(info.min)
    smooth = (np.sin(x * 0.3) + np.cos(y * 0.17) + 2.0) / 4.0
    values = info.min + smooth * span * 0.98 + rng.integers(0, 3, shape)
    values = np.clip(values, info.min, info.max).astype(dtype)
    values[0, 0] = info.min
    values[0, 1] = info.max
    return values


# name, dtype, (height, width), write options
CASES = [
    ("u8_none_strips", "u1", (23, 37), dict(rowsperstrip=5)),
    ("i8_none", "i1", (23, 37), dict()),
    ("u16_lzw_pred2", "u2", (23, 37), dict(compression="lzw", predictor=True, rowsperstrip=7)),
    ("i16_zlib_pred2_be", ">i2", (23, 37), dict(compression="zlib", predictor=True)),
    ("u32_packbits", "u4", (23, 37), dict(compression="packbits", rowsperstrip=4)),
    ("i32_zlib_tiled", "i4", (23, 37), dict(compression="zlib", tile=(16, 16))),
    ("f32_none_be", ">f4", (23, 37), dict()),
    ("f32_lzw_pred3_tiled", "f4", (40, 33), dict(compression="lzw", predictor=3, tile=(16, 32))),
    ("f32_zlib_pred3_be", ">f4", (23, 37), dict(compression="zlib", predictor=3, rowsperstrip=6)),
    ("f64_zlib", "f8", (23, 37), dict(compression="zlib")),
    ("f64_deflate32946_pred3", "f8", (17, 19), dict(compression="deflate", predictor=3)),
    ("u8_lzw_pred2_tiled_be", ">u1", (48, 48), dict(compression="lzw", predictor=True, tile=(32, 16))),
    ("u16_miniswhite", "u2", (12, 9), dict(photometric="miniswhite")),
    ("f32_nodata", "f4", (23, 37), dict(compression="zlib", extratags=[(42113, "s", 0, "-9999", True)])),
    # Low-entropy noise over 256x256 fills the LZW table, forcing 12-bit codes and clear codes.
    ("u8_lzw_large", "u1", (256, 256), dict(compression="lzw")),
]


def main():
    os.makedirs(OUT, exist_ok=True)
    rows = []
    for name, dtype, shape, options in CASES:
        if name == "u8_lzw_large":
            data = np.random.default_rng(7).integers(0, 16, shape).astype(np.uint8)
        else:
            data = raster(dtype, shape, len(rows) + 1)
        path = os.path.join(OUT, name + ".tif")
        photometric = options.pop("photometric", "minisblack")
        byteorder = ">" if dtype.startswith(">") else "<"
        tifffile.imwrite(path, data, photometric=photometric, byteorder=byteorder, **options)
        # Read back with tifffile, not from `data`: the reference is the independent decoder.
        decoded = tifffile.imread(path)
        assert np.array_equal(decoded, data), name
        expected = decoded.astype(np.float32).astype("<f4").tobytes()
        with gzip.GzipFile(os.path.join(OUT, name + ".f32.gz"), "wb", mtime=0) as handle:
            handle.write(expected)
        nodata = "-9999" if name == "f32_nodata" else "-"
        rows.append((name, shape[1], shape[0], nodata))

    # A first directory the decoder must skip (RGB), then the grayscale one.
    rgb = np.zeros((8, 8, 3), np.uint8)
    gray = raster("u2", (8, 8), 99)
    path = os.path.join(OUT, "rgb_then_gray.tif")
    with tifffile.TiffWriter(path) as writer:
        writer.write(rgb, photometric="rgb")
        writer.write(gray, photometric="minisblack")
    decoded = tifffile.imread(path, key=1)
    with gzip.GzipFile(os.path.join(OUT, "rgb_then_gray.f32.gz"), "wb", mtime=0) as handle:
        handle.write(decoded.astype(np.float32).astype("<f4").tobytes())
    rows.append(("rgb_then_gray", 8, 8, "-"))

    with open(os.path.join(HERE, "cases.tsv"), "w") as handle:
        handle.write("name\twidth\theight\tnodata\n")
        for row in rows:
            handle.write("\t".join(map(str, row)) + "\n")

    digests = []
    for entry in sorted(os.listdir(OUT)):
        with open(os.path.join(OUT, entry), "rb") as handle:
            digests.append(f"{hashlib.sha256(handle.read()).hexdigest()}  cases/{entry}")
    with open(os.path.join(HERE, "PROVENANCE"), "w") as handle:
        handle.write("generator=fixtures/tiff/make-fixtures.py\n")
        handle.write(f"python={sys.version.split()[0]}\n")
        handle.write(f"tifffile={tifffile.__version__}\nimagecodecs={imagecodecs.__version__}\nnumpy={np.__version__}\n")
        handle.write("# file digests (not golden-checked by scripts/check-fixtures.sh)\n")
        handle.write("\n".join("file " + line for line in digests) + "\n")


if __name__ == "__main__":
    main()
