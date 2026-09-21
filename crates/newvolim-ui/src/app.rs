//! The page: session state, the server connection, and the components.
//!
//! Layout follows `omezarr_viewers-rs`: a tab strip, then the viewer shell (floating tool
//! strip, the 2×2 grid of XY / XZ / YZ slices and the orientation box — or one pane alone —
//! axis sliders, a status line) beside a sidebar of layer cards. All state is a set of signals
//! in [`Session`], and every server interaction is latest-only: while a frame is in flight a
//! newer camera or crosshair only marks the kind dirty, and the reply triggers the next
//! request, so the page never queues more work than the renderer can drain.

use std::{cell::RefCell, collections::HashMap};
use std::rc::Rc;

use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use newvolim_scene::qupath::{self, Annotation, Geometry, ObjectType, Plane as AnnotationPlane};

use crate::api::*;
use crate::cube::CubeView;

const MAX_FRAME_SIDE: u32 = 4096;
const ZOOM_RANGE: (f32, f32) = (0.25, 4.0);
const INTERACTION_SETTLE_MS: u32 = 180;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnnotationTool {
    Pan, Select, Point, Rectangle, Ellipse, Polygon, FreehandRegion, Polyline, FreehandLine,
}

#[derive(Clone)]
struct AnnotationViewStyle {
    class: String,
    class_color: [u8; 3],
    object_type: ObjectType,
    stroke_width: Option<f64>,
    dense_region: bool,
    filter: Option<String>,
    world_radius: bool,
    radius: f64,
    class_radii: HashMap<String, f64>,
    filled: bool,
    color_by_class: bool,
    opacity: f64,
    point_size: f64,
    slab: f64,
}

impl Default for AnnotationViewStyle {
    fn default() -> Self {
        Self { class: String::new(), class_color: [51, 230, 255], object_type: ObjectType::Annotation,
            stroke_width: None, dense_region: false, filter: None, world_radius: false,
            radius: 20.0, class_radii: HashMap::new(), filled: false, color_by_class: false, opacity: 0.95,
            point_size: 11.0, slab: 8.0 }
    }
}

impl AnnotationTool {
    fn label(self) -> &'static str {
        match self {
            Self::Pan => "Pan", Self::Select => "Select", Self::Point => "Point",
            Self::Rectangle => "Rectangle", Self::Ellipse => "Ellipse", Self::Polygon => "Polygon",
            Self::FreehandRegion => "Freehand region", Self::Polyline => "Polyline",
            Self::FreehandLine => "Freehand line",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub orientation: [f32; 4],
    pub zoom: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self { orientation: [0.0, 0.0, 0.0, 1.0], zoom: 1.0 }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Slices {
    pub xy: String,
    pub xz: String,
    pub yz: String,
    pub viewport: bool,
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
    pub annotation_layers: RwSignal<Vec<AnnotationLayer>>,
    pub annotation_roi_tables: RwSignal<Vec<RoiTableSummary>>,
    pub annotation_layer: RwSignal<Option<u64>>,
    pub annotation_tool: RwSignal<AnnotationTool>,
    pub annotation_class: RwSignal<String>,
    pub annotation_class_color: RwSignal<[u8; 3]>,
    pub annotation_object_type: RwSignal<ObjectType>,
    pub annotation_stroke_width: RwSignal<Option<f64>>,
    pub annotation_dense_region: RwSignal<bool>,
    pub annotation_filter: RwSignal<Option<String>>,
    pub annotation_world_radius: RwSignal<bool>,
    pub annotation_radius: RwSignal<f64>,
    pub annotation_class_radii: RwSignal<HashMap<String, f64>>,
    pub selected_annotation: RwSignal<Option<u64>>,
    pub annotation_filled: RwSignal<bool>,
    pub annotation_color_by_class: RwSignal<bool>,
    pub annotation_opacity: RwSignal<f64>,
    pub annotation_point_size: RwSignal<f64>,
    pub annotation_slab: RwSignal<f64>,
    pub annotation_draft: RwSignal<Vec<[f64; 2]>>,
    annotation_create_inflight: StoredValue<bool>,
    queued_annotations: StoredValue<Vec<Annotation>>,
    annotation_undo: StoredValue<Vec<(u64, Vec<Annotation>)>>,
    annotation_styles: StoredValue<HashMap<u64, AnnotationViewStyle>>,
    pub voxel_shape: RwSignal<Option<[u32; 3]>>,
    /// The voxel at the centre of every slice pane: the integer crosshair the slices are cut
    /// at, derived from `focus`.
    pub crosshair: RwSignal<[u32; 3]>,
    /// The point at the centre of the slice panes, in voxels, continuous so a pan is smooth;
    /// `crosshair` is its floor.
    pub focus: RwSignal<[f64; 3]>,
    /// Zoom of the XY, XZ and YZ panes over their slice: 1 fits the whole slice.
    pub zoom_2d: RwSignal<[f64; 3]>,
    /// Bumped on window resize so pane geometry recomputes.
    pub layout_tick: RwSignal<u32>,
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
    volume_preview: StoredValue<bool>,
    volume_preview_generation: StoredValue<u64>,
    ortho_inflight: StoredValue<Option<u64>>,
    ortho_dirty: StoredValue<bool>,
    browser_busy: StoredValue<bool>,
    browser_dirty: StoredValue<bool>,
    channel_inflight: StoredValue<bool>,
    channel_dirty: StoredValue<Option<ChannelEdit>>,
    /// Scene-wide see-through depth (the server's `depthScale`), shown at once and sent
    /// latest-only.
    pub depth_scale: RwSignal<f32>,
    settings_inflight: StoredValue<bool>,
    settings_dirty: StoredValue<Option<f32>>,
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
            annotation_layers: RwSignal::new(Vec::new()),
            annotation_roi_tables: RwSignal::new(Vec::new()),
            annotation_layer: RwSignal::new(None),
            annotation_tool: RwSignal::new(AnnotationTool::Pan),
            annotation_class: RwSignal::new(String::new()),
            annotation_class_color: RwSignal::new([51, 230, 255]),
            annotation_object_type: RwSignal::new(ObjectType::Annotation),
            annotation_stroke_width: RwSignal::new(None),
            annotation_dense_region: RwSignal::new(false),
            annotation_filter: RwSignal::new(None),
            annotation_world_radius: RwSignal::new(false),
            annotation_radius: RwSignal::new(20.0),
            annotation_class_radii: RwSignal::new(HashMap::new()),
            selected_annotation: RwSignal::new(None),
            annotation_filled: RwSignal::new(false),
            annotation_color_by_class: RwSignal::new(false),
            annotation_opacity: RwSignal::new(0.95),
            annotation_point_size: RwSignal::new(11.0),
            annotation_slab: RwSignal::new(8.0),
            annotation_draft: RwSignal::new(Vec::new()),
            annotation_create_inflight: StoredValue::new(false),
            queued_annotations: StoredValue::new(Vec::new()),
            annotation_undo: StoredValue::new(Vec::new()),
            annotation_styles: StoredValue::new(HashMap::new()),
            voxel_shape: RwSignal::new(None),
            crosshair: RwSignal::new([0; 3]),
            focus: RwSignal::new([0.0; 3]),
            zoom_2d: RwSignal::new([1.0; 3]),
            layout_tick: RwSignal::new(0),
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
            volume_preview: StoredValue::new(false),
            volume_preview_generation: StoredValue::new(0),
            ortho_inflight: StoredValue::new(None),
            ortho_dirty: StoredValue::new(false),
            browser_busy: StoredValue::new(false),
            browser_dirty: StoredValue::new(false),
            channel_inflight: StoredValue::new(false),
            channel_dirty: StoredValue::new(None),
            depth_scale: RwSignal::new(1.0),
            settings_inflight: StoredValue::new(false),
            settings_dirty: StoredValue::new(None),
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
            || self.channel_inflight.get_value()
            || self.settings_inflight.get_value();
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
        if !self.close() { return; }
        self.dataset.set(Some(name.clone()));
        self.error.set(None);
        self.status.set(format!("Opening {name}…"));
        self.refresh_layers();
        self.refresh_annotations();
        self.refresh_settings();
        self.open_socket();
        // The panes mount on the next frame; ask for the first frames at their real sizes. The
        // first orthogonal reply brings the voxel shape and a centred crosshair.
        request_animation_frame(move || {
            self.request_orthogonal();
            self.request_volume();
        });
    }

    pub fn close(self) -> bool {
        if self.annotation_layers.get_untracked().iter().any(|layer| layer.dirty)
            && !window().confirm_with_message("Unsaved annotations will be lost. Close this dataset?").unwrap_or(false) {
            return false;
        }
        self.volume_preview_generation.update_value(|generation| *generation = generation.wrapping_add(1));
        self.volume_preview.set_value(false);
        if let Some(socket) = self.socket.get_value() {
            let _ = socket.borrow().ws.close();
        }
        self.socket.set_value(None);
        self.dataset.set(None);
        self.layers.set(Vec::new());
        self.select_annotation_layer(None);
        self.annotation_styles.set_value(HashMap::new());
        self.annotation_layers.set(Vec::new());
        self.annotation_roi_tables.set(Vec::new());
        self.selected_annotation.set(None);
        self.annotation_tool.set(AnnotationTool::Pan);
        self.annotation_draft.set(Vec::new());
        self.annotation_create_inflight.set_value(false);
        self.queued_annotations.set_value(Vec::new());
        self.annotation_undo.set_value(Vec::new());
        self.voxel_shape.set(None);
        self.slices.set(None);
        self.volume_png.set(None);
        self.camera.set(Camera::default());
        self.volume_inflight.set_value(None);
        self.ortho_inflight.set_value(None);
        self.volume_dirty.set_value(false);
        self.ortho_dirty.set_value(false);
        self.update_busy();
        true
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

    fn refresh_annotations(self) {
        let Some(dataset) = self.dataset.get_untracked() else { return };
        let url = annotation_layers_url(&self.origin.get_untracked(), &dataset);
        spawn_local(async move {
            match get_json::<Vec<AnnotationLayer>>(&url).await {
                Ok(layers) => {
                    let has_annotations = layers.iter().any(|layer| layer.visible && !layer.annotations.is_empty());
                    let active = self.annotation_layer.get_untracked();
                    self.select_annotation_layer(active.filter(|id| layers.iter().any(|layer| layer.id == *id)).or_else(|| layers.first().map(|layer| layer.id)));
                    self.annotation_layers.set(layers);
                    if has_annotations { self.request_volume(); }
                }
                Err(message) => self.fail(message),
            }
        });
        let tables_url = annotation_roi_tables_url(&self.origin.get_untracked(), &dataset);
        spawn_local(async move {
            if let Ok(tables) = get_json::<Vec<RoiTableSummary>>(&tables_url).await { self.annotation_roi_tables.set(tables); }
        });
    }

    fn set_annotation_layer(self, updated: AnnotationLayer) {
        self.annotation_layers.update(|layers| {
            if let Some(existing) = layers.iter_mut().find(|layer| layer.id == updated.id) {
                *existing = updated;
            } else { layers.push(updated); }
        });
    }

    fn active_annotations(self) -> Vec<Annotation> {
        let id = self.annotation_layer.get_untracked();
        self.annotation_layers.get_untracked().into_iter().find(|layer| Some(layer.id) == id).map(|layer| layer.annotations).unwrap_or_default()
    }

    fn current_annotation_style(self) -> AnnotationViewStyle {
        AnnotationViewStyle {
            class: self.annotation_class.get(),
            class_color: self.annotation_class_color.get(),
            object_type: self.annotation_object_type.get(),
            stroke_width: self.annotation_stroke_width.get(),
            dense_region: self.annotation_dense_region.get(),
            filter: self.annotation_filter.get(),
            world_radius: self.annotation_world_radius.get(),
            radius: self.annotation_radius.get(),
            class_radii: self.annotation_class_radii.get(),
            filled: self.annotation_filled.get(),
            color_by_class: self.annotation_color_by_class.get(),
            opacity: self.annotation_opacity.get(),
            point_size: self.annotation_point_size.get(),
            slab: self.annotation_slab.get(),
        }
    }

    fn select_annotation_layer(self, next: Option<u64>) {
        let previous = self.annotation_layer.get_untracked();
        if previous == next { return; }
        if let Some(id) = previous {
            let style = self.current_annotation_style();
            self.annotation_styles.update_value(|styles| { styles.insert(id, style); });
        }
        let style = next.and_then(|id| self.annotation_styles.get_value().get(&id).cloned()).unwrap_or_default();
        self.annotation_class.set(style.class);
        self.annotation_class_color.set(style.class_color);
        self.annotation_object_type.set(style.object_type);
        self.annotation_stroke_width.set(style.stroke_width);
        self.annotation_dense_region.set(style.dense_region);
        self.annotation_filter.set(style.filter);
        self.annotation_world_radius.set(style.world_radius);
        self.annotation_radius.set(style.radius);
        self.annotation_class_radii.set(style.class_radii);
        self.annotation_filled.set(style.filled);
        self.annotation_color_by_class.set(style.color_by_class);
        self.annotation_opacity.set(style.opacity);
        self.annotation_point_size.set(style.point_size);
        self.annotation_slab.set(style.slab);
        self.annotation_layer.set(next);
        self.selected_annotation.set(None);
    }

    fn edit_selected_annotation(self, edit: impl FnOnce(&mut Annotation)) {
        let Some(id) = self.selected_annotation.get_untracked() else { return };
        let Some(mut annotation) = self.active_annotations().into_iter().find(|item| item.id == id) else { return };
        edit(&mut annotation);
        self.remember_annotations();
        self.update_annotation(annotation);
    }

    fn remember_annotations(self) {
        let Some(layer) = self.annotation_layer.get_untracked() else { return };
        let rows = self.active_annotations();
        self.annotation_undo.update_value(|history| {
            history.push((layer, rows));
            if history.len() > 50 { history.remove(0); }
        });
    }

    fn undo_annotations(self) {
        let Some((layer, rows)) = self.annotation_undo.get_value().last().cloned() else { return };
        let Some(dataset) = self.dataset.get_untracked() else { return };
        let url = format!("{}/state", annotation_layer_url(&self.origin.get_untracked(), &dataset, layer));
        spawn_local(async move {
            match put_json::<AnnotationLayer, _>(&url, &rows).await {
                Ok(updated) => {
                    self.annotation_undo.update_value(|history| { history.pop(); });
                    self.select_annotation_layer(Some(layer));
                    self.set_annotation_layer(updated);
                    self.selected_annotation.set(None);
                    self.error.set(None);
                    self.request_volume();
                }
                Err(message) => self.fail(message),
            }
        });
    }

    fn finish_annotation_draft(self) {
        let points = self.annotation_draft.get_untracked();
        self.annotation_draft.set(Vec::new());
        let tool = self.annotation_tool.get_untracked();
        let Some((geometry, is_ellipse)) = annotation_geometry(tool, &points) else { return };
        let z = self.crosshair.get_untracked()[2] as i32;
        self.add_annotation(Annotation { geometry, is_ellipse, plane: AnnotationPlane::at(z, 0), ..Annotation::default() });
    }

    fn create_annotation_layer(self, name: String) {
        let Some(dataset) = self.dataset.get_untracked() else { return };
        let url = annotation_layers_url(&self.origin.get_untracked(), &dataset);
        spawn_local(async move {
            match post_json::<AnnotationLayer, _>(&url, &serde_json::json!({"name": name})).await {
                Ok(layer) => { self.select_annotation_layer(Some(layer.id)); self.set_annotation_layer(layer); self.error.set(None); }
                Err(message) => self.fail(message),
            }
        });
    }

    fn remove_annotation_layer(self) {
        let (Some(dataset), Some(id)) = (self.dataset.get_untracked(), self.annotation_layer.get_untracked()) else { return };
        let layer = self.annotation_layers.get_untracked().into_iter().find(|layer| layer.id == id);
        if layer.as_ref().is_some_and(|layer| layer.dirty)
            && !window().confirm_with_message("Unsaved annotations in this layer will be lost. Remove it from the session?").unwrap_or(false) {
            return;
        }
        let url = annotation_layer_url(&self.origin.get_untracked(), &dataset, id);
        spawn_local(async move {
            match delete_request(&url).await {
                Ok(()) => {
                    self.annotation_layers.update(|layers| layers.retain(|layer| layer.id != id));
                    self.select_annotation_layer(self.annotation_layers.get_untracked().first().map(|layer| layer.id));
                    self.annotation_styles.update_value(|styles| { styles.remove(&id); });
                    self.selected_annotation.set(None);
                    self.annotation_undo.update_value(|history| history.retain(|(layer, _)| *layer != id));
                    self.error.set(None);
                    self.request_volume();
                }
                Err(message) => self.fail(message),
            }
        });
    }

    fn add_annotation(self, mut annotation: Annotation) {
        let Some(dataset) = self.dataset.get_untracked() else { return };
        let origin = self.origin.get_untracked();
        let Some(layer_id) = self.annotation_layer.get_untracked() else {
            self.queued_annotations.update_value(|queue| queue.push(annotation));
            if self.annotation_create_inflight.get_value() { return; }
            self.annotation_create_inflight.set_value(true);
            spawn_local(async move {
                let url = annotation_layers_url(&origin, &dataset);
                let name = format!("manual_{}", js_sys::Date::now() as u64);
                match post_json::<AnnotationLayer, _>(&url, &serde_json::json!({"name": name})).await {
                    Ok(layer) => {
                        self.select_annotation_layer(Some(layer.id));
                        self.set_annotation_layer(layer);
                        let queued = self.queued_annotations.get_value();
                        self.queued_annotations.set_value(Vec::new());
                        for item in queued { self.add_annotation(item); }
                    }
                    Err(message) => self.fail(message),
                }
                self.annotation_create_inflight.set_value(false);
            });
            return;
        };
        self.remember_annotations();
        spawn_local(async move {
            let id = layer_id;
            annotation.label = self.annotation_class.get_untracked();
            annotation.object_type = self.annotation_object_type.get_untracked();
            annotation.dense_region = self.annotation_dense_region.get_untracked() && matches!(&annotation.geometry, Geometry::Polygon(_) | Geometry::MultiPolygon(_));
            if matches!(&annotation.geometry, Geometry::LineString(_) | Geometry::MultiLineString(_)) {
                annotation.stroke_width = self.annotation_stroke_width.get_untracked();
            }
            let url = annotation_layer_url(&origin, &dataset, id);
            match post_json::<Annotation, _>(&url, &annotation).await {
                Ok(stored) => {
                    self.annotation_layers.update(|layers| if let Some(layer) = layers.iter_mut().find(|layer| layer.id == id) {
                        layer.annotations.push(stored.clone()); layer.dirty = true;
                    });
                    self.selected_annotation.set(Some(stored.id));
                    self.error.set(None);
                    self.request_volume();
                }
                Err(message) => self.fail(message),
            }
        });
    }

    fn update_annotation(self, annotation: Annotation) {
        let (Some(dataset), Some(layer)) = (self.dataset.get_untracked(), self.annotation_layer.get_untracked()) else { return };
        let url = format!("{}/{}", annotation_layer_url(&self.origin.get_untracked(), &dataset, layer), annotation.id);
        spawn_local(async move {
            match put_json::<Annotation, _>(&url, &annotation).await {
                Ok(stored) => {
                    self.annotation_layers.update(|layers| if let Some(layer) = layers.iter_mut().find(|item| item.id == layer) {
                        if let Some(item) = layer.annotations.iter_mut().find(|item| item.id == stored.id) { *item = stored; }
                        layer.dirty = true;
                    });
                    self.error.set(None);
                    self.request_volume();
                }
                Err(message) => self.fail(message),
            }
        });
    }

    fn delete_annotation(self, id: u64) {
        let (Some(dataset), Some(layer)) = (self.dataset.get_untracked(), self.annotation_layer.get_untracked()) else { return };
        let url = format!("{}/{id}", annotation_layer_url(&self.origin.get_untracked(), &dataset, layer));
        self.remember_annotations();
        spawn_local(async move {
            match delete_request(&url).await {
                Ok(()) => {
                    self.annotation_layers.update(|layers| if let Some(layer) = layers.iter_mut().find(|item| item.id == layer) {
                        layer.annotations.retain(|item| item.id != id);
                        layer.dirty = true;
                    });
                    self.selected_annotation.set(None);
                    self.error.set(None);
                    self.request_volume();
                }
                Err(message) => self.fail(message),
            }
        });
    }

    fn save_annotations(self) {
        let (Some(dataset), Some(layer)) = (self.dataset.get_untracked(), self.annotation_layer.get_untracked()) else { return };
        let Some(target) = self.annotation_layers.get_untracked().into_iter().find(|item| item.id == layer).map(|item| item.save_target) else { return };
        let url = format!("{}/save-to", annotation_layer_url(&self.origin.get_untracked(), &dataset, layer));
        spawn_local(async move {
            match post_json::<AnnotationSaveReport, _>(&url, &serde_json::json!({"target": target})).await {
                Ok(report) => {
                    self.annotation_layers.update(|layers| if let Some(item) = layers.iter_mut().find(|item| item.id == layer) { item.dirty = false; });
                    self.status.set(format!("Saved {} annotations as {} to {}{}", report.rows, report.format, report.target,
                        if report.flattened > 0 { format!("; {} shapes reduced to boxes", report.flattened) } else { String::new() }));
                    self.error.set(None);
                }
                Err(message) => self.fail(message),
            }
        });
    }

    fn set_annotation_visibility(self, visible: bool) {
        let (Some(dataset), Some(layer)) = (self.dataset.get_untracked(), self.annotation_layer.get_untracked()) else { return };
        let url = format!("{}/visibility", annotation_layer_url(&self.origin.get_untracked(), &dataset, layer));
        spawn_local(async move {
            match put_json::<AnnotationLayer, _>(&url, &serde_json::json!({"visible": visible})).await {
                Ok(updated) => { self.set_annotation_layer(updated); self.error.set(None); self.request_volume(); }
                Err(message) => self.fail(message),
            }
        });
    }

    fn save_annotation_roi(self) {
        let (Some(dataset), Some(layer)) = (self.dataset.get_untracked(), self.annotation_layer.get_untracked()) else { return };
        let url = format!("{}/save-roi", annotation_layer_url(&self.origin.get_untracked(), &dataset, layer));
        spawn_local(async move {
            match post_json::<RoiSaveReport, _>(&url, &serde_json::json!({})).await {
                Ok(report) => {
                    self.status.set(format!("ROI table saved to {}; {} shapes reduced to boxes", report.target, report.flattened));
                    self.refresh_annotations();
                }
                Err(message) => self.fail(message),
            }
        });
    }

    fn import_annotation_roi(self, name: String) {
        let Some(dataset) = self.dataset.get_untracked() else { return };
        let url = annotation_roi_import_url(&self.origin.get_untracked(), &dataset, &name);
        spawn_local(async move {
            match post_json::<AnnotationLayer, _>(&url, &serde_json::json!({})).await {
                Ok(layer) => { self.select_annotation_layer(Some(layer.id)); self.set_annotation_layer(layer); self.error.set(None); self.request_volume(); }
                Err(message) => self.fail(message),
            }
        });
    }

    fn import_annotations(self, text: String) {
        let (Some(dataset), Some(layer)) = (self.dataset.get_untracked(), self.annotation_layer.get_untracked()) else { return };
        let url = format!("{}/geojson", annotation_layer_url(&self.origin.get_untracked(), &dataset, layer));
        self.remember_annotations();
        spawn_local(async move {
            match put_geojson(&url, text).await {
                Ok(updated) => { self.set_annotation_layer(updated); self.selected_annotation.set(None); self.error.set(None); self.request_volume(); }
                Err(message) => self.fail(message),
            }
        });
    }

    fn annotation_action(self, suffix: String) {
        let (Some(dataset), Some(layer)) = (self.dataset.get_untracked(), self.annotation_layer.get_untracked()) else { return };
        let url = format!("{}/{}", annotation_layer_url(&self.origin.get_untracked(), &dataset, layer), suffix);
        self.remember_annotations();
        spawn_local(async move {
            match post_empty(&url).await {
                Ok(()) => { self.refresh_annotations(); self.error.set(None); }
                Err(message) => self.fail(message),
            }
        });
    }

    fn refresh_settings(self) {
        let Some(dataset) = self.dataset.get() else { return };
        let url = settings_url(&self.origin.get(), &dataset);
        spawn_local(async move {
            match get_json::<SceneSettings>(&url).await {
                Ok(settings) => self.depth_scale.set(settings.depth_scale),
                Err(message) => self.fail(message),
            }
        });
    }

    /// The see-through depth: shown at once, sent latest-only, then the volume follows.
    pub fn set_depth_scale(self, scale: f32) {
        self.depth_scale.set(scale);
        if self.settings_inflight.get_value() {
            self.settings_dirty.set_value(Some(scale));
            return;
        }
        self.post_depth_scale(scale);
    }

    fn post_depth_scale(self, scale: f32) {
        let Some(dataset) = self.dataset.get_untracked() else { return };
        let url = settings_url(&self.origin.get_untracked(), &dataset);
        self.settings_inflight.set_value(true);
        self.update_busy();
        spawn_local(async move {
            match post_json::<SceneSettings, _>(&url, &SceneSettings { depth_scale: scale }).await {
                Ok(settings) => {
                    if self.settings_dirty.get_value().is_none() {
                        self.depth_scale.set(settings.depth_scale);
                    }
                    self.error.set(None);
                }
                Err(message) => self.fail(message),
            }
            self.settings_inflight.set_value(false);
            self.update_busy();
            if let Some(next) = self.settings_dirty.get_value() {
                self.settings_dirty.set_value(None);
                self.post_depth_scale(next);
            } else {
                self.request_volume();
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
        let zooms = self.zoom_2d.get_untracked();
        let Some(pane_index) = [Plane::Xy, Plane::Xz, Plane::Yz]
            .iter()
            .enumerate()
            .filter(|(_, &plane)| mode.shows_plane(plane))
            .max_by(|(left, _), (right, _)| zooms[*left].total_cmp(&zooms[*right]))
            .map(|(index, _)| index)
        else { return };
        if self.ortho_inflight.get_value().is_some() {
            self.ortho_dirty.set_value(true);
            return;
        }
        let (width, height) = self.ortho_panes[pane_index].get_untracked().map(|pane| physical_size(&pane)).unwrap_or((256, 256));
        let crosshair = self.voxel_shape.get_untracked().map(|_| self.crosshair.get_untracked());
        let focus_xyz = self.voxel_shape.get_untracked().map(|shape| normalized_focus_xyz(self.focus.get_untracked(), shape));
        let request_id = self.next_request_id();
        self.ortho_inflight.set_value(Some(request_id));
        self.ortho_dirty.set_value(false);
        self.update_busy();
        let request = FrameRequest {
            dataset,
            width,
            height,
            orbit_x: 0,
            orbit_y: 0,
            zoom: zooms[pane_index] as f32,
            focus_xyz,
            orientation: None,
            request_id,
            view: RenderView::Orthogonal,
            x: crosshair.map(|c| c[0]),
            y: crosshair.map(|c| c[1]),
            z: crosshair.map(|c| c[2]),
            slice_axis: [2, 1, 0][pane_index],
            slice_zooms: Some(zooms.map(|zoom| zoom as f32)),
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
        let (width, height) = self.volume_pane.get_untracked().map(|pane| preview_size(physical_size(&pane), self.volume_preview.get_value())).unwrap_or((256, 256));
        let camera = self.camera.get_untracked();
        let focus_xyz = self.voxel_shape.get_untracked().map(|shape| normalized_focus_xyz(self.focus.get_untracked(), shape));
        let request_id = self.next_request_id();
        self.volume_inflight.set_value(Some(request_id));
        self.volume_dirty.set_value(false);
        self.update_busy();
        let request = FrameRequest {
            dataset,
            width,
            height,
            orbit_x: 0,
            orbit_y: 0,
            zoom: camera.zoom,
            focus_xyz,
            orientation: Some(camera.orientation),
            request_id,
            view: RenderView::Volume,
            x: None,
            y: None,
            z: None,
            slice_axis: 2,
            slice_zooms: None,
        };
        self.socket_send(serde_json::to_string(&request).expect("FrameRequest serializes"));
    }

    fn render_in_browser(self, dataset: String) {
        if self.browser_busy.get_value() {
            self.browser_dirty.set_value(true);
            return;
        }
        let Some(canvas) = self.volume_canvas.get_untracked() else { return };
        let (width, height) = self.volume_pane.get_untracked().map(|pane| preview_size(physical_size(&pane), self.volume_preview.get_value())).unwrap_or((256, 256));
        let camera = self.camera.get_untracked();
        let focus_xyz = self.voxel_shape.get_untracked().map(|shape| normalized_focus_xyz(self.focus.get_untracked(), shape));
        let origin = self.origin.get_untracked();
        self.browser_busy.set_value(true);
        self.browser_dirty.set_value(false);
        self.update_busy();
        spawn_local(async move {
            let canvas: web_sys::HtmlCanvasElement = canvas.clone();
            let outcome = self.browser_frame(&origin, &dataset, &canvas, width, height, camera, focus_xyz).await;
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

    /// One frame by client-side residency: the plan and its locally expanded rays, then dispatch,
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
        focus_xyz: Option<[f32; 3]>,
    ) -> Result<String, String> {
        use newvolim_residency::{ChunkCache, ClientResidency, ScenePlan, StepOutcome};
        install_shader()?;
        let plan: ScenePlan = get_json(&scene_plan_url(origin, dataset, width, height, 0, 0, camera.zoom, Some(camera.orientation), focus_xyz, None)).await?;
        let mut ray_words = newvolim_residency::ray_words_for_plan(&plan)?;
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
                    let mut rgba: Vec<u8> = output.iter().take(pixels).flat_map(|word| word.to_le_bytes()).collect();
                    if self.annotation_layers.get_untracked().iter().any(|layer| layer.visible && !layer.annotations.is_empty()) {
                        let url = annotation_projection_url(origin, dataset, width, height, camera.zoom, camera.orientation, focus_xyz);
                        let words: Vec<u32> = get_json(&url).await?;
                        let primitives = projected_annotations_from_words(&words)?;
                        if !primitives.is_empty() {
                            let depths = output.get(pixels..pixels*2).ok_or("scene output has no paired depth")?
                                .iter().map(|bits| f32::from_bits(*bits)).collect();
                            let frame = palace_core::gpu::PortableFrameAttachments::new(width, height, rgba, depths)
                                .ok_or("scene colour and depth cannot form annotation attachments")?;
                            rgba = palace_core::gpu::PortableAnnotationCompositeInput::new(frame, primitives)
                                .ok_or("annotation projection exceeds compositor capacity")?
                                .composite_cpu().ok_or("annotation compositor rejected the frame")?.rgba;
                        }
                    }
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
                    let plan: ScenePlan = get_json(&scene_plan_url(origin, dataset, width, height, 0, 0, camera.zoom, Some(camera.orientation), focus_xyz, Some(&coarser))).await?;
                    ray_words = newvolim_residency::ray_words_for_plan(&plan)?;
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
            SocketReply::Orthogonal { request_id, render_ms, xy_base64, xz_base64, yz_base64, voxel_shape_xyz, crosshair_xyz, pyramid_levels, viewport, .. } => {
                if self.ortho_inflight.get_value() == Some(request_id) {
                    self.ortho_inflight.set_value(None);
                    let first = self.voxel_shape.get_untracked().is_none();
                    self.voxel_shape.set(Some(voxel_shape_xyz));
                    if first {
                        self.crosshair.set(crosshair_xyz);
                        self.focus.set(crosshair_xyz.map(|v| v as f64 + 0.5));
                        self.zoom_2d.set([1.0; 3]);
                        self.request_volume();
                    }
                    self.slices.set(Some(Slices {
                        xy: format!("data:image/png;base64,{xy_base64}"),
                        xz: format!("data:image/png;base64,{xz_base64}"),
                        yz: format!("data:image/png;base64,{yz_base64}"),
                        viewport,
                    }));
                    self.error.set(None);
                    self.status.set(match pyramid_levels {
                        Some([xy, xz, yz]) => format!("Slices XY L{xy}, XZ L{xz}, YZ L{yz} in {render_ms:.0} ms"),
                        None => format!("Slices in {render_ms:.0} ms"),
                    });
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

    /// Move the crosshair to a voxel (from the sliders or the orientation box): the focus goes
    /// to that voxel's centre.
    pub fn move_crosshair(self, next: [u32; 3]) {
        let Some(shape) = self.voxel_shape.get_untracked() else { return };
        let clamped = [next[0].min(shape[0].saturating_sub(1)), next[1].min(shape[1].saturating_sub(1)), next[2].min(shape[2].saturating_sub(1))];
        let next_focus = clamped.map(|v| v as f64 + 0.5);
        let moved = self.focus.get_untracked() != next_focus;
        self.focus.set(next_focus);
        if clamped != self.crosshair.get_untracked() {
            self.crosshair.set(clamped);
            self.request_orthogonal();
        }
        if moved { self.request_interactive_volume(); }
    }

    /// Move the focus continuously (a pan): the crosshair follows as its floor, and the slices
    /// are re-cut only when that integer changes.
    pub fn set_focus(self, next: [f64; 3]) {
        let Some(shape) = self.voxel_shape.get_untracked() else { return };
        let clamped: [f64; 3] = std::array::from_fn(|axis| next[axis].clamp(0.0, shape[axis].max(1) as f64));
        let moved = self.focus.get_untracked() != clamped;
        self.focus.set(clamped);
        let crosshair: [u32; 3] = std::array::from_fn(|axis| (clamped[axis].floor() as u32).min(shape[axis].saturating_sub(1)));
        if crosshair != self.crosshair.get_untracked() {
            self.crosshair.set(crosshair);
            self.request_orthogonal();
        }
        if moved { self.request_interactive_volume(); }
    }

    pub fn orbit_by(self, dx: i32, dy: i32) {
        if dx == 0 && dy == 0 { return; }
        self.camera.update(|camera| camera.orientation = drag_orientation(camera.orientation, dx, dy));
        self.request_interactive_volume();
    }

    pub fn zoom_by(self, factor: f32) {
        self.camera.update(|camera| camera.zoom = (camera.zoom * factor).clamp(ZOOM_RANGE.0, ZOOM_RANGE.1));
        self.request_interactive_volume();
    }

    fn request_interactive_volume(self) {
        self.volume_preview.set_value(true);
        let generation = self.volume_preview_generation.get_value().wrapping_add(1);
        self.volume_preview_generation.set_value(generation);
        self.request_volume();
        spawn_local(async move {
            gloo_timers::future::TimeoutFuture::new(INTERACTION_SETTLE_MS).await;
            if self.volume_preview_generation.get_value() == generation {
                self.volume_preview.set_value(false);
                self.request_volume();
            }
        });
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
        self.layout_tick.update(|tick| *tick += 1);
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

fn projected_annotations_from_words(words: &[u32]) -> Result<Vec<palace_core::gpu::ProjectedAnnotationPrimitive>, String> {
    if !words.len().is_multiple_of(13) { return Err("annotation projection has an incomplete record".into()); }
    words.chunks_exact(13).map(|record| {
        let color = record[2].to_be_bytes();
        let color = [color[1], color[2], color[3]];
        let vertices: [[f32; 3]; 3] = std::array::from_fn(|index| {
            std::array::from_fn(|axis| f32::from_bits(record[4 + index*3 + axis]))
        });
        let id = u64::from(record[1]);
        let primitive = match record[0] {
            1 => palace_core::gpu::ProjectedAnnotationPrimitive::point(id, color, f32::from_bits(record[3]), vertices[0]),
            2 => palace_core::gpu::ProjectedAnnotationPrimitive::segment(id, color, f32::from_bits(record[3]), vertices[0], vertices[1]),
            3 => palace_core::gpu::ProjectedAnnotationPrimitive::triangle(id, color, vertices),
            _ => return Err("annotation projection has an unknown primitive kind".into()),
        };
        primitive.ok_or_else(|| "annotation projection contains invalid coordinates".into())
    }).collect()
}

// ---- HTTP -------------------------------------------------------------------------------

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

async fn put_json<T: serde::de::DeserializeOwned, B: serde::Serialize>(url: &str, body: &B) -> Result<T, String> {
    let request = gloo_net::http::Request::put(url).json(body).map_err(|error| format!("PUT {url}: {error}"))?;
    let response = request.send().await.map_err(|error| format!("PUT {url}: {error}"))?;
    if !response.ok() { return Err(format!("PUT {url}: {} {}", response.status(), response.text().await.unwrap_or_default())); }
    response.json::<T>().await.map_err(|error| format!("PUT {url}: {error}"))
}

async fn put_geojson(url: &str, text: String) -> Result<AnnotationLayer, String> {
    let response = gloo_net::http::Request::put(url).header("Content-Type", "application/geo+json").body(text)
        .map_err(|error| format!("PUT {url}: {error}"))?.send().await.map_err(|error| format!("PUT {url}: {error}"))?;
    if !response.ok() { return Err(format!("PUT {url}: {} {}", response.status(), response.text().await.unwrap_or_default())); }
    response.json::<AnnotationLayer>().await.map_err(|error| format!("PUT {url}: {error}"))
}

async fn delete_request(url: &str) -> Result<(), String> {
    let response = gloo_net::http::Request::delete(url).send().await.map_err(|error| format!("DELETE {url}: {error}"))?;
    if !response.ok() { return Err(format!("DELETE {url}: {} {}", response.status(), response.text().await.unwrap_or_default())); }
    Ok(())
}

async fn post_empty(url: &str) -> Result<(), String> {
    let response = gloo_net::http::Request::post(url).send().await.map_err(|error| format!("POST {url}: {error}"))?;
    if !response.ok() { return Err(format!("POST {url}: {} {}", response.status(), response.text().await.unwrap_or_default())); }
    Ok(())
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

fn preview_size(size: (u32, u32), preview: bool) -> (u32, u32) {
    if preview { (size.0.div_ceil(2), size.1.div_ceil(2)) } else { size }
}


// ---- components ---------------------------------------------------------------------------

#[component]
pub fn App() -> impl IntoView {
    let session = Session::new();
    provide_context(session);
    session.connect();
    window_event_listener(leptos::ev::resize, move |_| {
        session.layout_tick.update(|tick| *tick += 1);
        if session.dataset.get_untracked().is_some() {
            session.request_orthogonal();
            session.request_volume();
        }
    });
    window_event_listener(leptos::ev::keydown, move |ev| {
        let tag = ev.target().and_then(|target| target.dyn_into::<web_sys::Element>().ok())
            .map(|element| element.tag_name()).unwrap_or_default();
        if matches!(tag.as_str(), "INPUT" | "TEXTAREA" | "SELECT") { return; }
        if (ev.ctrl_key() || ev.meta_key()) && ev.key().eq_ignore_ascii_case("z") {
            ev.prevent_default(); session.undo_annotations();
        } else if ev.key() == "Escape" {
            session.annotation_draft.set(Vec::new()); session.annotation_tool.set(AnnotationTool::Pan);
        } else if ev.key() == "Enter" && matches!(session.annotation_tool.get_untracked(), AnnotationTool::Polygon | AnnotationTool::Polyline) {
            session.finish_annotation_draft();
        } else if ev.key() == "Delete" && session.annotation_tool.get_untracked() == AnnotationTool::Select {
            if let Some(id) = session.selected_annotation.get_untracked() { session.delete_annotation(id); }
        }
    });
    window_event_listener(leptos::ev::beforeunload, move |ev| {
        if session.annotation_layers.get_untracked().iter().any(|layer| layer.dirty) {
            ev.prevent_default();
            ev.set_return_value("Unsaved annotations");
        }
    });
    view! {
        <div class="workspace">
            <nav class="workspace-tabs">
                <span class="brand">"newvolim"</span>
                <Show when=move || session.dataset.get().is_some()>
                    <span class="tab">
                        {move || session.dataset.get().unwrap_or_default()}
                        <button class="tab-close" title="Close dataset" on:click=move |_| { session.close(); }>"✕"</button>
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
                    <div class="tool-group annotation-tools" title="Draw annotations in the XY slice">
                        {[
                            AnnotationTool::Pan, AnnotationTool::Select, AnnotationTool::Point,
                            AnnotationTool::Rectangle, AnnotationTool::Ellipse, AnnotationTool::Polygon,
                            AnnotationTool::FreehandRegion, AnnotationTool::Polyline, AnnotationTool::FreehandLine,
                        ].into_iter().map(|tool| view! {
                            <button class="tool-button" class:active=move || session.annotation_tool.get() == tool
                                on:click=move |_| { session.annotation_tool.set(tool); session.annotation_draft.set(Vec::new()); }>{tool.label()}</button>
                        }).collect_view()}
                        <button class="tool-button" on:click=move |_| session.undo_annotations()>"Undo"</button>
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
                <span class="readout">{move || format!("3D zoom {:.2}", session.camera.get().zoom)}</span>
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

/// One slice pane as a 2-D camera: the slice image is placed so the session's focus sits at
/// the pane's centre at the pane's zoom; drag pans (moving the focus, hence the other panes'
/// cuts), wheel zooms about the cursor. The crosshair is implicit — the centre — and not drawn.
fn annotation_geometry(tool: AnnotationTool, points: &[[f64; 2]]) -> Option<(Geometry, bool)> {
    let first = *points.first()?;
    let last = *points.last()?;
    match tool {
        AnnotationTool::Point => Some((Geometry::Point(first), false)),
        AnnotationTool::Rectangle => Some((Geometry::rect(first[0], first[1], last[0], last[1]), false)),
        AnnotationTool::Ellipse => {
            let (cx, cy) = ((first[0] + last[0]) * 0.5, (first[1] + last[1]) * 0.5);
            let (rx, ry) = ((first[0] - last[0]).abs() * 0.5, (first[1] - last[1]).abs() * 0.5);
            if rx < 0.01 || ry < 0.01 { return None; }
            let mut ring = (0..48).map(|step| {
                let a = std::f64::consts::TAU * step as f64 / 48.0;
                [cx + rx * a.cos(), cy + ry * a.sin()]
            }).collect::<Vec<_>>();
            ring.push(ring[0]);
            Some((Geometry::Polygon(vec![ring]), true))
        }
        AnnotationTool::Polygon | AnnotationTool::FreehandRegion => {
            let mut ring = simplify_annotation_points(points);
            if ring.len() < 3 { return None; }
            if ring.first() != ring.last() { ring.push(ring[0]); }
            Some((Geometry::Polygon(vec![ring]), false))
        }
        AnnotationTool::Polyline | AnnotationTool::FreehandLine => {
            let path = simplify_annotation_points(points);
            (path.len() >= 2).then_some((Geometry::LineString(path), false))
        }
        AnnotationTool::Pan | AnnotationTool::Select => None,
    }
}

fn simplify_annotation_points(points: &[[f64; 2]]) -> Vec<[f64; 2]> {
    let mut out = Vec::new();
    for point in points {
        if out.last().is_none_or(|last: &[f64; 2]| (point[0] - last[0]).hypot(point[1] - last[1]) >= 0.5) {
            out.push(*point);
        }
    }
    out
}

fn annotation_svg_path(geometry: &Geometry, point_radius: f64) -> String {
    let mut result = String::new();
    for point in geometry.markers() {
        let (x, y, r) = (point[0], point[1], point_radius);
        result.push_str(&format!("M {} {} a {r} {r} 0 1 0 {} 0 a {r} {r} 0 1 0 {} 0 ", x-r, y, 2.0*r, -2.0*r));
    }
    for path in geometry.outlines() {
        for (index, point) in path.iter().enumerate() {
            result.push_str(&format!("{} {} {} ", if index == 0 { "M" } else { "L" }, point[0], point[1]));
        }
    }
    result
}

fn annotation_z_fade(item: &Annotation, z: i32, slab: f64) -> f64 {
    let end = item.plane.z as i64 + item.z_extent as i64;
    let distance = if (z as i64) < item.plane.z as i64 {
        item.plane.z as i64 - z as i64
    } else if z as i64 > end { z as i64 - end } else { 0 };
    if slab > 0.0 { (1.0 - distance as f64 / slab).clamp(0.0, 1.0) } else { 1.0 }
}

#[derive(Clone)]
enum AnnotationHandle { Body, Vertex(usize, usize), Corner([f64; 2]) }

#[derive(Clone)]
struct AnnotationDrag { original: Annotation, start: [f64; 2], handle: AnnotationHandle }

fn selected_handle(annotation: &Annotation, at: [f64; 2], pad: f64) -> AnnotationHandle {
    if annotation.is_ellipse || is_rectangle_annotation(annotation) {
        if let Some([x0, y0, x1, y1]) = annotation.bounds() {
            for (corner, opposite) in [([x0,y0],[x1,y1]), ([x1,y0],[x0,y1]), ([x1,y1],[x0,y0]), ([x0,y1],[x1,y0])] {
                if (corner[0]-at[0]).hypot(corner[1]-at[1]) <= pad { return AnnotationHandle::Corner(opposite); }
            }
        }
    } else {
        for (path_index, path) in annotation.geometry.outlines().iter().enumerate() {
            for (vertex_index, point) in path.iter().enumerate() {
                if (point[0]-at[0]).hypot(point[1]-at[1]) <= pad { return AnnotationHandle::Vertex(path_index, vertex_index); }
            }
        }
    }
    AnnotationHandle::Body
}

fn closest_annotation_edge(annotation: &Annotation, at: [f64; 2], pad: f64) -> Option<(usize, usize)> {
    let mut closest = None;
    let mut best = pad * pad;
    for (path_index, path) in annotation.geometry.outlines().iter().enumerate() {
        for (edge_index, pair) in path.windows(2).enumerate() {
            let (a, b) = (pair[0], pair[1]);
            let (dx, dy) = (b[0]-a[0], b[1]-a[1]);
            let length = dx*dx + dy*dy;
            if length <= 0.0 { continue; }
            let t = (((at[0]-a[0])*dx + (at[1]-a[1])*dy)/length).clamp(0.0, 1.0);
            let distance = (at[0]-a[0]-t*dx).powi(2) + (at[1]-a[1]-t*dy).powi(2);
            if distance < best { best = distance; closest = Some((path_index, edge_index)); }
        }
    }
    closest
}

fn is_rectangle_annotation(annotation: &Annotation) -> bool {
    match &annotation.geometry {
        Geometry::Polygon(rings) if rings.len() == 1 && rings[0].len() == 5 => {
            let p = &rings[0];
            p[0] == p[4] && p[0][1] == p[1][1] && p[1][0] == p[2][0] && p[2][1] == p[3][1] && p[3][0] == p[0][0]
        }
        _ => false,
    }
}

fn move_annotation_drag(drag: &AnnotationDrag, at: [f64; 2]) -> Annotation {
    let mut next = drag.original.clone();
    let dx = at[0] - drag.start[0];
    let dy = at[1] - drag.start[1];
    match drag.handle {
        AnnotationHandle::Body => next.geometry.translate(dx, dy),
        AnnotationHandle::Vertex(path, vertex) => { next.geometry.move_vertex(path, vertex, dx, dy); }
        AnnotationHandle::Corner(opposite) => {
            let sx = if (drag.start[0]-opposite[0]).abs() > 1e-6 { (at[0]-opposite[0])/(drag.start[0]-opposite[0]) } else { 1.0 };
            let sy = if (drag.start[1]-opposite[1]).abs() > 1e-6 { (at[1]-opposite[1])/(drag.start[1]-opposite[1]) } else { 1.0 };
            next.geometry.scale_about(opposite[0], opposite[1], sx.max(0.01), sy.max(0.01));
        }
    }
    next
}

#[component]
fn OrthoPane(plane: Plane) -> impl IntoView {
    let session = expect_context::<Session>();
    let (h_axis, v_axis, _) = plane.axes();
    let pane_index = match plane {
        Plane::Xy => 0,
        Plane::Xz => 1,
        Plane::Yz => 2,
    };
    let node_ref = session.ortho_panes[pane_index];
    let hidden = move || !session.view_mode.get().shows_plane(plane);
    let image = move || {
        session.slices.get().map(|slices| match plane {
            Plane::Xy => slices.xy,
            Plane::Xz => slices.xz,
            Plane::Yz => slices.yz,
        })
    };
    let is_viewport = move || session.slices.get().is_some_and(|slices| slices.viewport);
    // The pane's size in CSS pixels and the scale from voxels to pixels at zoom 1 (the whole
    // slice fits, square voxels).
    let geometry = move || -> Option<(f64, f64, f64)> {
        let _ = session.layout_tick.get();
        let pane = node_ref.get()?;
        let rect = pane.get_bounding_client_rect();
        let shape = session.voxel_shape.get()?;
        let dims = (shape[h_axis].max(1) as f64, shape[v_axis].max(1) as f64);
        let fit = (rect.width() / dims.0).min(rect.height() / dims.1);
        (fit.is_finite() && fit > 0.0).then_some((rect.width(), rect.height(), fit))
    };
    let placement = move || -> Option<(f64, f64, f64, f64)> {
        let (width, height, fit) = geometry()?;
        let shape = session.voxel_shape.get()?;
        let scale = fit * session.zoom_2d.get()[pane_index];
        let focus = session.focus.get();
        Some((
            width * 0.5 - focus[h_axis] * scale,
            height * 0.5 - focus[v_axis] * scale,
            shape[h_axis] as f64 * scale,
            shape[v_axis] as f64 * scale,
        ))
    };
    let last = StoredValue::new(None::<(i32, i32)>);
    let annotation_drag = StoredValue::new(None::<AnnotationDrag>);
    let world_at = move |ev: &web_sys::PointerEvent| -> Option<([f64; 2], f64)> {
        if plane != Plane::Xy { return None; }
        let (width, height, fit) = geometry()?;
        let pane = node_ref.get_untracked()?;
        let rect = pane.get_bounding_client_rect();
        let scale = fit * session.zoom_2d.get_untracked()[pane_index];
        let focus = session.focus.get_untracked();
        Some(([
            focus[0] + (ev.client_x() as f64 - rect.left() - width * 0.5) / scale,
            focus[1] + (ev.client_y() as f64 - rect.top() - height * 0.5) / scale,
        ], scale))
    };
    let on_down = move |ev: web_sys::PointerEvent| {
        if ev.button() != 0 {
            return;
        }
        let tool = session.annotation_tool.get_untracked();
        if plane == Plane::Xy && tool != AnnotationTool::Pan {
            let Some((at, scale)) = world_at(&ev) else { return };
            if let Some(target) = ev.current_target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()) {
                let _ = target.set_pointer_capture(ev.pointer_id());
            }
            match tool {
                AnnotationTool::Select => {
                    let z = session.crosshair.get_untracked()[2] as i32;
                    let visible = session.active_annotations().into_iter().filter(|item| item.at_plane(z, 0)).collect::<Vec<_>>();
                    let selected = session.selected_annotation.get_untracked().and_then(|id| visible.iter().find(|item| item.id == id)).cloned();
                    let chosen = selected.filter(|item| item.contains(at[0], at[1], 8.0/scale))
                        .or_else(|| qupath::pick_annotation(&visible, at[0], at[1], 8.0/scale).cloned());
                    session.selected_annotation.set(chosen.as_ref().map(|item| item.id));
                    if let Some(item) = chosen.filter(|item| !item.locked) {
                        let handle = selected_handle(&item, at, 8.0/scale);
                        if ev.alt_key() {
                            if let Some((path, edge)) = closest_annotation_edge(&item, at, 8.0/scale) {
                                let mut edited = item;
                                if edited.geometry.insert_vertex(path, edge, at) { session.remember_annotations(); session.update_annotation(edited); }
                            }
                        } else if ev.shift_key() {
                            if let AnnotationHandle::Vertex(path, vertex) = handle {
                                let mut edited = item;
                                if edited.geometry.remove_vertex(path, vertex) { session.remember_annotations(); session.update_annotation(edited); }
                            }
                        } else {
                            session.remember_annotations();
                            annotation_drag.set_value(Some(AnnotationDrag { original: item, start: at, handle }));
                        }
                    }
                }
                AnnotationTool::Point => session.add_annotation(Annotation { geometry: Geometry::Point(at), plane: AnnotationPlane::at(session.crosshair.get_untracked()[2] as i32, 0), ..Annotation::default() }),
                AnnotationTool::Polygon | AnnotationTool::Polyline => {
                    let mut points = session.annotation_draft.get_untracked();
                    if tool == AnnotationTool::Polygon && points.len() >= 3 && (points[0][0]-at[0]).hypot(points[0][1]-at[1]) < 8.0/scale {
                        session.finish_annotation_draft();
                    } else { points.push(at); session.annotation_draft.set(points); }
                }
                AnnotationTool::Rectangle | AnnotationTool::Ellipse => session.annotation_draft.set(vec![at, at]),
                AnnotationTool::FreehandRegion | AnnotationTool::FreehandLine => session.annotation_draft.set(vec![at]),
                AnnotationTool::Pan => {}
            }
            return;
        }
        last.set_value(Some((ev.client_x(), ev.client_y())));
        if let Some(target) = ev.current_target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()) {
            let _ = target.set_pointer_capture(ev.pointer_id());
        }
    };
    let on_move = move |ev: web_sys::PointerEvent| {
        let tool = session.annotation_tool.get_untracked();
        if plane == Plane::Xy && tool != AnnotationTool::Pan {
            if ev.buttons() & 1 == 0 { return; }
            let Some((at, _)) = world_at(&ev) else { return };
            match tool {
                AnnotationTool::Select => if let Some(drag) = annotation_drag.get_value() {
                    let edited = move_annotation_drag(&drag, at);
                    session.annotation_layers.update(|layers| if let Some(layer) = layers.iter_mut().find(|layer| Some(layer.id) == session.annotation_layer.get_untracked()) {
                        if let Some(item) = layer.annotations.iter_mut().find(|item| item.id == edited.id) { *item = edited; }
                    });
                },
                AnnotationTool::Rectangle | AnnotationTool::Ellipse => session.annotation_draft.update(|points| if points.len() == 2 { points[1] = at; }),
                AnnotationTool::FreehandRegion | AnnotationTool::FreehandLine => session.annotation_draft.update(|points| {
                    if points.last().is_none_or(|last| (last[0]-at[0]).hypot(last[1]-at[1]) >= 0.5) { points.push(at); }
                }),
                _ => {}
            }
            return;
        }
        let Some((lx, ly)) = last.get_value() else { return };
        if ev.buttons() & 1 == 0 {
            return;
        }
        let (x, y) = (ev.client_x(), ev.client_y());
        last.set_value(Some((x, y)));
        let Some((_, _, fit)) = geometry() else { return };
        let scale = fit * session.zoom_2d.get_untracked()[pane_index];
        let mut focus = session.focus.get_untracked();
        focus[h_axis] -= (x - lx) as f64 / scale;
        focus[v_axis] -= (y - ly) as f64 / scale;
        session.set_focus(focus);
    };
    let on_up = move |ev: web_sys::PointerEvent| {
        last.set_value(None);
        if plane != Plane::Xy { return; }
        let tool = session.annotation_tool.get_untracked();
        if tool == AnnotationTool::Select {
            if let Some(drag) = annotation_drag.get_value() {
                annotation_drag.set_value(None);
                if let Some((at, _)) = world_at(&ev) {
                    if (at[0]-drag.start[0]).hypot(at[1]-drag.start[1]) > 1e-6 { session.update_annotation(move_annotation_drag(&drag, at)); }
                }
            }
        } else if matches!(tool, AnnotationTool::Rectangle | AnnotationTool::Ellipse | AnnotationTool::FreehandRegion | AnnotationTool::FreehandLine) {
            if session.annotation_draft.get_untracked().len() > 1 { session.finish_annotation_draft(); }
        }
    };
    let on_double = move |ev: web_sys::MouseEvent| {
        if plane == Plane::Xy && matches!(session.annotation_tool.get_untracked(), AnnotationTool::Polygon | AnnotationTool::Polyline) {
            ev.prevent_default();
            session.finish_annotation_draft();
        }
    };
    let on_wheel = move |ev: web_sys::WheelEvent| {
        ev.prevent_default();
        let Some((width, height, fit)) = geometry() else { return };
        let Some(pane) = node_ref.get_untracked() else { return };
        let rect = pane.get_bounding_client_rect();
        let cursor = (ev.client_x() as f64 - rect.left() - width * 0.5, ev.client_y() as f64 - rect.top() - height * 0.5);
        let old_zoom = session.zoom_2d.get_untracked()[pane_index];
        let new_zoom = (old_zoom * (1.0 - ev.delta_y() * 0.001)).clamp(0.25, 64.0);
        // The voxel under the cursor stays under the cursor.
        let (old_scale, new_scale) = (fit * old_zoom, fit * new_zoom);
        let mut focus = session.focus.get_untracked();
        focus[h_axis] += cursor.0 / old_scale - cursor.0 / new_scale;
        focus[v_axis] += cursor.1 / old_scale - cursor.1 / new_scale;
        session.zoom_2d.update(|zoom| zoom[pane_index] = new_zoom);
        let old_crosshair = session.crosshair.get_untracked();
        session.set_focus(focus);
        // A wheel step at the pane centre does not move the crosshair, but it can still cross a
        // pyramid threshold and must ask the server for the newly appropriate source level.
        if session.crosshair.get_untracked() == old_crosshair {
            session.request_orthogonal();
        }
    };
    view! {
        <div
            class="pane ortho"
            class:hidden-pane=hidden
            node_ref=node_ref
            on:pointerdown=on_down
            on:pointermove=on_move
            on:pointerup=on_up
            on:pointercancel=on_up
            on:dblclick=on_double
            on:wheel=on_wheel
        >
            {move || match (image(), is_viewport(), placement()) {
                (Some(src), true, _) => view! { <img class="pane-image" src=src alt=plane.label() draggable="false"/> }.into_any(),
                (Some(src), false, Some((left, top, width, height))) => view! {
                    <img class="slice-image" src=src alt=plane.label() draggable="false"
                        style:left=format!("{left}px") style:top=format!("{top}px")
                        style:width=format!("{width}px") style:height=format!("{height}px")/>
                }.into_any(),
                (Some(src), false, None) => view! { <img class="pane-image" src=src alt=plane.label() draggable="false"/> }.into_any(),
                (None, _, _) => view! { <div class="pane-empty">"waiting for slices…"</div> }.into_any(),
            }}
            {move || {
                let (Some(shape), Some((left, top, width, height))) = (session.voxel_shape.get(), placement()) else { return view! { <span></span> }.into_any() };
                if plane != Plane::Xy { return view! { <span></span> }.into_any(); }
                let scale = width / shape[0].max(1) as f64;
                let z = session.crosshair.get()[2] as i32;
                let selected = session.selected_annotation.get();
                let layers = session.annotation_layers.get();
                let active_id = session.annotation_layer.get();
                let current_style = session.current_annotation_style();
                let saved_styles = session.annotation_styles.get_value();
                let visible = layers.into_iter().filter(|layer| layer.visible).flat_map(|layer| {
                    let active = Some(layer.id) == active_id;
                    let style = if active { current_style.clone() } else { saved_styles.get(&layer.id).cloned().unwrap_or_default() };
                    layer.annotations.into_iter().filter_map(move |item| {
                        (item.at_plane(item.plane.z, 0) && annotation_z_fade(&item, z, style.slab) > 0.0
                            && style.filter.as_ref().is_none_or(|name| &item.label == name))
                            .then(|| (active, item, style.clone()))
                    })
                }).collect::<Vec<_>>();
                let draft = session.annotation_draft.get();
                let draft_path = if draft.is_empty() { String::new() } else {
                    // Preview drag-defined shapes with the same geometry that will be saved.
                    // Treating every two-point draft as a line made a rectangle appear as a
                    // diagonal until pointer-up, even though pointer-up stored a rectangle.
                    let tool = session.annotation_tool.get();
                    let geometry = annotation_geometry(tool, &draft).map(|(geometry, _)| geometry)
                        .unwrap_or_else(|| if draft.len() == 1 { Geometry::Point(draft[0]) } else { Geometry::LineString(draft) });
                    annotation_svg_path(&geometry, current_style.point_size * 0.5/scale)
                };
                view! {
                    <svg class="annotation-overlay" style:left=format!("{left}px") style:top=format!("{top}px")
                        style:width=format!("{width}px") style:height=format!("{height}px")
                        viewBox=format!("0 0 {} {}", shape[0], shape[1]) preserveAspectRatio="none">
                        {visible.into_iter().map(|(active, item, style)| {
                            let is_selected = active && selected == Some(item.id) && item.at_plane(z, 0);
                            let color = item.effective_color().unwrap_or_else(|| if style.color_by_class && !item.label.is_empty() { qupath::class_color(&item.label) } else { style.class_color });
                            let stroke = format!("rgb({},{},{})", color[0], color[1], color[2]);
                            let stroke_width = item.stroke_width.map(|world| (world * scale).max(0.5)).unwrap_or(2.0);
                            let fill_color = if style.filled && matches!(item.geometry, Geometry::Polygon(_) | Geometry::MultiPolygon(_)) { stroke.clone() } else { "none".into() };
                            let radius = if style.world_radius { style.class_radii.get(&item.label).copied().unwrap_or(style.radius) }
                                else { style.point_size * 0.5/scale };
                            let d = annotation_svg_path(&item.geometry, radius);
                            let nucleus_path = item.nucleus.as_ref().map(|nucleus| annotation_svg_path(nucleus, radius)).unwrap_or_default();
                            let alpha = style.opacity * annotation_z_fade(&item, z, style.slab);
                            let handles = if is_selected && session.annotation_tool.get_untracked() == AnnotationTool::Select {
                                if item.is_ellipse || is_rectangle_annotation(&item) {
                                    item.bounds().map(|[x0,y0,x1,y1]| vec![[x0,y0],[x1,y0],[x1,y1],[x0,y1]]).unwrap_or_default()
                                } else { item.geometry.outlines().into_iter().flatten().collect() }
                            } else { Vec::new() };
                            view! {
                                <g>
                                    <path class="annotation-shape" class:selected=is_selected d=d stroke=stroke.clone()
                                        stroke-width=stroke_width.to_string() fill=fill_color fill-opacity="0.19" fill-rule="evenodd"
                                        opacity=alpha.to_string()/>
                                    <path class="annotation-shape" class:selected=is_selected d=nucleus_path stroke=stroke
                                        stroke-width=stroke_width.to_string() fill="none" opacity=alpha.to_string()/>
                                    {handles.into_iter().map(|point| view! {
                                        <circle class="annotation-handle" cx=point[0].to_string() cy=point[1].to_string() r=(4.0/scale).to_string()/>
                                    }).collect_view()}
                                </g>
                            }
                        }).collect_view()}
                        <path class="annotation-shape selected" d=draft_path stroke="#ffd848" stroke-width="2" fill="none"/>
                    </svg>
                }.into_any()
            }}
            <span class="pane-label">{plane.label()}</span>
            <span class="pane-zoom">{move || format!("{:.2}×", session.zoom_2d.get()[pane_index])}</span>
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
        // Camera zoom scales eye distance: smaller values bring the volume closer.
        session.zoom_by(if ev.delta_y() < 0.0 { 1.0 / 1.1 } else { 1.1 });
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
            <div class="layer-block">
                <h3>"Scene"</h3>
                <div class="slider-row" title="How far light penetrates: a multiplier on the distance over which an opaque voxel absorbs everything (the scene diagonal / 256 at 1×). Larger sees deeper.">
                    <span>"Depth"</span>
                    <input
                        type="range"
                        min="-1"
                        max="2"
                        step="0.02"
                        prop:value=move || slider_from_depth_scale(session.depth_scale.get()).to_string()
                        on:input=move |ev| {
                            if let Ok(position) = event_target_value(&ev).parse::<f32>() {
                                session.set_depth_scale(depth_scale_from_slider(position));
                            }
                        }
                    />
                    <span class="slider-value">{move || format!("{:.2}×", session.depth_scale.get())}</span>
                </div>
                <div class="hint">"Each channel's window start is its transparency cutoff and its opacity scales alpha; depth changes how far the ray sees before it saturates."</div>
            </div>
            <div class="layer-block add-layer">
                <AnnotationControls/>
            </div>
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
fn AnnotationControls() -> impl IntoView {
    let session = expect_context::<Session>();
    let new_name = RwSignal::new("manual".to_string());
    let import_text = RwSignal::new(String::new());
    let roi_choice = RwSignal::new(String::new());
    let active = move || session.annotation_layers.get().into_iter().find(|layer| Some(layer.id) == session.annotation_layer.get());
    let selected = move || active().and_then(|layer| layer.annotations.into_iter().find(|item| Some(item.id) == session.selected_annotation.get()));
    view! {
        <h3>"Annotations"</h3>
        <div class="hint">"Draw in XY. Double-click or Enter finishes a polygon or line. In Select, drag to edit, Alt-click an edge to add a vertex, Shift-click a vertex to remove it. Escape cancels; Ctrl+Z undoes."</div>
        <div class="row">
            <input type="text" class="annotation-name" aria-label="New annotation layer name" prop:value=move || new_name.get()
                on:input=move |ev| new_name.set(event_target_value(&ev)) />
            <button class="plain-button" on:click=move |_| session.create_annotation_layer(new_name.get_untracked())>"New layer"</button>
        </div>
        <Show when=move || !session.annotation_roi_tables.get().is_empty()>
            <div class="row">
                <select aria-label="Stored ROI table" on:change=move |ev| roi_choice.set(event_target_value(&ev))>
                    <option value="">"ROI table…"</option>
                    {move || session.annotation_roi_tables.get().into_iter().map(|table| view! {
                        <option value=table.name.clone() disabled=!table.supported>{format!("{} ({})", table.name, table.backend)}</option>
                    }).collect_view()}
                </select>
                <button class="plain-button" disabled=move || roi_choice.get().is_empty()
                    on:click=move |_| session.import_annotation_roi(roi_choice.get_untracked())>"Import"</button>
            </div>
        </Show>
        <Show when=move || !session.annotation_layers.get().is_empty()>
            <div class="row">
                <select aria-label="Active annotation layer" on:change=move |ev| {
                    session.select_annotation_layer(event_target_value(&ev).parse().ok());
                }>
                    {move || session.annotation_layers.get().into_iter().map(|layer| view! {
                        <option value=layer.id.to_string() prop:selected=move || session.annotation_layer.get() == Some(layer.id)>{layer.name}</option>
                    }).collect_view()}
                </select>
                <button class="plain-button" title="Remove this layer from the session; saved files stay in the dataset"
                    on:click=move |_| session.remove_annotation_layer()>"Remove layer"</button>
            </div>
            <div class="row">
                <input type="text" class="annotation-name" aria-label="Annotation save target"
                    title="Within this dataset: annotations/<name> preserves all geometry; tables/<name> writes ROI boxes"
                    prop:value=move || active().map(|layer| layer.save_target).unwrap_or_default()
                    on:input=move |ev| {
                        let target = event_target_value(&ev);
                        session.annotation_layers.update(|layers| if let Some(layer) = layers.iter_mut().find(|layer| Some(layer.id) == session.annotation_layer.get_untracked()) { layer.save_target = target; });
                    } />
                <button class="plain-button" on:click=move |_| session.save_annotations()>"Save"</button>
            </div>
            <label class="hint"><input type="checkbox" prop:checked=move || active().is_some_and(|layer| layer.visible)
                on:change=move |ev| session.set_annotation_visibility(event_target_checked(&ev)) />" Show layer"</label>
            <div class="hint">{move || active().map(|layer| format!("{} shapes{}", layer.annotations.len(), if layer.dirty { " • unsaved" } else { "" })).unwrap_or_default()}</div>
            <div class="row">
                <label>"Class" <input type="text" aria-label="Class for new annotations" prop:value=move || session.annotation_class.get()
                    on:input=move |ev| session.annotation_class.set(event_target_value(&ev)) /></label>
            </div>
            <div class="row">
                <label>"Type" <select aria-label="Object type for new annotations" on:change=move |ev| session.annotation_object_type.set(ObjectType::parse(&event_target_value(&ev)))>
                    <option value="annotation">"annotation"</option><option value="detection">"detection"</option>
                    <option value="cell">"cell"</option><option value="tile">"tile"</option><option value="tmaCore">"TMA core"</option>
                </select></label>
                <label><input type="checkbox" prop:checked=move || session.annotation_filled.get()
                    on:change=move |ev| session.annotation_filled.set(event_target_checked(&ev)) />" Fill"</label>
                <label><input type="checkbox" prop:checked=move || session.annotation_color_by_class.get()
                    on:change=move |ev| session.annotation_color_by_class.set(event_target_checked(&ev)) />" Color by class"</label>
            </div>
            <div class="row">
                <label>"Opacity" <input type="number" min="0" max="1" step="0.05" aria-label="Annotation opacity"
                    prop:value=move || session.annotation_opacity.get().to_string()
                    on:change=move |ev| { if let Ok(value) = event_target_value(&ev).parse::<f64>() { session.annotation_opacity.set(value.clamp(0.0, 1.0)); } } /></label>
                <label>"Point size" <input type="number" min="2" max="40" step="1" aria-label="Annotation point size"
                    prop:value=move || session.annotation_point_size.get().to_string()
                    on:change=move |ev| { if let Ok(value) = event_target_value(&ev).parse::<f64>() { session.annotation_point_size.set(value.clamp(2.0, 40.0)); } } /></label>
                <label>"Z slab" <input type="number" min="0" max="64" step="1" aria-label="Annotation Z slab"
                    prop:value=move || session.annotation_slab.get().to_string()
                    on:change=move |ev| { if let Ok(value) = event_target_value(&ev).parse::<f64>() { session.annotation_slab.set(value.clamp(0.0, 64.0)); } } /></label>
            </div>
            <div class="row">
                <label>"Layer color" <input type="color" aria-label="Annotation layer color" prop:value=move || color_hex(session.annotation_class_color.get())
                    on:input=move |ev| if let Some(color) = parse_color_hex(&event_target_value(&ev)) { session.annotation_class_color.set(color); } /></label>
                <label><input type="checkbox" prop:checked=move || session.annotation_dense_region.get()
                    on:change=move |ev| session.annotation_dense_region.set(event_target_checked(&ev)) />" Dense region"</label>
            </div>
            <div class="row">
                <label>"Line width" <input type="number" min="0" step="0.5" aria-label="Width for new lines"
                    placeholder="geometric" prop:value=move || session.annotation_stroke_width.get().map(|value| value.to_string()).unwrap_or_default()
                    on:change=move |ev| session.annotation_stroke_width.set(event_target_value(&ev).parse::<f64>().ok().filter(|value| *value > 0.0)) /></label>
            </div>
            <div class="row">
                <label><input type="checkbox" prop:checked=move || session.annotation_world_radius.get()
                    on:change=move |ev| session.annotation_world_radius.set(event_target_checked(&ev)) />" World radius"</label>
                <input type="number" min="0.1" step="1" aria-label="Point radius in world pixels" disabled=move || !session.annotation_world_radius.get()
                    prop:value=move || {
                        let class = session.annotation_class.get();
                        session.annotation_class_radii.get().get(&class).copied().unwrap_or(session.annotation_radius.get()).to_string()
                    }
                    on:change=move |ev| { if let Ok(radius) = event_target_value(&ev).parse::<f64>() {
                        let radius = radius.clamp(0.1, 10000.0);
                        let class = session.annotation_class.get_untracked();
                        if class.is_empty() { session.annotation_radius.set(radius); }
                        else { session.annotation_class_radii.update(|radii| { radii.insert(class, radius); }); }
                    } } />
            </div>
            <div class="row">
                <label>"Show class" <select aria-label="Filter annotation class" on:change=move |ev| {
                    let value = event_target_value(&ev);
                    session.annotation_filter.set((value != "__all__").then_some(value));
                }>
                    <option value="__all__">"all"</option>
                    {move || {
                        let mut classes = active().map(|layer| layer.annotations.into_iter().map(|item| item.label).collect::<Vec<_>>()).unwrap_or_default();
                        classes.sort(); classes.dedup();
                        classes.into_iter().map(|class| {
                            let label = if class.is_empty() { "unclassified".to_string() } else { class.clone() };
                            view! { <option value=class>{label}</option> }
                        }).collect_view()
                    }}
                </select></label>
            </div>
            <div class="annotation-list">
                {move || active().map(|layer| qupath::in_tree_order(&layer.annotations).into_iter().take(200).map(|(item, depth)| {
                    let id = item.id;
                    let children = layer.annotations.iter().filter(|candidate| candidate.parent == Some(id)).count();
                    let label = format!("#{} {}{}{}{}", id, item.display_name(),
                        if children > 0 { format!(" ({children} inside)") } else { String::new() },
                        if item.dense_region { " ▨ dense" } else { "" }, if item.locked { " 🔒" } else { "" });
                    view! { <button class="annotation-row" class:active=move || session.selected_annotation.get() == Some(id)
                        style:padding-left=format!("{}px", 8 + depth * 12)
                        on:click=move |_| { session.selected_annotation.set(Some(id)); session.annotation_tool.set(AnnotationTool::Select); }>{label}</button> }
                }).collect_view()).unwrap_or_default()}
            </div>
            <Show when=move || selected().is_some()>
                <div class="annotation-edit">
                    <label>"Name" <input type="text" aria-label="Selected annotation name" prop:value=move || selected().and_then(|item| item.name).unwrap_or_default()
                        on:change=move |ev| { let name = event_target_value(&ev); session.edit_selected_annotation(|item| item.name = (!name.is_empty()).then_some(name)); } /></label>
                    <label>"Class" <input type="text" aria-label="Selected annotation class" prop:value=move || selected().map(|item| item.label).unwrap_or_default()
                        on:change=move |ev| { let class = event_target_value(&ev); session.edit_selected_annotation(|item| item.label = class); } /></label>
                    <label><input type="checkbox" prop:checked=move || selected().is_some_and(|item| item.locked)
                        on:change=move |ev| { let locked = event_target_checked(&ev); session.edit_selected_annotation(|item| item.locked = locked); } />" Locked"</label>
                    <label>"Object type" <select aria-label="Selected object type" prop:value=move || selected().map(|item| item.object_type.as_str()).unwrap_or("annotation")
                        on:change=move |ev| { let kind = ObjectType::parse(&event_target_value(&ev)); session.edit_selected_annotation(|item| item.object_type = kind); }>
                        <option value="annotation">"annotation"</option><option value="detection">"detection"</option>
                        <option value="cell">"cell"</option><option value="tile">"tile"</option><option value="tmaCore">"TMA core"</option>
                    </select></label>
                    <label>"Color" <input type="color" aria-label="Selected annotation color" prop:value=move || color_hex(selected().and_then(|item| item.color).unwrap_or([255, 216, 72]))
                        on:change=move |ev| { if let Some(color) = parse_color_hex(&event_target_value(&ev)) { session.edit_selected_annotation(|item| item.color = Some(color)); } } /></label>
                    <label>"Stroke width" <input type="number" min="0" step="0.5" aria-label="Selected stroke width" placeholder="geometric"
                        prop:value=move || selected().and_then(|item| item.stroke_width).map(|value| value.to_string()).unwrap_or_default()
                        on:change=move |ev| { let width = event_target_value(&ev).parse::<f64>().ok().filter(|value| *value > 0.0); session.edit_selected_annotation(|item| item.stroke_width = width); } /></label>
                    <label>"Z start" <input type="number" min="0" step="1" aria-label="Selected Z plane" prop:value=move || selected().map(|item| item.plane.z.to_string()).unwrap_or_default()
                        on:change=move |ev| { if let Ok(z) = event_target_value(&ev).parse::<i32>() { session.edit_selected_annotation(|item| item.plane.z = z.max(0)); } } /></label>
                    <label>"Z span" <input type="number" min="0" step="1" aria-label="Selected Z extent" prop:value=move || selected().map(|item| item.z_extent.to_string()).unwrap_or_default()
                        on:change=move |ev| { if let Ok(span) = event_target_value(&ev).parse::<u32>() { session.edit_selected_annotation(|item| item.z_extent = span); } } /></label>
                    <label>"T span" <input type="number" min="0" step="1" aria-label="Selected time extent" prop:value=move || selected().map(|item| item.t_extent.to_string()).unwrap_or_default()
                        on:change=move |ev| { if let Ok(span) = event_target_value(&ev).parse::<u32>() { session.edit_selected_annotation(|item| item.t_extent = span); } } /></label>
                    <label><input type="checkbox" prop:checked=move || selected().is_some_and(|item| item.dense_region)
                        on:change=move |ev| { let dense = event_target_checked(&ev); session.edit_selected_annotation(|item| item.dense_region = dense); } />" Dense region"</label>
                    <div class="row">
                        <button class="plain-button" on:click=move |_| if let Some(id) = session.selected_annotation.get_untracked() { session.annotation_action(format!("{id}/detach")); }>"Detach"</button>
                        <button class="plain-button" on:click=move |_| if let Some(id) = session.selected_annotation.get_untracked() { session.delete_annotation(id); }>"Delete"</button>
                    </div>
                </div>
            </Show>
            <div class="row">
                <button class="plain-button" on:click=move |_| session.annotation_action("renest".into())>"Renest"</button>
                <a class="plain-button" href=move || {
                    let (Some(dataset), Some(layer)) = (session.dataset.get(), session.annotation_layer.get()) else { return "#".into() };
                    format!("{}/geojson", annotation_layer_url(&session.origin.get(), &dataset, layer))
                } download="annotations.geojson">"Export GeoJSON"</a>
            </div>
            <button class="plain-button" title="Save physical bounding boxes in an ngio CSV ROI table. Non-box shapes are flattened and counted in the status line."
                on:click=move |_| session.save_annotation_roi()>"Export ROI table (boxes)"</button>
            <details>
                <summary>"Import GeoJSON"</summary>
                <input type="file" accept=".geojson,application/geo+json,application/json" aria-label="Choose GeoJSON file"
                    on:change=move |ev| {
                        let file = ev.target().and_then(|target| target.dyn_into::<web_sys::HtmlInputElement>().ok())
                            .and_then(|input| input.files()).and_then(|files| files.get(0));
                        if let Some(file) = file {
                            spawn_local(async move {
                                match wasm_bindgen_futures::JsFuture::from(file.text()).await {
                                    Ok(value) => if let Some(text) = value.as_string() { import_text.set(text); },
                                    Err(error) => session.fail(format!("read GeoJSON file: {}", js_error(error))),
                                }
                            });
                        }
                    } />
                <textarea aria-label="GeoJSON to import" prop:value=move || import_text.get() on:input=move |ev| import_text.set(event_target_value(&ev))></textarea>
                <button class="plain-button" disabled=move || import_text.get().is_empty()
                    on:click=move |_| session.import_annotations(import_text.get_untracked())>"Replace layer"</button>
            </details>
        </Show>
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
    // Use the conventional integer range enclosing the current window. Keeping 255 and 65535
    // exact matters: doubling a padded endpoint put a dtype-wide window halfway along its track.
    let bound = {
        let top = state.get_untracked().window_end.max(1.0);
        if top <= 255.0 { 255.0 }
        else if top <= 4_095.0 { 4_095.0 }
        else if top <= 65_535.0 { 65_535.0 }
        else { top }
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
                <div class="slider-row contrast-row">
                    <span>"Black"</span>
                    <input type="range" min="0" max=bound.to_string() step="1"
                        prop:value=move || state.get().window_start.to_string() on:input=on_min/>
                    <span class="slider-value">{move || format!("{:.0}", state.get().window_start)}</span>
                </div>
                <div class="slider-row contrast-row">
                    <span>"White"</span>
                    <input type="range" min="0" max=bound.to_string() step="1"
                        prop:value=move || state.get().window_end.to_string() on:input=on_max/>
                    <span class="slider-value">{move || format!("{:.0}", state.get().window_end)}</span>
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
