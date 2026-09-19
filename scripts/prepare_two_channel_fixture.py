"""Generate a small two-channel Zarr v3 OME-NGFF fixture.

Deliberately synthetic: each channel carries a different analytic gradient so a test can tell the
channels apart by value, which is what makes a residency key collision visible rather than merely
present.
"""
import json, os, pathlib, struct, sys

root = pathlib.Path(sys.argv[1])
if root.exists():
    raise SystemExit(f"{root} already exists")

# c, z, y, x
LEVELS = [
    {"path": "0", "shape": [2, 8, 16, 16], "chunks": [1, 4, 8, 8], "scale": [1.0, 0.5, 0.25, 0.25]},
    {"path": "1", "shape": [2, 8, 8, 8], "chunks": [1, 4, 8, 8], "scale": [1.0, 0.5, 0.5, 0.5]},
]

def value(channel, z, y, x, shape):
    # Channel 0 ramps along x, channel 1 ramps along y, so the two are never equal except at the
    # origin and a swapped page is visible as a different scalar.
    if channel == 0:
        return 1000 + 200 * x
    return 1000 + 200 * y

root.mkdir(parents=True)
(root / "zarr.json").write_text(json.dumps({
    "zarr_format": 3,
    "node_type": "group",
    "attributes": {
        "multiscales": [{
            "version": "0.4",
            "name": "synthetic two-channel gradient",
            "axes": [
                {"name": "c", "type": "channel"},
                {"name": "z", "type": "space", "unit": "micrometer"},
                {"name": "y", "type": "space", "unit": "micrometer"},
                {"name": "x", "type": "space", "unit": "micrometer"},
            ],
            "datasets": [
                {"path": level["path"],
                 "coordinateTransformations": [{"type": "scale", "scale": level["scale"]}]}
                for level in LEVELS
            ],
        }],
        "omero": {"channels": [
            {"active": True, "color": "FF0000",
             "window": {"start": 0, "end": 4000, "min": 0, "max": 65535}},
            {"active": True, "color": "00FF00",
             "window": {"start": 0, "end": 4000, "min": 0, "max": 65535}},
        ]},
    },
}, indent=2) + "\n")

for level in LEVELS:
    shape, chunks, path = level["shape"], level["chunks"], level["path"]
    array = root / path
    array.mkdir()
    (array / "zarr.json").write_text(json.dumps({
        "zarr_format": 3,
        "node_type": "array",
        "shape": shape,
        "data_type": "uint16",
        "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": chunks}},
        "chunk_key_encoding": {"name": "default", "configuration": {"separator": "/"}},
        "fill_value": 0,
        "codecs": [{"name": "bytes", "configuration": {"endian": "little"}}],
        "attributes": {},
    }, indent=2) + "\n")
    counts = [-(-shape[axis] // chunks[axis]) for axis in range(4)]
    for cc in range(counts[0]):
        for cz in range(counts[1]):
            for cy in range(counts[2]):
                for cx in range(counts[3]):
                    directory = array / "c" / str(cc) / str(cz) / str(cy)
                    directory.mkdir(parents=True, exist_ok=True)
                    words = []
                    for z in range(chunks[1]):
                        for y in range(chunks[2]):
                            for x in range(chunks[3]):
                                words.append(value(
                                    cc,
                                    cz * chunks[1] + z,
                                    cy * chunks[2] + y,
                                    cx * chunks[3] + x,
                                    shape,
                                ) & 0xFFFF)
                    (directory / str(cx)).write_bytes(
                        struct.pack(f"<{len(words)}H", *words))
print("wrote", root)
