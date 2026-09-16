#!/usr/bin/env python3
"""Regression tests for the standalone anisotropic Zarr-v3 pyramid writer."""

from __future__ import annotations

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

import numpy as np

MODULE_PATH = Path(__file__).with_name("prepare_cells3d_fixture.py")
SPEC = importlib.util.spec_from_file_location("prepare_cells3d_fixture", MODULE_PATH)
assert SPEC and SPEC.loader
fixture = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(fixture)


class PyramidBuilderTests(unittest.TestCase):
    def test_ten_to_one_schedule_delays_z(self) -> None:
        self.assertEqual(
            fixture.propose_pyramid((65_536, 65_536, 512), (1.0, 1.0, 10.0), 256)[:6],
            [(1, 1, 1), (2, 2, 1), (4, 4, 1), (8, 8, 1), (16, 16, 2), (32, 32, 4)],
        )

    def test_edge_voxels_are_averaged_and_written_as_padded_chunks(self) -> None:
        values = np.arange(9 * 33 * 34, dtype=np.uint16).reshape(9, 33, 34)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "edge.ome.zarr"
            factors = fixture.write_anisotropic_ngff_pyramid(root, values, (1.0, 1.0, 10.0), 32)
            self.assertEqual(factors[:2], [(1, 1, 1), (2, 2, 1)])
            metadata = json.loads((root / "zarr.json").read_text())
            scales = metadata["attributes"]["multiscales"][0]["datasets"]
            self.assertEqual(scales[1]["coordinateTransformations"][0]["scale"], [2.0, 2.0, 10.0])
            # The final level-zero chunk is a full 8×32×32 uint16 block even though its logical
            # data region is smaller; Zarr readers crop it using the declared array shape.
            edge_chunk = root / "0" / "c" / "1" / "1" / "1"
            self.assertEqual(edge_chunk.stat().st_size, 8 * 32 * 32 * 2)
            reduced = fixture.mean_blocks(values, (2, 2, 1))
            self.assertEqual(reduced.shape, (5, 17, 34))
            self.assertEqual(reduced[-1, -1, -1], np.rint(values[8:9, 32:33, 33:34].mean()))


if __name__ == "__main__":
    unittest.main()
