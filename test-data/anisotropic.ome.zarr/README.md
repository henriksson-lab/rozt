# Anisotropic OME-Zarr fixture

This is a compact, legally redistributable metadata fixture for CPU-side OME-NGFF parsing and
coordinate-transform tests. Its spatial axis order is Z/Y/X; level 0 has 5.0 × 0.5 × 0.5 µm
sampling and its two coarser levels only downsample X/Y. The shared translation makes omitted
per-level transforms detectable.

It intentionally contains no pixel chunks. It is a stable metadata/transform fixture, not the
representative image dataset required for the S1 Palace rendering gate. That real-data gate stays
open until a suitable redistributable volume is supplied.
