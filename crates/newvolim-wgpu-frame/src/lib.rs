//! Reusable bounded native WGPU portable-frame recorder.
//!
//! The command-line binary and native desktop/headless callers share one implementation so the
//! fixed page-table, typed direct-volume, and annotation packet contracts cannot drift.

#[path = "main.rs"]
// The implementation file also contains CLI parsing/source-opening helpers used only by the
// binary target. They are intentionally not part of this library's public surface.
#[allow(dead_code)]
mod implementation;

pub use implementation::{
    encode_pfm, encode_pgm, encode_ppm, pack_portable_scene_channels, pack_portable_scene_layers,
    portable_scene_ray_ranges, prepare_portable_scene_gpu_packet, render_portable_camera_draw,
    render_portable_draw, render_portable_scene_camera_draw, PortableSceneGpuPacket, RayAxis,
    RenderedProjection, MAX_PORTABLE_SCENE_CHANNELS, MAX_PORTABLE_SCENE_RAY_STEPS,
};
