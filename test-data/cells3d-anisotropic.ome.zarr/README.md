# cells3d anisotropic OME-Zarr fixture

A 32×128×128 membrane-channel crop from scikit-image's `cells3d` fluorescence microscopy image, stored as an uncompressed Zarr v3 three-level OME-NGFF pyramid. The source is CC0. The original dimensions are Z/C/Y/X = 60×2×256×256; this fixture keeps physical Z/Y/X sampling 0.29×0.26×0.26 µm and uses level factors 1×1×1, 1×2×2, and 2×4×4.

Source: https://gitlab.com/scikit-image/data/-/raw/2cdc5ce89b334d28f06a58c9f0ca21aa6992a5ba/cells3d.tif

Source SHA-256: `afc7c7d80d38bfde09788b4064ac1e64ec14e88454ab785ebdc8dbba5ca3b222`

Generated with `scripts/prepare_cells3d_fixture.py`.
