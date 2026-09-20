# TODO2 — gaps against the aim (2026-09-19)

The aim: a visualizer for volume images stored as OME-Zarr on disk or S3, with a renderer that
runs both server-side and client-side, where the client is either a Tauri app or a WebAssembly
web interface.

Measured against that, the renderer itself is in good shape (see HANDOVER.md); the gaps are at
the edges. This list comes from what was actually seen in the code during the 2026-09-19
session, not from a full audit.

## Not built

1. **S3 sources.** Everything opens a local path (`open_local_omezarr`, `LocalOmeZarrSource`,
   `load_local_zarr_volume`). PLAN.md lists "authenticated S3/SSH" as a deferred acceptance
   gate, and nothing in the session, server or UI takes a remote URL. For a viewer whose data
   lives on S3 this is the biggest hole. *2026-09-19 evening:* chunk decoding now goes through
   `zarrs` (compressed v2/v3 stores, NGFF 0.5 roots, nested paths — STAGE0 "Compressed and
   NGFF 0.5 stores read through `zarrs`"), so remote access is a store-backend change
   (`zarrs_opendal`/`zarrs_object_store`), not a reader rewrite. Two public stores are
   mirrored locally under `/big/henriksson/omezarr-public/` and render.
2. ~~**Client-side rendering of the whole volume in the browser.**~~ Done 2026-09-19 for
   rendering (STAGE0 "The browser renders the whole scene itself"): the page dispatches the
   desktop's scene shader over the server-packed full-level inputs (`/portable/scene`), pixel
   for pixel the desktop's frame on the local adapter. Not done: client-side residency (the
   feedback loop and chunk fetching in the page), and picking in the browser against that
   frame. *(The earlier text saying the browser only drew a synthetic preview was wrong; it
   rendered one chunk region per layer with its own shader.)*
3. ~~**Transfer-function control.**~~ Done 2026-09-19 (STAGE0 "Transfer-function control"):
   session API, desktop commands, page panel; and on the server, `GET/POST
   /v1/datasets/{dataset}/channels` plus a frame-socket edit message.

*2026-09-20:* the page itself was rebuilt (STAGE0 "The web interface, rebuilt"): grid of
slices + 3D + orientation box, layer cards, server or browser rendering. Later the same day:
client-side residency (STAGE0 "Client-side residency") — the page runs the demand loop and
fetches only missed chunks. Still open in the browser: rays generated locally, picking,
annotations (no server API), labels layers. **New gap:** the Tauri
desktop no longer has a backend behind the page until it hosts the server in-process.

## Built but not reachable from where the aim needs it

4. ~~**Server-side portable renderer.**~~ Done 2026-09-19 (STAGE0 "The portable host is a
   library, and the server renders through it"): `newvolim-portable` is shared by desktop and
   server; a server volume frame is `scene_route_frame` over a per-dataset session, Vulkan only
   as fallback. Orthogonal socket frames still go through Vulkan.
5. ~~**Multi-layer scenes.**~~ Done 2026-09-19 for image layers (STAGE0 "Multi-layer scenes"):
   `add_portable_image_layer` binds a further OME-Zarr to its own layer, the demand route
   renders every visible image layer at its own level, desktop and server expose it. Labels
   layers over an image remain: the scene has the kind, no route renders it.

## Built but unverified where it matters

6. **Other platforms.** Browser, Apple and Windows/D3D12 adapters have never been run — every
   adapter test so far is the one Linux host.
7. **Scale.** The demand route is bounded by four 4 MiB static pages. *2026-09-20:* it now
   coarsens the level until the working set fits instead of falling back to Vulkan, so a large
   pane over a large volume renders at a coarser level (IDR 6001240 at 1120×810: ~1 s, release).
   A finer level for the visible part only (true demand residency) is still not built.

## Defects noticed in passing, not fixed

- `LocalSession::import_annotations` replaces the whole scene, dropping the image layer.
- The page's chunk-region plumbing (`newvolimNativePortableRegion`) is inert now that the volume
  canvas uses the scene route, and should be removed.
- The browser's one-chunk raw preview and `GET /v1/datasets/{d}/zarr/{asset}` serve stored bytes
  verbatim; on a compressed store the preview would decode nothing. The scene packet route is
  unaffected (the server decodes).

## Suggested order for the stated aim

S3 (1) → transfer-function control (3) → server-side portable route (4) → browser client (2).
The browser needs S3/server streaming and a portable renderer that already runs server-side
before it is worth wiring. Items 5–7 follow once real data flows end to end.
