//! The newvolim web interface.
//!
//! A CSR Leptos page in the style of `omezarr_viewers-rs`: a full-bleed viewer (the 2×2 slice
//! grid with an orientation box, or one pane alone), floating tool strips, and a sidebar of
//! per-layer cards. It talks to `newvolim-server` only — the datasets list, the frame socket
//! for volume and orthogonal frames, the channel and layer routes — and can render the volume
//! itself on WebGPU from the server's scene packet (`scene-webgpu.js`).
//!
//! `api` and `cube` are pure and compile natively, so their tests run with the workspace;
//! everything that touches the DOM is `wasm32`-only.

pub mod api;
pub mod cube;

#[cfg(target_arch = "wasm32")]
mod app;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn mount() {
    std::panic::set_hook(Box::new(|info| {
        web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(&info.to_string()));
    }));
    leptos::mount::mount_to_body(app::App);
}
