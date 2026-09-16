#!/usr/bin/env sh
# Reproducible Linux/local verification. Platform adapters, live remote credentials, and
# production-scale deployment are intentionally separate deferred acceptance gates.
set -eu

cd "$(dirname "$0")/.."

cargo fmt --check
cargo clippy --offline --workspace --all-targets --no-deps -- -D warnings
cargo test --offline --workspace -q

# Portable code coverage only; this is not a Windows/D3D12 adapter execution.
cargo check --offline -p newvolim-wgpu-frame --target x86_64-pc-windows-gnu

# Palace's GPU tests create Vulkan devices and must not compete in the Rust test harness.
cargo test --offline -p palace-core -q -- --test-threads=1

# Exercise the native host's complete bounded local-data path, including paired Palace PFM
# transport and linked orthogonal panes, against the committed anisotropic fixture.
cargo run --offline -q -p newvolim-desktop -- --smoke-local test-data/cells3d-anisotropic.ome.zarr

# Operator and storage resource-use declarations stay backend-neutral; Vulkan conversion belongs
# in the backend implementation rather than leaking into portable operator code.
! rg -n 'vk::(AccessFlags2|PipelineStageFlags2)' palace-dev/palace-core/src/operators palace-dev/palace-core/src/storage/gpu.rs

# Retarget Palace's actual entry/exit shaders through SPIR-V into both portable outputs.
(
    cd palace-dev
    # ZIP is a local OME-Zarr container option. Its append-only writer must finalize readable
    # archives and reject mutation operations rather than silently producing duplicate keys.
    cargo test --offline -q -p palace-zarr
    cargo run --offline -q -p palace-shader-spike
    # The shared core must remain usable by portable consumers without a Python ABI, while
    # bindings remain available only when their explicit feature is requested.
    cargo check --offline -q -p palace-core --no-default-features
    cargo check --offline -q -p palace-core --features python
    # Keep the public Python tensor route buildable without requiring the optional FFmpeg/video
    # development packages that are not part of the portable local baseline.
    cargo check --offline -q -p palace --no-default-features --features png
    cargo test --offline -q -p palace-wgpu-spike
    # The ignored subset is the real local WGPU adapter/runtime contract: it validates the
    # portable operator recorder, chunk-origin/padding uniforms, and recorder-selected tensor
    # scheduling rather than only compiling the spike.
    cargo test --offline -q -p palace-wgpu-spike -- --ignored --test-threads=1
    cargo check --offline -q -p palace-core --no-default-features --target wasm32-unknown-unknown
    cargo run --offline -q -p palace-wgpu-spike
    cargo run --offline -q -p palace-wgpu-spike -- \
        --raw-u16 ../test-data/cells3d-anisotropic.ome.zarr/0/c/0/0/0 --shape 8,32,32
)

node scripts/test_ui_admission.mjs
(
    cd crates/newvolim-ui
    # Trunk 0.21 requires a boolean, not NO_COLOR=1 inherited from some shells.
    env NO_COLOR=false trunk build --release
)
