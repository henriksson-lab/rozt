//! CSR-only Leptos shell shared by the browser and Tauri desktop webview.

#[cfg(target_arch = "wasm32")]
use leptos::prelude::*;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsValue;

/// Browser-facing wrapper around the Rust SIMD-capable Zstd chunk decoder. Store/codec-chain
/// validation remains in the JavaScript NGFF boundary; this export accepts only encoded bytes.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn decode_zstd_chunk(bytes: js_sys::Uint8Array) -> Result<js_sys::Uint8Array, JsValue> {
    let decoded = newvolim_decode_bench::decode_zstd_chunk(&bytes.to_vec())
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
    Ok(js_sys::Uint8Array::from(decoded.as_slice()))
}

/// Browser decoder with an explicit decompression budget. Direct OME-Zarr preview passes its
/// declared chunk size here, so a compressed payload cannot allocate past the fixed page pool.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn decode_zstd_chunk_bounded(
    bytes: js_sys::Uint8Array,
    max_decoded_bytes: u32,
) -> Result<js_sys::Uint8Array, JsValue> {
    let decoded = newvolim_decode_bench::decode_zstd_chunk_bounded(
        &bytes.to_vec(),
        max_decoded_bytes as usize,
    )
    .map_err(|error| JsValue::from_str(&error.to_string()))?;
    Ok(js_sys::Uint8Array::from(decoded.as_slice()))
}

/// Browser-facing wrapper around the size-prepended LZ4 decoder used by the current corpus.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn decode_lz4_chunk(bytes: js_sys::Uint8Array) -> Result<js_sys::Uint8Array, JsValue> {
    let decoded = newvolim_decode_bench::decode_lz4_chunk(&bytes.to_vec())
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
    Ok(js_sys::Uint8Array::from(decoded.as_slice()))
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn decode_lz4_chunk_bounded(
    bytes: js_sys::Uint8Array,
    max_decoded_bytes: u32,
) -> Result<js_sys::Uint8Array, JsValue> {
    let decoded = newvolim_decode_bench::decode_lz4_chunk_bounded(
        &bytes.to_vec(),
        max_decoded_bytes as usize,
    )
    .map_err(|error| JsValue::from_str(&error.to_string()))?;
    Ok(js_sys::Uint8Array::from(decoded.as_slice()))
}

/// Browser-facing wrapper around the portable Blosc decoder.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn decode_blosc_chunk(bytes: js_sys::Uint8Array) -> Result<js_sys::Uint8Array, JsValue> {
    let decoded = newvolim_decode_bench::decode_blosc_chunk(&bytes.to_vec())
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
    Ok(js_sys::Uint8Array::from(decoded.as_slice()))
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn decode_blosc_chunk_bounded(
    bytes: js_sys::Uint8Array,
    max_decoded_bytes: u32,
) -> Result<js_sys::Uint8Array, JsValue> {
    let decoded = newvolim_decode_bench::decode_blosc_chunk_bounded(
        &bytes.to_vec(),
        max_decoded_bytes as usize,
    )
    .map_err(|error| JsValue::from_str(&error.to_string()))?;
    Ok(js_sys::Uint8Array::from(decoded.as_slice()))
}

/// Mounts the static UI shell. Rendering arrives through the canvas transport; UI components
/// deliberately do not compile as a native server-rendered application.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn mount() {
    leptos::mount::mount_to_body(App);
}

#[cfg(target_arch = "wasm32")]
#[component]
fn App() -> impl IntoView {
    view! {
        <main class="newvolim-shell">
            <header>
                <h1>"newvolim"</h1>
                <span id="newvolim-render-status" class="render-status" aria-live="polite">"Renderer not connected"</span>
            </header>
            <section class="source-controls" aria-label="Local data source">
                <input id="newvolim-local-path" type="text" placeholder="/path/to/dataset.ome.zarr" aria-label="Local OME-Zarr path"/>
                <button on:click=move |_| window_newvolim_open_and_render()>"Open local dataset"</button>
                <button on:click=move |_| window_newvolim_render_synthetic()>"Render synthetic volume"</button>
                <button on:click=move |_| window_newvolim_add_point_annotation()>"Add point at crosshair"</button>
                <button on:click=move |_| window_newvolim_begin_polygon_annotation()>"Start polygon ROI"</button>
                <button on:click=move |_| window_newvolim_finish_polygon_annotation()>"Finish polygon ROI"</button>
                <button on:click=move |_| window_newvolim_begin_rectangle_annotation()>"Start rectangle ROI"</button>
                <button on:click=move |_| window_newvolim_finish_rectangle_annotation()>"Finish rectangle ROI"</button>
                <button on:click=move |_| window_newvolim_begin_ellipse_annotation()>"Start ellipse ROI"</button>
                <button on:click=move |_| window_newvolim_finish_ellipse_annotation()>"Finish ellipse ROI"</button>
                <button on:click=move |_| window_newvolim_refresh_annotations()>"Refresh annotations"</button>
            </section>
            <section class="source-controls remote-controls" aria-label="Remote frame source">
                <input id="newvolim-remote-url" type="url" placeholder="ws://host:port/v1/frames" aria-label="Remote frame WebSocket URL"/>
                <input id="newvolim-remote-dataset" type="text" placeholder="configured server dataset name" aria-label="Remote OME-Zarr dataset name"/>
                <button on:click=move |_| window_newvolim_open_remote_frame_server()>"Open remote frame server"</button>
            </section>
            <section class="source-controls remote-controls" aria-label="Direct browser OME-Zarr source">
                <input id="newvolim-browser-zarr-url" type="url" placeholder="https://host/dataset.ome.zarr/" aria-label="Browser OME-Zarr URL"/>
                <input id="newvolim-browser-zarr-level" type="number" min="0" step="1" value="0" aria-label="Browser OME-Zarr multiscale level"/>
                <input id="newvolim-browser-zarr-chunk" type="text" value="0,0,0" aria-label="Browser OME-Zarr one to four semicolon-separated Z,Y,X chunk coordinates"/>
                <button on:click=move |_| window_newvolim_open_browser_omezarr()>"Preview browser OME-Zarr chunk"</button>
            </section>
            <section class="source-controls remote-controls" aria-label="Browser chunk server source">
                <input id="newvolim-browser-chunk-server-url" type="url" placeholder="server URL (empty = this page's origin)" aria-label="Browser chunk server URL"/>
                <input id="newvolim-browser-chunk-server-dataset" type="text" placeholder="configured dataset name" aria-label="Browser chunk server dataset name"/>
                <button on:click=move |_| window_newvolim_discover_browser_datasets()>"Discover server datasets"</button>
                <button on:click=move |_| window_newvolim_open_browser_server_dataset()>"Preview server dataset"</button>
                <button on:click=move |_| window_newvolim_render_server_scene()>"Render server dataset here (WebGPU)"</button>
            </section>
            <section class="annotation-list" aria-label="Annotations">
                <h2>"Annotations"</h2>
                <input id="newvolim-annotation-document" type="text" placeholder="/path/to/annotations.newvolim.json" aria-label="Annotation document path"/>
                <button on:click=move |_| window_newvolim_export_annotations()>"Export annotations"</button>
                <button on:click=move |_| window_newvolim_import_annotations()>"Import annotations"</button>
                <ul id="newvolim-annotations"></ul>
            </section>
            <section class="channels" aria-label="Channel transfer functions">
                <input id="newvolim-layer-path" type="text" placeholder="/path/to/another.ome.zarr" aria-label="Additional OME-Zarr layer path"/>
                <button on:click=move |_| window_newvolim_add_layer()>"Add layer from OME-Zarr"</button>
                <div id="newvolim-channels" class="channel-panel"></div>
            </section>
            <section class="viewport-grid" aria-label="Volume views">
                <canvas id="newvolim-xy" aria-label="XY view"></canvas>
                <canvas id="newvolim-xz" aria-label="XZ view"></canvas>
                <canvas id="newvolim-yz" aria-label="YZ view"></canvas>
                <canvas id="newvolim-volume" aria-label="3D volume view"></canvas>
            </section>
        </main>
    }
}

#[cfg(target_arch = "wasm32")]
fn window_newvolim_open_and_render() {
    invoke_page_function("newvolimOpenAndRender");
}

#[cfg(target_arch = "wasm32")]
fn window_newvolim_render_synthetic() {
    invoke_page_function("newvolimRenderSynthetic");
}

#[cfg(target_arch = "wasm32")]
fn window_newvolim_add_point_annotation() {
    invoke_page_function("newvolimAddPointAnnotation");
}

#[cfg(target_arch = "wasm32")]
fn window_newvolim_begin_polygon_annotation() {
    invoke_page_function("newvolimBeginPolygonAnnotation");
}

#[cfg(target_arch = "wasm32")]
fn window_newvolim_finish_polygon_annotation() {
    invoke_page_function("newvolimFinishPolygonAnnotation");
}

#[cfg(target_arch = "wasm32")]
fn window_newvolim_begin_rectangle_annotation() {
    invoke_page_function("newvolimBeginRectangleAnnotation");
}

#[cfg(target_arch = "wasm32")]
fn window_newvolim_finish_rectangle_annotation() {
    invoke_page_function("newvolimFinishRectangleAnnotation");
}

#[cfg(target_arch = "wasm32")]
fn window_newvolim_begin_ellipse_annotation() {
    invoke_page_function("newvolimBeginEllipseAnnotation");
}

#[cfg(target_arch = "wasm32")]
fn window_newvolim_finish_ellipse_annotation() {
    invoke_page_function("newvolimFinishEllipseAnnotation");
}

#[cfg(target_arch = "wasm32")]
fn window_newvolim_refresh_annotations() {
    invoke_page_function("newvolimRefreshAnnotations");
}

#[cfg(target_arch = "wasm32")]
fn window_newvolim_add_layer() {
    invoke_page_function("newvolimAddLayer");
}

#[cfg(target_arch = "wasm32")]
fn window_newvolim_render_server_scene() {
    invoke_page_function("newvolimRenderServerScene");
}

#[cfg(target_arch = "wasm32")]
fn window_newvolim_export_annotations() {
    invoke_page_function("newvolimExportAnnotations");
}

#[cfg(target_arch = "wasm32")]
fn window_newvolim_import_annotations() {
    invoke_page_function("newvolimImportAnnotations");
}

#[cfg(target_arch = "wasm32")]
fn window_newvolim_open_remote_frame_server() {
    invoke_page_function("newvolimOpenRemoteFrameServer");
}

#[cfg(target_arch = "wasm32")]
fn window_newvolim_open_browser_omezarr() {
    invoke_page_function("newvolimOpenBrowserOmeZarr");
}

#[cfg(target_arch = "wasm32")]
fn window_newvolim_discover_browser_datasets() {
    invoke_page_function("newvolimDiscoverBrowserDatasets");
}

#[cfg(target_arch = "wasm32")]
fn window_newvolim_open_browser_server_dataset() {
    invoke_page_function("newvolimOpenBrowserServerDataset");
}

#[cfg(target_arch = "wasm32")]
fn invoke_page_function(name: &str) {
    use wasm_bindgen::JsCast;

    let window = web_sys::window().expect("CSR UI has a browser window");
    let function = js_sys::Reflect::get(&window, &name.into())
        .expect("page function lookup should not throw")
        .dyn_into::<js_sys::Function>()
        .expect("page function must be callable");
    function
        .call0(&window)
        .expect("page function should not throw");
}

/// Native builds intentionally expose no UI entry point: this crate is CSR-only and is served
/// by Tauri or a static-file server, never by Leptos SSR.
#[cfg(not(target_arch = "wasm32"))]
pub fn csr_only() {}
