# newvolim — plan

**Status: implementation started.** The Stage-0 evidence log is in `STAGE0.md`; code for the
workspace, OME-NGFF metadata boundary, scene vocabulary, render contract, CSR shell, and Tauri
host is present under `crates/`. Gates that require a representative real dataset, Palace frame
transport, or a portable backend remain open.

This file is written to be self-contained: an LLM or developer starting fresh in this
directory should be able to work from it without having seen the conversation that produced
it. Everything load-bearing is restated here, with pointers to the source material.

Last updated 2026-09-16.

### Current delivery scope and deferred acceptance gates

The active implementation delivery is the **Linux/local-data path**: local OME-Zarr admission
and rendering, Palace frame transport, desktop interaction, local server/browser behaviour,
annotations, performance instrumentation, and portable-backend code and tests that can be
verified on this host. Work on any of those remains actionable even when another environment is
unavailable.

The following remain important acceptance gates for a later release, but are deliberately **not
blockers** for this delivery:

- an Apple-Silicon adapter run;
- a Windows/D3D12 adapter run;
- live authenticated S3 or SSH-agent integration against provisioned infrastructure; and
- production-scale dataset, throughput, residency, and deployment validation.

Code and tests must continue to preserve portable semantics for those gates. The evidence log
must name them as deferred rather than using an unavailable machine, credential, or production
dataset as a reason to stop local implementation work.

### Current implementation checkpoint

`STAGE0.md` is the authoritative command-by-command evidence log. The current local delivery is
not merely scaffolded: it has a tested OME-NGFF/scene/source-policy foundation, a Linux desktop
viewer smoke against the committed anisotropic fixture, browser and WebSocket frame admission,
linked orthogonal views, point/polygon annotation persistence and 2D overlay, a bounded local
frame service, and a native WGPU colour plus first-opacity-depth proof with cold/warm timing
artifacts. The attachment transport is carried end-to-end as colour PNG plus an optional PFM
sidecar. Palace now produces its renderer-owned first-opacity sidecar, which Tauri and the CSR
preserve and validate through the local desktop, server, and browser paths.

The remaining **local implementation** work is consequently focused rather than blocked:

| Area | Current verified state | Remaining implementation work |
|---|---|---|
| Palace depth | Layer boxes are voxel-centred everywhere (`layer_world_box`; voxel `i` at `translation + i × scale`, half a voxel either side), so annotations, sampling and picking share one grid. Raycaster-owned paired `f32` first-opacity surface, validated PFM readback, and end-to-end transport; native and browser WGPU also produce real distances; native desktop selection reconstructs the exact Palace camera ray and clips annotation picks against a host-read PFM sample. Palace Frame also projects raw camera-space points back to physical frame pixels/ray distance with a ray round-trip test. The native world-ray scene pass consumes the same stable 13-word point/line/triangle records and depth-composites them against its own first-opacity attachment; local adapter evidence proves a front annotation remains visible while a behind annotation remains occluded. The trusted desktop scene-camera admission/render command now carries that exact route without accepting `state_ray`, alpha, or a second render as authority. | Preserve the paired-depth ownership rule when the Palace raycaster is moved onto the portable backend. |
| Palace portable backend | Linux static-page representation, shader/tooling probes, a native WGPU point/segment/triangle depth-compositing proof, backend-neutral synchronization, and a fixed 13-word projected-annotation record (sRGB, projected radius, per-vertex physical ray distances, and reversible nonzero `scene_id + 1` wire ID) emitted by `newvolim-render`. The native world-ray scene pass consumes typed ordered layers, physical rays, finite intervals, a fixed layer/channel table, the shared static-page pool, and those depth-tested annotation records. Desktop now constructs and renders the matching trusted scene-camera packet. `palace-core` owns bounded non-aliasing requests plus owner-tagged 4 MiB portable tensor pages and authoritative 1–3D resample/chunk-extraction layouts; both bounded scheduler bridges assemble one or more complete logical CPU chunks within that bound, skip source-edge allocation padding, and emit ordinary output chunks with explicit edge zero padding. Resample's fixed 16-word uniform now carries global output dimensions plus chunk origin/logical/memory extents, so CPU and WGPU cover chunked output too. `palace-wgpu` records both layouts through fixed uniforms; local adapter tests cover direct extraction, padded edge memory, and recorder-selected normal rechunk/resample chunks. Bounded lossless scalar (`u16`/`u32`) tensor resampling and rechunking select the installed recorder, otherwise falling back to CPU; dynamic bridges cover both contracts, while Python selects portable rechunk for admitted pages and retains Vulkan for larger requests. The resample bridge intentionally remains distinct from the arbitrary-transform Vulkan operator because its floor-nearest coordinate rule has different semantics. `RunTimeBuilder` can install that recorder, and `OpaqueTaskContext` lends it to selected tasks without removing the Vulkan map. A paired core `PortableFrameAttachments` contract now reaches the desktop transport directly from native WGPU, while the legacy Palace raycaster converts its validated attachment to that same contract before encoding. | Migrate the general production resample closure and multi-page rechunk planning from concrete Vulkan storage requests to portable page input/output, then connect raycaster and sliceviewer. |
| Remote/server/desktop | Allow-listed local service, router integration, bounded WebSocket and Tauri envelopes, browser correlation, PFM validation, and bounded physical-distance diagnostics. The local desktop smoke now opens the committed anisotropic fixture and verifies genuine default/camera Palace colour+PFM attachment pairs and all three orthogonal panes. | Retain live deployment/load validation as explicitly deferred acceptance work; no local smoke gap remains. |
| Annotations | Physical NGFF point/polygon/rectangle/ellipse persistence, bounded drafting, 2D overlays, native depth-correct 3D selection through the camera-ray/NGFF bridge, and a projection-validated portable GPU record stream with a WGPU visible-versus-occluded point/line/polygon proof. The browser WGPU direct-preview pass owns its perspective camera and admits projected records only when their supplied camera/extent seal exactly matches it; it rejects desktop/Palace pixels as a mixed authority. Native direct and multi-layer scene routes construct their own trusted camera packets and retain host-owned picking. | No separate browser-camera work remains; retain the distinct browser and host picking authority as the routes evolve. |
| Layers/channels | Backend-neutral layers retain transform, visibility, per-channel sRGB colour/window/opacity, and sparse label palettes. Desktop admission assembles all selected authorized layers/channels into one shared four-page scene packet, retaining physical transforms, voxel origins, local dimensions, and absolute channel ranges. The native world-ray compositor resolves that page pool, blends layers in declared order, and receives trusted desktop world rays. The independent browser direct-preview loader now admits one to four explicitly requested spatial chunks, maps each to its own static-page layer with normalized extent/origin transform, and retains the source's full camera domain. | Keep the four-page bound while evolving source planning; browser and desktop remain distinct camera/depth authorities. |

This table does not promote a deferred Apple/Windows/live-S3/SSH/production-scale gate to a
blocker, and it must be revised only with evidence that changes these facts.

---

## 1. What we are building

A **3D/2D visualizer for TB-scale bioimaging data** (OME-Zarr, light-sheet/confocal), built
as an application on top of [`palace`](https://github.com/ftilde/palace) used as a library.

Two separable efforts:

| | Where | Upstreamable? |
|---|---|---|
| **A. Changes to palace** — a wgpu backend alongside the existing Vulkan one, a remote/pluggable data-source path, wasm viability | a fork of palace | **Yes — write them to be PR-able.** |
| **B. newvolim** — the visualizer: GUI, metadata/annotation rendering, session/layer model, client-or-server rendering | this directory | No. Application-specific. |

The split matters. palace is someone else's project with a published paper; changes that are
generally useful (a second GPU backend, a source plugin path) should be clean, feature-gated
and offered upstream. Everything domain-specific (OME-NGFF layers, GeoJSON annotations, ROI
tables, our GUI) stays in newvolim.

### Requirements

- **TB-scale out-of-core rendering.** GPU VRAM far smaller than the dataset. This is palace's
  core competence and the reason to build on it.
- **Anisotropic voxels are the norm**, ~10:1 in Z. Nothing may assume cubic voxels, cubic
  bricks, or a scalar LOD level. This invalidates assumptions in most reference code.
- **Client *and* server rendering, chosen by deployment.** Our servers often have **no GPU**;
  clients almost always have at least a limited one. The same renderer must run in a browser
  (WebAssembly + WebGPU), natively on a desktop, and headless on a server.
- **Cross-platform**: Linux, macOS, Windows.
- **GUI: Leptos**, and therefore **Tauri** for the desktop build.
- **2D slice and orthogonal views** as in `omezarr_viewers-rs`, but driven by palace rather
  than reimplemented.
- **IO via `zarrs`** with the same reach as `omezarr_viewers-rs`: local files, HTTP, S3, and
  an SSH remote agent, under an allow-list permission model.
- Polygonal/metadata overlays (annotations, detections, labels) composited with the volume.

---

## 2. Repositories and paths

| Path | What |
|---|---|
| `/home/mahogny/github/claude/newvolim` | **this project** |
| `<project>/palace-dev` | our fork of palace (`henriksson-lab/palace-dev`, at upstream `87ec9f7`), a subdirectory of this project — ours to edit freely. A second clone of the same fork is at `/home/mahogny/github/claude/volim/palace-dev`. |
| `/home/mahogny/github/claude/omezarr_viewers-rs` | the existing 2D viewer — source of features, formats and UI to port |
| `/data/henriksson/github/claude/volim` | research and decision docs (below) |

Background documents in `/data/henriksson/github/claude/volim`, worth reading in this order:

- `HANDOVER.md` — the architecture research: out-of-core rendering, residency, LOD, IO,
  annotations, and measured GPU limits.
- `forme.md` — fork-vs-reimplement analysis of palace, with module measurements.
- `PALACE_OURVIEWER.md` — integration design against `omezarr_viewers-rs`.
- `ADD_OMEZARR.md` — the alternative (palace as the base product) and its trade-offs.
- `dual.md` — client/server dual deployment and WebAssembly viability.
- `probe-results/` — measured GPU capabilities (see §5).
- `crates/volim-gpuprobe` — the headless probe that produced them; reusable.

---

## 3. Architecture

```
┌─ newvolim (this repo) ───────────────────────────────────────────────┐
│                                                                       │
│  newvolim-ui        Leptos components; canvas; camera/tool input      │
│                     builds to WASM (browser) and drives Tauri (desktop)│
│                                                                       │
│  newvolim-scene     session + layers + channels + transfer functions   │
│                     annotations, detections, label palettes            │
│                     (no GPU, no IO — pure data, unit-testable)         │
│                                                                       │
│  newvolim-render    metadata/annotation rendering: meshes, lines,      │
│                     points, depth-correct compositing against the      │
│                     volume. Wraps palace's render output.              │
│                                                                       │
│  newvolim-io        OME-NGFF metadata; source registry (fs/http/s3/    │
│                     ssh-agent); allow-lists; exposed to palace as a    │
│                     TensorOperator source (§7.3)                       │
│                                                                       │
│  newvolim-server    optional: chunk server + frame server (§8.4)       │
└───────────────────────────────────────────────────────────────────────┘
                                   │ uses as a library
┌─ palace (our fork; changes intended for upstream) ────────────────────┐
│  palace-core   task graph, 3-tier storage, residency, operators,      │
│                raycaster, sliceviewer                                  │
│                + NEW: wgpu backend alongside Vulkan                    │
│  palace-io     source dispatch  + NEW: runtime-registered sources      │
│  palace-zarr   zarrs reader     + NEW: remote stores                   │
└───────────────────────────────────────────────────────────────────────┘
```

**Rule: nothing bioimaging-specific goes into palace.** OME-NGFF, `omero` rendering settings,
GeoJSON, ROI tables, label palettes and our session model all live in newvolim.

---

## 4. What palace is, and what we measured about it

palace is "the progressive accelerated large array computing engine" — a Rust library for
interactive out-of-core processing *and* visualization of chunked multidimensional arrays.
Paper: [arXiv:2509.26213](https://arxiv.org/abs/2509.26213), Drees & Risse (Münster, the
Voreen lab). MPL-2.0. Dormant since 2025-09-30 — the day the paper was submitted. Single
author, so assume we own anything we touch.

Benchmarked on a **377 GB light-sheet kidney scan** (1634×12723×9070 uint16) on an **8 GB
RTX 2080** — our exact domain. Beats Sarton et al. 2020 by 1–2 orders of magnitude.
Converged frame times there: 1.36 s (far), 6.67 s (near). **These are seconds per converged
frame, not frame rates** — the interaction model must be progressive-first.

### 4.1 Structure (measured)

`palace-core` is 30,670 LOC in 58 files. **8,743 LOC is entirely Vulkan-free**, including the
parts we most depend on:

| File | LOC | Vulkan refs |
|---|---|---|
| `task_graph.rs` | 1,225 | 0 |
| `storage/cpu.rs` | 1,465 | 0 |
| `task_manager.rs` | 214 | 0 |
| `operator.rs` | 760 | 0 |
| `runtime.rs` | 1,498 | 4 |
| `task.rs` | 1,198 | 5 |
| `jit.rs` | 1,521 | 8 |
| `storage/gpu.rs` | 2,491 | 54 |
| `vulkan.rs` + `vulkan/*` | 3,706 | the backend itself |

Shaders: **45 files, 3,339 LOC of GLSL**, compiled at runtime by `shaderc`, plus `jit.rs`
which **generates GLSL source at runtime** for pointwise operations.

31 of 58 files reference Vulkan. The two most-used types outside `src/vulkan/` are
`vk::AccessFlags2` (**155 uses**) and `vk::PipelineStageFlags2` (**126 uses**) — Vulkan's
explicit synchronisation vocabulary, spread across operators. wgpu has no equivalent; it
inserts barriers automatically.

**All real threading is in one file**, `src/threadpool.rs` (an `spmc` job pool). Every other
`std::thread` reference is `thread::panicking()` inside a `Drop` impl. No rayon, no tokio, no
crossbeam, no mutexes in the crate. The task graph is genuinely thread-agnostic — which is
what makes WebAssembly plausible.

### 4.2 Design facts worth knowing before touching it

- **Each chunk is stored in a separate GPU buffer**, not in a texture atlas. palace says so
  explicitly and notes it "potentially underutilis[es] GPU sampling hardware" — i.e. **no
  hardware trilinear filtering**; interpolation is manual in-shader. This is why the page
  table holds *device addresses* (`VK_KHR_buffer_device_address`).
- **Page table**: fixed three-level, GPU-side insertion via atomics, no CPU round-trip.
- **Request/Use tables**: 2048-entry hash tables with bounded linear probing; dropped
  requests are explicitly acceptable. The **Use table** marks bricks actually sampled, so LRU
  recency comes from real GPU access rather than a CPU guess. Good design; keep it.
- **Storage**: 3 tiers (disk via `memmap`, RAM, GPU) with epoch-aware LRU — eviction stops
  when it meets an item whose command-buffer epoch hasn't completed. GC target ~10% of
  capacity. Size-bucket allocation recycling quantised to 1/256th.
- **Progressive rendering** via a `preview` tag: queries against a preview result spawn a new
  task rather than returning the stale one.
- **Tasks cannot be aborted.** Documented limitation. Conservative concurrency limits exist
  because deep graphs can **deadlock on memory**. We need cancellation (§7.5).
- **Anisotropic voxel spacing is supported** and is a stated differentiator over prior work —
  but it was still being fixed six days before submission (`Fix empty brick optimization for
  spacing!=1`). Treat anisotropic paths as the least-tested code.
- `palace-zarr` is **not mentioned in the paper**; it postdates it. Undocumented and outside
  every published benchmark.
- **No polygon, line or mesh rendering.** `operators/geometry.rs` (107 LOC) is a compute
  operator that applies a matrix to a point tensor — nothing is drawn. The only `.vert`/
  `.frag` pair is `raycaster/entryexitpoints.*` (proxy-box rasterisation for ray entry/exit).
  `GraphicsPipelineBuilder` does exist (`vulkan/pipeline.rs:212`) and is used by the GUI and
  raycaster, so the rasterisation plumbing is present — but mesh rendering and depth-correct
  compositing are ours to write (§8.3).

---

## 5. Measured GPU capabilities

From `volim-gpuprobe` (see §2 for the path; it is reusable and worth running on any new
target). **Do not re-derive these from documentation — the documentation disagrees with
itself.**

| | Linux / Quadro RTX 5000 (Vulkan) | macOS / Intel Iris Plus (Metal) | llvmpipe (Vulkan, CPU) |
|---|---|---|---|
| `max_texture_dimension_3d` | 16384 | **2048** | 4096 |
| `max_buffer_size` | 4 GiB | 2 GiB | 2 GiB |
| `TEXTURE_FORMAT_16BIT_NORM` | yes | **yes** | yes |
| `r16unorm` filterable | **yes** | **yes** | yes |
| largest cubic 3D texture | 1024³ | 1024³ | 1024³ |
| largest single allocation | **8 GiB** | 2 GiB | 2 GiB |

Conclusions already drawn from this:

- **`r16unorm` with hardware trilinear works everywhere.** The `r8unorm` + per-brick
  `(min, quantum)` fallback is not needed.
- **Bytes bind before dimensions.** The 2048-vs-16384 argument is moot; design the atlas to a
  byte budget. `max_buffer_size` does *not* cap textures (8 GiB texture on a 4 GiB
  `max_buffer_size` device).
- **Metal caps one resource at 2 GiB while allowing ≥8 GiB total.** A large portable pool may
  need multiple resources, but it cannot assume an unbounded/runtime-sized bindless texture
  array. The Stage-0 resource-model spike must establish a bounded, statically bindable pool
  design and encode its pool/page location in the page table.
- **llvmpipe works headless**, so a GPU-less server can still render (slowly).

From `vulkaninfo` through MoltenVK on the same Mac:

```
bufferDeviceAddress       = true
shaderBufferInt64Atomics  = false
shaderSharedInt64Atomics  = false
sparseResidencyImage3D    = false
```

So **hardware sparse residency and 64-bit atomics are *Metal* limitations, not wgpu gaps** —
no API choice routes around them. Software virtual texturing is permanent, and palace's u64
`atomicCompSwap` page table must become u32 packing on any portable path.

---

## 6. Decisions already made, with rationale

1. **wgpu 30 as the portable GPU API**, added *alongside* Vulkan rather than replacing it.
   Vulkan is not available in browsers; Metal lacks the features a Vulkan-everywhere plan
   would need; hand-writing Metal+Vulkan backends is strictly more work for no capability
   gain. Choosing wgpu is choosing Metal+Vulkan+D3D12 without maintaining them.
2. **Software virtual texturing** (page table + brick pool), never hardware sparse.
3. **Progressive-first interaction.** Never blank, never block: on a miss, substitute a
   coarser resident level and report the miss. Pin the coarsest pyramid level(s) permanently
   resident so the substitution walk always terminates.
4. **Per-brick LOD, never per-frame global LOD.** Bricks sharpen independently as they
   arrive. Requires a resident-level field in the page-table entry.
5. **Cache key is a 4-tuple** `{timepoint, channel, level, brick_xyz}` plus **per-level,
   per-axis** scale factors. Anisotropy means `level` alone does not determine spacing.
6. **Brick shape is a runtime parameter and non-cubic** (e.g. 128×128×8 for 10:1 data).
7. **Cancellation at the decode boundary**, not the read boundary — we are decompression-
   bound, so the read is cheap and the decode is many cores' worth.
8. **lz4 on the frame path, zstd for cold/archival.** Saturating 7 GB/s needs ~4 cores of
   lz4 versus ~15 of zstd-1.
9. **Do not design around bindless.** Metal storage-texture binding arrays remain
   unimplemented in wgpu.
10. **Upload bricks with `write_texture`**, never storage-texture writes.

### Anti-decisions — things not to do

- Do not use `io_uring`. Every published win is at 4–8 KiB, 128–4000× smaller than our
  chunks; it is also blocked by Docker's default seccomp profile.
- Do not use BC4/BC6H for quantitative data: BC4 is 8-bit, and fp16's 10-bit mantissa cannot
  round-trip uint16 above 2048.
- Do not use NanoVDB: dense data activates every leaf, loses hardware trilinear, and its
  topology is immutable in one allocation — incompatible with evicting a brick.
- Do not rely on `zarrs`' async API: it does not spawn tasks, so it is concurrent but not
  parallel. Use the sync API on our own pool.
- Do not use `zarrs`' chunk cache: cached retrieves bypass partial decoders. Cache *bricks*.
- Do not key a file-handle cache by path on Windows: `std::fs::File` lacks
  `FILE_FLAG_OVERLAPPED`, so concurrent `seek_read` is kernel-serialised at queue depth 1.
  Key by `(path, thread_id)`.
- Do not trust `zarrs_ome` or `bioformats2raw` to build pyramids: **neither can downsample
  anisotropically** (§8.6).

---

## 7. Work package A — changes to palace (write these to be upstreamable)

Principles: feature-gate everything; do not break the Vulkan path; keep bioimaging concerns
out; prefer additive changes. Consider contacting ftilde (Dominik Drees) early — a dormant
single-author artifact with a serious user is a collaboration opportunity, and agreement on
the backend seam before writing it will make PRs far easier.

### A1. Backend abstraction (prerequisite for everything else)

The seam mostly exists: `src/vulkan.rs` exposes `VulkanContext`, `DeviceContext`,
`CommandBuffer`, `DeviceId`, `CmdBufferEpoch`, `BarrierInfo`, `SrcBarrierInfo`,
`DstBarrierInfo`. The work is to make that an interface rather than a concrete type.

1. **Replace the leaked Vulkan sync vocabulary with palace-owned enums.** **Completed for
   operators and GPU storage:** resource-use declarations now use `palace_core::gpu::Access`
   and `::Stage`, while Vulkan conversion remains confined to backend execution and explicit
   image operations. This is a reviewable first PR-sized readability improvement even for the
   Vulkan-only build.
2. **Define the backend trait(s)** around device/queue, buffer/image allocation, command
   recording, pipeline creation, descriptor/bind-group binding, and epoch tracking. The first
   usable slice is complete: `gpu::SubmissionEpoch` and `SubmissionTracker` now own the
   submission-order/completion contract used by GPU storage and Vulkan temporary-resource
   reclamation. Keep the remaining traits narrow; resist making them a general GPU abstraction.
3. **Move `vk::`-typed items out of operator signatures.** 73 distinct `vk::` types appear in
   `src/operators/` alone.

Scope control: the wgpu backend does **not** have to support every operator initially.
`randomwalker`, `vesselness` and friends can stay Vulkan-only behind a capability flag. Aim
for **raycaster + sliceviewer + rechunk/resample** first — that is what a viewer needs.

### A2. wgpu backend

- **The Vulkan buffer-per-chunk model is not portable to wgpu.** WebGPU has neither buffer
  device addresses nor a portable bindless array of arbitrary per-brick buffers. Keep the
  model intact on Vulkan, but make the wgpu implementation a bounded brick pool: either a
  texture atlas or statically bound storage-buffer pages, selected by the Stage-0 spike.
- **Page table: device addresses → pool locations.** Encode a pool/page/offset location, not
  an arbitrary buffer reference. The pool count and binding scheme must stay within portable
  WebGPU/Metal limits; do not defer this to a later atlas optimisation.
- **Request/Use tables: u64 → u32.** WGSL atomics are 32-bit only. Pack `(chunk_index, level)`
  into a `u32` — Tuvok's `Serialize()` shows the pattern. Note the Quadro *does* expose
  `SHADER_INT64_ATOMIC_ALL_OPS`, so the Vulkan path can keep u64 and we can diff the two
  implementations against each other during bring-up.
- **Push constants → uniform buffers.** WebGPU has no push constants; wgpu's immediates are
  native-only. palace uses `DynPushConstants` pervasively, so this is per-call-site work.
- **Epochs → `SubmissionIndex` + poll.** palace's epoch-aware eviction needs an equivalent;
  this is a translation of the resource-lifetime model, not a rename.

### A3. Shaders

The largest single work item. palace's shader story is GLSL throughout: 3,339 LOC by hand,
`shaderc` (a C++ library) at runtime, `crevice` for GLSL struct layout, SPIR-V reflection via
`spirq`, and `jit.rs` generating GLSL source at runtime.

Two candidate paths — **test both before committing**:

- **(a) Retarget through naga.** GLSL → `shaderc` → SPIR-V → naga → MSL/HLSL/WGSL. Would
  preserve the hand-written GLSL *and* `jit.rs`. Risks: naga's `spv-in` is less exercised
  than `wgsl-in`; `shaderc` is C++ and will not build for wasm; SPIR-V passthrough in wgpu is
  Vulkan-only. Possibly viable natively, **not** viable in the browser.
- **(b) Rewrite in WGSL** and retarget `jit.rs` to emit WGSL. More work, but it is the only
  path that reaches the browser, and it removes the C++ dependency.

Given the browser requirement (§8.4), **(b) is likely unavoidable**; (a) may still be worth
it as an interim step to get a working wgpu backend sooner.

### A4. Data sources — extend, don't bolt on

**Important: palace already has the right abstraction, and it is not a "chunk source" trait.**
The unit is `TensorOperator` — a source is an operator that produces chunks on demand and
plugs into the existing task graph. `palace-io` (170 LOC) dispatches on **file extension** to
feature-gated crates (`palace-zarr`, `palace-hdf5`, `palace-nifti`, `palace-vvd`,
`palace-png`, `palace-video`), returning `EmbeddedTensorOperator<DDyn, DType>` or
`LODTensorOperator`, with a `Hints { chunk_size, location, rechunk, lod_downsample_steps }`.

Do **not** invent a parallel `ChunkSource` trait. Extend what exists:

1. **Runtime registration instead of extension dispatch.** `palace-io` currently matches on
   the filename suffix of a `PathBuf`. We need URL/spec-based opening (`s3://…`, `http://…`,
   an SSH-agent handle) and a registry an application can add to at runtime. This is a
   genuinely general improvement and a good upstream PR.
2. **Remote stores in `palace-zarr`.** It currently pins `zarrs 0.19.2` with only
   `filesystem`. Add `zarrs_opendal`/`zarrs_object_store` behind features so http/s3 work.
   (`omezarr_viewers-rs` uses `zarrs 0.18` + `opendal 0.53`; current upstream is 0.23+.
   **Pick one version across all three codebases early** — drift here will hurt.)
   Its local ZIP writer is deliberately append-only: every completed single-tensor or LOD save
   explicitly flushes a readable central directory (and returns finalization errors), while
   partial writes and erases return explicit unsupported-operation errors rather than creating
   ambiguous duplicate archive members; repeated `set` keys are rejected for the same reason.
3. **Keep credentials and allow-lists out of palace.** Our permission model, S3 profiles and
   SSH agent live in `newvolim-io`, which supplies palace with an already-authorised store.

### A5. Cancellation

palace cannot abort tasks, and conservative concurrency limits exist because deep graphs can
deadlock on memory. A camera fling must be able to abandon the work it obsoleted.

This is surgery on the least-documented subsystem and the riskiest change in package A — the
failure mode is a hang, not a crash. Approach: requests carry a generation stamp; drop
queued-but-unstarted work aggressively; check the stamp before dispatching decode. A
completed-but-stale brick is still a valid cache entry — insert it at low priority rather
than discarding it. **Cancellation is about not starting work, not about discarding results.**

### A6. WebAssembly viability

Blockers beyond the GPU layer, all currently unconditional (the repo contains **zero**
`target_arch` or wasm awareness):

| Item | Action |
|---|---|
| `ash`, `gpu-allocator` (git fork) | replaced by the wgpu backend |
| `shaderc` (C++) | eliminated by A3(b) |
| `memmap` 0.7 — the disk tier | no filesystem in browsers: RAM+GPU tiers only, network as third tier |
| `pyo3`/`numpy` | **Completed:** `python` is explicit rather than a default feature; shared viewer state and its Rust API compile without the Python ABI, while the bindings retain their explicit feature build. Disable the feature for wasm. |
| `winit`, `winapi`, `num_cpus`, `good_memory_allocator`, `graphviz-rust` | cfg-gate or drop |
| `threadpool.rs` | single-threaded executor, or `wasm-bindgen-rayon` (needs COOP/COEP — see §8.4) |

The encouraging part: the task graph is thread-agnostic (§4.1), the Vulkan-free core is plain
Rust, and `zarrs` treats wasm as a first-class target.

---

## 8. Work package B — newvolim

### B1. `newvolim-scene` — session and layers (no GPU, no IO)

Port the model from `omezarr_viewers-rs` (`LayerInfo`, `LayerKind`, `ChannelState`,
`ViewerState`, `OmeroChannel`, `ChannelWindow`). Per-layer scale; per-channel colour,
contrast window and opacity; image layers composite additively. `omero` metadata is our
**default transfer function** (`window.start`/`end` = display range).

Pure data, fully unit-testable, and the vocabulary everything else speaks.

### B2. `newvolim-io` — OME-NGFF and sources

`omezarr_viewers-rs` already has, in production and shared between its server and frontend:
`DatasetMetadata`, `Multiscale`, `Axis`, `CoordinateTransformation`, `OmeroMetadata`,
`ArrayInfo`, `DatasetInfo`, `SeriesInfo`; plus a source registry over opendal (fs/http/s3),
S3 profiles, an SSH remote agent, and allow-list validation. **Port this rather than rewrite
it** — it is months of unglamorous, security-sensitive work.

Note NGFF 0.6rc0 replaces flat scale/translation with a graph of named `coordinateSystems`
(`affine`, `rotation`, `sequence`, `displacements`). **Build the transform layer as a
composable graph from day one**; retrofitting means rewriting camera and picking maths. Also:
per-level `translation` is not optional — with unequal per-axis factors the half-voxel offsets
differ per axis, so omitting it misregisters Z against XY.

Remote-source security is a product requirement, not only an allow-list: define capability-
scoped credentials, SSRF-resistant URL/store validation, metadata and decompression budgets,
per-user request quotas, tenant-separated caches, audit logging, and credential revocation.
Exercise these with adversarial fixtures before exposing any remote-source endpoint.

### B3. `newvolim-render` — metadata and annotation rendering

palace renders volumes and slices; **everything polygonal is ours** (§4.2). Needed:

- a backend-neutral render-target contract from the start: physical-pixel extent, linear or
  encoded colour semantics, progressive state, readback, and volume ray-distance/depth
  semantics. Palace's current public raycast result is a frame only; depth-correct composition
  requires this contract before annotations are implemented;

- mesh/line/point primitives and upload;
- tessellation (`omezarr_viewers-rs` uses `earcut` client-side; `i_triangle` is the better
  maintained choice for new work, with `lyon` for strokes);
- **depth-correct compositing**: render annotation geometry to a depth target first, bind it
  to the ray pass, clamp each ray's `t_max`. Correct occlusion *and* a performance win via
  early ray termination;
- picking (`parry3d` for ray/mesh queries).

Annotation kinds to support, in order: points (billboards) → rectangles/ellipses (quads/
ellipsoids) → polygons (planar meshes) → label volumes (through the volume path) →
isosurfaces (later).

If TB-scale annotation volume ever becomes a problem, Neuroglancer's precomputed
multiresolution mesh format is the only format designed for it; `draco-oxide`'s
`decode_mesh_portable()` returns integer attributes, which is exactly its contract.

### B4. Client *or* server rendering

The same wgpu renderer in three places:

1. **Browser** (WASM + WebGPU) — client-side rendering; the server stays a **chunk server**.
2. **Desktop** (Tauri + native wgpu) — local GPU, local or remote data.
3. **Headless server** (native wgpu, or llvmpipe with no GPU) — frames streamed to a thin
   client.

Client-side rendering fits `omezarr_viewers-rs`'s existing architecture *better* than
server-side, because the server already serves tiles with a cache; the only addition is a
brick request alongside the existing tile request.

**Decision: Leptos runs in CSR mode only. We do not use Leptos SSR.**

In this project **"server-side rendering" always means rendering 2D/3D *views* (frames) on
the server — never HTML.** Leptos's own "SSR" feature, which server-renders HTML and
hydrates, is explicitly not used. The two are orthogonal; conflating them would be an easy
and expensive mistake.

What that buys us:

- No `leptos_axum`/`leptos_actix`, no hydration, no server functions, no isomorphic routing.
- **Component code only ever compiles to wasm.** Leptos SSR requires components to build for
  both wasm and native, which constrains what they may do and spreads `cfg` gates through the
  UI. CSR-only avoids that entirely.
- The UI is a static wasm bundle (trunk), exactly as `omezarr_viewers-rs`'s yew app is built;
  Tauri serves the same bundle.
- The server stays a pure data/frame service behind a plain HTTP/WebSocket API, with no HTML
  involvement — so it remains compatible with the existing actix server and its API shape.

Browser constraints that shape the design even natively:

- **No push constants** → uniform buffers everywhere.
- **No threads without COOP/COEP.** Those headers sever `window.opener` (breaking OAuth
  popups) and require CORP on cross-origin subresources; Neuroglancer refuses them for this
  reason. Staying COOP/COEP-free means **single-threaded decode**, which matters because we
  are decompression-bound. Decide early.
- **No filesystem** → the network is the third storage tier.
- **Lower default limits**, raisable toward adapter limits.

### B5. 2D and orthogonal views — via palace

`omezarr_viewers-rs` has a 2×2 layout: an `xy` view with drawing tools, two orthogonal
slices, and a proportioned box showing where the cuts sit (deliberately not normalised to a
cube — a 512×512×8 volume is a slab and should look like one; the camera is near-isometric
rather than isometric so the axes stay distinguishable).

**Do not reimplement the slicing.** palace has `operators/sliceviewer` (721 LOC) and
`operators/imageviewer` (399 LOC) plus a `transfunc` module, all out-of-core. Drive those and
keep our own code to layout, crosshair linkage, tools and overlays.

Also worth keeping from the existing viewer: server-side max/mean projection through a z-slab
— it already exists there and maps onto palace operators.

### B6. `newvolim-ui` — Leptos, and Tauri

- **Leptos** for components, state and reactivity; compiles to WASM for the browser and is
  reused by the Tauri desktop shell.
- **Tauri** for desktop packaging, local filesystem access and the SSH remote-agent flow
  (`omezarr_viewers-rs`'s `desktop` crate already implements SSH discovery, diagnostics and
  profile setup — port it).
- The volume view is a `<canvas>`: WebGPU-backed in the browser, or displaying streamed
  frames when rendering server-side. **The switch between those two must be invisible to the
  rest of the UI.**
- Verify current Leptos and Tauri major versions before starting; both move quickly.

---

## 9. Implementation order

### 9.1 Principles

1. **Build the application on palace's *existing Vulkan* backend first; substitute wgpu
   underneath it later.** palace renders today, on Linux, which is our development platform.
   Building the viewer, IO, scene model and 2D/ortho views against it means months of
   user-visible progress before the backend port starts — and once the port happens, the
   working Vulkan path is a **correctness reference to diff against** (the dev Quadro exposes
   `SHADER_INT64_ATOMIC_ALL_OPS`, so palace's shaders run there unmodified).
2. **Frame streaming is the common path for desktop *and* server-side rendering.** Tauri with
   native palace and a headless frame server both hand the Leptos webview a rendered frame;
   only the transport differs (IPC vs WebSocket). Build the protocol once, early, but budget
   separately for headless surfaces, GPU readback, encoding, backpressure, resizing, input
   latency and multi-user isolation. **Client-side WebGPU is the last mode added, not the
   first.**
3. **Spike the plan-invalidating unknowns in week one**, before committing to the
   architecture (§9.2).
4. **No long unfeedbacked branches.** Every stage ends with something runnable.
5. **Keep GPU-independent work off the critical path** — the pyramid builder, NGFF parsing,
   the source registry and the scene model depend on no backend and can proceed in parallel.

### 9.2 Stage 0 — spikes (about a week; each kills or confirms a major assumption)

Do these **before** committing to the rest. Each is days, not weeks, and each answers a
question that would otherwise be discovered expensively later.

| # | Spike | Decides |
|---|---|---|
| S1 | **Resolved for the committed CC0 cells3d fixture:** release `demo-headless-frame` renders its anisotropic OME-Zarr path through Palace, including forced llvmpipe | Palace works for this real microscopy data path; retain a production-scale residency/throughput benchmark as later validation because `palace-zarr` postdates the paper and is unbenchmarked. |
| S2 | **Resolved for the representative fixture:** native, browser SIMD wasm, and browser `numcodecs.js` decode zstd/LZ4/Blosc chunks with matching checksum/warm-up rules | Single-threaded Rust wasm is viable for the MVP; do not require COOP/COEP before browser work, but re-benchmark larger production chunks (§10.2) |
| S3 | **Resolved for Palace entry/exit shaders:** compiled SPIR-V passes Naga `spv-in` and emits WGSL and MSL | Retarget the shared geometry/pipeline subset first; the full raycaster still needs a capability-by-capability migration because its Vulkan-only buffer references and 64-bit atomics are outside WebGPU. |
| S4 | Leptos CSR in a Tauri webview displaying a frame rendered by native palace | The UI↔renderer seam, and therefore the whole frame-streaming design |
| S5 | **Resolved:** `Hints::lod_downsample_steps` is one fixed per-axis vector reused at every generated level | Write the anisotropic pyramid builder (or upstream a per-level schedule); Palace's current hint cannot express one (§10.1) |
| S6 | **Partial:** Chromium WebGPU capability smoke on Linux reports a 2048³ default 3D texture limit, 1 GiB buffer/storage-binding limits, 10 storage buffers/stage and 4 bind groups | Run the reusable probe on Apple Silicon and Windows/D3D12; browser defaults are useful floor evidence, not a replacement for those target measurements |
| S7 | **Partial:** the native wgpu spike resolves packed page-table locations through four fixed storage pages, emits a real first-opacity index (`+∞`-equivalent no-hit sentinel at the empty edge), and consumes a seventh fixed storage binding for depth-tested annotation primitives; Chromium's 10 storage-buffers/stage floor admits that layout; `newvolim-wgpu-frame` also cross-checks for `x86_64-pc-windows-gnu` | Run the same spike on macOS and Windows/D3D12; the selected static storage-page representation remains the portable first backend. Linux cannot cross-check Apple because the host has no Apple C toolchain/SDK. |
| S8 | **Partial:** `newvolim-render` defines the physical-pixel linear RGBA16Float + `RayDistanceF32` contract and progressive admission; native WGPU packs/reads back linear RGBA16F plus physical PFM depth, the browser writes matching RGBA16F/R32F attachments before sRGB presentation, and Palace now returns a paired first-opacity PFM | Prove on real browser/Apple/Windows adapters, add camera-ray reconstruction and depth-correct annotation compositing, and implement progressive renderer passes |

**Gate:** if S2 shows decode throughput is hopeless single-threaded, client-side rendering
drops out and the plan simplifies to server-side only — which is a *smaller* project, not a
failed one. If S1 shows `palace-zarr` cannot read our data usefully, IO work moves earlier.
S7 must select a representation within the portable target limits before backend work starts;
S8 must establish an implementable depth contract before the Vulkan viewer API is frozen.
Every spike records first meaningful-frame time, refinement time, peak RAM/VRAM, decode
throughput, request-drop rate and a repeatable image-diff tolerance where applicable.

### 9.3 Stages

**Stage 1 — local-data foundation** *(no GPU; parallelisable with everything below)*
`newvolim-io`: OME-NGFF metadata (ported from `omezarr_viewers-rs`) and the transform graph
for one representative, local, pre-pyramided OME-Zarr fixture. `newvolim-scene`: session,
layers, channels and transfer functions. Commit a small, legally redistributable anisotropic
fixture plus golden camera/transform cases.
*Ends with:* real data described correctly and unit-tested on CPU; a stable fixture for every
later rendering and image-diff test.

**Stage 2 — Vulkan viewer MVP** (B1, B5, B6 + frame streaming)
Leptos CSR UI; Tauri desktop shell; 2D and orthogonal views driven by palace's `sliceviewer`;
3D view; the 2×2 layout, crosshair linkage and camera controls. Implement the S8 render-target
contract and application-level request admission: coalesce camera updates, bound in-flight
work, and drop queued obsolete requests before they enter palace.
*Ends with:* **a usable 3D viewer on Linux for the local fixture.** This is the first stage a
user can be shown.

**Stage 3 — data sources and pyramid generation** (A4, B2; **raw-v3 local pyramid writer
implemented for bounded in-memory inputs**)
Runtime-registered URL/spec-based sources and remote stores in `palace-zarr`; the fs/http/s3
source registry, allow-lists and the security/operational controls in B2. Build the
**anisotropic pyramid builder** (§10.1), unless an upstream API is extended to express a
per-level schedule.
*Ends with:* palace rendering authorised local and S3 OME-Zarr; correct anisotropic pyramids;
a small, reviewable candidate upstream source-registration PR.

**Stage 4 — cancellation and server-side rendering** (A5, B4 partial; **loopback remote frame viewer proven**)
Implement generation-stamped cancellation in palace after the MVP has proved the request
shape, then swap the IPC transport for HTTP/WebSocket. Add headless frame rendering,
readback/encoding, progressive-frame backpressure, resize/input handling, session isolation
and llvmpipe fallback for GPU-less servers.
*Ends with:* the same viewer working against a remote GPU server, and against a CPU-only one.

**Stage 5 — palace backend seam** (A1) — **first upstream PR of the backend work**
palace-owned sync enums replacing `vk::AccessFlags2`/`PipelineStageFlags2` (281 sites), the
backend trait, `vk::` out of operator signatures. Vulkan path unchanged, tests still pass.
Valuable to upstream on its own as a readability improvement.

**Stage 6 — wgpu backend, native** (A2, A3)
Scoped to raycaster + sliceviewer + rechunk/resample. Implement the S7-selected bounded brick
pool and pool-location page table, u32 request/use tables, uniform buffers instead of push
constants, and the S8 render-target contract.
*Ends with:* the Stage 2 viewer running on macOS and Windows, **validated frame-by-frame
against the Vulkan path.**

**Stage 7 — browser** (A6; **real raw and Zstd chunk previews proven**)
wasm build, WebGPU, the chunk-server path. Client-side rendering; the server reverts to being
a chunk server, which is what `omezarr_viewers-rs`'s already is.
*Ends with:* all three rendering sites live, selectable by deployment.

**Stage 8 — annotations** (B3)
Mesh/line/point rendering, depth-correct compositing, picking, label volumes. **Written once,
against wgpu only** — see §9.4.

**Stage 9 — performance**
Optimise the Stage-6 brick pool (including an atlas with hardware trilinear if S7 did not
select it), compressed-in-VRAM bricks, and speculative prefetch.

### 9.3.1 Remaining portable-renderer migration plan

The bounded portable page, scheduler, and native WGPU frame paths are implemented. The remaining
work is a staged migration of the legacy Vulkan-only production renderer, not a reason to weaken
the established camera/depth contract.

1. **Portable frame boundary.** Define backend-neutral bounded frame inputs in `palace-core`:
   ordered page/layer/channel descriptors, trusted rays and finite intervals, transfer data, and
   paired RGBA plus first-opacity `f32` outputs. Keep page ownership and submission epochs.
   Add CPU reference execution for every layout.
2. **Sliceviewer first.** The CPU oracle and local WGPU recorder now fix raw orthogonal axis
   order and edge-memory zero padding across one to four ordered scalar pages. Desktop adapts
   an admitted single-channel volume with one to four pages to that same CPU boundary; an
   unsupported page range or multi-channel packet retains the existing route. The local-adapter
   four-page test compares the static WGPU bindings with the CPU page lookup, including distinct
   owner tags and short page lengths. Retarget the three orthogonal views to that boundary using
   the desktop-admitted pages and physical transforms. Keep Vulkan as fallback until orientation,
   crosshair, transfer-function and edge-padding comparison fixtures agree.

   Local evidence: `cargo test --offline --manifest-path palace-dev/Cargo.toml -p
   palace-core --lib gpu::tests::orthogonal_slice_reads_the_ordered_four_page_scalar_representation`,
   `cargo test --offline --manifest-path palace-dev/Cargo.toml -p palace-wgpu-spike
   bounded_wgpu_slice_matches_cpu_across_four_scalar_pages -- --ignored`, and `cargo test
   --offline --manifest-path crates/newvolim-desktop/Cargo.toml
   palace_slice_adapter_reads_an_admitted_four_page_channel_in_order` passed on this Linux
   local adapter. The desktop now also exposes a portable orthogonal-pane command: it rebuilds
   the crosshair-selected bounded admission, extracts XY/XZ/YZ through the shared four-page CPU
   layout, carries the resident voxel origin/dimensions to the webview, and uses the declared
   transfer table for the resulting PNGs. The webview selects that command only while native
   portable admission is active; an admission/range failure retains the full-volume Palace pane
   command. The desktop pane extractor now attempts the recorder-selected path on a local
   adapter before falling back to the identical CPU page oracle; its four-page desktop regression
   passed on the local adapter. A deterministic common fixture now rasterizes Palace's procedural
   ball scalar field into an admitted portable page, uses Palace's matching intensity-coupled
   grey-ramp RGBA transfer, and compares all XY/XZ/YZ pixels with Palace's legacy slice output
   at a maximum error of one 8-bit value. The same fixture now compares Palace's red-ramp colour
   and opacity policy, and asserts the admitted 0.26/0.26/0.29 physical transform produces the
   expected XY/XZ/YZ viewport ratios. A declared-channel regression also fixes the portable LUT
   mapping for a non-default window, sRGB colour, and half opacity. Portable panes now accept
   ordered multi-channel page ranges: each channel extracts its own bounded page sequence, then
   the shared premultiplied-linear portable scene-composition oracle combines those samples before
   straight-sRGB PNG encoding. A two-channel red/green half-opacity fixture proves distinct page
   ranges, expected linear `[0.5, 0.5, 0.0, 1.0]` composition, and payload generation. Multi-layer
   admissions now have a bounded portable scene-pane command: each axis-aligned layer is
   nearest-sampled at the first layer's physical voxel centers, then composes in declared
   source-over order via the same oracle. A red-under-green fixture verifies straight-sRGB output
   after premultiplied linear composition; scale-change and translated-out layer cases prove
   physical sampling and transparent out-of-bounds behavior. Transformed layers now state and
   execute one bounded resample policy: map the first layer's physical voxel centre into each
   target layer, take `floor` in target voxel coordinates, subtract that layer's resident origin,
   and make an out-of-range result transparent. A deterministic 0.6-scale fixture uses a value
   for which floor and round-to-nearest select different target voxels, and repeats it with the
   target's boundary exactly at the reference centre. Unsupported admissions still retain Palace
   fallback. The existing Palace procedural-ball all-pane comparison remains the common
   untransformed baseline; arbitrary transformed-layer sampling has no matching legacy Palace
   interface, so its fixture is an explicit scalar-oracle comparison. Linear interpolation is a
   separate future contract rather than an implicit change to label and transfer semantics.

   The linked desktop panes now carry an explicit XY/XZ/YZ physical aspect-ratio sidecar. It is
   derived from the level-zero axis-aligned NGFF transform and extent, applied only to canvas
   presentation, and leaves voxel-space crosshair requests and the legacy Palace route intact.
   Non-axis-aligned or incomplete metadata deliberately falls back to square pane pixels rather
   than claiming a false physical transform. The committed anisotropic fixture verifies the
   XY/XZ/YZ ratios and a synthetic transport test verifies the serialized sidecar. The remaining
   fixture work now has a legacy Palace comparison baseline; retain it while broadening coverage
   beyond the deterministic axis-aligned grey-ramp case.
3. **Bounded raycaster.** The core CPU oracle validates trusted unit rays, admitted AABB
   intervals, Palace DVR transfer compositing, and paired colour/first-opacity frame assembly.
   Local-adapter WGPU compute dispatches consume the serialized bounded ray/sample packet and
   explicit transfer LUT, and are parity-tested against that oracle for both attachments. The
   page path now reads scalar values directly from one to four static owner-tagged WGPU pages via
   portable page/word records. A bounded one-level CPU raymarcher clips trusted rays to an
   admitted physical volume and generates those records across page boundaries; its output has
   CPU and local-adapter WGPU parity coverage, including the fourth static page binding. The existing runtime recorder trait now exposes
   this page-DVR operation, allowing a migrated operator to select it without a concrete WGPU
   dependency. Desktop now adapts its admitted direct-volume page range and fitted local-XYZ
   camera rays to this packet through the layer's anisotropic physical transform and converts its
   declared one-channel window/colour/opacity into an explicit Palace LUT. The production
   direct-volume camera route now executes that page-DVR packet through the local Palace WGPU
   recorder, with the identical core CPU oracle as adapter fallback. A fitted-camera packet that
   exceeds the current bounded sample limit retains the existing native recorder rather than
   truncating or rejecting a valid frame; its annotation picker reads
   this renderer-owned physical first-opacity distance directly. The camera packet now carries
   its resident global voxel origin separately from page-local rays, and a nonzero-chunk fixture
   proves Palace reconstructs the matching physical ray origin before clipping/marching. Packets carrying projected
   annotation colour still use the existing native overlay recorder until Palace owns that pass,
   while multi-channel/layer DVR remains on the typed ordered-scene recorder pending a Palace
   ordered-layer operator. `palace-core` now also defines the bounded physical
   `PortableDvrSceneFrameInput`: up to four globally non-aliasing resident pages, ordered
   layers/channels, explicit per-channel transfer tables, unit physical rays, and a deterministic
   source-over/Beer-Lambert CPU oracle with paired first-opacity distance. Its red/green/blue
   fixture proves channel and layer order plus transparent out-of-bounds physical sampling. The
   fixed-binding WGPU scene compute pass is now implemented and parity-tested against that oracle
   on this local adapter, across all RGBA bytes and the paired physical first-opacity distance;
   the per-frame CPU comparison guard that previously stood in for parity has been removed, and
   the core oracle is retained only as the no-adapter/recording-failure fallback. Reaching parity
   required a portable Beer-Lambert correction: a fully opaque transfer entry makes the base
   exactly zero, where SPIR-V/GLSL leave `pow` undefined and this driver returns NaN while Rust's
   `powf` returns zero, so one opaque classification previously poisoned the whole composite. The
   scene shader now emits a bounded single-invocation decode trace, which is what identified that
   cause and is retained for the deferred Apple/Windows adapter gates. The same latent expression
   in the two older single-volume DVR shaders is fixed identically and now has opaque-entry
   regression coverage. That ordered-scene route now has its own trusted picker: it rebuilds
   the scene camera packet, normalizes the Palace-derived physical world ray after the reference
   layer transform, and compares annotations only against the scene renderer's paired physical
   depth. A local-adapter fixture proves the path without accepting browser depth. Scene
   picking now reads its occluding distance from that Palace attachment rather than from the
   native scene renderer. Both renderers keep depth volume-only — the native shaders
   colour-composite a covered annotation without writing it into `ray_distance` — so the
   substantive difference is the first-opacity rule itself: the native pass records the step where
   accumulated opacity first reaches `0.01`, while Palace records the step-centre distance of the
   first strictly positive sample, which is the oracle-defined and parity-tested rule. Routing the
   picker moves the occlusion boundary rather than relocating an identical number. Reaching a reachable route required admitting a ray that misses the scene
   as a degenerate `near == far` interval instead of rejecting the whole packet: a framed camera
   always has such pixels, so the previous rule sent essentially every realistic camera back to
   the native renderer. Both the oracle and the shader render such a ray transparent with
   `+infinity` depth, and a local-adapter fixture covers a mixed hit/miss frame. The desktop
   fixture additionally asserts the picked depth equals the Palace attachment word at a hit pixel,
   a missed pixel and both frame edges, and that an annotation in front of the first-opacity
   surface is selectable while one behind it stays occluded. Palace now also owns the projected annotation
   pass, completing this item: `PortableAnnotationCompositeInput` carries a rendered frame plus up
   to 4 096 ordered projected primitives, its CPU oracle and the fixed-binding WGPU pass have
   local-adapter parity across point, segment, triangle and degenerate records, and the desktop
   scene route no longer leaves the portable path for an annotation-bearing packet. The pass
   returns the first-opacity attachment unchanged, so annotation compositing consumes the
   renderer-owned depth without becoming part of it. Two rules deliberately differ from the
   viewer's older pass and are the reason for owning it: the nearest covering primitive wins
   rather than the last one in the packet, and colour is written in the attachment's own encoded
   sRGB space rather than linearized for a float surface. Next, connect normal Palace level/chunk
   planning and physical camera rays to this admitted level.
4. **Page/residency planning.** The current desktop scene route still calls
   `LocalSession::local_layer_chunk_plan` with a webview-supplied chunk region; it is bounded and
   validated, but is not normal Palace level/chunk task planning. Translate normal Palace chunk
   requests into an ordered bounded packet, with deterministic Vulkan fallback whenever the
   working set exceeds the bound. Reuse cannot occur before completion; page-table generality
   remains a later expansion.

   The translation half is implemented. `PortableChunkGrid` carries one level's dimensions and
   chunk shape with Palace's X-fastest numbering and level-clipped edge chunks;
   `PortableChunkPlan::from_demand` turns `PortableFeedbackKey` demand into an ordered page
   layout with per-chunk origin, logical extent, page ordinal and first word, per-page word
   lengths, and consecutive global owners; and `PortableChunkPlanOutcome::ExceedsPortableBound`
   reports the required page count rather than truncating, which is the deterministic Vulkan
   fallback this item requires. The plan sorts by chunk index because a page ordinal is a static
   binding slot: `PortableFeedbackTable::keys` iterates hash slots, so inheriting its order would
   rebind pages between frames and defeat residency reuse.

   The shader-visible residency map is also implemented. `PortablePageTable` maps a
   `PortableFeedbackKey` to a `PortablePageLocation`, replacing the Vulkan page table's 64-bit
   buffer addresses that WebGPU does not expose. It shares `PortableFeedbackTable`'s hash, linear
   probing and probe cap, because a shader that misses a residency lookup records that key into
   the request table and the two must agree about slots; and its lookups are total, with absence
   carried as a separate flag rather than a sentinel location, since page zero word zero is a
   legal residency. A local-adapter fixture holds the WGSL probing rule to the oracle across
   probe chains, probe exhaustion and absent keys, and fails on a single-bit change to the hash
   constant.

   Request emission exists as a parity-tested primitive.
   `WgpuOperatorRecorder::record_feedback_inserts` rebuilds Palace's request table without 64-bit
   atomics, using one `atomicCompareExchangeWeak` per candidate slot and the same hash, probing
   and probe cap as the host `PortableFeedbackTable`. Because WGSL's compare-exchange is the weak
   form, a spurious failure retries the same slot rather than advancing the probe, so the shader is
   never lossier than the host. Local-adapter fixtures pin key-set equality, slot agreement with
   the page table, dedup across 512 invocations of one key, and explicit drop reporting past the
   probe bound; where contention makes an outcome order-dependent the tests assert occupancy and
   subset membership instead of identity, rather than asserting something that is not determined.

   Demand-resident scalar addressing is also implemented. `PortableChunkedChannel` maps a voxel to
   a chunk key plus an intra-chunk offset, resolves it through the page table, and yields a named
   missing key instead of a wrong scalar; a local-adapter dispatch performs the same addressing,
   lookup and request recording and is parity-tested per voxel. The intra-chunk stride is the
   chunk's clipped logical extent rather than its nominal shape, matching how the plan sizes an
   edge chunk, because any other stride would read a neighbouring chunk's scalars at every level
   whose dimensions are not an exact multiple of the chunk shape.

   The feedback loop itself is implemented and proven on the GPU. `PortableResidencyLoop` merges
   a pass's reported misses and replans, distinguishing completion, a new plan, a page-bound
   overflow that preserves committed residency, an iteration cap that corresponds to Palace's
   preview-version timeout, and a desynchronized pass that reports a miss for an already-resident
   chunk. Convergence rests on the request table being lossy but monotone: every accepted
   iteration plans at least one previously unplanned chunk. A local-adapter fixture drives the real
   resolve dispatch with a deliberately tiny request table so the shader drops most misses each
   pass, and asserts both that drops occurred and that several passes were needed before the frame
   converged and requested nothing.

   The scene pass consumes it. A channel can be declared demand-resident, in which case the
   shader resolves its scalars through the residency map and records a request for every chunk it
   misses, while a statically paged channel renders unchanged. A local-adapter fixture renders the
   same scene both ways and requires the demand render to equal the static render's oracle output
   exactly, rather than introducing a second oracle. Fitting this revealed that the portable
   storage-binding budget is **eight** per compute stage, not the ten S6 measured in Chromium —
   ten is above the WebGPU guaranteed floor — so the residency map was packed into the metadata
   buffer and the diagnostic trace into the output buffer rather than taking new bindings.

   Camera-driven level selection is implemented too. `select_portable_level` mirrors
   `sliceviewer::select_level` rather than introducing a second policy — same acceptance rule,
   same behaviour across multiple directions, same level-zero floor — and `portable_pixel_footprint`
   supplies the pixel term from two neighbouring trusted rays rather than from a projection matrix,
   keeping camera convention out of `palace-core`. A local-adapter fixture runs the whole chain:
   the selected level index becomes the residency key's level, the loop discovers that level's
   chunks through the real scene dispatch, and the converged frame equals the statically paged
   render of the same data.

   What remains is replacing the desktop's webview-supplied chunk region with that chain. The
   session can now express it: `local_layer_chunk_plan_for_chunks` plans an explicit, sorted,
   deduplicated set of chunk coordinates rather than only a box, with both planners sharing one
   address-construction helper so they cannot drift, proven by requiring plan equality over the
   same box. Page assembly is also implemented: `portable_chunk_plan_pages`
   concatenates a channel's demanded chunks in ascending chunk order, verifying each against the
   plan's page ordinal and first word and refusing a mismatched grid or an unread chunk. Admitting
   a non-level-zero source is implemented too — the session had carried a `level` field that
   nothing ever set, so the pyramid was not addressable and camera-driven level selection had
   nothing to choose between. Per-level geometry is implemented: the desktop derives each
   level's transform from that dataset's own NGFF coordinate transformations rather than level
   zero's, exposes every level's physical voxel spacing for the selection rule, and can admit the
   default layer at a chosen level with array and transform moving together — the invariant being
   that a layer's physical extent is identical at every level. On the committed anisotropic
   fixture the same pixel footprint selects level one looking along x and level two looking along
   z, which is the S5 case driving a real decision. A demand-resident level may also be admitted without its pages
   covering the volume, since readability there is the residency map's job and a frame
   legitimately begins with nothing resident; a local-adapter fixture renders exactly that
   bootstrap and converges to a frame byte-identical to the statically paged render. That fixture
   also shows the property feedback planning exists for: an opaque chunk early-terminates the ray
   at the 0.95 threshold, so the chunks behind it are never requested, which a region-based plan
   cannot express. The desktop now renders from that chain:
   `render_demand_driven_scene_camera_draw` takes no chunk region, and three requests carrying
   different regions produce byte-identical frames. Several demand-resident channels can now share the
   single page-table binding without colliding: `PortableResidencyTag` composes the channel
   ordinal and level into the key's seven-bit level field, four channels at thirty-two levels
   filling it exactly, with a local-adapter fixture proving two co-registered channels render what
   the statically paged composite renders. The desktop route now admits one to four channels, each with
   its own residency loop, tag and owner range sharing one page table and one four-page budget;
   a committed synthetic two-channel fixture now exercises it
   end to end. Reaching that required decoupling the camera from Palace's three-dimensional file
   open, which no dataset with a channel axis can pass: `camera_ray_for_geometry` fits the
   identical camera from ZYX dimensions and spacing alone. Camera-driven level selection now drives the desktop
   frame: the footprint is measured between two neighbouring centre pixels where the centre ray
   enters the volume, and a coarse level renders end to end through its own source array,
   transform and chunk grid. Reading a level needs no session mutation, because a level's array and
   transform are pure functions of the metadata and the level index. Measured in a release build on the committed fixture the
   route is interactive: about 19 ms for a 256x192 frame, having fitted the camera once per frame
   rather than once per pixel; the same frame takes 552 ms in a debug build, so any performance
   claim must name the profile.

   A side-by-side comparison against the native compositor then found that portable DVR corrected
   opacity in the wrong units: Palace's raycaster normalizes its step by the volume's physical
   diagonal before the 256x reference correction, while the portable path fed the physical step in
   directly, overstating the exponent by the whole diagonal and saturating the image. Every DVR
   entry point now takes an explicit `opacity_reference`, with the correction
   `step_size / opacity_reference` and callers passing `diagonal / 256`; it cannot be folded into
   the step size, which must stay physical because it also sets the marching distance. The bug
   survived because every fixture used a step of `1/256`, the one value at which the exponent is
   `1.0` regardless of the reference. The residual difference from the native compositor is that
   renderer's fixed `0.06` blend factor and `0.01` first-opacity threshold, both already designated
   legacy behaviour here.

   Restrictions remain and are stated rather than claimed away — one
   layer only, which is unreachable from the desktop session in any case, and, until now, one
   adapter acquisition per demand iteration. That last one is fixed: every portable desktop route shares one
   session-owned WGPU device, five acquisition sites became one, and two routes that were
   evaluating their CPU oracle before attempting the GPU now use it only as a fallback. The cache
   is proven by counting acquisitions rather than by timing. The device is owned by the session
   rather than a process-wide static on purpose: a static is never dropped, and a leaked device
   leaves the graphics driver's background threads alive at exit, which segfaulted the process on
   roughly a third of runs until the scope was corrected. An interactive per-frame measurement of the demand route
   remains unmeasured. Normal Palace planning is
   camera-driven level selection plus a request/use feedback loop — `imageviewer::view_image`
   selects its level through `sliceviewer::select_level`, then re-renders until the request table
   reports `Done`. On the portable path the scene shader has no page table to miss against and no
   request buffer to write misses into, and there is no camera-driven level selection at all
   (`select_level` is 2D-oriented). Until demand comes from the renderer rather than the webview,
   this item is not complete.
5. **Operator migration.** Retain the explicit portable floor-nearest API. Add a separate,
   transform-compatible path only after centre rounding, affine coordinates and border policy are
   encoded explicitly; never silently redirect `resample_transform`. Grow rechunk toward streaming
   multi-page windows while retaining zero-padding semantics.

   The rechunk half is implemented. `PortableRechunkStream` splits a rechunk along the slowest axis
   into page-bounded windows, each an ordinary `PortableRechunkLayout`, so the existing
   fixed-binding kernel serves sources far past its former ~1M-voxel input cap. The slow axis keeps
   both the input and output runs of every window contiguous; a fast-axis split would be strided
   and is refused rather than silently substituted. Fast-axis zero padding stays inside each
   window's memory extent and split-axis padding is reported separately for the caller to fill, so
   the semantics remain explicit rather than emergent. `resample_transform` is untouched.

   The resample half is implemented as a separate contract, `PortableAffineResampleLayout`: an
   explicit row-major affine matrix on global output coordinates, `floor(p + 0.5)` rounding
   (written out because GLSL's `round` is unspecified on ties and WGSL's is half-to-even), and a
   named `Repeat`/`Pad0` border. It has a CPU oracle, a fixed-binding WGPU kernel, an opt-in
   operator bridge (`portable_resample_affine_cpu`), and a test holding the oracle to Vulkan's
   `resample_transform` on a centre-based rescale and a translation under both borders. Nothing is
   redirected: the floor-nearest layout keeps its callers and `resample_transform` keeps the LOD
   builder. `select_resample_transform` is the scheduler's choice — the portable affine path for
   an admitted lossless page, Vulkan otherwise — and `py-palace`'s `resample_transform` goes
   through it as `rechunk` already did. Evidence in STAGE0 "Resample, the transform-compatible
   half" and "Scheduler selection of the affine resample". Palace's `from_uniform` now rounds
   its 8-bit DVR state, so the Vulkan path is a colour reference again (STAGE0 "Palace rounds
   its 8-bit state").
6. **Desktop/server selection.** Select the portable route only for admitted packets and retain
   current Vulkan fallback. Preserve PNG+PFM transport, picker ray/depth conversion, bounded
   envelopes, and the browser's independent camera/depth authority.

   Audited. The desktop selects the portable route for admitted packets on the scene, direct and
   orthogonal routes and falls back otherwise; on each of the scene and direct routes the render
   command and the pick command now obtain their frame from one function
   (`scene_route_frame`, `direct_route_frame`), so selection is occluded by the displayed
   surface by construction, and the direct route's transported depth is physical whichever
   renderer produced it. The shipped UI's volume canvas now drives the scene route for both
   rendering and picking, so the demand-driven frame is what users see. the server never selects it, calling only the
   legacy Vulkan entry points; the browser retains independent authority because the server serves
   it chunks rather than frames. The audit also found that the paired first-opacity attachment was
   **empty for every real dataset** — a local OME-Zarr painted colour while every distance was
   `+infinity`, where the procedural ball through the same reader did not. That broke the PNG+PFM
   contract and the picker's Vulkan fallback and was pre-existing. **Fixed in `raycaster.glsl`:**
   the main sampling branch recorded the ray distance after `update_state` had replaced `t` with
   the NaN `T_DONE` sentinel on saturation, so any transfer that saturates on its first
   contributing sample lost the distance; the sample distance is now captured first, as the
   constant-chunk branch already did. Guarded by an un-ignored palace-frame regression test on a
   real dataset and by the server fixture test, which now asserts finite distances rather than a
   PFM header; both fail with the fix reverted. This is the first correctness fix this migration
   has made to the Vulkan raycaster itself, and it matters for the portable move: the paired-depth
   ownership rule (§ Palace depth row) is only worth preserving if the owner writes real values.

   The Vulkan comparison for DVR now exists (`compare_desktop_portable_and_server_renderers`,
   matched transfer, unshaded, level zero pinned on both sides through
   `palace_frame::CameraRenderOptions`) and found three defects outside the portable compositor:
   the Vulkan attachment was measured from the entry face rather than the camera (the entry/exit
   pass now carries each entry's camera distance), the desktop treated Palace's physical camera
   ray as voxel coordinates in both portable routes (fixed; the level-selection test re-derived
   on the corrected camera), and the Vulkan DVR composited exactly one sample per ray because
   the depth guard short-circuited `update_state` (fixed). With those in, the two renderers find
   volume on the same rays, agree on depth to a constant explained by Palace skipping its on-face
   sample, and agree on opacity within 3%; Palace's 8-bit truncating state biases its hue and is
   recorded as a Vulkan property. Evidence in STAGE0 "The Vulkan comparison, and what it found".

Each phase requires CPU-oracle tests, local WGPU adapter tests, and a Vulkan comparison where a
legacy equivalent exists. The final local gate adds comparison fixtures plus desktop and server
smokes. Apple Silicon, Windows/D3D12, authenticated S3/SSH, and production-scale runs remain
deferred acceptance gates, not prerequisites for these phases.

### 9.4 Two ordering subtleties worth stating explicitly

**Annotations are deliberately late, and that is not a deferral of the requirement.** 3D
annotation rendering must composite depth-correctly with the volume, which means living in
the same GPU context as palace's renderer. Writing it before Stage 6 means writing it twice —
once for Vulkan, once for wgpu. Meanwhile **2D annotation overlay is available from Stage 2
in the UI layer**, drawn over the frame on a canvas, exactly as `omezarr_viewers-rs` does
today. So users get annotations early; only the depth-correct 3D compositing waits.

**The wgpu brick pool is Stage 6, not Stage 9.** palace's buffer-per-chunk Vulkan model relies
on device addresses and cannot port unchanged. S7 chooses a bounded portable pool and Stage 6
implements it as part of the backend. A later atlas conversion, if S7 selected storage-buffer
pages, is a performance change with its own upstream conversation.

### 9.5 Parallel tracks

If more than one person is working, these are largely independent:

| Track | Stages | Depends on |
|---|---|---|
| **Data** — NGFF, sources, pyramid builder | 1, 3 | nothing |
| **UI** — Leptos, Tauri, layout, tools | 2 | a frame source (S4 spike unblocks it) |
| **palace backend** — seam, wgpu, shaders | 5, 6, 7 | S3 spike |
| **Annotations** | 8 | Stage 6 for 3D; nothing for the 2D overlay |

The critical path to a demonstrable product runs **S1 + S4 + S8 → Stage 1 → Stage 2**. Remote
data and pyramid generation follow in Stage 3; none of the demonstrable-product path requires
the wgpu port.

---

## 10. Known problems that will bite

1. **Neither `zarrs_ome` nor `bioformats2raw` can build anisotropic pyramids.** `zarrs_ome`
   applies one fixed factor vector at every level (its `Downsample` filter is constructed
   before the level loop); `bioformats2raw` hardcodes `scaledDepth = sizeZ`, so Z is never
   downsampled, and `--chunk-depth` defaults to 1. **We must ship our own pyramid builder.**
   Port BigDataViewer's `ProposeMipmaps`: normalise voxel size to the finest axis, halve only
   axes whose normalised extent is ≤ 2.0, re-normalise, terminate at maxSize ≤ 256. For 10:1
   data this gives `[1,1,1] → [2,2,1] → [4,4,1] → [8,8,1] → [16,16,2] → [32,32,4]`; Z lags XY
   by ⌊log₂(anisotropy)⌋ levels then tracks it.
   Palace's `Hints::lod_downsample_steps` is a single per-axis `Vector<DDyn, DownsampleStep>`
   reused at every level by both dynamic LOD creation and `save_lod_tensor`, so it has the same
   defect. **Useful corollary:** above the crossover level voxels are world-cubic, so the brick
   sees cubic bricks at every level above 3–4. Only the bottom few levels need
   anisotropy-aware screen-space-error handling.
2. **WASM decode throughput is unmeasured by anyone.** The incumbent JS Zarr stack
   (`numcodecs.js`, shared by Neuroglancer, zarr.js and zarrita.js) builds with `-Os` and
   **no `simd128`**, with Blosc's SSE2/AVX2 shuffle explicitly disabled. Rust with
   `+simd128` should beat it — but that is a hypothesis on the critical path.
3. **The GLSL→WGSL rewrite**, and `jit.rs` in particular, is the largest single work item.
4. **Two rendering codepaths must agree**, or users see different images depending on
   deployment. Mitigation: CPU "oracle" implementations for differential testing (an idea
   worth stealing from `kirchhausenlab/mirante4d`), plus golden-image tests on both paths.
5. **palace's least-tested code is our critical path**: anisotropic spacing, and `palace-zarr`
   (which postdates the paper entirely).
6. **Colorspace and HiDPI.** Compute shaders cannot write sRGB storage textures. Decide
   explicitly whether to write linear to `Rgba16Float` and apply the OETF in a blit, or write
   encoded values to `Rgba8Unorm`. Size offscreen targets in *physical* pixels (4× on a
   Retina display), which makes render resolution a decoupled parameter with progressive
   refinement — needed anyway.
7. **Frame encoding cost** if server-side rendering re-encodes every refinement pass. Encode
   at render resolution, not display resolution.

---

## 11. Open questions

1. Should newvolim host palace as a path dependency during development, or track the fork by
   git revision? (Path is simpler while both move.)
2. Does the existing viewer's `convert.rs` produce isotropic pyramids? If so it has the same
   defect as `bioformats2raw`, and since we control that writer it is the natural first fix.
3. COOP/COEP: accept the deployment cost for multi-threaded wasm decode, or stay
   single-threaded? Depends on the benchmark in Stage 0.
4. How much of `omezarr_viewers-rs` is ported versus kept running alongside? The plan assumes
   porting the model and the desktop/SSH flow, but its server could keep serving 2D tiles,
   annotations and object tables during the transition.
5. Do we need interactive out-of-core *processing* (palace's random walker, vesselness) in
   the product, or only visualization? If yes, palace's operator framework becomes a major
   asset and the balance of effort shifts.
6. Upstream relationship with ftilde: worth opening a conversation before **Stage 3** (the
   first PR, runtime-registered sources) and certainly before **Stage 5** (the backend seam),
   since agreement on the seam determines how PR-able everything after it is. The reordering
   in §9 helps here — the source-registration PR is small and uncontroversial, which is a
   better first contact than a 281-site refactor.
