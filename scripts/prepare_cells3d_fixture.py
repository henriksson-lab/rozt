#!/usr/bin/env python3
"""Build the compact, CC0 cells3d OME-Zarr fixture used by newvolim tests.

The source TIFF is the scikit-image archive's ``cells3d`` fluorescence-microscopy volume. This
script deliberately takes a local source path: acquisition is separate from fixture generation,
and its pinned source URL and checksum are recorded in the generated README.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
from pathlib import Path

import numpy as np
import tifffile

SOURCE_URL = (
    "https://gitlab.com/scikit-image/data/-/raw/"
    "2cdc5ce89b334d28f06a58c9f0ca21aa6992a5ba/cells3d.tif"
)


def mean_blocks(values: np.ndarray, factors: tuple[int, int, int]) -> np.ndarray:
    """Downsample Z/Y/X, retaining edge voxels instead of requiring divisible shapes."""
    if any(factor < 1 for factor in factors):
        raise ValueError(f"invalid downsample factors {factors}")
    output_shape = tuple((size + factor - 1) // factor for size, factor in zip(values.shape, factors))
    output = np.empty(output_shape, dtype=np.uint16)
    for output_index in np.ndindex(output_shape):
        source = tuple(
            slice(index * factor, min((index + 1) * factor, size))
            for index, factor, size in zip(output_index, factors, values.shape)
        )
        output[output_index] = np.rint(values[source].mean())
    return output


def propose_pyramid(
    shape: tuple[int, int, int], spacing_us: tuple[float, float, float], max_size: int
) -> list[tuple[int, int, int]]:
    """Return cumulative BigDataViewer-style factors for an anisotropic Z/Y/X pyramid.

    An axis halves only once its current physical voxel size is no more than twice the finest
    axis. This lets coarse XY catch up to thick Z sampling before the Z axis starts reducing.
    """
    if max_size < 1 or any(size < 1 for size in shape) or any(size <= 0 for size in spacing_us):
        raise ValueError("shape, spacing, and max_size must all be positive")
    factors = [1, 1, 1]
    levels = [tuple(factors)]
    while any((size + factor - 1) // factor > max_size for size, factor in zip(shape, factors)):
        current_spacing = [spacing * factor for spacing, factor in zip(spacing_us, factors)]
        finest = min(current_spacing)
        advanced = False
        for axis, (size, spacing) in enumerate(zip(shape, current_spacing)):
            if (size + factors[axis] - 1) // factors[axis] > 1 and spacing / finest <= 2.0:
                factors[axis] *= 2
                advanced = True
        if not advanced:
            raise ValueError("pyramid cannot reduce any remaining axis")
        levels.append(tuple(factors))
    return levels


def write_uncompressed_v3_array(
    directory: Path, values: np.ndarray, *, spacing_us: tuple[float, float, float] | None = None
) -> None:
    """Write exact C-order v3 chunks without relying on a host Zarr implementation."""
    chunks = (8, 32, 32)
    directory.mkdir(parents=True)
    attributes: dict[str, object] = {}
    if spacing_us is not None:
        attributes["spacing_us"] = list(spacing_us)
    (directory / "zarr.json").write_text(
        json.dumps(
            {
                "zarr_format": 3,
                "node_type": "array",
                "shape": list(values.shape),
                "data_type": "uint16",
                "chunk_grid": {
                    "name": "regular",
                    "configuration": {"chunk_shape": list(chunks)},
                },
                "chunk_key_encoding": {"name": "default", "configuration": {"separator": "/"}},
                "fill_value": 0,
                "codecs": [{"name": "bytes", "configuration": {"endian": "little"}}],
                "attributes": attributes,
            },
            indent=2,
        )
        + "\n"
    )
    for z in range(0, values.shape[0], chunks[0]):
        for y in range(0, values.shape[1], chunks[1]):
            for x in range(0, values.shape[2], chunks[2]):
                # Zarr's final regular chunk has the configured full chunk shape.  Pad it with
                # the declared fill value; readers crop it to the array shape.
                chunk = np.zeros(chunks, dtype="<u2")
                source = values[z : z + chunks[0], y : y + chunks[1], x : x + chunks[2]]
                chunk[: source.shape[0], : source.shape[1], : source.shape[2]] = source
                chunk_path = directory / "c" / str(z // chunks[0]) / str(y // chunks[1]) / str(x // chunks[2])
                chunk_path.parent.mkdir(parents=True, exist_ok=True)
                chunk_path.write_bytes(chunk.tobytes())


def write_anisotropic_ngff_pyramid(
    output: Path,
    level_zero: np.ndarray,
    spacing_us: tuple[float, float, float],
    max_size: int,
) -> list[tuple[int, int, int]]:
    """Write an uncompressed uint16 NGFF v3 pyramid with per-level anisotropic scales."""
    if level_zero.ndim != 3 or level_zero.dtype != np.uint16:
        raise ValueError("pyramid input must be a three-dimensional uint16 Z/Y/X array")
    factors = propose_pyramid(level_zero.shape, spacing_us, max_size)
    if output.exists():
        shutil.rmtree(output)
    output.mkdir(parents=True)
    for index, factor in enumerate(factors):
        level = mean_blocks(level_zero, factor)
        print(f"writing level {index}: {level.shape}, cumulative factor {factor}", flush=True)
        write_uncompressed_v3_array(output / str(index), level)
    scales = [[spacing * factor for spacing, factor in zip(spacing_us, factor)] for factor in factors]
    root_attributes = {
        "multiscales": [{
            "version": "0.4",
            "name": "cells3d membrane crop",
            "axes": [
                {"name": "z", "type": "space", "unit": "micrometer"},
                {"name": "y", "type": "space", "unit": "micrometer"},
                {"name": "x", "type": "space", "unit": "micrometer"},
            ],
            "datasets": [
                {"path": str(index), "coordinateTransformations": [{"type": "scale", "scale": scale}]}
                for index, scale in enumerate(scales)
            ],
        }],
        "omero": {"channels": [{
            "active": True, "label": "cell membranes", "color": "FF3355",
            "window": {"start": 0, "end": 65535, "min": 0, "max": 65535},
        }]},
    }
    (output / "zarr.json").write_text(json.dumps(
        {"zarr_format": 3, "node_type": "group", "attributes": root_attributes}, indent=2
    ) + "\n")
    return factors


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--max-size", type=int, default=32)
    args = parser.parse_args()

    source = args.source.resolve()
    output = args.output.resolve()
    print(f"reading {source}", flush=True)
    raw = tifffile.imread(source)
    if raw.shape != (60, 2, 256, 256) or raw.dtype != np.uint16:
        raise ValueError(f"unexpected cells3d source shape/dtype: {raw.shape} {raw.dtype}")

    # A central, non-empty membrane-channel crop: 32 Z planes by 128² XY pixels. It retains
    # original 0.29×0.26×0.26 µm sampling rather than fabricating a cubic voxel size.
    level0 = raw[14:46, 0, 64:192, 64:192].copy()
    print("building anisotropic pyramid", flush=True)
    factors = write_anisotropic_ngff_pyramid(output, level0, (0.29, 0.26, 0.26), args.max_size)
    digest = hashlib.sha256(source.read_bytes()).hexdigest()
    (output / "README.md").write_text(
        "# cells3d anisotropic OME-Zarr fixture\n\n"
        "A 32×128×128 membrane-channel crop from scikit-image's `cells3d` fluorescence "
        "microscopy image, stored as an uncompressed Zarr v3 three-level OME-NGFF pyramid. "
        "The source is CC0. The original dimensions are Z/C/Y/X = 60×2×256×256; this fixture "
        "keeps physical Z/Y/X sampling 0.29×0.26×0.26 µm and uses cumulative level factors "
        + ", ".join("×".join(map(str, factor)) for factor in factors) + ".\n\n"
        f"Source: {SOURCE_URL}\n\n"
        f"Source SHA-256: `{digest}`\n\n"
        "Generated with `scripts/prepare_cells3d_fixture.py`.\n"
    )
    print(f"wrote {output} from {source} sha256={digest}")


if __name__ == "__main__":
    main()
