#!/usr/bin/env python3
"""Writes synthetic tree-cover tiles around the repository's terrarium fixture (western Grand
Canyon), in the layout of upstream's treecover2000 dataset (`13/<x>/<z>.tiff`, 256x256, percent).

The values are invented, not real tree cover: 0 percent over the west 96 columns, then a
west-to-east ramp reaching 100 percent at the east edge of the second tile column (where the
terrarium fixture ends) and staying there, with a band of GDAL no-data (255) rows in the tile row south of
the terrarium fixture, so tests see bare, sparse, dense and missing cover. Encoded as
single-channel uint8 with LZW and horizontal prediction, which the decoder must handle.

    PYTHONPATH=/tmp/pyref python3 fixtures/treecover/make-fixture.py
"""

import math
import os

import numpy as np
import tifffile

HERE = os.path.dirname(os.path.abspath(__file__))
ZOOM = 13
CENTER = (-113.247, 35.995)


def tile_of(lon, lat):
    n = 1 << ZOOM
    x = (lon + 180.0) / 360.0 * n
    y = (1.0 - math.asinh(math.tan(math.radians(lat))) / math.pi) / 2.0 * n
    return int(x), int(y)


def main():
    cx, cz = tile_of(*CENTER)
    written = []
    for tx in (cx - 1, cx, cx + 1):
        for tz in (cz - 1, cz, cz + 1):
            column = np.arange(256, dtype=np.float64) + (tx - (cx - 1)) * 256
            ramp = np.round(np.clip((column - 96) / (511 - 96), 0, 1) * 100).astype(np.uint8)
            data = np.tile(ramp, (256, 1))
            if tz == cz + 1:
                data[120:136, :] = 255
            directory = os.path.join(HERE, str(ZOOM), str(tx))
            os.makedirs(directory, exist_ok=True)
            path = os.path.join(directory, f"{tz}.tiff")
            tifffile.imwrite(path, data, photometric="minisblack", compression="lzw", predictor=True,
                             extratags=[(42113, "s", 0, "255", True)])
            written.append(path)
    print("\n".join(os.path.relpath(path, HERE) for path in written))


if __name__ == "__main__":
    main()
