# Stage-0 execution record

This file records evidence for the implementation gates in `PLAN.md`. A passing synthetic
test is not evidence that `palace-zarr` supports a representative production OME-Zarr.

## 2026-09-13: revised delivery scope and Linux regression baseline

The active delivery is the Linux/local-data path. Apple-Silicon and Windows/D3D12 adapter
executions, live authenticated S3/SSH integration, and production-scale deployment validation
remain explicitly deferred acceptance gates; they are not reasons to stop work that can be
implemented and verified locally. The implementation still retains portable API contracts and
source-policy tests for those future environments.

The complete local workspace regression baseline was rerun after this scope revision:

```sh
cargo test --offline --workspace
```

It passed all 70 unit tests across the local crates: decode (3), desktop (8), IO (24), pyramid
CLI (1), render (10), scene (7), server (16), and native WGPU frame (1). The run continues to
emit known upstream Palace warnings about a parenthesised error trait object and future-
incompatible implicit `f64`-to-`f32` literals; it emits no test failures. This is a Linux
regression baseline, not evidence for a deferred adapter or production-scale gate.

After the attachment-aware server/browser changes, `cargo fmt --check`, the same complete
offline 70-test workspace suite, and `cargo clippy --offline --workspace --all-targets --no-deps
-- -D warnings` all pass. The only output outside project crates is the known Palace warnings and
the existing `proc-macro-error2` future-compatibility notice.

The desktop's real local-fixture smoke was rerun after those transport changes. `cargo run
--offline -q -p newvolim-desktop -- --smoke-local test-data/cells3d-anisotropic.ome.zarr`
returned 821-byte fitted and 752-byte camera-adjusted volume PNGs, `[675,333,333]` orthogonal
PNG byte lengths, and shape `[128,128,32]`. Palace initialized two Vulkan devices while
continuing without the unavailable optional validation layer. This verifies the Linux/local
desktop path, not a deferred platform adapter.

Desktop polygon annotations are now bounded while their input iterator is consumed: at most 4,097
vertices are inspected to enforce the persisted 4,096-vertex limit, rather than first collecting
an arbitrary caller-provided iterator. The regression passes an infinite iterator, verifies the
bounded error, and verifies the existing point/polygon annotations remain unchanged. `cargo test
--offline -p newvolim-desktop -q` passes all 8 tests and strict desktop clippy passes. This is a
local UI/session resource-boundary safeguard; it makes no claim about remote annotation editing.

The browser polygon draft applies the same 4,096-vertex cap before changing the crosshair or
adding another point, so it reports an actionable limit rather than sending a known-invalid ROI
to Tauri. The UI admission regression asserts both the shared constant and the pre-append guard;
`node scripts/test_ui_admission.mjs` and `env -u NO_COLOR trunk build --release --config
crates/newvolim-ui/Trunk.toml` pass. This is a native-session UI rule, not a remote annotation
protocol.

Remote browser frame admission now records the active request ID and accepts a WebSocket reply
only from the current socket with that exact ID. A stale response cannot draw into the new
connection's canvas or release its admission slot. The Node UI regression simulates both a
mismatched reply and a replaced socket, then verifies that only the matching reply releases the
active slot; the release CSR build passes. This is protocol hardening tested locally, not a live
remote-server deployment result.

The offline S3 capability policy now rejects invalid bucket labels (including empty dotted
labels, label-edge hyphens, and IPv4-shaped names) before OpenDAL can construct a client. The
source-policy regression covers those cases alongside its bucket/profile/prefix authorization
checks; `cargo test --offline -p newvolim-io -q` passes 24 tests and strict IO clippy passes.
This improves the future S3 boundary without claiming live authenticated S3 access.

The execution sandbox does not permit loopback `bind()` (the standalone server exits with
`EPERM` before accepting connections), so a socket-level smoke cannot be performed here. To keep
the same application routing boundary covered locally, `newvolim-server` now has an in-process
Axum integration regression. It invokes the production router and verifies that discovery returns
only the configured `cells3d` name, its named `zarr.json` response has JSON MIME and real
multiscale metadata, and an unknown dataset or traversal-shaped asset route is not served.
`cargo test --offline -p newvolim-server` passes 13 tests and strict server clippy passes. This
does not turn the sandbox bind restriction into a project blocker; a normal Linux deployment can
run the separate loopback socket smoke later.

The server now also has a real local-frame integration test that needs no TCP listener: it creates
the production Axum router and dispatcher, POSTs a named `cells3d` request with bounded camera
controls, and waits for the actual Palace worker result. On this Linux host Palace initialized two
Vulkan devices (continuing without the unavailable validation layer), and the route returned a
valid `32×24` PNG with JSON request handling and a finite `Server-Timing: render;dur=…` value.
It also renders the fitted default and asserts its PNG differs from the bounded non-default
orbit/zoom request, proving that the interactive camera controls reach Palace rather than merely
being parsed by the server.
The same integration then requests all three linked orthogonal frames with an intentionally
out-of-range crosshair; the server clamps it to the fixture's `[127,127,31]` extent and returns
three valid PNG payloads with the declared `[128,128,32]` geometry. The focused test and the
complete server suite passed (14 tests), as did strict server clippy.

The real native WGPU local-data path was rerun against the committed anisotropic fixture with all
three output products enabled:

```sh
cargo run --offline -q -p newvolim-wgpu-frame -- \
  --zarr test-data/cells3d-anisotropic.ome.zarr \
  --output /tmp/newvolim-local-opacity.pgm \
  --color-output /tmp/newvolim-local-colour.ppm \
  --depth-output /tmp/newvolim-local-depth.pfm --axis z
```

It rendered a `128×128` PGM with all 16,384 pixels nonzero, a same-sized sRGB PPM, and a
same-sized little-endian, bottom-to-top PFM depth attachment. The renderer now splits cold
execution into load, adapter/device, setup/upload, and dispatch/readback phases; those host
figures are diagnostic rather than a steady-state frame-time claim. This verifies the native
WGPU depth product on the current Linux adapter; it does not claim that the separate Palace
colour transport yet emits a paired depth product.

`newvolim-wgpu-frame --timing-output FILE.json` now records those four phases in a stable JSON
artifact (`kind`, physical width/height, `loadMs`, `adapterDeviceMs`, `setupUploadMs`, and
`dispatchReadbackMs`) rather than requiring benchmark automation to scrape stdout. A local
fixture run wrote valid `128×128` JSON with finite timings for every phase. This is a cold-path
measurement record, not a claim that repeated dispatch timings have reached a production budget.

A corrected local rerun after enforcing the PFM bottom-to-top convention again produced opacity,
sRGB colour, PFM ray-distance, and timing products for the same `128×128×32` fixture. The PFM
has SHA-256 `7d40e45dc45526fd9b468ebdf65a8c2b9b5a1320058a5ca011353e23b35fa5b3`; cold timings
were 17.252 ms load, 294.786 ms adapter/device, 160.243 ms setup/upload, and 3.829 ms
dispatch/readback. The wide difference among cold runs reinforces that these are diagnostic host
measurements, not a steady-state budget.

`newvolim-wgpu-frame` now accepts `--warm-iterations N` to issue `N` completed dispatches after
the cold colour/depth readback while reusing the device, pipeline, bind group, and uploaded page
pool. It deliberately does not read those warm results back, so its `warmDispatchMs` must not be
compared directly with the cold `dispatchReadbackMs`. A real local `--warm-iterations 5` run on
the `128×128×32` fixture reported 8.383 ms cold dispatch/readback and 2.505 ms total warm
dispatch (0.501 ms mean); the depth PFM retained SHA-256
`7d40e45dc45526fd9b468ebdf65a8c2b9b5a1320058a5ca011353e23b35fa5b3`. The JSON timing artifact
now records both `warmIterations` and `warmDispatchMs`. This is repeatable Linux instrumentation,
not a production-scale throughput claim.

The Stage-0 S3 shader-retargeting probe now compiles Palace's actual ray entry/exit vertex and
fragment GLSL shaders to import-compatible SPIR-V 1.3 without debug metadata, validates each in
Naga, and writes both WGSL and MSL 1.2. The local command
`cargo run --offline -p palace-shader-spike` reports `756` SPIR-V words → `2882` WGSL / `4550`
MSL bytes for the vertex shader and `341` words → `1020` WGSL / `1855` MSL bytes for the
fragment shader. Palace's normal runtime compiler retains its prior SPIR-V 1.6 and debug/release
behaviour; the lower version is limited to this importer-facing tooling path. This establishes a
portable translation seam, not an Apple-Silicon runtime or a completed wgpu backend.

The Palace-owned bounded WGPU storage-page spike was rerun on the Linux Vulkan adapter and on a
real level-zero `cells3d` chunk (`8×32×32` Z,Y,X `uint16`). Both runs created four fixed 4 MiB
storage pages with six storage bindings/stage, resolved all page selections `[1,2,3,4]`, and
returned the packed page-table/depth witness `(3,42)`. The real-chunk raymarch edge/centre values
were `(4961,4682)`, demonstrating that the proof consumed actual microscopy bytes rather than
only its synthetic ball. This is Linux portable-representation evidence, not a deferred Apple or
Windows/D3D12 adapter execution.

### Palace paired-depth implementation boundary

The current Palace raycaster cannot safely expose its internal `state_ray` buffer as the S8
attachment. That buffer records progressive traversal state and uses a NaN sentinel when the
whole ray is done; it is not the first-opacity distance. Further, Palace state-cache keys combine
the *current operator* ID, chunk position, and cache name, so a sibling depth operator cannot
address the colour raycaster's private cache without breaking cache isolation. A correct Palace
implementation must therefore introduce an explicit paired raycast result/operator: the shader
must retain a separate `f32` first-opacity value (`+∞` on no hit) while it composites colour, and
the task graph must expose both completed surfaces through one render request. Reconstructing
depth from PNG alpha, the traversal cache, or a second unrelated raycast would violate the
render contract. Until that upstreamable operator is stable, the desktop payload declares
`DepthAttachment::None` for a colour-only or incomplete Palace result, while preserving a
validated optional PFM sidecar when supplied. The native/browser WGPU paths provide the verified
finite attachment implementation.

A local two-resolution probe confirmed why cache readback alone cannot close this gap: resolving
the colour producer in one Palace runtime resolution and looking up its cache in the next still
returns a new state page, because that private cache is not a task-graph output/dependency across
the frame boundary. The extra resolve was removed rather than adding a redundant render pass.
Consequently the real local Palace server test remains colour-verified and the attachment is
truthfully optional; making it mandatory requires the explicit paired raycast task output above.

The local frame transport now carries the complete attachment seam through the server/browser
protocol. `palace-frame` exposes a camera-controlled attachment render function; the WebSocket
envelope emits an optional base64 little-endian PFM sidecar and declares `RayDistanceF32` only
when it exists. The browser rejects a mismatched declaration/sidecar pair and preserves a valid
sidecar without confusing it with display PNG alpha. It validates the PFM header, physical extent,
exact byte length, and every finite/non-negative or `+∞` sample before retaining it; it also
enforces the server's 16 Mi-pixel frame budget and exact base64 length *before* decoding the
sidecar. Remote colour frames and each orthogonal pane likewise have a bounded base64 transport
and must expose PNG IHDR dimensions equal to the declared physical target before the browser
constructs an image. Server
serialization tests cover both the colour-only and paired envelope shapes; focused
Palace-frame/server tests, strict server clippy, the UI regression, and the release CSR build
pass. Palace currently exercises the colour-only shape, so this is forward-compatible transport
plumbing rather than a claim of a new raycaster depth producer.

`palace-png` now supplies the transport-side types that a paired operator will need:
`RayDistanceFrame` accepts only non-negative finite `f32` distances or `+∞` for a no-hit ray,
and `FrameAttachments` rejects a depth surface whose physical dimensions differ from its RGBA
frame. When such a surface is available, `encode_ray_distance_pfm` writes it losslessly as a
little-endian, bottom-to-top grayscale PFM sidecar; the focused regression verifies the exact
header, row order, and `+∞` bit pattern. `encode_attachments` packages the color PNG and that
optional PFM together from one `FrameAttachments` object, so a future paired output cannot mix
surfaces from distinct frames or silently discard supplied depth. Legacy PNG encoding now
traverses `read_attachments`, which truthfully returns no depth for the colour-only raycaster.
Its seven focused tests cover valid `+∞`, invalid NaN/negative and zero-sized frames, mismatched
attachments, the PFM encoding, and paired/colour-only encoded payloads. This is deliberately an
API/readback foundation only; it does not assert that the current Palace raycaster emits the
attachment.

`palace-frame` now carries that boundary through its synthetic and authorized-local-Zarr
readback APIs (`render_synthetic_attachments` and `render_local_zarr_attachments`). Existing
PNG and RGBA callers remain compatible wrappers over the same path. The focused synthetic test
proves the physical `16×12` colour frame and verifies that the returned ray-distance field is
explicitly `None`; it does not blur the distinction between an absent Palace attachment and the
native/browser WGPU attachments that are already real.

The CSR desktop transport now validates every native PNG payload before drawing it: its physical
extent must match the declared target, its format must be final sRGB `rgba8Unorm`, and a
depth-capable declaration must have a matching validated PFM sidecar. The UI admission regression
rejects a mismatched extent, non-final progress, or mismatched depth declaration, and the release
Trunk build passes. Browser WebGPU continues to own its independent real `RayDistanceF32`
readback path.

The loopback WebSocket frame service now carries that same `RenderTarget` declaration on both
volume and orthogonal PNG envelopes. The browser rejects a remote frame whose positive physical
extent, final state, sRGB `rgba8Unorm` format, or explicit `none` depth declaration does not
match the envelope. This prevents the remote colour-only transport from being interpreted as a
depth-bearing frame merely because its field set differs from Tauri's. The server suite (14
tests), strict server clippy, UI admission regression, and release Trunk build pass.

Camera-control validation is now public at the Palace frame boundary and applied before a request
enters the server dispatcher. Orbit deltas beyond ±10,000, non-finite zooms, and zoom values
outside 0.25–4 are `400 Bad Request` inputs rather than delayed renderer failures that occupy the
single active render slot. The server regression covers the accepted bounds and rejected values;
the focused server suite now has 15 tests.

Orthogonal crosshair input is also now all-or-nothing: clients either provide all `x`, `y`, and
`z` coordinates or request the centre planes, while volume requests must omit them. Partial or
irrelevant coordinates are rejected at the same `400` boundary instead of silently selecting a
different slice. The focused server suite now has 16 tests.

Attached volume canvases now also observe CSS-layout resizes. A change in their derived physical
extent is submitted through the same debounced newest-request-wins camera admission path used for
drag and wheel input, preventing a stale native, remote, or browser-WebGPU frame from being
stretched after a window resize. The UI admission regression and release Trunk build pass.

`palace-png` local input now normalizes palette/low-bit/16-bit variants to eight-bit colour,
converts grayscale and grayscale-alpha PNGs to Palace RGBA frames, and returns ordinary I/O or
format errors rather than panicking while writing a PNG. Its six focused tests include actual
grayscale and grayscale-alpha temporary files through the reader. This improves the local image
source boundary; it is independent of the still-required paired raycast depth output.

The desktop's headless local smoke completed against the committed `cells3d` fixture with a
`32×24` target: it opened the `128×128×32` level-zero shape and returned nonempty default-volume
(821-byte), camera-volume (752-byte), and linked XY/XZ/YZ (675/333/333-byte) PNGs. This is a
reproducible Linux/local transport check, not a window-manager interaction test or a claim about
the deferred Apple-Silicon and Windows/D3D12 adapters.

## 2026-09-12: palace Zarr build (S1 prerequisite)

The raycaster demo builds for the relevant Zarr-only configuration:

```sh
CARGO_TARGET_DIR=/tmp/newvolim-palace-s1 \
  CMAKE_POLICY_VERSION_MINIMUM=3.5 \
  cargo build -q -p demo-raycaster --no-default-features --features zarr
```

Result: success on the Linux development host. The build emits existing warnings about
parenthesised trait-object syntax and future-incompatible `f64`-to-`f32` fallback literals;
it emits no errors.

Notes:

- `shaderc-sys 0.9.1` needs `CMAKE_POLICY_VERSION_MINIMUM=3.5` with the host's newer CMake,
  because its bundled CMake project declares an obsolete policy minimum.
- The demo's default features enable `video`, which requires the unavailable system package
  `libavfilter`. This is unrelated to the OME-Zarr path; build the Zarr-only configuration
  above for S1 until demo features are made independently selectable by default.
- The workspace now contains `test-data/cells3d-anisotropic.ome.zarr`: a committed CC0,
  pixel-bearing Zarr v3 OME-NGFF crop with a three-level anisotropic pyramid. Its later
  llvmpipe run records the completed S1 runtime gate. It is intentionally compact, so it does
  not substitute for a production-scale throughput and residency benchmark.

## Confirmed source fact (S5)

`palace_io::Hints::lod_downsample_steps` is one `Vector<DDyn, DownsampleStep>`. Both
`palace_io::open_or_create_lod` and `palace_zarr::save_lod_tensor` reuse that vector at every
level. It cannot represent a per-level anisotropic downsample schedule. The plan's custom
anisotropic pyramid builder (or an upstream API extension that accepts such a schedule) is
required.

## 2026-09-12: Vulkan render-loop smoke test

The release raycaster ran to completion on the Linux development host using llvmpipe under a
virtual X display:

```sh
xvfb-run -a /tmp/newvolim-palace-s1/release/demo-raycaster \
  --bench --mem-size 512M --gpu-mem-size 512M synthetic 64 ball
```

Result: exit status 0; `Finished initializing Vulkan! (2 devices)`.

This validates the existing Vulkan window/render loop and a CPU Vulkan fallback in this
environment. It is **not** the real-data part of S1: the volume is palace's synthetic 64³ ball,
not an anisotropic OME-Zarr. A debug build currently cannot run on this host because palace
unconditionally requests `VK_LAYER_KHRONOS_validation` under `debug_assertions`, while that
layer is absent; the release build intentionally has no such layer request.

## 2026-09-12: forced llvmpipe real-data headless frame (Stage 4 progress)

The release headless renderer was then explicitly constrained to Mesa's CPU Vulkan ICD, rather
than relying on device enumeration on a GPU-equipped host:

```sh
env VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json \
  palace-dev/target/release/demo-headless-frame \
  --zarr test-data/cells3d-anisotropic.ome.zarr \
  --output /tmp/newvolim-cells3d-llvmpipe.png --width 192 --height 144
file /tmp/newvolim-cells3d-llvmpipe.png
sha256sum /tmp/newvolim-cells3d-llvmpipe.png
```

It initialized one Vulkan device and produced a valid 192×144 RGBA PNG with SHA-256
`3e2eef6abeb0eb87b27882e435b78ca663b5268bc9f302cc868fb97a89bc9201`, matching the fitted-camera
real-data reference. This proves the release Palace frame path itself works on the CPU driver.

The release `newvolim-server` was also run with that same ICD, bound only to loopback, and served
the named `cells3d` dataset. Its health endpoint returned `{"status":"ok"}`; an HTTP
192×144 frame was a valid RGBA PNG with that same checksum and reported
`Server-Timing: render;dur=1119.073`. This closes the basic CPU-only server fallback execution
gap. It is one cold request, not evidence of concurrency behavior or CPU performance under load.

## 2026-09-12: Leptos CSR shell (S4 partial)

The project now contains a CSR-only Leptos canvas shell with linked XY, XZ, YZ and 3D canvas
surfaces. It builds to a browser artifact successfully:

```sh
PATH="$PWD/.tools/bin:$PATH" NO_COLOR=true trunk build --release
```

Run that command from `crates/newvolim-ui`. The project-local `.tools/bin/wasm-bindgen` is
version 0.2.128, matching the Rust dependency; it is needed because the host-global CLI is
0.2.127 and Trunk cannot write its own global cache in this environment.

This was deliberately **partial** S4 evidence at this point in the chronology: it proved the CSR
build seam and physical-canvas layout, but not a Tauri webview displaying a frame rendered by
native Palace. The Vulkan frame readback/IPC implementation was subsequently added in the
native-frame-payload update below.

## 2026-09-12: Tauri desktop host (S4 partial)

`newvolim-desktop` packages the CSR distribution in a Tauri 2 host. It compiles successfully
with `cargo check -p newvolim-desktop` and remained alive for the complete virtual-display
startup probe below; exit code 124 is the expected `timeout` termination, not a crash:

```sh
timeout 8s xvfb-run -a cargo run -q -p newvolim-desktop
```

The host has an application icon at `crates/newvolim-desktop/icons/icon.png`. It was generated
with the built-in image-generation tool for this project: a transparent, text-free scientific
volume icon showing translucent cyan/violet orthogonal imaging slices. The browser bundle and
the desktop webview are proven independently; the later native-frame-payload update connects
Palace readback and IPC. A scripted local-file interaction remains the outstanding S4 evidence
gap.

## 2026-09-12: local OME-Zarr desktop-session boundary (S3/S4 foundation)

The desktop host now has test-covered commands for `open_local_omezarr` and `session_summary`.
The open command canonicalizes and directory-checks the user-selected root, then uses the bounded
local v2/v3 OME-Zarr metadata reader. Its serializable result reports the root, multiscale count,
and OMERO channel count. It explicitly returns `rendererConnected: false`; a successful metadata
open is therefore not represented as a rendered or streamed frame.

Verification:

```sh
cargo test -p newvolim-scene -p newvolim-desktop
cargo clippy -p newvolim-scene -p newvolim-desktop --all-targets -- -D warnings
```

Both commands passed. This is a host-side local-data seam only: UI file selection and native
Palace pixel transport remain open work.

## 2026-09-12: Palace in-memory frame encoding seam

`palace-png` now exposes `encode`, alongside its file-writing API. It requests a completed
`FrameOperator` through Palace's normal CPU storage path and returns an in-memory RGBA PNG. This
avoids treating a Vulkan swapchain image as an application transport and is usable by both an
embedded desktop webview and a future frame server.

```sh
cd palace-dev
CARGO_TARGET_DIR=/tmp/newvolim-palace-s1 \
  CMAKE_POLICY_VERSION_MINIMUM=3.5 \
  cargo test -p palace-png --no-default-features
```

Result: passed (`encodes_a_valid_rgba_png_in_memory`). This proves the byte encoder boundary;
it does not yet prove an end-to-end raycast-to-webview frame, which still requires the native
renderer worker and UI transport.

## 2026-09-12: committed anisotropic transform fixture (Stage 1)

`test-data/anisotropic.ome.zarr` is a small, redistributable OME-NGFF metadata fixture. It has
Z/Y/X axes, 10:1 Z-to-X/Y spacing, a shared translation, three pyramid levels that delay Z
downsampling, and one OMERO display channel. The corresponding test verifies both level-0 and
coarse-level physical coordinates; it catches axis swaps, scalar-spacing regressions, missing
translation, and accidental Z decimation.

```sh
cargo test -p newvolim-io
cargo clippy -p newvolim-io --all-targets -- -D warnings
```

Both passed (8 unit tests). It is deliberately metadata-only, not a substitute for the S1
representative pixel-data gate.

## 2026-09-12: headless Vulkan raycast to frame bytes (S4 transport prerequisite)

`palace-dev/demo-headless-frame` is a small reference executable for the non-window transport
path. It constructs a Palace volume, raycasts it, requests the completed frame through CPU
storage, and encodes the returned pixels to PNG. This is the same rendering/readback boundary a
Tauri webview and a future frame server use; no swapchain pixels are exposed.

```sh
cd palace-dev
CARGO_TARGET_DIR=/tmp/newvolim-headless-release-2 \
  CMAKE_POLICY_VERSION_MINIMUM=3.5 \
  cargo build --release -p demo-headless-frame --no-default-features
/tmp/newvolim-headless-release-2/release/demo-headless-frame \
  --output /tmp/newvolim-headless-frame.png \
  --volume-size 32 --width 128 --height 96 \
  --mem-size 256M --gpu-mem-size 256M
file /tmp/newvolim-headless-frame.png
```

Result: exit status 0; Vulkan initialized one device; output was a 128×96 RGBA PNG and visual
inspection showed the expected rendered red ball. This is a synthetic-volume transport proof,
not S1 real-data evidence and not yet Tauri IPC.

The executable now also accepts `--zarr <local-path>` and feeds that local Palace Zarr source
through the same cast, raycast, CPU readback, and PNG encoding path. Its Zarr build check passed:

```sh
cd palace-dev
CARGO_TARGET_DIR=/tmp/newvolim-palace-s1 \
  CMAKE_POLICY_VERSION_MINIMUM=3.5 \
  cargo check -p demo-headless-frame --no-default-features
```

The committed `cells3d` fixture subsequently supplied the runtime evidence: the release binary
renders it through this same local Palace Zarr path (see the S1 update at the top of this record).

## 2026-09-12: native Palace frame payload in the Tauri/CSR seam (S4 substantial progress)

`palace-frame` is now the shared window-independent renderer. The Tauri host invokes it directly
for a deterministic transport preview or for the canonicalized, opened local dataset. It returns
an explicit `FramePayload` (`image/png`, physical width/height, base64 data URL). The CSR shell
can draw that payload into the 3D canvas; it exposes a local-path action and a synthetic transport
test. Browser deployments report that the native renderer is unavailable instead of pretending
to support it.

The desktop build enables Tauri's global API only for this bundled local UI, and no remote source
or credential path is added by this work.

Verification:

```sh
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo test -p newvolim-desktop
PATH="$PWD/.tools/bin:$PATH" NO_COLOR=true trunk build --release  # from crates/newvolim-ui
timeout 8s xvfb-run -a env CMAKE_POLICY_VERSION_MINIMUM=3.5 \
  cargo run -q -p newvolim-desktop
```

The desktop tests passed (including the self-contained PNG payload test), the CSR build passed,
and the host stayed alive until the expected timeout. The current bundled host was rechecked with
`timeout 20s xvfb-run -a cargo run --offline -q -p newvolim-desktop`; it likewise remained alive
until the expected timeout without a startup error. The remaining S4 evidence gap is an automated
or manually observed webview click showing the native payload on the canvas. The later browser
and remote-frame checks do not replace that native-webview interaction check.

`newvolim-desktop --smoke-local PATH` now provides a non-window-manager integration probe for
the same local host path: it opens through `LocalSession`, renders a bounded Palace 3D frame and
all three linked orthogonal panes, and rejects empty PNG payloads. The committed fixture passed:

```sh
cargo run --offline -q -p newvolim-desktop -- \
  --smoke-local test-data/cells3d-anisotropic.ome.zarr
```

On this host it initialized two Vulkan devices for each render (without the unavailable optional
validation layer), reported the correct `128×128×32` XYZ shape, and produced 821 bytes for the
default volume PNG, 752 bytes for a bounded non-default orbit/zoom camera PNG, plus 675/333/333
bytes for XY/XZ/YZ. This closes the headless local-session-to-Palace and bounded-camera integration
evidence; it still does not replace scripted interaction inside the visible webview.

## 2026-09-12: loopback frame-service foundation (Stage 4 partial)

`newvolim-server` supplies non-HTML `/v1/frame` and `/health` services. It defaults to
`127.0.0.1`, requires one or more `--allow-root` directories and at least one `--dataset
NAME=PATH` registration. `LocalSourcePolicy` canonicalizes the registered paths at startup; frame
and browser-chunk clients then send only the opaque configured name. This keeps server filesystem
paths out of both HTTP and WebSocket requests and serializes render admission to one in-flight
Palace task. This is intentionally not presented as a multi-user/remote deployment:
authentication, quotas, audit logs, tenant cache separation, revocation, remote stores, and
progressive WebSocket backpressure remain required.

Verification:

```sh
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo check -p newvolim-server
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo run -q -p newvolim-server -- \
  --bind 127.0.0.1:19876 --allow-root test-data \
  --dataset cells3d=test-data/cells3d-anisotropic.ome.zarr
curl -fsS http://127.0.0.1:19876/health
```

The server compiled and the loopback probe returned `{"status":"ok"}`. The registered
`cells3d` fixture is pixel-bearing, and later entries record successful local Palace frame
rendering through this named-dataset boundary.

## 2026-09-12: portable wgpu bounded brick-pool selection (S7)

`palace-wgpu-spike` chooses the first portable representation: **four statically bound
storage-buffer pages**, each 4 MiB, with a packed `u32` location (`page` plus word offset).
It deliberately requests only default WebGPU limits and no binding-array, device-address, 64-bit
atomic, or adapter-specific feature. The compute proof binds the output plus all four pages as
six fixed storage bindings (including a packed page table), resolves every ray sample through its
packed location, and verifies that every page was selected after GPU readback. Each selected page
contains only its own compact subset of source words: the proof no longer mirrors the volume into
all four bindings.

```sh
cd palace-dev
cargo test -p palace-wgpu-spike
cargo run -q -p palace-wgpu-spike
```

Both passed on the current headless adapter. The actual output was:

```text
adapter: Quadro RTX 5000 (Vulkan; driver: NVIDIA)
portable bounded storage-page pool passed: 4 pages × 4194304 bytes,
6 storage bindings/stage (adapter limit 524288), storage binding bytes limit 2147483644,
packed location=0x0030002a; raymarch edge/center=(0, 46129),
page-table/depth proof=(3, 42), selected pages=[1, 2, 3, 4]
```

The spike now also accepts one real, already-decoded little-endian `uint16` brick with explicit
`Z,Y,X` dimensions. It converts the brick to portable `u32` storage words, uploads it to the same
four fixed pages as compact page-local allocations, and uses the packed table for every raymarch
sample; the shape is carried by a uniform rather than baked into the shader. On the committed
level-zero cells3d chunk this command passed on the Quadro RTX 5000:

```sh
cargo run --offline --manifest-path palace-dev/Cargo.toml -p palace-wgpu-spike -- \
  --raw-u16 test-data/cells3d-anisotropic.ome.zarr/0/c/0/0/0 --shape 8,32,32
```

It reported dimensions `32×32×8` in GPU `X,Y,Z` order, raymarch samples `(4961, 4682)`, and the
same page-table/depth and all-four-page witness (`[1,2,3,4]`). This is real local pixel data
flowing through native wgpu's bounded page representation. It is still a Stage-0 proof: callers
provide a raw chunk and shape explicitly; Zarr chunk-key discovery, codec decoding, multi-brick
assembly, camera/output presentation, and integration with Palace's task graph remain Stage-6
implementation work.

The WGSL proof performs an actual 16-step alpha raymarch through a synthetic `16³` volume,
resolving each voxel through a packed page-table location into one of the four static pages, and
producing a `32×32` storage-image-equivalent readback. On the current adapter its edge/centre
samples are `(0, 46129)`, proving that the non-empty volume is sampled while an empty ray remains
transparent; its page witnesses `[1, 2, 3, 4]` prove every static page was selected. It writes a
page-table/depth proof `(3, 42)` to the same readback, exercising the S8 lifetime shape. This is
deliberately a small storage-buffer raymarch, not yet a textured volume raycaster, physical depth
texture, or Palace integration.

The browser CSR renderer now uses the same packed page-table shape in its real WebGPU render pass:
a page-table storage buffer and four 4 MiB storage pages are bound at fixed bindings, and the WGSL
fragment shader resolves the selected page from the packed entry before producing the frame. A
release bundle reached `Browser WebGPU synthetic-volume frame (packed page table, four fixed
storage pages)` through loopback headless Chromium with
`--enable-unsafe-webgpu --enable-features=Vulkan`. That supplies browser execution evidence for
the selected representation, not merely a limit comparison.

The direct-browser NGFF loader accepts its chunk control in semantic `Z,Y,X` order, then maps it
to the order declared by the array's `axes` before bounds checks and `c/...` URL construction.
The UI regression covers both conventional `Z,Y,X` and reordered `X,Y,Z` metadata, preventing a
valid-but-reordered dataset from silently loading a different brick. `node
scripts/test_ui_admission.mjs` and `env -u NO_COLOR trunk build --release --config
crates/newvolim-ui/Trunk.toml` pass. This corrects metadata interpretation; it does not replace
the still-required real-adapter browser runs on Apple and Windows.

That loader now applies a streaming **1 MiB** transport/allocation cap before parsing its root
`zarr.json`, array `zarr.json`, or chunk-server discovery JSON; it rejects non-success responses,
oversized advertised or streamed bodies, and malformed JSON. Its chunk cap remains independently
at 16 MiB encoded and each decoded browser brick remains bounded to its 4 MiB fixed storage page.
The regression covers valid, oversized, and malformed metadata documents. These are client-side
preview budgets, not a substitute for the server's authorization, tenant, and audit controls.

Before resolving `dataset.path` below the selected direct-browser store root, the loader now
requires a non-empty normal relative path and checks raw and percent-decoded segments. It rejects
`.`/`..`, encoded traversal, decoded separators, empty internal components, query/fragment
delimiters, and control characters, then verifies the resolved URL remains below the selected
root. The same UI regression exercises valid nested paths and those adversarial cases. This keeps
root metadata from changing the browser's requested store scope through URL resolution.

Every direct-browser OME-Zarr root/array/chunk fetch now uses `credentials: "omit"` and
`redirect: "error"`. Thus a user-entered preview root cannot receive ambient browser credentials
or silently redirect the scoped store to another origin. This policy applies only to the direct,
credential-free preview route; named application-server discovery remains a separate transport
that can later participate in explicit application authentication.

Direct browser stores must use HTTPS. The sole HTTP exception is `localhost`, `127.0.0.1`, or
`[::1]`, retained for the committed loopback fixture and local development; arbitrary remote HTTP
roots are rejected before any metadata request. The UI regression covers HTTPS normalization,
loopback HTTP acceptance, and remote HTTP/file URL rejection.

This settles the S7 representation choice for the first wgpu backend. Four pages are a portable
baseline, not the final residency capacity; page count and byte budget remain adapter-governed.
The same proof must still run on macOS and Windows before claiming full S7 target coverage.

## 2026-09-12: Palace SPIR-V retargeting is not currently viable (S3)

`palace-shader-spike` compiles Palace's actual `entryexitpoints.vert` and
`entryexitpoints.frag` through Palace's runtime GLSL/shaderc path, then attempts Naga 30
SPIR-V import, validation, WGSL output, and MSL output. The push-constant declaration uses the
same scalar-array ABI as Palace's generated `Matrix<D4, f32>` and `Vector<D2, u32>` structs.

```sh
cd palace-dev
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo run --release -q -p palace-shader-spike
```

The Palace GLSL compiles successfully, but Naga's release SPIR-V importer fails at the vertex
shader with `unsupported instruction CopyLogical at Function`. A debug build fails earlier on
`OpLine`, so removing debug information does not solve the material incompatibility. Therefore
S3 selects **A3(b), a WGSL rewrite**, for browser-capable work; an optional native-only SPIR-V
retargeter would require a different translator or a demonstrated lowering pass. No WGSL/MSL
output or cross-platform backend compatibility is claimed from this spike.

## 2026-09-12: backend-neutral target and progressive-admission contract (S8)

`newvolim-render` now makes the renderer/UI boundary explicit and testable: extents are physical
pixels; `Rgba16Float` is linear-only (encoding happens in a display or stream blit); and the
optional depth product is an `f32` camera-ray distance to the first opacity-contributing volume
sample, with `+∞` denoting no hit. This supplies annotation rendering with a stable `t_max`
meaning instead of overloading raster depth. Frame state is `Preview`, `Refining { pass }`, or
`Final`, and admission retains only the newest request queued behind an uncancellable task.

```sh
cargo test -p newvolim-render
```

The contract tests establish format validation, physical extent handling, depth semantics, and
latest-request coalescing. This is the API-level S8 proof; a Vulkan and wgpu renderer must still
emit a matching depth surface and prove compositing against it before S8 has implementation-level
coverage.

The Tauri frame payload now carries that contract at the native/UI boundary: its PNG frames
explicitly declare physical extent, `Rgba8Unorm`/sRGB encoding, `Final` progress, and no depth
attachment. This prevents the current Palace colour-only readback from being mistaken for a
depth-aware or progressively refinable result while keeping the payload shape compatible with
the later paired-output implementation.

`palace-png` now exposes that boundary below the encoder as `read_rgba`: resolving a rechunked
Palace `ImageOperator` yields a dimensioned CPU-resident `RgbaFrame`, and PNG encoding delegates
to it. `palace-frame` exposes the same raw result for synthetic and local-Zarr 3D and orthogonal
renders, so callers do not have to encode/decode PNG merely to obtain pixels. Its regression
renders a complete `16×12` Vulkan frame and verifies the dimensions and byte count. In debug
builds, Palace now uses `VK_LAYER_KHRONOS_validation` when it is installed, but enumerates layers
and warns before continuing without it when it is absent; that regression ran successfully on
this host with two Vulkan devices. This is an upstreamable colour-readback seam for future
non-PNG transport or paired attachments. It deliberately does **not** claim a depth product—
Palace must first produce a separate ray-distance operator before the application can expose
`RayDistanceF32`.

`newvolim-render` now also makes one readback sample a typed `RayDistance`: only non-negative
finite distances and the explicit `+∞` no-hit value are representable. Its depth-aware picker
entry point accepts that type directly, so a NaN, negative infinity, or negative GPU readback
cannot accidentally become an unbounded overlay pick. The contract test covers finite occlusion,
no-hit behavior, and all rejected invalid values. This validates consumers of a future Palace
attachment; it does not claim that Palace produces that attachment yet.

The CSR shell converts each canvas's CSS size through `window.devicePixelRatio` before requesting
a native frame, and uses that same physical size for the WebGPU backing store. Pointer positions
continue to be mapped through `getBoundingClientRect`, so the 2D crosshair remains in CSS-space
input while renderer targets are never accidentally requested at CSS resolution. `trunk build
--release` passed after this change.

## 2026-09-12: S2 decode benchmark evolution

At the start of this work S2 was open: the committed `cells3d` fixture is intentionally
uncompressed so it can be a small, dependency-free real-pixel regression fixture, and no pinned
compressed corpus or browser comparator existed. The following entries record the corpus and
browser work that subsequently resolved the fixture-sized S2 decision.

The next S2 change must add a pinned corpus with identical decoded payloads in zstd, LZ4, and
Blosc variants; report native Rust, single-threaded wasm with `+simd128`, and `numcodecs.js`
throughput using the same warm-up, iteration count, and output checksum. It must also record
first meaningful-frame time, refinement time, peak RAM, and request-drop rate as required by the
Stage-0 gate.

`crates/newvolim-decode-bench` now supplies the measurement scaffold: a deterministic 4 MiB
payload, common warm-up/iteration/checksum rules, zstd and LZ4 decoding, JSON output, and an
offline `wasm32-unknown-unknown` build with `-C target-feature=+simd128`. Its first native run
reported 406.626 MiB/s for zstd and 681.552 MiB/s for LZ4. Those values are **not S2 results**:
the payload is intentionally synthetic. They only prove the initial native/wasm harness mechanics
and gave the later real-corpus work a stable command surface.

The same shared loop is now exported through wasm-bindgen and was executed in Node 20 with SIMD:
394.089 MiB/s zstd and 559.441 MiB/s LZ4, with the same 4 MiB checksum and 3 warm-up/20 measured
iterations. The harness now prefers `performance.now()` on wasm because `std::time::Instant`
panics on `wasm32-unknown-unknown`; native continues to use `Instant`. This initial Node run was
not browser evidence; later paragraphs record the real-corpus and browser measurements.
The wasm-bindgen CLI schema must exactly match the Cargo lockfile; this workspace uses 0.2.128
for its CSR build, while the local standalone CLI is 0.2.127. The recorded Node run used the
matching cached 0.2.127 export, and the checked-in 0.2.128 configuration is verified to compile
for wasm and to build through Trunk.

The harness also accepts `--chunk-dir` and recompresses/decodes each file independently, matching
the Zarr work unit rather than concatenating a stream. On the committed `cells3d` level-zero
directory (64 real microscopy chunks, 1,048,576 decoded bytes), native decoding measured 355.569
MiB/s zstd (891,718 encoded bytes) and 625.247 MiB/s LZ4 (1,052,934 encoded bytes). This is the
first real-pixel codec evidence; later paragraphs extend it to wasm/browser and `numcodecs.js`.

The same real-chunk run now includes Blosc configured as LZ4, level 5, byte shuffle, and
`typesize=2` for the fixture's `uint16` voxels: 960,549 encoded bytes and 597.568 MiB/s native
decode. `zarrs` supplies this codec through native `blosc-src` and portable wasm `blusc`; the
entire three-codec harness compiles for `wasm32-unknown-unknown` with `+simd128`.

With an offline-built matching wasm-bindgen 0.2.128 CLI, the same 64 real chunks were passed as
JavaScript `Uint8Array`s to the SIMD wasm export in Node 20. It reported 215.054 MiB/s zstd,
625.000 MiB/s LZ4, and 625.000 MiB/s Blosc-LZ4/shuffle. This proves the production wasm codec
paths and chunk-boundary loop execute; Node's millisecond clock gives coarse values, and Node is
not a browser or `numcodecs.js` comparison. S2 therefore remains open for that browser-grounded
decision, but the single-threaded wasm decoder is no longer unmeasured.

The temporary `numcodecs@0.3.2` installation was then measured by
`scripts/benchmark_numcodecs.mjs` on the identical files, with three warm-up and 20 measured
iterations, independent per-chunk encode/decode, and a checksum after every decode. It reported
92.347 MiB/s zstd (891,951 encoded bytes), 117.959 MiB/s LZ4 (1,052,349 bytes), and 95.897 MiB/s
Blosc-LZ4/shuffle (995,652 bytes). Its package configuration is zstd level 3, LZ4 acceleration 1,
and Blosc LZ4/level 5/byte-shuffle. The Node measurements are still not browser performance, but
the matching corpus shows the Rust SIMD wasm path is materially faster than this incumbent JS
stack, especially for LZ4.

Chromium headless then loaded the actual production SIMD wasm module and fetched the same 64
fixture chunks over loopback HTTP. Its real-clock result was 187.793 MiB/s zstd, 645.161 MiB/s
LZ4, and 626.959 MiB/s Blosc-LZ4/shuffle. That completes the native-versus-browser-wasm half of
S2 and indicates single-threaded client decoding is viable for this representative chunk size.

That final browser comparator now passes after supplying an import map for `numcodecs`'s `fflate`
dependency. In one Chromium-headless real-clock run over the exact same 64 chunks and rules, Rust
SIMD wasm measured 177.148 MiB/s zstd, 573.066 MiB/s LZ4, and 613.497 MiB/s Blosc-LZ4/shuffle;
browser `numcodecs@0.3.2` measured 133.067, 218.103, and 154.202 MiB/s respectively. Its encoded
sizes were 891,951, 1,052,349, and 995,652 bytes, while Rust's were 891,718, 1,052,934, and
1,049,600 bytes due to implementation differences. Every decoded chunk passed the same FNV-1a
checksum. **S2 is resolved for the representative fixture:** the single-threaded Rust wasm path
is viable and materially faster than the incumbent browser stack for LZ4/Blosc, so COOP/COEP is
not an MVP prerequisite. Larger production chunk sizes must still be re-benchmarked before a
deployment-wide threading policy is frozen.

The S2 browser check was re-run from a fresh release WASM bundle built from the current workspace
with `wasm-bindgen 0.2.128`, rather than the earlier cached bundle. A loopback Chromium run over
the same 64 cells3d chunks, three warm-up and 20 measured iterations, and per-chunk FNV-1a
checksums reported Rust SIMD WASM at **191.205 MiB/s** zstd, **623.053 MiB/s** LZ4, and
**619.195 MiB/s** Blosc-LZ4/shuffle. Browser `numcodecs@0.3.2` reported **145.666**, **204.918**,
and **144.928 MiB/s** respectively. This preserves the S2 conclusion with a current artifact;
the values are host diagnostics, not a production-size performance target.

## 2026-09-12: S5 Palace pyramid-generation capability

S5 is resolved: `palace_io::Hints::lod_downsample_steps` is a single
`Vector<DownsampleStep>` reused for every generated level. `create_lod` repeatedly calls
`coarser_lod_md` with that same vector; a synchronized axis can pause only when it has no more
chunks, not because a later level specifies a different schedule. The Zarr writer follows the
same pattern when it recreates levels. Consequently Palace cannot create Newvolim's prescribed
per-level anisotropic pyramid schedule as-is. Stage 3 must implement the builder in Newvolim or
offer an upstream API that accepts a vector of per-level step vectors.

## 2026-09-12: Newvolim anisotropic pyramid writer (Stage 3 progress)

`scripts/prepare_cells3d_fixture.py` is no longer a three-level special case. Its reusable
`propose_pyramid` and `write_anisotropic_ngff_pyramid` functions generate an uncompressed
uint16 Zarr-v3 OME-NGFF pyramid for arbitrary Z/Y/X array dimensions: every level has its own
cumulative factor and physical `scale`, thick axes wait until their current physical voxel size
is within 2× of the finest axis, and edge blocks are averaged without dropping samples. The
writer pads final encoded chunks with the declared zero fill value, so their fixed regular chunk
shape remains valid while readers crop them by the logical array shape.

```sh
python3 scripts/test_prepare_cells3d_fixture.py
python3 scripts/prepare_cells3d_fixture.py \
  --source /tmp/newvolim-cells3d.tif \
  --output /tmp/newvolim-cells3d-pyramid-rebuild.ome.zarr --max-size 32
cd palace-dev
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo run --release -q -p demo-headless-frame \
  --no-default-features -- --zarr /tmp/newvolim-cells3d-pyramid-rebuild.ome.zarr \
  --output /tmp/newvolim-cells3d-pyramid-rebuild.png --width 192 --height 144
```

The Python regression suite passed a 10:1 schedule (`[1,1,1]`, `[2,2,1]`, `[4,4,1]`,
`[8,8,1]`, `[16,16,2]`, `[32,32,4]`) and an odd-sized edge-chunk case. The generated real-pixel
cells3d pyramid rendered through Palace as a 192×144 RGBA PNG. Its source spacing is only mildly
anisotropic (0.29/0.26/0.26 µm), so the correct generated schedule reduces all axes together;
the 10:1 test is deliberately separate rather than asserting a false Z delay for that source.
This is a local uncompressed uint16 builder. Compressed inputs/outputs, labels, channels,
affines, arbitrary axis layouts, and source-registry wiring remain Stage-3 work.

## 2026-09-12: Palace single-array Zarr round trip

To exercise pixel IO without misrepresenting it as S1 data, a temporary 32³ procedural ball was
written with Palace's own Zarr writer and then rendered through `demo-headless-frame --zarr`.
That first exposed an abort in `palace_zarr::open_lod`: it unwrapped missing root-group metadata,
while the same project writes valid single-array data at `/array` without root metadata. The
reader now propagates this normal discovery error, allowing `open_or_create_lod` to use its
intended single-array fallback.

```sh
cd palace-dev
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo run --release -q -p convert --no-default-features \
  --features zarr -- --size-hint 32 --mem-size 256M --gpu-mem-size 256M --chunk-size 32 \
  ball /tmp/newvolim-synthetic.zarr single
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo run --release -q -p demo-headless-frame \
  --no-default-features -- --zarr /tmp/newvolim-synthetic.zarr \
  --output /tmp/newvolim-synthetic.png --width 128 --height 96
file /tmp/newvolim-synthetic.png
```

Both commands completed successfully; the result is a visually inspected red-ball 128×96 RGBA
PNG. This validates the local, pixel-bearing Palace Zarr-to-frame plumbing and the single-array
fallback only. That synthetic test alone does **not** validate OME-NGFF metadata, anisotropy,
multiscales, or representative microscopy data; the following cells3d run supplies that separate
S1 evidence.

## 2026-09-12: real local microscopy OME-Zarr fixture and Palace S1 run

`test-data/cells3d-anisotropic.ome.zarr` is now a committed pixel-bearing fixture built
by `scripts/prepare_cells3d_fixture.py`. It is a central 32×128×128 membrane-channel crop from
scikit-image's CC0 `cells3d` fluorescence-microscopy volume. The source's documented Z/C/Y/X
shape is 60×2×256×256 and physical voxel size is 0.29×0.26×0.26 µm. The fixture retains that
anisotropy, includes an uncompressed Zarr v3 three-level NGFF pyramid with factors 1×1×1,
1×2×2, and 2×4×4, and records the pinned source URL plus SHA-256 in its README.

```sh
python3 scripts/prepare_cells3d_fixture.py \
  --source /tmp/newvolim-cells3d.tif \
  --output test-data/cells3d-anisotropic.ome.zarr
cargo test -p newvolim-io
cd palace-dev
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo run --release -q -p demo-headless-frame \
  --no-default-features -- --zarr ../test-data/cells3d-anisotropic.ome.zarr \
  --output /tmp/newvolim-cells3d.png --width 192 --height 144
file /tmp/newvolim-cells3d.png
```

The IO suite passed 14 tests, including shape/chunk/axis/physical-transform checks for this
fixture. The desktop session test suite (5 tests) also opens it and reports its actual
`[x,y,z] = [128,128,32]` extent. Palace initialised Vulkan and produced a valid 192×144 RGBA PNG;
visual inspection showed the expected red membrane signal. This closes the core S1 question that
Palace's local Zarr path can render a real, physically anisotropic microscopy volume.

## 2026-09-12: Palace NGFF pyramid discovery

`palace-zarr::open_lod` now checks root-group NGFF attributes before its legacy hierarchy-prefix
and single-array fallback paths. For the first declared multiscale, it opens each declared dataset
path and applies its NGFF `scale` transformation as Palace physical spacing. The implementation is
deliberately limited to array-path discovery and scale vectors, which makes it a small candidate
upstream change instead of embedding application scene semantics in Palace.

```sh
cd palace-dev
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo test -p palace-zarr --no-default-features
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo run --release -q -p demo-headless-frame \
  --no-default-features -- --zarr ../test-data/cells3d-anisotropic.ome.zarr \
  --output /tmp/newvolim-cells3d-ngff-only.png --width 192 --height 144
```

The Palace regression test passed and verified all three declared NGFF levels plus their physical
spacing. The fixture has no `/array` fallback node, and the release renderer still produced a
valid 192×144 RGBA PNG. NGFF axis semantics beyond shape-preserving scale vectors, translations/
affines, labels, channels, multidimensional selections, and remote stores remain future work.

## 2026-09-12: local 3D camera controls (Stage 2)

`palace-frame` now owns the camera boundary as `CameraControls`: cumulative integral trackball
orbit and a finite multiplicative zoom constrained to 0.25 through 4.0. The Tauri host exposes
that only for an opened local dataset, and the CSR volume canvas coalesces pointer-drag and wheel
events for 50 ms before requesting a new frame. CSS layout size is converted to a physical-pixel
target before each request.

```sh
cd palace-dev
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo run --release -q -p demo-headless-frame \
  --no-default-features -- --zarr ../test-data/cells3d-anisotropic.ome.zarr \
  --output /tmp/newvolim-cells3d-camera.png --width 192 --height 144 \
  --orbit-x 120 --orbit-y=-80 --zoom 1.2
sha256sum /tmp/newvolim-cells3d-ngff-only.png /tmp/newvolim-cells3d-camera.png
```

The command initialised Vulkan and wrote a valid 192×144 RGBA PNG. Its SHA-256 was
`05af9d4de7b190f4cc84816405598fcf056342979a06abb7b45d80ed1c25a55c`, distinct from the
fitted-camera render (`3e2eef6abeb0eb87b27882e435b78ca663b5268bc9f302cc868fb97a89bc9201`).
`palace-frame`, `newvolim-desktop`, and the CSR release build all pass their focused checks.

The CSR now additionally uses a UI admission boundary around native frame requests: one Palace
render can be active and only the newest camera request is retained behind it. The existing 50 ms
input debounce reduces submissions; the admission queue prevents an already-slow native render
from turning a camera fling into an unbounded Tauri/PALACE backlog. `scripts/test_ui_admission.mjs`
loads the real inline script with a mocked renderer and proves that `first`, `second`, `third`
starts only `first`, then `third`; `node --check` validates the extracted inline script and a
release Trunk build passes. This matches the same newest-pending policy already used by
`newvolim-render` and `newvolim-server`.

## 2026-09-12: Palace orthogonal-frame transport (Stage 2 progress)

`palace-frame` now routes the deterministic volume through Palace's `sliceviewer` for XY, XZ,
and YZ frames. The desktop `render_synthetic_orthogonal_preview` command returns those three
PNG payloads to the corresponding CSR canvases alongside the existing 3D preview.

```sh
cd palace-dev
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo run --release -q -p demo-headless-frame \
  --no-default-features -- --orthogonal --output /tmp/newvolim-ortho \
  --volume-size 32 --width 96 --height 72
file /tmp/newvolim-ortho.xy.png /tmp/newvolim-ortho.xz.png /tmp/newvolim-ortho.yz.png
```

All three outputs were valid 96×72 RGBA PNGs. `cargo check -p newvolim-desktop` also passed.
This validates the sliceviewer-to-IPC data path for the deterministic input; shared crosshair
interaction and local-dataset orthogonal frames remain Stage-2 work.

## 2026-09-12: linked 2D crosshair transport (Stage 2 progress)

The CSR panes now share a click-driven `{x, y, z}` crosshair. A click in XY updates X/Y, XZ
updates X/Z, and YZ updates Y/Z; the desktop command renders all three Palace slices again at
that crosshair and the canvases overlay the linked cyan guides. The same command shape is wired
for authorized opened local Zarr datasets. Before Palace work starts, the desktop session derives
the level-zero X/Y/Z extent from NGFF metadata and clamps the untrusted IPC crosshair to it; it
also releases the session mutex before rendering. The fixture test keeps `[15,31,16]` unchanged
and clamps `[u32::MAX,128,32]` to `[127,127,31]` for the declared `128×128×32` cells3d volume.

```sh
PATH="$PWD/.tools/bin:$PATH" NO_COLOR=true trunk build --release  # from crates/newvolim-ui
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo test -p newvolim-desktop
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo clippy -p newvolim-desktop --all-targets --no-deps -- -D warnings
```

The release UI build, desktop tests, and strict desktop lint passed. Browser/webview click
automation remains verification work; normalized CSS-to-voxel mapping is derived from the
metadata extent in the active local UI path.

## 2026-09-12: Vulkan headless baseline (performance starting point)

The already-built release executable was timed directly (not through Cargo) on the synthetic 64³
ball at a 256×256 output target:

```sh
cd palace-dev
/usr/bin/time -f 'elapsed=%e seconds max_rss_kib=%M' \
  target/release/demo-headless-frame --output /tmp/newvolim-perf-direct.png \
  --volume-size 64 --width 256 --height 256
```

Result: Vulkan initialised two devices; direct runs measured **1.77, 1.96, and 2.02 seconds**,
with maximum resident set **322,624–323,028 KiB**. This includes Vulkan/runtime startup, volume generation, render,
readback, and PNG encoding, so it is a first-frame end-to-end baseline rather than steady-state
frame time. Repeated warm-frame, VRAM, and real-data measurements remain required before making
performance claims.

The same direct release executable was also measured on the representative local microscopy
fixture at a 256×256 target:

```sh
cd palace-dev
/usr/bin/time -f 'elapsed=%e seconds max_rss_kib=%M' \
  target/release/demo-headless-frame --zarr ../test-data/cells3d-anisotropic.ome.zarr \
  --output /tmp/newvolim-cells3d-perf.png --width 256 --height 256
```

It initialized Vulkan and completed the real-data cast/readback/PNG path in **1.91 seconds** at
**350,128 KiB** maximum RSS. This is a single cold first-frame reference, not a steady-state or
cross-machine performance claim; repeat/warm-frame timing, VRAM, image-diff quality, and request
drop measurements remain required before Stage 9 can be assessed.

## Workspace verification scope

`CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo test --workspace` currently cannot be the aggregate
verification command because the nested Palace fork's default video feature activates
`ffmpeg-sys-next`, whose build requires host `libavfilter.pc`. This environment does not provide
that system development package. The failure is outside newvolim's selected Zarr/frame feature
path; targeted root-package tests and Palace `--no-default-features` checks remain the supported
verification commands until Palace feature defaults are separated or the host package is added.

## 2026-09-12: remote OME-Zarr metadata boundary (Stage 3 foundation)

`newvolim-io` now exposes `read_remote_dataset_metadata`, which turns an allow-listed HTTPS Zarr
root into a bounded OME-NGFF metadata read. It treats a root without a trailing slash as a store
directory, requests `zarr.json` first, and uses `.zattrs` only after an explicit 404. The helper
rejects credentials, non-default HTTPS ports, query-bearing roots, fragments, unapproved hosts,
oversized bodies, and redirect destinations outside the policy. Redirect following remains
manual so every hop is re-authorized.

```sh
cargo test -p newvolim-io
```

All 13 IO tests passed, including the URL-resolution and unsafe-source checks. This is a secure
metadata boundary, not a remote chunk store: HTTP/S3 Zarr chunk access, credentials, quotas,
tenant caches, audit logging, and adversarial live HTTPS redirect fixtures remain Stage-3 work.

## 2026-09-12: newest-frame admission in the local frame service (Stage 4 progress)

The loopback frame service now uses a bounded dispatcher rather than accumulating tasks behind a
semaphore. One non-cancellable Palace request can remain active. While it runs, every WebSocket
connection retains at most its own newest pending request; an update from that connection replaces
only its prior pending request (`409 Conflict`). Up to eight independent pending sessions are
kept FIFO; a ninth receives `429 Too Many Requests`, as does a saturated small ingress buffer.
Stateless HTTP callers deliberately share one anonymous session. This makes newest-request-wins
concrete without allowing one socket client to evict another's pending view or pretending Palace
can interrupt a dispatched Vulkan render.

```sh
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo test -p newvolim-server
```

The frame request now also accepts optional `orbitX`, `orbitY`, and `zoom` fields and passes the
same bounded `CameraControls` to Palace as the desktop host. Omitted fields retain the fitted
camera (`0`, `0`, `1.0`), which is covered alongside overflow-safe frame sizing and
superseded-pending notification by the server tests. This extends the request contract; it
now has live transport evidence as well:

```sh
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo run --release -q -p newvolim-server -- \
  --bind 127.0.0.1:9904 --allow-root test-data \
  --dataset cells3d=test-data/cells3d-anisotropic.ome.zarr
curl -fsS http://127.0.0.1:9904/health
curl -fsS -X POST http://127.0.0.1:9904/v1/frame \
  -H 'content-type: application/json' \
  --data '{"dataset":"cells3d","width":192,"height":144,"orbitX":120,"orbitY":-80,"zoom":1.2}' \
  -o /tmp/newvolim-server-frame.png
file /tmp/newvolim-server-frame.png
```

The loopback release service returned health `{"status":"ok"}` and an authorized real-data
192×144 RGBA PNG. Debug Palace now detects a missing Vulkan validation layer and continues without
it, while reporting the fallback; release remains the configuration used for the actual service.
End-to-end concurrent HTTP/load testing, generation propagation into Palace shaders, progressive
passes, authentication/tenant authorization, and CPU performance measurement under load remain
Stage-4 work.

The service also now exposes a loopback WebSocket endpoint at `/v1/frames`. A client sends the
same camel-case frame request JSON as HTTP plus an optional `requestId`; the final reply is JSON
with `type: "frame"`, the echoed ID, `mimeType: "image/png"`, `progress: "final"`, and base64
PNG bytes. Invalid/non-text/oversized requests receive structured errors. Both transports share
the authorization, pixel budget, camera-control validation, and newest-pending dispatcher rather
than implementing two subtly different source policies.

```sh
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo run --release -q -p newvolim-server -- \
  --bind 127.0.0.1:9905 --allow-root test-data \
  --dataset cells3d=test-data/cells3d-anisotropic.ome.zarr
node /tmp/newvolim-websocket-frame-check.mjs
```

The loopback check sent the authorized cells3d request with `requestId: 73` and received the
expected final PNG frame envelope containing **8,430** valid PNG bytes. A separate forced-
llvmpipe check opened three WebSocket connections and submitted one 192×144 request to each while
the first was active; all three received final frames (`requestId` 1, 2, and 3) with measured
renderer times of 1105.4, 1015.8, and 997.8 ms. Thus pending views from separate connections are
not superseded. The connection is currently sequential and emits only a final Palace frame:
preview/refining passes, authentication/tenant authorization, and browser deployment hardening
remain Stage-4 work.

The CSR UI now has an explicit remote frame-server URL plus configured server-dataset-name
controls. It accepts only `ws:`/`wss:` URLs, sends physical canvas dimensions and bounded camera
controls, retains only the latest local camera request while a socket reply is active, and draws
the declared final `image/png` payload into the same 3D canvas used by the Tauri transport.
Opening a local source closes any remote socket, keeping the two data authorities separate.

```sh
# release CSR bundle at /tmp/newvolim-ui-remote-dist; both temporary services bind 127.0.0.1
sh /tmp/run-newvolim-browser-remote-frame.sh
```

With the release server on `127.0.0.1:9905`, headless Chromium filled the real UI fields, opened
the socket, and reached `Remote Palace frame #1`; its canvas backing store was **382×191 physical
pixels**. This is end-to-end browser CSR → WebSocket → authorized Palace frame → canvas evidence.
The WebSocket protocol now also accepts `"view":"orthogonal"` and returns three separately
base64-encoded final PNGs (`xyBase64`, `xzBase64`, `yzBase64`) from the same named-dataset
admission queue. Each reply also declares `voxelShapeXyz` and echoes its bounded
`crosshairXyz`; the server derives the shape from the level-zero NGFF axis metadata rather than
assuming storage order. The CSR client requests that reply after each remote volume frame, paints
the three linked canvases, and maps a click in any pane into the requested voxel coordinate.
Optional `x`, `y`, and `z` select the server-side crosshair when all are present. A fresh release
loopback run used a separate server at `127.0.0.1:9921` and Chromium CDP at `127.0.0.1:9927`; it
reached `Remote Palace volume + orthogonal slices #2 (3803.1 ms slices)` with volume, XY, XZ,
and YZ canvas backing stores all at `551×275` physical pixels. The same browser check dispatched
an XY click, requested `[15,31,16]`, and received `#3` with that exact echoed crosshair and the
fixture's declared `[128,128,32]` extent.
The server applies its 16 Mi-pixel admission budget to the aggregate three-pane reply, rather
than permitting each pane to consume the entire single-frame limit. A regression test accepts a
`2000×2000` orthogonal request and rejects `4096×4096` before Palace work begins.
TLS/authentication, multi-user session isolation, progressive passes, and non-loopback deployment
hardening remain required.

The chunk service no longer sends wildcard CORS headers. It is same-origin by default; a browser
deployment must pass one or more exact `--cors-origin https://viewer.example` capabilities, which
are installed through the server's CORS middleware for discovery and chunk routes alike.
The server regression invokes both `/v1/datasets` and a real configured dataset's `zarr.json`
through that middleware: the configured origin receives `Access-Control-Allow-Origin`, while an
untrusted origin does not. This verifies the browser chunk capability at the HTTP boundary rather
than only testing command-line parsing.

## 2026-09-12: physical point annotations in linked slices (Stage 8 progress)

The desktop session now owns the existing backend-neutral `Scene` annotations. The “Add point at
crosshair” control sends the linked `{x,y,z}` voxel coordinate to the native session, which maps
it through level-zero's NGFF axis order and coordinate transformations before storing an
`AnnotationGeometry::Point` in physical `[x,y,z]` space. Reopening a dataset clears the prior
scene because annotation coordinates from one dataset must not be silently applied to another.
The IPC reply includes a transient voxel projection only so the current XY/XZ/YZ slices can draw
a marker on the matching plane; the stored annotation remains physical.

```sh
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo test -p newvolim-desktop
cargo test -p newvolim-scene
cd crates/newvolim-ui && XDG_CACHE_HOME=/tmp/newvolim-trunk-cache \
  PATH="$PWD/../../.tools/bin:$PATH" NO_COLOR=true trunk build --release
```

The desktop session now also exposes `list_annotations` and `delete_annotation`; the CSR renders
the physical point list and can delete entries by their stable scene identifier. Deletion removes
the corresponding transient slice marker and requests a fresh slice render. Focused tests now
cover delete/re-delete behavior (desktop: 6 tests; scene: 5 tests), the release CSR bundle builds,
and the inline UI script passes `node --check` after extraction from `index.html`.

This still provides point placement plus 2D marker rendering only. Editing, durable session
storage, non-point geometry tools, depth-aware 3D compositing, and automated native-webview
interaction evidence remain Stage-8 work.

## 2026-09-12: browser WebGPU canvas path (Stage 7 progress)

The CSR shell now takes a browser-only route when Tauri's native IPC is absent: it requests a
WebGPU adapter/device, configures the same 3D canvas at physical device-pixel resolution, and
submits a WGSL full-screen ray-marched synthetic-volume render pass. Native desktop operation
continues to use the Palace PNG transport and the orthogonal panes; this browser path is clearly
limited to a deterministic in-shader volume and does not claim browser OME-Zarr support.

```sh
cd crates/newvolim-ui && XDG_CACHE_HOME=/tmp/newvolim-trunk-cache \
  PATH="$PWD/../../.tools/bin:$PATH" NO_COLOR=true trunk build --release
```

The release CSR bundle passed. A loopback Trunk server plus headless Chromium (`--enable-unsafe-webgpu
--enable-features=Vulkan`) also reached the application's “Browser WebGPU synthetic-volume frame”
state, demonstrating adapter/device acquisition, WGSL pipeline creation, a render pass, and queue
submission. Chromium reported a headless swap-chain `SharedImageBackingFactory` warning, so this
is execution evidence rather than a visual-presentation benchmark.

The browser now also has a direct OME-Zarr **chunk preview** route. Given a same-origin or
CORS-enabled HTTP(S) root, it reads the v3 root/array metadata, checks a single three-axis Z/Y/X
multiscale and little-endian `uint16` bytes codec, fetches `c/0/0/0`, and uploads the decoded
actual voxel payload into page zero of the four fixed 4 MiB WebGPU storage pages. Raw chunks and
one Zstd, LZ4, or Blosc codec are admitted through the production Rust wasm exports. The
representative `numcodecs` LZ4 encoder emits the same little-endian size-prefixed framing as the
decoder corpus; the browser validates the resulting decoded byte count against the chunk shape.
WGSL reads the packed uint16
data while rendering; the other fixed pages and a uniform chunk descriptor remain bound,
preserving the chosen portable resource model. A loopback-only static route served the release
bundle and the real cells3d fixture to headless Chromium. The UI selected
`http://127.0.0.1:9908/test-data/cells3d-anisotropic.ome.zarr/` and reached
`Browser WebGPU OME-Zarr chunk frame (packed page table, four fixed storage pages)` on a
**382×191** physical canvas.

The direct preview now enforces both sides of that byte boundary. It streams each compressed
response with a 16 MiB encoded-transport cap, then passes the declared raw chunk size to bounded
Rust/WASM Zstd, LZ4, and Blosc decoders. Zstd is read incrementally and stops before extending
its output past the cap; LZ4 and Blosc reject their self-described output size before allocating.
The decoder regression proves all three reject a 16 KiB payload under a 1 KiB budget, and the
release Trunk artifact exports the bounded bindings used by the page. This closes the browser
preview's decompression-bomb gap; it does not create a general multi-tenant remote-source quota
or replace server-side authorization.

The same check then served a Zstd level-3 encoding of that real fixture chunk as a v3 NGFF array.
The rebuilt CSR bundle fetched its metadata and compressed `c/0/0/0`, called
`decode_zstd_chunk` in the SIMD wasm module, and reached the identical WebGPU chunk-frame status
at **382×191**. This proves the wired decode-and-render path, not merely an isolated decoder
benchmark.

The same fixture was encoded with `numcodecs`' LZ4 codec and served as a separate v3 NGFF array.
Its size-prefixed LZ4 block reached `decode_lz4_chunk` in the rebuilt wasm bundle and produced the
same final WebGPU chunk-frame status at **382×191**. A final `numcodecs` Blosc-LZ4/byte-shuffle
encoding likewise reached `decode_blosc_chunk` and the same final WebGPU status. The browser
direct-preview evidence therefore covers raw, Zstd, LZ4, and Blosc data on the representative
fixture.

The direct-preview control now accepts a `Z,Y,X` chunk coordinate instead of hard-coding the first
chunk. It validates the coordinate against the selected array's three-axis shape and chunk grid,
uses the logical edge extent when applicable, and then requests the corresponding canonical v3
chunk key. Chromium loaded the real uncompressed fixture coordinate `1,2,3`; the loopback server
recorded `GET …/0/c/1/2/3` and the page reached the chunk-frame state at **383×191**. Array-axis
order is respected while mapping the user-facing Z/Y/X control, but arbitrary non-three-axis
layouts remain intentionally outside this preview's scope.

Its physical extent now composes every declared `scale` transformation at both the multiscale and
dataset scopes in order, while accepting finite translations (which do not change extent).
Reordered X/Y/Z axes are mapped back to the renderer's X/Y/Z spacing explicitly. An inline UI
regression composes multiscale `[10,2,1]` and dataset `[0.5,4,5]` scales on X/Y/Z axes into
renderer spacing `[5,8,5]`; it also proves an affine transform fails explicitly instead of being
silently misrepresented as scalar spacing. Arbitrary affine browser rendering remains outside
this compact preview until the WebGPU camera consumes the full transform graph.

The control also now selects a numeric dataset index from that multiscale after validating it
against the declared dataset list. A loopback Chromium run selected level `1` and coordinate
`1,1,1`; the server recorded `GET …/1/zarr.json` and `GET …/1/c/1/1/1`, then produced the browser
chunk frame at **551×275**. This remains a one-chunk preview rather than an LOD chooser or a
resident multi-brick volume renderer.

## 2026-09-12: browser chunk-server boundary (Stage 4/7 progress)

`newvolim-server` now exposes a deliberately narrow browser chunk route:

```text
GET /v1/datasets/{configured-name}/zarr/{relative-asset}
```

Operators opt in with `--dataset NAME=PATH`; each path must first pass the existing
`--allow-root` canonicalization policy. Browser clients see only the stable name, not a local
filesystem path. The server rejects unknown names, empty/absolute/traversal asset paths, symlink
escapes after canonicalization, non-files, and assets over 16 MiB. It marks `zarr.json` as JSON,
chunks as octet-stream, and emits `Access-Control-Allow-Origin: *` for this unauthenticated
loopback/development boundary. Production authentication, tenant isolation, credentials, range
requests, cache policy, and configurable CORS are deliberately still required.

`GET /v1/datasets` provides CORS-enabled deterministic discovery of configured names only; the
loopback fixture returned `{"datasets":["cells3d"]}` without disclosing its filesystem path.

The CSR UI now has a chunk-server origin and configured-dataset-name control alongside the direct
Zarr URL control. Discovery validates the returned name schema and auto-fills exactly one result;
selection constructs the named `/v1/datasets/{name}/zarr/` root before invoking the ordinary NGFF
loader. A loopback Chromium test exercised discovery (`cells3d`), selection, level-1/chunk-`1,1,1`
loading through CORS, wasm decode dispatch, and the fixed-page WebGPU frame at **551×275**.

The browser page pool now carries a real 2×2 XY neighborhood instead of using page zero for one
chunk and filling the remaining fixed bindings with proof values. It requests the selected chunk
plus +X, +Y, and +X/+Y neighbors when in bounds (out-of-bounds pages are zero-filled), decodes
each independently, and selects the correct static binding in WGSL using the ray's brick
coordinate. The chunk-server Chromium check selected level 0 / `Z,Y,X=1,1,1` and asserted that
the four loaded keys were `c/1/1/1`, `c/1/1/2`, `c/1/2/1`, and `c/1/2/2` before the **551×275**
frame was submitted. This is a bounded four-brick preview window, not yet a page table, a general
residency manager, or a full raycaster.

Browser pointer/wheel camera controls now apply to that direct WebGPU preview as well as to native
and remote frame transports. The chunk-info uniform carries orbit and zoom through the WGSL ray
construction; camera input rerenders the resident four-page window without refetching chunks. The
browser caches one adapter/device for successive frames and clears only the cached device on its
`lost` promise, which avoids Chromium refusing a second adapter request. In the loopback test,
after the four-key load, an orbit of `(37,-19)` and zoom `1.4` submitted another chunk frame at
**551×275** with the same four resident keys.

The fixed-page browser shader now performs a bounded 48-step front-to-back volume ray march
through the sampled sphere, alpha-compositing real `uint16` densities (or the synthetic fallback)
and terminating early at high opacity. The named-server Chromium run compiled and submitted this
pipeline for all four real chunks, then repeated it after the camera update. It is therefore a
small real client volume raycaster, while general multi-brick 3D residency, per-level LOD choice,
transfer functions, composed transforms, and production performance tuning remain Stage-7 work.

The browser ray marcher now reads the selected dataset's three-axis NGFF `scale`, validates it,
maps it from declared axis order into X/Y/Z, and intersects a physically proportioned box instead
of assuming a cubic volume. The `cells3d` level-zero scale (`Z,Y,X = 0.29,0.26,0.26` µm) was used
in the four-chunk Chromium ray-march/camera check. Arbitrary composed coordinate transforms and
cross-level registration remain in the CPU transform layer and are not yet consumed by this
bounded WebGPU preview.

The browser preview now consumes up to four OME `omero` channels' six-digit sRGB colours and
display `window.start`/`window.end` values. For a declared `C` axis with `chunk_shape[C] == 1`,
it fetches one spatial chunk per channel into the four fixed storage pages, validates the
intervals, converts colours to linear light, and additively front-to-back composites enabled
channels. An inactive OMERO channel stays in the fixed binding layout with zero opacity. Absent
or invalid metadata gets an explicit red/full-`uint16` fallback. Its sRGB transfer pair now uses
the standard piecewise IEC sRGB curve, matching the native WGPU colour attachment rather than
the former gamma-2.2 approximation. General layer/session request binding, channel chunks wider
than one, and more than four browser channels remain future work.

The browser raymarch now first renders at physical canvas resolution into an `rgba16float` linear
target. A separate presentation pass loads that target and applies standard sRGB encoding to the
preferred canvas format. This gives browser rendering the same colour-target boundary as the
native packed RGBA16Float proof, rather than making canvas encoding part of volume compositing.
The same volume pass also writes a second `r32float` attachment: physical camera-ray distance to
the first sample that crosses the opacity threshold, with `+∞` for no hit. A canvas click now
copies the selected physical pixel to a 256-byte-aligned map-read buffer and reports either its
finite physical distance or no opacity hit. That exercises the attachment as real readback data
and creates the browser picking diagnostic seam. It is not yet an annotation-composite pass, GPU
ID-buffer picker, or depth-correct annotation renderer.

## 2026-09-12: dataset-bound annotation documents (Stage 8 progress)

The desktop annotation panel now exposes explicit export/import actions for a user-supplied JSON
path. The version-1 document contains only the canonical dataset root and stable physical
annotations—never voxel coordinates or renderer state. Import rejects a missing opened dataset,
an incompatible version, a different canonical root, malformed/non-finite/degenerate geometry,
duplicate IDs, and ID exhaustion. It constructs a replacement scene first, so a failed import
does not disturb the active annotations; a successful import advances the next stable ID.

The session regression test writes one physical point, exports it, imports it into a fresh session
for the same dataset, and verifies the point plus next ID. It also proves the identical document
is rejected after opening a different root. For a point annotation whose level-zero affine is
invertible and in bounds, import reverses that transform and restores its nearest-voxel 2D marker.
The physical coordinate remains authoritative; non-point, singular, or out-of-bounds annotations
remain in the desktop list without a misleading marker.

`newvolim-scene` now gives `LayerKind::Labels` an explicit sparse `LabelPalette`: every declared
integer label has an sRGB color and opacity, duplicate IDs and invalid opacities are rejected,
and undeclared labels resolve to transparent rather than receiving an accidental pseudo-colour.
`Layer::labels` attaches the palette without image-channel state, preserving the distinction
between semantic segmentation IDs and intensity channels. This is backend-neutral label-volume
display intent; decoding/rasterizing an actual label array and depth-compositing it with volume
samples remain renderer work.

The palette now also exposes that intent as deterministic straight-alpha sRGB `[r,g,b,a]` for a
sampled label value: configured opacity is rounded to the eight-bit transport value and unknown
IDs become `[0,0,0,0]`. This is the exact lookup contract for a future label-volume shader or
CPU preview; it does not yet decode or composite a label array.

The display projection now extends to imported polyline and polygon vertices as well. The desktop
returns a transient `voxelPoints` array only after every physical vertex inverts through the
level-zero NGFF affine and passes bounds checks. The 2D canvas draws a point, open stroke, or
closed polygon when all its vertices lie in the selected XY/XZ/YZ plane; slanted geometry remains
listed but is not falsely rendered as a plane intersection. A regression uses an anisotropic,
translated transform to recover a two-vertex physical segment as `[4,6,2] → [5,7,3]`.

The desktop now also authors polygons rather than only importing them. “Start polygon ROI” opens
an XY draft; slice clicks collect bounded level-zero vertices on that one plane, and “Finish
polygon ROI” requires at least three before invoking the native session. The host limits a
polygon to 4,096 vertices, checks every voxel point against the opened array, and transforms each
one through the NGFF level-zero affine before persisting `AnnotationGeometry::Polygon`. The
session regression verifies the exact anisotropic/translated physical vertices and their inverse
display projection; the UI regression locks in the native command and same-plane draft guard.
This is still a 2D authoring/overlay path, not a claim of depth-correct 3D annotation compositing.

That projection now includes the new ROI forms too: rectangles cross the desktop boundary as
their four physical corners and ellipses as a deterministic 32-point physical perimeter, rather
than as their centre and radius handles. The linked-slice canvas closes `Rectangle` and `Ellipse`
outlines just as it closes polygons. The session regression confirms an anisotropic transformed
rectangle reaches `[3,5,2]`, `[5,5,2]`, `[5,7,2]`, `[3,7,2]`; the UI admission regression checks
the resulting rectangle stroke is actually closed. This is intentionally 2D slice presentation,
not a substitute for the planned 3D depth-correct annotation renderer.

`newvolim-render` now also turns visible persisted annotations into backend-neutral 3D overlay
packets: physical-space point sprites, line segments, and filled triangles, with a stable
annotation ID retained for future picking. Polylines remain open; polygons retain their closing
boundary and use ear clipping to emit `n−2` triangles only when all vertices are coplanar in
physical space. Concave coplanar ROIs therefore gain a real mesh packet, while non-planar,
degenerate, or self-intersecting geometry remains outline-only rather than receiving an invented
triangle fan. Point
and line radii are validated physical units, so anisotropic data cannot be silently treated as
cubic before a backend expands them into camera-facing raster primitives. Eight render-contract
tests cover physical coordinates, concave tessellation, non-planar/self-intersecting fallback,
visibility, and invalid styles. A ninth test now covers `PickRay` selection: points are spheres,
segments are capsules, and filled packets are two-sided triangles; the nearest hit wins with a
stable-ID tie-break. Its `max_ray_distance` is exactly the render contract's first-opacity depth,
including `+∞` for no volume hit, so callers can reject occluded annotations consistently.
This is preparation for depth-aware GPU compositing—not a claim that the current Vulkan PNG path
has 3D annotation rasterization or GPU ID-buffer picking.

The shared annotation vocabulary now also has physical-space `Rectangle` and `Ellipse` ROI
forms. Each records a centre plus two non-zero, non-collinear physical half-axis/radius vectors,
so oblique ROIs do not collapse into voxel-aligned shapes. The renderer expands rectangles to
four outline segments plus two triangles and ellipses to a deterministic 32-segment outline and
30-triangle mesh; the desktop projection validates the centre and defining extremities before
exposing transient voxel markers. The new scene and renderer regressions cover oblique anisotropic
axes and degenerate-basis rejection. This still supplies primitive intent only: a wgpu backend
must perform the actual depth-correct rasterization and ID readback.

## 2026-09-12: browser four-brick timing (Stage 9 progress)

The production browser UI records `performance.now()` around the named-server NGFF load and around
the debounced resident camera rerender. In one loopback Chromium run using the four level-zero
cells3d chunks, it measured **56.7 ms** from source request to submitted chunk frame and **51.9
ms** from camera scheduling to the resident-page rerender. The latter includes the intentional
50 ms input debounce, so it is an interaction-latency measurement rather than GPU time. These are
headless Linux/loopback values, not a production network or cross-platform budget; the test now
requires both finite timings while also asserting the four chunk keys, OME transfer metadata, and
camera state.

`scripts/benchmark_frame_server.py` now makes the server-side part repeatable. It sends named
frame requests, requires an `image/png` response with the PNG signature, parses the server's
`Server-Timing: render;dur=…` metric, and reports min/median/p95/max separately from client
end-to-end wall time. A fresh release-server run forced to Mesa llvmpipe on the real cells3d
fixture at 192×144 measured renderer min/median/max **1131.331/1154.774/1219.788 ms** and
end-to-end **1133.030/1168.418/1221.313 ms**; all three were 12,636-byte PNGs. This is a small
CPU-fallback baseline, not a GPU, concurrent-load, or production-network claim.

## 2026-09-13: refreshed release frame-server CPU baseline (Stage 9 progress)

After the CORS-router refactor, a fresh `--release` server was run against the same real cells3d
fixture, forced to Mesa llvmpipe, at `192×144` for three sequential named requests. The PNG size
remained 12,636 bytes. Renderer min/median/p95/max were
**1170.245/1246.451/1253.903/1253.903 ms**; loopback end-to-end values were
**1172.001/1255.427/1258.946/1258.946 ms**. The server was then stopped. This confirms that the
current routing and origin-policy changes did not prevent the CPU-only fallback from producing
valid Palace PNG frames, while remaining a deliberately slow baseline rather than a throughput,
concurrency, GPU, or cross-platform performance result.

```sh
env VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json \
  target/release/newvolim-server --bind 127.0.0.1:9916 --allow-root test-data \
  --dataset cells3d=test-data/cells3d-anisotropic.ome.zarr
python3 -B scripts/benchmark_frame_server.py \
  --url http://127.0.0.1:9916/v1/frame --dataset cells3d --requests 3 \
  --width 192 --height 144
```

```sh
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo run -q -p newvolim-server -- \
  --bind 127.0.0.1:9910 \
  --allow-root test-data \
  --cors-origin http://127.0.0.1:8080 \
  --dataset cells3d=test-data/cells3d-anisotropic.ome.zarr
```

The live check fetched a real 16 KiB `0/c/1/2/3` chunk with the expected binary content type and
CORS header naming its explicitly admitted CSR origin, while a traversal-shaped request did not
succeed. A second loopback check served the CSR UI from `http://127.0.0.1:8080` and this API from
another origin; Chromium used
`http://127.0.0.1:9910/v1/datasets/cells3d/zarr/` as the browser OME-Zarr root, fetched level 1
and chunk `1/c/1/1/1` through CORS, then reached the WebGPU chunk-frame state at **551×275**.
This establishes the browser-to-server chunk seam without conflating it with the remote PNG frame
service.

Boundary chunks now preserve the distinction between their logical cropped extent and their
regular-grid stored stride. The preview admits the configured full stored chunk byte count, maps
sample positions within the logical extent, and indexes page-zero with the full X/Y stride. An
odd-size generated NGFF array (`9×33×34`, chunks `8×32×32`) supplied its padded far edge
`0/c/1/1/1` (16 KiB); loopback Chromium fetched that exact key and rendered the chunk frame at
**551×275**. This matches the local writer's declared zero-fill padding policy.

This is real browser NGFF metadata/chunk loading, codec decoding, and GPU consumption, not a claim
of a complete client raycaster. Arbitrary
multiscales/axes/chunk selection, more codecs and their Zarr framing, cross-origin deployment
policy, browser orthogonal slices, and the full Rust wgpu renderer remain Stage-7 work.

## 2026-09-12: browser WebGPU capability floor (Stage 6 progress)

The CSR shell now exposes `?webgpu-capabilities-smoke`, a query-gated probe that waits for CSR
mounting, requests the real browser adapter, and reports the limits which constrain the bounded
brick-pool design. A release Trunk build was served on loopback and queried through Chromium CDP
with the same `--enable-unsafe-webgpu --enable-features=Vulkan` flags used for the Stage-7 render
smoke. The reported default WebGPU limits were:

| limit | Chromium / Linux browser result |
|---|---:|
| `maxTextureDimension3D` | 2048 |
| `maxBufferSize` | 1 GiB |
| `maxStorageBufferBindingSize` | 1 GiB |
| `maxStorageBuffersPerShaderStage` | 10 |
| `maxBindGroups` | 4 |

This is an actual browser-adapter floor, not a claim about native Vulkan limits or all browser
implementations. It confirms that the Stage-7 bounded/static page-pool direction fits the browser
model; Apple Silicon and Windows/D3D12 still require the reusable native `volim-gpuprobe` runs
before S6 can be closed.

## 2026-09-12: portable brick-residency state machine (Stages 6/9 progress)

`newvolim-render` now contains the backend-neutral residency model used by the selected portable
page-pool design. A `BrickKey` carries the full `{timepoint, channel, level, xyz}` identity; a
`BrickLocation` packs a static-page index and 20-bit word offset into a `u32`, matching the
native and browser four-page capability proofs without a device address or bindless array.

`BrickPool` has a fixed `1..=4`-page shape, explicit coarse-brick pinning, draw-time LRU touches,
and deterministic eviction of only non-pinned bricks. Therefore an exhausted pool either evicts
the least-recently-used refinement brick or reports that all slots are pinned; it never evicts the
coarse fallback required for progressive rendering. The model deliberately owns no GPU upload or
shader page table yet—those are the remaining Palace wgpu backend work—not a second renderer.

```sh
cargo test --offline -p newvolim-render
cargo clippy --offline -p newvolim-render --all-targets -- -D warnings
```

All seven tests and clippy passed. They cover portable page/offset packing, distinct timepoint and
channel cache keys, LRU replacement while preserving a pinned coarse brick, and invalid/all-pinned
pool states.

## 2026-09-12: Palace-owned synchronization vocabulary (Stage 5 foundation)

`palace-core::gpu` now defines owned `Stage` and `Access` bitflags for the stage/access semantics
currently used by Palace operators. The Vulkan backend contains the only conversion to
`ash::vk::{PipelineStageFlags2, AccessFlags2}`, with an exact-mapping regression test. Fifty-seven
production barrier constructions now go through the transitional
`SrcBarrierInfo::from_semantics`/`DstBarrierInfo::from_semantics` constructors: the chunk-request
read barrier, JIT input read/write barrier, and two JIT output-write barriers. The central
raycaster migrates its eleven barrier constructions too: render-target transfer to
fragment access, fragment-to-compute repair, final compute writes, entry/exit/transfer reads, and
request/use-table synchronization. Sliceviewer now migrates its six compute/transfer request and
final-write barriers as well. The transfer-function operator's input/table access and output write
barriers now use the same owned vocabulary, eliminating its direct `ash::vk` import. Imageviewer
now does the same for its input read, request/use-table synchronization, and final-write barriers;
its only remaining direct Vulkan use is the image-size sentinel.
The planned wgpu-native `rechunk` and `resample` operators now migrate their two and five
constructions respectively (input reads, page-table transfer-to-compute visibility, and output
writes), eliminating their direct Vulkan imports entirely.
The pure-compute `conv`, `geometry`, and `procedural` operators also migrate their six input-read
and output-write constructions and no longer import Vulkan. This is still an incremental sync
vocabulary migration, not a claim that unrelated operators or backend-internal commands are
portable yet.
`aggregation`, `slice`, `splitter`, and `vesselness` add fourteen more constructions, while the
raw-volume source's transfer-write completion adds one and correctly retains Vulkan for its actual
buffer-copy command. The only remaining direct operator sync literals are in GUI and Random
Walker; the latter is explicitly Vulkan-only outside the first portable viewer scope.
`SrcBarrierInfo`/`DstBarrierInfo` fields and backend-internal call sites still carry Vulkan flags,
so this is deliberately not a claim of a complete backend abstraction. It creates a verified seam
for incremental migration toward a wgpu backend that does not import `ash` merely to describe
resource intent.

That same owned module now also defines the first portable feedback-table contract for the wgpu
path: `PortableFeedbackKey` packs a 24-bit chunk index plus a 7-bit level into a non-sentinel
`u32`, while `PortableFeedbackTable` models fixed linear probing and explicit dropped feedback.
The model preserves the original request/use-table intent—deduplicate work, accept a bounded
collision loss, and retry a missing brick on a later frame—without requiring 64-bit shader
atomics. It is CPU-only scaffolding at this point; the existing Vulkan `u64` feedback buffers and
GLSL remain unchanged until a WGSL recorder consumes this contract.

```sh
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-core --lib \
  'gpu::tests::backend_neutral_flags_preserve_combined_usage'
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-core --lib \
  'vulkan::sync_mapping_tests::owned_sync_vocabulary_maps_exactly_to_vulkan_flags'
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-core --lib 'gpu::tests'
```

## 2026-09-12: focused product-suite verification

The viewer product path passes its explicit offline suite: 65 unit tests across `newvolim-io`,
`newvolim-scene`, `newvolim-render`, `newvolim-desktop`, `newvolim-server`,
`newvolim-decode-bench`, and `newvolim-wgpu-frame` (plus their doc tests) passed via:

```sh
cargo test --offline --workspace
cargo clippy --offline --workspace --all-targets --no-deps -- -D warnings
```

The root workspace explicitly excludes the nested `palace-dev` workspace, so normal newvolim
checks do not build its unrelated optional `palace-video` package and do not require its missing
system `libavfilter.pc`. Palace remains a path dependency and its selected renderer crates still
compile as part of these checks; its full video-enabled workspace continues to be verified
separately when that system dependency is available.

Both focused tests passed. The full Palace GPU test suite still cannot run under the debug build
on this host because it unconditionally asks for the absent `VK_LAYER_KHRONOS_validation` layer;
that is a pre-existing environment limitation and separate from this CPU-only mapping change.

The release `demo-headless-frame` was rebuilt after the raycaster migration and rendered the
committed cells3d fixture twice at 192×144. Both Vulkan-initialized runs produced valid RGBA PNGs
with identical SHA-256
`3f6a7b799d4fe6e5b55d20b6c477df82c4beb81b43470ac732b3397db7637489`. This is a repeatability
check of the migrated build, not a cross-version image-diff claim: the older stored reference
predates other rendering changes in this working tree.

## 2026-09-12: measured frame-server render duration (Stage 9 progress)

The frame server now measures only the Palace render/PNG-production interval in its blocking
worker, excluding dispatcher wait, HTTP/WebSocket transfer, browser image decode, and display. It
emits that number as standard `Server-Timing: render;dur=…` on HTTP frames and required
`renderMs` in final WebSocket frame envelopes. The CSR validates the latter and displays it rather
than treating client-side elapsed time as GPU time.

In a release loopback request for the named real `cells3d` dataset at `192×144`, the HTTP PNG
response contained `Server-Timing: render;dur=1781.217`. A rebuilt CSR bundle then reached
`Remote Palace frame #1 (1638.8 ms render)` through Chromium → WebSocket → server → canvas at
`551×275` physical pixels. These are single-host, first-frame diagnostic values, not a production
latency budget or a cross-platform benchmark.

```sh
cargo test --offline -p newvolim-server
cargo clippy --offline -p newvolim-server --all-targets --no-deps -- -D warnings
```

Both passed; the frame-envelope unit test asserts the timing field in addition to the explicit
final-PNG contract.

## 2026-09-12: bounded native HTTPS Zarr assets (Stage 3 progress)

`newvolim-io` now exposes `RemoteZarrStore`: a capability-scoped HTTPS OME-Zarr root built from
the existing credential-free host allow-list. It derives only non-empty normal relative asset
paths, rejects query/fragment/percent-encoded/backslash traversal forms, and fetches each asset
under a caller-selected byte budget (64 MiB default). The generic fetch helper disables automatic
redirects and re-authorizes every redirect target, retaining the prior SSRF boundary for chunks
as well as metadata. `RemoteZarrStore` now also provides the same v3-first/v2-fallback root and
array metadata reads as the local boundary, so remote metadata cannot bypass the store's URL and
size checks. The local and remote paths share one transport-independent Zarr v3 parser, with a
regression test for the exact root and array shape/chunk interpretation.

`SourceRegistry` now gives application callers one explicit, capability-carrying choice between
`DatasetSource::Local` (canonicalized below configured roots) and `DatasetSource::Https`
(allow-listed and byte-bounded). Both variants expose the same root/array metadata interface, so
later renderer integration cannot choose a less-protected metadata path by accident. The registry
is intentionally outside Palace: its currently locked `zarrs` version is not compatible with the
locally cached `zarrs_opendal` remote-store adapter without a separate dependency upgrade.

The HTTPS leg alone did not establish complete remote-source coverage: SSH-agent sources,
decompression quotas, tenant cache isolation, audit/revocation, and wiring readers into Palace's
tensor-source abstraction remain work.

`newvolim-io` now also has an application-owned S3 source boundary. A caller adds exact bucket
and credential-profile names to `S3SourcePolicy`; `SourceRegistry::open_s3` admits only that
pair, a normal root-scoped prefix, and an optional region. It constructs OpenDAL's S3 operator
with the selected profile, checks object metadata against the configured byte cap before reading,
and also range-reads at most cap-plus-one bytes so a changed object cannot turn the metadata
check into an unbounded allocation. It exposes the same v3/v2 metadata, array-info, and raw-asset
API as local and HTTPS sources.
Credentials remain in the selected AWS provider chain and are never passed through Palace or
stored in Newvolim. The focused test proves bucket/profile/prefix rejection and rejects an
invalid asset before network I/O. A live authenticated bucket test still requires a deliberately
provisioned test bucket and profile; S3 publishing, credential rotation/audit, tenant cache
isolation, decompression quotas, and SSH-agent transport remain separate work.

`palace-io` now has that upstreamable tensor-source dispatch seam: `SourceRegistry` accepts
runtime-registered `TensorSource` implementations that return Palace's existing single-level or
LOD `TensorOperator` values. `SourceRegistry::with_local_paths()` preserves the historical
extension-based local-path behaviour, while a bare registry accepts no URL implicitly; an
application must register its already-authorized HTTPS, S3, or SSH provider explicitly. The
selection test proves first-match registration and rejects an unregistered `s3://` specification.
This is deliberately not a second chunk-source trait and does not yet wire `newvolim-io`'s HTTPS
reader into an out-of-core Palace operator.

```sh
cargo test --offline -p newvolim-io
cargo clippy --offline -p newvolim-io --all-targets -- -D warnings
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-io
```

All 19 tests and clippy passed. The regression tests verify a normal derived chunk URL, reject
traversal-shaped assets (including array metadata paths) before network access, enforce non-zero
budgets, reject unsafe HTTP before a request is attempted, and ensure the registry cannot open an
out-of-root local source or an unallow-listed HTTPS source.

`DatasetSource` now also exposes the same bounded `fetch_asset` operation for an already
authorized local or HTTPS store. Local reads use the identical normal-relative asset constraint
and a 64 MiB cap; they canonicalize the target to reject both textual traversal and symlinks that
escape the authorized root, and report an explicit byte-limit error rather than allocating
unbounded chunk data. This is the raw-asset seam needed by a native
wgpu chunk loader. It deliberately does not claim codec decoding, Zarr chunk-key interpretation,
or tensor-operator integration: those layers must retain their own shape, dtype, and resource
budget validation. The focused IO suite now has 20 passing tests, including local asset success,
traversal/symlink rejection, and cap enforcement.

`newvolim-io` now builds on that asset boundary with `read_v3_raw_u16_volume`: a bounded
multi-brick reader for the intentionally narrow portable Zarr v3 subset used by the fixture.
It requires a regular three-axis `Z,Y,X` grid, default slash-separated `c/z/y/x` keys, a single
little-endian `bytes` codec, and `uint16` dtype. It validates the final allocation before
allocating, validates every full chunk length, and assembles chunks into row-major `Z,Y,X` voxels;
compressed codecs and other key/layout schemes fail explicitly rather than being treated as raw
pixels. The fixture regression assembles all 64 level-zero cells3d chunks into `32×128×128`,
checks both the first chunk and the next-Z-chunk boundary, and verifies that a 1 KiB output cap
rejects the 1 MiB volume before allocation. A separate compressed-codec fixture proves `zstd`
is rejected from metadata before any chunk payload is read. The focused IO suite now has 22
passing tests and strict clippy passes. This is the multi-brick data layer for a native wgpu
loader, not yet its renderer integration.

## 2026-09-13: anisotropic raw-v3 pyramid writer (Stage 3 progress)

`newvolim-io::write_v3_raw_u16_pyramid` now turns an in-memory level-zero `uint16` volume into
a valid local OME-NGFF Zarr v3 hierarchy. It uses the existing physically-aware per-level
schedule, box-averages every level directly from level zero, writes complete padded regular
chunks in the supported little-endian `bytes` layout, and records a separate Z/Y/X scale for
each level. It refuses malformed volumes, zero chunk dimensions, non-physical spacings, and a
non-empty destination, so generating a pyramid cannot silently overwrite an existing store.

The round-trip regression starts with a two-Z, two-Y, four-X volume and 10:1 Z spacing. It
produces cumulative factors `[1,1,1] → [2,2,1]`, verifies the resulting physical transform
`[10,2,2]`, reopens both levels through the same authorized source boundary, checks edge chunk
padding, and checks the correctly rounded box means. `cargo test --offline -p newvolim-io` now
has 23 passing tests and strict clippy passes. This is intentionally a deterministic,
in-memory raw-data builder: it is not yet a streaming TB-scale writer, codec encoder, atomic
publication protocol, or a remote/S3/SSH writer.

`newvolim-pyramid` exposes that builder as a reproducible local command. It accepts an explicit
local `--input` and empty `--output`, derives the level-zero Z/Y/X spacing from a diagonal NGFF
transform (and explicitly rejects rotation or shear it cannot faithfully rewrite), retains the
input raw-v3 chunk shape, and accepts `--max-size N` (256 by default). A real run was:

```sh
cargo run --offline -p newvolim-io --bin newvolim-pyramid -- \
  --input test-data/cells3d-anisotropic.ome.zarr \
  --output /tmp/newvolim-pyramid.7ykZXu --max-size 32
```

It emitted levels with scales `[0.29,0.26,0.26]`, `[0.58,0.52,0.52]`, and
`[1.16,1.04,1.04]`. The native WGPU reader then rendered emitted level 2 as a nonempty
`32×32` PGM with SHA-256
`9d930c3af5e4cd97c26ee42c411dbc3c5b9426e22eb551d29175f26b380ed294`.

## 2026-09-12: native WGPU local-data frame (Stage 6 progress)

`newvolim-wgpu-frame` is now a newvolim-owned native command-line presentation path. It creates
an explicit local source policy for the requested dataset, or accepts an HTTPS root only with one
or more explicit credential-free allow-listed hosts, then uses `read_v3_raw_u16_volume` to
assemble the selected raw v3 level, expands its `uint16` voxels to portable `u32` words, and
raymarches them through a four-page, 16 MiB statically bound storage pool and packed page table
selected by S7. Each page owns a contiguous range of words; its table entries pack the actual
page index and page-local offset. It reads the compute output back and writes a viewable binary
PGM rather than treating a storage-buffer witness as a frame.

```sh
cargo test --offline -p newvolim-wgpu-frame
cargo clippy --offline -p newvolim-wgpu-frame --all-targets -- -D warnings
cargo run --offline -p newvolim-wgpu-frame -- \
  --zarr test-data/cells3d-anisotropic.ome.zarr \
  --output /tmp/newvolim-wgpu-cells3d-xy.pgm --axis z
```

The command accepts `--axis z` (XY), `--axis y` (XZ), or `--axis x` (YZ), with Z as the default.
`--level auto` walks the first declared OME-NGFF multiscale in its declared (fine-to-coarse)
order and chooses the first uncompressed `uint16` payload that fits the fixed 16 MiB portable
page pool; an explicit level path retains its exact caller-selected meaning. The cells3d
fixture regression resolves `auto` to level `0`, and the release GPU command emitted the same
valid `128×128` PGM (SHA-256
`86a30233e5246441b2b6fe05c6ca9a65d556791660d8632198c4bb13d86ad55c`). This is a bounded
whole-level admission choice, not yet per-brick LOD selection or residency/eviction.
The shared `newvolim-render::BrickPool` now also records a real fixed word footprint per slot:
its packed offset is the first word of the brick payload rather than the slot ordinal. It rejects
any page shape whose complete slot payloads cannot fit the 20-bit offset field, preserves the
pinned-coarse LRU fallback, and has a regression covering a non-unit eight-word slot. That makes
the CPU-side pool contract directly usable by a future native/browser shader uploader; the CLI
frame proof still uses its intentionally simple whole-volume table and does not claim that
residency integration yet.
For an already-authorized remote raw-v3 store the equivalent source form is
`--https-root https://example.org/store/ --allow-host example.org`; omitting the host policy or
using a root on another host is rejected before any fetch. The unit test and strict clippy command passed. On the Quadro RTX 5000 WGPU/Vulkan adapter, all
three axes assembled the full level-zero `128×128×32` cells3d volume and emitted valid frames:
XY was `128×128` with all 16,384 pixels nonzero and SHA-256
`86a30233e5246441b2b6fe05c6ca9a65d556791660d8632198c4bb13d86ad55c`; XZ and YZ were each
`128×32` with all 4,096 pixels nonzero, with respective SHA-256 values
`738afaf25bcdb894908ad8fe0203199c96345df816f2954e26caa39db6c193cb` and
`9beab6adaf336acbc3c900d63aad49cb95d68b4d23abb169ec022ae485068f8d`.

The same native command now accepts `--depth-output DEPTH.pfm`. Its compute shader writes a
`f32` first-opacity ray distance beside the grayscale output, using the norm of the selected
X/Y/Z column of the composed NGFF affine as its physical step; a ray with no opacity hit remains
exact `+∞`. This consumes one additional fixed storage binding (seven total: output, page table,
four pages, depth) and still fits Chromium's ten-buffer floor. On the real anisotropic fixture,
the bottom row of the Z-ray sidecar begins with **1.305** physical units (`4.5 × 0.29`), was
`128×128`, and had SHA-256 `7d40e45dc45526fd9b468ebdf65a8c2b9b5a1320058a5ca011353e23b35fa5b3`.
The PFM sidecar is a standalone proof of the `RayDistanceF32` contract, not a claim that Palace
or the desktop UI yet binds it for GPU annotation compositing or ID-buffer picking.

## 2026-09-13: native WGPU colour attachment readback (S8 progress)

The native frame proof now has an optional `--color-output FRAME.ppm` attachment. It reads the
first OME `omero` channel, validates its six-digit sRGB colour and finite increasing display
window, converts the colour to linear light, windows raw `uint16` samples in the compute shader,
front-to-back composites premultiplied linear colour into a packed **RGBA16Float** target, and
encodes that target to an sRGB PPM only at readback. The packed two-word representation uses
WGSL `pack2x16float`, so it retains the contract's 16-bit floating target without requiring the
optional shader-f16 feature. Missing or malformed channel metadata uses the explicit red
(`F2281F`), full-`uint16` fallback; it never silently substitutes a palette.

`--output` remains the compatibility PGM opacity projection, and `--depth-output` remains the
independent physical `RayDistanceF32` PFM. Thus all three readbacks arise from one bounded
compute dispatch and one target contract, rather than separate renderers. The real anisotropic
fixture command

```sh
cargo run --offline -p newvolim-wgpu-frame -- \
  --zarr test-data/cells3d-anisotropic.ome.zarr \
  --output /tmp/newvolim-wgpu-cells3d-xy.pgm \
  --color-output /tmp/newvolim-wgpu-cells3d-xy.ppm \
  --depth-output /tmp/newvolim-wgpu-cells3d-xy.pfm --axis z
```

produced a nonempty `128×128` `FF3355`-tinted PPM with SHA-256
`526e187ec65f9d2812de973d59f2a4754861cf8ded693f984ec88e2295a4f379`; its corresponding
16-bit-alpha PGM had SHA-256 `f632007989eff8baabfb1157d1e99b6f9c50f963fad142ab310bf88b52e58ace`.
This is an implementation-level renderer-target proof, still not a Palace wgpu backend, a
surface-presented desktop renderer, multi-channel compositing, or depth-correct annotation
composition.

The command now reports a repeatable timing boundary with every frame: source metadata/chunk
load separately from device creation, pipeline setup, dispatch, and readback. A fresh local
cells3d run measured **22.114 ms** load and **500.743 ms** for that full WGPU setup-and-frame
path. This is a cold, debug-profile end-to-end baseline rather than a steady-state GPU frame
budget; it makes the later Stage-9 comparison measurable without claiming an optimisation.
`cargo check --offline -p newvolim-wgpu-frame --target x86_64-pc-windows-gnu` also passes. The
corresponding Apple-target check cannot be completed on this Linux host: `ring`/`aws-lc` require
an Apple C compiler and SDK, and the host C compiler correctly rejects `-arch arm64` and
`-mmacosx-version-min`. Actual Apple-Silicon and Windows/D3D12 adapter runs remain S6/S7 gates.

This is a genuine native local-data WGPU frame, but not the finished Palace wgpu backend or a
desktop window: it currently supports only the bounded raw-v3 `uint16` subset, one full volume
that fits in its 16 MiB four-page static pool, selectable orthogonal raymarches, and PGM readback. Codec
support, multi-page residency/eviction for larger volumes, linked interactive camera controls,
surface presentation, the Palace task graph, and Vulkan-vs-WGPU frame comparison remain required
Stage-6 work.

The same native WGPU command now also accepts a credentialed S3 source through the common source
registry: `--s3-bucket BUCKET --s3-prefix PREFIX --s3-profile PROFILE [--s3-region REGION]`.
The command turns its explicit bucket and profile into a one-source `S3SourcePolicy`, so it does
not use a generic `s3://` path or an unselected ambient profile. Parser coverage rejects an
incomplete S3 source before any GPU or network work. A live S3 render still awaits the
provisioned test bucket/profile described above.

## 2026-09-13: desktop optional Palace-depth transport

The native Tauri host now transports the same optional attachment shape as the frame service:
a Palace `FrameAttachments` result becomes an sRGB PNG data URL plus an omitted-or-base64 PFM
`rayDistancePfmBase64` sidecar. The host sets `RenderTarget.depth` to `rayDistanceF32` only when
the sidecar was validated and encoded; colour-only, progressive-incomplete Palace results retain
`depth: none`. This prevents a desktop caller from treating a PNG or renderer cache as picking
depth while keeping the UI boundary ready for the paired Palace producer.

`cargo fmt --check && cargo test --offline -p newvolim-desktop -q` passed with nine tests,
including a paired synthetic attachment serialization assertion. The real local command

```sh
cargo run --offline -q -p newvolim-desktop -- \
  --smoke-local test-data/cells3d-anisotropic.ome.zarr
```

completed against the committed anisotropic fixture, exercising local admission, default and
camera-controlled Palace attachment paths, and linked orthogonal panes. This is Linux/local
transport evidence only; it neither claims a stable finite Palace paired-depth producer nor
replaces the deferred Apple-Silicon, Windows/D3D12, live S3/SSH, or production-scale gates.

The CSR native-PNG admission now uses the same exact PFM validation as the loopback WebSocket
path: it accepts `depth: rayDistanceF32` only with an exact-size, declared-extent, little-endian
PFM containing non-negative finite or `+∞` values, and keeps the validated sidecar separate from
display PNG alpha. The Node regression (`node scripts/test_ui_admission.mjs`) covers both the
colour-only desktop shape and the paired desktop shape; `env NO_COLOR=false trunk build --release`
completed successfully. This closes the native desktop-to-CSR transport gap without claiming that
the optional Palace cache is always ready or that the CSR performs 3D depth compositing yet.

The CSR also now exposes a strict `newvolimReadRayDistancePfm` handoff for the eventual picker:
after whole-sidecar validation it converts a bounded top-left canvas pixel to PFM's bottom-up
row convention and returns exactly one validated physical `f32` distance. The admission test
covers both row orientation and bounds rejection. This gives native and remote attachments the
same safe CPU-pixel boundary; it is not a substitute for the future camera-ray reconstruction and
GPU depth-correct annotation composite pass.

Desktop commands now also apply the loopback service's **16 Mi-pixel** request budget before
entering Palace: a volume request counts once, while linked orthogonal panes count three times.
The overflow-safe regression admits an exact `4096×4096` volume, rejects one pixel beyond it,
rejects that same size as a three-pane aggregate, and rejects zero/overflow geometry. Ten desktop
tests pass, and the bounded `--smoke-local` command again rendered the local anisotropic fixture
with default volume, camera volume, and all three linked panes. This is an in-process webview
admission limit, not a production throughput claim.

When a native or remote Palace frame has a validated PFM sidecar, clicking the volume canvas now
reports its bounded physical first-opacity distance using the same PFM sampler. It never creates
or moves an annotation from that value: a 3D physical point still requires camera-ray
reconstruction, so the interaction remains an explicit diagnostic rather than a misleading
depth-picking claim. The Node UI admission regression and release CSR build passed after adding
this interaction.

### Palace-core local test discipline

The Palace-core GPU tests create Vulkan devices and are not safe to run concurrently on this
host: the default parallel harness can report `ERROR_INITIALIZATION_FAILED` or validation
failures from device contention. The authoritative local command is therefore

```sh
cargo test --offline -p palace-core -q -- --test-threads=1
```

which completed **34 passed, 0 failed** in 64.04 seconds on 2026-09-13. The fork also has no
local compiler warnings under `cargo fmt --check && cargo clippy --offline --workspace
--all-targets --no-deps -- -D warnings`; the only remaining notice is the transitive external
`proc-macro-error2` future-incompatibility report.

For routine local acceptance, `sh scripts/verify_local.sh` now composes formatting, strict
offline workspace linting/tests, a Windows-target compile of `newvolim-wgpu-frame`, the required
serialized Palace-core GPU suite, the Node UI admission regression, and the release Trunk build.
It completed successfully end-to-end on 2026-09-13 before the targeted compile was added; that
added compile has independently passed. The serialized GPU segment reported **34 passed, 0
failed** in 147.53 seconds during the full run. The script deliberately does not claim to execute
the deferred platform adapters, live credentials, or production-scale deployment gates.

The targeted portable renderer compile continues to pass with
`cargo check --offline -p newvolim-wgpu-frame --target x86_64-pc-windows-gnu`. A full-workspace
Windows cross-check is not currently an equivalent test: it reaches Palace's default Python
binding feature (whose Python-only imports are not fully cfg-gated) and a cross-built `shaderc`
toolchain/CMake policy path. Those build-system limitations are distinct from the deferred
Windows/D3D12 adapter acceptance run; they do not invalidate the targeted renderer compile.

The Stage-5 backend seam advanced in the random-walker entry module: its input, output, and
debug-download barriers now use Palace-owned `gpu::Stage`/`gpu::Access` through the transitional
Vulkan conversion constructors, rather than importing `ash::vk` synchronization flags directly.
`cargo test --offline -p palace-core randomwalker::test -q -- --test-threads=1` passed **5/5**;
the serial harness remains necessary for GPU tests on this host.

The random-walker weight generators and solver now use that same backend-neutral synchronization
vocabulary for every compute and transfer dependency. Their remaining `ash::vk` use is confined
to Vulkan pipeline, allocation, and command-buffer implementation details rather than operator
resource semantics. `cargo fmt --check`, a direct zero-result audit for Vulkan synchronization
flags in both modules, and the serial `randomwalker::test` regression passed **5/5** on
2026-09-13. This is a bounded Stage-5 seam migration, not a claim that Random Walker itself is
already available on the portable backend.

### Palace paired first-opacity attachment

Palace now reserves the raycaster-owned `state_depth` surface before scheduling its colour
producer, initializes it to the explicit `+∞` no-hit sentinel, and establishes the required
transfer-to-compute visibility. The raycaster writes its independent first-opacity `f32` while
compositing colour; producer-side validation prevents a negative or indeterminate intermediate
value from entering the attachment. The one runtime resolution consequently reads the same
producer cache allocation rather than a later, unrelated cache generation. This is a genuine
paired render surface, not `state_ray`, PNG alpha, or a second raycast.

`palace-png` reads that surface with the raycaster's physical row stride, validates the
non-negative-finite-or-`+∞` contract, and encodes it as its paired PFM sidecar. The focused
synthetic Palace regression requires a matching `16×12` attachment, valid values, and at least
one finite first-opacity distance; it passed. Full `palace-frame` (**3/3**) and
`newvolim-server` (**16/16**) suites passed afterward. The desktop smoke was strengthened to
require PFM sidecars for both default and camera-controlled local views; it passed against the
committed anisotropic fixture, reporting 1118/1026 colour data-URL bytes and all three linked
orthogonal panes. This is verified Linux/local depth transport; Apple-Silicon, Windows/D3D12,
live S3/SSH, and production-scale validation remain deferred acceptance gates.

The hierarchical Random Walker wrapper has also migrated every live compute/transfer dependency
to `gpu::Stage` and `gpu::Access`; its remaining Vulkan references are only allocation, pipeline,
and command submission implementation detail. `cargo fmt --check`, a direct zero-result audit
for Vulkan synchronization flags in the module, and the serialized
`cargo test --offline -p palace-core randomwalker::test -q -- --test-threads=1` suite passed
**5/5** on 2026-09-13. This extends the Stage-5 portable synchronization seam without claiming
that the complete Random Walker operator has a wgpu implementation.

### Shader retargeting spike

The existing nested `palace-shader-spike` compiled Palace's real entry/exit vertex and fragment
GLSL through Palace's shader compiler, then passed the emitted SPIR-V through Naga's `spv-in`,
WGSL output, and MSL output. `cargo run --offline -q -p palace-shader-spike` from `palace-dev`
completed on 2026-09-13: `entryexitpoints.vert` converted from 756 SPIR-V words to 2882 bytes of
WGSL and 4550 bytes of MSL; `entryexitpoints.frag` converted from 341 words to 1020 bytes of WGSL
and 1855 bytes of MSL. This resolves the Stage-0 retargeting decision for this shared
geometry/pipeline subset. It does not claim the full raycaster is WebGPU-ready: its buffer
references and 64-bit atomics still require the selected bounded-pool/feedback redesign.

### Post-paired-depth local regression baseline

`sh scripts/verify_local.sh` completed end-to-end on 2026-09-13 after the paired Palace depth
producer, Random Walker synchronization migrations, and shader-spike evidence changes. It passed
formatting; strict offline workspace clippy; the complete workspace test set; the targeted
`newvolim-wgpu-frame` Windows compile; the serialized Palace-core GPU suite (**35/35** in
94.28 seconds); the Node UI admission regression; and the release Trunk CSR build. As before,
the only notice is the external transitive `proc-macro-error2` future-incompatibility warning.
This is the current Linux/local regression baseline, not evidence for deferred Apple-Silicon,
Windows/D3D12 adapter, live S3/SSH, or production-scale validation gates.

The verifier now also includes the nested-workspace `palace-shader-spike`, so future local
baselines will fail if Palace's actual entry/exit SPIR-V no longer retargets to both WGSL and MSL.
`sh -n scripts/verify_local.sh` and that new step passed after the update.

The GUI operator's CPU-frame buffer upload/download declarations now also use the portable
`Stage::TRANSFER`/`Access::{TRANSFER_READ, TRANSFER_WRITE}` semantics. The sole direct Vulkan
synchronization site left in that module is its image-layout transition, which is an explicit
Vulkan image operation rather than a portable buffer-use declaration. `cargo fmt --check` and
`cargo check --offline -p palace-core` passed after this bounded migration.

The server now has a fixture-backed paired-depth regression rather than only synthetic envelope
coverage. It renders the committed anisotropic OME-Zarr through
`render_local_zarr_with_camera_attachments`, requires the returned `Pf 32×24` sidecar, and
checks that the serialized WebSocket frame declares `rayDistanceF32`. The focused test and the
complete `cargo test --offline -p newvolim-server -q` suite passed **17/17** on 2026-09-13.

The desktop boundary has the matching fixture-backed regression: it renders the same local
OME-Zarr through `render_local_zarr_with_camera_attachments`, converts the result into the native
`FramePayload`, and requires both `RayDistanceF32` and the exact `Pf 32×24` PFM header. The
complete `cargo test --offline -p newvolim-desktop -q` suite passed **11/11** on 2026-09-13.

The GPU-storage layer now expresses its page-table upload, use-table readback, optional NaN-fill,
and page initialization dependencies through `gpu::Stage`/`gpu::Access`; Vulkan remains solely
the conversion/execution backend. A direct audit finds no `vk::AccessFlags2` or
`vk::PipelineStageFlags2` in `storage/gpu.rs`. `cargo fmt --check` and the serialized complete
Palace-core suite passed **35/35** in 69.42 seconds after this central Stage-5 migration.

The final GUI image-layout transition now also converts `Stage::ALL_COMMANDS` and memory access
semantics at the Vulkan call boundary, leaving no raw `vk::AccessFlags2` or
`vk::PipelineStageFlags2` use anywhere under `palace-core/src/operators`. The complete
operator-tree audit and serialized Palace-core suite passed **35/35** in 74.19 seconds. This
completes the Stage-5 vocabulary replacement for operator and GPU-storage resource semantics;
the backend traits and actual wgpu recorder remain separate required work.

`scripts/verify_local.sh` now enforces that result with a zero-match audit for raw Vulkan access
or stage flags in Palace operators and GPU storage. Its shell syntax and the new audit passed on
2026-09-13, preventing this completed seam milestone from silently regressing.

The canonical verifier was rerun after adding the real server/desktop depth regressions and the
synchronization audit. `sh scripts/verify_local.sh` passed end-to-end on 2026-09-13: strict
offline workspace checks; desktop **11/11**; server **17/17**; target renderer compile; serialized
Palace-core **35/35** in 73.56 seconds; the zero-match sync audit; shader retargeting to WGSL and
MSL; UI admission; and the release CSR build. The external `proc-macro-error2` notice remains
the only warning. This remains a Linux/local baseline, not deferred platform or infrastructure
acceptance evidence.

The nested `palace-wgpu-spike` now emits a genuine per-pixel first-opacity index from its four
fixed storage-page raymarch instead of a hard-coded depth witness. Its synthetic proof requires
the empty edge to return `u32::MAX` (the portable no-hit sentinel), the opaque centre to return
an in-range depth index, and all four packed-page selections to resolve. The nested unit suite
passed **4/4**, including a CPU page-pool oracle for the synthetic front-face sample; the
executable passed on the Quadro Vulkan adapter and again on llvmpipe, where
the six storage bindings remain below the adapter's 48-per-stage limit. The canonical local
verifier now runs this proof after shader retargeting. This is Linux portable-representation
evidence only, not a deferred Apple-Silicon or Windows/D3D12 adapter run.

The canonical verifier now runs the `palace-wgpu-spike` **4/4** CPU/oracle suite before invoking
its live adapter proof. Shell syntax, that test step, and the llvmpipe proof passed after the
update.

`scripts/verify_local.sh` now also runs the native desktop `--smoke-local` fixture path after
the serialized Palace suite. It passed after the update with the committed anisotropic OME-Zarr:
1118 default-volume bytes, 1026 camera-volume bytes, linked orthogonal PNG sizes 675/333/333,
and the declared `[128, 128, 32]` shape. The smoke requires paired PFM sidecars for both volume
views, so this is maintained end-to-end local attachment transport evidence.

The expanded `sh scripts/verify_local.sh` completed end-to-end on 2026-09-13. In addition to
the strict workspace checks, desktop **11/11**, server **17/17**, and Palace-core **35/35** in
75.99 seconds, it ran the fixture desktop smoke; real SPIR-V→WGSL/MSL retargeting; the portable
pool oracle **4/4** plus its llvmpipe execution; Node UI admission; and the release CSR build.
The external `proc-macro-error2` notice remains the sole warning. This is the current Linux/local
baseline and still does not substitute for deferred platform adapters, live infrastructure, or
production-scale validation.

The canonical verifier now also feeds the committed decoded `cells3d` `8×32×32` Z/Y/X chunk
through `palace-wgpu-spike` after its synthetic run. The real-data proof passed on the Quadro
and llvmpipe with `32×32×8` GPU dimensions, four selected pages, colour edge/centre
`(4961, 4682)`, and first-opacity edge/centre `(0, 0)`. It remains a bounded raw-chunk proof,
not a completed Palace task-graph backend, but it ensures the portable page representation is
exercised against actual committed microscopy bytes on every local baseline.

The complete verifier was rerun after adding that real-chunk step. It passed end-to-end on
2026-09-13: desktop **11/11**, server **17/17**, serialized Palace-core **35/35** in 72.97
seconds, fixture desktop smoke, shader retargeting, portable wgpu oracle **4/4**, synthetic and
real cells3d pool runs on llvmpipe, UI admission, and release CSR build. The only notice remains
the transitive external `proc-macro-error2` future-incompatibility warning. This is the current
Linux/local baseline, with platform adapters, live remote credentials, and production-scale
validation still explicit deferred gates.

## 2026-09-13: local ZIP Zarr writer safety

`palace-zarr`'s local ZIP store no longer panics for partial writes or erases. ZIP has no safe
in-place update/delete operation: appending a second member for an existing Zarr key can make
reader behaviour ambiguous. Those operations now return explicit `Unsupported` storage errors,
as do repeated `set` keys; normal writes propagate ZIP and I/O failures instead of unwrapping.
The single-tensor, embedded-tensor, and LOD save APIs now all call the store flush boundary so a
ZIP central directory is present before the API reports success; unlike `ZipWriter`'s `Drop`,
this reports finalization failures. Intermediate pyramid levels remain reopenable for append.

The focused offline `palace-zarr` suite passed **4/4**, including a write → flush → standard ZIP
read → append → flush round trip plus partial-write, erase, and duplicate-key error cases.
`scripts/verify_local.sh` now runs that suite before the shader and portable-wgpu checks. This
is concrete Linux/local source handling, independent of the deferred live S3/SSH and
platform-adapter gates.

The complete `sh scripts/verify_local.sh` baseline passed after this verifier extension on
2026-09-13: strict offline workspace checks; desktop **11/11**; server **17/17**; serialized
Palace-core **35/35** in 76.38 seconds; fixture desktop smoke; ZIP-store **3/3** at that point; shader
retargeting; portable pool **4/4** plus synthetic and real cells3d llvmpipe runs; UI admission;
and the release CSR build. The sole notice was the external transitive `proc-macro-error2`
future-incompatibility warning. Deferred adapters, live credentials, and production-scale
validation remain intentionally outside this local baseline.

After extending the explicit flush to the two single-tensor save APIs, the focused offline
`palace-zarr` suite again passed **4/4**. An exploratory `convert` package check did not run to
completion because its optional video stack needs the host's missing `libavfilter.pc`; that
system development dependency is outside `palace-zarr` and the canonical local verifier, so it
does not block ZIP-source correctness work.

The canonical verifier was rerun after the single-save finalization correction and passed
end-to-end on 2026-09-13: strict workspace checks; desktop **11/11**; server **17/17**;
serialized Palace-core **35/35** in 71.44 seconds; local fixture depth smoke; ZIP-store **4/4**;
shader retargeting; portable WGPU **4/4** plus synthetic and real cells3d llvmpipe executions;
UI admission; and the release CSR build. The transitive `proc-macro-error2`
future-incompatibility notice remains the only warning. This is Linux/local evidence and does
not replace deferred adapter, credential, or production-scale gates.

## 2026-09-14: portable submission-lifetime seam

`palace_core::gpu` now owns `SubmissionEpoch` and the narrow `SubmissionTracker` contract:
current submission, oldest completed submission, and completion of a given epoch. The previous
Vulkan-named `CmdBufferEpoch` has been eliminated from GPU storage, descriptor reuse,
temporary-state returns, and window lifetime state. Vulkan's `DeviceContext` implements the
contract through its existing fences; GPU storage and temporary-resource reclamation consume the
trait instead of command-buffer-specific APIs.

This is deliberately a small real backend seam, not a pretend universal renderer: allocation,
recording, pipelines, and bind groups remain backend-specific until their callers are migrated.
It gives wgpu a direct home for `SubmissionIndex`-based completion without changing the
epoch-aware LRU policy. `cargo check --offline -p palace-core` and the serialized core suite
passed **37/37** in 60.15 seconds after the migration, including compile-time proof that Vulkan
implements `SubmissionTracker` and portable epoch ordering coverage.

The complete `sh scripts/verify_local.sh` baseline also passed after this core migration on
2026-09-14: strict workspace checks; desktop **11/11**; server **17/17**; serialized
Palace-core **37/37** in 72.77 seconds; fixture depth smoke; ZIP-store **4/4**; SPIR-V retarget
checks; portable WGPU **4/4** plus synthetic and real cells3d llvmpipe runs; UI admission; and
the release CSR build. The only notice remains the external transitive `proc-macro-error2`
future-incompatibility warning. Apple Silicon, Windows/D3D12, live credentials, and
production-scale deployment continue as explicit deferred acceptance gates.

## 2026-09-14: native depth-correct 3D annotation selection

The desktop host now implements an actual native annotation-pick path instead of treating the
PFM sidecar as a display-only diagnostic. `palace-frame` reconstructs the fitted camera ray from
the same local source, frame extent, and bounded controls used for rendering. Its public ray is
explicitly raw array-axis order; `LocalSession::palace_ray_to_physical` mirrors Palace's
dataset-local scale, applies the complete NGFF affine/translation transform, reorders to physical
`[x,y,z]`, and returns the distance conversion needed for the renderer-owned sidecar.

`pick_open_dataset_annotation` rerenders the bounded local request in the host, reads the exact
paired first-opacity sample, converts its distance through that bridge, and selects only an
annotation at or before the volume surface. It never accepts browser-provided depth. Browser
clicks invoke this path only for native frames; remote frames remain diagnostics until they have
the matching authoritative session contract. Focused checks passed: Palace-frame **6/6**,
desktop **14/14**, including a committed-cells3d camera-ray → NGFF bridge → paired-depth pick
regression, and UI admission. GPU overlay compositing remains a separate portable backend task;
this evidence does not claim it is complete.

The complete `sh scripts/verify_local.sh` run passed after the native-picker IPC integration on
2026-09-14: strict workspace checks; desktop **14/14**; server **17/17**; serialized
Palace-core **37/37** in 72.06 seconds; fixture depth smoke; ZIP-store **4/4**; shader retarget
checks; portable WGPU **4/4** plus synthetic and real cells3d llvmpipe runs; UI admission; and
the release CSR build. The only notice remains the external transitive `proc-macro-error2`
future-incompatibility warning. Deferred platform adapters, live credentials, and
production-scale deployment remain outside this Linux/local baseline.

The camera reconstruction was additionally checked at an off-centre pixel against the inverse of
Palace's own fitted projection matrix, guarding its FOV, aspect, orbit, zoom, top-left pixel, and
raw-array-axis conventions rather than merely checking a centre ray. `palace-frame` passed
**6/6** and the dependent desktop suite **14/14**. The full local verifier passed again on
2026-09-14: desktop **14/14**, server **17/17**, Palace-core **37/37** in 74.75 seconds, fixture
depth smoke, ZIP-store **4/4**, shader and portable WGPU checks, UI admission, and release CSR
build. The sole notice remains external transitive `proc-macro-error2`; deferred platform and
deployment gates remain unchanged.

## 2026-09-14: exact native depth-sidecar reuse

Native annotation picks now reuse the most recent renderer-owned depth sidecar only when its
dataset root, target extent, and complete camera controls exactly match the pick request. The
single-entry cache is bounded by the desktop frame budget (at most 64 MiB for `f32` depth); a
miss rerenders locally and no browser-provided depth is accepted. A regression proves that a
different dataset, extent, or camera cannot retrieve the cached sidecar. Focused desktop checks
passed **15/15**, including the committed-fixture camera/NGFF/depth bridge.

## 2026-09-14: portable depth-gated annotation-compositing proof

`palace-wgpu-spike` now performs bounded GPU annotation composition in the same compute pass
that produces its colour and first-opacity outputs. A fixed primitive table carries a stable ID,
ray distance, and screen-space point, line-segment, or triangle geometry. The shader depth-tests
each primitive against that exact pass's first-opacity value, yielding the synthetic and committed
cells3d result `point/line/triangle/occluded=(1, 2, 3, 0)`. The final point lies behind the
volume and retains its volume colour; no second render, alpha-derived depth, or host-provided ray
state participates. This remains a deliberately bounded portable proof: scene-generated meshes,
arbitrary polygons, and desktop/browser attachment integration are still needed.

The complete `sh scripts/verify_local.sh` baseline passed after this addition on 2026-09-14:
strict workspace checks; desktop **16/16**; server **17/17**; serialized Palace-core **37/37**;
fixture depth smoke; ZIP-store **4/4**; shader retarget checks; portable WGPU **5/5** plus
synthetic and real cells3d llvmpipe runs with `point/line/triangle/occluded=(1, 2, 3, 0)`; UI
admission; and release CSR build. The only notice remains the external transitive
`proc-macro-error2` future-incompatibility warning. Deferred adapter, credential, and
production-scale acceptance gates remain unchanged.

## 2026-09-14: native two-corner rectangle and ellipse ROIs

The desktop now creates rectangle and ellipse annotations from two linked-slice corners. The
session requires exactly one shared voxel axis, reconstructs the two physical half-axis vectors
and centre through the full level-zero NGFF transform, and persists the resulting backend-neutral
geometry. The CSR UI exposes bounded two-corner drafting and sends only those corners through
the Tauri boundary. Desktop tests passed **16/16**, including anisotropic physical-axis and
invalid-plane coverage; UI admission and the release CSR build passed as well.

The browser OME-Zarr preview's existing bounded WASM decode routes for Zstd, LZ4, and Blosc are
now individually asserted by the UI admission regression. Each decoder receives the fixed page
budget before output allocation; the browser does not treat compressed chunk bytes as an
unbounded preview input.

`newvolim-render::composite_additive_channels` defines the shared transfer-function step for
native and future multi-channel renderers. It applies each enabled channel's native-unit window,
opacity, and sRGB-to-linear colour conversion before additive premultiplied composition; disabled
or non-finite samples contribute nothing. The browser's independently bounded four-page route
now consumes equivalent OMERO state for up to four channels. Render tests passed **11/11**;
general native frame and layer-session request binding remain outstanding.

The UI admission harness now drives the complete direct-store metadata path with a `C,Z,Y,X`
fixture, not just its isolated helpers. It verifies the two exact singly-chunked URLs, physical
spacing with `X` at array index three, the two fixed-page payloads, and OMERO transfer state
including a disabled channel's zero-opacity binding. This caught and fixed an actual four-axis
scale accumulation bug before it could reach WebGPU.

The portable annotation table is now a `palace_core::gpu` contract rather than a private WGPU
demo layout. `ProjectedAnnotationPrimitive` has fixed point/segment/triangle encodings (nine
`u32` words each), requires a nonzero stable ID that fits the portable ID attachment, and carries
the ray distance used by the first-opacity comparison. The WGPU spike uploads these records
directly. Its core regression rejects zero and overflowing `u64` IDs, so a future scene bridge
cannot silently alias annotation identities.

`palace-core` now also has a real no-default `wasm32-unknown-unknown` boundary: its portable
geometry, metadata, and `gpu` contracts compile without native memory mapping, Vulkan loading, or
shader compilation dependencies. The Vulkan runtime/operator surface remains target-gated for
native Palace consumers. `scripts/verify_local.sh` runs this check alongside the native frame and
llvmpipe proofs; it is not an Apple-Silicon or Windows/D3D12 adapter acceptance substitute.

The complete `sh scripts/verify_local.sh` baseline passed again on 2026-09-14: strict workspace
checks; desktop **16/16**; server **17/17**; serialized Palace-core **37/37** in 68.37 seconds;
fixture depth smoke; ZIP-store **4/4**; shader retarget checks; portable WGPU **4/4** plus
synthetic and real cells3d llvmpipe runs; UI admission; and release CSR build. The sole notice
remains the external transitive `proc-macro-error2` future-incompatibility warning. Deferred
adapter, credential, and production-scale acceptance gates remain unchanged.

The complete `sh scripts/verify_local.sh` baseline passed after the ROI interaction wiring on
2026-09-14: strict workspace checks; desktop **16/16**; server **17/17**; serialized
Palace-core **37/37** in 68.43 seconds; fixture depth smoke; ZIP-store **4/4**; shader retarget
checks; portable WGPU **4/4** plus synthetic and real cells3d llvmpipe runs; UI admission; and
release CSR build. The only notice remains the external transitive `proc-macro-error2`
future-incompatibility warning. Deferred adapter, credential, and production-scale acceptance
gates remain unchanged.

## 2026-09-14: Python bindings made opt-in for portable core consumers

`palace-core` no longer enables its `python` feature by default. The shared image-viewer,
raycaster, slice-viewer, and dtype APIs retain ordinary Rust constructors and methods; their
PyO3 fields, constructors, and binding-only helpers are now compiled in separate explicit
Python implementations. Both `cargo check --offline -p palace-core --no-default-features` and
the explicit `--features python` configuration pass. The local verifier now guards both forms.
This removes the Python ABI from the default core feature graph, but does not by itself make the
remaining Vulkan, shader, filesystem, or threading dependencies wasm-compatible.

A direct local `wasm32-unknown-unknown` probe now gets past `rand`'s entropy setup: the
target-specific `getrandom/js` feature is selected by `palace-core` without affecting native
builds. The probe then stops at the known unconditional `memmap` disk tier and `ash` Vulkan
loader dependencies. This is useful evidence for the required next split (RAM/GPU-only storage
and a non-Vulkan backend), not a claim of wasm support or a deferred-platform blocker.

The complete `sh scripts/verify_local.sh` baseline passed with both feature configurations on
2026-09-14: strict workspace checks; desktop **15/15**; server **17/17**; serialized
Palace-core **37/37** in 69.92 seconds; fixture depth smoke; ZIP-store **4/4**; shader retarget
checks; portable WGPU **4/4** plus synthetic and real cells3d llvmpipe runs; UI admission; and
release CSR build. The only notice remains the external transitive `proc-macro-error2`
future-incompatibility warning. Deferred adapter, credential, and production-scale acceptance
gates remain unchanged.

## 2026-09-14: portable projected-annotation stream

`newvolim-render` now projects expanded physical annotation geometry through a renderer-owned
camera adapter into the fixed portable record layout used by `palace-core` and the bounded WGPU
proof. Each 13-word record retains the primitive kind, non-truncated stable ID, packed sRGB
colour, camera-projected pixel radius, and three `(x, y, physical-ray-distance)` vertices as
IEEE-754 bits. This allows the GPU to interpolate depth across slanted segments and triangles
before comparing it with the first-opacity attachment. Projection rejects unavailable cameras,
non-finite data, negative distance, off-target vertices, invalid radii, and IDs that cannot fit
the portable ID attachment; it never silently drops an editable annotation.

The Palace WGPU proof consumes the corresponding expanded layout and passed locally on the
Quadro RTX 5000 Vulkan adapter: it emitted the projected point's packed red colour, visible
point/line/triangle IDs were `(1, 2, 3)`, and the point behind the volume was `0`. Focused
`newvolim-render` (**13/13**), `palace-core` GPU (**5/5**),
and `palace-wgpu-spike` (**5/5**) tests passed. This establishes a tested scene-to-recorder
wire contract, but the desktop and browser render passes have not yet bound the emitted stream;
that remains the next portable compositing step. It is independent of the deferred Apple,
Windows/D3D12, live credential, and production-scale acceptance gates.

The Palace Frame API now exposes the complementary fitted-camera projection for a raw
Palace-array point: `project_point_for_local_zarr` returns physical frame pixels and the same
unit-ray distance used by first-opacity depth, or no projection for points behind/invalid for the
camera. Its orbit/zoom round-trip test projects a point sampled from `camera_ray_for_volume` back
to the originating pixel and distance. This removes the remaining camera-convention ambiguity
for the desktop adapter that will transform NGFF physical annotations to raw Palace coordinates
before submitting the portable record stream.

The desktop host now carries that route through `project_open_dataset_annotations`. It transforms
each persisted physical NGFF vertex to an **unrounded** level-zero voxel coordinate, reorders it
to Palace's raw `[z,y,x]` convention, and uses the fitted camera projection to emit the portable
13-word stream for the requested extent and controls. The host does not accept a browser camera
matrix or annotation coordinates for this operation. Its local fixture test covers the entire
physical-scene-to-record path. Scene ID zero is valid, while GPU attachment zero means “no
annotation”; consequently the record uses a reversible nonzero `scene_id + 1` wire ID. The
projection retains finite vertices outside the frame so GPU clipping can preserve a partially
visible line or polygon rather than dropping it. The webview has not yet submitted this trusted
stream to its WGPU compositing pass.

The browser's existing WebGPU presentation pass now accepts an optional `Uint32Array` of those
fixed 13-word records. It binds the record buffer beside the linear RGBA16F volume target and
the R32F first-opacity texture, interpolates segment/triangle ray distance per physical pixel,
rejects records behind the first opacity, converts record sRGB colour to linear light, and only
then applies display sRGB encoding. The UI admission test covers the record-width validation and
the shader's storage/depth/linear-colour contract. This is a real browser portable compositing
path for a camera-matched record producer; the current trusted desktop producer uses Palace's
fitted camera, while direct browser OME-Zarr preview uses its independent browser camera, so the
two are intentionally not mixed yet.

Focused verification after the projection and browser-compositing integration passed on
2026-09-14: `newvolim-render` **13/13**, desktop **17/17**, Palace Frame **7/7**, portable WGPU
**5/5**, and the live Quadro RTX 5000/Vulkan WGPU proof (visible IDs `(1,2,3)`, occluded ID
`0`, and packed red annotation output). The browser UI admission harness passed. This is
Linux/local evidence; Apple-Silicon, Windows/D3D12, live S3/SSH, and production-scale runs
remain the explicitly deferred acceptance gates.

The direct browser OME-Zarr preview no longer requires a singly chunked channel axis. A bounded
decoded `C` chunk (up to the existing four 4 MiB spatial pages) is split by declared Zarr
strides, so C may appear in any axis position and each selected channel still reaches one static
page. The UI harness exercises a two-channel `C,Z,Y,X` chunk and verifies both the extracted
bytes and the one-request bounded load. This does not expand the four-channel/page budget or
claim a general multi-layer renderer.

After the browser record-compositing and wider-C extraction changes, `env NO_COLOR=false trunk
build --release` completed successfully on 2026-09-14. The only build notice remains the external
`proc-macro-error2` future-incompatibility warning.

## 2026-09-16: general native portable draw-packet annotation pass

`newvolim-wgpu-frame` now accepts a `NativePortableDrawInput`, rather than only its volume
component, for the one-page direct local route. The compute pass validates the shared 13-word
projected-annotation records and the 4,096-record bound, binds them as a fixed eighth storage
resource, interpolates per-vertex physical ray distances, converts packed sRGB colours to linear
light, and composites only records at or in front of the same first-opacity `RayDistanceF32`
attachment produced by its volume raymarch. The existing bounded four-page volume path uses the
same annotation binding, so it does not silently lose overlays merely because it selects a
larger local input.

The command-only `--annotation-fixture` diagnostic creates canonical overlapping red-front and
green-behind points; it is not an alternate scene source. On the committed anisotropic fixture,
the local Linux WGPU run completed and the three center PPM pixels beginning at byte offset
24,783 were `255 0 0`; the following pixel remained the OME volume colour (`94 12 26`). The
green record was therefore rejected by the same first-opacity comparison. This establishes that
the general pass—not a CPU compositor or the isolated Palace proof—consumes the portable packet.
`cargo test --offline -p newvolim-wgpu-frame` and strict
`cargo clippy --offline -p newvolim-wgpu-frame --all-targets --no-deps -- -D warnings` passed.
Trusted desktop-generated records and their camera transform still need to be submitted to this
general recorder. Apple-Silicon, Windows/D3D12, credentialed S3/SSH, and production-scale
acceptance remain explicitly deferred rather than blockers.

The complete local verifier also passed after this change on 2026-09-16: strict compilation and
clippy; local crate tests; serialized Palace-core **38/38**; the committed local desktop smoke;
ZIP-store **4/4**; shader retarget checks; portable WGPU **6/6** plus synthetic and real cells3d
llvmpipe runs (the latter retained visible IDs `(1,2,3)` and rejected the occluded ID); UI
admission; and the release CSR build. The only notices were the known absent optional Vulkan
validation layer and transitive `proc-macro-error2` future-incompatibility warning. Neither is a
deferred-platform or implementation blocker.

The native frame crate now also exposes `render_portable_draw` as a reusable library entry point,
with public axis/readback helpers. It executes the exact same typed direct draw packet as the CLI;
the CLI parser, source policies, and diagnostic-only fixture remain private implementation detail.
This is the required native/headless handoff seam, not a claim that the desktop has already
submitted its Palace-camera packet to a matching portable camera. Focused WGPU-frame tests and
strict clippy passed for both its library and binary targets.

The desktop host now provides the next handoff object, `NativePortableCameraDrawInput`. It pairs
the trusted direct volume/annotation packet with one exact fitted-Palace camera ray for every
top-to-bottom physical pixel. Rays are generated from the same local root, controls, and extent
used for annotation projection, then converted from Palace raw `[z,y,x]` coordinates to portable
`[x,y,z]`; they are finite, normalized, extent-counted, and capped at 4,194,304 entries before
allocation. The focused desktop regression uses a non-default orbit/zoom fixture and proves the
first portable ray is precisely that coordinate reordering of Palace's ray. Desktop **20/20**,
render **20/20**, and strict desktop clippy passed. The portable WGPU shader had not yet bound
this ray table at that point, so this entry made no premature camera-match claim.

## 2026-09-16: camera-complete native portable recorder

`newvolim-wgpu-frame` now consumes `NativePortableCameraDrawInput` directly. Its WGPU shader
uploads the exact per-pixel origins and normalized directions beside the fixed annotation records,
intersects every ray with the admitted local XYZ volume, marches that interval at a bounded
half-voxel step, and writes first-opacity distance in the same ray parameter used by the desktop
projection records. The existing orthogonal CLI path remains intact. To meet the local adapter's
eight storage-buffer-per-compute-stage limit, annotations followed by camera-ray bits share one
read-only payload binding rather than adding a ninth storage binding.

The desktop now invokes this reusable recorder through `render_native_portable_camera_draw` and
converts its RGBA/readback depth into the same validated PNG+PFM `FramePayload` transport used by
Palace. A crucial coordinate correction translates Palace's global raw-ZYX ray origins by the
selected request origin after reordering them to local XYZ; directions and ray distances remain
unchanged. Focused desktop tests passed **20/20**, normal WGPU tests passed, and the opt-in local
adapter camera-packet integration test passed for both library and binary targets. This proves the
native fitted-camera path locally; wiring it to the UI's selected rendering/picking flow remains.

## 2026-09-16: explicit portable-layer UI activation

The local desktop UI now makes the camera-complete native WGPU recorder reachable without
weakening source authority. After a user opens a local dataset it explicitly invokes
`prepare_default_portable_image_layer`; the host derives an axis-aligned level-zero transform,
an OME display channel/window/colour, and a canonical source binding before admitting any page.
Disabled channel entries are retained ahead of an OME-selected C>0 channel, preserving the actual
source C index instead of accidentally reading C=0. The UI selects
`render_native_portable_camera_draw` for this bounded one-chunk route and cleanly falls back to
the existing Palace renderer when preparation or admission is unavailable.

Portable first-opacity PFM was deliberately diagnostic-only in that UI revision. It was not sent
to the Palace picker, because mixing a portable ray parameter with Palace's renderer-owned camera
would violate the depth ownership contract.

## 2026-09-16: native portable camera/depth picker

`pick_native_portable_annotation` now rebuilds the exact local packet for a clicked physical
pixel, retrieves that pixel's fitted portable XYZ ray, restores its global voxel origin from the
admitted chunk-plan address, converts the normalized ray through the complete NGFF transform, and
re-records the bounded WGPU frame for its paired first-opacity value. It then performs the same
physical annotation query used by the Palace path. The browser supplies only pixel and bounded
camera/chunk request fields; it cannot supply or forge depth.

This work also corrected the request vocabulary: `origin_xyz` and `extent_xyz` are spatial-chunk
coordinates, not voxel coordinates. The host now derives the local voxel translation from the
authorized chunk plan, while the UI requests exactly one chunk for the currently bounded portable
route. Desktop **22/22**, strict clippy, the opt-in local WGPU portable-picker test, and UI
admission passed. Browser portable-camera authority and multi-chunk residency remain separate
work rather than being silently approximated.

## 2026-09-16: browser camera-authority seal

The direct-browser WebGPU renderer already constructs its own fitted perspective rays in WGSL.
Its optional projected-annotation packet now additionally requires an `annotationCamera` seal
matching that renderer's orbit, zoom, physical width, and physical height exactly. A record stream
from a desktop/Palace frame or a stale browser target is rejected before GPU upload, so the
first-opacity comparison cannot accidentally mix camera authorities. The browser UI admission
harness and desktop **22/22** portable unit baseline passed; producing browser-native projected
scene geometry and multi-chunk page residency remain distinct follow-on work.

## 2026-09-16: fixed-boundary multi-page direct volume contract

`NativePortableVolumeInput` now admits one logical XYZ volume across one to four consecutive
static pages. Every page except the last must contain exactly the fixed 1,048,576 storage words;
this prevents a claimed page boundary from disagreeing with the WGPU shader's packed location
calculation. The native WGPU recorder flattens all declared pages before its standard static-page
upload, preserving the same boundary. Focused render and WGPU-frame unit tests cover an actual
page-zero/page-one transition and strict clippy passes. Desktop source admission remains one
chunk/channel pending an explicit spatial residency map; no chunk concatenation or inferred page
layout has been introduced.

## 2026-09-16: canonical chunk axis ordering for portable admission

Desktop portable admission now converts every selected Zarr chunk from its declared array-axis
layout into the recorder's X-fastest XYZ word order before page upload. C/T dimensions remain in
the source stride calculation even though a selected chunk fixes them, preventing an unusual
declared X/Z/Y order from silently transposing volume content. The committed fixture and a focused
nonstandard-axis regression passed with desktop **23/23** normal tests plus strict clippy. This is
the prerequisite for future spatial multi-chunk tile assembly, not a claim that residency is
already implemented.

## 2026-09-16: bounded contiguous multi-chunk desktop admission

The desktop now uses the canonical tile assembler for exactly one selected image layer/channel.
It verifies loaded assets remain in the authorized plan order, assembles contiguous spatial chunks
into one X-fastest logical volume, rejects gaps and overlaps, and partitions the result into the
fixed one-to-four-page portable packet. The committed anisotropic fixture regression reads two
adjacent chunks and proves the resulting admitted volume is `64×32×8`. Multi-channel/layer
compositing and a UI residency selector remain explicitly separate; no source selection or blend
policy was silently changed. Desktop **24/24** normal tests passed.

## 2026-09-16: source-derived portable UI chunk selection

The desktop preparation command now returns its canonical `LocalOmeZarrSource` binding to the
webview. The portable UI retains that source only for the active local route and derives its
crosshair-centered bounded XYZ region `originXyz`/`extentXyz` from the current crosshair,
`chunkShape`, and declared `spatialAxesXyz`. It balances the rectangular extent across available
chunk-grid axes and limits full-size tile residency to the native four-page word and 4,096-address
capacities before asking the host to assemble the contiguous range.
Both camera redraw and portable annotation picking share that exact request constructor; a
crosshair move schedules a matching volume redraw. This removes the former hard-coded chunk-zero
assumption without granting the browser a new source or depth authority. The UI admission harness,
desktop **24/24** normal tests, strict desktop clippy, rustfmt, whitespace check, and release CSR
build passed. The complete local verifier also passed before this final selector generalization:
workspace tests; Palace-core **38/38**; local desktop smoke; ZIP/shader checks; portable llvmpipe
synthetic and real-fixture runs; UI admission; and release CSR build.

## 2026-09-16: explicit portable channel-page packet contract

`NativePortableVolumeInput` now carries an ordered `PortableVolumeChannel` range and transfer for
each direct channel. Every channel must cover the shared XYZ word count in its own consecutive
page range, so a partial page can never be reinterpreted as another channel's prefix. The legacy
single-channel constructor produces this same representation. The current native WGPU recorder
intentionally rejects multiple channels until its shader performs real composition; it does not
discard or arbitrarily select a channel. Render **22/22** tests and strict render/WGPU clippy
passed.

## 2026-09-16: native direct multi-channel composition

The native WGPU direct-volume recorder now consumes one to four explicit channel page ranges.
It uploads every channel into its own static range, addresses each range through a channel-indexed
page table, windows and colours each channel in linear light, then additively composes channels at
each ray sample before the existing front-to-back volume integration. Desktop preparation now
retains all bounded active OME display channels and desktop admission independently canonicalizes
and assembles their contiguous XYZ tile sets before assigning those ranges. A two-active-channel
desktop regression verifies red/green OME transfers and page contents; the opt-in local WGPU test
verifies both colours reach rendered pixels. Desktop **25/25** normal tests, render **22/22**,
WGPU normal tests, both WGPU adapter tests, and strict clippy passed. The complete local verifier
also passed after this change: workspace tests, Palace-core **38/38**, local desktop smoke,
ZIP/shader checks, portable llvmpipe synthetic and real-fixture runs, UI admission, and release
CSR build.

## 2026-09-16: physical transform inverse for native scene composition

`LayerTransform` now exposes the exact inverse world-to-voxel coordinate mapping and the separate
world-direction-to-voxel mapping for its validated anisotropic scale and translation. This is the
shared mathematical boundary for a future native multi-layer packet: one physical camera ray can
be converted independently into each layer's local voxel ray without translating directions or
collapsing anisotropic spacing. Scene **7/7** tests and strict scene clippy passed.

`newvolim-render` now carries the matching typed `PortableWorldRay` and `PortableLayerRay`
conversion. A normalized world ray converts through a layer's inverse transform and admitted
global voxel origin into a local page ray, intentionally retaining the non-normalized local
direction so its ray parameter remains a world distance. Render **23/23** tests and strict
render clippy passed.

`NativePortableSceneInput` now validates the next boundary: ordered layer descriptors, physical
transforms, global admitted voxel origins, scalar types, per-layer dimensions, and absolute
channel page ranges all bind to one fixed four-page submission. Its two-layer regression proves
that page ownership and world-ray-to-layer conversion remain distinct across transformed layers.
Render **24/24** tests and strict render clippy passed.

`NativePortableSceneCameraDrawInput` now binds that scene packet to one bounded normalized
physical-world ray per physical output pixel, retaining the existing projected-annotation limits.
The renderer can therefore receive camera-complete scene authority without reusing a
single-volume local ray table. Render **24/24** tests and strict render clippy passed.

## 2026-09-16: bounded native scene GPU records and ray intervals

`newvolim-wgpu-frame` now exposes a fixed 64-word (four 64-byte records) scene-layer upload
boundary. It preserves scene order, dimensions, local voxel origins, per-layer channel-table
offsets, and physical scale/translation while rejecting non-`uint16` layers, unsupported channel
counts, origins outside the `u32` GPU record range, and transforms outside the native `f32`
range. The companion `portable_scene_ray_ranges` derives the exact union of each physical camera
ray's admitted layer intervals, uses an explicit empty `[0,-1]` miss range, and rejects requests
above its 4,096-step portable bound. This is a validated admission boundary for the upcoming
world-ray scene shader, not a claim that multi-layer pixels are already rendered. The focused
WGPU record/range test and strict WGPU clippy passed.

The layer table now has its matching fixed 16-channel transfer/page-range table: each eight-word
record retains an absolute static-page range, native-unit window, opacity, and linearized sRGB
colour. Layer channel offsets therefore identify concrete records instead of an implied ordinal.
The same focused record test proves both table offsets and transfers.

## 2026-09-16: executable native world-ray scene composition

`render_portable_scene_camera_draw` now uploads the typed scene records, its flat shared-page
lookup table, and the eight-word physical world-ray/interval record for every output pixel to a
separate bounded WGPU compute pass. The shader transforms a common world sample through every
layer's own scale, translation, and admitted voxel origin; accumulates each layer's selected
channels; composites layers in declared order; and retains the first-opacity physical ray
distance. It hard-caps the already-admitted loop at 4,096 iterations. Local adapter execution
tests prove a world ray samples a static page and prove a later opaque overlapping layer replaces
the earlier layer without page aliasing. Normal WGPU tests, both scene adapter tests, strict
WGPU clippy, formatting, and whitespace checks passed. Scene annotation records are not yet
consumed by this new pass, so this is not a completion claim for the general annotation route.

## 2026-09-16: desktop shared-page scene admission

`LocalSession::native_portable_scene_page_admission` now turns every selected local image layer
and its contiguous authorized spatial-channel tiles into the typed `NativePortableSceneInput`
consumed by the native world-ray pass. It assigns absolute ranges in one shared four-page pool,
retains each layer's physical transform, voxel origin, local dimensions, scalar type, and ordered
channel transfers, and rejects plan/loaded-order mismatches, differing per-layer channel extents,
overflow, and extra assets. The matching Tauri command accepts only the bounded XYZ region and
derives all page/source details from the host session. Desktop **25/25** normal tests and strict
desktop clippy passed. The scene-specific projected annotation stream remains the next integration
step.

## 2026-09-16: executable trusted desktop scene route

The desktop now has a complete host-owned scene-camera admission and render command. It derives
the bounded shared-page scene from its authorized local tiles, projects stable annotation records
through the matching Palace camera, converts the reference layer's fitted local rays into
normalized physical world rays, and dispatches the scene WGPU pass before returning the ordinary
bounded colour plus PFM payload. The scene shader receives annotations and rays in one packet;
local adapter evidence proves a front green point is visible while a behind blue point is occluded.
The full local verifier passed after this route: workspace tests, Palace-core tests, desktop smoke,
WGPU probes, UI admission, and release CSR build.

Palace core now also has `WgpuSubmissionTracker`, a backend-owned monotonic submission/completion
ledger intended for `Queue::submit` plus completion callbacks. Its focused test passed. A fresh
strict Clippy baseline for legacy Palace is currently unsuitable as a quality gate: after fixing
five simple dependency lints, current Clippy reports hundreds of unrelated pre-existing legacy
findings across core/Vulkan code. The new desktop/render crates remain strict-clean.

`WgpuPagePool` now provides the matching Palace-owned portable residency boundary: it leases only
contiguous static ranges, distinguishes active leases from released-but-in-flight ranges, and
reclaims a range only after `WgpuSubmissionTracker` confirms its last-use epoch. The real Palace
WGPU spike leases all four static pages, releases them with its submitted epoch, then proves they
are reclaimable after device completion. Both the focused pool regression and the real local WGPU
spike passed; the next Palace increment is a first operator bridge on this residency contract.

The first two bounded operator-kernel proofs now execute in that same local WGPU spike. Rechunk
transposes two X-fastest `u32` rows from `[10,11,20,21]` to `[10,20,11,21]`; resample performs
nearest-neighbour 2× row downsampling from `[0,10,20,30]` to `[0,20]`. Both use explicit storage
bindings, pipeline layouts, dispatch, GPU readback, and exact host assertions. The focused
adapter-gated tests and full spike passed on the local Quadro RTX 5000/Vulkan adapter. These are
intentionally bounded kernels, not yet a claim that Palace tensor operators themselves select the
WGPU backend; connecting their real tensor/page inputs is the next locally achievable step.

`WgpuPageResidency` is now the Palace-owned admission façade over the page pool. It returns
opaque owner/range leases, refuses duplicate active work items, and retires each lease at its
last-use submission epoch. Rechunk and resample each reserve one 4 MiB source page plus one 4 MiB
destination page through that façade, dispatch their WGPU kernel, and admit a replacement
two-page work item only after completion. The focused Palace-core residency test, both
adapter-gated kernel tests, and the full Quadro WGPU spike passed. This is a usable bounded
recorder/residency seam; the existing Vulkan-bound tensor scheduler still needs a backend-neutral
device/request interface before those production operator closures can choose WGPU.

The reusable recorder now lives in the new `palace-wgpu` workspace crate rather than in the
diagnostic executable. `WgpuOperatorRecorder` owns bounded two-page source/destination admission,
queue completion, and fixed rechunk/resample storage dispatches; `palace-wgpu-spike` calls that
public API for its readback assertions. Both adapter-gated operator regressions and the complete
llvmpipe spike passed after extraction. The scheduler is still explicitly Vulkan-bound, so this is
the concrete portable recording backend—not a premature claim that ordinary tensor requests have
already switched backends.

`palace-core::gpu` now owns `PortableOperatorRequest`: a validated operator kind plus distinct
source/destination page-owner identities and fixed input/output word bounds. The WGPU recorder
accepts this request directly, rather than accepting recorder-private owner IDs. A live adapter
regression submits a caller-provided request (`701 → 702`) and verifies the exact rechunk
readback. Palace-core's request validation test, all three adapter-gated recorder regressions,
the complete spike, and strict Clippy for `palace-wgpu` passed. The remaining scheduler work is
therefore precise: make `OpaqueTaskContext` able to lend a selected portable device/storage
backend alongside its present Vulkan-only `device_contexts` map.

That runtime seam is now implemented. `RunTimeBuilder::portable_operator_recorder` retains an
owned core-defined recorder; resolve-time `ContextData` and `OpaqueTaskContext` lend it as an
explicit optional capability while preserving every existing Vulkan device request. `palace-wgpu`
provides the owned WGPU implementation. The adapter-gated integration test constructs a real
runtime with that recorder, obtains it from a real task context, records a core resample request,
and receives `[0,20]`. The production rechunk/resample closures have not yet opted into that
capability because their data requests and output slots are still Vulkan storage handles; that
migration is now the next bounded implementation item rather than an unmodelled runtime gap.

The recorder boundary now accepts `PortableTensorPage`, an owner-tagged host-visible page capped
at the selected 4 MiB storage-page size. The core trait checks that its owner matches the request
source and returns a page owned by the request destination. The live WGPU regression verifies
both the destination owner/readback words and rejection of an owner mismatch. This is the first
portable tensor storage value; production tensor operators still need adapters from their generic
CPU/Vulkan handles to this `u32` page representation before they can select the recorder.

`PortableTensorPage` now has explicit lossless conversion for the viewer's common `u16` scalar
tensors: upload widens values element-by-element to `u32`, and readback checks every word before
narrowing. The focused core regression covers zero, maximum `u16`, and an overflowing portable
word. This avoids a host-alignment/endianness cast at the eventual CPU tensor adapter boundary.

The portable resample is no longer restricted to its initial four-word probe: a bounded request
now carries arbitrary input and output word counts within one page, and the WGPU recorder emits
enough 64-thread workgroups for the output. The live adapter regression resamples eight values to
three (`[0,10,20,30,40,50,60,70] → [0,20,50]`), confirming the nearest-neighbour index rule on a
real non-probe page. This is a flattened scalar-page kernel; ND tensor coordinate transforms and
the production scheduler's CPU-page adapter remain the next work.

The CPU side now has the matching typed page adapter: `PortablePageScalar` supports lossless
`u16` and `u32` conversion, `PortableTensorPage::resample_scalars` retains explicit page owners,
and `operators::resample::portable_resample_cpu_page` exposes the path at the operator boundary.
Its focused two-dimensional `u16` regression uses the shared ND layout and rejects a short source
page. The existing production `resample_transform` remains Vulkan-storage-backed; replacing its
GPU handles with this typed CPU-page path is deliberately still outstanding rather than implied by
the helper's existence.

`PortableResampleLayout` now defines the missing ND coordinate rule for one-to-three-dimensional
scalar pages. It uses Palace's fastest-last row-major indexing and maps each output coordinate to
`floor(out * input_extent / output_extent)`; the core regression verifies a 2×4×8 → 2×2×3
anisotropic mapping including the final flattened source index. The current WGPU dispatch remains
the flattened 1D form, so binding this layout as a bounded uniform is the next recorder step.

The direct browser OME-Zarr route now accepts one to four semicolon-separated `Z,Y,X` chunk
coordinates. It validates every coordinate and duplicate, fetches only the corresponding reviewed
assets, enforces `chunks × channels ≤ 4`, and turns each spatial brick into an ordered static-page
layer. Each layer retains its normalized extent and origin transform while the camera continues to
use the full source extent. The UI admission fixture verifies two adjacent X chunks fetch separate
paths and produce transforms `[-0.5, +0.5]`; the Node admission harness, wasm32 UI build,
formatting, and whitespace check passed. This is bounded browser multi-chunk residency, not a
claim that browser projected annotations share the desktop or Palace camera authority.

The complete `sh scripts/verify_local.sh` baseline passed after those additions on 2026-09-16:
strict local checks, the serialized Palace-core suite **40/40**, local desktop smoke, ZIP and
shader checks, portable WGPU synthetic and committed-fixture llvmpipe runs (both reporting the
rechunk and resample readback values above), UI admission, and release CSR build. The known
transitive `proc-macro-error2` future-incompatibility notice and absent optional Vulkan validation
layer were the only notices; deferred platform, credential, and production-scale gates remain
outside this local evidence.

The portable resample now has a real bounded tensor-operator bridge, not only a helper: the
single-chunk lossless scalar path (`u16` and `u32`) reads a normal Palace CPU chunk, converts it
through the owner-tagged page contract, uses the shared 1–3D layout, and writes a normal output
slot. End-to-end regressions start from owned 2×4 tensors and verify the 2×2 result for both
scalar widths. It is deliberately CPU-page-only and single-chunk unless a runtime recorder is
installed; the existing generic `resample_transform` is still the Vulkan storage path.

Portable rechunking now has its corresponding CPU scheduler bridge. `PortableRechunkLayout`
describes a bounded 1–3D output region inside one complete source page using the shared
fastest-last ordering, and `portable_rechunk_cpu` materializes every full output chunk through
normal Palace CPU storage. Its end-to-end 2×4 fixture splits the source into four 1×2 chunks and
reassembles `[0, 1, 2, 3, 4, 5, 6, 7]`. It treats allocated padding explicitly rather than
silently as source data. The corresponding `RechunkNd` request now binds a
fixed thirteen-word layout uniform in `palace-wgpu`; real adapter extraction returns `[5, 6]` for
the offset 2D fixture, and a runtime-installed recorder produces all four padded-edge output
chunks.

Partial edge chunks are now admitted rather than rejected. A rechunk layout carries both logical
and allocated output dimensions; every allocated cell outside the source volume is explicitly
zero-filled. The CPU operator regression verifies a 2×4 source rechunked with 1×3 storage strides
as `[0,1,2]`, `[3,0,0]`, `[4,5,6]`, `[7,0,0]`; the WGPU readback independently verifies the same
zero-padding rule. This removes the padding ambiguity from the bounded portable contract, though
the original generic Vulkan closures have not yet been redirected to it.

The public Python `TensorOperator.rechunk` route now selects the dynamically typed portable
rechunk bridge for scalar `u16`/`u32` tensors with one complete source page; unsupported and
multi-page requests retain the existing Vulkan closure. A focused core regression round-trips the
dynamic `DType` boundary, reads the normal output chunks, and verifies the padded edge values;
the portable core suite now passes 16 tests.
`py-palace` now explicitly enables the `python` features it consumes from `palace-core` and
`state-link`; its PNG-only feature set compiles successfully without the unavailable optional
FFmpeg/video development dependency. `scripts/verify_local.sh` now runs that exact PNG-only
binding check as part of the reproducible local baseline.

The WGPU request contract now also carries the layout's fixed seven-word uniform. The recorder
binds that uniform and performs the same row-major nearest-neighbour mapping for one to three
dimensions, instead of treating all portable pages as a flattened 1D transform. The local adapter
suite passed all six ignored recorder/runtime tests, including a real 2×4 → 2×2 WGPU readback of
`[0, 2, 4, 6]`. The runtime integration regression also installs `WgpuRuntimeRecorder`, requests
the normal output tensor chunk, and verifies that same result through the recorder-selected path.
A strict clippy attempt remains unsuitable as a gate on this checkout because the
current toolchain promotes 378 pre-existing `palace-core` lints to errors before it reaches
`palace-wgpu`; focused formatting, core tests, adapter tests, and whitespace checking passed.

The portable resample bridge now also has a dynamically typed public-core entry point for
single-chunk scalar `u16` and `u32` tensors. Its regression starts as a `DType` tensor, invokes
the bridge, converts the result back to its static scalar type, and reads the normal output page
as `[0, 2, 4, 6]`; the focused portable core suite passes **17/17**. This is intentionally not
used as a shortcut in Python's arbitrary `resample_transform`: that established Vulkan operator
rounds center-based transformed coordinates and supports borders, whereas the portable contract
is floor-nearest and bounded to an in-page downsample. Keeping those paths separate preserves
existing image semantics while providing an explicit portable API for a future compatible
resample scheduler.

The dynamic `u32` bridge is also covered by the actual recorder-selection route: a runtime
installs `WgpuRuntimeRecorder`, the test crosses `DType → StaticElementType<u32>` for ordinary
page readback, and the recorded 2×4 → 2×2 page is `[0, 2, 4, 6]`. The complete local ignored
adapter subset now passes **11/11** (including all direct-layout, recorder, rechunk padding, and
runtime bridge tests), alongside the **17/17** focused core suite.

Portable rechunk planning no longer requires its input to be a single Palace chunk. The scheduler
now copies the logical cells of every source CPU chunk into one bounded row-major portable page,
skipping each source edge chunk's allocation padding before extracting the requested chunks. The
new typed and dynamic regressions split a 2×4 source into four chunks (including 1×3 edge-memory
padding), reassemble it through
the portable bridge, and verify both ordinary output values and the public `DType` boundary.
Python admits this path whenever the complete logical scalar source fits the 4 MiB page cap;
larger inputs continue on the established Vulkan planner. The focused portable core suite now
passes **19/19**, and the PNG-only public-binding check passes (with only pre-existing unused
variable warnings in optional IO/transfer-function paths).

The compatible floor-nearest portable resample scheduler now uses that same bounded multi-chunk
assembly rule. Its typed and dynamic tests read a padded 1×3-chunked 2×4 source, produce the
correct 2×2 `[0,2,4,6]` page, and prove the source allocation padding is ignored. The
runtime-installed WGPU recorder test was upgraded to use that multi-chunk source too, exercising
both scheduler assembly and the shared ND WGPU resample layout. The focused portable core suite
passes **21/21** and the full local adapter subset remains **11/11**. This remains a deliberately
separate contract from arbitrary transform resampling; it is now a multi-source bounded contract,
not a single-source-chunk shortcut.

Portable resample now also plans chunked output. Its fixed sixteen-word CPU/WGPU uniform carries
global output dimensions, output chunk origin, logical extent, and allocated memory extent; edge
memory is explicitly zero-filled. The offset/padding oracle checks a chunk at global output index
two returns `[12,0]`, and the adapter recorder independently returns the same words. The operator
regression emits all four padded 1×3 chunks of a 2×4 output. This advances the bounded scheduler;
the remaining general transform path still has distinct round/border semantics.

`py-palace` now exposes that contract as `TensorOperator.portable_resample_nearest(output_size)`.
It validates dimensionality and routes through the dynamic scalar bridge without changing the
long-standing arbitrary `resample_transform` API. The PNG-only binding build verifies the public
method under the portable local feature set.

The runtime-selected WGPU resample regression now requests all four output chunks rather than
only the first page, and reads `[0,2,4,6]` in chunk order. This closes the scheduler-level
coverage gap between the direct chunk-origin/padding recorder test and normal Palace task output.

`palace-core` now owns a small backend-neutral `PortableFrameAttachments` value that validates a
single render invocation's RGBA bytes and first-opacity `f32` surface have exactly matching pixel
extents. `palace-png::FrameAttachments` converts to it only when the raycaster supplied its
validated PFM source; colour-only frames return no portable paired attachment. This begins the
renderer boundary without coupling `newvolim-render` (which deliberately has no Palace dependency)
back into Palace core.

The native desktop WGPU route now constructs that core paired value directly and the PNG+PFM
transport adapter encodes it without first wrapping it in the Vulkan-era `RgbaFrame` and
`RayDistanceFrame` readback types. Conversely, a validated Palace raycast attachment converts to
the same core value before encoding. Focused regressions prove both paths produce byte-identical
PNG and little-endian PFM output for the same colour/depth pair, and the desktop payload test
checks the portable path retains `RayDistanceF32` plus the exact `Pf 2×1` sidecar. Local results:
`cargo test --offline -p palace-png -- --test-threads=1` **9/9** and
`cargo test --offline -p newvolim-desktop portable_frame_payload_preserves_its_paired_depth_sidecar -- --test-threads=1` **1/1**.

The first sliceviewer migration unit is now explicit in `palace-core` as
`PortableOrthogonalSliceLayout`. It extracts a bounded complete `[z,y,x]` scalar page into the
legacy sliceviewer’s `[vertical,horizontal]` orientation for every selected axis, with a fixed
nine-word future-recorder uniform and zero-filled allocated edge columns. Its CPU oracle verifies
all three orientations against source-index values and rejects invalid axes, slices, or undersized
output memory. This intentionally stops before transfer evaluation and physical affine sampling;
those remain separate contracts rather than being silently approximated by the initial route.
`cargo test --offline -p palace-core portable_ -- --test-threads=1` passed **16/16**.

That layout is now a `PortableOperatorRequest::slice_orthogonal` rather than CPU-only design
work. `palace-wgpu` records the same fixed nine-word source-axis/slice/logical-memory contract
and the local adapter regression reads the complete selected Z plane with each allocated trailing
column zero-filled. `cargo test --offline --manifest-path palace-dev/Cargo.toml -p
palace-wgpu-spike bounded_wgpu_recorder_extracts_the_shared_orthogonal_slice_layout -- --ignored
--test-threads=1` passed **1/1**. Affine physical sampling and transfer-function evaluation are
still intentionally separate follow-up passes. The adapter regression now covers all three legacy
slice axes, not merely the Z plane, including the padded Z-plane allocation; it confirms the
recorder's three explicit axis branches agree with the shared CPU layout.

`palace-core` now also owns `PortableRayInterval`, the trusted physical-ray input for the later
portable raycaster. It rejects non-unit or non-finite directions and invalid finite intervals;
its CPU oracle maps an in-range distance to the exact physical point. This keeps a future
first-opacity output in the same distance authority as camera rays and annotation depth tests.
Its slab-intersection helper now clips that interval to an admitted physical AABB without
renormalizing or changing the distance parameter; the oracle verifies both the clipped `[1,3]`
interval and a parallel-ray miss.
Each validated interval now has a fixed eight-word storage-buffer representation—origin XYZ,
unit direction XYZ, then physical near/far—with a bit-round-trip regression.

The slice contract also now has the scheduler-facing owner-tagged page fallback used by the other
portable operators. A `u16` page can execute `slice_orthogonal_with` or its typed scalar wrapper,
and its destination owner/counts exactly match `PortableOperatorRequest::slice_orthogonal`.
The regression verifies padding at the end of *each* allocated row, preventing a future caller
from treating the plane as tightly packed. `cargo test --offline -p palace-core
portable_orthogonal_slice -- --test-threads=1` passed **2/2**.

`PortableTransferFunction` now makes the other first-frame input explicit too: a bounded RGBA
LUT with finite ordered limits and Palace’s established normalize/truncate/clamp lookup rule.
The portable CPU oracle checks below-range, interior-bin, and upper-edge mapping against that
rule, so the future WGPU LUT binding cannot quietly adopt different transfer semantics.
`cargo test --offline -p palace-core portable_ -- --test-threads=1` passed **19/19** after the
trusted-ray addition.

The CPU slice oracle now composes extraction and transfer evaluation into RGBA output. Crucially,
allocated edge cells remain transparent black rather than being classified as scalar zero, while
an in-volume scalar zero still follows the caller's LUT. This preserves the distinction between
real data and edge padding before the equivalent WGPU LUT binding is added.

The transfer table now also exposes one little-endian `u32` per `[r,g,b,a]` entry for a portable
storage-buffer upload, with a round-trip byte-order regression. This prevents host endianness or
packed-colour interpretation from becoming an untested shader-side convention.
Transfer admission is also bounded to one 4 MiB portable page; an oversized LUT is rejected
before it can bypass the residency budget used by tensor pages.

The legacy Vulkan rechunk path now follows the same edge-padding rule as the portable path:
brand-new destination allocations are zero-filled before overlap copies. Reused progressive tiles
are deliberately not cleared, preserving already-final source regions. This removes the prior
uninitialized-outside-region hole while retaining incremental reuse.
`cargo test --offline -p palace-core rechunk -- --test-threads=1` passed **6/6**, including the
Vulkan rechunk regression across multiple edge chunk shapes.
The dedicated Vulkan regression additionally reads the physical `[1,3]` edge allocation and
observes `[3,0,0]`, proving the assertion covers padding bytes rather than only logical pixels.

`PortableTransferFunction` now has a CPU oracle for Palace DVR's step-size-corrected
front-to-back compositing. It records the first contributing opacity distance (or `+∞` for a
transparent ray) and preserves the legacy 0.95 early-termination threshold.

## Portable ordered-scene WGPU DVR parity (2026-09-18)

The fixed-binding Palace WGPU ordered-scene compute pass now has local-adapter parity with
`PortableDvrSceneFrameInput::render_cpu`, and the per-frame CPU comparison guard is gone.

The divergence that blocked this milestone was **not** the metadata/LUT layout. The production
shader now writes a bounded pixel-zero trace of its own decoded state (ray words, step count,
AABB hit count, channel record base, scalar, transfer range, LUT offset/count/index/word, alpha,
layer/scene sample alpha, and final colour). Reading that trace from the existing red/green/blue
adapter fixture showed every decode step correct — 512 steps, 768 AABB hits, first hit at step
256, scalar `1`, LUT index `1`, packed `0x800000FF`, alpha `0.5019608` — while the composited
colour was `NaN`.

Cause: `pow(0.0, 1.0)` returns `NaN` on this local driver. A fully opaque transfer entry makes
the Beer-Lambert base `1.0 - 255/255` exactly zero, and SPIR-V/GLSL leave `pow` undefined at
zero, whereas Rust's `powf(0.0, 1.0)` returns `0.0`. One NaN alpha then poisoned every later
source-over accumulation, so the frame read back fully transparent with `+∞` depth.

Fix: a `step_corrected_alpha` helper that returns `1.0` when the base is zero and otherwise
evaluates `1.0 - pow(base, exponent)`. This is exact parity with the oracle rather than a
tolerance. The identical expression was latent in the two older DVR shaders
(`record_dvr_frame` and `record_dvr_page_frame`); both are fixed the same way. Their existing
fixtures used alpha `128` only, which is precisely why the defect stayed hidden.

Evidence on this Linux local adapter:

```text
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-wgpu -- --include-ignored
```

passed **2/2**, including
`ordered_scene_dvr_dispatch_matches_cpu_oracle_for_channels_and_layers`, which now compares all
RGBA bytes plus the paired depth and additionally asserts the physical first-opacity distance
`1.0019531`.

```text
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-wgpu-spike \
  fully_opaque_entry -- --ignored
```

passed **2/2**. Both new fixtures were confirmed to be genuine regressions: reverting only the
`step_corrected_alpha` call sites makes both fail, and restoring them makes both pass.

```text
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-core --lib \
  portable_scene_dvr_composites_channels_and_layers_in_physical_order
cargo test --offline --manifest-path crates/newvolim-desktop/Cargo.toml \
  native_portable_scene_picker_uses_ordered_scene_renderer_depth -- --ignored
```

each passed **1/1**.

The desktop annotation-free scene route also no longer renders every frame twice. It previously
evaluated `render_cpu()` unconditionally before attempting the recorder; the oracle is now
evaluated only when adapter acquisition or recording fails. `cargo check --offline
--manifest-path crates/newvolim-desktop/Cargo.toml` passes with only the two pre-existing unused
slice-helper warnings.

The full spike suite also passes, but **only serially**:

```text
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-wgpu-spike -- \
  --include-ignored --test-threads=1
```

passed **24/24 in 7.5 s**. The same suite run at the default parallelism does not finish: it was
left for fourteen minutes at ~100% CPU with no test completing, and had to be killed. Every test
also passes individually in one second or less, so this is concurrent WGPU adapter/device
acquisition on this host, not a slow test. Treat `--test-threads=1` as required for any adapter
suite here, exactly as the existing `palace-core portable_` evidence commands already do.
Diagnosing the parallel deadlock itself is open work, not a gate on the scene-DVR milestone.

## Scene picking on Palace renderer-owned depth (2026-09-18)

`pick_native_portable_scene_annotation_for_session` no longer reads its occluding distance from
the native scene renderer. The new `portable_scene_pick_depth` prefers Palace's ordered-scene
attachment and retains the native renderer only for packets Palace cannot admit or a host without
an eligible adapter.

Both renderers keep their depth attachment volume-only: `SCENE_SHADER` and the direct camera
shader in `crates/newvolim-wgpu-frame/src/main.rs` colour-composite a covered annotation but
never write it into `ray_distance`. So the reason to prefer the Palace attachment is not that the
native one is annotation-contaminated — an earlier draft of this log said that and it was wrong.

The real difference is the first-opacity *rule*. The native pass records `travelled` at the step
where **accumulated** opacity first reaches `0.01`. Palace records the step-centre distance of
the first sample with **any strictly positive** opacity, which is the rule
`PortableDvrSceneFrameInput::render_cpu` defines and the shader is parity-tested against. Those
are different surfaces, so routing the picker changes the occlusion boundary rather than merely
changing which crate computes the same number. One renderer owning depth is the ownership
property PLAN.md §9.3.1 asks for; volume-only is what makes an annotation unable to occlude
itself.

Reaching this exposed the actual reason the Palace scene route was almost never taken. The
desktop scene adapter rejected the whole packet when *any* ray missed the admitted scene AABB
(`scene world ray misses admitted layers`). A framed camera necessarily has such pixels, so
essentially every realistic camera silently fell back to the native renderer — the earlier
"annotation-free scene rendering calls the Palace recorder" claim was true of the code path but
rarely true of a real frame.

A missed ray is instead admitted as a degenerate `near == far` interval. `PortableRayInterval`
already accepts that (`far >= near`), it carries no samples, and both the core oracle and the
WGPU shader render it as a transparent pixel with `+infinity` depth. This is representation, not
tolerance: nothing about the composition rule changed.

Evidence on this Linux local adapter:

```text
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-wgpu -- \
  --include-ignored --test-threads=1
```

passed **3/3**, now including
`ordered_scene_dvr_dispatch_matches_cpu_oracle_for_a_mixed_hit_and_miss_frame`, which renders one
hitting and one missing ray in the same frame and compares both attachments against the oracle.

```text
cargo test --offline --manifest-path crates/newvolim-desktop/Cargo.toml -- \
  --include-ignored --test-threads=1
```

passed **38/38 in 29.2 s**, including the new
`portable_scene_pick_depth_is_palace_volume_depth_and_occludes_behind_annotations`. That fixture
opens the committed anisotropic fixture, builds the real scene camera packet, and asserts:

- the packet is Palace-admissible and contains **both** an opacified pixel and a missed pixel, so
  the degenerate-ray admission is exercised against a real camera rather than only in a synthetic
  core fixture;
- the picked depth at a hit pixel, a missed pixel, and both frame edges equals the Palace
  attachment word exactly; and
- with that real depth, an annotation at half the first-opacity distance is selected while one at
  one-and-a-half times it stays occluded.

The earlier `native_portable_scene_picker_uses_ordered_scene_renderer_depth` smoke is retained but
is not the routing evidence: its assertion is `is_none() || annotation_id == 0`, which passes
either way.

Two consequences are recorded rather than claimed as resolved:

1. Palace is now genuinely authoritative for annotation-free scene frames on this host. Any
   visible difference from the native compositor's older ad-hoc blend semantics will now appear in
   the desktop scene view. PLAN.md is explicit that the Palace oracle is the correct authority, so
   this is intended, but it has had no side-by-side visual comparison yet.
2. Each scene render and each pick acquires a fresh `wgpu::Instance`/adapter/device (three call
   sites in `crates/newvolim-desktop/src/main.rs`), and a pick renders the whole frame to read one
   depth pixel. That predates this change and applies to the direct route too, but it is a real
   interactive-latency problem and a device-cache design decision, not a cleanup.

## Palace-owned projected annotation compositing (2026-09-18)

This completes PLAN.md §9.3.1 item 3. Palace now owns **both** passes of an ordered scene frame:
the volume raymarch and the depth-tested projected-annotation composite over its own first-opacity
attachment. An annotation-bearing packet is no longer a reason to leave the portable route.

`palace-core::gpu::PortableAnnotationCompositeInput` is the new bounded contract: a rendered
`PortableFrameAttachments` plus up to 4 096 ordered `ProjectedAnnotationPrimitive` records, with
`composite_cpu()` as the oracle and `to_gpu_input()` as the fixed-binding serialization.
`WgpuOperatorRecorder::record_annotation_composite` is the matching compute pass, reachable
through the existing recorder trait as `record_portable_annotation_composite`.

Three semantics are stated deliberately, because two of them differ from the viewer's older pass:

1. **The first-opacity attachment is returned unchanged.** It stays the volume's surface, which is
   what annotation picking and any later pass must test against. An annotation that wrote depth
   would occlude itself.
2. **The nearest covering primitive wins**, with declaration order breaking an exact tie. The
   native pass let the *last* primitive in the packet win regardless of depth, so two overlapping
   annotations previously resolved by packet order. Nearest-wins is what a depth-buffered
   rasterizer of opaque primitives produces, and depth-correct compositing is the stated point of
   owning this pass (PLAN.md §9.4).
3. **Colour is written in the attachment's own encoding.** The desktop scene transfer LUT carries
   encoded sRGB bytes, so the Palace frame's RGBA is in that space and a primitive's `color_srgb`
   is copied directly. The native shaders linearize because they target a linear RGBA16Float
   surface presented through sRGB; copying that conversion here would double-apply it.

Coverage and distance interpolation are *not* new policy — they are the meaning of the thirteen-
word record, so point/segment/triangle coverage, clamped segment parameterization, and barycentric
distance interpolation are kept literally identical between the oracle and the shader, including
the `1e-6` degenerate-triangle guard.

Evidence on this Linux local adapter, all serial:

```text
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-core --lib gpu::tests -- \
  --test-threads=1
```

passed **9/9**, including four new oracle fixtures: nearest-wins proven by asserting the reversed
packet gives the same pixel, a primitive hidden behind the surface while one exactly at it stays
visible, segment and triangle distance interpolation checked against a limit that admits one end
and rejects the other, and the word serialization.

```text
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-wgpu -- \
  --include-ignored --test-threads=1
```

passed **5/5**, including
`annotation_composite_dispatch_matches_the_cpu_oracle_for_every_record_kind` — all three record
kinds plus a degenerate triangle in one 16x12 frame, over a non-uniform first-opacity surface with
a transparent column, with two overlapping points ordered so a last-wins implementation would
differ — and the empty-packet pass-through.

```text
cargo test --offline --manifest-path crates/newvolim-desktop/Cargo.toml -- \
  --include-ignored --test-threads=1
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-core --lib portable_ -- \
  --test-threads=1
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-wgpu-spike -- \
  --include-ignored --test-threads=1
```

passed **39/39**, **21/21** and **24/24**. The new desktop fixture
`palace_scene_annotation_composite_paints_over_its_own_frame_and_keeps_volume_depth` proves the
wiring: the session's own annotations convert to admitted Palace primitives, a primitive at
distance zero over an opacified pixel is painted, the local adapter matches the oracle exactly,
the volume first-opacity attachment is byte-identical afterwards, and
`render_native_portable_scene_camera_draw_for_session` returns a paired-depth portable payload
rather than falling back.

The desktop conversion goes through the typed projected record, not the word stream:
`project_session_annotation_records` is now the single projection, with
`project_session_annotation_words` and `palace_annotation_primitives` as its two consumers. A
future record-layout change therefore cannot silently desynchronize two decoders.

Recorded, not resolved: an annotated scene frame now acquires **two** adapters and devices —
one for the raymarch, one for the composite — on top of the per-frame acquisition already noted.
There are four `request_adapter` sites in the desktop crate. This is now the clearest remaining
interactive-latency problem on the portable route, and a shared device cache is a design decision
rather than a cleanup.

## Portable chunk-request translation and bounded residency plan (2026-09-18)

This is the **translation stage** of PLAN.md §9.3.1 item 4, not item 4 as a whole. Read the
"still open" list at the end before claiming otherwise.

Reading the normal planning seam first: `palace-core::operators::imageviewer::view_image` selects
its LOD through `sliceviewer::select_level` from the camera transform, then loops — render pass,
shader records page-table misses into a `RequestTable` and touches into a `UseTable`, host
downloads both, resolves the requests, re-renders — until `RequestTableResult::Done`, degrading to
`DataVersionType::Preview` on timeout. So "normal Palace level/chunk planning" means camera-driven
level selection plus *feedback-driven* chunk demand. It is not a caller-supplied region, which is
exactly what `LocalSession::local_layer_chunk_plan` takes today.

`palace-core::gpu` gains the piece that converts that demand into the portable representation:

- `PortableChunkGrid` — one level's voxel dimensions and chunk shape, with X-fastest chunk
  numbering matching Palace, and edge chunks clipped to the level so a planner never claims the
  source's allocation padding as real data.
- `PortableChunkPlan::from_demand(level, grid, demand, owner_base)` — takes `PortableFeedbackKey`
  demand (the portable request-table key that already existed) and produces an ordered page
  layout: each demanded chunk's origin, logical extent, page ordinal and first word, plus per-page
  word lengths and consecutive global owners.
- `PortableChunkPlanOutcome::ExceedsPortableBound { required_pages }` — the deterministic Vulkan
  fallback signal item 4 asks for. A working set needing more than four pages, or a single chunk
  larger than one page, is reported as an overflow with the page count it would need. It is never
  truncated, because a truncated plan would silently render a partial volume.

One non-obvious property is load-bearing and has its own test. A page ordinal **is** a static
binding slot in the portable shader, so the plan must be a pure function of the demanded set.
`PortableFeedbackTable::keys` iterates hash slots, so its order depends on the hash and on
insertion history: for chunks 0..4 at level 2 in a 64-slot table it hands them back as
`[2, 0, 3, 1]`. Letting that order choose page slots would make the same demanded set bind
differently from frame to frame and defeat residency reuse, so the plan sorts by chunk index. The
test asserts the table's own order really is `[2, 0, 3, 1]` before asserting the plan is
`[0, 1, 2, 3]`, so it cannot quietly become vacuous if the hash or probing changes — an earlier
draft of the test compared two insertion orders that did not collide and therefore proved nothing.

Evidence:

```text
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-core --lib \
  gpu::tests -- --test-threads=1
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-core --lib \
  portable_ -- --test-threads=1
```

passed **14/14** and **26/26**, including five new fixtures: X-fastest chunk numbering with
clipped edge extents, demand-order determinism, multi-chunk-per-page packing in index order,
bound overflow for both a too-large working set and an oversized single chunk, and rejection of
planning-invalid demand (a key naming another level, a chunk index outside the grid, owner zero)
with empty demand planning trivially.

The rest of the suite is unchanged by this addition:

```text
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-wgpu -- \
  --include-ignored --test-threads=1
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-wgpu-spike -- \
  --include-ignored --test-threads=1
cargo test --offline --manifest-path crates/newvolim-desktop/Cargo.toml -- \
  --include-ignored --test-threads=1
```

passed **5/5**, **24/24** and **39/39**.

Still open in item 4, and deliberately not claimed:

1. **Demand generation.** Nothing yet produces `PortableFeedbackKey` demand from a portable
   render pass. The portable scene shader has no page table to miss against and no request buffer
   to write misses into, so the feedback loop that makes this "normal Palace planning" does not
   exist on the portable path. This is the substantial remaining work.
2. **Level selection.** `select_level` is 2D-oriented (`RenderConfig2D`, D2 pixel transforms); the
   portable scene route has no camera-driven level choice at all yet.
3. **Desktop wiring.** The scene route still calls `LocalSession::local_layer_chunk_plan` with a
   webview-supplied chunk region, so the UI still decides which chunks are rendered.

## Shader-visible portable page table (2026-09-18)

Step 1 of the three-step demand-generation plan for PLAN.md §9.3.1 item 4. Item 4 remains open.

Palace's Vulkan page table resolves through 64-bit buffer addresses, which WebGPU does not
expose, so the portable path needs its own shader-readable residency map.
`palace-core::gpu::PortablePageTable` is a bounded open-addressed map from a
`PortableFeedbackKey` to a `PortablePageLocation` (page ordinal plus word offset, packed as
8 bits of page and 24 bits of word).

Two design points are deliberate:

1. **It uses the same probing rule as `PortableFeedbackTable`** — the same `feedback_hash`, the
   same linear probing, the same `max_probes` cap. A shader that misses a residency lookup will
   record that key into the request table, so if the two disagreed about which slot a key belongs
   in, host and shader would desynchronize and the renderer would re-request chunks that are in
   fact resident. A core test inserts the same keys into both tables and requires an identical
   occupancy pattern.
2. **A lookup is total.** An absent key returns `None`, never a sentinel location, and the
   readback carries a separate found flag rather than a reserved packed value. Page zero, word
   zero is a perfectly legal residency, so a sentinel would be indistinguishable from the first
   chunk of the first page. `PortablePageTable::from_plan` likewise returns `None` rather than
   accepting a dropped entry, because a dropped entry would make the renderer re-request a
   resident chunk forever.

`WgpuOperatorRecorder::record_page_table_lookups` is the matching WGSL lookup. It exists so the
shader's probing rule is held to the core oracle by test rather than by inspection.

Evidence on this Linux local adapter, all serial:

```text
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-core --lib \
  gpu::tests -- --test-threads=1
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-core --lib \
  portable_ -- --test-threads=1
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-wgpu -- \
  --include-ignored --test-threads=1
```

passed **18/18**, **30/30** and **7/7**. The four new oracle fixtures cover packed-location round
tripping with field-overflow rejection, a total lookup that distinguishes absence from page zero
word zero (including the same chunk index at another level, in-place replacement, and `clear`),
shared probing with the request table, and construction from a bounded chunk plan including
failure on a capacity too small to hold it.

The two new adapter fixtures are
`page_table_lookup_dispatch_matches_the_cpu_oracle_including_probe_chains` — a deliberately
collision-heavy 8-slot, 3-probe table fed twelve keys, asserting the fixture produced both
residency and probe exhaustion before querying resident keys, exhausted keys, keys at another
level and keys never offered — and
`page_table_lookup_dispatch_resolves_every_chunk_of_a_bounded_plan`.

Both were confirmed discriminating: changing a single bit of the WGSL hash constant
(`0x7feb352d` to `0x7feb352e`) fails both with "residency lookup diverged for chunk 1 level 5"
and "chunk 1 was not resolvable by the shader"; restoring it passes.

The rest of the suite is unchanged: `palace-wgpu-spike` **24/24** and the desktop crate
**39/39**.

Still open, unchanged from the previous entry: request emission from the scene shader, the host
re-render loop, camera-driven level selection for the portable route, and the desktop's
webview-supplied chunk region.

## Portable request emission from the shader (2026-09-18)

Step 2a of the three-step demand-generation plan for PLAN.md §9.3.1 item 4. Item 4 remains open,
and this primitive is **not yet wired into the scene shader**.

`WgpuOperatorRecorder::record_feedback_inserts` is the shader half of Palace's request table,
rebuilt without 64-bit atomics: one `atomicCompareExchangeWeak` per candidate slot over a
`array<atomic<u32>>` storage binding, following the same `feedback_hash`, linear probing and probe
cap as `PortableFeedbackTable::insert`. It returns both the resulting table words and each
invocation's outcome, so tests can hold the recorded set *and* the explicit lossiness to the host
oracle.

One shader detail is deliberate and commented in place. WGSL's compare-exchange is the **weak**
form and may fail spuriously, so a spurious failure retries the same slot under a bounded
four-attempt loop rather than advancing the probe; advancing would make the shader lossier than
the host oracle for no reason. A failure that reports a real occupant does advance the probe,
which is exactly what the sequential rule does on finding a slot taken.

Concurrency limits what can honestly be asserted, and the tests say so rather than pretending
otherwise. Slot assignment under contention depends on arrival order, and so does *which* keys
survive a probe-bound overflow, so identity of the surviving set is not asserted where it is not
determined.

Evidence on this Linux local adapter, all serial:

```text
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-wgpu -- \
  --include-ignored --test-threads=1
```

passed **10/10**, including three new fixtures:

1. `request_shader_records_the_same_key_set_as_the_host_oracle` — 24 distinct keys with room to
   spare must all report `Inserted` and must produce the oracle's exact key set. Set equality
   alone would survive a wrong hash, so this additionally records one uncontended key and requires
   it to land in the **same slot** the `PortablePageTable` puts it in. That shared slot rule is
   the reason a shader can miss a residency lookup and record the key without desynchronizing
   from the host, so it is worth pinning directly.
2. `request_shader_deduplicates_a_key_offered_by_many_invocations` — 512 invocations offering one
   key must leave exactly one occupied slot, one `Inserted`, and 511 `AlreadyPresent`.
3. `request_shader_drops_beyond_its_probe_bound_exactly_as_the_oracle_does` — 64 distinct keys
   into a 16-slot, 3-probe table. It first asserts the fixture overflows the *host* oracle too,
   then requires that the shader never writes past its table, that every occupied slot
   corresponds to exactly one reported `Inserted`, that no key was invented, that at least one
   `Dropped` was reported rather than silently overflowing, and that distinct keys never report
   as `AlreadyPresent`.

Both new behaviours were confirmed discriminating by mutation:

- Removing the two dedup checks from the shader makes the dedup fixture fail with the same key in
  eight slots instead of one (eight being the probe chain length).
- Changing one bit of the request shader's second hash constant (`0x846ca68b` to `0x846ca68c`)
  fails the slot assertion with "the request shader and the page table must place a key in the
  same slot". Set equality alone still passed, which is precisely why that assertion was added.

The rest of the suite is unchanged: `palace-core` **18/18** `gpu::tests` and **30/30**
`portable_`, `palace-wgpu-spike` **24/24**, desktop **39/39**.

Still open: wiring the lookup and the request write into the scene DVR shader's scalar fetch path
(step 2b), the host re-render loop (step 3), camera-driven level selection for the portable route,
and the desktop's webview-supplied chunk region.

## Demand-resident scalar fetch, oracle and shader (2026-09-18)

The addressing half of step 2b for PLAN.md §9.3.1 item 4. Item 4 remains open, and this is **not
yet wired into `SCENE_DVR_SHADER`** — that plumbing is the remainder of 2b.

`palace-core::gpu::PortableChunkedChannel` is the piece that lets a portable raymarcher stop
depending on a statically bound page range. A voxel resolves to a chunk key plus an intra-chunk
offset; the key resolves through a `PortablePageTable`; and a miss becomes
`PortableChunkedSample::Missing(key)` — a request to record — rather than a wrong scalar. `resolve`
is the CPU oracle.

The load-bearing detail is the **intra-chunk stride: it is the chunk's clipped logical extent, not
its nominal chunk shape.** An edge chunk is clipped to the level and `PortableChunkPlan` sizes it
by that clipped extent, so any other stride reads a neighbour's scalars at every level whose
dimensions are not an exact multiple of the chunk shape.

`WgpuOperatorRecorder::record_chunked_resolves` is the matching dispatch: chunk addressing,
residency lookup, and a recorded request on a miss, in one pass, reusing the request-emission
probing rule verbatim.

Evidence on this Linux local adapter, all serial:

```text
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-core --lib \
  gpu::tests -- --test-threads=1
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-core --lib \
  portable_ -- --test-threads=1
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-wgpu -- \
  --include-ignored --test-threads=1
```

passed **21/21**, **33/33** and **11/11**. Three new oracle fixtures cover the clipped edge stride
directly (a 6-wide level in 4-wide chunks, where voxel `(5, 3, 0)` must be offset `1 + 2*3 = 7`
and not `1 + 4*3 = 13`) plus an aliasing check that all thirty voxels map to distinct
`(chunk, offset)` pairs; residency versus a named missing key; and a whole-plan check that the
thirty voxels occupy words `0..30` of the single planned page exactly once, which ties the plan,
the page table and the addressing together.

The new adapter fixture
`chunked_resolve_dispatch_matches_the_cpu_oracle_and_requests_only_missing_chunks` plans two of
four chunks, resolves every voxel of the level plus three out-of-level coordinates, asserts the
fixture covers residency, misses and out-of-level voxels, holds every voxel to the oracle, and
requires the recorded request set to be exactly the two unplanned chunks — deduplicated across all
the voxels that missed them.

**A first version of that fixture was silently not testing the stride.** It planned chunks 0 and 3,
and chunk 3's resident voxels all sat on a single row of y, so the stride multiplier was never
exercised: replacing the shader's clipped `logical` with the nominal `shape` still passed. The
fixture now plans chunk 1 instead — the x-clipped chunk that still spans four rows of y — and
additionally pins voxel `(5, 3, 0)` to word `16 + 7`. With that change the same mutation fails
with "voxel [4, 1, 0] resolved differently on the adapter".

The rest of the suite is unchanged: `palace-wgpu-spike` **24/24** and desktop **39/39**.

Still open: wiring this into `SCENE_DVR_SHADER`'s channel loop so `channel_scalar` resolves
through the page table and records misses (rest of 2b), the host re-render loop (step 3),
camera-driven level selection, and the desktop's webview-supplied chunk region.

## The portable demand loop closes on a real GPU pass (2026-09-18)

Step 3 of the demand-generation plan for PLAN.md §9.3.1 item 4. This retires the item's central
risk — whether a feedback-driven residency loop converges on the portable backend — but item 4
is still not complete; see the end of this entry.

`palace-core::gpu::PortableResidencyLoop` drives what Palace's Vulkan viewers do with their
request/use tables. It owns planning only: which chunks must be resident and where they live once
they are. Uploading a planned chunk's scalars stays the caller's job, because the bytes come from
the caller's source.

`absorb` merges one pass's reported misses and replans, returning one of five outcomes:

- `Complete` — the pass reported no misses.
- `Planned` — new chunks were planned; upload and re-run the pass.
- `ExceedsPortableBound { required_pages }` — fall back to Vulkan for this frame. The loop's
  committed state is deliberately left untouched, so a later, smaller working set can still use
  what is already resident.
- `Exhausted` — the iteration cap was reached. Palace's Vulkan viewers present a preview version
  in the equivalent situation rather than spinning, and this is the portable equivalent.
- `Desynchronized { chunk_index }` — the pass reported a miss for an already-resident chunk. Host
  and shader disagree about residency, which would spin forever, so it is named rather than
  retried. This is the failure the shared probing rule between `PortablePageTable` and
  `PortableFeedbackTable` exists to prevent, and it is now observable if it ever happens.

Convergence rests on the request table being **lossy but monotone**: a pass may report only some
of its misses, but every accepted iteration plans at least one chunk that was not planned before,
so the demanded set strictly grows and the loop terminates at the page bound, the iteration cap,
or completion.

Evidence on this Linux local adapter, all serial:

```text
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-core --lib \
  gpu::tests -- --test-threads=1
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-core --lib \
  portable_ -- --test-threads=1
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-wgpu -- \
  --include-ignored --test-threads=1
```

passed **25/25**, **37/37** and **12/12**. Four new oracle fixtures cover convergence under a
pass that reports at most two misses per run, bound overflow that preserves committed residency,
the iteration cap, and a desynchronized pass — plus the distinction between planning-invalid
demand (which returns `None`, a caller bug) and every loop state.

The decisive fixture is `residency_loop_converges_driving_the_real_resolve_dispatch`. Each
iteration runs the actual `record_chunked_resolves` dispatch over all 384 voxels of a six-chunk
level, feeds the keys the **shader** recorded back into the loop, and re-runs. The request table
is deliberately tiny — 4 slots, 2 probes — so the shader drops most of its misses every pass. The
test asserts that drops actually occurred and that more than two passes were needed, so it cannot
pass by way of one all-revealing pass; then it requires the converged frame to request nothing, to
resolve every voxel resident in agreement with the core oracle, and the final plan's pages to sum
to exactly 24 x 16 = 384 words.

The rest of the suite is unchanged: `palace-wgpu-spike` **24/24** and desktop **39/39**.

Still open in item 4:

1. `SCENE_DVR_SHADER` still reads scalars from a statically bound page range. Wiring
   `channel_scalar` onto the residency lookup plus request write is the remaining plumbing, and
   it needs the channel record extended past its current sixteen words to carry chunk shape and
   level. Note the binding budget: the scene pass already uses eight storage bindings and
   Chromium's measured floor is ten per stage, so the page table and request buffer fit but a
   third new storage binding would not — the descriptor belongs inside the existing metadata
   buffer rather than beside it.
2. Camera-driven level selection for the portable route; `select_level` is 2D-oriented.
3. The desktop scene route still takes its chunk region from the webview.

## Demand-resident channels in the scene pass (2026-09-18)

The scene-shader half of step 2b for PLAN.md §9.3.1 item 4. `SCENE_DVR_SHADER` can now resolve a
channel's scalars through a residency map and record a request for every chunk it misses, while a
statically paged channel renders exactly as before.

**Correction to the previous entry's binding-budget guidance.** That entry said the page table and
request buffer "just fit" under a ten-storage-buffer floor, citing S6's Chromium measurement. That
was wrong in practice: `wgpu::Limits::default()` allows **eight** storage buffers per compute
stage, and the attempt failed on this adapter with

```text
Too many bindings of type StorageBuffers in Stage ShaderStages(COMPUTE), limit is 8, count was 10
```

Eight is the WebGPU default; Chromium's measured ten is above the guaranteed floor, so designing
to ten would not have been portable. The scene pass already used all eight, so two had to be
freed rather than two added:

- the residency map is packed into the **metadata buffer**, after the transfer LUT, addressed
  through a new four-word header (layer word count, absolute LUT base, absolute residency base,
  reserved); and
- the pixel-zero trace is packed into the tail of the **output buffer**.

That leaves the atomic request table as the one new binding, which cannot share a read-only
buffer. Final count: four scalar pages, metadata, rays, output, requests — eight exactly.

Channel records grew from sixteen to `PortableDvrSceneGpuInput::CHANNEL_WORDS` (24) words. Word 15
was unused padding and is now the residency mode flag; words 16..23 carry chunk shape XYZ, level,
and chunk counts XYZ. A statically paged channel writes mode zero and zeros, so its record and its
rendering are unchanged. `PortableDvrSceneChannel::with_residency` is how a caller opts in.

The page-table ordinal is **plan-relative**, so the shader adds the channel's own first global
page to it. That is what lets a demand-resident channel share the existing four static page slots
with statically paged channels in the same scene.

Evidence on this Linux local adapter, all serial:

```text
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-wgpu -- \
  --include-ignored --test-threads=1
```

passed **13/13**. Both pre-existing ordered-scene parity fixtures still pass unchanged, which is
the evidence that the 24-word record stride, the four-word header, and the relocated trace did not
disturb the fixed-page path.

The new fixture
`demand_resident_scene_matches_the_static_page_render_and_requests_an_absent_chunk` makes the
strongest parity statement available for the demand path without inventing a second oracle: it
renders the *same* scene twice, once statically paged and once demand-resident with its chunk
resident, and requires the demand render to equal the static render's CPU oracle output exactly.
It also asserts the serialized record actually carries the descriptor, that a fully resident frame
requests nothing, and that the same scene against an empty residency map renders transparent with
`+infinity` depth while recording exactly the missing chunk's key.

The rest of the suite is unchanged: `palace-core` **25/25** `gpu::tests` and **37/37**
`portable_`, `palace-wgpu-spike` **24/24**, desktop **39/39**.

Still open in item 4: camera-driven level selection for the portable route, and the desktop scene
route still taking its chunk region from the webview rather than from the demand loop.

## Camera-driven level selection for the portable route (2026-09-18)

The last mechanism gap in PLAN.md §9.3.1 item 4. What remains after this is desktop wiring only.

`select_portable_level` deliberately **mirrors** `sliceviewer::select_level` rather than
introducing a second level policy: walking from the finest level, a level is acceptable while its
voxel extent projected onto each supplied direction is no coarser than the pixel footprint times
`coarse_lod_factor`, and the coarsest acceptable level wins. Every supplied direction must accept
a level, matching the Vulkan viewer's `break 'outer` over neighbour directions, and level zero
remains the floor even when it is already too coarse — all three behaviours are Palace's, not new.

`portable_pixel_footprint` supplies the other term. It measures the physical separation of two
neighbouring pixel rays at a distance along them, computed from the trusted rays themselves rather
than from a projection matrix, so no camera convention enters `palace-core`.

Evidence on this Linux local adapter, all serial:

```text
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-core --lib \
  gpu::tests -- --test-threads=1
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-core --lib \
  portable_ -- --test-threads=1
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-wgpu -- \
  --include-ignored --test-threads=1
```

passed **29/29**, **41/41** and **14/14**.

Four new oracle fixtures cover the selection ladder across an isotropic pyramid including the
exact threshold (Palace rejects on `>`, so a footprint equal to a level's extent still admits it)
and the level-zero floor; anisotropy, where looking along a coarse axis selects differently from a
fine one and supplying both directions takes the restrictive answer; the diagonal case, which
projects the spacing onto the direction rather than taking the coarsest axis — with a fixture
chosen so a coarsest-axis rule would give a different answer; and rejection of input the rule
cannot reason about, including a zero-length direction.

One assertion in the anisotropy fixture was initially mislabelled: it was commented as testing a
diagonal but actually passed an empty direction array, duplicating a check from the validation
fixture. It now tests a real 45-degree direction against a fixture that discriminates between
projection and a coarsest-axis rule.

The new adapter fixture `selected_level_drives_the_demand_loop_through_the_scene_pass` covers the
whole chain where it matters: level choice is host planning with no shader counterpart, so rather
than contriving a GPU test for it, the selected level index becomes the residency key's level, the
residency loop discovers that level's chunks through the real scene dispatch, and the converged
frame must equal the statically paged render of the same data. The fixture also asserts that more
than one pass was needed, so convergence is genuinely exercised.

The rest of the suite is unchanged: `palace-wgpu-spike` **24/24** and desktop **39/39**.

Still open in item 4: only the desktop wiring. `render_native_portable_scene_camera_draw` takes
its chunk region from the webview through `LocalSession::local_layer_chunk_plan`; replacing that
with level selection plus a `PortableResidencyLoop` driving
`record_dvr_scene_frame_with_residency` is what finally makes demand come from the renderer.

## The session can plan an explicit chunk set (2026-09-18)

The first half of the desktop wiring for PLAN.md §9.3.1 item 4. Item 4 remains open.

`LocalSession::local_layer_chunk_plan` can only describe a **box**, which is why the desktop scene
route had to be told by its caller which chunks to render. Demand-driven planning produces
whatever set the renderer actually missed, which is generally not a box.
`local_layer_chunk_plan_for_chunks` takes an explicit list of XYZ chunk coordinates instead.

`read_local_layer_chunks` already iterates `plan.chunks` without assuming a shape, so only the
planner needed changing. Both planners now build their addresses through one extracted
`chunk_address` helper, so asset-path encoding, dataset axis order, channel and timepoint
placement, and edge-aware logical extents cannot drift between the box path and the demand path.

The explicit planner sorts and deduplicates its input. That matters for the same reason
`PortableChunkPlan` sorts: a demanded set arrives in whatever order the request table happened to
hold it, repeats are normal because many pixels miss the same chunk, and the resulting plan must
be a pure function of the set.

Evidence, all serial:

```text
cargo test --offline --manifest-path crates/newvolim-desktop/Cargo.toml -- \
  --include-ignored --test-threads=1
```

passed **41/41**, up from 39 with two new fixtures. The load-bearing one is
`explicit_chunk_plan_matches_the_region_plan_over_the_same_box`: planning `[[0,0,0],[1,0,0]]`
explicitly must produce a plan *equal* to the region planner's over the same box, which is what
demonstrates the `chunk_address` extraction changed no behaviour. The second covers order
independence, deduplication of repeated demand, rejection of a chunk outside the source grid,
enforcement of the address bound, and refusal of empty demand.

`palace-core` **29/29** `gpu::tests` and `palace-wgpu` **14/14** are unchanged.

### What the second half needs

The remaining bridge is page assembly. `native_portable_page_admission` runs loaded chunks through
`assemble_portable_xyz_tiles`, which builds one **dense XYZ subvolume** spanning the requested
box. The demand path needs the other layout: each planned chunk's words concatenated in ascending
chunk order, which is exactly what `PortableChunkPlan` computed offsets for.

That is simpler than it sounds, because `portable_words_xyz` already returns a chunk's scalars
X-fastest over its *logical* extent — the same clipped-extent layout the plan sizes by and the
shader strides by. So the new assembly is a concatenation in plan order, and its test should
assert the resulting page words line up with `PortableChunkPlan::chunks()` offsets and
`page_words()` totals.

Then the desktop scene route becomes: `portable_pixel_footprint` over two neighbouring camera
rays, `select_portable_level`, a `PortableResidencyLoop`, and
`record_dvr_scene_frame_with_residency` re-run until `Complete` — with `ExceedsPortableBound`
falling back to the existing native route and `Exhausted` presenting a preview.
`selected_level_drives_the_demand_loop_through_the_scene_pass` in `palace-wgpu` is a working
template for that sequence.

## Plan-ordered page assembly, and the pyramid becomes addressable (2026-09-18)

More of the desktop wiring for PLAN.md §9.3.1 item 4, plus a finding that changes what the rest of
that wiring costs. Item 4 remains open.

`LocalSession::portable_chunk_plan_pages` assembles one channel's demanded chunks into the layout
`PortableChunkPlan` computed: each chunk's words concatenated in ascending chunk-index order,
rather than the dense XYZ subvolume `native_portable_page_admission` builds for a box. It works
because `portable_words_xyz` already returns a chunk's scalars X-fastest over its *logical* extent
— the same clipped-extent layout the plan sizes by and the shader strides by.

Nothing is trusted: every chunk is checked against the plan's own page ordinal and first word, the
plan's grid is required to match the source's dimensions and chunk shape, and each assembled page
must end at the plan's declared word count. A chunk that was planned but not read is refused rather
than leaving a short page. All three matter because a silent mismatch would make the residency map
address a neighbour's scalars, which renders plausible-looking wrong data.

### The finding: the session was hard-wired to level zero

Camera-driven level selection landed in the previous entry, but the desktop had nothing to select
between. `LocalOmeZarrSource` has carried a `level: u32` field from the start, and both
construction sites set it to `0`; `level_zero` read `multiscale.datasets.first()` directly. The
pyramid was never addressable.

`LocalOmeZarrSource::for_level` now admits any declared level, with `level_zero` delegating to it.
`local_source_admits_each_declared_pyramid_level` pins this against the committed fixture, whose
levels 0 and 1 are `[128, 128, 32]` and `[64, 64, 32]` in XYZ — halved in x and y, unchanged in z,
which is exactly the anisotropic case worth selecting between. It also asserts level zero still
resolves identically through the old entry point, and that an undeclared level is refused rather
than clamped.

Evidence, all serial:

```text
cargo test --offline --manifest-path crates/newvolim-desktop/Cargo.toml -- \
  --include-ignored --test-threads=1
```

passed **44/44**, up from 41. The new fixtures are plan-ordered placement (asserting each chunk's
own scalars appear at the page and word the plan assigned, so a concatenation in read order rather
than plan order would be caught), refusal of a mismatched grid and of an unread chunk, and the
pyramid-level admission above.

`palace-core` **29/29** `gpu::tests`, `palace-wgpu` **14/14** and `palace-wgpu-spike` **24/24** are
unchanged.

### What is left, revised

The remaining desktop work is larger than "assembly", because level selection needs per-level
geometry the session does not yet produce:

1. **Per-level physical transform and spacing.** `portable_axis_aligned_transform(multiscale)`
   derives one transform from the multiscale; NGFF declares coordinate transformations *per
   dataset*, and `select_portable_level` needs each level's physical voxel spacing. Without this,
   a chosen level would be rendered with the wrong physical extent.
2. **Layer admission at a chosen level.** `prepare_default_portable_image_layer` calls
   `level_zero`; it needs to accept a level, and `local_layer_render_requests` must carry it.
3. **The demand route itself**: `portable_pixel_footprint` over two neighbouring camera rays,
   `select_portable_level`, `PortableResidencyLoop`, `portable_chunk_plan_pages` into a
   `PortableDvrSceneChannel::with_residency`, and `record_dvr_scene_frame_with_residency` re-run
   until `Complete` — with `ExceedsPortableBound` falling back to the existing native route and
   `Exhausted` presenting a preview.

## Per-level NGFF geometry on the desktop (2026-09-18)

The two pieces of per-level geometry the previous entry identified as missing. PLAN.md §9.3.1
item 4 remains open; what is left is the demand route itself.

Both turned out contained, because `newvolim_io::level_transform(multiscale, level)` already
composes a dataset's own coordinate transformations with the multiscale's shared ones. The desktop
was simply passing `0`.

- `portable_axis_aligned_transform` now takes a level. `orthogonal_physical_aspect_ratios` keeps
  passing zero deliberately: pane aspect ratio is a presentation property of the whole volume.
- `LocalSession::portable_level_spacings` returns every declared level's physical voxel spacing,
  finest first — exactly the input `select_portable_level` consumes.
- `prepare_portable_image_layer_at_level` admits the default layer at a chosen level, moving the
  source array *and* its transform together. `prepare_default_portable_image_layer` delegates to it
  with level zero, so the existing route is unchanged.

The committed fixture is a genuinely anisotropic pyramid, which is what makes this testable rather
than nominal. Its XYZ spacings are `[0.26, 0.26, 0.29]`, `[0.52, 0.52, 0.29]` and
`[1.04, 1.04, 0.58]`: level one halves x and y but keeps z, and only level two halves z.

Evidence, all serial:

```text
cargo test --offline --manifest-path crates/newvolim-desktop/Cargo.toml -- \
  --include-ignored --test-threads=1
```

passed **46/46**, up from 44.

`portable_level_spacings_follow_each_datasets_own_ngff_transform` pins the three spacings, asserts
z is identical between levels zero and one while x is not — the property a shared level-zero
spacing would destroy — and then feeds them straight into `select_portable_level`, which answers
level **one** for a camera looking along x and level **two** for the same footprint looking along
z, because z stays fine longer. That is the anisotropic-pyramid case S5 exists for, now driving a
real level decision.

`portable_layer_admission_follows_the_chosen_level_in_both_array_and_transform` asserts that a
layer admitted at level one reports half the voxels in x and y at twice the spacing, and that the
resulting **physical extent is identical at both levels**. That invariant is what a coarser array
carrying level zero's transform would break, and the test was confirmed to catch exactly that:
reverting the transform lookup to level zero fails it, restoring it passes.

`palace-core` **29/29** `gpu::tests` and `palace-wgpu` **14/14** are unchanged.

Still open in item 4: the demand route itself. Every part it needs now exists —
`portable_level_spacings` and `select_portable_level` for the level, `PortableResidencyLoop` for
demand, `local_layer_chunk_plan_for_chunks` and `portable_chunk_plan_pages` for the pages,
`PortableDvrSceneChannel::with_residency` and `record_dvr_scene_frame_with_residency` for the
render. `selected_level_drives_the_demand_loop_through_the_scene_pass` in `palace-wgpu` is a
working template for the sequence.

## A scene frame that bootstraps from nothing resident (2026-09-18)

The last mechanism needed before the desktop demand route, plus an observed behaviour worth
recording. PLAN.md §9.3.1 item 4 remains open.

`PortableDvrVolumeLevel::new` requires its statically bound pages to cover every voxel, which is
correct there: without it a ray could read past the admitted scalars. That requirement is wrong for
a demand-resident channel, where which voxels are readable is decided by the `PortablePageTable`,
not by page length — and where a frame legitimately begins with **no resident chunks at all**.
`new_demand_resident` drops only the coverage check; zero dimensions, an empty physical extent, no
pages, and aliasing page owners are all still rejected, and a core fixture asserts each of those
separately so the relaxation cannot widen by accident.

### Demand-driven residency only loads what the frame can see

The new adapter fixture
`demand_resident_scene_bootstraps_from_nothing_and_converges_to_the_static_render` runs the shape a
migrated viewer actually runs: the first pass has an empty residency map and a single placeholder
word, every sample misses and is recorded, the host plans, and the frame converges — then must
equal the statically paged render of the same data exactly.

Writing it surfaced a behaviour that had been predicted wrongly. The fixture was first asserted to
demand all four chunks along the ray. It does not, and should not: the ray meets an opaque chunk
first, accumulates past the 0.95 early-termination threshold, and never reaches the chunks behind
it, so those are never requested. The converged image is still byte-identical to the static render,
because the static render terminates at the same sample.

That is the point of feedback planning rather than a quirk: occluded chunks are never loaded, which
a region-based plan cannot express — the webview-supplied box would have fetched all four. The
fixture now asserts the entered chunk is demanded and that strictly fewer than all four are, which
pins the property rather than the accident.

Evidence on this Linux local adapter, all serial:

```text
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-core --lib \
  gpu::tests -- --test-threads=1
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-wgpu -- \
  --include-ignored --test-threads=1
```

passed **30/30** and **15/15**. `palace-core --lib portable_` **41/41**, `palace-wgpu-spike`
**24/24** and the desktop crate **46/46** are unchanged.

Still open in item 4: assembling the demand route inside
`render_native_portable_scene_camera_draw`. One constraint found while planning it —
`prepare_portable_image_layer_at_level` takes `&mut self` and requires an empty scene, so a
per-frame level *switch* would have to re-admit the layer and destroy the user's scene. The first
desktop route should therefore render demand-driven at the layer's already-admitted level; dynamic
level switching needs session support for re-admission without scene mutation and is a separate
step.

## Desktop chunk demand now comes from the renderer (2026-09-18)

`render_demand_driven_scene_camera_draw` in `crates/newvolim-desktop/src/main.rs` renders an
ordered scene frame with **no caller-supplied chunk region**. The scene pass runs against a
residency map, records every chunk it misses, and the host plans, reads and uploads exactly those
chunks until the frame reports no misses. `render_native_portable_scene_camera_draw` tries this
route first and falls back to the established region-based routes for anything it refuses.

This is the core of PLAN.md §9.3.1 item 4 for the single-layer case. The restrictions below are
real and are not claimed away.

The decisive evidence is `demand_driven_scene_ignores_the_webview_chunk_region`: three requests
carrying deliberately different `origin_xyz`/`extent_xyz` must produce **byte-identical** frames.
It also asserts the frame actually contains volume, since an all-transparent frame would satisfy
equality trivially, and that renderer-owned depth still pairs with colour — an opacified pixel
carries a finite physical distance and a transparent one stays at `+infinity`.

Two things caught during assembly:

- The first packet was refused outright (`demand-driven scene packet is not admitted`) because the
  rays were built with `far = f32::MAX`. Unclipped, every pixel exceeds the admitted sample count.
  They are now clipped to the layer's physical AABB, with a missing ray becoming a degenerate
  `near == far` interval — the same rule the region-based adapter uses.
- The layer's physical box is level-invariant, which is what lets the rays and the AABB be
  computed once, independently of which level is resident.

Evidence, all serial:

```text
cargo test --offline --manifest-path crates/newvolim-desktop/Cargo.toml -- \
  --include-ignored --test-threads=1
```

passed **47/47**. `palace-core` **30/30** `gpu::tests` and **41/41** `portable_`, `palace-wgpu`
**15/15** and `palace-wgpu-spike` **24/24** are unchanged.

### Restrictions, stated rather than claimed away

1. **One layer, one channel.** A multi-layer demand render needs one residency loop per layer
   sharing the four-page budget, and that planning is not designed. Anything else falls back.
2. **No per-frame level switching.** The route renders at the layer's already-admitted level.
   `prepare_portable_image_layer_at_level` takes `&mut self` and requires an empty scene, so
   switching per frame would re-admit the layer and destroy the user's scene. Level *selection* is
   implemented and tested (`select_portable_level` over `portable_level_spacings`), but nothing
   drives it yet; that needs session support for re-admission without scene mutation.
3. **An adapter is acquired per demand iteration.** The per-frame adapter acquisition already
   recorded is now multiplied by the number of re-render passes, on top of the annotation
   composite's own acquisition. A converging frame can therefore acquire several devices. This is
   the clearest remaining interactive-latency problem on the portable route and a shared device
   cache is the fix; it is a design decision, not a cleanup.
4. **No side-by-side visual comparison** against the native compositor has been done, and the two
   now differ in volume blend, annotation ordering, and which chunks are resident.

## One shared WGPU device for every portable route (2026-09-18)

Restriction 3 from the previous entry. Every portable route in the desktop crate acquired its own
`wgpu::Instance`, adapter and device per call. Demand-driven rendering made that much worse: a
converging frame re-renders until it reports no misses, so it paid full acquisition once per pass,
and an annotated frame paid it again for the composite.

`shared_portable_device` is a process-wide `OnceLock`. `wgpu::Device` and `wgpu::Queue` are
internally reference-counted and `Send + Sync`, so one instance serves every route. Five
acquisition sites became one.

Two decisions are deliberate:

- **A failed acquisition is cached as a failure, not retried.** An eligible adapter does not
  normally appear part-way through a session, and retrying per frame would pay the full
  acquisition cost on every frame of exactly the host that most needs its CPU fallback to be cheap.
- **Two routes were also rendering everything twice.** The direct page-DVR route and the
  orthogonal-slice route both evaluated their CPU oracle unconditionally *before* attempting the
  GPU path, then discarded it on success — the same pattern already removed from the scene route.
  Both now evaluate the oracle only when the device is unavailable or the recording fails.

Evidence, all serial:

```text
cargo test --offline --manifest-path crates/newvolim-desktop/Cargo.toml -- \
  --include-ignored --test-threads=1
```

passed **48/48**. `palace-core` **30/30** `gpu::tests`, `palace-wgpu` **15/15** and
`palace-wgpu-spike` **24/24** are unchanged.

`portable_routes_share_one_wgpu_device` proves the cache by **counting acquisitions**, not by
timing: a static counter is incremented inside the initializer, the first call must take it to
one, eight further calls must return a pointer-identical device and leave it at one, and then a
full demand-driven frame — which re-renders until convergence and then composites annotations,
exercising several routes — must still leave it at one.

Directional, not a benchmark: the desktop suite went from **36.7 s to 31.0 s** while gaining a
test. Suite timing is noisy and this is not a per-frame measurement, so it is recorded as
consistent with the change rather than as a performance claim. A real interactive measurement of
the demand route remains unmeasured.

## Several demand-resident channels in one residency map (2026-09-18)

The foundation for the multi-layer restriction. The portable scene pass has a **single**
page-table binding — the storage-binding budget is eight and the pass already uses all of them —
so every demand-resident channel in a frame resolves through the same table. Two channels of the
same volume hold the same chunk indices at the same level, so without a namespace their keys
collide and one channel reads the other's scalars.

`PortableResidencyTag` composes the channel ordinal and the pyramid level into the key's
seven-bit level field as `channel * MAX_LEVELS + level`. Four channels at up to thirty-two levels
is exactly the `0..=127` that field admits; those bounds are not round numbers, they are what
makes the space exactly full. A core fixture walks all 128 pairs, requires every tag distinct and
round-tripping, and asserts the maximum composed tag *is* `PortableFeedbackKey::MAX_LEVEL`, so the
composition cannot silently start wasting or overflowing the field.

Evidence on this Linux local adapter, all serial:

```text
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-core --lib \
  gpu::tests -- --test-threads=1
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-wgpu -- \
  --include-ignored --test-threads=1
```

passed **33/33** and **16/16**. `palace-core --lib portable_` **44/44**, `palace-wgpu-spike`
**24/24** and the desktop crate **48/48** are unchanged.

`two_demand_resident_channels_share_one_page_table_without_colliding` is the adapter evidence: two
co-registered channels, both holding chunk zero at the same level, resolving through one table,
must render exactly what the statically paged two-channel composite renders.

**Making that test able to fail took two attempts, and the first failure was instructive.** The
first fixture gave the channels scalars `1` and `3` against a two-entry LUT over `0..4`. Both
normalize into the same bin, so one channel rendered transparent and — worse — a collision would
have been *invisible*, since reading the neighbour's page would have selected the same LUT entry.
The fixture now uses four-entry LUTs where each channel is opaque only at the bin its own scalar
selects and transparent at the other's, so a collision in either direction makes that channel
vanish. Confirmed discriminating: dropping the level component from the shader's key
(`key = chunk_index`) fails the test; restoring it passes.

Still open: wiring multi-channel demand into the desktop route, which now has its key namespace,
and per-frame level switching.

## Multi-channel demand on the desktop, and a fixture gap (2026-09-18)

`render_demand_driven_scene_camera_draw` now admits one to four channels instead of exactly one.
Each channel gets its own `PortableResidencyLoop` tagged with
`PortableResidencyTag::compose(ordinal, level)`, its own owner range so two channels' planned
pages cannot alias, and its own entry in a single shared `PortablePageTable`. Recorded request
keys are routed back to the channel whose tag they carry, and the frame is finished only when
**every** channel reports no misses. The shared four-page budget is summed across channels before
anything is rendered, and exceeding it is an error that falls back to the established route.

`PortablePageTable::insert_plan` is the core addition. A repeated key across two plans is refused
rather than treated as an update, because a repeat means two channels' tags collided and one would
silently read the other's scalars.

### The gap: no desktop-level multi-channel fixture, and why

A two-channel desktop fixture was attempted and removed rather than left fragile. Two facts block
it locally:

1. **Both committed datasets are single-channel.** `anisotropic.ome.zarr` and
   `cells3d-anisotropic.ome.zarr` both declare `[z, y, x]` axes, so neither exercises more than one
   channel.
2. **The demand route needs a Palace-readable dataset.** It reconstructs camera rays through
   `camera_ray_for_local_zarr`, so a synthetic tempdir fixture of the kind the session tests use is
   rejected before rendering with `Unknown tensor format`. The session's own two-channel fixture is
   therefore unusable here.

So the multi-channel path is covered where it can be: its **rendering semantics** are pinned in
`palace-wgpu` by `two_demand_resident_channels_share_one_page_table_without_colliding`, a fixture
deliberately built so a key collision is visible and confirmed to fail when the shader drops the
level component from its key. The desktop **routing** — one loop and page range per channel,
request routing by tag, the summed page budget — is implemented and compiles against those types,
but has no end-to-end desktop fixture. Committing a small multi-channel OME-Zarr that Palace can
open would close this; it is the single missing piece of evidence, not a design gap.

Evidence, all serial:

```text
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-core --lib \
  gpu::tests -- --test-threads=1
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-core --lib \
  portable_ -- --test-threads=1
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-wgpu -- \
  --include-ignored --test-threads=1
cargo test --offline --manifest-path crates/newvolim-desktop/Cargo.toml -- \
  --include-ignored --test-threads=1
```

passed **34/34**, **45/45**, **16/16** and **48/48**. `palace-wgpu-spike` **24/24** unchanged.

## A leaked WGPU device crashes the process at exit (2026-09-19)

The session-scoped device cache, a committed two-channel fixture, and a camera decoupling — all
driven by one finding.

### The crash

The device cache added in the previous entry stored the device in a process-wide `OnceLock`. Rust
never drops statics, so the device leaked. Tests that touched it began segfaulting **at process
exit**, intermittently: 4/10, 3/10 and 2/10 for three different device-using tests, while a test
that never touches a device crashed 0/10. Under gdb the faulting thread is `[vkps] Update` — a
graphics-driver pipeline thread, still running while the process tears down.

A twenty-line probe with no project code settled it: acquire a device and drop it → **0/20**
crashes; acquire and `mem::forget` it → **8/20**. The leak is the cause, not a coincidence.

The fix is scope, not suppression. `LocalSession::portable_device` now owns the device behind an
`Arc<OnceLock<…>>`, so it is destroyed when the session drops — before the process exits. The
sharing that mattered is untouched: every pass of a converging demand frame, plus the annotation
composite, still reuse one device. Five call sites take a `&LocalSession` instead of reaching for
a global. After the change, the two previously-crashing filters run **0/15** each.

`portable_routes_share_one_wgpu_device_per_session` counts acquisitions rather than timing them:
one per session across eight repeat calls, a whole converging demand frame and an annotation
composite; a clone shares it; a separate session acquires its own, which is the scope that makes
teardown deterministic.

### Palace's camera no longer needs a three-dimensional file open

Committing a multi-channel fixture first failed with `Unknown tensor format`, then with a panic
inside `palace-frame`: `load_local_zarr_volume` ends in `try_into_static().unwrap()`, which is
three-dimensional, so **any dataset with a channel axis cannot be opened through it at all**. The
demand route reconstructed its camera through `camera_ray_for_local_zarr`, which opens the file, so
multi-channel was blocked by the camera rather than by anything about residency.

The camera itself only needs ZYX dimensions and spacing. `palace_frame::camera_ray_for_geometry`
takes those directly, and `geometry_camera_rays_match_the_file_opening_path` requires it to produce
**identical** rays to the file-opening path across three control settings and four pixels on the
committed fixture. Because that fixture is chunked 8×32×32 while the geometry path declares a
single whole-volume chunk, passing also pins that chunk size does not influence the fitted camera.

The demand route now derives its camera this way. The physical box is level-invariant, so an
admitted coarser level's dimensions paired with that level's spacing fit the same camera.

### The two-channel fixture

`test-data/two-channel-gradient.ome.zarr` is a synthetic 2×8×16×16 two-level pyramid, ~13 kB,
generated by `scripts/prepare_two_channel_fixture.py`. Channel 0 ramps along x and channel 1 along
y, so a residency key collision is visible as a different scalar rather than an identical one.

Wiring it up caught a real bug: on the bootstrap pass every channel received placeholder page owner
`1`, so the owners aliased and `PortableDvrSceneFrameInput` refused the packet outright. Each
channel now gets its own placeholder owner.

Evidence, all serial:

```text
cargo test --offline --manifest-path crates/newvolim-desktop/Cargo.toml -- \
  --include-ignored --test-threads=1
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-frame -- --test-threads=1
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-core --lib \
  gpu::tests -- --test-threads=1
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-wgpu -- \
  --include-ignored --test-threads=1
```

passed **49/49**, **8/8**, **34/34** and **16/16**; `palace-core --lib portable_` **45/45** and
`palace-wgpu-spike` **24/24** unchanged.

## Camera-driven level selection now drives the desktop frame (2026-09-19)

Restriction 2 from the demand-route entry. Level selection and per-level geometry were implemented
and tested separately; nothing consulted them. Now the frame does.

The blocker was stated as needing "re-admission without scene mutation", and that turned out to be
the wrong framing. **Nothing about a level is session state.** The source array and its physical
transform are both pure functions of the dataset metadata and the level index, so a renderer can
ask for a level without the session changing at all —
`prepare_portable_image_layer_at_level` (which takes `&mut self` and requires an empty scene) never
needed to be involved. Three non-mutating accessors were added instead:
`local_layer_render_requests_at_level`, `local_layer_chunk_plan_for_chunks_at_level`, and
`portable_level_transform`. The layer's channel selection, transform authority and visibility are
untouched; only the array being read moves.

`demand_scene_level` measures the footprint between two horizontally neighbouring centre pixels
and feeds `select_portable_level` over `portable_level_spacings`.

### Two bugs the assertions caught, both about where a measurement is valid

1. **A footprint measured at the eye is zero.** The first version measured at `near = 0`, where two
   neighbouring rays are coincident, so the footprint was zero and the finest level would always
   have been chosen. It now measures where the centre ray *enters* the volume, which is also the
   conservative choice: the footprint only grows with distance, so the entry point never selects a
   level coarser than some visible pixel warrants.
2. **Independently clipped rays share no parameterization.** The second version clipped both rays
   to the box and measured both at one distance. Clipped rays generally have *different* near
   distances, so `point_at` is invalid for whichever ray enters later and the measurement was
   unrepresentable. `demand_world_ray` now returns an **unclipped** ray and clipping is the
   caller's step: the frame's ray table clips each ray (degenerate on a miss) while level selection
   clips only the centre ray to obtain the entry distance. The reason is recorded on the function,
   because the failure is silent rather than loud.

Both of those were also a reminder that the ZYX/XYZ conversion was duplicated: Palace returns its
ray in ZYX voxel coordinates *(correction, later the same day: it is ZYX **physical** — see "The
Vulkan comparison, and what it found"; the conversion described here applied the layer scale to
an already-physical ray)*, and an earlier attempt compared a ZYX ray against an XYZ physical
box. The axis swap and voxel-to-world mapping now live once, in `demand_world_ray`, shared by the
frame and by level selection so the two cannot disagree about the camera.

Evidence, all serial:

```text
cargo test --offline --manifest-path crates/newvolim-desktop/Cargo.toml -- \
  --include-ignored --test-threads=1
```

passed **51/51**. `palace-core --lib gpu::tests` **34/34**, `palace-wgpu` **16/16** and
`palace-frame` **8/8** unchanged. The previously-crashing exit path stays clean at **0/12**.

`demand_scene_level_follows_the_camera_and_frame_extent` pins the direction of the response rather
than specific levels: a larger frame may never choose a coarser level than a smaller one, pulling
the camera back may only coarsen, and — so the test cannot pass vacuously — it additionally
requires the fixture to *distinguish* both pairs and selection to leave level zero. Measured on the
committed fixture, a 16x12 frame selects levels 0, 1 and 2 across the valid zoom range while a
256x192 frame stays at level 0 throughout, which is the expected shape: pixel count dominates.

`demand_driven_scene_renders_a_coarse_level` then renders at a selected coarse level end to end,
which exercises a different source array, physical transform and chunk grid than level zero, and
checks that renderer-owned depth still pairs with colour there.

## The demand route is interactive, and multi-layer is unreachable (2026-09-19)

Two findings, one correction, one optimisation.

### Multi-layer demand is unreachable from the desktop

The handover called multi-layer demand "the widest correctness gap" and then "the last
restriction". Both were wrong. **The desktop session can hold at most one image layer.**
`prepare_portable_image_layer_at_level` refuses a non-empty scene, `bind_layer_to_open_dataset`
only binds a layer that already exists, and nothing else inserts one —
`import_annotations` preserves whatever was there. The multi-layer scene *renderer* works and is
tested, but it is reached only by synthetic scenes built directly in tests, never by the desktop.

Generalising the demand route to several layers would therefore be speculative work against no
caller. It is recorded as a contract-level gap with no reachable path, not as a correctness risk.

### The interactive measurement, which had been flagged twice as missing

`measure_demand_driven_scene_frame_cost` reports rather than asserts, because a timing threshold on
a shared machine is a flaky test rather than evidence. On this host, committed fixture, level 0:

| build | extent | cold | warm mean | of which rays |
|---|---|---|---|---|
| debug | 64x48 | 386 ms | 108 ms | 36 ms |
| debug | 256x192 | 552 ms | 552 ms | 344 ms |
| release | 64x48 | 499 ms | 6.4 ms | 1.6 ms |
| release | 256x192 | 17 ms | 19.0 ms | 5.1 ms |

**The first reading of these numbers was wrong and is worth recording as a caution.** Measured in a
debug build the route looked plainly unusable — 552 ms at 256x192 — and the conclusion drafted from
that was "not interactive". Release is roughly thirty times faster at that extent: **19 ms, about
53 frames per second.** Debug timings for this workload overstate cost by more than an order of
magnitude, so any future performance claim about the portable route must say which profile it was
measured in.

### Fitting the camera once per frame

The breakdown showed camera-ray construction was **344 ms of the 552 ms debug frame, 62%**, because
`camera_ray_for_geometry` refits the entire camera for every pixel — about 7 microseconds per
pixel spent re-deriving the same camera 49 152 times.

`palace_frame::camera_rays_for_geometry` fits once and loops, with identical per-pixel arithmetic.
`batched_camera_rays_match_the_per_pixel_path_exactly` requires both paths to agree on every ray of
a 23x11 frame across three control settings, so the optimisation cannot change what is rendered.

Release effect at 256x192: warm frame **25.0 ms to 19.0 ms**, rays **12.6 ms to 5.1 ms**. Modest in
release, dramatic in debug, and free of behavioural risk because the equivalence is pinned.

Still per-pixel, and not addressed here: `portable_camera_rays_xyz`, used by the native scene route
and the pickers, calls `camera_ray_for_local_zarr` for every pixel — which **re-opens the dataset
each time**. That is a far worse shape than the one just fixed and is the obvious next
optimisation, but it belongs to the native route rather than the portable one.

Evidence, all serial:

```text
cargo test --offline --manifest-path crates/newvolim-desktop/Cargo.toml -- \
  --include-ignored --test-threads=1
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-frame -- --test-threads=1
```

passed **52/52** and **9/9**; `palace-core --lib gpu::tests` **34/34** and `palace-wgpu` **16/16**
unchanged.

## The native ray table opened the dataset once per pixel (2026-09-19)

Item 1 of the day plan, and the largest single performance defect found in this work.

`portable_camera_rays_xyz` built a frame's ray table by calling
`palace_frame::camera_ray_for_local_zarr` for every pixel, and that function opens the Zarr
dataset on every call. Opening per call is the right shape for a single pick and a pathological one
for a frame: the same dataset was opened once per pixel. The native scene route and both pickers
use this function.

Measured on the committed fixture, **release build**:

| extent | before | after |
|---|---|---|
| 64x48 (3 072 rays) | 1.315 s | 5.5 ms |
| 256x192 (49 152 rays) | **16.15 s** | **1.84 ms** |

That is roughly 8 800x at 256x192 — 328 microseconds per ray before, essentially all of it
re-opening the dataset. (The 64x48 figure is larger than 256x192 afterwards only because it runs
first and pays the one-time open.)

`palace_frame::camera_rays_for_local_zarr` opens once and reuses the existing batched loop, which
now sits behind a shared `camera_rays_for_volume` used by both the geometry and the
open-the-dataset entry points. `batched_local_zarr_rays_match_the_per_pixel_path_exactly` requires
it to agree with the per-pixel path on **every ray** of a 17x9 frame across three control settings,
so the change cannot alter what is rendered or picked.

This is the same defect shape as the portable route's per-pixel camera refit fixed in the previous
entry, one layer further down: there the camera was re-derived per pixel, here the dataset was
re-opened per pixel. Both were invisible in the test suite because correctness never depended on
them.

Evidence, all serial:

```text
cargo test --offline --manifest-path crates/newvolim-desktop/Cargo.toml -- \
  --include-ignored --test-threads=1
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-frame -- --test-threads=1
```

passed **53/53** and **10/10**; `palace-core --lib gpu::tests` **34/34**, `palace-wgpu` **16/16**
and `palace-wgpu-spike` **24/24** unchanged. Timings above are release; the suites are debug.

## Portable DVR opacity is corrected in the wrong units (2026-09-19)

Item 2 of the day plan — the side-by-side comparison — and it found a real rendering bug on its
first run, which is what it was for.

### What the comparison shows

Rendering the *same* scene packet through both compositors isolates the blend rule and the
first-opacity rule from residency and level choice. On the committed fixture at 64x48, level 0,
step size 0.13:

- **89.1%** of pixels differ in colour; max per-channel delta **253**, mean **104.9**.
- Fully opaque pixels: **Palace 2688, native 0**.
- Representative pixel: Palace `[245, 49, 81, 255]` at depth 18.95; native `[24, 1, 3, 2]` at
  `+infinity`.

Both are wrong, in opposite directions, and for different reasons.

### The native compositor under-accumulates by design

`SCENE_SHADER` multiplies every sample by a fixed `0.06` regardless of step size, so its image is
nearly transparent — alpha 2/255 at a pixel Palace renders opaque. Its first-opacity threshold of
`0.01` accumulated opacity is then never reached either, which is why it reports `+infinity` depth
at pixels that do have colour. PLAN.md already says not to carry those "older ad-hoc blend
semantics" over to Palace, so this half is expected.

### Palace corrects opacity in the wrong units

Palace's own legacy raycaster (`raycaster.glsl`) does:

```glsl
float diag = length(vec3(root_level.dimensions) * vec3(root_level.spacing));
float norm_step = step / diag;
float alpha = 1.0 - pow(1.0 - sample_f.a, norm_step * REFERENCE_STEP_SIZE_INV /* 256 */);
```

The step is **normalized by the volume's physical diagonal** before the 256x correction, so a
transfer alpha is defined per 1/256 of the diagonal. The portable path passes the *physical* step
size straight into `step_size * 256`.

For this fixture the volume is 33.28 x 33.28 x 9.28 physical units, diagonal **47.97**. The
correct exponent is `(0.13 / 47.97) * 256 = 0.69`; the portable path uses `0.13 * 256 = 33.28`,
**48x too large** — exactly the diagonal. That is why 87.5% of hit pixels saturate to alpha 255.

The formula itself matches the legacy shader exactly; only the units of its input are wrong. It
went unnoticed because every existing fixture passes `step_size = 1/256`, which makes the exponent
`1.0` whatever the intended normalization is — the one value at which the bug is invisible.

The fix is not to rescale the caller's step: `step_size` also drives the marching distance, so it
must stay physical. The opacity reference has to travel with the input as its own quantity.

### The fix: the opacity reference travels with the input

`step_size` cannot be rescaled, because it also sets the marching distance. The reference is
therefore carried as its own quantity: `PortableDvrSceneFrameInput::new` and the single-volume DVR
entry points (`composite_dvr_samples`, `composite_dvr_ray`, `render_dvr_frame`,
`render_dvr_input`, `PortableDvrPageFrameInput::render_cpu`, `record_dvr_frame`,
`record_dvr_page_frame`) all take an explicit `opacity_reference`, and the correction is
`step_size / opacity_reference`. Palace's convention is then written at the call site as
`diagonal / 256.0` — `portable_opacity_reference` in the desktop crate — which makes the unit
visible instead of implied. All three shaders take the reference through their uniform.

After the fix, on the same packet: fully opaque pixels **2688 to 1143** (87.5% of hit pixels down
to 42%), and the representative pixel `[245, 49, 81, 255]` becomes `[44, 8, 14, 44]` — the same
character as the native compositor's `[24, 1, 3, 2]` rather than a saturated block. Mean channel
delta 104.9 to 89.6.

The residual difference is the native compositor's fixed `0.06` factor and its `0.01` first-opacity
threshold, both of which PLAN.md already designates as legacy behaviour not to be copied. So the
comparison's verdict is: **the remaining differences are intended and Palace is the correct side;
the saturation was not, and is fixed.**

### The regression test, and why the old suite could not catch this

Every pre-existing fixture passed `step_size = 1/256`, which makes the exponent `1.0` under *any*
interpretation of the reference — the single value at which the bug is invisible. Passing
`1/256` as the new explicit reference therefore reproduces every previously expected value exactly,
which is why all suites still pass unchanged.

`portable_scene_opacity_follows_its_declared_reference_length` is the test that would have caught
it: it renders one scene at references `1.0`, `0.5` and `0.25` and requires the documented
`step / reference` exponent at each, requires opacity to increase as the reference shortens, and
pins the concrete arithmetic — for a 4x4x4 box the diagonal is 6.928, so a 0.13 step gives exponent
**4.80** under Palace's convention and **33.28** under the bug.

Evidence, all serial:

```text
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-core --lib -- --test-threads=1
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-wgpu -- --include-ignored --test-threads=1
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-wgpu-spike -- --include-ignored --test-threads=1
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-frame -- --test-threads=1
cargo test --offline --manifest-path crates/newvolim-desktop/Cargo.toml -- --include-ignored --test-threads=1
```

passed **81/81**, **16/16**, **24/24**, **10/10** and **54/54**.

## Route-selection audit, and an empty first-opacity surface for every real dataset (2026-09-19)

Item 3 of the day plan — PLAN.md §9.3.1 item 6. The audit found the routes consistent with the
rule and one serious pre-existing defect underneath them.

### The route table, as actually wired

| route | renderer | portable? |
|---|---|---|
| Desktop scene camera | Palace portable, demand-driven; native WGPU fallback | yes, for admitted packets |
| Desktop direct camera | Palace portable page-DVR; core CPU oracle fallback | yes, for admitted packets |
| Desktop orthogonal panes | Palace portable slice; Palace legacy fallback | yes |
| Server `/v1/frame`, `/v1/frames` | Palace **legacy Vulkan** raycaster only | **no** |
| Browser | client-side WebGPU over `/v1/datasets/{name}/zarr/{asset}` | independent authority |

The server calls only `render_local_zarr_with_camera_attachments` and
`render_local_zarr_orthogonal_at_png`, so it is permanently on the Vulkan fallback and never
selects the portable route. That satisfies item 6's rule. The browser keeps its independent
camera and depth authority by construction: the server hands it *chunks*, not frames.

The whole workspace builds and passes — server **17/17** including two tests that render real
Palace frames, so the server path is genuinely exercised, not merely plumbed.

### The defect: a real dataset renders colour with no depth at all

Comparing the desktop portable route against the server's renderer at the same camera showed the
server reporting **zero** finite first-opacity distances while painting 1849 of 3072 pixels.
Isolating it in `palace-frame` on the committed fixture at 32x24:

| render | painted pixels | finite depths |
|---|---|---|
| `render_local_zarr_attachments` | 484 | **0** |
| `render_local_zarr_with_camera_attachments` | 484 | **0** |
| `render_synthetic_attachments` (procedural ball) | 253 | 142 |

The camera is irrelevant — both local-Zarr entry points behave identically — and the procedural
ball works through the *same* reader. The volume source is what differs.

This is not cosmetic. The PNG+PFM transport contract promises a paired first-opacity surface, and
the desktop picker's Vulkan fallback depends on it; for any real dataset that surface is entirely
`+infinity`.

**Two tests could have caught it and each missed from a different side.**
`palace_frame::synthetic_attachment_readback_has_paired_ray_distance` does assert a finite
distance, but only for the procedural ball. The server's
`fixture_volume_response_carries_the_paired_palace_depth_attachment` does use a real dataset, but
only asserts the PFM *header* (`Pf\n32 24\n-1.0\n`) — which an all-`+infinity` surface satisfies
perfectly. Neither is wrong on its own; together they leave the actual contract untested.

It is **pre-existing**, not introduced here: nothing in this run touched `palace-png`, the
raycaster, or `render_frame_attachments`.

`local_zarr_render_carries_a_real_first_opacity_surface` now states the contract as a runnable
test, marked `#[ignore]` with the diagnosis so the default suites stay green while the defect is
visible and reproducible. `raycaster.glsl` writes `state_depth` immediately beside `state_colors`
and the colour plainly arrives, so the likely fault is the reader and the producer disagreeing
about which state-cache generation holds the surface — see the reservation comment in
`palace_png::read_raycast_attachments` — with a later progressive pass resetting a good distance
to the sentinel (`consts.reset_state` at raycaster.glsl:180) as the other candidate.

### Evidence

```text
cargo test --offline --workspace -- --test-threads=1
```

passes throughout, including server **17/17** and desktop **43 passed, 12 ignored**.
`palace-core` **81/81**, `palace-wgpu` **16/16** and `palace-wgpu-spike` **24/24** pass with
`--include-ignored`. `palace-frame` passes **10/10** by default; with `--include-ignored` it
reports **1 failure**, which is the documented defect above and is expected until it is fixed.

## Rechunk grows past the single-page bound (2026-09-19)

Item 4 of the day plan — the rechunk half of PLAN.md §9.3.1 item 5.

`PortableRechunkLayout` requires the **whole** input to fit one 4 MiB page
(`element_count(&input_dimensions) > PortableTensorPage::MAX_WORDS` is rejected), which caps a
source at about a million voxels — roughly a 102 cube, far below what this renderer targets.

`PortableRechunkStream` splits the work along the **slowest** axis into windows that each satisfy
that bound. Each window is an ordinary `PortableRechunkLayout`, so the existing fixed-binding
kernel runs unchanged: the kernel stays page-bounded while the source it can serve grows.

Two properties are deliberate:

- **The split axis is the slowest one**, so both the input run and the output run of every window
  are contiguous and a caller can stream them without gathering. Splitting a fast axis would make
  both strided; that is a different contract, not a silent fallback, and is refused.
- **Zero padding stays explicit rather than emergent.** Fast-axis padding remains inside each
  window's own memory extent, exactly as the single-window layout emits it. Split-axis padding is
  covered by no window and is reported separately as `padding_words` for the caller to zero-fill.
  A test asserts the windows plus that padding are the whole output.

A slice that alone exceeds the page bound is refused, because no split along the slow axis can
help it.

### The adapter test's first oracle was worthless, and a mutation showed it

`bounded_wgpu_recorder_replays_a_streaming_rechunk_window_by_window` records each window through
the existing kernel and assembles the result. It first compared against
`PortableRechunkStream::rechunk_words` — which is **derived from the same offsets under test**, so
the oracle moved with any bug in them. Mutating `input_offset` to drop its `output_begin` term was
caught by the core fixtures (3 of 4 failed) and **passed** on the adapter, proving the comparison
was empty.

The reference is now built from the layout's definition inline in the test, independent of the
stream. Re-checked against two mutations, both of which now fail it: dropping `output_begin` from
`input_offset`, and perturbing `output_offset` by one word. The fixture also asserts that the
single-window layout genuinely cannot express it, so it cannot pass by staying within the old
bound.

### Evidence

```text
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-core --lib -- --test-threads=1
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-wgpu-spike -- --include-ignored --test-threads=1
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-wgpu -- --include-ignored --test-threads=1
cargo test --offline --workspace -- --test-threads=1
```

passed **85/85**, **25/25**, **16/16**, and the workspace green throughout. Four new core fixtures
cover single-window equivalence, a genuine split with contiguous tiling, zero padding on both the
fast and split axes, and refusal of what a slow-axis split cannot bound.

Not done, and deliberately untouched: `resample_transform` is not redirected, and the
floor-nearest API stays explicit. A transform-compatible path still needs centre rounding, affine
coordinates and border policy stated first, exactly as item 5 requires.

## Diagnosing the empty first-opacity surface: four hypotheses eliminated (2026-09-19)

*Superseded — the root cause is found and fixed in the entry that follows this one. The
eliminations below are still sound and are kept for their method.*

An instrumented investigation of the defect recorded above. **Not fixed at the time of writing.** All probe scaffolding
has been reverted — `raycaster.glsl`, `raycaster/mod.rs` and `palace-png/src/lib.rs` are back to
their pre-investigation state and every suite is green. What follows is what the probes
established, so the next attempt starts from facts rather than from scratch.

Setup: the committed fixture at 32x24 through
`render_local_zarr_with_camera_attachments`, which paints **484** of 768 pixels and reports **0**
finite distances; the procedural ball through the identical path reports 39 finite.

**Eliminated — the reader and producer disagreeing about the state-cache generation.** This was
the leading hypothesis. Printing both keys shows they are *identical* in both cases:
`OperatorId(Id(9163…), "raycast")` / `ChunkIndex(0)` for the Zarr, and the matching pair for the
ball. The reader addresses exactly the surface the raycaster writes.

**Eliminated — the writes not reaching the reader.** Replacing the two `state_depth` writes with
a sentinel `42.0` produced **484** finite values on readback, exactly the painted count, and all
of them `42`. So the marching branch runs for precisely the painted pixels and its writes arrive
intact. The `else` branch never fires.

**Eliminated — `pow(0.0, ·)` returning NaN.** This morning's WGSL defect looked like an exact
match: a saturating transfer makes `1.0 - sample_f.a` exactly zero, and `NaN > 0.0` is false, so
`update_state` would report no contribution. Guarding the zero base in
`update_state` changed nothing — still 484 painted, 0 finite. A control that forced `alpha = 0.0`
dropped painted to 0, which proves the GLSL really is being recompiled, so the negative result is
trustworthy rather than a stale-shader artifact.

**Eliminated — the `&&` short-circuit.** `if(ray_distance == INF && update_state(...))` stops
calling `update_state` once a distance is recorded, which also stops compositing. Hoisting the
call out so it always runs changed nothing.

**Retracted — this entry originally claimed a fifth finding, and it was an instrumentation bug.**
A diagnostic ORing every `update_state` return across a ray read back `8` ("no contribution") for
all 484 painted pixels, and that was written up as the narrowed fact. It is not. The shader source
dumped by the compile error that ended the investigation shows the accumulator was only wired into
the **const-brick-table skip branch**; the main sampling branch called `update_state` without
touching it. The probe therefore reported "no contribution" for every ray that never took the skip
path, which is most of them. Nothing is established about what `update_state` returns.

The lesson is the one this run keeps relearning: a probe is code, and an unverified probe produces
confident nonsense. The eliminations above are sound because each was a *behavioural* change with
a control — the `alpha = 0.0` control in particular proved the shader really recompiles — whereas
this one was a read-only accumulator that was simply never updated.

**Eliminated — `apply_tf`'s unclamped float-to-uint conversion.** In
`palace-core/src/glsl/color.glsl`:

```glsl
float norm = ((input_val)-(tf_min))/((tf_max)-(tf_min));
uint index = min(uint(max(0.0, norm) * (tf_table_len)), (tf_table_len) - 1);
```

The `min` clamps *after* the conversion. The Zarr path wraps each level in
`jit(...).cast(F32).compile()`, so samples are raw `uint16` magnitudes — thousands — against
`TransFuncOperator::red_ramp(0.0, 1.0)`. `norm` is therefore in the thousands and
`max(0.0, norm) * tf_table_len` is far beyond `uint` range, where **float-to-uint conversion is
undefined in GLSL**. A driver that saturates yields the intended clamp; one that wraps yields an
arbitrary palette entry. The procedural ball's values sit inside the ramp, so it never reaches the
undefined path — which is exactly the split between "works" and "every real dataset fails".

**Tested, not reasoned about.** Rewriting the macro to clamp in float before converting
(`uint(min(max(0.0, norm) * len, len - 1))`) produced a **byte-identical** frame: same 484 painted
pixels, same colour checksum `7be230ec617e6ba9`, same first pixels. This driver already saturates
the conversion, so the intended clamp is what happens in practice. It remains formally undefined
GLSL and a portability hazard for other drivers, but it is **not** the cause here and is not a
live bug on this host.

That control also corrected a misreading. The varied reds (`51`, `171`, `92`) were taken as
evidence of wrapped palette indices; they are not. With the index clamped to the ramp top every
sample classifies identically, and the variation comes from `Shading::Phong` modulating `rgb` by
the gradient while alpha stays `255`. So the colours are consistent with *saturated* classification
throughout — meaning `sample_f.a` really is `1.0` on these samples, which is the zero-base `pow`
case that was separately tested and eliminated above.

Where that leaves it: the writes arrive, the keys match, the palette clamps, the zero-base guard
and the short-circuit are both no-ops, and nothing is known about `update_state`'s return because
that probe was faulty. The next attempt should instrument `alpha` itself with a correctly wired
accumulator — declared before use, and verified with a control that deliberately forces a known
value through it before any conclusion is drawn from it.

## The empty first-opacity surface: root cause and fix (2026-09-19)

**Fixed.** The paired first-opacity attachment was `+infinity` at every pixel for every real
dataset because `raycaster.glsl` recorded the ray distance *after* a call that had already
replaced it with NaN.

### The cause

`update_state` is declared `bool update_state(inout float t, ...)`. When a sample saturates the
ray it writes `t = T_DONE`, where `T_DONE` is `uintBitsToFloat(0xFFC00000u)` — a NaN bit pattern —
so that the marching loop's `t < t_end` test fails and the ray terminates. The main sampling branch
recorded the first contribution as

```glsl
if(ray_distance == uintBitsToFloat(0x7F800000u) && update_state(t, state_color, sample_col, norm_step)) {
    ray_distance = t;
}
```

By the time `ray_distance = t` runs, `update_state` has already executed, and if *this* sample
saturated the ray, `t` is NaN. The write site then guards a non-finite distance to `+infinity`
(which is what the sentinel probe reported: `ray_distance` non-finite at the moment of the write).

That is exactly the split between "works" and "every real dataset fails". A real dataset through
`TransFuncOperator::red_ramp(0.0, 1.0)` sees raw `uint16` magnitudes in the thousands, classifies
every non-zero voxel at the ramp top with `alpha = 1.0`, and saturates on the **first**
contributing sample — so the recorded distance is NaN on every painted ray. The procedural ball's
values sit inside the ramp, its first contributing sample is partially transparent, `update_state`
leaves `t` alone, and the distance survives. The ball was never a fair test of the contract.

The constant-chunk branch a few lines above was already correct: it captured `float sample_t = t;`
before advancing and recorded `sample_t`. The fix makes the main branch do the same:

```glsl
float sample_t = t;
if(ray_distance == uintBitsToFloat(0x7F800000u)
    && update_state(t, state_color, sample_col, norm_step)) {
    ray_distance = sample_t;
}
```

Twelve lines changed in `raycaster.glsl`, applied onto a fresh copy of the pristine file; the
`pow`-guard, `&&`-hoist and float-clamp experiments from the eliminations are **not** part of the
fix. `raycaster/mod.rs`, `color.glsl` and `palace-png` are untouched.

### How it was found

The four eliminations in the previous entry were right: the keys match, the writes arrive, the
palette clamps, and neither the zero-base `pow` nor the short-circuit is involved. What they had not
established was *what value* reached the write. The probe that settled it wrote a marker into the
**blue channel** of the colour output rather than into `state_depth`, because a probe written into
the depth cache is read back by the *next* progressive dispatch through the same `&&` guard and
confounds itself — that is what produced the retracted "no contribution" finding. Out-of-band in
the colour, two markers were unambiguous:

- `blue = 255` when the `ray_distance = t` statement executed: **484 of 484** painted pixels. So
  the statement runs on every painted ray, and `update_state` *does* report a contribution — the
  earlier retraction was correct to withdraw the opposite claim.
- `blue = 102` when `ray_distance` was non-finite at the write site: **484 of 484**. So the value
  being recorded is already not a number when it is written.

A distance that is written on every ray and is NaN on every ray can only come from `t` itself, and
the only thing that touches `t` between the loop head and the write is the `inout` parameter of
`update_state`. Reading its saturation branch closed it.

### Evidence

Debug profile, single-threaded, this host's local WGPU adapter and Vulkan.

- Reproduction before the fix (unchanged from the previous entry): 484 painted, **0** finite.
- After the fix, same fixture and path: `DEPTHPROBE painted 484 finite 484`.
- `local_zarr_render_carries_a_real_first_opacity_surface` in `palace-frame` is no longer
  `#[ignore]`d; its doc comment now records the cause. It passes.
- The server's `fixture_volume_response_carries_the_paired_palace_depth_attachment` now asserts
  at least one finite distance and that every distance is a finite non-negative value or
  `+infinity`. It previously checked only the PFM header, which an all-`+infinity` surface
  satisfies — that is the gap the defect lived in.
- **Mutation check.** With `raycaster.glsl` reverted to the pristine file and nothing else
  changed, both tests fail:
  `tests::local_zarr_render_carries_a_real_first_opacity_surface ... FAILED` and
  `tests::fixture_volume_response_carries_the_paired_palace_depth_attachment ... FAILED`.
  With the fix restored both pass. The failure mode of each was silent before this run (a
  green header check), so the mutation check is what makes them evidence.
- Temporary `probe_depth_values` test removed from `palace-frame`; `git diff --check` clean.
- Full sweep, all `--test-threads=1`: workspace green (desktop 43 passed / 12 ignored, server 17
  passed, every other crate green); `palace-core` 85 passed; `palace-frame` 11 passed / 0 ignored;
  adapter suites (`--ignored`, `--test-threads=1`): `palace-wgpu` 16 passed, `palace-wgpu-spike`
  19 passed, desktop 12 passed — every DVR parity, resolve, rechunk, resample and picker fixture
  is unchanged by the raycaster edit, as expected for a change confined to the Vulkan path.

### What the fix changes downstream

`compare_desktop_portable_and_server_renderers` (desktop, `--ignored`, 64x48 on
`cells3d-anisotropic`) now reads:

```
server: 1849 painted pixels, 1849 finite depths
finite depth: server 1849, portable 3072, both 1849; mean |delta| where both 12.5930
colour: 3072 differing (100.0%), mean channel delta 86.93
```

Before the fix the server column was `1849 painted, 0 finite` and the depth comparison was
vacuous. Two things in these numbers are **not** conclusions yet and belong to the next handover
item, the matched-transfer Vulkan comparison:

1. The portable route reports a finite distance at all 3072 pixels while the server paints 1849.
   The fixture's physical extent is 33.28 x 33.28 x 9.28 µm; the frame at `zoom 1.0` is filled by
   the box, so every ray enters it, and the two routes disagree about whether a ray that enters the
   box but crosses only zero-valued voxels has a first-opacity surface. The portable transfer and
   the server's `red_ramp` are not matched, so this is a transfer difference until proven
   otherwise.
2. The mean depth delta where both are finite is 12.59 µm against a 33 µm extent — far larger
   than the first-opacity-rule difference (`0.01` accumulated alpha versus first strictly positive
   sample) can explain on its own. With colour differing at 100% of pixels the two frames are not
   rendering the same classification, and the depth delta cannot be interpreted until they are.

Neither number is evidence for or against the portable renderer; both are the reason the
comparison needs an unshaded entry point and one transfer function on both sides.

### The lesson, stated once

An `inout` parameter that is also used as a sentinel is a hazard at every call site that reads the
variable afterwards. The const-brick branch got it right and the main branch did not, three lines
apart. The two tests that should have caught it each looked at the other half of the contract:
one asserted a finite distance on a fixture that could not fail, the other used a fixture that
did fail but asserted only a header. A contract test must combine the fixture that can fail with
the assertion that can fail.

## The Vulkan comparison, and what it found (2026-09-19)

Handover item 2 was "compare portable DVR against Palace's own Vulkan raycaster" — with an
unshaded entry point and a matched transfer, because until then every portable-versus-native
number had been portable-versus-portable. Doing it exposed three defects, none of them in the
portable compositor, and the comparison now runs as a pinned test. In the order they surfaced:

### 1. The Vulkan attachment measured from the volume's surface, not the camera

`raycaster.glsl` starts its march at `t == 0` on the box entry point (`start = eep.entry`) and
recorded `ray_distance = sample_t`. So the paired attachment — the moment it stopped being empty —
was the distance from the **entry face**, while `newvolim-render`'s `RayDistanceF32` contract,
`palace_frame::project_point_for_volume`, the portable renderers and the desktop pickers all
measure from the **camera**. Probed on the fixture at 64x48: the fitted camera sits at
`z = 76.6 µm` (centre `4.64` plus 1.5 diagonals), the box's front face at `z = 9.28`, and the
centre pixel's attachment read `0.58`. The all-`+infinity` surface had hidden it.

The raycaster has no camera in its push constants and is deliberately camera-agnostic, so the
entry/exit pass now carries the information: `entry_exit_points` derives the camera position from
its projection (the preimage of the homogeneous clip point `(0, 0, 1, 0)`, which a perspective
projection maps its own centre to; Palace stores homogeneous vectors `[w, z, y, x]`), passes it
and `norm_to_world` to the rasterizing fragment shader and to the inside-the-volume fix pass, and
each writes the entry record's fourth component as the **camera-to-entry distance** — strictly
positive, so it still serves as the validity flag. The raycaster records
`eep.entry_distance + sample_t`. An orthographic projection has no camera position; the flag
stays `1.0` and the attachment stays entry-relative, which is the only meaningful definition
there. Same centre pixel afterwards: `67.899` = `67.32` to the face + the old `0.58`.

Guards, both Vulkan-run with a slab-intersection oracle:
`local_zarr_first_opacity_distance_is_measured_from_the_camera` (every finite distance lies within
its own ray's crossing of the volume box, and the closest one is within four voxels of the entry
face) and `synthetic_first_opacity_distance_is_measured_from_the_camera` (the ball touches the box
faces, so the centre ray's distance is the camera-to-face distance to within three voxels).
**Mutation:** dropping `eep.entry_distance +` fails both.

### 2. The desktop treated Palace's physical camera ray as voxel coordinates

`CameraState::for_volume` fits its camera in physical units — the eye is 1.5 diagonals from the
centre of `spacing × dimensions` — yet `PortableCameraRay` was documented as "XYZ voxel
coordinates", `portable_camera_rays_xyz` passed the physical ray through as if it were, and
`demand_world_ray_from_camera` then applied `voxel_to_world` (scale and translation) to it. On the
fixture that scales the eye by `0.26`–`0.29` into a corner of the box: every one of 3072 rays hit
where the server's camera hits 1849, which is the "portable 3072 finite" number recorded in the
previous entry and now explained. Every synthetic adapter fixture has unit spacing, where the
mistake is invisible; and the picker tests that ride on the conversion accept `None` as a valid
outcome, so they never noticed either. (The legacy pick bridge `palace_ray_to_physical` was
right all along: it divides by the spacing before applying the NGFF transform.)

Fixed at both sites. `demand_world_ray_from_camera` adds only the layer translation — Palace's
frame has voxel `i` centred at `i × spacing`, which is NGFF's `translation + i × scale` without
the translation. `portable_camera_rays_xyz` now takes the level-zero spacing and divides the
physical ray by it (renormalizing the direction) to reach the direct volume's voxel coordinates;
`portable_scene_world_rays` then multiplies back, so the scene routes see physical rays plus the
translation as before. CPU guards, both pinned to exact values under an anisotropic transform:
`demand_world_ray_adds_only_the_layer_translation_to_the_physical_palace_ray` and
`fitted_palace_camera_rays_are_reordered_and_rescaled_for_the_portable_xyz_volume`.
**Mutation:** restoring the old body fails both, and the comparison below fails with **1136**
one-sided pixels against 1936 shared.

The camera-driven level selection had been tuned to the wrong camera.
`demand_scene_level_follows_the_camera_and_frame_extent` failed after the fix because 16x12
now selects the coarsest level at every zoom (two microns per pixel against a one-micron coarsest
spacing — the right answer). Re-measured on the corrected camera: 16x12 → 2 at every zoom;
64x48 → 1/2/2/2 at zoom 0.5/1/2/3; 128x96 → 0/1/2/2; 256x192 → 0/0/1/2; 512x384 → 0/0/0/1.
The test's distance pair now uses 128x96, where all three levels are reachable; it still requires
the fixture to distinguish both pairs and selection to leave level zero.

### 3. The Vulkan DVR composited exactly one sample per ray

With the camera and the units right, the matched comparison still showed the server at a mean
alpha of **19** over hit pixels against the portable pass's **208**, same hue. One sample's
worth. The cause was the guard this run had already looked at once, for depth:

```glsl
if(ray_distance == uintBitsToFloat(0x7F800000u) && update_state(t, state_color, sample_col, norm_step))
```

Once a distance is recorded the left operand is false and `update_state` is never evaluated
again — no compositing after the first contribution. The earlier diagnosis noted that hoisting
the call "changed nothing", which was true of the *depth* it was probing and false of the colour
nobody was measuring; every Vulkan frame the server has served since the attachment was added is
a first-hit rendering. Both sites now composite unconditionally and record the distance on the
first contribution:

```glsl
bool contributed = update_state(t, state_color, sample_col, norm_step);
if(contributed && ray_distance == uintBitsToFloat(0x7F800000u)) {
    ray_distance = eep.entry_distance + sample_t;
}
```

Afterwards the means are **201.8** and **208.2**. **Mutation:** restoring the `&&` form fails the
comparison's aggregate-alpha assertion with the 19-versus-208 numbers.

### The comparison itself

`compare_desktop_portable_and_server_renderers` (desktop, `--ignored`, needs Vulkan and a WGPU
adapter) now renders the fixture at 64x48 both ways with everything but the renderer matched:
Palace through the new `render_local_zarr_with_camera_attachments_using` with
`CameraRenderOptions { shading: None, transfer, lod_coarseness: 0.0 }` — the transfer is the
demand route's own channel-state table rebuilt as a `TransFuncOperator`, and zero coarseness
stops the raycaster's per-sample level walk at level zero — and the portable pass through
`render_demand_driven_scene_camera_draw_at_level(…, 0)`, a split of the demand route that lets a
comparison pin the level. `CameraRenderOptions::default()` reproduces the server path exactly.

Measured, debug profile, this host:

```text
pixels: 3072
finite depth: server 1936, portable 1936, both 1936, only one side 0;
  where both: mean |delta| 0.2239, max 0.2250, mean signed (portable - server) -0.2239,
  within one z voxel (0.29): 1936 of 1936
alpha over 1936 hit pixels: mean server 201.8, mean portable 208.2;
  portable - server percentiles 5/25/50/75/95: -51 -15 5 28 65
colour: 1936 differing (63.0%), max channel delta 137, mean channel delta 12.12
```

What each line means, and what the test asserts:

- **Hit set: identical.** Every ray that finds volume on one side finds it on the other. Asserted
  (one-sided pixels at most 5% of shared; measured 0).
- **Depth: a constant `−0.224`, explained.** Palace's sample *on* the entry face is
  `round(31.5) = 32`, outside the array, and is skipped, so its first contribution is one step
  (`|dir ⊙ spacing|`, `0.29` on the centre rays) inward; the portable pass starts half a step
  (`0.065`) in. `0.29 − 0.065 = 0.225`, the measured maximum, and the mean `0.2239` is the
  slightly shorter step on tilted rays. Asserted: every shared depth within one voxel, signed
  offset bounded by one Palace step. Neither renderer is wrong here; the contract is "first
  contributing sample" and each reports its own.
- **Colour: aggregate agreement, per-pixel scatter, and a hue bias in Palace.** Mean alpha
  within 3%. Per pixel the two sampling grids (nearest at `round(p / spacing)` versus
  `floor((p − min) / extent × dims)`, a half-voxel shift, at different steps) see different voxels
  through membrane-thin structure, hence the ±50 scatter. Separately, Palace keeps its state in 8
  bits between steps and `from_uniform` is `u8vec4(v * 255)` — truncation — so small increments
  are lost, most of all in the weak channels: the fixture's `[255, 51, 85]` transfer comes back
  from Palace at ratios like `[206, 28, 60]` while the portable pass, accumulating in `f32`,
  returns the transfer's hue exactly. Asserted: means within 10%, and the portable hue equal to
  the transfer's at every pixel with measurable colour. Palace's truncation is recorded, not
  fixed: it is a precision property of the Vulkan renderer's progressive state, outside this
  migration's remit.

### Evidence

All `--test-threads=1`, debug profile. Workspace green; desktop 44 default + 12 adapter (the
comparison included); `palace-core` 85; `palace-frame` 13 (two new); `palace-wgpu` 16 adapter;
`palace-wgpu-spike` 19 adapter + 6 default. `git diff --check` clean. Mutation checks as listed
under each defect: five guards, each failing without its fix.

### What this changes about earlier conclusions

- The "portable 3072 finite" and "mean delta 12.59" numbers in the previous entry were the
  wrong camera, not a transfer difference.
- The morning's "portable opacity 48× too strong" finding was derived from the GLSL formula and
  stands; but any *visual* judgement made against a Vulkan frame before today was made against a
  single-sample rendering.
- The picker tests `native_portable_picker_uses_its_matching_camera_packet_and_depth` and
  `native_portable_scene_picker_uses_ordered_scene_renderer_depth` accept `None`, and
  `fixture_native_pick_uses_the_paired_palace_depth_and_ngff_bridge` asserts `None`; they pass
  before and after every fix above and therefore pin nothing about the camera. They need a
  placed annotation that is *found*, which is now possible because both the camera and the depth
  are real. Left for the next session and listed in the handover.

## The picker tests now pick something (2026-09-19)

The three tests guarding annotation picking against the renderer-owned depth accepted `None`
(`fixture_native_pick_uses_the_paired_palace_depth_and_ngff_bridge` asserted it), so every camera
and depth defect found today satisfied them. Each now follows one recipe against its own route's
depth: find the pixel whose ray has the most room on both sides of the first-opacity surface
inside the fixture box (`roomiest_pixel`), place an annotation **behind** the surface — halfway
to the exit — and require the pick at that pixel to return `None`; place one **in front** —
halfway from the entry — and require the pick to return that annotation at its placed distance
(within the 0.5 µm pick radius). Annotations are placed by voxel (`place_on_ray` rounds to the
nearest level-zero voxel), which moves a point by at most 0.235 µm; the room bound is 0.6 µm so
the half-room margin stays above that.

- Legacy Vulkan pick (`pick_local_dataset_annotation`, default suite): depth from the server's
  own 32x24 frame, rays from `camera_rays_for_local_zarr` through `palace_ray_to_physical`, whose
  unit factor is now asserted to be **1.0** — the bridge's "Palace unit" is a physical micron. The
  server's saturating `red_ramp` surface sits at the first non-zero voxel, so the roomiest ray
  offers only **0.64 µm**; hence the bound.
- Direct portable pick (`pick_native_portable_annotation_for_session`, adapter): depth from
  `render_palace_portable_camera_draw` on the same packet, in the same order of preference as the
  pick; rays from the packet's voxel-space table through `portable_voxel_ray_to_physical`.
- Scene pick (`pick_native_portable_scene_annotation_for_session`, adapter): depth from one
  render of the packet's Palace scene, asserted equal to what `portable_scene_pick_depth` reads at
  the chosen pixel; rays are the packet's physical world rays.

**Mutation:** with `depth_aware_annotation_pick` handed `f64::INFINITY` instead of the renderer's
distance, all three fail at "an annotation behind the first-opacity surface must be occluded".
With the fix restored all three pass (default suite and `--ignored`, `--test-threads=1`).

One observation for the handover rather than a change: the two portable pickers read depth from
a **region-bounded static packet** (`origin_xyz`/`extent_xyz` in chunks — the tests use one
chunk), while the desktop's display route is the demand-driven scene over the whole level. A pick
against a one-chunk depth surface can therefore disagree with what is on screen wherever the
displayed frame has volume the packet does not. That is the pre-existing shape of the two
commands, not something introduced today.

## Resample, the transform-compatible half (2026-09-19)

PLAN §9.3.1 item 5 asked for a transform-compatible portable resample "only after centre
rounding, affine coordinates and border policy are encoded explicitly; never silently redirect
`resample_transform`". That is now the shape of it: a separate contract with the three decisions
written into it, a CPU oracle, a fixed-binding WGPU kernel, an opt-in operator bridge, and a test
that holds the oracle to Palace's own Vulkan operator. `resample_transform` and the floor-nearest
`PortableResampleLayout` are untouched.

### The contract

`PortableAffineResampleLayout` in `palace-core::gpu`, alongside its `PortableResampleBorder`:

- **Coordinates.** Palace's fastest-last order. A *global* output coordinate `g` — chunk begin
  plus local coordinate, exactly what the Vulkan shader forms as `out_brick_pos + out_begin` —
  maps to `A · [g0, g1, g2, 1]` for a row-major 4x4 `A` whose fourth column is the translation
  and whose fourth row must be `[0, 0, 0, 1]`; projective matrices are refused, and axes beyond
  the rank must be inert (identity on their row, absent from the others). The product is
  evaluated in `f32` as the translation followed by the axis terms in order, and the oracle and
  the kernel do it in that same order.
- **Rounding.** `floor(p + 0.5)`, round half up. It is written out rather than named because
  GLSL's `round` leaves ties to the implementation and WGSL's `round` is half-to-even.
- **Border.** `Repeat` clamps each rounded coordinate to `[0, extent − 1]`; `Pad0` reads any
  coordinate outside `[0, extent)` as zero. These are Palace's two `BorderHandling` variants under
  their own names.
- Chunked output with the same begin/logical/memory convention and zero padding as the other
  layouts; one complete input page.

The uniform is 36 words: rank, three extents each for input/output/begin/logical/memory, the
border word, then the sixteen coefficients as `f32` bits, and three zero words to a 16-byte
multiple. The kernel reads it as `array<vec4<u32>, 9>`.

`portable_affine_from_palace_matrix` converts a Palace `element_out_to_in` matrix: Palace stores
homogeneous vectors as `[w, c0, c1, …]` and serializes matrices column-major, so its GLSL `mul` is
a standard product with the translation in column 0 and the affine row in row 0. The converter
refuses a non-affine row 0.

### The pieces

- `PortableOperatorKind::ResampleAffineNd`, `PortableOperatorRequest::resample_affine_nd`,
  `PortableTensorPage::resample_affine_with` / `resample_affine_scalars`.
- `WgpuOperatorRecorder::record` gains the `resample-affine-nd` kernel; the uniform buffer is
  sized from the request instead of a fixed 16 words.
- `portable_resample_affine_cpu` in `operators::resample`: the page-bounded operator bridge,
  mirroring `portable_resample_cpu` — whole source assembled from CPU chunks, one layout per
  output chunk, recorder if installed, CPU page otherwise.

### Evidence

Debug profile, `--test-threads=1`.

- **CPU oracle** (`palace-core` `gpu::tests`, 3 new): identity reproduces the input and pads a
  chunk's memory; on a 1-D `out × 0.5` the exact halves round *up* (`10 11 11 12 12 13 13 …`)
  where a floor would give `10 10 11 11 …`, and the output that lands outside reads `13` under
  Repeat and `0` under Pad0; a `−1` translation gives `0 10 11 12` under Pad0 and `10 10 11 12`
  under Repeat; projective, coupled-inert-axis, NaN, rank-4 and oversized layouts are refused; the
  uniform words land where the kernel reads them.
- **Vulkan comparison** (`portable_affine_resample_oracle_matches_vulkan_resample_transform`):
  a 3x5 `u32` tensor through `resample_rescale_mat` into 2x3 under Repeat — coordinates
  `0.25, 1.75` and `0.33, 2.0, 3.67`, no ties — and through a `(+1, −2)` translation under Pad0.
  The oracle's words, pinned by hand (`[0, 2, 4, 10, 12, 14]` and
  `[0, 0, 5, 6, 7, 0, 0, 10, 11, 12, 0, 0, 0, 0, 0]`), equal Vulkan's `resample_transform` output
  through `compare_tensor_fn` in both cases.
- **Bridge operator** (`portable_affine_resample_operator_writes_chunked_output_like_the_oracle`):
  the same translation under Pad0 with 2x3 output chunks equals the oracle through the scheduler.
- **Adapter** (`palace-wgpu-spike`, `--ignored`,
  `bounded_wgpu_recorder_resamples_under_an_explicit_affine_transform`): the second output chunk
  of the translated page under both borders equals the oracle *and* a hand-written expectation
  (`[0 0 0 0 0]` then padding under Pad0; `[10 10 10 11 12]` under Repeat — row 3 clamps to row 2,
  columns −2/−1 to column 0), and a centre-based half-scale over the full page rounds its halves
  up on the GPU exactly as the oracle does.
- **Mutation checks**, each restored afterwards:
  - kernel `floor(pos)` instead of `floor(pos + 0.5)` → the adapter test fails on the half-scale
    row (`0 0 1 1 2 2 3 3 …` against `0 1 1 2 2 3 3 4 …`);
  - oracle `position.floor()` → the Vulkan test fails (`[0, 2, 3, 5, 7, 8]` against
    `[0, 2, 4, 10, 12, 14]`);
  - oracle Pad0 treated as Repeat → the Vulkan test fails (`[5, 5, 5, 6, 7, …]` against the
    zero-padded row).
  The two oracle mutations trip the pinned values first; `compare_tensor_fn` against Vulkan is
  what makes those pins trustworthy when the oracle is intact.
- Full sweep after the change, all `--test-threads=1`, debug: `palace-core` 90 passed;
  `palace-wgpu` 16 (`--include-ignored`); `palace-wgpu-spike` 26 (`--include-ignored`);
  `palace-frame` 13; workspace green (desktop 44 default + 12 adapter, server 17); `git diff
  --check` clean.

### What is deliberately not here

- No caller is redirected. `resample` (the LOD builder) still goes through `resample_transform`;
  selecting the portable path for admitted pages is a scheduler decision like the one already
  made for the floor-nearest resample and rechunk, and is left to that item.
- Ties. Palace's `round` and this `floor(p + 0.5)` can differ on an exact `.5`; the comparison
  avoids ties and the contract says so. A caller that needs tie-for-tie equality with Vulkan has
  no such guarantee from Vulkan itself.

## Pick versus display: one frame decision per route (2026-09-19)

The handover's open item was that the portable pick commands could test an annotation against a
surface other than the one on screen. Reading the shipped webview (`crates/newvolim-ui/index.html`)
first corrected the premise: the UI displays through `render_native_portable_camera_draw` — the
**direct**, region-bounded route — and picks through `pick_native_portable_annotation`. The
demand-driven scene route (`render_native_portable_scene_camera_draw`) exists and is tested but
the UI never invokes it; the 2026-09-19 handover text saying "the desktop displays the
demand-driven scene" was wrong and is corrected there.

So each route had its own pick/display gap:

- **Direct route.** The display used Palace's page DVR for an annotation-free packet and the
  native recorder for an annotation-bearing one, while the picker read Palace's depth first in
  every case. On this fixture that gap turned out to be hypothetical — Palace's page DVR does not
  admit a fitted-camera packet at all here (`"portable Palace DVR raymarch input is not admitted"`
  for every region tried, with or without annotations), so both sides fell to the native
  recorder. What was *not* hypothetical: the transported PFM carried the native recorder's
  **voxel-space ray parameter** as if it were a physical distance (`260.67` where the physical
  distance is `75.49`), while the picker converted per ray. The `RayDistanceF32` contract says
  physical.
- **Scene route.** The display rendered the demand-driven frame over the whole level; the picker
  rendered a static packet of the request's chunk region. Where the displayed frame has volume
  the packet does not, an annotation behind the visible surface was selectable.

### The change

`RouteFrame { attachments, rays, renderer }` and one function per route that decides it:

- `direct_route_frame(session, packet, physical_rays)` — Palace page DVR for an annotation-free
  packet when it admits one, else the native recorder with its distances converted per ray to
  physical by the same `physical_distance_per_palace_unit` the picker uses. The render command
  and `pick_native_portable_annotation` both consume it.
- `scene_route_frame(session, request)` — demand-driven frame with the rays it marched (the
  demand route now returns them, `demand_driven_scene_camera_draw_with_rays`), else Palace's
  static scene over the region, else the native scene recorder. The render command composites
  annotations on top for Palace frames (a rejected projected record is now an error rather than
  a quiet switch to the native recorder, which would have put a different surface on screen
  from the one the pick tests); `pick_native_portable_scene_annotation` consumes the same frame.
- `pick_in_route_frame(frame, index, annotations)` — the pixel's own ray and depth, `+infinity`
  occluding nothing.

`RouteRenderer` is recorded on the frame so a test can assert *which* frame the display used.
`FramePayload::native_wgpu_camera` is gone (nothing builds a payload from a raw native frame any
more) and `portable_scene_pick_depth` is test-only.

### Evidence

All `--test-threads=1`, debug. Annotations in these tests are placed **exactly** on a pixel's
ray (`LocalSession::add_point_annotation_physical`, test-only) rather than rounded to a voxel:
the fixture's volume begins at the entry face on nearly every ray, so a route sampling every
0.13 µm puts its surface 0.065 µm in and there is no voxel-sized room in front of it. The pick
compares an exact closest-approach distance against an `f32` depth, so a 0.02 µm room is three
orders above the depth's resolution.

- `pick_in_route_frame_uses_the_pixels_own_ray_and_depth` (CPU oracle): a two-pixel synthetic
  frame; the nearer annotation wins at its distance, one behind the depth is occluded, one on a
  `+infinity` pixel is found, an out-of-frame pixel is refused.
- `scene_pick_is_occluded_by_the_displayed_demand_frame_not_the_region_packet` (adapter): at
  128x96, zoom 0.5 (level zero on the corrected camera), the displayed frame is `Demand`; on the
  roomiest ray where the region packet finds no volume, an annotation behind the displayed
  surface returns `None` and one in front is found. **Mutation:** the picker reading the static
  packet again finds the hidden annotation (`Some(id 0 at 36.41)` against a surface at `31.75`).
- `direct_route_frame_reports_physical_distances` (adapter): the fixture packet's frame is
  `Native`; every finite distance equals the recorder's parameter times the per-ray factor, the
  factor differs from one somewhere, and a second evaluation is identical. **Mutation:** leaving
  the parameter unconverted fails at `260.67` against `75.49`.
- `native_portable_picker_uses_its_matching_camera_packet_and_depth` and
  `native_portable_scene_picker_uses_ordered_scene_renderer_depth` now read their surface through
  the route frame (behind occluded, front found at its distance). **Mutation:**
  `pick_in_route_frame` handed `+infinity` fails both.
- Sweep after the change, all `--test-threads=1`, debug, in the relocated target: workspace
  green (desktop 45 default + 14 adapter, server 17); `palace-core` 90 (rerun alone after the
  batched run's rebuild overran its slot); `palace-frame` 13; `palace-wgpu` 16;
  `palace-wgpu-spike` 26 (both `--include-ignored`); `git diff --check` clean.

### Environment note, because it cost an hour

Mid-run the `stable` toolchain was updated to rustc 1.98.1 (`~/.rustup/toolchains/stable-…`
modified 16:39), which invalidates every cached artefact: 102 GB in `target/` and 31 GB in
`palace-dev/target`, all built by 1.98.0. The full rebuild then failed in `shaderc-sys 0.9.1`,
whose bundled CMake project is rejected by the host's CMake without
`CMAKE_POLICY_VERSION_MINIMUM=3.5` — it had only ever built from cache. `.cargo/config.toml`
now sets that variable for build scripts. `/data` was down to 4.6 GB, so this session's builds
go to `/big/henriksson/cargo-target/newvolim` via `CARGO_TARGET_DIR`; nothing was deleted.

## The webview now shows the demand-driven frame (2026-09-19)

Until now the shipped page (`crates/newvolim-ui/index.html`) drove the volume canvas through the
direct, region-bounded route: `render_native_portable_camera_draw` for every frame and
`pick_native_portable_annotation` for clicks. The demand-driven scene route — the whole point of
§9.3.1 item 4 — was reachable only from tests. The volume canvas now renders through
`render_native_portable_scene_camera_draw` and picks through
`pick_native_portable_scene_annotation`, named once each as `NEWVOLIM_NATIVE_VOLUME_COMMAND` and
`NEWVOLIM_NATIVE_VOLUME_PICK_COMMAND`; the frame-kind flag and the camera-control attach check use
the same constant. Both commands take the request the page already builds
(`newvolimNativePortableDrawRequest`): the chunk region in it is consulted only by the scene
route's fallbacks, so the page's region plumbing is now inert for the volume. The direct route
stays registered for its own tests and is no longer invoked by the page. `dist/` was rebuilt with
`trunk build` (0.21.14, dev profile).

### Evidence

- `webview_volume_canvas_is_wired_to_the_scene_route` (CPU): reads the page source and requires
  the two constants with the scene command names, exactly one admit, one draw and one pick call
  through them, no string invocation of either direct command, and both scene commands in the
  desktop's `generate_handler!`. **Mutation:** the pick constant set back to the direct command
  fails it.
- `scene_render_command_payload_is_the_webview_contract_over_the_route_frame` (adapter): the scene
  render command, fed the page's request shape (a chunk region, canvas extent, orbit, zoom) with
  an annotation in the session, returns a payload the page's validator accepts — `image/png`,
  final, sRGB RGBA8, `rayDistanceF32` with a PFM of the frame's extent — whose decoded PFM equals
  the route frame's depth pixel for pixel, and that frame is `Demand`. **Mutation:** the render
  command displaying the static Palace scene instead fails the depth equality.
- Sweep, all `--test-threads=1`, debug, relocated target: workspace green (desktop 46 default,
  server 17), desktop adapter suite 15 passed, `trunk build` of the page succeeds; `git diff
  --check` clean. The palace crates were not touched by this step.

## The voxel-centre convention, stated once (2026-09-19)

Annotations, NGFF and Palace put voxel `i` at `translation + i × scale`; the desktop's layer box
was corner-based, `[t, t + s × dims]`, so its sampling rule `floor((p − min) / extent × dims)`
painted voxel `i` over `[t + i·s, t + (i+1)·s)` — centred half a voxel away from where an
annotation "at voxel `i`" is drawn. The comparison against Vulkan had measured the consequence
as a constant `−0.224` depth offset it could explain but not remove.

Decided: **voxel-centred everywhere.** A layer box runs from `origin − 0.5` to
`origin + dims − 0.5` voxels, and every renderer reads the nearest voxel of a local coordinate.

- Desktop: `layer_world_box(transform, origin, dims)` is the one place the box is built, used by
  the direct route (whose page-local rays are now converted through the transform of the global
  voxel coordinate, not offset from `minimum`), the static scene, the scene extent folds, the
  demand route and level selection.
- `newvolim-render`: `PortableSceneLayerInput::world_ray_interval` intersects `[-0.5, dim − 0.5]`.
- `newvolim-wgpu-frame`: the direct shader's slabs and the scene shader's bounds are
  voxel-centred and both sample `floor(local + 0.5)`.
- Palace-side contracts are untouched: `PortableDvrVolumeLevel` takes whatever box it is given.

### Evidence

All `--test-threads=1`, debug, relocated target.

- `layer_world_box_centres_each_voxel_on_its_annotation_position` (CPU): under an anisotropic
  scale, a translation and a non-zero page origin, every voxel's `voxel_to_world` position lands
  at cell coordinate `i + 0.5` of the box on every axis. **Mutation:** the corner-based box fails
  it (and `fitted_native_camera_and_page_submission_adapt_to_palace_dvr`).
- Four pinned values re-derived, each with its arithmetic in a comment: the render crate's
  interval `(0, 8) → (0, 6)`; the native scene ray range `[1, 3] → [0.5, 2.5]` and its packed
  words; the direct-route CPU render `2.0 → 1.5` (twice) and `4.0 → 3.0` under scale 2. Two
  native scene adapter tests marched a single-voxel layer at `(0.5, 0.5)` — the corner box's
  cell centre, now the voxel's edge — and ran through nothing; their rays go through the voxel's
  position `(0, 0)`.
- `compare_desktop_portable_and_server_renderers`: hit set still identical (1936/1936/0
  one-sided), mean alpha 201.8 vs 207.3, and the depth offset against Vulkan is now
  `−0.0760` mean / `−0.0800` max — down from `−0.224` by the predicted `0.145` (half a 0.29 µm
  voxel) — with every shared depth still within one voxel.
- The pick tests (exact placement) pass unchanged; the fixture box in their oracle is now
  voxel-centred too.
- Sweep, all `--test-threads=1`, debug, relocated target: workspace green (desktop 47 default,
  render 25, server 17), desktop adapter suite 15, `newvolim-wgpu-frame` 8 with adapter tests;
  `git diff --check` clean. Palace crates untouched by this step.

What remains different from Palace is Palace's own entry face: its entry/exit pass rasterizes
the corner box `[0, dims × spacing]` while its sampler is voxel-centred, so its rays start half a
voxel earlier than ours and its on-face sample rounds outside the array. That is a Palace
convention, recorded, not something this migration should mirror.

## Scheduler selection of the affine resample (2026-09-19)

The transform-compatible resample now has the same selection the rechunk and floor-nearest
paths have, and in the same place.

- `portable_resample_affine_dynamic` — the `DType` entry to `portable_resample_affine_cpu`
  (`u16`/`u32` scalars), mirroring `portable_resample_dynamic`.
- `portable_resample_transform_admits` — the decision, exposed on its own: a lossless scalar
  tensor whose complete input and output each fit one portable page, under a matrix and rank
  `PortableAffineResampleLayout::new` accepts.
- `select_resample_transform` — the portable affine path when admitted, the Vulkan
  `resample_transform` otherwise. It is the one place the choice is made; callers wanting a
  specific path call it directly, and nothing is redirected silently.
- `py-palace`'s `Tensor.resample_transform` now goes through the selector, exactly as its
  `rechunk` already selected the portable page for admitted tensors and kept Vulkan for larger
  ones. The LOD builder (`smooth_downsample` → `resample` → `resample_transform`) is generic
  over the element type and is deliberately left on Vulkan: it runs on every dtype and on
  volumes far past one page, and a selector inside a generic operator would have to specialise
  on the element type to say anything.

### Evidence

- `select_resample_transform_takes_the_portable_path_only_for_an_admitted_page` (CPU +
  Vulkan): the selection is observable through operator identity. An admitted `u16` 3x5 page
  under a `(+1, −2)` translation with `Pad0` resolves to `portable_resample_affine_cpu`'s
  operator and its values are the oracle's; an `f32` tensor and a 1025x1024 `u16` tensor (one
  row past the 4 MiB page) resolve to `resample_transform`'s operator, the `f32` case with
  values equal to a direct Vulkan call. **Mutation:** the selector forced to Vulkan fails on the
  admitted page's identity (`"resample_transform"` against `"portable_resample_affine_cpu"`).
- `runtime_selects_wgpu_for_the_bounded_affine_resample` (adapter, `palace-wgpu-spike`): the
  selected operator with an installed WGPU recorder produces the oracle's chunked, zero-padded
  words through the ordinary scheduler.
- `cargo check -p palace --no-default-features` compiles the Python binding with the selector
  (default features need ffmpeg development libraries this host lacks).

## Palace rounds its 8-bit state (2026-09-19)

`from_uniform` in `palace-core/src/glsl/color.glsl` was `u8vec4(v * 255)` — truncation. The DVR
raycaster converts its accumulated colour through it on every step, so any increment smaller
than a level was dropped, most of all in a tinted transfer's weak channels: with `[255, 51, 85]`
the fixture came back at ratios like `[206, 28, 60]`. Both overloads now round
(`u8vec4(clamp(v, 0, 1) * 255 + 0.5)`); the only users are the raycaster's state and
`intensity_to_grey`.

### Evidence

`compare_desktop_portable_and_server_renderers` (matched transfer, unshaded, level zero):

- mean alpha over hit pixels: Palace **207.7** against portable 207.3 (was 201.8 — the
  truncation bias); per-pixel `portable − server` alpha percentiles 5/25/50/75/95 now
  `−57 −21 0 21 57`, median exactly zero; mean channel delta 12.25 → 9.79.
- Palace's green over 1874 bright pixels: bias **+0.47** levels against the transfer's hue,
  worst pixel 7.2. Asserted: `|bias| ≤ 1.0` and worst `≤ 12`. **Mutation:** truncation restored
  gives bias `−11.92`, worst 20.4, and the assertion fails.
- Sweep after items 3 and 4, all `--test-threads=1`, debug, relocated target: `palace-core` 91,
  `palace-frame` 13, `palace-wgpu` 16 and `palace-wgpu-spike` 27 (both `--include-ignored`),
  workspace green (desktop 47 default + 15 adapter, render 25, server 17); `git diff --check`
  clean. No palace-core test pins raycast bytes, so the rounding change broke nothing there.

## Transfer-function control (2026-09-19)

The scene's `ChannelState` (enabled, sRGB colour, window, opacity) was read-only from the
`omero` metadata: nothing could change it. Now:

- `Scene::layer_mut` (newvolim-scene); `LocalSession::layer_channels` and
  `LocalSession::set_channel_state(layer, channel, state)`, which validates the window and
  opacity and refuses to disable a layer's last enabled channel — a layer with none is dropped
  from the render plan, and the one-layer demand route would then fall back to a Vulkan frame
  that ignores the state altogether.
- Desktop commands `layer_channels` and `set_channel_state`, the latter taking a camelCase
  `ChannelStateInput` (the scene's own type serializes snake_case; the page had been reading
  `colorSrgb` off it, which is a separate bug in the browser chunk path, noted in TODO2).
- The page: a transfer panel (`#newvolim-channels`) with enabled, colour, window start/end and
  opacity per channel; a change goes through `set_channel_state`, then the volume and the
  orthogonal views re-render. `dist/` rebuilt.

Evidence (`--test-threads=1`, debug): session test — an edit reaches `layer_render_plan`,
invalid window/opacity/addresses are refused, the last channel cannot be disabled; CPU LUT test —
window `[1000, 3000]` at opacity 0.5 classifies below the window to alpha 0, at the end to 127,
the colour throughout; adapter test — opacity 0 empties the demand frame and its depth surface,
a window starting at 20000 finds less volume than `[0, 65535]`; page wiring pinned.
**Mutation:** the setter made a no-op fails the session and adapter tests.

## The portable host is a library, and the server renders through it (2026-09-19)

`crates/newvolim-portable` now holds `session` (moved from the desktop) and `routes` (every
non-command item of the desktop's `main.rs`: the route frames, demand route, picks, payloads,
transfers, with their tests). The desktop keeps its Tauri commands, `main`, and the tests that
read `main.rs` itself. Nothing was rewritten in the move; the split was scripted by item, and
the suites carry over whole: `newvolim-portable` 47 default + 16 adapter, desktop 3 (from 50 +
16 and 3 before). `py`-style rename traps aside, the only edits were visibility and imports.

The server (`newvolim-server`) then gains a `SessionStore`: one `LocalSession` per configured
dataset, opened on first use exactly as the desktop's open command does. A volume `FrameJob`
renders through `render_volume_frame`, which is `scene_route_frame` — the same function the
desktop displays and picks against, in the same order of preference — with the session's
annotations composited, and falls back to Palace's Vulkan raycaster only without a session. The
`Server-Timing` header now carries `route;desc="portable-demand"` (or `-palace`, `-native`,
`vulkan`). Channel edits reach the server too: `GET/POST /v1/datasets/{dataset}/channels` and
a frame-socket message carrying `layerId, channel, state` (parsed ahead of frame requests as an
untagged enum), replied to with a `channels` message.

Evidence: `socket_requests_parse_frames_and_channel_edits`;
`channel_edits_persist_in_the_dataset_session` (opened once, edit visible to the next snapshot,
earlier snapshot unchanged, bad channel refused); adapter test
`server_volume_frame_is_the_portable_route_frame` — the served PFM decodes to exactly the
`scene_route_frame` depth for the same camera and the route is `portable-demand`; without a
session the Vulkan frame as before. Server suite 19 passed. **Mutations:** the route ignoring
its session fails (`"vulkan"` against `"portable-demand"`); the store dropping the edit fails
(`1.0` against `0.25`).

Not done here: the orthogonal socket view still renders through Vulkan, and the page's remote
frame client does not yet send channel edits or read the route name.

## Multi-layer scenes (2026-09-19)

The session could hold one image layer, bound to the one opened dataset, and the demand route
admitted exactly one. Now:

- **Per-layer datasets.** `LocalSession` keeps `layer_datasets: LayerId → (root, metadata)`;
  the first layer reads the opened dataset, layers added with
  `add_portable_image_layer(root)` read their own OME-Zarr (level zero, `omero` channels and
  windows, built by the same `image_layer_for_dataset` as the default layer). Every per-layer
  resolution goes through that map: `local_layer_render_requests_at_levels` (one level per
  layer), `layer_chunk_plan_for_chunks_at_level`, `portable_layer_level_transform`,
  `portable_layer_level_spacings`, and `read_local_layer_chunks` checks each plan against its own
  layer's root. The four static page bindings are shared by every enabled channel of every layer,
  so a layer that would take the scene past four is refused before it enters the scene.
- **The demand route renders every visible image layer.** Each layer gets its own camera-chosen
  level (`demand_scene_levels`; the camera is fitted to the first layer, the scene's reference,
  and each layer's footprint is measured where the centre ray enters *that* layer's box), its
  own grid, transform and voxel-centred box; the rays are clipped to the union of the boxes; the
  step is the finest layer's; residency loops are numbered by a scene-wide (layer, channel)
  ordinal that keys the shared page and request tables and spaces the page owners; layers are
  composited in scene order by the existing scene shader. `render_demand_driven_scene_camera_draw_at_level`
  applies one level to every layer, which keeps the Vulkan comparison as it was.
- Desktop command `add_portable_image_layer(root)` with a page control ("Add layer from
  OME-Zarr"); the channel panel lists every layer. Server `POST /v1/datasets/{dataset}/layers`
  with `{"dataset": name}` adds a *registry* dataset as a layer of another dataset's session —
  names only, never paths.

### Evidence

- `a_second_image_layer_is_bound_to_its_own_dataset` (session, CPU): two requests with distinct
  roots and per-layer levels; the second layer's transform `[0.25, 0.25, 0.5]` and two pyramid
  levels come from its own metadata; a chunk plan and read for it alone; the panel lists both;
  a third layer that would enable five channels is refused.
- `demand_scene_renders_a_second_layer_from_its_own_dataset_in_its_own_box` (adapter): with
  the cells layer silenced (opacity 0), the demand frame's surface exists exactly on the 50 rays
  that cross the gradient fixture's 4 µm corner box (every hit's ray crosses it; hits are at
  least half the crossings, since the gradient is zero along its first row and column), its
  colour carries no blue (the gradient channels are red and green), and the frame is the scene
  route's frame. **Mutation:** the route truncated to its first layer gives 0 hits on those 50
  rays. A byte comparison against the one-layer frame was tried first and is the wrong test:
  adding a layer changes the scene-wide step and the union box, so every ray's samples move.
- `a_registry_dataset_can_join_another_dataset_session_as_a_layer` (server, CPU); the desktop's
  wiring pin covers the command and the page control.
- The one-layer suites are unchanged: `newvolim-portable` 48 default + 17 adapter, server 20,
  desktop 3; sweep recorded in the handover.

What this does not do: a labels layer (the scene model has the kind; the demand route renders
image layers only), and per-layer visibility toggles in the page.

## The browser renders the whole scene itself (2026-09-19)

Until now the web client marched one chunk region per layer with its own shader and camera.
It now renders the whole scene — every layer at its camera-chosen level — with the **desktop's
own scene shader** over **byte-identical inputs**, in one dispatch:

- `palace-wgpu`: the recorder's host-side packing is factored into `scene_dvr_dispatch`
  (`SceneDvrDispatch`: four pages, scene data, rays, 16-word uniform, request capacity, output
  words, workgroups) and `scene_frame_from_output`; `SCENE_DVR_SHADER` is public. The recorder
  itself now uses both, so what the desktop uploads and what is served cannot drift.
- `newvolim-portable`: the demand route is split into `prepare_demand_scene` and
  `assemble_demand_scene`; `full_level_scene_inputs` feeds every chunk of each layer's level to
  the residency loops up front — no feedback iteration — and assembles the same shader input the
  converged demand loop would. A level past the four-page budget is refused, not partially
  resident.
- `newvolim-server`: `GET /v1/datasets/{dataset}/portable/scene?width&height&orbitX&orbitY&zoom`
  returns `BrowserScenePacket`: the WGSL, the levels (the demand route's choice for this camera),
  and the dispatch words as little-endian `u32` base64, for the dataset's session — so channel
  edits and added layers apply to the browser's frame too.
- The page: `newvolimRenderServerScene` fetches the packet, creates the nine bindings in the
  recorder's order, dispatches `packet.workgroups` of `packet.shader`, reads the output back
  (colour words then depth bits), blits the colour to the canvas and keeps the depth; camera
  moves re-fetch through the existing debounced path. A button "Render server dataset here
  (WebGPU)" next to the chunk-server controls. `dist/` rebuilt.

Client-side planning (the residency feedback loop in the page, fetching only missed chunks) is
not done: the server plans the full level and the page renders it. That is client-side rendering
of the whole volume with the shared contract; it is not yet client-side residency.

### Evidence

- `browser_scene_packet_reproduces_the_desktop_frame_on_the_local_adapter` (server, adapter):
  the packet is consumed exactly as the page consumes it — base64 decoded, nine bindings in the
  documented order with a bind-group layout the page's `layout: "auto"` derives, the packet's
  own WGSL and workgroup count, output decoded as colour then depth — and the frame equals
  `scene_route_frame`'s for the same session and camera (96x64, orbit 12/−7, zoom 1.3),
  **pixel for pixel in both colour and depth**, with the route `Demand`. This is the strongest
  check available without a browser on this host.
- `webview_dispatches_the_browser_scene_packet_with_the_documented_bindings` (server, CPU): the
  page's dispatch names the endpoint, binds 0–8 in order, dispatches `packet.workgroups` of
  `packet.shader`.
- **Mutations:** the served step doubled — the reproduction test fails; full-level planning
  emptied — it fails; the page binding rays before scene data — the pin fails. (Swapping width
  and height in the uniform is a no-op by the shader's contract — it uses only their product and
  the pre-indexed rays — and so was not used as a mutation.)
- `palace-wgpu` 16 adapter and `palace-wgpu-spike` 27 pass unchanged after the refactor;
  `newvolim-portable` 48 default + 17 adapter; server 21 default.
- Sweep after all four TODO2 items, all `--test-threads=1`, debug, relocated target: workspace
  green (portable 48, server 21, render 25, io 24, desktop 3, wgpu-frame 3, ui 3); adapter
  suites portable 17, server 2, wgpu-frame 5; `palace-wgpu` 16 and spike 27
  (`--include-ignored`); `trunk build` succeeds; `git diff --check` clean. One catch on the way:
  the page's two new wrapper functions lacked the file's `#[cfg(target_arch = "wasm32")]`
  gate and broke the native workspace build until gated.

## Compressed and NGFF 0.5 stores read through `zarrs` (2026-09-19)

The reader accepted only the raw little-endian `bytes` codec, so every real store the user
pointed at (the public IDR and ome-zarr-scivis images referenced by `omezarr_viewers-rs`) was
unreadable by the portable route and the browser packet. Instead of hand-rolling codecs, chunk
reads now go through `zarrs` 0.18 (already in the offline registry, used by `palace-zarr`):

- `newvolim_io::read_array_region(root, array_path, start, shape)` opens the array with
  `zarrs::array::Array::open` and retrieves exactly the logical region — Zarr v2 or v3, `bytes`
  at either endianness, blosc, zstd, gzip, crc32c, transpose, sharding — returning C-order
  little-endian element bytes. An edge region comes back at its logical extent, never padded.
- `LocalSession::read_local_layer_chunks` computes each chunk's origin as `coordinates ×
  chunk_shape` and reads the region; the edge-aware length check stays. The raw asset reader
  `read_local_asset` remains only for the browser's one-chunk preview and the
  `/zarr/{asset}` route, which decode nothing.
- Zarr v2 `dimension_separator` is honoured: `ArrayInfo.dimension_separator` from `.zarray`,
  `LocalChunkKeyEncoding::V2Slash` for `"/"` (the IDR/bioformats2raw layout), so asset paths
  and error messages name the file that exists.
- NGFF 0.5 roots: `attributes.ome.{multiscales,omero}` (ome-zarr-scivis) are read; 0.4-in-v3
  roots (`attributes.multiscales`) keep working; an `ome` key without multiscales does not hide
  legacy attributes. Shared by the local, remote and S3 metadata readers.
- Nested dataset paths (`scale0/backpack`) work unchanged.

Two test fixtures had carried truncated v3 array metadata (no `zarr_format`, codecs or fill
value) that only the old reader tolerated; they now carry valid metadata. The truncated-chunk
guard is kept: a 153-byte chunk is rejected by the codec pipeline, never padded.

### Evidence

- `compressed_chunks_are_decoded_through_zarrs` (portable, CPU): an NGFF 0.5 store in the
  ome-zarr-scivis layout (`attributes.ome`, array at `scale0/image`), 2×4×6×10 uint16 with
  1×2×4×4 chunks (edges on y and x), written with `zarrs` under three codec chains — zstd 5;
  blosc zstd 5 with byte shuffle; gzip 6 + crc32c — each chunk on disk verified not raw; five
  spatial chunks × two channels read back equal to the analytic value `c·1000+z·100+y·10+x`
  at every element.
- `zarr_v2_blosc_chunks_are_decoded_through_zarrs` (portable, CPU): the IDR layout key for
  key (`.zarray`, `<u2`, blosc lz4 level 5 shuffle 1, `dimension_separator: "/"`, one chunk
  per z slice), chunks written by `zarrs`; every planned chunk decodes to the analytic value,
  the encoding is `V2Slash`, and each address's asset path names an existing file.
- `reads_ngff_0_5_root_metadata_under_the_ome_key` (io, CPU): the backpack root verbatim
  (multiscales and omero under `ome`, translation transforms, nested paths); a 0.4-in-v3 root;
  an `ome` key without multiscales.
- **Mutation:** session reads switched back to the raw asset reader — both codec tests fail
  (`chunk scale0/image/c/0/0/0/0 has 62 bytes, expected edge-aware 64`; the v2 test at the
  file lookup). A `zarrs` quirk found on the way: its `store_metadata` writes a `node_type`
  key into `.zarray` that its own v2 parser rejects, so the v2 test writes the `.zarray`
  itself, as real stores have it.
- Fixed a first-pass mistake of my own: the mutant's `u64::MAX` byte budget overflowed inside
  the raw reader, so the first mutant run failed for the wrong reason; rerun with a finite
  budget it fails at the length check.
- Suites after the change, all `--test-threads=1`, debug, relocated target: io 25 (+1),
  portable 50 default (+2) + 17 adapter, server 21 default, desktop 3; `git diff --check`
  clean. Release server rebuilt.
- **Real data, release profile, `POST /v1/frame` 256×192, route `portable-demand`, on the
  local adapter, timings including PNG encoding and HTTP:** the IDR image
  `idr0062A/6001240.zarr` (v2, blosc lz4, 2 channels × 236 × 275 × 271 uint16) mirrored to
  `/big/henriksson/omezarr-public/` renders in 0.42 s cold, 0.12–0.26 s warm, showing the
  blue nuclei and yellow cytoplasm channels; `v0.5/96x2/backpack.ome.zarr` (v3, zstd,
  373 × 512 × 512 uint16, `scale0/backpack`) renders in 0.39 s cold, 0.09 s warm. Before the
  NGFF 0.5 fix the backpack request failed on both routes ("array metadata is missing"). The
  server now serves both alongside the fixtures (`--allow-root /big/henriksson/omezarr-public`).
- A false alarm, recorded so nobody chases it: frames looked orbit-invariant over HTTP because
  the probe sent `orbit_x`; the API is camelCase (`orbitX`), and with that the frames differ.

Not done: the stores were mirrored with a script (`scratchpad/mirror.py`, 1416 + 176 keys,
no errors); opening `http(s)://` or `s3://` directly is still TODO2 item 1. The browser's
one-chunk raw preview and the `/zarr/{asset}` route still serve stored bytes verbatim.

## One port: the server serves the page (2026-09-19)

The page had been served by a Python `http.server` on a second port, with the API on its own
and a CORS allow-list between them. The user wants a Rust deployment on a single port.

- `newvolim-server --page-dir <dir>` serves the Trunk `dist/` of `newvolim-ui` at `/` through
  `tower_http::services::ServeDir` as the router's fallback: API routes take precedence,
  directory requests get `index.html`, unknown paths are 404, nothing escapes the directory.
  Without the option `/` is 404 as before.
- The page's `newvolimChunkServerOrigin` falls back to `window.location.origin` when the box
  is empty and the page is on `http(s):`, so the single-port deployment needs no configuration
  in the browser; a `file:`/`tauri:` page must still name the server. `dist/` rebuilt.
- `--cors-origin` remains for the split deployment (page on one host, renderer on another).

### Evidence

- `page_dir_serves_the_built_page_beside_the_api` (server, CPU): `/` returns the directory's
  `index.html` as `text/html`, `/app.js` as JavaScript, `/v1/datasets` still answers, a missing
  file is 404, `/../Cargo.toml` is not 200, and a router without the option returns 404 at `/`.
- `webview_defaults_the_chunk_server_to_its_own_origin` (server, CPU): pins the fallback in
  the page's origin helper and that it applies only to an empty box.
- Server 23 default (+2), 2 adapter (`--ignored`), all `--test-threads=1`, debug; release
  build and `trunk build` succeed; `git diff --check` clean. No rendering code changed, so no
  new adapter test.
- Live: one process on `0.0.0.0:9876` serves `/` (130 KB HTML), the 34 KB JS, the 15.9 MB
  wasm as `application/wasm`, and `/v1/datasets`, checked from the host address; the Python
  server is stopped and port 8080 is closed.

## The web interface, rebuilt (2026-09-20)

The user asked for a total overhaul of the GUI in the style of `omezarr_viewers-rs`, which
already had a mature Yew viewer. Its frontend was studied first (layout, stylesheet, panel
components, state model, the pure-Rust orientation box, its API contract), and the parts that
are backend-agnostic were ported to `newvolim-ui` (Leptos 0.8 CSR, Rust, one small JavaScript
file for the WebGPU API). The 2189-line `index.html` of page JavaScript with 95 `window.*`
functions is gone.

**What the page is now** (`crates/newvolim-ui`):

- `src/api.rs` — the server's wire vocabulary (frame and channel requests, the four socket
  replies, layer summaries, channel input), the origin rule (empty box = the page's own
  origin over http(s); `file:`/`tauri:` must name the server) and the route URLs. Pure serde,
  tested natively with JSON recorded from the live server.
- `src/cube.rs` — the orientation box from `omezarr_viewers-rs` (`CubeView`: near-isometric
  orthographic camera, plane hit test, drag-to-fraction), pure Rust with native tests.
- `src/app.rs` — `Session` (signals; latest-only request discipline per kind: a newer camera
  or crosshair while a frame is in flight only marks it dirty and the reply sends the next
  request), the frame socket, and the components: front page (dataset browser, server URL),
  tab strip, tool strip (Grid / XY / 3D view modes; Server / WebGPU renderer; reset), the 2×2
  grid of XY, XZ, YZ slices with crosshair overlays (click moves the crosshair, wheel steps
  the perpendicular axis) and the 3D pane (drag orbits, wheel zooms) with the orientation box
  inset (drag a plane to scrub its axis), axis sliders, a status line, and the sidebar of
  layer cards with per-channel checkbox, colour, contrast dual-range and opacity, plus "Add
  layer". `?dataset=name` deep-links straight into a dataset.
- `scene-webgpu.js` — the browser-side renderer: fetch the scene packet, dispatch the
  desktop's WGSL on WebGPU with the nine documented bindings, blit. Called from Rust.
- `style.css` — the other viewer's stylesheet with its literals lifted into variables.
- Release wasm 2.6 MB (`trunk build --release`, `data-wasm-opt="0"` because `wasm-opt` is
  not installed and cannot be fetched offline); the previous dev build was 17 MB.

**Three backend defects the new page exposed, all fixed:**

1. **Orthogonal frames went through Palace's Vulkan slicer**, which fails on the compressed
   mirrors ("array metadata is missing"). `portable_orthogonal_slices` /
   `portable_orthogonal_slice_pngs` (routes) slice the base layer on the CPU from the portable
   session at the finest pyramid level whose whole extent fits the four static pages; the
   server's orthogonal branch uses it, Vulkan is the fallback, and the fallback's error now
   carries the portable reason. Planes come at the level's own resolution (the page stretches
   them; the crosshair overlay is a fraction of the pane). Further layers are not sliced yet.
2. **The CPU slicer's XZ and YZ planes were transposed and truncated**: `palace_scene_slice_rgba_with_sampling`
   ran z across and the other axis down while declaring the opposite width and height, so the
   desktop's side panes had been wrong all along and nothing pinned them. Measured on a store
   with one unique value per voxel and fixed; the guard is the measured mapping.
3. **The demand route refused any pane larger than about 256 px on real data**: the camera
   chose a finer level than the four pages hold (`ExceedsPortableBound`, 17 pages for the IDR
   image, 128 for backpack) and the frame fell to Vulkan, which fails on those stores. The
   route now steps every layer one level coarser and retries until the level fits or none is
   left (`coarsen_levels`, `DEMAND_EXCEEDS_PAGE_BOUND`), and the scene route reports the demand
   route's reason when every fallback fails.

Also found and recorded: the socket's discriminator is `type`, not `kind` (the server renames
its `kind` field on the wire); my first page rejected every reply and my first unit test had
only confirmed my own assumption — the tests now use recorded server JSON.

**Not in this pass:** the Tauri desktop. The page no longer invokes Tauri commands, so the
desktop host's webview needs an HTTP server behind it; running `newvolim-server` in-process
is the next step (HANDOVER). The desktop's scene commands stay registered for that. Also not
built: annotations in the page (the server has no annotation API), picking in the browser,
labels layers, physical aspect ratio of the side slices, per-layer visibility (the server has
only per-channel enable).

### Evidence

- `newvolim-ui` native (CPU): 9 tests — camelCase request keys (`orbitX`…), the flattened
  channel edit, the four replies from recorded server JSON with the old `kind` guess rejected,
  the origin rule, route URLs, colour hex; cube fit / no edge-on axis / slab refuses a z drag,
  press grabs the nearest plane, a full-span drag moves a cut face to face and a perpendicular
  drag does not move it.
- `newvolim-portable` (CPU): `portable_orthogonal_slice_pixels_map_to_voxels_as_documented`
  (8×8×4 store, `v = 256·(x+8y+64z)`, every pixel of all three planes decoded from alpha to
  its voxel: XY (x across, y down) at z=3, XZ (x, z) at y=5, YZ (y, z) at x=2);
  `portable_orthogonal_slices_agree_across_planes_and_report_their_level` (gradient fixture:
  XY row y_c equals XZ row z_c, XZ column x_c equals YZ column y_c, level 0, clamped and
  centred crosshairs, PNG headers and dimensions); `coarsen_levels_…`.
- `newvolim-portable` (adapter, `--ignored`): `demand_route_coarsens_the_level_when_the_pages_cannot_hold_it`
  — a two-level zstd store, 512×512×40 uint16 over 256×256×20; at 640×480 the camera wants
  level 0, level 0 alone fails with the bound message, the route renders (Demand) with more
  than a tenth of the rays hitting.
- `newvolim-server` (CPU): the bindings pin now reads `scene-webgpu.js`; the orthogonal test
  expects per-plane voxel dimensions (128×128, 128×32, 128×32 for cells3d). Desktop: the two
  page-wiring tests replaced by `webview_talks_to_the_server_routes_and_invokes_no_desktop_command`.
- **Mutations:** raw session reads → both codec tests fail (earlier entry); the old transposed
  sampling → both slice tests fail (`(0,5,1)` vs `(1,5,0)`); coarsening disabled → the adapter
  test fails with "11 pages needed at the coarsest levels [0]".
- Suites, all `--test-threads=1`, debug, relocated target: portable 53 default + 18 adapter,
  server 22 + 2 adapter, ui 9, io 25, desktop 2; `trunk build --release` succeeds; `git diff
  --check` clean.
- **Live, release server, Chrome 2026-09-20 driven over the DevTools protocol** (the other
  repo's `tests/browser/cdp.py`; screenshots in the session scratchpad `final-1..3.png`):
  `?dataset=idr6001240` opens to the grid with slices at level 2 (67×68, 67×236, 68×236) in
  88–100 ms and the volume frame in 388 ms at 559×406; a click in XY moves the crosshair
  (135,137,118 → 80,164,118) and the slices follow; a 120×−60 drag orbits and the volume
  returns in 362 ms. Volume frames over the socket, orbit 30/−20: IDR 105 ms at 256×192,
  ~300 ms at 560×406, ~0.9–1.0 s at 1120×810; backpack 40 / 170 / 710 ms. Before the
  coarsening fix every size above 256×192 was an error.
- Headless-Chrome screenshots with `--virtual-time-budget` are unreliable for this page (the
  budget does not wait for the network); the CDP driver with a real settle is what works.

## Orbit axes were swapped (2026-09-20)

The user noticed that dragging in the 3D view mixed up x and y. Measured by projecting physical
points through `palace_frame`'s camera on the cells3d fixture: a horizontal drag left the point
on the screen-horizontal axis fixed (a pitch) and a vertical drag left the vertical-axis point
fixed (a yaw). Cause: `camera_for_volume` handed `orbit_delta` (`[dx, dy]`) to the trackball's
`pan_around` as a Palace `Vector<D2>`, which is stored `(y, x)` like Palace's `(z, y, x)`
volumes — so `dx` became the vertical component. Every route derives its rays from this one
camera, so the desktop, the server frames, the browser packet and the old page all had it.

Fix in `palace-dev/palace-frame/src/lib.rs`: `pan_around([-dy, dx])` — `dx` is the trackball's
x (a yaw whose near face follows a rightward drag) and the trackball's vertical component moves
the near face up for a positive value, so a screen-down drag is its negative.

### Evidence

- `horizontal_orbit_yaws_and_vertical_orbit_pitches` (portable, CPU): on cells3d
  (128×128×32 at 0.26/0.26/0.29) the centre projects to (199.5, 149.5) under every orbit; at
  rest +x is right and +y is up; under orbit [200, 0] the vertical-axis point stays, +x moves
  along the horizontal and the near point moves right; under [0, 200] the horizontal-axis point
  stays and the near point moves down.
- **Mutation:** the old order — the test fails at "yaw keeps the vertical axis: [199.5, 84.7]".
- Sweep, all `--test-threads=1`, debug: palace-frame 13, portable 54 + 18 adapter, server 22 +
  2, desktop 2, wgpu-frame 3 + 5, render 25; the route-comparison and picker tests pass
  unchanged because every route shares the corrected camera; `git diff --check` clean.
- Release server, backpack at 320×240: orbit [300, 0] turns the volume about the screen-vertical
  axis (the scanner's cylinder axis goes horizontal), [0, 300] tilts it; before the fix the two
  were the other way round (`scratchpad/orbit-montage.png` vs `orbit2-montage.png`).

## The browser packet coarsens its level too (2026-09-20)

The user pressed "WebGPU" and the object vanished. The scene packet route
(`/portable/scene`) planned the camera's level with `full_level_scene_inputs` and, at pane
size over a real volume, stopped at the four-page bound (`ExceedsPortableBound`, 17 pages
for the IDR image, 128 for backpack) — the same refusal the volume route had until the
coarsening fix, which had not been applied here. The page then had an empty canvas and the
error only in the status line.

`full_level_scene_inputs_fitting` (routes) chooses the camera's levels and steps every layer
coarser until the whole level fits, returning the levels used; `browser_scene_packet` uses it.
`full_level_scene_inputs` now reports the bound with the `DEMAND_EXCEEDS_PAGE_BOUND` prefix so
the retry keys off a known condition.

### Evidence

- `full_level_planning_coarsens_the_level_when_the_pages_cannot_hold_it` (portable, CPU — the
  packet's planning and page assembly touch no adapter): on the oversized two-level store at
  640×480 level 0 alone is refused with the bound message and the fitting planner returns
  levels `[1]` with one ray per pixel.
- **Mutation:** coarsening disabled — the test fails with "11 pages needed at the coarsest
  levels [0]".
- Portable 55 default + 18 adapter, server 22 + 2, `git diff --check` clean.
- Release server: the packet for IDR and backpack at 560×406 returns in 0.34 s (21–26 MB of
  JSON, level 2) and at 1120×810 in 0.8–0.9 s (50–55 MB); both were HTTP 400 before. The
  packet carries the rays and pages as base64 words, so a pane-sized browser frame is tens of
  megabytes per camera move — the price of the page computing nothing itself; client-side
  residency (TODO2) is the way past it.

## A turntable camera, and XZ / YZ views (2026-09-20)

After the axis swap was fixed the user still found the horizontal drag wrong. The remaining
cause was the camera model: the server applied Palace's `pan_around` once with the *total*
drag. That nudges the look vector additively — right for one mouse event, but as a function of
the whole drag it saturates at 90° and turns about a drifting axis, so a long horizontal drag
stopped reading as a turn about the vertical.

`camera_for_volume` (palace-frame) is now a turntable: the total horizontal drag spins the
fitted eye about the volume's vertical axis (image y) at 0.01 rad per pixel, the vertical drag
tilts it, clamped to ±89° so `up` stays defined; zoom still moves the eye in and out. It is one
function every route derives its rays from, so the desktop, the server frames, the browser
packet and the projections all share it.

The page gained XZ and YZ single-pane modes beside XY, Grid and 3D; the orthogonal request is
sized by whichever slice pane is visible and is skipped while none is.

### Evidence

- `horizontal_orbit_yaws_and_vertical_orbit_pitches` (portable, CPU) extended: a 314-pixel
  drag (π) shows the volume from behind — +x mirrored to the left, the near face now farther
  than at rest by ray distance — a 157-pixel drag puts +x on the view axis, and a 2000-pixel
  vertical drag still yields a camera.
- **Mutation:** the previous camera (the fixed-order nudge) — fails at "half turn mirrors +x:
  [247.6, 149.5]" (it never crosses the centre).
- Sweep, all `--test-threads=1`, debug: palace-frame 13, portable 55 + 18 adapter, server 22
  + 2, desktop 2, wgpu-frame 3 + 5; `git diff --check` clean.
- Live, release server, Chrome over CDP on backpack: a 160 px right drag turns the scan a
  quarter turn about the vertical (the lid stays on top, the side comes round), a 100 px down
  drag tilts it (`scratchpad/turn-montage.png`); XZ mode shows one 1120 px pane with the
  crosshair and slices in 110 ms.

## Client-side residency (2026-09-20)

The user asked for it. Until now the browser's own renderer received the *whole* level in the
packet (tens of MB per camera move) and could show only a level that fits the four pages. Now
the page runs the demand-driven residency loop itself and fetches only the chunks the shader
missed, keeping them across camera moves.

**Enablers.** `palace-core` builds for wasm32 once `gpu-allocator` is a native-only dependency
(the Vulkan modules that use it were already `cfg(not(wasm32))`); its portable `gpu` module
imports nothing but `std::ops`. The scene dispatch packing, the output decoding and the WGSL
(`SceneDvrDispatch`, `scene_dvr_dispatch`, `scene_frame_from_output`, `SCENE_DVR_SHADER`,
the trace labels) moved from `palace-wgpu` into `palace-core::gpu` — 350 lines that touch no
wgpu type — and `palace-wgpu` re-exports them. A wasm build also needs `getrandom` 0.3's
`wasm_js` backend (feature + `.cargo/config.toml` cfg for the wasm target).

**`crates/newvolim-residency`** (new, pure): `ScenePlan` (per layer: id, level, extent, chunk
shape, box; per channel: source index, scene ordinal, residency tag, owner base, transfer),
`ChunkRequest` (layer, level, channel, chunk index and grid coordinate), `ChunkCache` (words
by ordinal/level/chunk, bounded, oldest out first, the newest never starved), and
`ClientResidency`: one `PortableResidencyLoop` per channel, `absorb_requests` decoding the
shader's request table exactly as the server does, `missing_chunks`, `insert_chunk`,
`assemble` (the server's `assemble_demand_scene` + `demand_scene_layer_pages` +
`portable_chunk_plan_pages`, with the cache for the disk: placeholder page when a channel has
nothing yet, chunks placed at the plan's `first_word`, page lengths and owners from the plan)
and `dispatch` (the recorder's nine bindings). `coarser_levels` for the page-bound retry.

**Server** (`newvolim-portable` + `newvolim-server`): `demand_scene_plan` (the prepared scene
minus pages and rays) and `layer_chunk_words` (the words of named chunks, X-fastest logical
extent, via the session's chunk planner and `zarrs` reads), behind
`GET /v1/datasets/{d}/portable/plan?width&height&orbitX&orbitY&zoom[&levels=a,b]` (JSON),
`GET …/portable/rays?…` (binary LE words, eight per pixel) and
`POST …/portable/chunks {layerId, level, sourceIndex, chunks:[[x,y,z]…]}` (binary: per chunk a
word count then the words, up to 256 per request).

**Page** (`newvolim-ui`): "WebGPU" now runs the loop — plan and rays for the camera, then
dispatch, absorb, fetch the missing chunks grouped by (layer, level, channel), dispatch again
until complete; `ExceedsPortableBound` re-fetches the plan one level coarser with the cache
kept; the cache (256 MiB) persists across frames. `scene-webgpu.js` is now only `dispatch`
(one pass, returning output and request words) and `present`; the shader is the page's own
copy of `SCENE_DVR_SHADER` from palace-core, installed into the script once. When WebGPU is
unavailable the page says so in a sticky notice and falls back to server frames instead of
leaving the 3D pane empty.

### Evidence

- `newvolim-residency` (CPU): a frame starts with a placeholder page and an empty map; missed
  keys plan chunks in ascending order once each; dispatch before fetching is refused; two
  fetched chunks land back to back at the plan's placement (a full 4×4×1 chunk then the 2×4×1
  edge chunk); an empty request table completes; a key for a channel the frame lacks is an
  error; a chunk larger than a page reports the bound; the cache evicts oldest first and keeps
  an over-budget newcomer; rays round-trip through their words; the plan is camelCase JSON.
- `client_residency_assembles_the_servers_dispatch_word_for_word` (portable, CPU): a
  256×256×64 store in 64×64×32 chunks (32 chunks); both sides absorb every other chunk
  (16 chunks over two full pages); the client's `dispatch()` — pages, scene data with the
  residency map, rays, uniform — equals the server's `scene_dvr_dispatch` of its own assembly,
  with the plan having gone through JSON and the chunks through `layer_chunk_words`.
  **Mutation:** chunks paged in reverse plan order — fails ("chunk 30 is placed at word 917504
  but the page holds 0"). (On the cells3d fixture this mutant passed, because each channel
  planned one chunk; hence the 32-chunk store.)
- `client_residency_loop_renders_the_servers_demand_frame` (portable, adapter): the client loop
  driven with the session's reads and the local adapter converges to the server's demand frame,
  colour and depth pixel for pixel, having fetched only what the shader asked for.
- `newvolim-ui` (CPU): the three route URLs, the chunk request body and the binary chunk reply
  decoder, including short and trailing input.
- Sweep, all `--test-threads=1`, debug: residency 4, ui 10, io 25, render 25, portable 56 +
  19 adapter, server 22 + 2, desktop 2, wgpu-frame 3 + 5, palace-frame 13, palace-wgpu 16
  (own directory), palace-core 91 native and a clean wasm32 build; `trunk build --release`
  (wasm 2.85 MB); `git diff --check` clean.
- Release server, IDR image at 560×406: the plan is 8 KB in 46 ms, the rays 7.3 MB in 67 ms,
  three level-0 chunks (one z-slice each, 74 525 words) 894 KB in 20 ms.
- **Not verified here:** the page's WebGPU leg. Headless Chrome on this host grants no WebGPU
  adapter (hardware Vulkan flags or SwiftShader), so the loop in the browser was exercised only
  as far as the adapter request; it then fell back to server frames with the notice "WebGPU is
  present but no adapter was granted", which is the intended fallback. The dispatch glue is
  the same shape as the previous packet renderer's. It needs a browser with WebGPU.

**Left open:** the rays are still fetched (7 MB per pane-sized move); generating them in the
page from the camera would leave only chunks and an 8 KB plan on the wire. GPU buffers are
recreated per pass. Only image layers; the per-pass fetch is serial per (layer, level,
channel) group.

## Slice panes are 2-D cameras (2026-09-20)

The user: wheel in a slice pane should zoom, not step the third axis, and the drawn red
crosshair is the wrong model — the crosshair is implicitly the centre of the pane, and the pane
should pan. Done as in `omezarr_viewers-rs`:

- The session holds a continuous `focus` (voxels) and a per-pane `zoom_2d`; the integer
  `crosshair` the slices are cut at is the focus's floor, and the slices are re-requested only
  when that integer changes. Each slice image is placed so the focus sits at the pane's centre
  at `fit × zoom` pixels per voxel (square voxels; the whole slice fits at zoom 1).
- Drag pans the focus along the pane's two axes, which moves the other two panes' cuts.
  Wheel zooms about the cursor (the voxel under it stays put), 0.25×–64×; the zoom is shown in
  the pane's corner. The depth axis moves through the axis sliders, the orientation box or a
  pan in another pane. No crosshair lines are drawn.

### Evidence

- Live, release server, Chrome over CDP on the IDR image: a 100×40 px drag in XY moved the
  crosshair from (135, 137, 118) to (67, 110, 118) — the slice is 1.48 px per voxel, so 68 and
  27 voxels — and the image moved by exactly the drag; a wheel of −500 zoomed XY to 1.50× with
  the image growing from 400.6 to 600.9 px wide; a 60 px upward drag in XZ moved z from 118 to
  153 and re-cut the XY slice; the page contains no crosshair elements
  (`scratchpad/pan-zoom.png`, `pan-xz.png`).
- The pane logic is DOM-bound (`app.rs`), so it is verified by the driven browser above; the
  native suites are unchanged (ui 10).

## See-through depth (2026-09-20)

The user asked how transparency is handled and whether a slider is needed. How it works: a
sample's alpha is the channel's linear window ramp times its opacity; that alpha is
step-corrected as `1 − (1 − a)^(step / opacity_reference)` (Beer-Lambert), the reference being
the scene diagonal / 256; channels and layers composite front to back and the ray stops at
0.95. The per-channel opacity scales alpha linearly, so it cannot make a ray see much deeper;
the reference can, exponentially, and it was hard-coded.

At introduction: `LocalSession::depth_scale` (`DepthScale`, 0.05..=20, default 1), a multiplier on the
opacity reference in `scene_opacity_reference`, which both the demand route and the direct
route take their reference from (so the browser plan carries it too). Server:
`GET`/`POST /v1/datasets/{d}/settings` with `{depthScale}`. Page: a "Scene" block in the
sidebar with a logarithmic Depth slider (0.1×–10×), shown at once, sent latest-only, then the
volume re-renders; slices are unaffected. The early-termination threshold stays 0.95.

### Evidence

- `depth_scale_multiplies_the_opacity_reference_of_every_route` (portable, CPU): the direct
  route's reference is diagonal / 256 at 1; at 4 both the plan's `opacity_reference` and the
  direct reference are 4×; 0, −1, NaN, ∞ and 100 are refused and leave the old scale.
- `depth_scale_lets_light_through_deeper_on_the_adapter` (portable, adapter): the demand
  frame's mean alpha over the frame falls 153 → 130 → 64 at scales 0.25 / 1 / 4 while the set
  of pixels that hit the volume is identical.
- `settings_route_sets_the_depth_scale_the_plan_renders_with` (server, CPU): GET reads 1.0,
  POST 2.5 is stored, POST 0 is 400 and leaves 2.5, and the plan's `opacityReference` is 2.5×.
- `settings_are_camel_case_and_the_depth_slider_is_logarithmic` (ui, CPU).
- **Mutation:** the multiplier dropped — the portable test fails ("0.1874 vs 0.1874") and the
  server test fails ("the plan's opacity reference scales: 1").
- Suites: ui 11, portable 57 + 20 adapter, server 23 + 2, `git diff --check` clean.
- Live, release server, Chrome over CDP on the IDR image: the slider at 10× stores
  `{"depthScale":10.0}`, the volume re-renders in 486 ms, and the 3D pane's mean luminance
  falls from 91 to 50: the yellow cytoplasm haze becomes see-through and the ring of nuclei
  inside is visible (`scratchpad/depth-montage.png`).

The later depth-range correction extends the session's upper bound from 20× to 100× and the
logarithmic slider from 10× to 100×. The lower slider endpoint stays at 0.1×. Portable, server,
and UI tests check that 100× is accepted and reaches the render plan; 100.01× is rejected.

## Why the 3D renderers are slow, and a far-face bug found on the way (2026-09-20)

The user asked why both 3D renderers are slow. Measured with `measure_demand_frame_phases`
(portable, ignored, release profile, local adapter) at the page's pane size 560×406, orbit
30/−20, on the two mirrored stores:

| phase, IDR 6001240 (2 ch, 271×275×236, one z-slice per chunk) | ms |
|---|---|
| level choice from the camera → level 0 | 0.1 |
| level 0: prepare (camera rays for 227 k pixels, loop setup) | 27 |
| level 0: bootstrap dispatch, 472 misses → 17 pages needed, refused | 6 + 37 (133 on the very first dispatch of the process) |
| level 1: prepare + bootstrap dispatch → 5 pages needed, refused | 21 + 2 + 15 |
| level 2: prepare | 23 |
| level 2: bootstrap dispatch, 472 misses | 2 + 13 |
| level 2: read + decompress 472 chunks (8.2 MiB) and assemble pages | 65 |
| level 2: dispatch + readback | 22 |
| PNG 255 KB + PFM 888 KB encode | 2 |
| **whole frame** | **258** (server reports 275–300 over HTTP) |

Backpack (1 ch, 512×512×373, 47×128×128 chunks): the same shape, 225 ms, of which 31 ms
reading 16 chunks (11.6 MiB) at level 2 and ~60 ms spent refusing levels 0 and 1. The same
full-level dispatch repeated costs 18–21 ms, so the GPU pass is not the problem.

**Where the time goes, server:**
1. ~110 ms (IDR) / ~60 ms (backpack) refusing levels that cannot fit: the camera's level
   choice ignores the page budget, so every frame tries level 0, then 1, each with fresh rays,
   a loop setup and a bootstrap dispatch, before settling on level 2.
2. ~65 ms re-reading and decompressing every planned chunk from disk on every frame (and on
   every iteration within a frame): nothing is cached between frames on the server.
3. ~25 ms per `prepare` generating 227 k camera rays on the CPU, three times per frame.
4. The socket frame carries an 888 KB PFM depth image (1.2 MB base64) that the page never
   reads, on top of the 255 KB PNG.
5. The first dispatch of a process pays ~100 ms of pipeline compilation.

**Where the time goes, browser (WebGPU):** the same loop over HTTP: the plan (20–25 ms
server-side) is fetched once per level attempt (three times here), the rays once (7.3 MB —
70 ms on localhost, seconds on a remote link), one WebGPU pass per attempt plus two at the
final level, and the chunks (8 MiB for IDR level 2, in two requests). Each pass uploads all
pages again and creates a new pipeline; the browser's shader-compile cache decides what that
costs. On the IDR store residency cannot save anything: a chunk is a whole z-slice, so a
camera that sees the volume needs every chunk of the level. Backpack's 47×128×128 chunks do
allow partial residency when zoomed in.

**What would fix it** (not done; the user asked for the investigation): choose the level with
the page budget in hand — the finest level whose whole extent fits when the camera sees the
whole box (skips the refused attempts: −110 ms IDR, −60 ms backpack; the plan route likewise);
a chunk-word cache in the session keyed like the browser's (−65 ms per frame, −reads per
iteration); generate rays once per camera and reuse across attempts and iterations; send the
PFM only when a client asks; generate rays in the page from the camera (−7 MB per move); keep
the WebGPU pipeline and page buffers across passes. Together the server frame should land
near 60 ms and the browser frame near one pass plus chunks.

**The bug:** the measurement's level-1 pass on the IDR image recorded chunk 236 of a 236-chunk
grid — one past the far face — and the demand route refused the frame ("scene shader demanded
a chunk outside its own channel"), which is why `POST /v1/frame` for the IDR image at 560×406
was a 500 on the live server. The shader's box test passes for a point just inside the far
face, but `floor((p − min) / (max − min) · dims)` rounds up to `dims` in f32; the CPU oracle
had always clamped to `dims − 1`, the WGSL had not. Fixed by the same clamp in
`SCENE_DVR_SHADER`.

### Evidence

- `scene_shader_requests_only_chunks_inside_the_grid` (portable, adapter): every key the
  bootstrap pass records lies inside its channel's grid, over six cameras (including the
  page's 560×406 at orbit 30/−20) and every level of cells3d, a synthetic 32-chunk store and,
  when mirrored, the IDR and backpack stores — 10 128 keys. **Mutation:** the unclamped line —
  fails on the IDR mirror at "level 1 orbit [30, -20] zoom 1: chunk 236 of 236". (Without the
  mirrored store the case does not arise on the stores in the repository; the test then
  passes either way, which is stated in its docs.)
- palace-wgpu 16 (the WGSL-versus-CPU-oracle pins), spike 27, portable 57 + 22 adapter,
  server 23 + 2; `git diff --check` clean.
- Live, release server: the IDR image at 560×406 orbit 30/−20 is 200 again, route
  `portable-demand`, 275–280 ms after the first frame; backpack 236–245 ms.

## The renderers, faster (2026-09-20)

The four largest items from the investigation, done:

- **Budget-aware level choice** — `demand_scene_levels_fitting`: the camera's levels, stepped
  coarser up front while a layer's whole level cannot fit the four pages and the camera sees
  the whole box (zoom ≥ 1). Zoomed in, the camera's level stands and the loop's own
  refusal-and-coarsen handles a working set that does not fit. Used by the demand route, the
  browser packet and the browser plan.
- **A session chunk cache** — `SessionChunkCache` in `LocalSession`, decoded chunk words by
  (layer, level, channel, chunk), 512 MiB per dataset, oldest out first, shared by every
  clone of the session (the server hands out clones). `layer_chunk_words_cached` reads from
  disk only what no frame has read; the demand route's pages and the browser's chunk route go
  through it. Pages are placed by `newvolim_residency::assemble_pages`, which the browser's
  own assembly now uses too, so the two cannot pack differently.
- **The depth image on request** — a socket frame carries the ray-distance PFM only with
  `depth: true`; the page never asks.
- **One WebGPU pipeline per device** in `scene-webgpu.js` instead of one per pass.

### Evidence

- `chunk_cache_serves_repeat_frames_and_level_choice_skips_levels_that_cannot_fit` (portable,
  CPU): on a 256×256×96 store in 64×64×32 chunks the first frame's pages read 8 chunks
  (786 432 words), the second frame's pages are identical and read nothing (packed-page hit;
  decoded-chunk cache remains at 0 hits, 8 misses),
  a clone of the session reads nothing either, the chunk route serves chunk 3 from the same
  cache and refuses a coordinate outside the grid; at zoom 1 the level choice is `[1]`
  (level 0 is 6.3 M words, beyond 4.2 M) where the camera alone says `[0]`, and at zoom 0.4
  it stays `[0]`. **Mutations:** the cache never storing — fails at "(0, 8, 0, 0) vs
  (0, 8, 8, 786432)"; the level choice ignoring the budget — fails at "[0] vs [1]".
- `socket_frame_requests_ask_for_depth_explicitly` (server, CPU): `depth` defaults to false and
  the socket branch drops the PFM unless it is true.
- Suites, all `--test-threads=1`, debug: residency 4, portable 58 + 22 adapter, server 24 + 2,
  ui 11; `git diff --check` clean; release server and `trunk build --release` rebuilt.
- `measure_demand_frame_phases` (release, local adapter), 560×406, orbit 30/−20: the level
  choice is `[2]` at once — no refused attempts; IDR whole first frame 188 ms (was 258), of
  which 71 ms reading 472 chunks cold; the second frame through the route with the cache warm
  72 ms (472 hits, 0 new reads). Backpack 104 ms cold, 74 ms warm.
- Live, release server, `POST /v1/frame` at 560×406: IDR 468 ms for the first frame of the
  process, then 90–103 ms (was 275–300); backpack 259 ms then 85–89 ms (was 236–245). The
  socket message for a pane-sized frame is 340 KB without depth, 1524 KB with (was always the
  latter).

The browser now expands the fitted camera in the plan into clipped per-pixel rays in wasm. The
plan route fits the camera without making the ray table, and the browser no longer fetches the
7 MB ray response per camera move. The `/portable/rays` route remains as a compatibility and
comparison endpoint. The browser's GPU storage and readback buffers persist across passes and
camera moves, growing only when an input exceeds capacity; device loss clears the buffers and
pipeline. A mocked WebGPU run confirmed that a repeat pass allocates no buffers and a larger
page replaces only its own buffer. Browser GPU execution still needs an adapter-equipped host.
The plan's locally expanded ray words equal the demand route's words after a JSON round trip for
three orbit/zoom settings at two levels on the anisotropic fixture; the client dispatch oracle
also uses these local rays. A two-layer scene verifies clipping to the union box. The wasm
target, release page bundle (`trunk build --release`) and release server build pass.
An approved localhost smoke run of the rebuilt server served a 4,653-byte plan with the camera,
the legacy 42,656-byte ray response for 43×31 pixels, and the rebuilt page and renderer script.
The temporary server was stopped after the check.

## 3D drag camera uses a quaternion (2026-09-20)

The 3D pane now stores one unit XYZW orientation and zoom. Each pointer move composes a rotation
around an axis in the current camera's screen plane. The eye and up vectors rotate together, so
horizontal motion stays horizontal even after a vertical half-turn; there is no pitch stop or
fixed yaw axis. The fitted camera accepts that orientation alongside its legacy orbit fields, and
the server frame, scene packet, and browser plan routes carry it to the same camera calculation.
The browser expands rays from the oriented plan. Slice panes keep their existing controls.

`drag_orientation_composes_camera_local_rotations_without_a_pitch_limit` checks composition,
half-turns, and unit length after 10,000 moves. The portable camera test checks projected drag
direction and a horizontal turn beyond the pole. The plan test verifies local rays exactly match
the server rays after an oriented plan's JSON round trip. Server request and plan route tests cover
the optional orientation field. Legacy requests without it retain their previous camera path.

## Faster 3D interaction and wheel direction (2026-09-20)

The 3D wheel factor now follows the fitted camera's actual convention: `zoom` multiplies eye
distance, so a wheel movement toward the screen lowers it and brings the volume closer. Slice
wheel handling is separate and unchanged. During 3D drag and wheel input, both renderers request
half the pane's physical width and height. The image fills the pane while input is active; a
full-size frame is requested 180 ms after the last input. A generation counter makes older
settle timers harmless, and the existing latest-only request queue coalesces camera moves.

The native demand route now expands rays by scanline on up to eight workers for larger frames.
For a fitted or farther camera whose chosen levels fit the four pages, it plans the whole level
and dispatches once, skipping the GPU pass that only discovered missing chunks. Closer cameras
retain shader-driven partial residency. The page budget gate was corrected for the camera's
distance convention (`zoom >= 1` means farther). Exact browser/server ray words are checked
above the parallel threshold, and an adapter test compares full-level and feedback output.

At 560×406, the previous live warm baseline was 75 ms (IDR) and 65 ms (Backpack) per frame.
The release adapter probe after these changes measured full-size warm frames at 53 and 47 ms;
the 280×203 interaction previews took 26 and 22 ms, respectively (2.9× and 3.0× faster than
that baseline). The full-size frame still completes after interaction stops.
The rebuilt server remained on `0.0.0.0:9876`; repeated live requests with an identity quaternion
measured warm preview medians of 18 ms (IDR) and 21 ms (Backpack). Headless Chromium sent a
230×203 frame with zoom 0.909 after one wheel-up event in a 460×406 pane, then a 460×406 frame
with the same camera after the settle delay. This checks the direction, preview size and final
render sequence through the actual page and socket.

## Full-resolution frame cost and image transport (2026-09-20)

At 560×406, the demand route now reuses packed pages across frames and session clones. Page
storage is reference-counted, so reusing a page does not copy its words; the GPU upload casts
those words to bytes without another allocation. The display route skips the per-pixel pick-ray
conversion, and annotation compositing returns immediately for an unannotated scene. The packed
page cache is bounded to 256 MiB and keyed by dataset, layer, level, channel, and exact chunk
plan. Reopening a session clears both caches. The chunk-cache test checks page reuse through a
clone, unchanged words, and reset on reopen; an adapter test compares the display and pick
attachments. Portable, server, and wasm builds/tests passed.

Alternating warm HTTP requests to the previous and new release binaries used identical cameras
and datasets. For IDR, old/new median wall time was 55.5/35.5 ms (1.57×); for Backpack it was
45.1/35.9 ms (1.25×). Both PNG hashes matched their previous binary. The server's render header
and HTTP wall time differed by about 1.4–1.8 ms on localhost. The release phase probe placed
GPU dispatch at roughly 15–23 ms and planning/pages at 12–17 ms; which dominates varies with
host load. Removing display-only pick-ray conversion saved another 2.9 ms (IDR) or 8.7 ms
(Backpack) in that probe.

The first Python WebSocket benchmark appeared to show 80–100 ms of transfer after rendering,
but that was the client's receive loop: a raw socket received the full 349 KB message within
about 5 ms of renderer completion, and the server's write returned in under 1 ms. DevTools
network event reporting showed similar false delay. Timing `send` to `onmessage` inside
Chromium gave about 3–5 ms beyond the server's render time for full-size IDR and Backpack
frames. A trial of `TCP_NODELAY` did not improve this and was removed.

The current PNGs are about 261 KB (IDR) and 199 KB (Backpack); JSON/base64 expands them to
about 349 KB and 266 KB. PNG encoding in the server probe is only a few milliseconds. Lossless
WebP did shrink these particular images (about 117 KB and 60 KB at method 1), but a standalone
Pillow encoder took about 73 ms and 37 ms, respectively, versus about 21 ms and 13 ms for its
PNG encoder. This is a codec comparison, not a Rust encoder measurement. On the measured local
path, the extra encode cost outweighs the few milliseconds available in transport. The server
continues to send lossless PNG so image output remains bit-for-bit identical.

## Zoomed-in 3D frames (2026-09-20)

The previous full-size timing hid a close-camera cost. At 560×406 and zoom 0.5, the warm
server render took about 147 ms on IDR and 112 ms on Backpack, versus about 34 ms at zoom 1.
The close-camera level chooser started at level 0, then the demand loop dispatched a full-size
placeholder frame and found it needed more than four pages; level 1 did the same. The fitting
level 2 then needed a placeholder and final dispatch. The trace showed IDR's rejected levels
needed 17 and 5 pages (first channel), and Backpack's needed 123 and 16 pages at zoom 0.5.

For close views whose whole selected level cannot fit, the server now probes every eighth
pixel using rays taken from the actual full-size ray table. If those rays alone need more than
four pages, the full frame necessarily fails the same page bound, so the route coarsens before
the expensive full-size feedback pass. A probe that fits leaves the normal full feedback loop
in charge, preserving partial residency. When the whole selected level fits, the route plans
it up front and renders it once even at close zoom. The latter removes the placeholder pass
at the fitting level. Both changes preserve the renderer and its full-resolution image.

Alternating old/new release servers on the same GPU, with identical 560×406 cameras, measured
warm render medians at zoom 0.5 of 169.7→129.7 ms (IDR) and 126.7→82.3 ms (Backpack); at
zoom 0.25, 168.0→123.0 ms and 157.4→85.8 ms. GPU load varied during the comparison. The PNG
SHA-256 matched between builds at zoom 1, 0.75, 0.5 and 0.25 for both stores. The adapter
parity test also compares the one-pass close path against feedback at zoom 0.5 and 0.25.
HTTP wall time exceeded renderer time by about 2 ms in these local measurements, so the zoom
slowdown came from render passes rather than image transfer.

## Linked 3D center (2026-09-20)

The 3D orbit target now follows the same continuous level-0 voxel focus as the orthogonal
panes. The UI divides XYZ focus by the reference shape and sends `focusXyz` with each socket
volume request and browser scene plan. The camera maps this fraction through physical spacing
and ZYX axis order before applying quaternion orientation and zoom. Slice slider moves and
continuous 2D pans update the 3D frame; the latter use the existing half-size interaction
preview and issue a full-size frame after the drag settles. Omitting `focusXyz` preserves the
previous volume-centered view for other API callers.

## Editable annotations (2026-09-21)

Ported the QuPath annotation model and GeoJSON reader/writer from
`omezarr_viewers-rs`. The XY pane has point, rectangle, ellipse, polygon,
polyline, freehand region and freehand line tools, plus selection, body and
vertex editing, class/type/color/width controls, nesting, undo, class filtering,
layer visibility and explicit Save. Shapes use level-0 pixel coordinates and
their Z/T ranges. The same layers project into server and browser 3D frames;
the browser composites them against the paired ray-distance attachment.

The server discovers native `annotations/*/annotations.geojson`, saves QuPath
GeoJSON with the ngio group metadata, and imports existing ROI tables in CSV,
JSON, Parquet or AnnData v1. The ROI table exporter writes physical-coordinate
CSV boxes and reports when a non-box geometry was reduced to its bounds. A
native GeoJSON save preserves full geometry and metadata. The server keeps
edits in memory until Save; restarting discards unsaved edits.

Evidence: scene model/GeoJSON tests 24; server 29 passed, 2 adapter-only
ignored, including annotation routes, 3D projection, visibility, GeoJSON
round trip, ROI physical spacing and AnnData categorical import; UI 12 passed;
wasm target check passed. Release server and Trunk page built; the service was
restarted on `0.0.0.0:9876`. Live HTTP returned the page and annotation routes,
and headless Chrome mounted the annotation controls.

### Annotation compatibility follow-up

The source viewer's remaining annotation behavior was compared against the
port. Layers can now be removed from the session without deleting saved files;
the shape list follows parent/child order. Each layer keeps its own drawing
defaults and view controls while switching: class, type, line width, fill,
class filter, true-size point radius (including per-class radii), stable class
colors, opacity, point size, and Z slab. The XY overlay strokes a cell's nucleus,
uses world-pixel scribble width, and fades shapes outside their Z range according
to the slab. A QuPath Z span appears in 3D at both ends with depth connectors.

The ROI reader recognizes ngio's older `experimental_*_v1` names, AnnData's
`anndata_v1`, `masking_roi_table`, and the source viewer's recorded pixel and
time scale. ROI CSV now writes `len_z_micrometer` and `len_t_second` as the
number of *further* planes/frames, matching the source format. Save accepts
`annotations/<name>` or `tables/<name>` within the configured dataset, reports
how many shapes the latter reduced to boxes, and remembers the target.

3D projection now fits one camera basis per frame rather than reopening the
Zarr for each vertex. A point behind the camera is skipped without failing the
whole volume frame. The fitted projection matched Palace's file-opening
projection across the default, orbit, zoom, and shifted-focus cameras in the
fixture. The affected scene, portable, server and UI suites passed; wasm check
and both release builds passed. The live server was restarted on
`0.0.0.0:9876`. A live demo round trip created a point, saved it as GeoJSON and
as an ROI table, read both routes, and removed the test data. Headless Chrome
mounted the controls and preserved distinct opacity values when switching
between two temporary annotation layers.

The first visual smoke check exposed a Leptos SVG attribute mistake: `attr:d`
and `attr:viewBox` appeared literally in the DOM, leaving paths invisible.
The overlay now uses SVG attribute names directly. A rebuilt release page in
headless Chromium showed a temporary cell and its nucleus as two paths with
real `d`, `stroke`, and `viewBox` attributes; the temporary layer was removed.
