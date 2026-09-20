# Portable renderer handover

## 2026-09-19 update — read this first

Every item on the previous "Immediate next action" list is done; the detailed per-item notes
are kept below it. What a fresh session needs to know:

**Three defects in the Vulkan path were found and fixed today, all by finally comparing the
portable DVR against Palace's own raycaster** (STAGE0 "The empty first-opacity surface: root
cause and fix" and "The Vulkan comparison, and what it found"):

1. The paired first-opacity attachment was `+infinity` for every real dataset: `raycaster.glsl`
   recorded `t` after `update_state` had replaced it with the NaN `T_DONE` sentinel.
2. It was then entry-relative: the march starts at the box entry. The entry/exit pass now writes
   each entry's camera distance into the record's fourth component and the raycaster adds it.
   The attachment is a distance **from the camera**, as `newvolim-render` always said.
3. The Vulkan DVR composited exactly one sample per ray: `ray_distance == INF && update_state(…)`
   short-circuited compositing after the first contribution. Every server frame since the
   attachment was added had been a first-hit rendering.

**And one in the desktop:** Palace fits its camera in *physical* units, but both portable routes
treated the ray as voxel coordinates and applied the layer scale again. On the anisotropic
fixture that put the eye inside the box. `demand_world_ray_from_camera` adds only the
translation; `portable_camera_rays_xyz` divides by the level-zero spacing. Level selection was
re-derived on the corrected camera. The legacy pick bridge had been right all along.

**Pick and display now share one frame per route** (`direct_route_frame`, `scene_route_frame`),
and the webview's volume canvas is wired to the scene route, so the demand-driven frame is what
users see and click against.

**Later on 2026-09-19 (TODO2 items):** transfer-function control (session, desktop, page,
server); the portable host extracted into `crates/newvolim-portable` and the server rendering
volume frames through `scene_route_frame`; multi-layer scenes (`add_portable_image_layer`,
per-layer datasets and levels in the demand route); and the browser rendering the whole scene
itself from a server-packed `/portable/scene` packet with the desktop's shader. STAGE0 entries
from "Transfer-function control" onward.

**Latest (2026-09-19, evening): real compressed stores render.** Chunk reads go through
`zarrs` (`newvolim_io::read_array_region`), so blosc/zstd/gzip/crc32c, Zarr v2 with either
separator, NGFF 0.5 `attributes.ome` roots and nested array paths all work; two public images
referenced by `omezarr_viewers-rs` (IDR `6001240.zarr`, ome-zarr-scivis `backpack.ome.zarr`)
are mirrored under `/big/henriksson/omezarr-public/` and render through `portable-demand` in
under half a second at 256×192 (release). STAGE0 "Compressed and NGFF 0.5 stores read through
`zarrs`". The frame API is camelCase (`orbitX`, `orbitY`); snake_case keys are silently
ignored. Remote (`http`/`s3`) opening is still not built.

**2026-09-20: the web interface is rebuilt** in the style of `omezarr_viewers-rs` (Leptos,
Rust; `crates/newvolim-ui/src/{api,cube,app}.rs`, `scene-webgpu.js`, `style.css`): dataset
browser, 2×2 slice grid with crosshair and orientation box, 3D pane (server frames or the
browser's own WebGPU render), layer cards with channel controls, `?dataset=` deep links. It
talks only to `newvolim-server`. On the way three backend defects were fixed: orthogonal
frames now come from the portable session (CPU slices at the finest level that fits the
pages) instead of Vulkan; the CPU slicer's XZ/YZ planes were transposed and truncated; and the
demand route now coarsens its level instead of refusing panes larger than ~256 px. STAGE0
"The web interface, rebuilt". Build the page with `trunk build --release` in
`crates/newvolim-ui` (no `wasm-opt` offline; `data-wasm-opt="0"`). Verify in a browser with
the other repo's CDP driver (`scratchpad/shoot.py` pattern), not with `--virtual-time-budget`.

**See-through depth:** `LocalSession::depth_scale` multiplies the opacity reference of every
route; `GET/POST /v1/datasets/{d}/settings {depthScale}`; the page's "Scene → Depth" slider
(STAGE0 "See-through depth").

**Slice panes** pan by drag and zoom by wheel about the cursor; the crosshair is the pane's centre
(`focus`, continuous; `crosshair` its floor) and is not drawn (STAGE0 "Slice panes are 2-D
cameras"). Depth moves via the axis sliders, the orientation box or another pane's pan.

**Camera:** `orbit_delta` is `[dx, dy]` of a screen drag and `camera_for_volume` (palace-frame)
is a turntable — `dx` spins about the volume's vertical axis at 0.01 rad/px, `dy` tilts,
clamped to ±89°. It replaced Palace's additive `pan_around` nudge, which also read the drag in
`(y, x)` order (STAGE0 "Orbit axes were swapped", "A turntable camera"); pinned by
`horizontal_orbit_yaws_and_vertical_orbit_pitches`.

**Client-side residency (2026-09-20):** the page's "WebGPU" renderer runs the demand loop itself
(`crates/newvolim-residency`, pure; `palace-core` now builds for wasm32; the scene dispatch
code lives in `palace_core::gpu`) against `GET …/portable/plan`, `GET …/portable/rays` and
`POST …/portable/chunks`, fetching only missed chunks and caching them across frames. Pinned
by a CPU oracle (client dispatch == server dispatch, word for word) and an adapter parity test
(client loop frame == server demand frame). The browser leg itself is unverified on this host
(headless Chrome has no WebGPU adapter); it falls back to server frames with a notice. STAGE0
"Client-side residency". Next there: rays generated in the page; reuse GPU buffers per pass.

**Both the demand route and the browser packet coarsen their level** until it fits the four
pages (`coarsen_levels`, `full_level_scene_inputs_fitting`); a pane-sized browser frame is
tens of MB of packet per camera move (STAGE0 "The browser packet coarsens its level too").

**Single port:** `newvolim-server --page-dir crates/newvolim-ui/dist` serves the page beside
the API; the page defaults its server URL to its own origin. No Python anywhere in the
deployment. STAGE0 "One port: the server serves the page".

**The comparison now runs as a pinned test**, `compare_desktop_portable_and_server_renderers`
(desktop, `--ignored`): matched transfer, unshaded, level zero on both sides. Identical hit set;
depth agrees at every pixel to a constant `−0.224` explained by Palace skipping its on-face
sample; opacity within 3%; Palace's 8-bit truncating state biases its hue, the portable pass
returns the transfer's hue exactly. The three picker tests that used to accept `None` now place
annotations behind and in front of each route's surface.

**§9.3.1 item 5 is complete:** `PortableAffineResampleLayout` is the transform-compatible
resample with explicit rounding, affine and border, held to Vulkan's `resample_transform`.

### Immediate next action

Nothing is blocked. Ordered by value:

0. **The Tauri desktop must host the server.** The page invokes no Tauri command any more
   (`webview_talks_to_the_server_routes_and_invokes_no_desktop_command`), so the desktop
   webview shows the new page but has no backend. Run `newvolim-server`'s router in-process
   in `newvolim-desktop` (bind `127.0.0.1:0`, point the webview at it, or serve through a
   custom protocol) and delete the page-side Tauri commands that then have no caller. Until
   then use the browser against `newvolim-server --page-dir`.
0b. **Remote stores** (TODO2 item 1). The read path is `zarrs` now, so `zarrs_opendal` or
   `zarrs_object_store` behind a feature gives `http(s)://` and `s3://` without touching the
   session; the metadata readers for remote and S3 already exist in `newvolim-io`. Until then
   the mirror script in the session scratchpad (`mirror.py <store-url> <dir>`) is how a public
   store gets onto disk.

1. ~~**Decide the pick-versus-display surface.**~~ **Done.** Each route now has one function
   that decides its frame (`direct_route_frame`, `scene_route_frame`) and both the render
   command and the pick command consume it, so the occluding surface is the one on screen by
   construction. Along the way: the UI (`index.html`) displays and picks through the **direct**
   route — the demand-driven scene command is not wired into the UI, contrary to what this
   file said; Palace's page DVR admits no fitted-camera packet on the fixture, so the direct
   route is the native recorder's; and that route's transported PFM carried voxel-unit
   distances (`260.7` for a physical `75.5`), now converted per ray. STAGE0 "Pick versus
   display: one frame decision per route".

   **And the UI is wired to it:** the volume canvas renders through
   `render_native_portable_scene_camera_draw` and picks through
   `pick_native_portable_scene_annotation` (`NEWVOLIM_NATIVE_VOLUME_COMMAND` /
   `…_PICK_COMMAND` in `index.html`; `dist/` rebuilt with `trunk build`). Pinned by
   `webview_volume_canvas_is_wired_to_the_scene_route` and
   `scene_render_command_payload_is_the_webview_contract_over_the_route_frame`. The page's
   chunk-region fields are now inert for the volume (only the scene route's fallbacks read
   them); removing them from the page is cleanup, not a contract change. STAGE0 "The webview
   now shows the demand-driven frame".
2. ~~**Decide the voxel-centre convention.**~~ **Done: voxel-centred everywhere.**
   `layer_world_box` is the one place the desktop builds a layer box (`origin − 0.5` to
   `origin + dims − 0.5` voxels); `newvolim-render`'s ray interval and both native shaders match.
   An annotation at voxel `i` is now drawn where voxel `i` is painted; the depth offset against
   Vulkan dropped from `−0.224` to `−0.076` (the remaining part is Palace's own corner-box entry
   face). Invariant test plus four re-derived pins, mutation-checked. STAGE0 "The voxel-centre
   convention, stated once".
3. ~~**Scheduler selection of the affine resample.**~~ **Done.** `select_resample_transform`
   (with `portable_resample_transform_admits` and `portable_resample_affine_dynamic`) takes the
   portable affine path for an admitted lossless page and keeps Vulkan otherwise; `py-palace`'s
   `resample_transform` goes through it, as `rechunk` already did. The LOD builder stays on
   Vulkan on purpose (generic element type, volumes past one page). Identity-observable test,
   spike runtime test, mutation-checked. STAGE0 "Scheduler selection of the affine resample".
4. ~~**Palace's 8-bit state truncation.**~~ **Done.** `from_uniform` rounds. Palace's mean
   alpha now equals the portable pass's (207.7 vs 207.3) and its green bias against the
   transfer's hue is +0.47 levels (was −11.9), asserted in the comparison and mutation-checked.
   STAGE0 "Palace rounds its 8-bit state".

**Everything on this list is done.** What remains is not a defect list:

- Commit: the outer repo's changes and `palace-dev`'s (raycaster, entry/exit pass, colour,
  affine resample, selector) are uncommitted in their two repositories.
- Cleanup: the page's chunk-region plumbing (`newvolimNativePortableRegion`) is inert for the
  volume now that the scene route is wired; remove it when convenient.
- Speed: the desktop adapter suite re-renders the demand frame per pick; ~15 s today, but the
  pick tests could cache the route frame if it grows.
- Deferred acceptance gates unchanged: browser, Apple and Windows/D3D12 adapters cannot be run
  from this host.

Not worth doing: multi-layer demand (the session cannot hold more than one image layer).

### Evidence commands (all `--test-threads=1`)

```text
cargo test --offline -- --test-threads=1
cargo test --offline --manifest-path crates/newvolim-desktop/Cargo.toml -- --ignored --test-threads=1
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-core -- --test-threads=1
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-frame -- --test-threads=1
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-wgpu -- --include-ignored --test-threads=1
cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-wgpu-spike -- --include-ignored --test-threads=1
```

Builds: the `stable` toolchain moved to rustc 1.98.1 on 2026-09-19, invalidating the caches in
`target/` (102 GB) and `palace-dev/target/` (31 GB); `.cargo/config.toml` sets
`CMAKE_POLICY_VERSION_MINIMUM=3.5` so `shaderc-sys` builds from source on current CMake; with
`/data` nearly full, this session built into `CARGO_TARGET_DIR=/big/henriksson/cargo-target/newvolim`.

The worktree is intentionally dirty and `palace-dev/` is its own repository (ignored here);
do not commit unless asked.

## 2026-09-18 continuation update

**Milestone complete: the Palace WGPU ordered-scene DVR pass has local-adapter parity with the
CPU oracle.** The per-frame CPU comparison guard has been removed. `STAGE0.md` carries the full
evidence block; the short version follows.

The blocking divergence was not the metadata/LUT layout. `SCENE_DVR_SHADER` now writes a bounded
24+4-word pixel-zero trace of its own decoded state, so a data-handoff fault is nameable instead
of appearing as a transparent frame. Reading that trace showed every decode step correct while
the composite was `NaN`: `pow(0.0, 1.0)` returns `NaN` on this driver, and a fully opaque
transfer entry makes the Beer-Lambert base exactly zero, where SPIR-V/GLSL leave `pow`
undefined but Rust's `powf` returns `0.0`. `step_corrected_alpha` now returns `1.0` for a zero
base — exact parity, not a tolerance. The same latent expression in `record_dvr_frame` and
`record_dvr_page_frame` is fixed identically; their fixtures only ever used alpha `128`, which
is why it stayed hidden.

What changed:

- `WgpuOperatorRecorder::record_dvr_scene_frame` returns the recorded frame directly.
  `record_dvr_scene_frame_traced` additionally returns the shader trace; `scene_divergence_report`
  is public so an unvalidated adapter (the deferred Apple/Windows gates) can name a failing
  decode step rather than reporting only a wrong frame.
- Two new opaque-entry regressions in `palace-wgpu-spike` cover `record_dvr_frame` and
  `record_dvr_page_frame`. Both were confirmed to fail without the fix and pass with it.
- The desktop annotation-free scene route no longer renders every frame twice: the CPU oracle is
  evaluated only when adapter acquisition or recording fails
  (`palace_portable_scene_camera_draw_on_adapter`).

Do not re-add a per-frame CPU comparison. The desktop no-adapter/error fallback is the intended
safety boundary.

## Scene picking now uses Palace depth (2026-09-18)

`portable_scene_pick_depth` in `crates/newvolim-desktop/src/main.rs` reads the occluding distance
from Palace's ordered-scene attachment, falling back to the native renderer only for
Palace-inadmissible packets or a host without an eligible adapter.

Be precise about why this matters, because it is easy to get wrong: **both** renderers already
keep depth volume-only — the native shaders colour-composite a covered annotation but never write
it into `ray_distance` — so an annotation could not occlude itself either way. What actually
differs is the first-opacity rule. The native pass records `travelled` at the step where
*accumulated* opacity first reaches `0.01`; Palace records the step-centre distance of the first
sample with *any strictly positive* opacity, which is the oracle-defined, parity-tested rule.
Routing the picker therefore moves the occlusion boundary, it does not just relocate the
computation.

This also fixed the reason the Palace scene route was almost never actually taken: the desktop
adapter rejected the whole packet when *any* ray missed the admitted scene AABB, and a framed
camera always has such pixels. A missed ray is now admitted as a degenerate `near == far`
interval — already valid for `PortableRayInterval`, carrying no samples, and rendering as a
transparent pixel with `+infinity` depth on both the oracle and the shader. Do not reintroduce
whole-frame rejection for a missed ray.

`portable_scene_pick_depth_is_palace_volume_depth_and_occludes_behind_annotations` pins this
against the committed fixture: it asserts the packet contains both an opacified and a missed
pixel, that the picked depth equals the Palace attachment word exactly, and that an annotation at
half the first-opacity distance is selected while one at one-and-a-half times it stays occluded.
The older `native_portable_scene_picker_uses_ordered_scene_renderer_depth` smoke is **not**
routing evidence — its assertion passes either way.

## Palace owns annotation compositing (2026-09-18) — §9.3.1 item 3 complete

Palace now owns **both** passes of an ordered scene frame. `PortableAnnotationCompositeInput` in
`palace-core::gpu` carries a rendered frame plus up to 4 096 ordered projected primitives, with
`composite_cpu()` as oracle; `WgpuOperatorRecorder::record_annotation_composite` is the matching
compute pass, reachable through the recorder trait. `render_native_portable_scene_camera_draw` no
longer branches on `annotation_words.is_empty()` — an empty packet is simply a no-op composite.

Three semantics are deliberate; two differ from the viewer's older pass, so do not "restore" them:

1. The first-opacity attachment is **returned unchanged** — it stays the volume's surface, which
   picking tests against. An annotation that wrote depth would occlude itself.
2. The **nearest** covering primitive wins, declaration order breaking an exact tie. The native
   pass let the *last* primitive in the packet win regardless of depth.
3. Colour is written as the record's **sRGB bytes directly**. The scene transfer LUT carries
   encoded sRGB, so the attachment is in that space; the native shaders linearize only because
   they target a linear RGBA16Float surface presented through sRGB.

Coverage and distance interpolation are the *meaning of the record*, not policy, and are kept
literally identical between oracle and shader — including the `1e-6` degenerate-triangle guard.

The desktop converts via the typed record, not the word stream:
`project_session_annotation_records` is the single projection feeding both
`project_session_annotation_words` and `palace_annotation_primitives`.

## §9.3.1 item 4: the desktop now renders from renderer-driven demand (2026-09-18)

`render_demand_driven_scene_camera_draw` renders an ordered scene frame with **no caller-supplied
chunk region**: the scene pass records the chunks it misses, the host plans, reads and uploads
exactly those, and the frame completes when it reports no misses.
`render_native_portable_scene_camera_draw` tries it first and falls back to the region-based routes
otherwise.

`demand_driven_scene_ignores_the_webview_chunk_region` is the evidence: three requests with
different `origin_xyz`/`extent_xyz` produce byte-identical frames, the frame actually contains
volume, and depth still pairs with colour.

The machinery underneath, all separately tested: `PortableChunkGrid` / `PortableChunkPlan`
(deterministic, sorted, `ExceedsPortableBound` rather than truncation), `PortablePageTable` and the
request table sharing one probing rule, `PortableChunkedChannel` addressing with clipped edge
strides, `PortableResidencyLoop` (lossy-but-monotone convergence, preview on exhaustion,
`Desynchronized` surfaced), `new_demand_resident` for the bootstrap frame, and
`select_portable_level` mirroring `sliceviewer::select_level`.

### Restrictions — real, not claimed away

1. ~~**One layer.**~~ **Lifted 2026-09-19.** `add_portable_image_layer(root)` binds a further
   OME-Zarr to its own layer and the demand route renders every visible image layer at its own
   level (STAGE0 "Multi-layer scenes"). The four-page budget across all enabled channels is the
   remaining bound.
2. ~~No per-frame level switching.~~ **Done.** `demand_scene_level` measures the footprint between
   two neighbouring centre pixels and feeds `select_portable_level`; the frame renders at the
   chosen level.

   The stated blocker — "needs re-admission without scene mutation" — was the wrong framing.
   **Nothing about a level is session state:** the source array and its transform are pure
   functions of the metadata and the level index, so `local_layer_render_requests_at_level`,
   `local_layer_chunk_plan_for_chunks_at_level` and `portable_level_transform` read a level without
   the session changing.

   **Two traps, both about where a measurement is valid.** A footprint measured at the eye is zero
   (neighbouring rays are coincident there), so it must be taken where the centre ray *enters* the
   volume — also the conservative end, since the footprint only grows with distance. And two rays
   clipped independently to the box have *different* near distances, so measuring both at one
   distance is unrepresentable: `demand_world_ray` returns an **unclipped** ray and clipping is the
   caller's step. Palace returns its ray in ZYX **physical** coordinates — *not* voxel
   coordinates, as this note first said and the code first assumed; see the 2026-09-19 update
   — so the axis swap and the translation-only mapping to the layer's world live once in that
   helper. An earlier attempt compared a ZYX ray against an XYZ box.
3. ~~An adapter per demand iteration.~~ **Fixed, twice.** The device is now owned by
   `LocalSession::portable_device` behind an `Arc<OnceLock<…>>`; five acquisition sites take a
   `&LocalSession`. A failed acquisition is cached as a failure rather than retried.

   **Do not move this back to a process-wide static.** The first version did, and a static is
   never dropped, so the device leaked and the graphics driver's `[vkps] Update` thread outlived
   process teardown: device-using tests segfaulted at exit 2–4 times in 10, while a test touching
   no device never did. A twenty-line probe confirmed it — drop a device before exit, 0/20 crashes;
   `mem::forget` it, 8/20. Session scope keeps the sharing that matters (every pass of a converging
   frame) and guarantees destruction before exit; the previously-crashing filters now run 0/15.

   Two routes were also rendering everything twice — the direct page-DVR and orthogonal-slice
   routes evaluated their CPU oracle *before* trying the GPU and discarded it on success; both now
   use it only as a fallback.

### The 2026-09-19 list, as completed

The previous four-item list is complete; what each found is kept below under "Completed this run".
These were the priorities for the day, highest value first, each now done:

1. ~~**Fix the empty first-opacity surface.**~~ **Done, root-caused, mutation-checked.** In
   `raycaster.glsl` the main sampling branch recorded `ray_distance = t` *after*
   `update_state(inout float t, ...)`, which sets `t = T_DONE` — a NaN bit pattern — when the
   sample saturates the ray. A saturating transfer (every real dataset through `red_ramp(0,1)`)
   saturates on the first contributing sample, so the recorded distance was NaN and the write
   guarded it to `+infinity`; the ball only passed because its samples never saturate first. The
   fix captures `sample_t` before the call, as the constant-chunk branch already did. Twelve GLSL
   lines; nothing else in Palace changed. Regression guards:
   `local_zarr_render_carries_a_real_first_opacity_surface` (palace-frame, un-ignored) and the
   server's `fixture_volume_response_carries_the_paired_palace_depth_attachment`, which now
   asserts finite distances instead of a PFM header. Both fail with the fix reverted. Full
   STAGE0 entry "The empty first-opacity surface: root cause and fix". The earlier eliminations
   remain valid and are kept there for their method.

2. ~~**Compare portable DVR against Palace's own Vulkan raycaster.**~~ **Done, and it found
   three defects — none in the portable compositor.** Full account in STAGE0 "The Vulkan
   comparison, and what it found". In short:
   - *Vulkan attachment was entry-relative.* The entry/exit pass now writes each entry's
     camera distance into the record's fourth component and the raycaster adds it; two
     slab-oracle guards in `palace-frame`.
   - *The desktop treated Palace's physical camera ray as voxel coordinates* (both portable
     routes; the legacy pick bridge was right). `demand_world_ray_from_camera` now adds only the
     layer translation; `portable_camera_rays_xyz` divides by the level-zero spacing. Level
     selection was re-derived on the corrected camera. Two pinned CPU guards.
   - *The Vulkan DVR composited one sample per ray* — the `&&` guard short-circuited
     `update_state` after the first contribution. Every server frame since the attachment was
     added was a first-hit rendering. Fixed at both sites.
   The comparison is `compare_desktop_portable_and_server_renderers` (desktop, `--ignored`):
   matched transfer, no shading, level zero on both sides via the new
   `palace_frame::CameraRenderOptions` and `render_demand_driven_scene_camera_draw_at_level`.
   Result: identical hit set; depth agrees at every pixel to a constant `−0.224` (Palace skips
   its on-face sample; bounded by one step); mean alpha 202 vs 208; Palace's 8-bit truncating
   state biases its hue, the portable pass returns the transfer's hue exactly. Asserted, with
   five mutation checks.

   **Also done:** the three picker tests that had accepted `None` now place an annotation
   behind their route's first-opacity surface (must be occluded) and one in front (must be
   found at its distance), mutation-checked by ignoring depth — STAGE0 "The picker tests now
   pick something".

   **Open, in priority order:**
   - The two portable pick commands read depth from a region-bounded static packet (chunk
     `origin_xyz`/`extent_xyz`) while the desktop displays the demand-driven scene over the
     whole level, so a pick can disagree with the screen wherever the displayed frame has volume
     the packet does not. Pre-existing; decide whether picking should run against the demand
     route's own frame.
   - The desktop's layer box is corner-based (`[translation, translation + scale × dims]`) while
     Palace centres voxel `i` at `i × spacing`; the portable grid is therefore shifted half a
     voxel from Palace's. Measured as harmless in the comparison (the hit set is identical and
     the depth offset is fully explained by Palace's skipped face sample), but it is a convention
     to decide, not to leave implicit; it touches picking and annotation placement too.
   - Palace's `from_uniform` truncates its 8-bit progressive state, which loses weak-channel
     colour. Recorded, not fixed — a Vulkan renderer property, outside this migration.

3. ~~**The resample half of §9.3.1 item 5.**~~ **Done.** `PortableAffineResampleLayout`
   (palace-core `gpu`) states the three decisions explicitly — row-major affine `A · [g, 1]` on
   global output coordinates, `floor(p + 0.5)` rounding, `Repeat`/`Pad0` border — with a CPU
   oracle, the `resample-affine-nd` WGPU kernel, `portable_resample_affine_cpu` as the opt-in
   operator bridge, and `portable_affine_from_palace_matrix` for Palace's `[w, c…]` matrices.
   The oracle is held to Vulkan's `resample_transform` on a rescale and a translation under both
   borders; three mutation checks. `resample_transform` and the floor-nearest layout are
   untouched, and no caller is redirected. STAGE0 "Resample, the transform-compatible half".

   **Open from this item:** selecting the portable affine path for admitted pages (the LOD
   builder's `resample` still calls `resample_transform`) is a scheduler decision like the ones
   already made for floor-nearest resample and rechunk, and belongs with that selection logic.

Not worth doing: multi-layer demand. The desktop session cannot hold more than one image layer
(see restriction 1), so it is speculative work against no caller.

### Completed this run

### The 2026-09-18/19 four-item list, as completed

PLAN.md §9.3.1 items 1–4 are done. What follows is ordered by value, not by plan number; each is
independent, so a blocked item should be skipped rather than worked around.

1. ~~Kill the per-pixel dataset reopen.~~ **Done, and it was the biggest defect in this work.**
   `portable_camera_rays_xyz` opened the Zarr once per pixel. Release measurements on the committed
   fixture: 256x192 went from **16.15 s to 1.84 ms**, 64x48 from 1.315 s to 5.5 ms.
   `palace_frame::camera_rays_for_local_zarr` opens once;
   `batched_local_zarr_rays_match_the_per_pixel_path_exactly` pins it ray-for-ray so nothing
   rendered or picked can change.

   **Look for this shape again.** Twice now a per-pixel call has hidden expensive setup — the
   camera refit, then the dataset open — and neither was visible in the test suite, because
   correctness never depended on them. Only a measurement found either.

2. ~~Look at the pictures.~~ **Done, and it found a real rendering bug on its first run.**

   Palace corrected DVR opacity in the **wrong units**. Its own raycaster normalizes the step by
   the volume's physical diagonal before the 256x reference correction
   (`norm_step = step / diag` in `raycaster.glsl`); the portable path fed the *physical* step into
   `step_size * 256`, overstating the exponent by the whole diagonal — 48x on the committed
   fixture — and saturating 87.5% of hit pixels.

   The reference is now an explicit `opacity_reference` on every DVR entry point, with the
   correction `step_size / opacity_reference` and callers passing `diagonal / 256.0`
   (`portable_opacity_reference`). It cannot be folded into `step_size`, which must stay physical
   because it also sets the marching distance.

   **Why the suite could not catch it:** every fixture used `step_size = 1/256`, making the
   exponent `1.0` under any interpretation of the reference — the one value where the bug is
   invisible. `portable_scene_opacity_follows_its_declared_reference_length` now varies the
   reference and pins the exponent arithmetic.

   Remaining portable-vs-native differences are the native compositor's fixed `0.06` blend factor
   and its `0.01` first-opacity threshold, both designated legacy behaviour by PLAN.md. Palace is
   the correct side. `compare_portable_and_native_scene_rendering` is kept as a reporting test.

3. ~~§9.3.1 item 6 — desktop/server selection.~~ **Audited. Routes are consistent with the rule;
   underneath them is a serious pre-existing defect.**

   The server calls only `render_local_zarr_with_camera_attachments` and
   `render_local_zarr_orthogonal_at_png`, so it is permanently on the Vulkan fallback and never
   selects the portable route. The browser keeps independent camera/depth authority by
   construction — the server hands it *chunks*, not frames. Whole workspace passes, server 17/17
   including two real-Palace-render tests.

   **A real dataset renders colour with no depth at all.** On the committed fixture at 32x24,
   both local-Zarr entry points paint 484 pixels and report **0** finite first-opacity distances,
   while the procedural ball through the same reader reports 142. The camera is irrelevant; the
   volume source is what differs. The PNG+PFM contract promises a paired surface and the desktop
   picker's Vulkan fallback depends on it.

   Two tests could have caught it and each missed from a different side: the palace-frame one
   asserts a finite distance but only for the ball; the server one uses a real dataset but only
   checks the PFM *header*, which an all-`+infinity` surface satisfies.
   `local_zarr_render_carries_a_real_first_opacity_surface` states the contract and, since the
   fix, runs un-ignored in the default suite. Pre-existing — nothing in this run touched
   `palace-png`, the raycaster or `render_frame_attachments`.

   **Since fixed** — see item 1 of "Immediate next action". Neither of the two starting points
   suggested here was the cause; it was the `inout t` sentinel in `update_state`.

4. ~~§9.3.1 item 5 — operator migration (rechunk half).~~ **Done.** `PortableRechunkStream`
   splits a rechunk along the **slowest** axis into windows that each fit one page, so the existing
   fixed-binding kernel runs unchanged while the source it can serve grows past the ~1M-voxel cap.
   The slow axis is chosen so both runs stay contiguous; a fast-axis split would be strided and is
   refused rather than silently substituted. Fast-axis padding stays inside each window's memory
   extent; split-axis padding is reported as `padding_words` for the caller to zero-fill.

   **Watch the oracle.** The adapter test first compared against
   `PortableRechunkStream::rechunk_words`, which is derived from the very offsets under test — a
   mutation dropping `output_begin` from `input_offset` failed the core fixtures but *passed* the
   adapter one. The reference is now built from the layout definition inline and is mutation-checked
   against two perturbations.

   `resample_transform` remains deliberately un-redirected and the floor-nearest API explicit; a
   transform-compatible path still needs centre rounding, affine coordinates and border policy
   stated first.


### Consequences recorded, not resolved

Palace is now authoritative for all admissible scene frames on this host — volume blend,
annotation ordering, and which chunks are resident all differ from the native compositor. That is
intended per PLAN.md, but **no side-by-side visual comparison has been done**, and it is worth
doing before calling the route finished.

### Run adapter suites serially

`palace-wgpu-spike -- --include-ignored` passes **24/24 in 7.5 s** with `--test-threads=1`, but
does not finish at the default parallelism — it sat for fourteen minutes at ~100% CPU with no
test completing. Every test also passes individually in under a second, so this is concurrent
WGPU adapter/device acquisition on this host, not a slow test. Always pass `--test-threads=1` for
adapter suites. Diagnosing that deadlock is open work; it predates this milestone.

## Goal

Connect the portable raycaster to normal Palace level/chunk planning and physical camera rays;
support ordered multi-channel/layer DVR composition; use renderer-owned depth for annotations.

## Current state

The worktree is intentionally dirty. Preserve unrelated changes. `palace-dev/` is an untracked
nested Palace working tree whose changes are part of this work.

Implemented foundations:

- `palace-core::gpu` has bounded owner-tagged pages, trusted unit physical rays, paired RGBA plus
  first-opacity attachments, single-volume DVR CPU/WGPU paths, and `PortableDvrSceneFrameInput`.
- `PortableDvrSceneFrameInput` supports at most four globally non-aliasing pages, ordered layers
  and channels, per-channel transfer functions, physical AABB sampling, and a CPU source-over /
  Beer-Lambert oracle.
- `PortableDvrSceneGpuInput` serializes the admitted scene for a fixed-binding shader: pages and
  lengths, layer/channel records, transfer LUTs, rays, and step size.
- `palace-wgpu::WgpuOperatorRecorder::record_dvr_scene_frame` is a real fixed-binding WGPU
  compute pass with local-adapter parity against the core CPU oracle. `record_dvr_scene_frame_traced`
  exposes the shader's pixel-zero decode trace for adapters that have not been validated.
- Desktop direct single-channel camera DVR is routed through Palace page-DVR WGPU when admitted,
  with CPU fallback. Its picker reads Palace paired physical depth.
- Desktop scene admission now has `palace_dvr_scene_from_native_camera` in
  `crates/newvolim-desktop/src/main.rs`. It converts ordered admitted scene layers into the Palace
  scene contract, preserves world rays, physical transforms, global page owners, and transfer
  policy. Annotation-free scene rendering executes the Palace WGPU recorder and falls back to the
  core CPU oracle only when adapter acquisition or recording fails; unsupported/annotation packets
  retain the existing native scene renderer fallback.

## Important limitations

1. The desktop scene route still uses `LocalSession::local_layer_chunk_plan` with a
   webview-supplied chunk region, not normal Palace scheduler/level/chunk planning, and the
   portable route has no camera-driven level selection. This is the open half of §9.3.1 item 4.
2. The scene adapter clips hitting rays to the broad admitted-scene AABB. It is safe and bounded
   but can spend steps in empty space between disjoint layers; a later shader can use per-layer
   bounds. A ray that misses entirely is admitted as a degenerate transparent interval and is not
   a limitation.
3. Scene DVR and annotation-composite parity are proven on this Linux adapter only. Apple Silicon
   and Windows/D3D12 remain deferred acceptance gates; the scene shader trace exists so those runs
   are diagnosable.
4. The portable route has had no side-by-side visual comparison against the native compositor,
   whose volume blend and annotation-ordering rules both differ.

## Completed milestone: Palace WGPU ordered-scene DVR

`WgpuOperatorRecorder::record_dvr_scene_frame` is a real WGPU compute pass driven solely by
`PortableDvrSceneFrameInput::to_gpu_input()`. It binds four static scalar pages, one combined
scene-metadata/transfer-LUT buffer, the trusted ray buffer, the paired RGBA/depth output, a
uniform, and the diagnostic trace. It matches `render_cpu` exactly on:

- nearest physical sampling inside half-open channel AABBs;
- declared channel order then declared layer order using premultiplied source-over;
- Beer-Lambert alpha correction `1 - (1 - alpha)^(step_size * 256)`, including the exactly-zero
  base that `pow` leaves undefined;
- first positive scene-sample opacity as physical ray distance;
- `+infinity` depth for transparent rays;
- stop at 0.95 opacity.

Scene layer/channel records and transfer LUT words share one storage buffer with a two-word
header (`layer_record_word_count`, `transfer_lut_word_offset`). That layout is now parity-proven,
but it remains an implementation choice rather than a frozen contract.

## Planning migration after scene parity

The normal Palace viewers are Vulkan feedback-table pipelines:

- `palace-dev/palace-core/src/operators/sliceviewer/mod.rs`
- `palace-dev/palace-core/src/operators/imageviewer/mod.rs`

Their request/use-table scheduling stage is the actual normal level/chunk planning seam. Do not
claim that replacing `LocalSession::local_layer_chunk_plan` alone completes this requirement.

## Evidence already run

- `cargo check --offline --manifest-path crates/newvolim-desktop/Cargo.toml` passes (only two
  existing unused slice helper warnings).
- `cargo check --offline --manifest-path palace-dev/Cargo.toml -p palace-wgpu -p
  palace-wgpu-spike` passes (one pre-existing unused-import warning in the spike).
- `cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-core --lib gpu::tests --
  --test-threads=1` passes 34/34, and `--lib portable_ -- --test-threads=1` passes 45/45.
- `cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-wgpu --
  --include-ignored --test-threads=1` passes 16/16, including two demand-resident channels sharing
  one residency map, a scene frame that bootstraps from
  nothing resident, level selection driving the demand loop through the scene pass, the GPU-driven
  demand loop,
  demand-resident scene parity against the static-page render, the ordered-scene parity, the mixed
  hit/miss frame, annotation-composite parity across every record kind, page-table lookup parity
  across probe chains, request-emission parity for key set, slot agreement, dedup and lossiness,
  and demand-resident scalar resolution per voxel.
- `cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-wgpu-spike --
  --include-ignored --test-threads=1` passes 24/24 in 7.5 s (it hangs without `--test-threads=1`).
- `cargo test --offline --manifest-path crates/newvolim-desktop/Cargo.toml --
  --include-ignored --test-threads=1` passes 53/53 in 29.0 s, and
  `-p palace-frame -- --test-threads=1` passes 10/10.
- `git diff --check` and `git -C palace-dev diff --check` passed before this handover addition.

Avoid claiming full `palace-core --lib` success: unrelated legacy Vulkan tests fail on this host
due to device initialization/validation failures. Targeted portable tests are the relevant local
evidence.

## Files to read first

- `PLAN.md`, section 9.3.1
- `palace-dev/palace-core/src/gpu.rs`
- `palace-dev/palace-wgpu/src/lib.rs`
- `crates/newvolim-desktop/src/main.rs`
- `crates/newvolim-wgpu-frame/src/main.rs` (`SCENE_SHADER`) as a behavioral reference only; do
  not copy its older ad-hoc blend semantics over the Palace CPU oracle.
