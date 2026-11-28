#!/usr/bin/env python3
"""Writes the OpenStreetMap oracle scenes: line-delimited GeoJSON in TerraMinusMinus' tile format.

Features are authored in block coordinates of a linear test projection
    x = (lon - lon0) * scale,  z = (lat0 - lat) * scale
and written in degrees, so that the oracle (upstream mapper and rasterisers) and the Rust port
both read the same tile and draw the same chunks. The scenes cover what osm.json5 maps: every
road class with lane and layer tags (numeric, fractional and unparsable), tunnels, footways
including an exactly vertical one, rivers, streams, building outlines and areas, water areas with
islands, coastlines on both sides of the Caspian box edge, multi-geometries whose parts share an
id, geometry collections, feature collections, points, references, out-of-projection elements,
chunk seams and negative block and geographic coordinates.

The last column of scenes.tsv names the mapping: "default" for TerraMinusMinus' osm.json5, or a
file relative to this directory.

Usage: make-scenes.py  (writes next to this script)
"""
import json
import pathlib

ROOT = pathlib.Path(__file__).resolve().parent

SCENES = {
    # name: (lon0, lat0, blocks per degree, chunk range x, chunk range z)
    "city": (2.35, 48.85, 100000.0, (-4, 3), (-4, 3)),
    "caspian_edge": (45.637, 40.0, 100000.0, (-3, 2), (-3, 2)),
    "southern_coast": (-58.38, -34.6, 80000.0, (-3, 2), (-3, 2)),
    "custom_mapping": (10.0, 0.05, 100000.0, (-4, 3), (-4, 3)),
}
# Scenes drawn with another mapping than the default osm.json5.
MAPPINGS = {"custom_mapping": "mappings/exhaustive.json5"}


class Scene:
    def __init__(self, lon0, lat0, scale):
        self.lon0, self.lat0, self.scale = lon0, lat0, scale

    def point(self, x, z):
        return [self.lon0 + x / self.scale, self.lat0 - z / self.scale]

    def line(self, *points):
        return [self.point(x, z) for x, z in points]

    def ring(self, *points):
        ring = self.line(*points)
        return ring + [ring[0]]


def feature(id_, tags, geometry):
    return {"type": "Feature", "geometry": geometry, "properties": tags, "id": id_}


def line_string(coordinates):
    return {"type": "LineString", "coordinates": coordinates}


def polygon(*rings):
    return {"type": "Polygon", "coordinates": list(rings)}


def city(s):
    yield feature("way/100", {"highway": "motorway", "lanes": "3"}, line_string(s.line((-70, -50), (70, 30))))
    yield feature("way/101", {"highway": "residential", "layer": "1"}, line_string(s.line((-60, 40), (-10, 45), (20, 60))))
    yield feature("way/102", {"highway": "residential", "layer": "-1"}, line_string(s.line((-64, 10), (64, -14))))
    yield feature("way/103", {"highway": "footway"}, line_string(s.line((-30, -60), (-25, 10), (5, 15))))
    yield feature("way/104", {"highway": "footway"}, line_string(s.line((10, -60), (10, -20))))  # exactly vertical
    yield feature("way/105", {"highway": "motorway", "tunnel": "yes"}, line_string(s.line((-64, 0), (64, 2))))
    yield feature("way/106", {"highway": "residential", "tunnel": "no", "lanes": "2.5"}, line_string(s.line((0, -64), (30, 64))))
    yield feature("way/107", {"waterway": "river"}, line_string(s.line((-64, -20), (0, -30), (64, -10))))
    yield feature("way/108", {"waterway": "stream"}, line_string(s.line((40, -64), (50, 64))))
    yield feature("way/109", {"building": "yes"}, polygon(s.ring((20, 20), (40, 20), (40, 35), (20, 35)),
                                                          s.ring((26, 25), (32, 25), (32, 30), (26, 30))))
    yield feature("way/110", {"building": "yes"}, line_string(s.line((-50, -40), (-40, -35), (-42, -30))))
    yield feature("way/111", {"natural": "water"}, polygon(s.ring((-50, -10), (-5, -12), (-8, 30), (-45, 25)),
                                                           s.ring((-30, 0), (-20, 0), (-20, 10), (-30, 10))))
    yield feature("relation/112", {"building": "apartments", "layer": "1.5"}, {
        "type": "MultiPolygon",
        "coordinates": [[s.ring((-60, -60), (-52, -60), (-52, -52), (-60, -52))],
                        [s.ring((-50, -62), (-44, -62), (-44, -55), (-50, -55))]],
    })
    yield {"type": "FeatureCollection", "features": [
        feature("way/113", {"highway": "primary", "lanes": "abc"}, line_string(s.line((-64, 60), (64, 50)))),
        feature("way/114", {"highway": "motorway_link", "lanes": "1"}, line_string(s.line((55, -64), (58, -30), (64, -28)))),
        feature("way/115", {"highway": "secondary", "lanes": "4"}, line_string(s.line((-20, 64), (-18, 20), (-64, -5)))),
    ]}
    yield feature("way/116", {"highway": "service"}, {"type": "GeometryCollection", "geometries": [
        line_string(s.line((30, -50), (45, -45))),
        {"type": "Point", "coordinates": s.point(0, 0)},
        polygon(s.ring((-15, 45), (-5, 45), (-5, 55), (-15, 55))),
    ]})
    yield feature("node/117", {"highway": "crossing"}, {"type": "Point", "coordinates": s.point(5, 5)})
    yield feature("way/118", {"waterway": "riverbank"}, polygon(s.ring((48, 40), (63, 40), (63, 63), (48, 63))))
    yield feature("way/119", {"highway": "trunk"}, {"type": "MultiLineString", "coordinates": [
        s.line((-64, -45), (-20, -48)), s.line((-30, -47), (20, -52))]})
    yield feature("way/120", {"water": "pond"}, polygon(s.ring((60, -40), (67, -40), (67, -33), (60, -33))))
    # A point outside the projection skips the whole element.
    out = s.line((-10, -64), (-12, -50))
    out.append([190.0, 0.0])
    yield feature("way/121", {"highway": "residential"}, line_string(out))


def caspian_edge(s):
    yield {"type": "Reference", "location": "shared/caspian-coast.json"}
    yield feature("way/200", {"natural": "coastline"}, polygon(s.ring((-40, 10), (-5, 12), (-6, 40), (-38, 38))))
    yield feature("way/201", {"waterway": "canal"}, line_string(s.line((-48, -40), (30, -20))))


def caspian_shared(s):
    # Touches the box edge exactly (max longitude == 45.637): Bounds2d.intersects is inclusive.
    yield feature("way/210", {"natural": "coastline"}, polygon(s.ring((-30, -30), (0, -28), (0, -10), (-28, -12))))
    yield feature("way/211", {"natural": "coastline"}, polygon(s.ring((5, -40), (40, -38), (38, 5), (8, 2))))


def southern_coast(s):
    yield feature("way/300", {"natural": "coastline"}, polygon(s.ring((-48, -48), (30, -45), (32, 0), (-46, 5)),
                                                               s.ring((-10, -20), (0, -20), (0, -10), (-10, -10))))
    yield feature("way/301", {"highway": "tertiary", "lanes": "3", "layer": "2"}, line_string(s.line((-48, 20), (31, -47))))
    yield feature("way/302", {"highway": "living_street"}, line_string(s.line((-48, 30), (31, 25))))


def custom_mapping(s):
    yield feature("way/400", {"waterway": "ditch"}, line_string(s.line((-40, -40), (30, -35))))
    yield feature("way/401", {"width": "80"}, line_string(s.line((-64, -64), (64, -60))))  # radius saturates at 60
    yield feature("way/402", {"width": "12d", "layer": "3"}, line_string(s.line((-50, 40), (50, 30))))
    yield feature("way/403", {"kind": "shallow", "width": "1e1"}, line_string(s.line((-64, 4), (-30, 8))))
    yield feature("way/404", {"kind": "deep"}, line_string(s.line((2, 2), (8, 8))))  # inside the excluded box
    yield feature("way/405", {"kind": "deep", "width": " 7 "}, line_string(s.line((20, 2), (40, 10))))
    yield feature("way/406", {"kind": "puddle"}, line_string(s.line((-10, -64), (-5, 64))))  # no rule matches
    yield feature("way/410", {"layer": "4"}, polygon(s.ring((-60, -60), (-20, -60), (-20, -20), (-60, -20))))
    yield feature("way/411", {"natural": "bay"}, polygon(s.ring((10, -60), (60, -60), (60, -15), (10, -15)),
                                                         s.ring((25, -45), (45, -45), (45, -30), (25, -30))))
    yield feature("way/412", {"landuse": "basin"}, polygon(
        s.ring((-60, 20), (-20, 20), (-20, 60), (-30, 60), (-30, 30), (-50, 30), (-50, 60), (-60, 60))))
    yield feature("way/413", {"place": "square", "level": "5"}, polygon(s.ring((0, 40), (14, 40), (14, 54), (0, 54))))
    yield feature("relation/414", {"landuse": "basin"}, {
        "type": "MultiPolygon",
        "coordinates": [[s.ring((40, 40), (50, 40), (50, 50), (40, 50))], [s.ring((44, 52), (60, 52), (60, 62), (44, 62))]],
    })
    yield feature("way/415", {"amenity": "parking"}, polygon(s.ring((28, -36), (36, -36), (36, -26), (28, -26))))  # intersects only


def write(path, objects):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("".join(json.dumps(o, separators=(",", ":")) + "\n" for o in objects))


def main():
    rows = ["# name\ttile file\tlon0\tlat0\tblocks per degree\tchunk min x\tchunk max x\tchunk min z\tchunk max z\tmapping"]
    builders = {"city": [("tile.json", city)],
                "caspian_edge": [("tile.json", caspian_edge), ("shared/caspian-coast.json", caspian_shared)],
                "southern_coast": [("tile.json", southern_coast)],
                "custom_mapping": [("tile.json", custom_mapping)]}
    for name, (lon0, lat0, scale, (x0, x1), (z0, z1)) in SCENES.items():
        scene = Scene(lon0, lat0, scale)
        for relative, build in builders[name]:
            write(ROOT / "scenes" / name / relative, build(scene))
        mapping = MAPPINGS.get(name, "default")
        rows.append(f"{name}\tscenes/{name}/tile.json\t{lon0!r}\t{lat0!r}\t{scale!r}\t{x0}\t{x1}\t{z0}\t{z1}\t{mapping}")
    (ROOT / "scenes.tsv").write_text("\n".join(rows) + "\n")


if __name__ == "__main__":
    main()
