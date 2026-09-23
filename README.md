# ROZT

**Rusty OmeZarr Tiles** is a Rust viewer for OME-Zarr images, multidimensional
volumes, and their annotations.

![ROZT displaying the cells3d dataset](docs/rozt-cells3d.png)

The browser interface talks to `newvolim-server`; the existing crate names
remain internal implementation identifiers.

## Features

- Progressive tiled 2D navigation with multiscale pyramid selection and caching
- Linked XY, XZ, and YZ slices plus interactive 3D volume rendering
- Server rendering and client-side WebGPU rendering
- Named channels with OME colors, contrast ranges, and opacity controls
- Automatic coarsest-pyramid contrast estimation when display windows are absent
- Multiple image layers from configured OME-Zarr datasets
- Timepoint navigation for OME-Zarr time series
- Calibrated scale bars from NGFF voxel spacing and spatial units
- Editable point, rectangle, ellipse, polygon, freehand, and line annotations
- Exact integer label overlays with hashed or NGFF colors, outlines, isolation, and ID inspection
- Spatially indexed object and measurement overlays with dense-view protection, column coloring,
  numeric filters, and row inspection
- Atlas region names and per-region object counts
- Line profiles with distance in pixels and physical units
- Quaternion-based 3D camera controls linked to the 2D view center

## Build

Build the server and browser application:

```bash
cargo build --release -p newvolim-server
cd crates/newvolim-ui
trunk build --release
```

## Run

Datasets are registered by name and must be below an allowed root:

```bash
target/release/newvolim-server \
  --bind 127.0.0.1:9876 \
  --page-dir crates/newvolim-ui/dist \
  --allow-root /path/to/data \
  --dataset example=/path/to/data/example.ome.zarr
```

Open <http://127.0.0.1:9876/> and select the dataset.
