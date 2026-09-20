//! The page: session state, the server connection, and the components.
//!
//! Layout follows `omezarr_viewers-rs`: a tab strip, then the viewer shell (floating tool
//! strip, the 2×2 grid of XY / XZ / YZ slices and the orientation box — or one pane alone —
//! axis sliders, a status line) beside a sidebar of layer cards. All state is a set of signals
//! in [`Session`], and every server interaction is latest-only: while a frame is in flight a
//! newer camera or crosshair only marks the kind dirty, and the reply triggers the next
//! request, so the page never queues more work than the renderer can drain.

use std::cell::RefCell;
use std::rc::Rc;

use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

use crate::api::*;
use crate::cube::CubeView;

const MAX_FRAME_SIDE: u32 = 4096;
const MAX_ORBIT: i32 = 10_000;
const ZOOM_RANGE: (f32, f32) = (0.25, 4.0);

#[wasm_bindgen]
extern "C" {
    /// `scene-webgpu.js`: one pass of the scene shader over packed inputs; resolves to
    /// `{ output, requests }`.
    #[wasm_bindgen(js_namespace = ["window", "newvolimSceneWebGpu"], js_name = dispatch, catch)]
    async fn scene_webgpu_dispatch(
        pages: js_sys::Array,
        scene_data: js_sys::Uint32Array,
        rays: js_sys::Uint32Array,
        params: js_sys::Uint32Array,
        request_capacity: u32,
        output_words: u32,
        workgroups: u32,
    ) -> Result<JsValue, JsValue>;
    /// `scene-webgpu.js`: blit RGBA bytes to the canvas.
    #[wasm_bindgen(js_namespace = ["window", "newvolimSceneWebGpu"], js_name = present, catch)]
    async fn scene_webgpu_present(
        canvas: &web_sys::HtmlCanvasElement,
        rgba: js_sys::Uint8ClampedArray,
        width: u32,
        height: u32,
    ) -> Result<(), JsValue>;
}

/// The chunk cache the browser renderer keeps across camera moves: 64 M words, 256 MiB.
const CHUNK_CACHE_WORDS: usize = 64 * 1024 * 1024;
/// Chunks per request to the chunk route (the server's bound).
const CHUNKS_PER_REQUEST: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewMode {
    Grid,
    Xy,
    Xz,
    Yz,
    Volume,
}

impl ViewMode {
    /// The one slice plane shown alone, if this mode is a single slice.
    pub fn single_plane(self) -> Option<Plane> {
        match self {
            ViewMode::Xy => Some(Plane::Xy),
            ViewMode::Xz => Some(Plane::Xz),
            ViewMode::Yz => Some(Plane::Yz),
            ViewMode::Grid | ViewMode::Volume => None,
        }
    }
    pub fn shows_plane(self, plane: Plane) -> bool {
        match self {
            ViewMode::Grid => true,
            ViewMode::Volume => false,
            single => single.single_plane() == Some(plane),
        }
    }
    pub fn shows_volume(self) -> bool {
        matches!(self, ViewMode::Grid | ViewMode::Volume)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Renderer {
    Server,
    Browser,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub orbit_x: i32,
    pub orbit_y: i32,
    pub zoom: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self { orbit_x: 0, orbit_y: 0, zoom: 1.0 }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Slices {
    pub xy: String,
    pub xz: String,
    pub yz: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Plane {
    Xy,
    Xz,
    Yz,
}

impl Plane {
    /// The voxel axes drawn horizontally and vertically, and the one the plane cuts.
    pub fn axes(self) -> (usize, usize, usize) {
        match self {
            Plane::Xy => (0, 1, 2),
            Plane::Xz => (0, 2, 1),
            Plane::Yz => (1, 2, 0),
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Plane::Xy => "XY",
            Plane::Xz => "XZ",
            Plane::Yz => "YZ",
        }
    }
}

struct Socket {
    ws: web_sys::WebSocket,
    open: bool,
    queued: Vec<String>,
    _closures: Vec<Closure<dyn FnMut(JsValue)>>,
}

/// Everything the page knows, as signals; `Copy` so every component and closure can hold it.
#[derive(Clone, Copy)]
pub struct Session {
    pub origin: RwSignal<String>,
    pub origin_entry: RwSignal<String>,
    pub datasets: RwSignal<Vec<String>>,
    pub dataset: RwSignal<Option<String>>,
    pub layers: RwSignal<Vec<LayerChannelSummary>>,
    pub voxel_shape: RwSignal<Option<[u32; 3]>>,
    pub crosshair: RwSignal<[u32; 3]>,
    pub camera: RwSignal<Camera>,
    pub view_mode: RwSignal<ViewMode>,
    pub renderer: RwSignal<Renderer>,
    pub slices: RwSignal<Option<Slices>>,
    pub volume_png: RwSignal<Option<String>>,
    pub status: RwSignal<String>,
    pub error: RwSignal<Option<String>>,
    /// A sticky note in the status line (why the renderer fell back), cleared on the next
    /// renderer or view change.
    pub notice: RwSignal<Option<String>>,
    pub panel_open: RwSignal<bool>,
    pub busy: RwSignal<bool>,
    socket: StoredValue<Option<Rc<RefCell<Socket>>>, LocalStorage>,
    next_request: StoredValue<u64>,
    volume_inflight: StoredValue<Option<u64>>,
    volume_dirty: StoredValue<bool>,
    ortho_inflight: StoredValue<Option<u64>>,
    ortho_dirty: StoredValue<bool>,
    browser_busy: StoredValue<bool>,
    browser_dirty: StoredValue<bool>,
    channel_inflight: StoredValue<bool>,
    channel_dirty: StoredValue<Option<ChannelEdit>>,
    /// The browser renderer's chunk cache, kept across frames (taken during a frame).
    chunk_cache: StoredValue<Option<newvolim_residency::ChunkCache>>,
    volume_canvas: NodeRef<leptos::html::Canvas>,
    volume_pane: NodeRef<leptos::html::Div>,
    /// The XY, XZ and YZ panes; the first visible one sizes the orthogonal request.
    ortho_panes: [NodeRef<leptos::html::Div>; 3],
}

impl Session {
    fn new() -> Self {
        Self {
            origin: RwSignal::new(String::new()),
            origin_entry: RwSignal::new(String::new()),
            datasets: RwSignal::new(Vec::new()),
            dataset: RwSignal::new(None),
            layers: RwSignal::new(Vec::new()),
            voxel_shape: RwSignal::new(None),
            crosshair: RwSignal::new([0; 3]),
            camera: RwSignal::new(Camera::default()),
            view_mode: RwSignal::new(ViewMode::Grid),
            renderer: RwSignal::new(Renderer::Server),
            slices: RwSignal::new(None),
            volume_png: RwSignal::new(None),
            status: RwSignal::new("Not connected".into()),
            error: RwSignal::new(None),
            notice: RwSignal::new(None),
            panel_open: RwSignal::new(true),
            busy: RwSignal::new(false),
            socket: StoredValue::new_local(None),
            next_request: StoredValue::new(1),
            volume_inflight: StoredValue::new(None),
            volume_dirty: StoredValue::new(false),
            ortho_inflight: StoredValue::new(None),
            ortho_dirty: StoredValue::new(false),
            browser_busy: StoredValue::new(false),
            browser_dirty: StoredValue::new(false),
            channel_inflight: StoredValue::new(false),
            channel_dirty: StoredValue::new(None),
            chunk_cache: StoredValue::new(None),
            volume_canvas: NodeRef::new(),
            volume_pane: NodeRef::new(),
            ortho_panes: [NodeRef::new(), NodeRef::new(), NodeRef::new()],
        }
    }

    fn fail(self, message: impl Into<String>) {
        let message = message.into();
        web_sys::console::error_1(&JsValue::from_str(&message));
        self.error.set(Some(message));
    }

    fn update_busy(self) {
        let busy = self.volume_inflight.get_value().is_some()
            || self.ortho_inflight.get_value().is_some()
            || self.browser_busy.get_value()
            || self.channel_inflight.get_value();
        self.busy.set(busy);
    }

    /// Resolve the API origin from the entry box (or the page's own origin) and list datasets.
    pub fn connect(self) {
        let location = window().location();
        let protocol = location.protocol().unwrap_or_default();
        let page_origin = location.origin().unwrap_or_default();
        let origin = match api_origin(&protocol, &page_origin, &self.origin_entry.get()) {
            Ok(origin) => origin,
            Err(message) => return self.fail(message),
        };
        self.origin.set(origin.clone());
        self.error.set(None);
        self.status.set(format!("Listing datasets at {origin}…"));
        spawn_local(async move {
            match get_json::<DatasetList>(&datasets_url(&origin)).await {
                Ok(list) => {
                    self.status.set(format!("{} datasets at {origin}", list.datasets.len()));
                    // A deep link: `?dataset=name` opens that dataset straight away.
                    let wanted = query_parameter("dataset").filter(|name| list.datasets.contains(name));
                    self.datasets.set(list.datasets);
                    if let Some(name) = wanted {
                        if self.dataset.get_untracked().is_none() {
                            self.open(name);
                        }
                    }
                }
                Err(message) => self.fail(message),
            }
        });
    }

    pub fn open(self, name: String) {
        self.close();
        self.dataset.set(Some(name.clone()));
        self.error.set(None);
        self.status.set(format!("Opening {name}…"));
        self.refresh_layers();
        self.open_socket();
        // The panes mount on the next frame; ask for the first frames at their real sizes. The
        // first orthogonal reply brings the voxel shape and a centred crosshair.
        request_animation_frame(move || {
            self.request_orthogonal();
            self.request_volume();
        });
    }

    pub fn close(self) {
        if let Some(socket) = self.socket.get_value() {
            let _ = socket.borrow().ws.close();
        }
        self.socket.set_value(None);
        self.dataset.set(None);
        self.layers.set(Vec::new());
        self.voxel_shape.set(None);
        self.slices.set(None);
        self.volume_png.set(None);
        self.camera.set(Camera::default());
        self.volume_inflight.set_value(None);
        self.ortho_inflight.set_value(None);
        self.volume_dirty.set_value(false);
        self.ortho_dirty.set_value(false);
        self.update_busy();
    }

    fn refresh_layers(self) {
        let Some(dataset) = self.dataset.get() else { return };
        let url = channels_url(&self.origin.get(), &dataset);
        spawn_local(async move {
            match get_json::<Vec<LayerChannelSummary>>(&url).await {
                Ok(layers) => self.layers.set(layers),
                Err(message) => self.fail(message),
            }
        });
    }

    fn open_socket(self) {
        let url = frames_socket_url(&self.origin.get());
        let ws = match web_sys::WebSocket::new(&url) {
            Ok(ws) => ws,
            Err(error) => return self.fail(format!("frame socket {url}: {error:?}")),
        };
        let socket = Rc::new(RefCell::new(Socket { ws: ws.clone(), open: false, queued: Vec::new(), _closures: Vec::new() }));
        let mut closures = Vec::new();

        let on_open = {
            let socket = socket.clone();
            Closure::<dyn FnMut(JsValue)>::new(move |_: JsValue| {
                let queued: Vec<String> = {
                    let mut inner = socket.borrow_mut();
                    inner.open = true;
                    std::mem::take(&mut inner.queued)
                };
                for text in queued {
                    let _ = socket.borrow().ws.send_with_str(&text);
                }
            })
        };
        ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        closures.push(on_open);

        let on_message = Closure::<dyn FnMut(JsValue)>::new(move |event: JsValue| {
            let Ok(event) = event.dyn_into::<web_sys::MessageEvent>() else { return };
            let Some(text) = event.data().as_string() else { return };
            match serde_json::from_str::<SocketReply>(&text) {
                Ok(reply) => self.handle_reply(reply),
                Err(error) => self.fail(format!("frame socket sent invalid JSON: {error}")),
            }
        });
        ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        closures.push(on_message);

        let on_close = Closure::<dyn FnMut(JsValue)>::new(move |_: JsValue| {
            if self.dataset.get_untracked().is_some() {
                self.status.set("Frame socket closed".into());
            }
            self.volume_inflight.set_value(None);
            self.ortho_inflight.set_value(None);
            self.update_busy();
        });
        ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        closures.push(on_close);

        let on_error = Closure::<dyn FnMut(JsValue)>::new(move |_: JsValue| {
            self.fail("frame socket error (is the server reachable and CORS allowed?)");
        });
        ws.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        closures.push(on_error);

        socket.borrow_mut()._closures = closures;
        self.socket.set_value(Some(socket));
    }

    fn socket_send(self, text: String) {
        let Some(socket) = self.socket.get_value() else { return };
        let mut inner = socket.borrow_mut();
        if inner.open {
            if let Err(error) = inner.ws.send_with_str(&text) {
                drop(inner);
                self.fail(format!("frame socket send: {error:?}"));
            }
        } else {
            inner.queued.push(text);
        }
    }

    fn next_request_id(self) -> u64 {
        let id = self.next_request.get_value();
        self.next_request.set_value(id + 1);
        id
    }

    /// The three slices at the visible slice pane's physical size, for the current crosshair.
    /// Nothing is requested while no slice pane is on screen.
    pub fn request_orthogonal(self) {
        let Some(dataset) = self.dataset.get_untracked() else { return };
        let mode = self.view_mode.get_untracked();
        let Some(pane_index) = [Plane::Xy, Plane::Xz, Plane::Yz].iter().position(|&plane| mode.shows_plane(plane)) else { return };
        if self.ortho_inflight.get_value().is_some() {
            self.ortho_dirty.set_value(true);
            return;
        }
        let (width, height) = self.ortho_panes[pane_index].get_untracked().map(|pane| physical_size(&pane)).unwrap_or((256, 256));
        let crosshair = self.voxel_shape.get_untracked().map(|_| self.crosshair.get_untracked());
        let camera = self.camera.get_untracked();
        let request_id = self.next_request_id();
        self.ortho_inflight.set_value(Some(request_id));
        self.ortho_dirty.set_value(false);
        self.update_busy();
        let request = FrameRequest {
            dataset,
            width,
            height,
            orbit_x: camera.orbit_x,
            orbit_y: camera.orbit_y,
            zoom: camera.zoom,
            request_id,
            view: RenderView::Orthogonal,
            x: crosshair.map(|c| c[0]),
            y: crosshair.map(|c| c[1]),
            z: crosshair.map(|c| c[2]),
        };
        self.socket_send(serde_json::to_string(&request).expect("FrameRequest serializes"));
    }

    /// The volume frame for the current camera: from the server over the socket, or rendered
    /// here from the server's scene packet.
    pub fn request_volume(self) {
        let Some(dataset) = self.dataset.get_untracked() else { return };
        if !self.view_mode.get_untracked().shows_volume() {
            return;
        }
        if self.renderer.get_untracked() == Renderer::Browser {
            return self.render_in_browser(dataset);
        }
        if self.volume_inflight.get_value().is_some() {
            self.volume_dirty.set_value(true);
            return;
        }
        let (width, height) = self.volume_pane.get_untracked().map(|pane| physical_size(&pane)).unwrap_or((256, 256));
        let camera = self.camera.get_untracked();
        let request_id = self.next_request_id();
        self.volume_inflight.set_value(Some(request_id));
        self.volume_dirty.set_value(false);
        self.update_busy();
        let request = FrameRequest {
            dataset,
            width,
            height,
            orbit_x: camera.orbit_x,
            orbit_y: camera.orbit_y,
            zoom: camera.zoom,
            request_id,
            view: RenderView::Volume,
            x: None,
            y: None,
            z: None,
        };
        self.socket_send(serde_json::to_string(&request).expect("FrameRequest serializes"));
    }

    fn render_in_browser(self, dataset: String) {
        if self.browser_busy.get_value() {
            self.browser_dirty.set_value(true);
            return;
        }
        let Some(canvas) = self.volume_canvas.get_untracked() else { return };
        let (width, height) = self.volume_pane.get_untracked().map(|pane| physical_size(&pane)).unwrap_or((256, 256));
        let camera = self.camera.get_untracked();
        let origin = self.origin.get_untracked();
        self.browser_busy.set_value(true);
        self.browser_dirty.set_value(false);
        self.update_busy();
        spawn_local(async move {
            let canvas: web_sys::HtmlCanvasElement = canvas.clone();
            let outcome = self.browser_frame(&origin, &dataset, &canvas, width, height, camera).await;
            self.browser_busy.set_value(false);
            self.update_busy();
            match outcome {
                Ok(summary) => {
                    self.error.set(None);
                    self.status.set(summary);
                    if self.browser_dirty.get_value() {
                        self.request_volume();
                    }
                }
                Err(message) => {
                    // No WebGPU, or a failed pass: say so and go back to server frames rather
                    // than leave the 3D pane empty.
                    web_sys::console::error_1(&JsValue::from_str(&message));
                    self.notice.set(Some(format!("WebGPU render failed: {message}. Showing server frames.")));
                    self.renderer.set(Renderer::Server);
                    self.request_volume();
                }
            }
        });
    }

    /// One frame by client-side residency: the plan and rays for the camera, then dispatch,
    /// read the misses, fetch only those chunks, and dispatch again until nothing is missed;
    /// a level whose working set exceeds the pages is replaced by a coarser one. The chunk
    /// cache survives into the next frame.
    async fn browser_frame(
        self,
        origin: &str,
        dataset: &str,
        canvas: &web_sys::HtmlCanvasElement,
        width: u32,
        height: u32,
        camera: Camera,
    ) -> Result<String, String> {
        use newvolim_residency::{ChunkCache, ClientResidency, ScenePlan, StepOutcome};
        install_shader()?;
        let plan: ScenePlan = get_json(&scene_plan_url(origin, dataset, width, height, camera.orbit_x, camera.orbit_y, camera.zoom, None)).await?;
        let ray_bytes = get_bytes(&scene_rays_url(origin, dataset, width, height, camera.orbit_x, camera.orbit_y, camera.zoom)).await?;
        let ray_words = words_from_le_bytes(&ray_bytes)?;
        let cache = self.chunk_cache.get_value().unwrap_or_else(|| ChunkCache::new(CHUNK_CACHE_WORDS));
        self.chunk_cache.set_value(None);
        let mut client = ClientResidency::new(plan, &ray_words, cache)?;
        let (mut passes, mut fetched, mut coarsened) = (0_usize, 0_usize, 0_usize);
        let summary = loop {
            if passes > newvolim_residency::MAX_ITERATIONS + 8 {
                self.chunk_cache.set_value(Some(client.into_cache()));
                return Err("the residency loop did not converge".into());
            }
            let dispatch = client.dispatch()?;
            let (output, requests) = dispatch_on_gpu(&dispatch).await?;
            passes += 1;
            match client.absorb_requests(&requests)? {
                StepOutcome::Complete => {
                    let pixels = width as usize * height as usize;
                    let rgba: Vec<u8> = output.iter().take(pixels).flat_map(|word| word.to_le_bytes()).collect();
                    scene_webgpu_present(canvas, js_sys::Uint8ClampedArray::from(&rgba[..]), width, height)
                        .await
                        .map_err(js_error)?;
                    let hits = output[pixels..(2 * pixels).min(output.len())].iter().filter(|bits| f32::from_bits(**bits).is_finite()).count();
                    let cache = client.cache();
                    break format!(
                        "Browser residency: levels {:?}, {passes} passes, {fetched} chunks fetched{}, {hits} of {pixels} pixels hit; cache {} chunks, {} MiB",
                        client.plan().levels,
                        if coarsened > 0 { format!(", coarsened {coarsened}×") } else { String::new() },
                        cache.len(),
                        cache.total_words() * 4 / (1024 * 1024)
                    );
                }
                StepOutcome::Planned => {
                    let missing = client.missing_chunks();
                    fetched += missing.len();
                    self.status.set(format!("Browser residency: pass {passes}, fetching {} chunks…", missing.len()));
                    fetch_chunks(origin, dataset, &mut client, &missing).await?;
                }
                StepOutcome::ExceedsPortableBound { required_pages } => {
                    let plan = client.plan();
                    let coarser = newvolim_residency::coarser_levels(&plan.levels, &plan.level_counts)
                        .ok_or_else(|| format!("the coarsest levels {:?} still need {required_pages} pages", plan.levels))?;
                    let plan: ScenePlan = get_json(&scene_plan_url(origin, dataset, width, height, camera.orbit_x, camera.orbit_y, camera.zoom, Some(&coarser))).await?;
                    client = ClientResidency::new(plan, &ray_words, client.into_cache())?;
                    coarsened += 1;
                }
            }
        };
        self.chunk_cache.set_value(Some(client.into_cache()));
        Ok(summary)
    }

    fn handle_reply(self, reply: SocketReply) {
        match reply {
            SocketReply::Frame { request_id, render_ms, data_base64, .. } => {
                if self.volume_inflight.get_value() == Some(request_id) {
                    self.volume_inflight.set_value(None);
                    self.volume_png.set(Some(format!("data:image/png;base64,{data_base64}")));
                    self.error.set(None);
                    self.status.set(format!("Server volume frame in {render_ms:.0} ms"));
                    self.update_busy();
                    if self.volume_dirty.get_value() {
                        self.request_volume();
                    }
                }
            }
            SocketReply::Orthogonal { request_id, render_ms, xy_base64, xz_base64, yz_base64, voxel_shape_xyz, crosshair_xyz, .. } => {
                if self.ortho_inflight.get_value() == Some(request_id) {
                    self.ortho_inflight.set_value(None);
                    let first = self.voxel_shape.get_untracked().is_none();
                    self.voxel_shape.set(Some(voxel_shape_xyz));
                    if first || !self.ortho_dirty.get_value() {
                        self.crosshair.set(crosshair_xyz);
                    }
                    self.slices.set(Some(Slices {
                        xy: format!("data:image/png;base64,{xy_base64}"),
                        xz: format!("data:image/png;base64,{xz_base64}"),
                        yz: format!("data:image/png;base64,{yz_base64}"),
                    }));
                    self.error.set(None);
                    self.status.set(format!("Slices in {render_ms:.0} ms"));
                    self.update_busy();
                    if self.ortho_dirty.get_value() {
                        self.request_orthogonal();
                    }
                }
            }
            SocketReply::Channels { layers, .. } => self.layers.set(layers),
            SocketReply::Error { request_id, status, message } => {
                if request_id.is_some() && self.volume_inflight.get_value() == request_id {
                    self.volume_inflight.set_value(None);
                }
                if request_id.is_some() && self.ortho_inflight.get_value() == request_id {
                    self.ortho_inflight.set_value(None);
                }
                self.update_busy();
                self.fail(format!("server {status}: {message}"));
            }
        }
    }

    pub fn move_crosshair(self, next: [u32; 3]) {
        let Some(shape) = self.voxel_shape.get_untracked() else { return };
        let clamped = [next[0].min(shape[0].saturating_sub(1)), next[1].min(shape[1].saturating_sub(1)), next[2].min(shape[2].saturating_sub(1))];
        if clamped != self.crosshair.get_untracked() {
            self.crosshair.set(clamped);
            self.request_orthogonal();
        }
    }

    pub fn orbit_by(self, dx: i32, dy: i32) {
        self.camera.update(|camera| {
            camera.orbit_x = (camera.orbit_x + dx).clamp(-MAX_ORBIT, MAX_ORBIT);
            camera.orbit_y = (camera.orbit_y + dy).clamp(-MAX_ORBIT, MAX_ORBIT);
        });
        self.request_volume();
    }

    pub fn zoom_by(self, factor: f32) {
        self.camera.update(|camera| camera.zoom = (camera.zoom * factor).clamp(ZOOM_RANGE.0, ZOOM_RANGE.1));
        self.request_volume();
    }

    pub fn set_renderer(self, renderer: Renderer) {
        self.notice.set(None);
        if self.renderer.get_untracked() != renderer {
            self.renderer.set(renderer);
            self.request_volume();
        }
    }

    pub fn set_view_mode(self, mode: ViewMode) {
        self.notice.set(None);
        self.view_mode.set(mode);
        // Pane sizes changed with the layout; render both at their new sizes.
        request_animation_frame(move || {
            self.request_orthogonal();
            self.request_volume();
        });
    }

    /// One channel edit: shown at once, sent latest-only, then both frames follow.
    pub fn set_channel(self, layer_id: u64, channel: usize, state: ChannelStateInput) {
        self.layers.update(|layers| {
            if let Some(slot) = layers.iter_mut().find(|layer| layer.layer_id == layer_id).and_then(|layer| layer.channels.get_mut(channel)) {
                slot.enabled = state.enabled;
                slot.color_srgb = state.color_srgb;
                slot.window_start = state.window_start;
                slot.window_end = state.window_end;
                slot.opacity = state.opacity;
            }
        });
        let edit = ChannelEdit { layer_id, channel, state };
        if self.channel_inflight.get_value() {
            self.channel_dirty.set_value(Some(edit));
            return;
        }
        self.post_channel(edit);
    }

    fn post_channel(self, edit: ChannelEdit) {
        let Some(dataset) = self.dataset.get_untracked() else { return };
        let url = channels_url(&self.origin.get_untracked(), &dataset);
        self.channel_inflight.set_value(true);
        self.update_busy();
        spawn_local(async move {
            match post_json::<Vec<LayerChannelSummary>, _>(&url, &edit).await {
                Ok(layers) => {
                    // Keep the page's own newer edits over the server's echo.
                    if self.channel_dirty.get_value().is_none() {
                        self.layers.set(layers);
                    }
                    self.error.set(None);
                }
                Err(message) => self.fail(message),
            }
            self.channel_inflight.set_value(false);
            self.update_busy();
            if let Some(next) = self.channel_dirty.get_value() {
                self.channel_dirty.set_value(None);
                self.post_channel(next);
            } else {
                self.request_orthogonal();
                self.request_volume();
            }
        });
    }

    pub fn add_layer(self, layer_dataset: String) {
        let Some(dataset) = self.dataset.get_untracked() else { return };
        let url = layers_url(&self.origin.get_untracked(), &dataset);
        self.status.set(format!("Adding layer {layer_dataset}…"));
        spawn_local(async move {
            match post_json::<Vec<LayerChannelSummary>, _>(&url, &LayerRequest { dataset: layer_dataset }).await {
                Ok(layers) => {
                    self.layers.set(layers);
                    self.error.set(None);
                    self.status.set("Layer added".into());
                    self.request_orthogonal();
                    self.request_volume();
                }
                Err(message) => self.fail(message),
            }
        });
    }
}

// ---- the browser renderer's plumbing -------------------------------------------------------

/// Hand the desktop's WGSL to the script once; it is the same constant the server dispatches.
fn install_shader() -> Result<(), String> {
    let module = js_sys::Reflect::get(&window(), &JsValue::from_str("newvolimSceneWebGpu")).map_err(js_error)?;
    if module.is_undefined() {
        return Err("scene-webgpu.js did not load".into());
    }
    let installed = js_sys::Reflect::get(&module, &JsValue::from_str("shader")).map_err(js_error)?;
    if installed.is_null() || installed.is_undefined() {
        js_sys::Reflect::set(&module, &JsValue::from_str("shader"), &JsValue::from_str(palace_core::gpu::SCENE_DVR_SHADER)).map_err(js_error)?;
    }
    Ok(())
}

async fn dispatch_on_gpu(dispatch: &palace_core::gpu::SceneDvrDispatch) -> Result<(Vec<u32>, Vec<u32>), String> {
    let pages = js_sys::Array::new();
    for page in &dispatch.pages {
        pages.push(&js_sys::Uint32Array::from(&page[..]));
    }
    let result = scene_webgpu_dispatch(
        pages,
        js_sys::Uint32Array::from(&dispatch.scene_data[..]),
        js_sys::Uint32Array::from(&dispatch.rays[..]),
        js_sys::Uint32Array::from(&dispatch.params[..]),
        dispatch.request_capacity as u32,
        dispatch.output_words as u32,
        dispatch.workgroups,
    )
    .await
    .map_err(js_error)?;
    let field = |name: &str| -> Result<Vec<u32>, String> {
        js_sys::Reflect::get(&result, &JsValue::from_str(name))
            .map_err(js_error)?
            .dyn_into::<js_sys::Uint32Array>()
            .map(|array| array.to_vec())
            .map_err(|_| format!("dispatch result has no {name} words"))
    };
    let output = field("output")?;
    let requests = field("requests")?;
    if output.len() != dispatch.output_words || requests.len() != dispatch.request_capacity {
        return Err("dispatch returned buffers of the wrong size".into());
    }
    Ok((output, requests))
}

/// Fetch the missing chunks grouped by (layer, level, channel), in batches, and insert them.
async fn fetch_chunks(
    origin: &str,
    dataset: &str,
    client: &mut newvolim_residency::ClientResidency,
    missing: &[newvolim_residency::ChunkRequest],
) -> Result<(), String> {
    let mut groups: Vec<((u64, u32, u32), Vec<newvolim_residency::ChunkRequest>)> = Vec::new();
    for request in missing {
        let key = (request.layer_id, request.level, request.source_index);
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, list)) => list.push(*request),
            None => groups.push((key, vec![*request])),
        }
    }
    let url = scene_chunks_url(origin, dataset);
    for ((layer_id, level, source_index), requests) in groups {
        for batch in requests.chunks(CHUNKS_PER_REQUEST) {
            let body = SceneChunksRequest { layer_id, level, source_index, chunks: batch.iter().map(|r| r.chunk_xyz).collect() };
            let bytes = post_bytes(&url, &body).await?;
            let chunks = chunks_from_le_bytes(&bytes, batch.len())?;
            for (request, words) in batch.iter().zip(chunks) {
                client.insert_chunk(request, words);
            }
        }
    }
    Ok(())
}

fn js_error(error: JsValue) -> String {
    error
        .as_string()
        .or_else(|| js_sys::Reflect::get(&error, &JsValue::from_str("message")).ok().and_then(|m| m.as_string()))
        .unwrap_or_else(|| format!("{error:?}"))
}

// ---- HTTP -------------------------------------------------------------------------------

async fn get_bytes(url: &str) -> Result<Vec<u8>, String> {
    let response = gloo_net::http::Request::get(url).send().await.map_err(|error| format!("GET {url}: {error}"))?;
    if !response.ok() {
        return Err(format!("GET {url}: {} {}", response.status(), response.text().await.unwrap_or_default()));
    }
    response.binary().await.map_err(|error| format!("GET {url}: {error}"))
}

async fn post_bytes<B: serde::Serialize>(url: &str, body: &B) -> Result<Vec<u8>, String> {
    let request = gloo_net::http::Request::post(url).json(body).map_err(|error| format!("POST {url}: {error}"))?;
    let response = request.send().await.map_err(|error| format!("POST {url}: {error}"))?;
    if !response.ok() {
        return Err(format!("POST {url}: {} {}", response.status(), response.text().await.unwrap_or_default()));
    }
    response.binary().await.map_err(|error| format!("POST {url}: {error}"))
}

async fn get_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<T, String> {
    let response = gloo_net::http::Request::get(url).send().await.map_err(|error| format!("GET {url}: {error}"))?;
    if !response.ok() {
        return Err(format!("GET {url}: {} {}", response.status(), response.text().await.unwrap_or_default()));
    }
    response.json::<T>().await.map_err(|error| format!("GET {url}: {error}"))
}

async fn post_json<T: serde::de::DeserializeOwned, B: serde::Serialize>(url: &str, body: &B) -> Result<T, String> {
    let request = gloo_net::http::Request::post(url).json(body).map_err(|error| format!("POST {url}: {error}"))?;
    let response = request.send().await.map_err(|error| format!("POST {url}: {error}"))?;
    if !response.ok() {
        return Err(format!("POST {url}: {} {}", response.status(), response.text().await.unwrap_or_default()));
    }
    response.json::<T>().await.map_err(|error| format!("POST {url}: {error}"))
}

/// One query-string parameter of the page's own URL, percent-decoded for the plain cases.
fn query_parameter(name: &str) -> Option<String> {
    let search = window().location().search().ok()?;
    let query = search.strip_prefix('?').unwrap_or(&search);
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then(|| value.replace("%2F", "/").replace('+', " "))
    })
}

/// A pane's size in physical pixels: the renderer's targets are sized in device pixels.
fn physical_size(element: &web_sys::Element) -> (u32, u32) {
    let scale = window().device_pixel_ratio().max(0.5);
    let size = |css: i32| ((css.max(1) as f64 * scale).floor() as u32).clamp(1, MAX_FRAME_SIDE);
    (size(element.client_width()), size(element.client_height()))
}

fn pointer_fraction(event: &web_sys::MouseEvent) -> Option<(f64, f64)> {
    let target = event.current_target()?.dyn_into::<web_sys::Element>().ok()?;
    let rect = target.get_bounding_client_rect();
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return None;
    }
    Some((
        ((event.client_x() as f64 - rect.left()) / rect.width()).clamp(0.0, 1.0),
        ((event.client_y() as f64 - rect.top()) / rect.height()).clamp(0.0, 1.0),
    ))
}

// ---- components ---------------------------------------------------------------------------

#[component]
pub fn App() -> impl IntoView {
    let session = Session::new();
    provide_context(session);
    session.connect();
    window_event_listener(leptos::ev::resize, move |_| {
        if session.dataset.get_untracked().is_some() {
            session.request_orthogonal();
            session.request_volume();
        }
    });
    view! {
        <div class="workspace">
            <nav class="workspace-tabs">
                <span class="brand">"newvolim"</span>
                <Show when=move || session.dataset.get().is_some()>
                    <span class="tab">
                        {move || session.dataset.get().unwrap_or_default()}
                        <button class="tab-close" title="Close dataset" on:click=move |_| session.close()>"✕"</button>
                    </span>
                </Show>
                <span class="spacer"></span>
                <span class="origin">{move || session.origin.get()}</span>
            </nav>
            <Show when=move || session.dataset.get().is_some() fallback=move || view! { <FrontPage/> }>
                <div class="app-container">
                    <ViewerShell/>
                    <button
                        class="panel-toggle"
                        class:collapsed=move || !session.panel_open.get()
                        title="Show or hide the layer panel"
                        on:click=move |_| session.panel_open.update(|open| *open = !*open)
                    >
                        {move || if session.panel_open.get() { "»" } else { "«" }}
                    </button>
                    <Sidebar/>
                </div>
            </Show>
        </div>
    }
}

#[component]
fn FrontPage() -> impl IntoView {
    let session = expect_context::<Session>();
    view! {
        <div class="front-page">
            <div class="front-browser">
                <h1>"newvolim"</h1>
                <div class="subtitle">"Volume images from OME-Zarr, rendered on the server or in this browser."</div>
                <div class="browser-list">
                    {move || {
                        let datasets = session.datasets.get();
                        if datasets.is_empty() {
                            view! { <div class="hint">"No datasets listed. Configure the server with --dataset name=path, or point this page at another server below."</div> }.into_any()
                        } else {
                            datasets.into_iter().map(|name| {
                                let open_name = name.clone();
                                view! {
                                    <button class="browser-row" on:click=move |_| session.open(open_name.clone())>
                                        <span class="icon">"▣"</span>
                                        <span class="name">{name}</span>
                                        <span class="kind">"OME-Zarr"</span>
                                    </button>
                                }
                            }).collect_view().into_any()
                        }
                    }}
                </div>
                <div class="connect">
                    <input
                        type="url"
                        placeholder="server URL (empty = this page's origin)"
                        prop:value=move || session.origin_entry.get()
                        on:input=move |ev| session.origin_entry.set(event_target_value(&ev))
                        on:keydown=move |ev: web_sys::KeyboardEvent| if ev.key() == "Enter" { session.connect() }
                    />
                    <button class="accent-button" on:click=move |_| session.connect()>"Connect"</button>
                </div>
                <div class="hint">{move || session.status.get()}</div>
                <Show when=move || session.error.get().is_some()>
                    <div class="error">{move || session.error.get().unwrap_or_default()}</div>
                </Show>
            </div>
        </div>
    }
}

#[component]
fn ViewerShell() -> impl IntoView {
    let session = expect_context::<Session>();
    let mode_button = move |mode: ViewMode, label: &'static str| {
        view! {
            <button class="tool-button" class:active=move || session.view_mode.get() == mode on:click=move |_| session.set_view_mode(mode)>{label}</button>
        }
    };
    let renderer_button = move |renderer: Renderer, label: &'static str, title: &'static str| {
        view! {
            <button class="tool-button" title=title class:active=move || session.renderer.get() == renderer on:click=move |_| session.set_renderer(renderer)>{label}</button>
        }
    };
    view! {
        <div class="viewer-shell">
            <div
                class="viewer-area"
                class:grid=move || session.view_mode.get() == ViewMode::Grid
                class:single=move || session.view_mode.get() != ViewMode::Grid
            >
                <div class="toolbar">
                    <div class="tool-group">
                        {mode_button(ViewMode::Grid, "Grid")}
                        {mode_button(ViewMode::Xy, "XY")}
                        {mode_button(ViewMode::Xz, "XZ")}
                        {mode_button(ViewMode::Yz, "YZ")}
                        {mode_button(ViewMode::Volume, "3D")}
                    </div>
                    <div class="tool-group">
                        {renderer_button(Renderer::Server, "Server", "Volume frames rendered by the server")}
                        {renderer_button(Renderer::Browser, "WebGPU", "Volume rendered in this browser: it plans residency itself and fetches only the chunks it misses")}
                    </div>
                    <div class="tool-group">
                        <button class="tool-button" title="Reset the camera" on:click=move |_| { session.camera.set(Camera::default()); session.request_volume(); }>"Reset view"</button>
                    </div>
                </div>
                <OrthoPane plane=Plane::Xy/>
                <OrthoPane plane=Plane::Xz/>
                <OrthoPane plane=Plane::Yz/>
                <VolumePane/>
            </div>
            <AxisSliders/>
            <div class="status-bar">
                <span class:busy=move || session.busy.get()>{move || if session.busy.get() { "●" } else { "○" }}</span>
                <span>{move || session.status.get()}</span>
                <span class="readout">{move || {
                    let c = session.crosshair.get();
                    match session.voxel_shape.get() {
                        Some(s) => format!("crosshair {} {} {} of {}×{}×{}", c[0], c[1], c[2], s[0], s[1], s[2]),
                        None => String::new(),
                    }
                }}</span>
                <span class="readout">{move || { let cam = session.camera.get(); format!("orbit {} {} zoom {:.2}", cam.orbit_x, cam.orbit_y, cam.zoom) }}</span>
                <Show when=move || session.notice.get().is_some()>
                    <span class="error">{move || session.notice.get().unwrap_or_default()}</span>
                </Show>
                <Show when=move || session.error.get().is_some()>
                    <span class="error">{move || session.error.get().unwrap_or_default()}</span>
                </Show>
            </div>
        </div>
    }
}

#[component]
fn OrthoPane(plane: Plane) -> impl IntoView {
    let session = expect_context::<Session>();
    let (h_axis, v_axis, depth_axis) = plane.axes();
    let hidden = move || !session.view_mode.get().shows_plane(plane);
    let fraction = move |axis: usize| {
        let shape = session.voxel_shape.get().unwrap_or([1; 3]);
        let at = session.crosshair.get()[axis] as f64 + 0.5;
        format!("{}%", (at / shape[axis].max(1) as f64 * 100.0).clamp(0.0, 100.0))
    };
    let image = move || {
        session.slices.get().map(|slices| match plane {
            Plane::Xy => slices.xy,
            Plane::Xz => slices.xz,
            Plane::Yz => slices.yz,
        })
    };
    let on_click = move |ev: web_sys::MouseEvent| {
        let Some(shape) = session.voxel_shape.get_untracked() else { return };
        let Some((fx, fy)) = pointer_fraction(&ev) else { return };
        let mut next = session.crosshair.get_untracked();
        next[h_axis] = ((fx * shape[h_axis] as f64).floor() as u32).min(shape[h_axis].saturating_sub(1));
        next[v_axis] = ((fy * shape[v_axis] as f64).floor() as u32).min(shape[v_axis].saturating_sub(1));
        session.move_crosshair(next);
    };
    let on_wheel = move |ev: web_sys::WheelEvent| {
        ev.prevent_default();
        let mut next = session.crosshair.get_untracked();
        let step = if ev.delta_y() > 0.0 { 1 } else { -1 };
        next[depth_axis] = (next[depth_axis] as i64 + step).max(0) as u32;
        session.move_crosshair(next);
    };
    let node_ref = session.ortho_panes[match plane {
        Plane::Xy => 0,
        Plane::Xz => 1,
        Plane::Yz => 2,
    }];
    view! {
        <div class="pane ortho" class:hidden-pane=hidden node_ref=node_ref on:click=on_click on:wheel=on_wheel>
            {move || match image() {
                Some(src) => view! { <img class="pane-image" src=src alt=plane.label() draggable="false"/> }.into_any(),
                None => view! { <div class="pane-empty">"waiting for slices…"</div> }.into_any(),
            }}
            <div class="crosshair-v" style:left=move || fraction(h_axis)></div>
            <div class="crosshair-h" style:top=move || fraction(v_axis)></div>
            <span class="pane-label">{plane.label()}</span>
        </div>
    }
}

#[component]
fn VolumePane() -> impl IntoView {
    let session = expect_context::<Session>();
    let hidden = move || !session.view_mode.get().shows_volume();
    let last = StoredValue::new(None::<(i32, i32)>);
    let on_down = move |ev: web_sys::PointerEvent| {
        last.set_value(Some((ev.client_x(), ev.client_y())));
        if let Some(target) = ev.current_target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()) {
            let _ = target.set_pointer_capture(ev.pointer_id());
        }
    };
    let on_move = move |ev: web_sys::PointerEvent| {
        let Some((lx, ly)) = last.get_value() else { return };
        if ev.buttons() & 1 == 0 {
            return;
        }
        let (x, y) = (ev.client_x(), ev.client_y());
        last.set_value(Some((x, y)));
        session.orbit_by(x - lx, y - ly);
    };
    let on_up = move |_: web_sys::PointerEvent| last.set_value(None);
    let on_wheel = move |ev: web_sys::WheelEvent| {
        ev.prevent_default();
        session.zoom_by(if ev.delta_y() < 0.0 { 1.1 } else { 1.0 / 1.1 });
    };
    view! {
        <div
            class="pane volume"
            class:hidden-pane=hidden
            node_ref=session.volume_pane
            on:pointerdown=on_down
            on:pointermove=on_move
            on:pointerup=on_up
            on:pointercancel=on_up
            on:wheel=on_wheel
        >
            <canvas class="pane-canvas" node_ref=session.volume_canvas hidden=move || session.renderer.get() != Renderer::Browser></canvas>
            {move || match (session.renderer.get(), session.volume_png.get()) {
                (Renderer::Server, Some(src)) => view! { <img class="pane-image" src=src alt="volume" draggable="false"/> }.into_any(),
                (Renderer::Server, None) => view! { <div class="pane-empty">"waiting for the volume frame…"</div> }.into_any(),
                (Renderer::Browser, _) => view! { <span></span> }.into_any(),
            }}
            <span class="pane-label">"3D"</span>
            <CubePane/>
        </div>
    }
}

/// The orientation box: the volume to its true proportions, the three crosshair planes, and
/// a drag on a plane that scrubs its axis.
#[component]
fn CubePane() -> impl IntoView {
    let session = expect_context::<Session>();
    let canvas_ref = NodeRef::<leptos::html::Canvas>::new();
    let hover = RwSignal::new(None::<usize>);
    let drag = StoredValue::new(None::<(usize, f32, (f32, f32))>);

    let view_for = move |canvas: &web_sys::HtmlCanvasElement| {
        let shape = session.voxel_shape.get_untracked().unwrap_or([1; 3]);
        let rect = canvas.get_bounding_client_rect();
        CubeView::new([shape[0] as f32, shape[1] as f32, shape[2] as f32], (rect.width() as f32, rect.height() as f32))
    };
    let cut_fractions = move || {
        let shape = session.voxel_shape.get_untracked().unwrap_or([1; 3]);
        let c = session.crosshair.get_untracked();
        [0, 1, 2].map(|axis| (c[axis] as f32 + 0.5) / shape[axis].max(1) as f32)
    };
    let pointer = move |ev: &web_sys::MouseEvent| -> Option<(f32, f32)> {
        let target = ev.current_target()?.dyn_into::<web_sys::Element>().ok()?;
        let rect = target.get_bounding_client_rect();
        Some(((ev.client_x() as f64 - rect.left()) as f32, (ev.client_y() as f64 - rect.top()) as f32))
    };

    // Redraw whenever the crosshair, shape or hover changes.
    Effect::new(move |_| {
        let _ = (session.crosshair.get(), session.voxel_shape.get(), hover.get(), session.view_mode.get(), session.volume_png.get());
        let Some(canvas) = canvas_ref.get() else { return };
        let canvas: web_sys::HtmlCanvasElement = canvas.clone();
        draw_cube(&canvas, &view_for(&canvas), cut_fractions(), hover.get_untracked(), drag.get_value().map(|d| d.0));
    });

    let on_down = move |ev: web_sys::PointerEvent| {
        let Some(canvas) = canvas_ref.get_untracked() else { return };
        let canvas: web_sys::HtmlCanvasElement = canvas.clone();
        let Some(at) = pointer(&ev) else { return };
        let view = view_for(&canvas);
        let cut = cut_fractions();
        if let Some(axis) = view.pick(cut, at) {
            drag.set_value(Some((axis, cut[axis], at)));
            hover.set(Some(axis));
            let _ = canvas.set_pointer_capture(ev.pointer_id());
        }
    };
    let on_move = move |ev: web_sys::PointerEvent| {
        let Some(canvas) = canvas_ref.get_untracked() else { return };
        let canvas: web_sys::HtmlCanvasElement = canvas.clone();
        let Some(at) = pointer(&ev) else { return };
        let view = view_for(&canvas);
        match drag.get_value() {
            Some((axis, start, from)) => {
                if let Some(fraction) = view.drag_fraction(axis, start, (at.0 - from.0, at.1 - from.1)) {
                    let shape = session.voxel_shape.get_untracked().unwrap_or([1; 3]);
                    let mut next = session.crosshair.get_untracked();
                    next[axis] = ((fraction * shape[axis] as f32).floor() as u32).min(shape[axis].saturating_sub(1));
                    session.move_crosshair(next);
                }
            }
            None => {
                let picked = view.pick(cut_fractions(), at);
                if picked != hover.get_untracked() {
                    hover.set(picked);
                }
                let _ = canvas.set_attribute("style", if picked.is_some() { "cursor: grab" } else { "cursor: default" });
            }
        }
    };
    let on_up = move |_: web_sys::PointerEvent| {
        drag.set_value(None);
        hover.update(|h| *h = h.take());
    };
    view! {
        <div class="cube-inset" title="Where the three slices cut the volume; drag a plane to scrub its axis" on:pointerdown=|ev| ev.stop_propagation() on:wheel=|ev| ev.stop_propagation()>
            <canvas class="pane-canvas" node_ref=canvas_ref on:pointerdown=on_down on:pointermove=on_move on:pointerup=on_up on:pointercancel=on_up on:pointerleave=move |_| { if drag.get_value().is_none() { hover.set(None) } }></canvas>
        </div>
    }
}

fn draw_cube(canvas: &web_sys::HtmlCanvasElement, view: &CubeView, cut: [f32; 3], hover: Option<usize>, dragging: Option<usize>) {
    let rect = canvas.get_bounding_client_rect();
    let scale = window().device_pixel_ratio().max(0.5);
    let (w, h) = (rect.width().max(1.0), rect.height().max(1.0));
    canvas.set_width((w * scale) as u32);
    canvas.set_height((h * scale) as u32);
    let Some(context) = canvas.get_context("2d").ok().flatten().and_then(|c| c.dyn_into::<web_sys::CanvasRenderingContext2d>().ok()) else { return };
    let _ = context.scale(scale, scale);
    context.set_fill_style_str("#0d0d1a");
    context.fill_rect(0.0, 0.0, w, h);

    // Planes back to front, then the wireframe on top.
    let mut order: Vec<(f32, usize)> = (0..3)
        .map(|axis| {
            let mut centre = [0.0; 3];
            centre[axis] = view.cut_coord(axis, cut[axis]);
            (view.depth_of(centre), axis)
        })
        .collect();
    order.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let colours = ["233, 69, 96", "84, 200, 120", "80, 140, 255"];
    for (_, axis) in order {
        let active = hover == Some(axis) || dragging == Some(axis);
        let alpha = if active { 0.42 } else { 0.20 };
        let quad = view.plane_quad(axis, cut[axis]);
        context.begin_path();
        context.move_to(quad[0].0 as f64, quad[0].1 as f64);
        for point in &quad[1..] {
            context.line_to(point.0 as f64, point.1 as f64);
        }
        context.close_path();
        context.set_fill_style_str(&format!("rgba({}, {alpha})", colours[axis]));
        context.fill();
        context.set_stroke_style_str(&format!("rgba({}, {})", colours[axis], if active { 1.0 } else { 0.7 }));
        context.set_line_width(if active { 1.5 } else { 1.0 });
        context.stroke();
    }
    let corners = view.corner_points();
    context.set_stroke_style_str("#7382a8");
    context.set_line_width(1.0);
    context.begin_path();
    for i in 0..8 {
        for axis in 0..3 {
            let j = i ^ (1 << axis);
            if i < j {
                context.move_to(corners[i].1 .0 as f64, corners[i].1 .1 as f64);
                context.line_to(corners[j].1 .0 as f64, corners[j].1 .1 as f64);
            }
        }
    }
    context.stroke();
    // Axis letters at the far end of each axis from the origin corner.
    context.set_fill_style_str("#aaa");
    context.set_font("11px system-ui, sans-serif");
    for (axis, letter) in ["x", "y", "z"].iter().enumerate() {
        let end = corners[1 << axis].1;
        let start = corners[0].1;
        let (x, y) = (end.0 + (end.0 - start.0) * 0.06, end.1 + (end.1 - start.1) * 0.06);
        let _ = context.fill_text(letter, x as f64, y as f64);
    }
}

#[component]
fn AxisSliders() -> impl IntoView {
    let session = expect_context::<Session>();
    let slider = move |axis: usize, label: &'static str| {
        let max = move || session.voxel_shape.get().map(|s| s[axis].saturating_sub(1)).unwrap_or(0);
        view! {
            <div class="slider-row">
                <span>{label}</span>
                <input
                    type="range"
                    min="0"
                    max=move || max().to_string()
                    step="1"
                    prop:value=move || session.crosshair.get()[axis].to_string()
                    on:input=move |ev| {
                        if let Ok(value) = event_target_value(&ev).parse::<u32>() {
                            let mut next = session.crosshair.get_untracked();
                            next[axis] = value;
                            session.move_crosshair(next);
                        }
                    }
                />
                <span class="slider-value">{move || format!("{} / {}", session.crosshair.get()[axis], max())}</span>
            </div>
        }
    };
    view! {
        <div class="axis-sliders">
            {slider(0, "X")}
            {slider(1, "Y")}
            {slider(2, "Z")}
        </div>
    }
}

#[component]
fn Sidebar() -> impl IntoView {
    let session = expect_context::<Session>();
    let add_choice = RwSignal::new(String::new());
    view! {
        <aside class="control-panel" class:hidden=move || !session.panel_open.get()>
            <h2>"Layers"</h2>
            {move || session.layers.get().into_iter().map(|layer| view! { <LayerCard layer=layer/> }).collect_view()}
            <Show when=move || session.layers.get().is_empty()>
                <div class="hint">"No image layers yet."</div>
            </Show>
            <div class="layer-block add-layer">
                <h3>"Add layer"</h3>
                <div class="row">
                    <select on:change=move |ev| add_choice.set(event_target_value(&ev))>
                        <option value="">"configured dataset…"</option>
                        {move || session.datasets.get().into_iter().map(|name| { let value = name.clone(); view! { <option value=value>{name}</option> } }).collect_view()}
                    </select>
                    <button class="accent-button" disabled=move || add_choice.get().is_empty() on:click=move |_| session.add_layer(add_choice.get())>"Add"</button>
                </div>
                <div class="hint">"Another configured OME-Zarr rendered in its own box over this one; its levels are chosen per layer."</div>
            </div>
        </aside>
    }
}

#[component]
fn LayerCard(layer: LayerChannelSummary) -> impl IntoView {
    let layer_id = layer.layer_id;
    let count = layer.channels.len();
    view! {
        <div class="channel-control">
            <div class="channel-header">
                <span class="layer-name" title=layer.name.clone()>{layer.name.clone()}</span>
                <span class="layer-meta">{format!("{count} ch")}</span>
            </div>
            {layer.channels.into_iter().map(|channel| view! { <ChannelRow layer_id=layer_id channel=channel/> }).collect_view()}
        </div>
    }
}

#[component]
fn ChannelRow(layer_id: u64, channel: ChannelSummary) -> impl IntoView {
    let session = expect_context::<Session>();
    let index = channel.source_index;
    let state = RwSignal::new(channel.to_input());
    // The contrast sliders span from zero to a round bound comfortably above the window, so a
    // 12-bit window on 16-bit data is not squeezed into the first tenth of the track.
    let bound = {
        let top = state.get_untracked().window_end.max(1.0) * 1.5;
        let mut bound = 255.0_f64;
        while bound < top {
            bound *= 2.0;
        }
        bound
    };
    let send = move || session.set_channel(layer_id, index, state.get_untracked());
    let on_min = move |ev: web_sys::Event| {
        if let Ok(value) = event_target_value(&ev).parse::<f64>() {
            state.update(|s| s.window_start = value.min(s.window_end - 1.0).max(0.0));
            send();
        }
    };
    let on_max = move |ev: web_sys::Event| {
        if let Ok(value) = event_target_value(&ev).parse::<f64>() {
            state.update(|s| s.window_end = value.max(s.window_start + 1.0));
            send();
        }
    };
    view! {
        <div class="image-channel-control">
            <div class="channel-header">
                <input
                    type="checkbox"
                    prop:checked=move || state.get().enabled
                    on:change=move |ev| { state.update(|s| s.enabled = event_target_checked(&ev)); send(); }
                />
                <span class="channel-name">{format!("channel {index}")}</span>
                <input
                    type="color"
                    class="color-picker"
                    prop:value=move || color_hex(state.get().color_srgb)
                    on:input=move |ev| {
                        if let Some(rgb) = parse_color_hex(&event_target_value(&ev)) {
                            state.update(|s| s.color_srgb = rgb);
                            send();
                        }
                    }
                />
            </div>
            <Show when=move || state.get().enabled>
                <div class="slider-row">
                    <span>"Contrast"</span>
                    <div class="dual-range">
                        <input type="range" class="min" min="0" max=bound.to_string() step="1" prop:value=move || state.get().window_start.to_string() on:input=on_min/>
                        <input type="range" class="max" min="0" max=bound.to_string() step="1" prop:value=move || state.get().window_end.to_string() on:input=on_max/>
                    </div>
                    <span class="slider-value">{move || { let s = state.get(); format!("{:.0}-{:.0}", s.window_start, s.window_end) }}</span>
                </div>
                <div class="slider-row">
                    <span>"Opacity"</span>
                    <input
                        type="range"
                        min="0"
                        max="100"
                        step="1"
                        prop:value=move || ((state.get().opacity * 100.0).round() as i32).to_string()
                        on:input=move |ev| {
                            if let Ok(value) = event_target_value(&ev).parse::<f32>() {
                                state.update(|s| s.opacity = (value / 100.0).clamp(0.0, 1.0));
                                send();
                            }
                        }
                    />
                    <span class="slider-value">{move || format!("{}%", (state.get().opacity * 100.0).round() as i32)}</span>
                </div>
            </Show>
        </div>
    }
}
