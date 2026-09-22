//! The page: session state, the server connection, and the components.
//!
//! Layout follows `omezarr_viewers-rs`: a tab strip, then the viewer shell (floating tool
//! strip, the 2×2 grid of XY / XZ / YZ slices and the orientation box — or one pane alone —
//! axis sliders, a status line) beside a sidebar of layer cards. All state is a set of signals
//! in [`Session`], and every server interaction is latest-only: while a frame is in flight a
//! newer camera or crosshair only marks the kind dirty, and the reply triggers the next
//! request, so the page never queues more work than the renderer can drain.

use std::rc::Rc;
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
};

use leptos::prelude::*;
use leptos::task::spawn_local;
use newvolim_scene::qupath::{self, Annotation, Geometry, ObjectType, Plane as AnnotationPlane};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

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
    Pan,
    Select,
    Point,
    Rectangle,
    Ellipse,
    Polygon,
    FreehandRegion,
    Polyline,
    FreehandLine,
    Profile,
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
        Self {
            class: String::new(),
            class_color: [51, 230, 255],
            object_type: ObjectType::Annotation,
            stroke_width: None,
            dense_region: false,
            filter: None,
            world_radius: false,
            radius: 20.0,
            class_radii: HashMap::new(),
            filled: false,
            color_by_class: false,
            opacity: 0.95,
            point_size: 11.0,
            slab: 8.0,
        }
    }
}

impl AnnotationTool {
    fn label(self) -> &'static str {
        match self {
            Self::Pan => "Pan",
            Self::Select => "Select",
            Self::Point => "Point",
            Self::Rectangle => "Rectangle",
            Self::Ellipse => "Ellipse",
            Self::Polygon => "Polygon",
            Self::FreehandRegion => "Freehand region",
            Self::Polyline => "Polyline",
            Self::FreehandLine => "Freehand line",
            Self::Profile => "Line profile",
        }
    }
}

fn annotation_tool_icon(tool: AnnotationTool) -> AnyView {
    match tool {
        AnnotationTool::Pan => view! { <span class="tool-glyph">"✋"</span> }.into_any(),
        AnnotationTool::Select => view! { <span class="tool-glyph">"⌖"</span> }.into_any(),
        AnnotationTool::Point => view! {
            <svg class="tool-icon" viewBox="0 0 20 20"><circle cx="10" cy="10" r="3"/></svg>
        }.into_any(),
        AnnotationTool::Rectangle => view! {
            <svg class="tool-icon" viewBox="0 0 20 20"><rect x="4" y="5" width="12" height="10" rx="1"/></svg>
        }.into_any(),
        AnnotationTool::Ellipse => view! {
            <svg class="tool-icon" viewBox="0 0 20 20"><ellipse cx="10" cy="10" rx="7" ry="5"/></svg>
        }.into_any(),
        AnnotationTool::Polygon => view! {
            <svg class="tool-icon" viewBox="0 0 20 20"><path d="M10 2.5 L17 7 L14.5 16 L5.5 16 L3 7 Z"/></svg>
        }.into_any(),
        AnnotationTool::FreehandRegion => view! {
            <svg class="tool-icon" viewBox="0 0 20 20"><path d="M3 13 C2 9 5 5 8 7 C11 9 12 3 16 5 C19 8 17 14 13 16 C9 18 4 16 3 13 Z"/></svg>
        }.into_any(),
        AnnotationTool::Polyline => view! {
            <svg class="tool-icon outline" viewBox="0 0 20 20"><path d="M2 16 L7 7 L11 12 L18 3"/></svg>
        }.into_any(),
        AnnotationTool::FreehandLine => view! {
            <svg class="tool-icon outline" viewBox="0 0 20 20"><path d="M3 13 C2 9 5 5 8 7 C11 9 12 3 16 5 C18 7 18 10 17 12"/></svg>
        }.into_any(),
        AnnotationTool::Profile => view! {
            <svg class="tool-icon outline" viewBox="0 0 20 20">
                <path d="M3 5 V15 M17 5 V15 M3 10 H17 M6 8 V12 M10 8 V12 M14 8 V12"/>
            </svg>
        }.into_any(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub orientation: [f32; 4],
    pub zoom: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            orientation: [0.0, 0.0, 0.0, 1.0],
            zoom: 1.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Slices {
    pub xy: String,
    pub xz: String,
    pub yz: String,
    pub viewport: bool,
    pub capture: OrthogonalCapture,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LineProfileView {
    pub line: [[f64; 2]; 2],
    pub response: Option<LineProfileResponse>,
    pub loading: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LabelUiState {
    pub summary: LabelSummary,
    pub visible: bool,
    pub opacity: f64,
    pub outline: bool,
    pub isolate: bool,
    pub selected: Option<LabelInspection>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ObjectUiState {
    pub summary: ObjectSummary,
    pub visible: bool,
    pub color: [u8; 3],
    pub opacity: f64,
    pub size: f64,
    pub hollow: bool,
    pub slab: f64,
    pub color_by: Option<usize>,
    pub filters: Vec<Option<[f64; 2]>>,
    pub points: Vec<ObjectPoint>,
    pub total_in_view: usize,
    pub selected: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MeasurementUiState {
    pub summary: MeasurementSummary,
    pub color_by: Option<usize>,
    pub filter: Option<[f64; 2]>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OrthogonalCapture {
    pub focus_xyz: [f64; 3],
    pub zooms: [f64; 3],
    pub pane_size: [u32; 2],
}

#[derive(Clone, Debug, PartialEq)]
struct SliceTilePlacement {
    src: String,
    source_x: u32,
    source_y: u32,
    source_width: u32,
    source_height: u32,
}

#[derive(Clone, Debug, PartialEq)]
struct SliceTileSet {
    level: usize,
    tiles: Vec<SliceTilePlacement>,
}

#[derive(Clone, Debug, PartialEq)]
struct LabelTileSet {
    key: String,
    level: usize,
    level_shape: [u32; 3],
    tiles: Vec<SliceTilePlacement>,
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
    pub label_layers: RwSignal<Vec<LabelUiState>>,
    pub object_layers: RwSignal<Vec<ObjectUiState>>,
    pub measurement_tables: RwSignal<Vec<MeasurementUiState>>,
    pub region_counts: RwSignal<Vec<RegionCount>>,
    pub feature_generation: RwSignal<u64>,
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
    pub line_profile: RwSignal<Option<LineProfileView>>,
    profile_generation: StoredValue<u64>,
    annotation_create_inflight: StoredValue<bool>,
    queued_annotations: StoredValue<Vec<Annotation>>,
    annotation_undo: StoredValue<Vec<(u64, Vec<Annotation>)>>,
    annotation_styles: StoredValue<HashMap<u64, AnnotationViewStyle>>,
    pub voxel_shape: RwSignal<Option<[u32; 3]>>,
    pub voxel_spacing: RwSignal<[f64; 3]>,
    pub spatial_units: RwSignal<[Option<String>; 3]>,
    pub timepoint: RwSignal<u32>,
    pub timepoint_count: RwSignal<u32>,
    /// The voxel at the centre of every slice pane: the integer crosshair the slices are cut
    /// at, derived from `focus`.
    pub crosshair: RwSignal<[u32; 3]>,
    /// The point at the centre of the slice panes, in voxels, continuous so a pan is smooth;
    /// `crosshair` is its floor.
    pub focus: RwSignal<[f64; 3]>,
    /// Zoom of the XY, XZ and YZ panes over their slice: 1 fits the whole slice.
    pub zoom_2d: RwSignal<[f64; 3]>,
    pub pyramid_shapes_xyz: RwSignal<Vec<[u32; 3]>>,
    /// 2D-only black/white windows, separate from the scene transfer state used by 3D.
    pub slice_windows: RwSignal<HashMap<(u64, usize), [f64; 2]>>,
    pub tile_generation: RwSignal<u64>,
    /// False until the first dataset response has selected its final 2D or 3D layout and the
    /// browser has had a frame to apply it.
    pub dataset_layout_ready: RwSignal<bool>,
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
    ortho_request_capture: StoredValue<Option<OrthogonalCapture>>,
    browser_busy: StoredValue<bool>,
    browser_dirty: StoredValue<bool>,
    channel_inflight: StoredValue<bool>,
    channel_dirty: StoredValue<Option<ChannelEdit>>,
    channel_tile_dirty: StoredValue<bool>,
    /// Scene-wide see-through depth (the server's `depthScale`), shown at once and sent
    /// latest-only.
    pub depth_scale: RwSignal<f32>,
    settings_inflight: StoredValue<bool>,
    settings_dirty: StoredValue<Option<SceneSettings>>,
    /// The browser renderer's chunk cache, kept across frames (taken during a frame).
    chunk_cache: StoredValue<Option<newvolim_residency::ChunkCache>>,
    volume_canvas: NodeRef<leptos::html::Canvas>,
    volume_pane: NodeRef<leptos::html::Div>,
    /// The XY, XZ and YZ panes; the first visible one sizes the orthogonal request.
    ortho_panes: [NodeRef<leptos::html::Div>; 3],
}

impl Session {
    fn uses_xy_tile_cache(self) -> bool {
        self.voxel_shape
            .get_untracked()
            .is_some_and(|shape| shape[2] == 1)
            && !self.pyramid_shapes_xyz.get_untracked().is_empty()
    }

    fn new() -> Self {
        Self {
            origin: RwSignal::new(String::new()),
            origin_entry: RwSignal::new(String::new()),
            datasets: RwSignal::new(Vec::new()),
            dataset: RwSignal::new(None),
            layers: RwSignal::new(Vec::new()),
            annotation_layers: RwSignal::new(Vec::new()),
            label_layers: RwSignal::new(Vec::new()),
            object_layers: RwSignal::new(Vec::new()),
            measurement_tables: RwSignal::new(Vec::new()),
            region_counts: RwSignal::new(Vec::new()),
            feature_generation: RwSignal::new(0),
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
            line_profile: RwSignal::new(None),
            profile_generation: StoredValue::new(0),
            annotation_create_inflight: StoredValue::new(false),
            queued_annotations: StoredValue::new(Vec::new()),
            annotation_undo: StoredValue::new(Vec::new()),
            annotation_styles: StoredValue::new(HashMap::new()),
            voxel_shape: RwSignal::new(None),
            voxel_spacing: RwSignal::new([1.0; 3]),
            spatial_units: RwSignal::new([None, None, None]),
            timepoint: RwSignal::new(0),
            timepoint_count: RwSignal::new(1),
            crosshair: RwSignal::new([0; 3]),
            focus: RwSignal::new([0.0; 3]),
            zoom_2d: RwSignal::new([1.0; 3]),
            pyramid_shapes_xyz: RwSignal::new(Vec::new()),
            slice_windows: RwSignal::new(HashMap::new()),
            tile_generation: RwSignal::new(js_sys::Date::now() as u64),
            dataset_layout_ready: RwSignal::new(false),
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
            ortho_request_capture: StoredValue::new(None),
            browser_busy: StoredValue::new(false),
            browser_dirty: StoredValue::new(false),
            channel_inflight: StoredValue::new(false),
            channel_dirty: StoredValue::new(None),
            channel_tile_dirty: StoredValue::new(false),
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
                    self.status
                        .set(format!("{} datasets at {origin}", list.datasets.len()));
                    // A deep link: `?dataset=name` opens that dataset straight away.
                    let wanted =
                        query_parameter("dataset").filter(|name| list.datasets.contains(name));
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
        if !self.close() {
            return;
        }
        self.dataset.set(Some(name.clone()));
        // The first orthogonal response decides whether this dataset has a volume. Starting from
        // Grid makes a later 3D dataset predictable without speculatively rendering one now.
        self.view_mode.set(ViewMode::Grid);
        self.error.set(None);
        self.status.set(format!("Opening {name}…"));
        self.refresh_layers();
        self.refresh_features();
        self.refresh_annotations();
        self.refresh_settings();
        self.open_socket();
        // The panes mount on the next frame; ask for the first frames at their real sizes. The
        // first orthogonal reply brings the voxel shape and a centred crosshair.
        request_animation_frame(move || {
            self.request_orthogonal();
        });
    }

    pub fn close(self) -> bool {
        if self
            .annotation_layers
            .get_untracked()
            .iter()
            .any(|layer| layer.dirty)
            && !window()
                .confirm_with_message("Unsaved annotations will be lost. Close this dataset?")
                .unwrap_or(false)
        {
            return false;
        }
        self.volume_preview_generation
            .update_value(|generation| *generation = generation.wrapping_add(1));
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
        self.label_layers.set(Vec::new());
        self.object_layers.set(Vec::new());
        self.measurement_tables.set(Vec::new());
        self.region_counts.set(Vec::new());
        self.annotation_roi_tables.set(Vec::new());
        self.selected_annotation.set(None);
        self.annotation_tool.set(AnnotationTool::Pan);
        self.annotation_draft.set(Vec::new());
        self.line_profile.set(None);
        self.profile_generation
            .update_value(|generation| *generation = generation.wrapping_add(1));
        self.annotation_create_inflight.set_value(false);
        self.queued_annotations.set_value(Vec::new());
        self.annotation_undo.set_value(Vec::new());
        self.voxel_shape.set(None);
        self.voxel_spacing.set([1.0; 3]);
        self.spatial_units.set([None, None, None]);
        self.timepoint.set(0);
        self.timepoint_count.set(1);
        self.pyramid_shapes_xyz.set(Vec::new());
        self.slice_windows.set(HashMap::new());
        self.dataset_layout_ready.set(false);
        self.slices.set(None);
        self.volume_png.set(None);
        self.camera.set(Camera::default());
        self.volume_inflight.set_value(None);
        self.ortho_inflight.set_value(None);
        self.volume_dirty.set_value(false);
        self.ortho_dirty.set_value(false);
        self.channel_tile_dirty.set_value(false);
        self.update_busy();
        true
    }

    fn refresh_layers(self) {
        let Some(dataset) = self.dataset.get() else {
            return;
        };
        let url = channels_url(&self.origin.get(), &dataset);
        spawn_local(async move {
            match get_json::<Vec<LayerChannelSummary>>(&url).await {
                Ok(layers) => {
                    self.slice_windows.set(
                        layers
                            .iter()
                            .flat_map(|layer| {
                                layer.channels.iter().map(move |channel| {
                                    (
                                        (layer.layer_id, channel.source_index),
                                        [channel.window_start, channel.window_end],
                                    )
                                })
                            })
                            .collect(),
                    );
                    self.layers.set(layers);
                }
                Err(message) => self.fail(message),
            }
        });
    }

    fn refresh_features(self) {
        let Some(dataset) = self.dataset.get_untracked() else {
            return;
        };
        let url = features_url(&self.origin.get_untracked(), &dataset);
        spawn_local(async move {
            match get_json::<FeatureManifest>(&url).await {
                Ok(manifest) => {
                    self.label_layers.set(
                        manifest
                            .labels
                            .into_iter()
                            .map(|summary| LabelUiState {
                                summary,
                                visible: true,
                                opacity: 0.65,
                                outline: false,
                                isolate: false,
                                selected: None,
                            })
                            .collect(),
                    );
                    self.object_layers.set(
                        manifest
                            .objects
                            .into_iter()
                            .map(|summary| {
                                let filters = vec![None; summary.columns.len()];
                                ObjectUiState {
                                    summary,
                                    visible: true,
                                    color: [255, 190, 45],
                                    opacity: 0.9,
                                    size: 7.0,
                                    hollow: false,
                                    slab: 8.0,
                                    color_by: None,
                                    filters,
                                    points: Vec::new(),
                                    total_in_view: 0,
                                    selected: None,
                                }
                            })
                            .collect(),
                    );
                    self.measurement_tables.set(
                        manifest
                            .tables
                            .into_iter()
                            .map(|summary| MeasurementUiState {
                                summary,
                                color_by: None,
                                filter: None,
                            })
                            .collect(),
                    );
                    self.feature_generation
                        .update(|value| *value = value.wrapping_add(1));
                }
                Err(message) => self.fail(format!("feature layers: {message}")),
            }
        });
    }

    fn load_objects(self, bounds: [f64; 6]) {
        let (Some(dataset), origin) = (self.dataset.get_untracked(), self.origin.get_untracked())
        else {
            return;
        };
        let layers = self.object_layers.get_untracked();
        for layer in layers.into_iter().filter(|v| v.visible) {
            let name = layer.summary.name.clone();
            let url = objects_url(&origin, &dataset, &name, bounds);
            spawn_local(async move {
                match get_json::<ObjectQueryResult>(&url).await {
                    Ok(result) => self.object_layers.update(|layers| {
                        if let Some(layer) = layers.iter_mut().find(|v| v.summary.name == name) {
                            layer.points = result.points;
                            layer.total_in_view = result.total
                        }
                    }),
                    Err(message) => self.fail(format!("objects {name}: {message}")),
                }
            });
        }
    }

    fn inspect_features(self, x: u32, y: u32, z: u32, world_per_screen: f64) {
        let (Some(dataset), origin) = (self.dataset.get_untracked(), self.origin.get_untracked())
        else {
            return;
        };
        for layer in self
            .label_layers
            .get_untracked()
            .into_iter()
            .filter(|v| v.visible)
        {
            let name = layer.summary.name.clone();
            let url = label_value_url(&origin, &dataset, &name, x, y, z);
            spawn_local(async move {
                match get_json::<LabelInspection>(&url).await {
                    Ok(value) => {
                        self.label_layers.update(|layers| {
                            if let Some(layer) = layers.iter_mut().find(|v| v.summary.name == name)
                            {
                                layer.selected = Some(value)
                            }
                        });
                        self.feature_generation.update(|v| *v = v.wrapping_add(1));
                    }
                    Err(message) => self.fail(format!("label inspection: {message}")),
                }
            });
        }
        let mut nearest = None::<(f64, String, usize)>;
        for layer in self
            .object_layers
            .get_untracked()
            .into_iter()
            .filter(|v| v.visible)
        {
            for point in &layer.points {
                let distance = (point.x - x as f64).hypot(point.y - y as f64);
                if distance <= layer.size * world_per_screen
                    && nearest.as_ref().is_none_or(|v| distance < v.0)
                {
                    nearest = Some((distance, layer.summary.name.clone(), point.row));
                }
            }
        }
        if let Some((_, name, row)) = nearest {
            let url = object_row_url(&origin, &dataset, &name, row);
            spawn_local(async move {
                match get_json::<serde_json::Value>(&url).await {
                    Ok(value) => self.object_layers.update(|layers| {
                        if let Some(layer) = layers.iter_mut().find(|v| v.summary.name == name) {
                            layer.selected = Some(value)
                        }
                    }),
                    Err(message) => self.fail(format!("object inspection: {message}")),
                }
            })
        }
    }

    fn count_regions(self, label: String, objects: String) {
        let (Some(dataset), origin) = (self.dataset.get_untracked(), self.origin.get_untracked())
        else {
            return;
        };
        let url = regions_url(&origin, &dataset, &label, &objects);
        spawn_local(async move {
            match get_json::<Vec<RegionCount>>(&url).await {
                Ok(rows) => self.region_counts.set(rows),
                Err(message) => self.fail(format!("region counts: {message}")),
            }
        })
    }

    fn refresh_annotations(self) {
        let Some(dataset) = self.dataset.get_untracked() else {
            return;
        };
        let url = annotation_layers_url(&self.origin.get_untracked(), &dataset);
        spawn_local(async move {
            match get_json::<Vec<AnnotationLayer>>(&url).await {
                Ok(layers) => {
                    let has_annotations = layers
                        .iter()
                        .any(|layer| layer.visible && !layer.annotations.is_empty());
                    let active = self.annotation_layer.get_untracked();
                    self.select_annotation_layer(
                        active
                            .filter(|id| layers.iter().any(|layer| layer.id == *id))
                            .or_else(|| layers.first().map(|layer| layer.id)),
                    );
                    self.annotation_layers.set(layers);
                    if has_annotations
                        && self
                            .voxel_shape
                            .get_untracked()
                            .is_some_and(|shape| shape[2] > 1)
                    {
                        self.request_volume();
                    }
                }
                Err(message) => self.fail(message),
            }
        });
        let tables_url = annotation_roi_tables_url(&self.origin.get_untracked(), &dataset);
        spawn_local(async move {
            if let Ok(tables) = get_json::<Vec<RoiTableSummary>>(&tables_url).await {
                self.annotation_roi_tables.set(tables);
            }
        });
    }

    fn set_annotation_layer(self, updated: AnnotationLayer) {
        self.annotation_layers.update(|layers| {
            if let Some(existing) = layers.iter_mut().find(|layer| layer.id == updated.id) {
                *existing = updated;
            } else {
                layers.push(updated);
            }
        });
    }

    fn active_annotations(self) -> Vec<Annotation> {
        let id = self.annotation_layer.get_untracked();
        self.annotation_layers
            .get_untracked()
            .into_iter()
            .find(|layer| Some(layer.id) == id)
            .map(|layer| layer.annotations)
            .unwrap_or_default()
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
        if previous == next {
            return;
        }
        if let Some(id) = previous {
            let style = self.current_annotation_style();
            self.annotation_styles.update_value(|styles| {
                styles.insert(id, style);
            });
        }
        let style = next
            .and_then(|id| self.annotation_styles.get_value().get(&id).cloned())
            .unwrap_or_default();
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
        let Some(id) = self.selected_annotation.get_untracked() else {
            return;
        };
        let Some(mut annotation) = self
            .active_annotations()
            .into_iter()
            .find(|item| item.id == id)
        else {
            return;
        };
        edit(&mut annotation);
        self.remember_annotations();
        self.update_annotation(annotation);
    }

    fn remember_annotations(self) {
        let Some(layer) = self.annotation_layer.get_untracked() else {
            return;
        };
        let rows = self.active_annotations();
        self.annotation_undo.update_value(|history| {
            history.push((layer, rows));
            if history.len() > 50 {
                history.remove(0);
            }
        });
    }

    fn undo_annotations(self) {
        let Some((layer, rows)) = self.annotation_undo.get_value().last().cloned() else {
            return;
        };
        let Some(dataset) = self.dataset.get_untracked() else {
            return;
        };
        let url = format!(
            "{}/state",
            annotation_layer_url(&self.origin.get_untracked(), &dataset, layer)
        );
        spawn_local(async move {
            match put_json::<AnnotationLayer, _>(&url, &rows).await {
                Ok(updated) => {
                    self.annotation_undo.update_value(|history| {
                        history.pop();
                    });
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
        let Some((geometry, is_ellipse)) = annotation_geometry(tool, &points) else {
            return;
        };
        let z = self.crosshair.get_untracked()[2] as i32;
        self.add_annotation(Annotation {
            geometry,
            is_ellipse,
            plane: AnnotationPlane::at(z, 0),
            ..Annotation::default()
        });
    }

    fn request_line_profile(self, line: [[f64; 2]; 2], level: u32) {
        let (Some(dataset), Some(layer)) = (
            self.dataset.get_untracked(),
            self.layers.get_untracked().into_iter().next(),
        ) else {
            return;
        };
        let channels = layer
            .channels
            .iter()
            .filter(|channel| channel.enabled)
            .map(|channel| channel.source_index)
            .collect::<Vec<_>>();
        if channels.is_empty() {
            self.line_profile.set(Some(LineProfileView {
                line,
                response: None,
                loading: false,
                error: Some("No enabled image channels to sample".into()),
            }));
            return;
        }
        let generation = self.profile_generation.get_value().wrapping_add(1);
        self.profile_generation.set_value(generation);
        self.line_profile.set(Some(LineProfileView {
            line,
            response: None,
            loading: true,
            error: None,
        }));
        let url = xy_profile_url(
            &self.origin.get_untracked(),
            &dataset,
            level,
            line[0],
            line[1],
            self.focus.get_untracked()[2],
            &channels,
        );
        spawn_local(async move {
            let result = get_json::<LineProfileResponse>(&url).await;
            if self.profile_generation.get_value() != generation {
                return;
            }
            self.line_profile.update(|profile| {
                let Some(profile) = profile else { return };
                profile.loading = false;
                match result {
                    Ok(response) => profile.response = Some(response),
                    Err(message) => profile.error = Some(message),
                }
            });
        });
    }

    fn create_annotation_layer(self, name: String) {
        let Some(dataset) = self.dataset.get_untracked() else {
            return;
        };
        let url = annotation_layers_url(&self.origin.get_untracked(), &dataset);
        spawn_local(async move {
            match post_json::<AnnotationLayer, _>(&url, &serde_json::json!({"name": name})).await {
                Ok(layer) => {
                    self.select_annotation_layer(Some(layer.id));
                    self.set_annotation_layer(layer);
                    self.error.set(None);
                }
                Err(message) => self.fail(message),
            }
        });
    }

    fn remove_annotation_layer(self) {
        let (Some(dataset), Some(id)) = (
            self.dataset.get_untracked(),
            self.annotation_layer.get_untracked(),
        ) else {
            return;
        };
        let layer = self
            .annotation_layers
            .get_untracked()
            .into_iter()
            .find(|layer| layer.id == id);
        if layer.as_ref().is_some_and(|layer| layer.dirty)
            && !window()
                .confirm_with_message(
                    "Unsaved annotations in this layer will be lost. Remove it from the session?",
                )
                .unwrap_or(false)
        {
            return;
        }
        let url = annotation_layer_url(&self.origin.get_untracked(), &dataset, id);
        spawn_local(async move {
            match delete_request(&url).await {
                Ok(()) => {
                    self.annotation_layers
                        .update(|layers| layers.retain(|layer| layer.id != id));
                    self.select_annotation_layer(
                        self.annotation_layers
                            .get_untracked()
                            .first()
                            .map(|layer| layer.id),
                    );
                    self.annotation_styles.update_value(|styles| {
                        styles.remove(&id);
                    });
                    self.selected_annotation.set(None);
                    self.annotation_undo
                        .update_value(|history| history.retain(|(layer, _)| *layer != id));
                    self.error.set(None);
                    self.request_volume();
                }
                Err(message) => self.fail(message),
            }
        });
    }

    fn add_annotation(self, mut annotation: Annotation) {
        let Some(dataset) = self.dataset.get_untracked() else {
            return;
        };
        let origin = self.origin.get_untracked();
        let Some(layer_id) = self.annotation_layer.get_untracked() else {
            self.queued_annotations
                .update_value(|queue| queue.push(annotation));
            if self.annotation_create_inflight.get_value() {
                return;
            }
            self.annotation_create_inflight.set_value(true);
            spawn_local(async move {
                let url = annotation_layers_url(&origin, &dataset);
                let name = format!("manual_{}", js_sys::Date::now() as u64);
                match post_json::<AnnotationLayer, _>(&url, &serde_json::json!({"name": name}))
                    .await
                {
                    Ok(layer) => {
                        self.select_annotation_layer(Some(layer.id));
                        self.set_annotation_layer(layer);
                        let queued = self.queued_annotations.get_value();
                        self.queued_annotations.set_value(Vec::new());
                        for item in queued {
                            self.add_annotation(item);
                        }
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
            annotation.dense_region = self.annotation_dense_region.get_untracked()
                && matches!(
                    &annotation.geometry,
                    Geometry::Polygon(_) | Geometry::MultiPolygon(_)
                );
            if matches!(
                &annotation.geometry,
                Geometry::LineString(_) | Geometry::MultiLineString(_)
            ) {
                annotation.stroke_width = self.annotation_stroke_width.get_untracked();
            }
            let url = annotation_layer_url(&origin, &dataset, id);
            match post_json::<Annotation, _>(&url, &annotation).await {
                Ok(stored) => {
                    self.annotation_layers.update(|layers| {
                        if let Some(layer) = layers.iter_mut().find(|layer| layer.id == id) {
                            layer.annotations.push(stored.clone());
                            layer.dirty = true;
                        }
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
        let (Some(dataset), Some(layer)) = (
            self.dataset.get_untracked(),
            self.annotation_layer.get_untracked(),
        ) else {
            return;
        };
        let url = format!(
            "{}/{}",
            annotation_layer_url(&self.origin.get_untracked(), &dataset, layer),
            annotation.id
        );
        spawn_local(async move {
            match put_json::<Annotation, _>(&url, &annotation).await {
                Ok(stored) => {
                    self.annotation_layers.update(|layers| {
                        if let Some(layer) = layers.iter_mut().find(|item| item.id == layer) {
                            if let Some(item) = layer
                                .annotations
                                .iter_mut()
                                .find(|item| item.id == stored.id)
                            {
                                *item = stored;
                            }
                            layer.dirty = true;
                        }
                    });
                    self.error.set(None);
                    self.request_volume();
                }
                Err(message) => self.fail(message),
            }
        });
    }

    fn delete_annotation(self, id: u64) {
        let (Some(dataset), Some(layer)) = (
            self.dataset.get_untracked(),
            self.annotation_layer.get_untracked(),
        ) else {
            return;
        };
        let url = format!(
            "{}/{id}",
            annotation_layer_url(&self.origin.get_untracked(), &dataset, layer)
        );
        self.remember_annotations();
        spawn_local(async move {
            match delete_request(&url).await {
                Ok(()) => {
                    self.annotation_layers.update(|layers| {
                        if let Some(layer) = layers.iter_mut().find(|item| item.id == layer) {
                            layer.annotations.retain(|item| item.id != id);
                            layer.dirty = true;
                        }
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
        let (Some(dataset), Some(layer)) = (
            self.dataset.get_untracked(),
            self.annotation_layer.get_untracked(),
        ) else {
            return;
        };
        let Some(target) = self
            .annotation_layers
            .get_untracked()
            .into_iter()
            .find(|item| item.id == layer)
            .map(|item| item.save_target)
        else {
            return;
        };
        let url = format!(
            "{}/save-to",
            annotation_layer_url(&self.origin.get_untracked(), &dataset, layer)
        );
        spawn_local(async move {
            match post_json::<AnnotationSaveReport, _>(&url, &serde_json::json!({"target": target}))
                .await
            {
                Ok(report) => {
                    self.annotation_layers.update(|layers| {
                        if let Some(item) = layers.iter_mut().find(|item| item.id == layer) {
                            item.dirty = false;
                        }
                    });
                    self.status.set(format!(
                        "Saved {} annotations as {} to {}{}",
                        report.rows,
                        report.format,
                        report.target,
                        if report.flattened > 0 {
                            format!("; {} shapes reduced to boxes", report.flattened)
                        } else {
                            String::new()
                        }
                    ));
                    self.error.set(None);
                }
                Err(message) => self.fail(message),
            }
        });
    }

    fn set_annotation_visibility(self, visible: bool) {
        let (Some(dataset), Some(layer)) = (
            self.dataset.get_untracked(),
            self.annotation_layer.get_untracked(),
        ) else {
            return;
        };
        let url = format!(
            "{}/visibility",
            annotation_layer_url(&self.origin.get_untracked(), &dataset, layer)
        );
        spawn_local(async move {
            match put_json::<AnnotationLayer, _>(&url, &serde_json::json!({"visible": visible}))
                .await
            {
                Ok(updated) => {
                    self.set_annotation_layer(updated);
                    self.error.set(None);
                    self.request_volume();
                }
                Err(message) => self.fail(message),
            }
        });
    }

    fn save_annotation_roi(self) {
        let (Some(dataset), Some(layer)) = (
            self.dataset.get_untracked(),
            self.annotation_layer.get_untracked(),
        ) else {
            return;
        };
        let url = format!(
            "{}/save-roi",
            annotation_layer_url(&self.origin.get_untracked(), &dataset, layer)
        );
        spawn_local(async move {
            match post_json::<RoiSaveReport, _>(&url, &serde_json::json!({})).await {
                Ok(report) => {
                    self.status.set(format!(
                        "ROI table saved to {}; {} shapes reduced to boxes",
                        report.target, report.flattened
                    ));
                    self.refresh_annotations();
                }
                Err(message) => self.fail(message),
            }
        });
    }

    fn import_annotation_roi(self, name: String) {
        let Some(dataset) = self.dataset.get_untracked() else {
            return;
        };
        let url = annotation_roi_import_url(&self.origin.get_untracked(), &dataset, &name);
        spawn_local(async move {
            match post_json::<AnnotationLayer, _>(&url, &serde_json::json!({})).await {
                Ok(layer) => {
                    self.select_annotation_layer(Some(layer.id));
                    self.set_annotation_layer(layer);
                    self.error.set(None);
                    self.request_volume();
                }
                Err(message) => self.fail(message),
            }
        });
    }

    fn import_annotations(self, text: String) {
        let (Some(dataset), Some(layer)) = (
            self.dataset.get_untracked(),
            self.annotation_layer.get_untracked(),
        ) else {
            return;
        };
        let url = format!(
            "{}/geojson",
            annotation_layer_url(&self.origin.get_untracked(), &dataset, layer)
        );
        self.remember_annotations();
        spawn_local(async move {
            match put_geojson(&url, text).await {
                Ok(updated) => {
                    self.set_annotation_layer(updated);
                    self.selected_annotation.set(None);
                    self.error.set(None);
                    self.request_volume();
                }
                Err(message) => self.fail(message),
            }
        });
    }

    fn annotation_action(self, suffix: String) {
        let (Some(dataset), Some(layer)) = (
            self.dataset.get_untracked(),
            self.annotation_layer.get_untracked(),
        ) else {
            return;
        };
        let url = format!(
            "{}/{}",
            annotation_layer_url(&self.origin.get_untracked(), &dataset, layer),
            suffix
        );
        self.remember_annotations();
        spawn_local(async move {
            match post_empty(&url).await {
                Ok(()) => {
                    self.refresh_annotations();
                    self.error.set(None);
                }
                Err(message) => self.fail(message),
            }
        });
    }

    fn refresh_settings(self) {
        let Some(dataset) = self.dataset.get() else {
            return;
        };
        let url = settings_url(&self.origin.get(), &dataset);
        spawn_local(async move {
            match get_json::<SceneSettings>(&url).await {
                Ok(settings) => {
                    self.depth_scale.set(settings.depth_scale);
                    self.timepoint.set(settings.timepoint);
                    self.timepoint_count.set(settings.timepoint_count.max(1));
                }
                Err(message) => self.fail(message),
            }
        });
    }

    /// The see-through depth: shown at once, sent latest-only, then the volume follows.
    pub fn set_depth_scale(self, scale: f32) {
        self.depth_scale.set(scale);
        self.queue_settings();
    }

    pub fn set_timepoint(self, timepoint: u32) {
        let timepoint = timepoint.min(self.timepoint_count.get_untracked().saturating_sub(1));
        if timepoint == self.timepoint.get_untracked() {
            return;
        }
        self.timepoint.set(timepoint);
        self.tile_generation
            .update(|generation| *generation = generation.wrapping_add(1));
        self.chunk_cache.set_value(None);
        self.line_profile.set(None);
        self.profile_generation
            .update_value(|generation| *generation = generation.wrapping_add(1));
        self.queue_settings();
    }

    fn current_settings(self) -> SceneSettings {
        SceneSettings {
            depth_scale: self.depth_scale.get_untracked(),
            timepoint: self.timepoint.get_untracked(),
            timepoint_count: self.timepoint_count.get_untracked(),
        }
    }

    fn queue_settings(self) {
        let settings = self.current_settings();
        if self.settings_inflight.get_value() {
            self.settings_dirty.set_value(Some(settings));
            return;
        }
        self.post_settings(settings);
    }

    fn post_settings(self, wanted: SceneSettings) {
        let Some(dataset) = self.dataset.get_untracked() else {
            return;
        };
        let url = settings_url(&self.origin.get_untracked(), &dataset);
        self.settings_inflight.set_value(true);
        self.update_busy();
        spawn_local(async move {
            match post_json::<SceneSettings, _>(&url, &wanted).await {
                Ok(settings) => {
                    if self.settings_dirty.get_value().is_none() {
                        self.depth_scale.set(settings.depth_scale);
                        self.timepoint.set(settings.timepoint);
                        self.timepoint_count.set(settings.timepoint_count.max(1));
                    }
                    self.error.set(None);
                }
                Err(message) => self.fail(message),
            }
            self.settings_inflight.set_value(false);
            self.update_busy();
            if let Some(next) = self.settings_dirty.get_value() {
                self.settings_dirty.set_value(None);
                self.post_settings(next);
            } else {
                self.ortho_dirty.set_value(false);
                self.request_orthogonal();
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
        let socket = Rc::new(RefCell::new(Socket {
            ws: ws.clone(),
            open: false,
            queued: Vec::new(),
            _closures: Vec::new(),
        }));
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
            let Ok(event) = event.dyn_into::<web_sys::MessageEvent>() else {
                return;
            };
            let Some(text) = event.data().as_string() else {
                return;
            };
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
        let Some(socket) = self.socket.get_value() else {
            return;
        };
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
        let Some(dataset) = self.dataset.get_untracked() else {
            return;
        };
        let mode = self.view_mode.get_untracked();
        if self.uses_xy_tile_cache()
            && mode == ViewMode::Xy
            && self.slices.get_untracked().is_some()
        {
            self.ortho_dirty.set_value(false);
            return;
        }
        let zooms = self.zoom_2d.get_untracked();
        let Some(pane_index) = [Plane::Xy, Plane::Xz, Plane::Yz]
            .iter()
            .enumerate()
            .filter(|(_, &plane)| mode.shows_plane(plane))
            .max_by(|(left, _), (right, _)| zooms[*left].total_cmp(&zooms[*right]))
            .map(|(index, _)| index)
        else {
            return;
        };
        if self.ortho_inflight.get_value().is_some() {
            self.ortho_dirty.set_value(true);
            return;
        }
        let (width, height) = self.ortho_panes[pane_index]
            .get_untracked()
            .map(|pane| physical_size(&pane))
            .unwrap_or((256, 256));
        let crosshair = self
            .voxel_shape
            .get_untracked()
            .map(|_| self.crosshair.get_untracked());
        let focus_xyz = self
            .voxel_shape
            .get_untracked()
            .map(|shape| normalized_focus_xyz(self.focus.get_untracked(), shape));
        let request_id = self.next_request_id();
        self.ortho_inflight.set_value(Some(request_id));
        self.ortho_request_capture
            .set_value(Some(OrthogonalCapture {
                focus_xyz: self.focus.get_untracked(),
                zooms,
                pane_size: [width, height],
            }));
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
            slice_zooms: Some(zooms),
        };
        self.socket_send(serde_json::to_string(&request).expect("FrameRequest serializes"));
    }

    /// The volume frame for the current camera: from the server over the socket, or rendered
    /// here from the server's scene packet.
    pub fn request_volume(self) {
        let Some(dataset) = self.dataset.get_untracked() else {
            return;
        };
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
        let (width, height) = self
            .volume_pane
            .get_untracked()
            .map(|pane| preview_size(physical_size(&pane), self.volume_preview.get_value()))
            .unwrap_or((256, 256));
        let camera = self.camera.get_untracked();
        let focus_xyz = self
            .voxel_shape
            .get_untracked()
            .map(|shape| normalized_focus_xyz(self.focus.get_untracked(), shape));
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
        let Some(canvas) = self.volume_canvas.get_untracked() else {
            return;
        };
        let (width, height) = self
            .volume_pane
            .get_untracked()
            .map(|pane| preview_size(physical_size(&pane), self.volume_preview.get_value()))
            .unwrap_or((256, 256));
        let camera = self.camera.get_untracked();
        let focus_xyz = self
            .voxel_shape
            .get_untracked()
            .map(|shape| normalized_focus_xyz(self.focus.get_untracked(), shape));
        let origin = self.origin.get_untracked();
        self.browser_busy.set_value(true);
        self.browser_dirty.set_value(false);
        self.update_busy();
        spawn_local(async move {
            let canvas: web_sys::HtmlCanvasElement = canvas.clone();
            let outcome = self
                .browser_frame(&origin, &dataset, &canvas, width, height, camera, focus_xyz)
                .await;
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
                    self.notice.set(Some(format!(
                        "WebGPU render failed: {message}. Showing server frames."
                    )));
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
        let plan: ScenePlan = get_json(&scene_plan_url(
            origin,
            dataset,
            width,
            height,
            0,
            0,
            camera.zoom,
            Some(camera.orientation),
            focus_xyz,
            None,
        ))
        .await?;
        let mut ray_words = newvolim_residency::ray_words_for_plan(&plan)?;
        let cache = self
            .chunk_cache
            .get_value()
            .unwrap_or_else(|| ChunkCache::new(CHUNK_CACHE_WORDS));
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
                    let mut rgba: Vec<u8> = output
                        .iter()
                        .take(pixels)
                        .flat_map(|word| word.to_le_bytes())
                        .collect();
                    if self
                        .annotation_layers
                        .get_untracked()
                        .iter()
                        .any(|layer| layer.visible && !layer.annotations.is_empty())
                    {
                        let url = annotation_projection_url(
                            origin,
                            dataset,
                            width,
                            height,
                            camera.zoom,
                            camera.orientation,
                            focus_xyz,
                        );
                        let words: Vec<u32> = get_json(&url).await?;
                        let primitives = projected_annotations_from_words(&words)?;
                        if !primitives.is_empty() {
                            let depths = output
                                .get(pixels..pixels * 2)
                                .ok_or("scene output has no paired depth")?
                                .iter()
                                .map(|bits| f32::from_bits(*bits))
                                .collect();
                            let frame = palace_core::gpu::PortableFrameAttachments::new(
                                width, height, rgba, depths,
                            )
                            .ok_or("scene colour and depth cannot form annotation attachments")?;
                            rgba = palace_core::gpu::PortableAnnotationCompositeInput::new(
                                frame, primitives,
                            )
                            .ok_or("annotation projection exceeds compositor capacity")?
                            .composite_cpu()
                            .ok_or("annotation compositor rejected the frame")?
                            .rgba;
                        }
                    }
                    scene_webgpu_present(
                        canvas,
                        js_sys::Uint8ClampedArray::from(&rgba[..]),
                        width,
                        height,
                    )
                    .await
                    .map_err(js_error)?;
                    let hits = output[pixels..(2 * pixels).min(output.len())]
                        .iter()
                        .filter(|bits| f32::from_bits(**bits).is_finite())
                        .count();
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
                    self.status.set(format!(
                        "Browser residency: pass {passes}, fetching {} chunks…",
                        missing.len()
                    ));
                    fetch_chunks(origin, dataset, &mut client, &missing).await?;
                }
                StepOutcome::ExceedsPortableBound { required_pages } => {
                    let plan = client.plan();
                    let coarser =
                        newvolim_residency::coarser_levels(&plan.levels, &plan.level_counts)
                            .ok_or_else(|| {
                                format!(
                                    "the coarsest levels {:?} still need {required_pages} pages",
                                    plan.levels
                                )
                            })?;
                    let plan: ScenePlan = get_json(&scene_plan_url(
                        origin,
                        dataset,
                        width,
                        height,
                        0,
                        0,
                        camera.zoom,
                        Some(camera.orientation),
                        focus_xyz,
                        Some(&coarser),
                    ))
                    .await?;
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
            SocketReply::Frame {
                request_id,
                render_ms,
                data_base64,
                ..
            } => {
                if self.volume_inflight.get_value() == Some(request_id) {
                    self.volume_inflight.set_value(None);
                    self.volume_png
                        .set(Some(format!("data:image/png;base64,{data_base64}")));
                    self.error.set(None);
                    self.status
                        .set(format!("Server volume frame in {render_ms:.0} ms"));
                    self.update_busy();
                    if self.volume_dirty.get_value() {
                        self.request_volume();
                    }
                }
            }
            SocketReply::Orthogonal {
                request_id,
                render_ms,
                xy_base64,
                xz_base64,
                yz_base64,
                voxel_shape_xyz,
                crosshair_xyz,
                pyramid_levels,
                viewport,
                pyramid_shapes_xyz,
                voxel_spacing_xyz,
                spatial_units_xyz,
                timepoint,
                timepoint_count,
                ..
            } => {
                if self.ortho_inflight.get_value() == Some(request_id) {
                    self.ortho_inflight.set_value(None);
                    let first = self.voxel_shape.get_untracked().is_none();
                    self.voxel_shape.set(Some(voxel_shape_xyz));
                    self.pyramid_shapes_xyz.set(pyramid_shapes_xyz);
                    self.voxel_spacing.set(voxel_spacing_xyz);
                    self.spatial_units.set(spatial_units_xyz);
                    self.timepoint.set(timepoint);
                    self.timepoint_count.set(timepoint_count.max(1));
                    let mut capture =
                        self.ortho_request_capture
                            .get_value()
                            .unwrap_or(OrthogonalCapture {
                                focus_xyz: crosshair_xyz.map(|value| value as f64 + 0.5),
                                zooms: [1.0; 3],
                                pane_size: [256, 256],
                            });
                    if first {
                        self.crosshair.set(crosshair_xyz);
                        self.focus.set(crosshair_xyz.map(|v| v as f64 + 0.5));
                        self.zoom_2d.set([1.0; 3]);
                        capture.focus_xyz = crosshair_xyz.map(|value| value as f64 + 0.5);
                        capture.zooms = [1.0; 3];
                        if voxel_shape_xyz[2] == 1 {
                            self.set_view_mode(ViewMode::Xy);
                        } else {
                            self.request_volume();
                        }
                        // Keep the provisional grid hidden until its final pane visibility and
                        // control set have reached layout. This also remeasures tile geometry.
                        request_animation_frame(move || {
                            self.layout_tick.update(|tick| *tick += 1);
                            self.dataset_layout_ready.set(true);
                        });
                    }
                    self.slices.set(Some(Slices {
                        xy: format!("data:image/png;base64,{xy_base64}"),
                        xz: format!("data:image/png;base64,{xz_base64}"),
                        yz: format!("data:image/png;base64,{yz_base64}"),
                        viewport,
                        capture,
                    }));
                    self.error.set(None);
                    self.status.set(match pyramid_levels {
                        Some([xy, xz, yz]) => {
                            format!("Slices XY L{xy}, XZ L{xz}, YZ L{yz} in {render_ms:.0} ms")
                        }
                        None => format!("Slices in {render_ms:.0} ms"),
                    });
                    self.update_busy();
                    if self.ortho_dirty.get_value() {
                        self.request_orthogonal();
                    }
                }
            }
            SocketReply::Channels { layers, .. } => self.layers.set(layers),
            SocketReply::Error {
                request_id,
                status,
                message,
            } => {
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
        let Some(shape) = self.voxel_shape.get_untracked() else {
            return;
        };
        let clamped = [
            next[0].min(shape[0].saturating_sub(1)),
            next[1].min(shape[1].saturating_sub(1)),
            next[2].min(shape[2].saturating_sub(1)),
        ];
        let next_focus = clamped.map(|v| v as f64 + 0.5);
        let moved = self.focus.get_untracked() != next_focus;
        self.focus.set(next_focus);
        if clamped != self.crosshair.get_untracked() {
            self.crosshair.set(clamped);
            if !self.uses_xy_tile_cache() {
                self.request_orthogonal();
            }
        }
        if moved && !self.uses_xy_tile_cache() {
            self.request_interactive_volume();
        }
    }

    /// Move the focus continuously (a pan): the crosshair follows as its floor, and the slices
    /// are re-cut only when that integer changes.
    pub fn set_focus(self, next: [f64; 3]) {
        let Some(shape) = self.voxel_shape.get_untracked() else {
            return;
        };
        let clamped: [f64; 3] =
            std::array::from_fn(|axis| next[axis].clamp(0.0, shape[axis].max(1) as f64));
        let moved = self.focus.get_untracked() != clamped;
        self.focus.set(clamped);
        let crosshair: [u32; 3] = std::array::from_fn(|axis| {
            (clamped[axis].floor() as u32).min(shape[axis].saturating_sub(1))
        });
        if crosshair != self.crosshair.get_untracked() {
            self.crosshair.set(crosshair);
            if !self.uses_xy_tile_cache() {
                self.request_orthogonal();
            }
        }
        if moved && !self.uses_xy_tile_cache() {
            self.request_interactive_volume();
        }
    }

    pub fn orbit_by(self, dx: i32, dy: i32) {
        if dx == 0 && dy == 0 {
            return;
        }
        self.camera
            .update(|camera| camera.orientation = drag_orientation(camera.orientation, dx, dy));
        self.request_interactive_volume();
    }

    pub fn zoom_by(self, factor: f32) {
        self.camera.update(|camera| {
            camera.zoom = (camera.zoom * factor).clamp(ZOOM_RANGE.0, ZOOM_RANGE.1)
        });
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
        // The mode's classes and hidden panes must reach browser layout before anything reads
        // clientWidth/clientHeight for the replacement geometry.
        request_animation_frame(move || {
            self.layout_tick.update(|tick| *tick += 1);
            self.request_orthogonal();
            self.request_volume();
        });
    }

    /// One channel edit: shown at once, sent latest-only, then both frames follow.
    pub fn set_channel(self, layer_id: u64, channel: usize, state: ChannelStateInput) {
        self.layers.update(|layers| {
            if let Some(slot) = layers
                .iter_mut()
                .find(|layer| layer.layer_id == layer_id)
                .and_then(|layer| layer.channels.get_mut(channel))
            {
                if slot.enabled != state.enabled
                    || slot.color_srgb != state.color_srgb
                    || slot.opacity != state.opacity
                {
                    self.channel_tile_dirty.set_value(true);
                }
                slot.enabled = state.enabled;
                slot.color_srgb = state.color_srgb;
                slot.window_start = state.window_start;
                slot.window_end = state.window_end;
                slot.opacity = state.opacity;
            }
        });
        let edit = ChannelEdit {
            layer_id,
            channel,
            state,
        };
        if self.channel_inflight.get_value() {
            self.channel_dirty.set_value(Some(edit));
            return;
        }
        self.post_channel(edit);
    }

    pub fn set_slice_window(self, layer_id: u64, channel: usize, window: [f64; 2]) {
        if !window.iter().all(|value| value.is_finite()) || window[0] >= window[1] {
            return;
        }
        self.slice_windows.update(|windows| {
            windows.insert((layer_id, channel), window);
        });
        self.tile_generation
            .update(|generation| *generation = generation.wrapping_add(1));
    }

    fn post_channel(self, edit: ChannelEdit) {
        let Some(dataset) = self.dataset.get_untracked() else {
            return;
        };
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
                if self.channel_tile_dirty.get_value() {
                    self.channel_tile_dirty.set_value(false);
                    self.tile_generation
                        .update(|generation| *generation = generation.wrapping_add(1));
                }
                self.request_orthogonal();
                self.request_volume();
            }
        });
    }

    pub fn add_layer(self, layer_dataset: String) {
        let Some(dataset) = self.dataset.get_untracked() else {
            return;
        };
        let url = layers_url(&self.origin.get_untracked(), &dataset);
        self.status.set(format!("Adding layer {layer_dataset}…"));
        spawn_local(async move {
            match post_json::<Vec<LayerChannelSummary>, _>(
                &url,
                &LayerRequest {
                    dataset: layer_dataset,
                },
            )
            .await
            {
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
    let module = js_sys::Reflect::get(&window(), &JsValue::from_str("newvolimSceneWebGpu"))
        .map_err(js_error)?;
    if module.is_undefined() {
        return Err("scene-webgpu.js did not load".into());
    }
    let installed =
        js_sys::Reflect::get(&module, &JsValue::from_str("shader")).map_err(js_error)?;
    if installed.is_null() || installed.is_undefined() {
        js_sys::Reflect::set(
            &module,
            &JsValue::from_str("shader"),
            &JsValue::from_str(palace_core::gpu::SCENE_DVR_SHADER),
        )
        .map_err(js_error)?;
    }
    Ok(())
}

async fn dispatch_on_gpu(
    dispatch: &palace_core::gpu::SceneDvrDispatch,
) -> Result<(Vec<u32>, Vec<u32>), String> {
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
            let body = SceneChunksRequest {
                layer_id,
                level,
                source_index,
                chunks: batch.iter().map(|r| r.chunk_xyz).collect(),
            };
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
        .or_else(|| {
            js_sys::Reflect::get(&error, &JsValue::from_str("message"))
                .ok()
                .and_then(|m| m.as_string())
        })
        .unwrap_or_else(|| format!("{error:?}"))
}

fn projected_annotations_from_words(
    words: &[u32],
) -> Result<Vec<palace_core::gpu::ProjectedAnnotationPrimitive>, String> {
    if !words.len().is_multiple_of(13) {
        return Err("annotation projection has an incomplete record".into());
    }
    words
        .chunks_exact(13)
        .map(|record| {
            let color = record[2].to_be_bytes();
            let color = [color[1], color[2], color[3]];
            let vertices: [[f32; 3]; 3] = std::array::from_fn(|index| {
                std::array::from_fn(|axis| f32::from_bits(record[4 + index * 3 + axis]))
            });
            let id = u64::from(record[1]);
            let primitive = match record[0] {
                1 => palace_core::gpu::ProjectedAnnotationPrimitive::point(
                    id,
                    color,
                    f32::from_bits(record[3]),
                    vertices[0],
                ),
                2 => palace_core::gpu::ProjectedAnnotationPrimitive::segment(
                    id,
                    color,
                    f32::from_bits(record[3]),
                    vertices[0],
                    vertices[1],
                ),
                3 => palace_core::gpu::ProjectedAnnotationPrimitive::triangle(id, color, vertices),
                _ => return Err("annotation projection has an unknown primitive kind".into()),
            };
            primitive.ok_or_else(|| "annotation projection contains invalid coordinates".into())
        })
        .collect()
}

// ---- HTTP -------------------------------------------------------------------------------

async fn post_bytes<B: serde::Serialize>(url: &str, body: &B) -> Result<Vec<u8>, String> {
    let request = gloo_net::http::Request::post(url)
        .json(body)
        .map_err(|error| format!("POST {url}: {error}"))?;
    let response = request
        .send()
        .await
        .map_err(|error| format!("POST {url}: {error}"))?;
    if !response.ok() {
        return Err(format!(
            "POST {url}: {} {}",
            response.status(),
            response.text().await.unwrap_or_default()
        ));
    }
    response
        .binary()
        .await
        .map_err(|error| format!("POST {url}: {error}"))
}

async fn get_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<T, String> {
    let response = gloo_net::http::Request::get(url)
        .send()
        .await
        .map_err(|error| format!("GET {url}: {error}"))?;
    if !response.ok() {
        return Err(format!(
            "GET {url}: {} {}",
            response.status(),
            response.text().await.unwrap_or_default()
        ));
    }
    response
        .json::<T>()
        .await
        .map_err(|error| format!("GET {url}: {error}"))
}

async fn post_json<T: serde::de::DeserializeOwned, B: serde::Serialize>(
    url: &str,
    body: &B,
) -> Result<T, String> {
    let request = gloo_net::http::Request::post(url)
        .json(body)
        .map_err(|error| format!("POST {url}: {error}"))?;
    let response = request
        .send()
        .await
        .map_err(|error| format!("POST {url}: {error}"))?;
    if !response.ok() {
        return Err(format!(
            "POST {url}: {} {}",
            response.status(),
            response.text().await.unwrap_or_default()
        ));
    }
    response
        .json::<T>()
        .await
        .map_err(|error| format!("POST {url}: {error}"))
}

async fn put_json<T: serde::de::DeserializeOwned, B: serde::Serialize>(
    url: &str,
    body: &B,
) -> Result<T, String> {
    let request = gloo_net::http::Request::put(url)
        .json(body)
        .map_err(|error| format!("PUT {url}: {error}"))?;
    let response = request
        .send()
        .await
        .map_err(|error| format!("PUT {url}: {error}"))?;
    if !response.ok() {
        return Err(format!(
            "PUT {url}: {} {}",
            response.status(),
            response.text().await.unwrap_or_default()
        ));
    }
    response
        .json::<T>()
        .await
        .map_err(|error| format!("PUT {url}: {error}"))
}

async fn put_geojson(url: &str, text: String) -> Result<AnnotationLayer, String> {
    let response = gloo_net::http::Request::put(url)
        .header("Content-Type", "application/geo+json")
        .body(text)
        .map_err(|error| format!("PUT {url}: {error}"))?
        .send()
        .await
        .map_err(|error| format!("PUT {url}: {error}"))?;
    if !response.ok() {
        return Err(format!(
            "PUT {url}: {} {}",
            response.status(),
            response.text().await.unwrap_or_default()
        ));
    }
    response
        .json::<AnnotationLayer>()
        .await
        .map_err(|error| format!("PUT {url}: {error}"))
}

async fn delete_request(url: &str) -> Result<(), String> {
    let response = gloo_net::http::Request::delete(url)
        .send()
        .await
        .map_err(|error| format!("DELETE {url}: {error}"))?;
    if !response.ok() {
        return Err(format!(
            "DELETE {url}: {} {}",
            response.status(),
            response.text().await.unwrap_or_default()
        ));
    }
    Ok(())
}

async fn post_empty(url: &str) -> Result<(), String> {
    let response = gloo_net::http::Request::post(url)
        .send()
        .await
        .map_err(|error| format!("POST {url}: {error}"))?;
    if !response.ok() {
        return Err(format!(
            "POST {url}: {} {}",
            response.status(),
            response.text().await.unwrap_or_default()
        ));
    }
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
    if preview {
        (size.0.div_ceil(2), size.1.div_ceil(2))
    } else {
        size
    }
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
        let tag = ev
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
            .map(|element| element.tag_name())
            .unwrap_or_default();
        if matches!(tag.as_str(), "INPUT" | "TEXTAREA" | "SELECT") {
            return;
        }
        if (ev.ctrl_key() || ev.meta_key()) && ev.key().eq_ignore_ascii_case("z") {
            ev.prevent_default();
            session.undo_annotations();
        } else if ev.key() == "Escape" {
            session.annotation_draft.set(Vec::new());
            session.annotation_tool.set(AnnotationTool::Pan);
        } else if ev.key() == "Enter"
            && matches!(
                session.annotation_tool.get_untracked(),
                AnnotationTool::Polygon | AnnotationTool::Polyline
            )
        {
            session.finish_annotation_draft();
        } else if ev.key() == "Delete"
            && session.annotation_tool.get_untracked() == AnnotationTool::Select
        {
            if let Some(id) = session.selected_annotation.get_untracked() {
                session.delete_annotation(id);
            }
        }
    });
    window_event_listener(leptos::ev::beforeunload, move |ev| {
        if session
            .annotation_layers
            .get_untracked()
            .iter()
            .any(|layer| layer.dirty)
        {
            ev.prevent_default();
            ev.set_return_value("Unsaved annotations");
        }
    });
    view! {
        <div class="workspace">
            <nav class="workspace-tabs">
                <span class="brand" title="Rusty OmeZarr Tiles">"ROZT"</span>
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
                <div class="app-container" class:probing=move || !session.dataset_layout_ready.get()>
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
                    <Show when=move || !session.dataset_layout_ready.get()>
                        <div class="dataset-loading">
                            <span class="busy">"●"</span>
                            <span>{move || session.status.get()}</span>
                            <Show when=move || session.error.get().is_some()>
                                <span class="error">{move || session.error.get().unwrap_or_default()}</span>
                            </Show>
                        </div>
                    </Show>
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
                <h1>"ROZT"</h1>
                <div class="subtitle">"Rusty OmeZarr Tiles — volume images rendered on the server or in this browser."</div>
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
    let is_2d = move || session.voxel_shape.get().is_some_and(|shape| shape[2] == 1);
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
                        {mode_button(ViewMode::Xy, "XY")}
                        <Show when=move || !is_2d()>
                            {mode_button(ViewMode::Grid, "Grid")}
                            {mode_button(ViewMode::Xz, "XZ")}
                            {mode_button(ViewMode::Yz, "YZ")}
                            {mode_button(ViewMode::Volume, "3D")}
                        </Show>
                    </div>
                    <Show when=move || !is_2d()>
                        <div class="tool-group">
                            {renderer_button(Renderer::Server, "Server", "Volume frames rendered by the server")}
                            {renderer_button(Renderer::Browser, "WebGPU", "Volume rendered in this browser: it plans residency itself and fetches only the chunks it misses")}
                        </div>
                        <div class="tool-group">
                            <button class="tool-button" title="Reset the camera" on:click=move |_| { session.camera.set(Camera::default()); session.request_volume(); }>"Reset view"</button>
                        </div>
                    </Show>
                    <div class="tool-group annotation-tools" title="Annotation and measurement tools for the XY slice">
                        {[
                            AnnotationTool::Pan, AnnotationTool::Select, AnnotationTool::Point,
                            AnnotationTool::Rectangle, AnnotationTool::Ellipse, AnnotationTool::Polygon,
                            AnnotationTool::FreehandRegion, AnnotationTool::Polyline, AnnotationTool::FreehandLine,
                            AnnotationTool::Profile,
                        ].into_iter().map(|tool| view! {
                            <button class="tool-button" title=tool.label() aria-label=tool.label()
                                class:active=move || session.annotation_tool.get() == tool
                                on:click=move |_| { session.annotation_tool.set(tool); session.annotation_draft.set(Vec::new()); }>{annotation_tool_icon(tool)}</button>
                        }).collect_view()}
                        <button class="tool-button" title="Undo the last annotation edit" aria-label="Undo"
                            on:click=move |_| session.undo_annotations()>"↶"</button>
                    </div>
                </div>
                <OrthoPane plane=Plane::Xy/>
                <OrthoPane plane=Plane::Xz/>
                <OrthoPane plane=Plane::Yz/>
                <VolumePane/>
            </div>
            <AxisSliders/>
            <LineProfilePanel/>
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
                <Show when=move || !is_2d()>
                    <span class="readout">{move || format!("3D zoom {:.2}", session.camera.get().zoom)}</span>
                </Show>
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

fn profile_polyline(
    samples: &[ProfileSample],
    values: &[f32],
    width: f32,
    height: f32,
    pad: f32,
    y_min: f32,
    y_max: f32,
) -> String {
    let x_max = samples
        .last()
        .map(|sample| sample.distance.max(1.0))
        .unwrap_or(1.0);
    samples
        .iter()
        .zip(values)
        .filter(|(_, value)| value.is_finite())
        .map(|(sample, value)| {
            let x = pad + sample.distance / x_max * (width - 2.0 * pad);
            let y = height - pad - (*value - y_min) / (y_max - y_min) * (height - 2.0 * pad);
            format!("{x:.2},{y:.2}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn measurement_color(value: f64) -> [u8; 3] {
    let t = value.clamp(0.0, 1.0);
    let stops = [
        [68.0, 1.0, 84.0],
        [59.0, 82.0, 139.0],
        [33.0, 145.0, 140.0],
        [94.0, 201.0, 98.0],
        [253.0, 231.0, 37.0],
    ];
    let scaled = t * 4.0;
    let i = (scaled.floor() as usize).min(3);
    let f = scaled - i as f64;
    [0, 1, 2].map(|axis| (stops[i][axis] * (1.0 - f) + stops[i + 1][axis] * f).round() as u8)
}

fn profile_length_label(response: &LineProfileResponse) -> String {
    let pixels = format!("{:.1} px", response.pixel_length);
    let Some(length) = response.physical_length else {
        return pixels;
    };
    let unit = match response.physical_unit.as_deref().unwrap_or("") {
        "micrometer" | "micrometre" | "um" | "µm" => "µm",
        other => other,
    };
    if unit.is_empty() {
        pixels
    } else {
        format!("{pixels} · {length:.2} {unit}")
    }
}

#[component]
fn LineProfilePanel() -> impl IntoView {
    let session = expect_context::<Session>();
    view! {
        <Show when=move || session.line_profile.get().is_some()>
            {move || {
                let Some(profile) = session.line_profile.get() else {
                    return view! { <span></span> }.into_any();
                };
                let channel_views = session.layers.get().first().map(|layer| layer.channels.clone())
                    .unwrap_or_default();
                let legend = channel_views.iter().filter(|channel| channel.enabled).map(|channel| {
                    let color = color_hex(channel.color_srgb);
                    let name = channel.label.clone().filter(|label| !label.trim().is_empty())
                        .unwrap_or_else(|| format!("Channel {}", channel.source_index + 1));
                    view! {
                        <span class="profile-channel">
                            <span class="profile-swatch" style:background=color></span>
                            {name}
                        </span>
                    }
                }).collect_view();
                let body = if profile.loading {
                    view! { <div class="profile-empty">"Sampling…"</div> }.into_any()
                } else if let Some(error) = profile.error {
                    view! { <div class="profile-error">{error}</div> }.into_any()
                } else if let Some(response) = profile.response {
                    let mut y_min = f32::INFINITY;
                    let mut y_max = f32::NEG_INFINITY;
                    for channel in &response.channels {
                        for value in &channel.values {
                            if value.is_finite() {
                                y_min = y_min.min(*value);
                                y_max = y_max.max(*value);
                            }
                        }
                    }
                    if !y_min.is_finite() || !y_max.is_finite() {
                        view! { <div class="profile-empty">"No finite values on this line."</div> }.into_any()
                    } else {
                        if (y_max - y_min).abs() < f32::EPSILON {
                            y_max = y_min + 1.0;
                        }
                        let width = 640.0_f32;
                        let height = 140.0_f32;
                        let pad = 10.0_f32;
                        let length_label = profile_length_label(&response);
                        let lines = response.channels.iter().filter_map(|channel| {
                            let view = channel_views.iter().find(|view| view.source_index == channel.index)?;
                            let points = profile_polyline(
                                &response.samples, &channel.values, width, height, pad, y_min, y_max,
                            );
                            (!points.is_empty()).then(|| view! {
                                <polyline class="profile-line" points=points stroke=color_hex(view.color_srgb)/>
                            })
                        }).collect_view();
                        view! {
                            <div class="profile-plot">
                                <svg viewBox=format!("0 0 {width} {height}") preserveAspectRatio="none">
                                    <line class="profile-axis" x1=pad y1=height-pad x2=width-pad y2=height-pad/>
                                    <line class="profile-axis" x1=pad y1=pad x2=pad y2=height-pad/>
                                    {lines}
                                </svg>
                                <div class="profile-range">
                                    <span>{format!("{y_min:.3}")}</span>
                                    <span>{format!("{length_label} · level {}", response.level)}</span>
                                    <span>{format!("{y_max:.3}")}</span>
                                </div>
                            </div>
                        }.into_any()
                    }
                } else {
                    view! { <div class="profile-empty">"Draw a line in XY."</div> }.into_any()
                };
                view! {
                    <section class="profile-panel">
                        <header class="profile-header">
                            <div>
                                <h3>"Line profile"</h3>
                                <div class="profile-channels">{legend}</div>
                            </div>
                            <button title="Close profile" on:click=move |_| {
                                session.line_profile.set(None);
                                session.annotation_draft.set(Vec::new());
                            }>"×"</button>
                        </header>
                        {body}
                    </section>
                }.into_any()
            }}
        </Show>
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
        AnnotationTool::Rectangle => {
            Some((Geometry::rect(first[0], first[1], last[0], last[1]), false))
        }
        AnnotationTool::Ellipse => {
            let (cx, cy) = ((first[0] + last[0]) * 0.5, (first[1] + last[1]) * 0.5);
            let (rx, ry) = (
                (first[0] - last[0]).abs() * 0.5,
                (first[1] - last[1]).abs() * 0.5,
            );
            if rx < 0.01 || ry < 0.01 {
                return None;
            }
            let mut ring = (0..48)
                .map(|step| {
                    let a = std::f64::consts::TAU * step as f64 / 48.0;
                    [cx + rx * a.cos(), cy + ry * a.sin()]
                })
                .collect::<Vec<_>>();
            ring.push(ring[0]);
            Some((Geometry::Polygon(vec![ring]), true))
        }
        AnnotationTool::Polygon | AnnotationTool::FreehandRegion => {
            let mut ring = simplify_annotation_points(points);
            if ring.len() < 3 {
                return None;
            }
            if ring.first() != ring.last() {
                ring.push(ring[0]);
            }
            Some((Geometry::Polygon(vec![ring]), false))
        }
        AnnotationTool::Polyline | AnnotationTool::FreehandLine => {
            let path = simplify_annotation_points(points);
            (path.len() >= 2).then_some((Geometry::LineString(path), false))
        }
        AnnotationTool::Pan | AnnotationTool::Select | AnnotationTool::Profile => None,
    }
}

fn simplify_annotation_points(points: &[[f64; 2]]) -> Vec<[f64; 2]> {
    let mut out = Vec::new();
    for point in points {
        if out
            .last()
            .is_none_or(|last: &[f64; 2]| (point[0] - last[0]).hypot(point[1] - last[1]) >= 0.5)
        {
            out.push(*point);
        }
    }
    out
}

fn annotation_svg_path(geometry: &Geometry, point_radius: f64) -> String {
    let mut result = String::new();
    for point in geometry.markers() {
        let (x, y, r) = (point[0], point[1], point_radius);
        result.push_str(&format!(
            "M {} {} a {r} {r} 0 1 0 {} 0 a {r} {r} 0 1 0 {} 0 ",
            x - r,
            y,
            2.0 * r,
            -2.0 * r
        ));
    }
    for path in geometry.outlines() {
        for (index, point) in path.iter().enumerate() {
            result.push_str(&format!(
                "{} {} {} ",
                if index == 0 { "M" } else { "L" },
                point[0],
                point[1]
            ));
        }
    }
    result
}

fn annotation_z_fade(item: &Annotation, z: i32, slab: f64) -> f64 {
    let end = item.plane.z as i64 + item.z_extent as i64;
    let distance = if (z as i64) < item.plane.z as i64 {
        item.plane.z as i64 - z as i64
    } else if z as i64 > end {
        z as i64 - end
    } else {
        0
    };
    if slab > 0.0 {
        (1.0 - distance as f64 / slab).clamp(0.0, 1.0)
    } else {
        1.0
    }
}

#[derive(Clone)]
enum AnnotationHandle {
    Body,
    Vertex(usize, usize),
    Corner([f64; 2]),
}

#[derive(Clone)]
struct AnnotationDrag {
    original: Annotation,
    start: [f64; 2],
    handle: AnnotationHandle,
}

fn selected_handle(annotation: &Annotation, at: [f64; 2], pad: f64) -> AnnotationHandle {
    if annotation.is_ellipse || is_rectangle_annotation(annotation) {
        if let Some([x0, y0, x1, y1]) = annotation.bounds() {
            for (corner, opposite) in [
                ([x0, y0], [x1, y1]),
                ([x1, y0], [x0, y1]),
                ([x1, y1], [x0, y0]),
                ([x0, y1], [x1, y0]),
            ] {
                if (corner[0] - at[0]).hypot(corner[1] - at[1]) <= pad {
                    return AnnotationHandle::Corner(opposite);
                }
            }
        }
    } else {
        for (path_index, path) in annotation.geometry.outlines().iter().enumerate() {
            for (vertex_index, point) in path.iter().enumerate() {
                if (point[0] - at[0]).hypot(point[1] - at[1]) <= pad {
                    return AnnotationHandle::Vertex(path_index, vertex_index);
                }
            }
        }
    }
    AnnotationHandle::Body
}

fn closest_annotation_edge(
    annotation: &Annotation,
    at: [f64; 2],
    pad: f64,
) -> Option<(usize, usize)> {
    let mut closest = None;
    let mut best = pad * pad;
    for (path_index, path) in annotation.geometry.outlines().iter().enumerate() {
        for (edge_index, pair) in path.windows(2).enumerate() {
            let (a, b) = (pair[0], pair[1]);
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let length = dx * dx + dy * dy;
            if length <= 0.0 {
                continue;
            }
            let t = (((at[0] - a[0]) * dx + (at[1] - a[1]) * dy) / length).clamp(0.0, 1.0);
            let distance = (at[0] - a[0] - t * dx).powi(2) + (at[1] - a[1] - t * dy).powi(2);
            if distance < best {
                best = distance;
                closest = Some((path_index, edge_index));
            }
        }
    }
    closest
}

fn is_rectangle_annotation(annotation: &Annotation) -> bool {
    match &annotation.geometry {
        Geometry::Polygon(rings) if rings.len() == 1 && rings[0].len() == 5 => {
            let p = &rings[0];
            p[0] == p[4]
                && p[0][1] == p[1][1]
                && p[1][0] == p[2][0]
                && p[2][1] == p[3][1]
                && p[3][0] == p[0][0]
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
        AnnotationHandle::Vertex(path, vertex) => {
            next.geometry.move_vertex(path, vertex, dx, dy);
        }
        AnnotationHandle::Corner(opposite) => {
            let sx = if (drag.start[0] - opposite[0]).abs() > 1e-6 {
                (at[0] - opposite[0]) / (drag.start[0] - opposite[0])
            } else {
                1.0
            };
            let sy = if (drag.start[1] - opposite[1]).abs() > 1e-6 {
                (at[1] - opposite[1]) / (drag.start[1] - opposite[1])
            } else {
                1.0
            };
            next.geometry
                .scale_about(opposite[0], opposite[1], sx.max(0.01), sy.max(0.01));
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
    let scale_bar = move || {
        let (_, _, fit) = geometry()?;
        let pixels_per_voxel = fit * session.zoom_2d.get()[pane_index];
        let spacing = session.voxel_spacing.get()[h_axis];
        let units = session.spatial_units.get();
        scale_bar_spec(pixels_per_voxel, spacing, units[h_axis].as_deref())
    };
    // A completed viewport remains a correctly registered fallback while focus and zoom move.
    // Reproject it immediately; source-aligned tiles below progressively replace it.
    let cached_view_placement = move || -> Option<(f64, f64, f64, f64)> {
        let slices = session.slices.get()?;
        let capture = slices.capture;
        let (width, height, fit) = geometry()?;
        let shape = session.voxel_shape.get()?;
        let old_fit = (capture.pane_size[0] as f64 / shape[h_axis].max(1) as f64)
            .min(capture.pane_size[1] as f64 / shape[v_axis].max(1) as f64);
        let old_scale = old_fit * capture.zooms[pane_index];
        if !old_scale.is_finite() || old_scale <= 0.0 {
            return None;
        }
        let current_scale = fit * session.zoom_2d.get()[pane_index];
        let current_focus = session.focus.get();
        let world_width = capture.pane_size[0] as f64 / old_scale;
        let world_height = capture.pane_size[1] as f64 / old_scale;
        let drawn_width = world_width * current_scale;
        let drawn_height = world_height * current_scale;
        Some((
            width * 0.5 + (capture.focus_xyz[h_axis] - current_focus[h_axis]) * current_scale
                - drawn_width * 0.5,
            height * 0.5 + (capture.focus_xyz[v_axis] - current_focus[v_axis]) * current_scale
                - drawn_height * 0.5,
            drawn_width,
            drawn_height,
        ))
    };
    let visible_tiles = move || -> Option<SliceTileSet> {
        if plane != Plane::Xy {
            return None;
        }
        let shape0 = session.voxel_shape.get()?;
        if shape0[2] != 1 {
            return None;
        }
        let levels = session.pyramid_shapes_xyz.get();
        if levels.is_empty() {
            return None;
        }
        let dataset = session.dataset.get()?;
        let pane = node_ref.get()?;
        let [physical_width, physical_height] = {
            let (width, height) = physical_size(&pane);
            [width, height]
        };
        let (width, height, fit) = geometry()?;
        let zoom = session.zoom_2d.get()[pane_index];
        let level = xy_tile_level(shape0, &levels, [physical_width, physical_height], zoom);
        let level_shape = levels[level];
        let scale = fit * zoom;
        let focus = session.focus.get();
        let world_bounds = [
            (focus[0] - width * 0.5 / scale).max(0.0),
            (focus[1] - height * 0.5 / scale).max(0.0),
            (focus[0] + width * 0.5 / scale).min(shape0[0] as f64),
            (focus[1] + height * 0.5 / scale).min(shape0[1] as f64),
        ];
        if world_bounds[2] <= world_bounds[0] || world_bounds[3] <= world_bounds[1] {
            return None;
        }
        const TILE: u32 = 512;
        let source_bounds = [
            (world_bounds[0] * level_shape[0] as f64 / shape0[0] as f64).floor() as u32,
            (world_bounds[1] * level_shape[1] as f64 / shape0[1] as f64).floor() as u32,
            (world_bounds[2] * level_shape[0] as f64 / shape0[0] as f64).ceil() as u32,
            (world_bounds[3] * level_shape[1] as f64 / shape0[1] as f64).ceil() as u32,
        ];
        let tile_min = [source_bounds[0] / TILE, source_bounds[1] / TILE];
        let tile_max = [
            source_bounds[2].saturating_sub(1) / TILE,
            source_bounds[3].saturating_sub(1) / TILE,
        ];
        let origin = session.origin.get();
        let generation = session.tile_generation.get();
        let layers = session.layers.get();
        let base = layers.first()?;
        let slice_windows = session.slice_windows.get();
        let windows = base
            .channels
            .iter()
            .map(|channel| {
                (
                    channel.source_index,
                    slice_windows
                        .get(&(base.layer_id, channel.source_index))
                        .copied()
                        .unwrap_or([channel.window_start, channel.window_end]),
                )
            })
            .collect::<Vec<_>>();
        let mut tiles = Vec::new();
        for tile_y in tile_min[1]..=tile_max[1] {
            for tile_x in tile_min[0]..=tile_max[0] {
                let source_x = tile_x * TILE;
                let source_y = tile_y * TILE;
                let source_width = TILE.min(level_shape[0].saturating_sub(source_x));
                let source_height = TILE.min(level_shape[1].saturating_sub(source_y));
                tiles.push(SliceTilePlacement {
                    src: xy_tile_url(
                        &origin,
                        &dataset,
                        level as u32,
                        tile_x,
                        tile_y,
                        generation,
                        &windows,
                    ),
                    source_x,
                    source_y,
                    source_width,
                    source_height,
                });
            }
        }
        Some(SliceTileSet { level, tiles })
    };
    // Tile images keep source-level coordinates. One reactive parent transform moves them during
    // pan/zoom, so keyed tile nodes survive while their URL is valid. At a pyramid transition the
    // level in the URL changes and <For> removes the old bitmap instead of stretching it into the
    // new tile's position while the replacement image decodes.
    let tile_layer_transform = move |level: usize| -> String {
        let Some(shape0) = session.voxel_shape.get() else {
            return String::new();
        };
        let levels = session.pyramid_shapes_xyz.get();
        let Some((width, height, fit)) = geometry() else {
            return String::new();
        };
        let zoom = session.zoom_2d.get()[pane_index];
        let Some(level_shape) = levels.get(level).copied() else {
            return String::new();
        };
        let focus = session.focus.get();
        let scale = fit * zoom;
        let scale_x = scale * shape0[0] as f64 / level_shape[0].max(1) as f64;
        let scale_y = scale * shape0[1] as f64 / level_shape[1].max(1) as f64;
        let translate_x = width * 0.5 - focus[0] * scale;
        let translate_y = height * 0.5 - focus[1] * scale;
        format!("matrix({scale_x},0,0,{scale_y},{translate_x},{translate_y})")
    };
    let label_tile_sets = move || -> Vec<LabelTileSet> {
        if plane != Plane::Xy {
            return Vec::new();
        }
        let Some(base_shape) = session.voxel_shape.get() else {
            return Vec::new();
        };
        let Some((width, height, fit)) = geometry() else {
            return Vec::new();
        };
        let Some(dataset) = session.dataset.get() else {
            return Vec::new();
        };
        let zoom = session.zoom_2d.get()[pane_index];
        let scale = fit * zoom;
        let focus = session.focus.get();
        let bounds = [
            (focus[0] - width * 0.5 / scale).max(0.0),
            (focus[1] - height * 0.5 / scale).max(0.0),
            (focus[0] + width * 0.5 / scale).min(base_shape[0] as f64),
            (focus[1] + height * 0.5 / scale).min(base_shape[1] as f64),
        ];
        let origin = session.origin.get();
        let generation = session.feature_generation.get();
        let z = session.crosshair.get()[2];
        session
            .label_layers
            .get()
            .into_iter()
            .filter(|layer| layer.visible)
            .filter_map(|layer| {
                let pane: web_sys::Element = node_ref.get()?.into();
                let physical = physical_size(&pane);
                let level = xy_tile_level(
                    layer.summary.shape_xyz,
                    &layer.summary.levels,
                    [physical.0, physical.1],
                    zoom,
                );
                let level_shape = *layer.summary.levels.get(level)?;
                let source = [
                    (bounds[0] * level_shape[0] as f64 / base_shape[0].max(1) as f64).floor()
                        as u32,
                    (bounds[1] * level_shape[1] as f64 / base_shape[1].max(1) as f64).floor()
                        as u32,
                    (bounds[2] * level_shape[0] as f64 / base_shape[0].max(1) as f64)
                        .ceil()
                        .min(level_shape[0] as f64) as u32,
                    (bounds[3] * level_shape[1] as f64 / base_shape[1].max(1) as f64)
                        .ceil()
                        .min(level_shape[1] as f64) as u32,
                ];
                if source[2] <= source[0] || source[3] <= source[1] {
                    return None;
                }
                const TILE: u32 = 512;
                let min = [source[0] / TILE, source[1] / TILE];
                let max = [
                    source[2].saturating_sub(1) / TILE,
                    source[3].saturating_sub(1) / TILE,
                ];
                let selected = layer
                    .isolate
                    .then_some(layer.selected.as_ref().map(|v| v.id).unwrap_or(0));
                let measurement = session
                    .measurement_tables
                    .get()
                    .into_iter()
                    .find(|table| {
                        table.color_by.is_some()
                            && table
                                .summary
                                .region
                                .as_deref()
                                .is_none_or(|region| region == layer.summary.name)
                    })
                    .and_then(|table| {
                        let column = table.color_by?;
                        let range = table
                            .filter
                            .or_else(|| table.summary.columns.get(column)?.range)?;
                        Some((table.summary.name, column, range))
                    });
                let mut tiles = Vec::new();
                for ty in min[1]..=max[1] {
                    for tx in min[0]..=max[0] {
                        let sx = tx * TILE;
                        let sy = ty * TILE;
                        tiles.push(SliceTilePlacement {
                            src: label_tile_url(
                                &origin,
                                &dataset,
                                &layer.summary.name,
                                level as u32,
                                tx,
                                ty,
                                generation,
                                layer.outline,
                                selected,
                                layer.opacity,
                                z,
                                measurement
                                    .as_ref()
                                    .map(|(name, column, range)| (name.as_str(), *column, *range)),
                            ),
                            source_x: sx,
                            source_y: sy,
                            source_width: TILE.min(level_shape[0] - sx),
                            source_height: TILE.min(level_shape[1] - sy),
                        })
                    }
                }
                Some(LabelTileSet {
                    key: format!("{}:{level}:{generation}", layer.summary.name),
                    level,
                    level_shape,
                    tiles,
                })
            })
            .collect()
    };
    let label_transform = move |level_shape: [u32; 3]| -> String {
        let Some(shape0) = session.voxel_shape.get() else {
            return String::new();
        };
        let Some((width, height, fit)) = geometry() else {
            return String::new();
        };
        let scale = fit * session.zoom_2d.get()[pane_index];
        let focus = session.focus.get();
        format!(
            "matrix({},0,0,{},{},{})",
            scale * shape0[0] as f64 / level_shape[0].max(1) as f64,
            scale * shape0[1] as f64 / level_shape[1].max(1) as f64,
            width * 0.5 - focus[0] * scale,
            height * 0.5 - focus[1] * scale
        )
    };
    let active_tiles = RwSignal::new(None::<SliceTileSet>);
    let previous_tiles = RwSignal::new(None::<SliceTileSet>);
    let last_complete_tiles = StoredValue::new(None::<SliceTileSet>);
    let pending_tiles = StoredValue::new(HashSet::<String>::new());
    let tile_transition = RwSignal::new(0_u64);
    Effect::new(move |_| {
        let desired = visible_tiles();
        if active_tiles.get_untracked() == desired {
            return;
        }
        previous_tiles.set(last_complete_tiles.get_value());
        pending_tiles.set_value(
            desired
                .as_ref()
                .map(|set| set.tiles.iter().map(|tile| tile.src.clone()).collect())
                .unwrap_or_default(),
        );
        tile_transition.update(|transition| *transition = transition.wrapping_add(1));
        active_tiles.set(desired.clone());
        if pending_tiles.get_value().is_empty() {
            last_complete_tiles.set_value(desired);
            previous_tiles.set(None);
        }
    });
    Effect::new(move |_| {
        let _ = session.feature_generation.get();
        if plane != Plane::Xy
            || session
                .object_layers
                .get_untracked()
                .iter()
                .all(|v| !v.visible)
        {
            return;
        }
        let Some(shape) = session.voxel_shape.get() else {
            return;
        };
        let Some((width, height, fit)) = geometry() else {
            return;
        };
        let zoom = session.zoom_2d.get()[pane_index];
        let scale = fit * zoom;
        let focus = session.focus.get();
        let z = session.crosshair.get()[2] as f64;
        let slab = session
            .object_layers
            .get_untracked()
            .iter()
            .filter(|v| v.visible && v.summary.has_z)
            .map(|v| v.slab)
            .fold(0.0, f64::max);
        session.load_objects([
            (focus[0] - width * 0.5 / scale).max(0.0),
            (focus[0] + width * 0.5 / scale).min(shape[0] as f64),
            (focus[1] - height * 0.5 / scale).max(0.0),
            (focus[1] + height * 0.5 / scale).min(shape[1] as f64),
            z - slab,
            z + slab,
        ]);
    });
    let last = StoredValue::new(None::<(i32, i32)>);
    let annotation_drag = StoredValue::new(None::<AnnotationDrag>);
    let world_at = move |ev: &web_sys::PointerEvent| -> Option<([f64; 2], f64)> {
        if plane != Plane::Xy {
            return None;
        }
        let (width, height, fit) = geometry()?;
        let pane = node_ref.get_untracked()?;
        let rect = pane.get_bounding_client_rect();
        let scale = fit * session.zoom_2d.get_untracked()[pane_index];
        let focus = session.focus.get_untracked();
        Some((
            [
                focus[0] + (ev.client_x() as f64 - rect.left() - width * 0.5) / scale,
                focus[1] + (ev.client_y() as f64 - rect.top() - height * 0.5) / scale,
            ],
            scale,
        ))
    };
    let on_down = move |ev: web_sys::PointerEvent| {
        if ev.button() != 0 {
            return;
        }
        let tool = session.annotation_tool.get_untracked();
        if plane == Plane::Xy && tool != AnnotationTool::Pan {
            let Some((at, scale)) = world_at(&ev) else {
                return;
            };
            if let Some(target) = ev
                .current_target()
                .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
            {
                let _ = target.set_pointer_capture(ev.pointer_id());
            }
            match tool {
                AnnotationTool::Select => {
                    let z = session.crosshair.get_untracked()[2] as i32;
                    let visible = session
                        .active_annotations()
                        .into_iter()
                        .filter(|item| item.at_plane(z, 0))
                        .collect::<Vec<_>>();
                    let selected = session
                        .selected_annotation
                        .get_untracked()
                        .and_then(|id| visible.iter().find(|item| item.id == id))
                        .cloned();
                    let chosen = selected
                        .filter(|item| item.contains(at[0], at[1], 8.0 / scale))
                        .or_else(|| {
                            qupath::pick_annotation(&visible, at[0], at[1], 8.0 / scale).cloned()
                        });
                    session
                        .selected_annotation
                        .set(chosen.as_ref().map(|item| item.id));
                    let z = session.crosshair.get_untracked()[2];
                    session.inspect_features(
                        at[0].max(0.0).floor() as u32,
                        at[1].max(0.0).floor() as u32,
                        z,
                        1.0 / scale,
                    );
                    if let Some(item) = chosen.filter(|item| !item.locked) {
                        let handle = selected_handle(&item, at, 8.0 / scale);
                        if ev.alt_key() {
                            if let Some((path, edge)) =
                                closest_annotation_edge(&item, at, 8.0 / scale)
                            {
                                let mut edited = item;
                                if edited.geometry.insert_vertex(path, edge, at) {
                                    session.remember_annotations();
                                    session.update_annotation(edited);
                                }
                            }
                        } else if ev.shift_key() {
                            if let AnnotationHandle::Vertex(path, vertex) = handle {
                                let mut edited = item;
                                if edited.geometry.remove_vertex(path, vertex) {
                                    session.remember_annotations();
                                    session.update_annotation(edited);
                                }
                            }
                        } else {
                            session.remember_annotations();
                            annotation_drag.set_value(Some(AnnotationDrag {
                                original: item,
                                start: at,
                                handle,
                            }));
                        }
                    }
                }
                AnnotationTool::Point => session.add_annotation(Annotation {
                    geometry: Geometry::Point(at),
                    plane: AnnotationPlane::at(session.crosshair.get_untracked()[2] as i32, 0),
                    ..Annotation::default()
                }),
                AnnotationTool::Polygon | AnnotationTool::Polyline => {
                    let mut points = session.annotation_draft.get_untracked();
                    if tool == AnnotationTool::Polygon
                        && points.len() >= 3
                        && (points[0][0] - at[0]).hypot(points[0][1] - at[1]) < 8.0 / scale
                    {
                        session.finish_annotation_draft();
                    } else {
                        points.push(at);
                        session.annotation_draft.set(points);
                    }
                }
                AnnotationTool::Rectangle | AnnotationTool::Ellipse | AnnotationTool::Profile => {
                    session.annotation_draft.set(vec![at, at])
                }
                AnnotationTool::FreehandRegion | AnnotationTool::FreehandLine => {
                    session.annotation_draft.set(vec![at])
                }
                AnnotationTool::Pan => {}
            }
            return;
        }
        last.set_value(Some((ev.client_x(), ev.client_y())));
        if let Some(target) = ev
            .current_target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        {
            let _ = target.set_pointer_capture(ev.pointer_id());
        }
    };
    let on_move = move |ev: web_sys::PointerEvent| {
        let tool = session.annotation_tool.get_untracked();
        if plane == Plane::Xy && tool != AnnotationTool::Pan {
            if ev.buttons() & 1 == 0 {
                return;
            }
            let Some((at, _)) = world_at(&ev) else { return };
            match tool {
                AnnotationTool::Select => {
                    if let Some(drag) = annotation_drag.get_value() {
                        let edited = move_annotation_drag(&drag, at);
                        session.annotation_layers.update(|layers| {
                            if let Some(layer) = layers.iter_mut().find(|layer| {
                                Some(layer.id) == session.annotation_layer.get_untracked()
                            }) {
                                if let Some(item) = layer
                                    .annotations
                                    .iter_mut()
                                    .find(|item| item.id == edited.id)
                                {
                                    *item = edited;
                                }
                            }
                        });
                    }
                }
                AnnotationTool::Rectangle | AnnotationTool::Ellipse | AnnotationTool::Profile => {
                    session.annotation_draft.update(|points| {
                        if points.len() == 2 {
                            points[1] = at;
                        }
                    })
                }
                AnnotationTool::FreehandRegion | AnnotationTool::FreehandLine => {
                    session.annotation_draft.update(|points| {
                        if points
                            .last()
                            .is_none_or(|last| (last[0] - at[0]).hypot(last[1] - at[1]) >= 0.5)
                        {
                            points.push(at);
                        }
                    })
                }
                _ => {}
            }
            return;
        }
        let Some((lx, ly)) = last.get_value() else {
            return;
        };
        if ev.buttons() & 1 == 0 {
            return;
        }
        let (x, y) = (ev.client_x(), ev.client_y());
        last.set_value(Some((x, y)));
        let Some((_, _, fit)) = geometry() else {
            return;
        };
        let scale = fit * session.zoom_2d.get_untracked()[pane_index];
        let mut focus = session.focus.get_untracked();
        focus[h_axis] -= (x - lx) as f64 / scale;
        focus[v_axis] -= (y - ly) as f64 / scale;
        session.set_focus(focus);
    };
    let on_up = move |ev: web_sys::PointerEvent| {
        last.set_value(None);
        if plane != Plane::Xy {
            return;
        }
        let tool = session.annotation_tool.get_untracked();
        if tool == AnnotationTool::Select {
            if let Some(drag) = annotation_drag.get_value() {
                annotation_drag.set_value(None);
                if let Some((at, _)) = world_at(&ev) {
                    if (at[0] - drag.start[0]).hypot(at[1] - drag.start[1]) > 1e-6 {
                        session.update_annotation(move_annotation_drag(&drag, at));
                    }
                }
            }
        } else if tool == AnnotationTool::Profile {
            let points = session.annotation_draft.get_untracked();
            if points.len() == 2
                && (points[0][0] - points[1][0]).hypot(points[0][1] - points[1][1]) > 1e-6
            {
                let level = session
                    .voxel_shape
                    .get_untracked()
                    .zip(Some(session.pyramid_shapes_xyz.get_untracked()))
                    .filter(|(_, levels)| !levels.is_empty())
                    .map(|(shape, levels)| {
                        let pane = node_ref.get_untracked().expect("XY pane is mounted");
                        let (width, height) = physical_size(&pane);
                        xy_tile_level(
                            shape,
                            &levels,
                            [width, height],
                            session.zoom_2d.get_untracked()[pane_index],
                        ) as u32
                    })
                    .unwrap_or(0);
                session.request_line_profile([points[0], points[1]], level);
                session.annotation_draft.set(Vec::new());
            }
        } else if matches!(
            tool,
            AnnotationTool::Rectangle
                | AnnotationTool::Ellipse
                | AnnotationTool::FreehandRegion
                | AnnotationTool::FreehandLine
        ) {
            if session.annotation_draft.get_untracked().len() > 1 {
                session.finish_annotation_draft();
            }
        }
    };
    let on_double = move |ev: web_sys::MouseEvent| {
        if plane == Plane::Xy
            && matches!(
                session.annotation_tool.get_untracked(),
                AnnotationTool::Polygon | AnnotationTool::Polyline
            )
        {
            ev.prevent_default();
            session.finish_annotation_draft();
        }
    };
    let on_wheel = move |ev: web_sys::WheelEvent| {
        ev.prevent_default();
        let Some((width, height, fit)) = geometry() else {
            return;
        };
        let Some(pane) = node_ref.get_untracked() else {
            return;
        };
        let rect = pane.get_bounding_client_rect();
        let cursor = (
            ev.client_x() as f64 - rect.left() - width * 0.5,
            ev.client_y() as f64 - rect.top() - height * 0.5,
        );
        let old_zoom = session.zoom_2d.get_untracked()[pane_index];
        let candidate = old_zoom * (1.0 - ev.delta_y() * 0.001);
        let new_zoom = if candidate.is_finite() {
            candidate.max(0.25)
        } else {
            old_zoom
        };
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
        if !session.uses_xy_tile_cache() && session.crosshair.get_untracked() == old_crosshair {
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
            {move || match (image(), is_viewport(), cached_view_placement(), placement()) {
                (Some(src), true, Some((left, top, width, height)), _) => view! {
                    <img class="slice-image cached-slice" src=src alt=plane.label() draggable="false"
                        style:left=format!("{left}px") style:top=format!("{top}px")
                        style:width=format!("{width}px") style:height=format!("{height}px")/>
                }.into_any(),
                (Some(src), false, _, Some((left, top, width, height))) => view! {
                    <img class="slice-image" src=src alt=plane.label() draggable="false"
                        style:left=format!("{left}px") style:top=format!("{top}px")
                        style:width=format!("{width}px") style:height=format!("{height}px")/>
                }.into_any(),
                (Some(src), _, _, _) => view! { <img class="pane-image" src=src alt=plane.label() draggable="false"/> }.into_any(),
                (None, _, _, _) => view! { <div class="pane-empty">"waiting for slices…"</div> }.into_any(),
            }}
            <div class="slice-tile-layer previous" style:transform=move || previous_tiles.get()
                .map(|set| tile_layer_transform(set.level)).unwrap_or_default()>
                <For
                    each=move || previous_tiles.get().map(|set| set.tiles).unwrap_or_default()
                    key=|tile| tile.src.clone()
                    children=|tile| view! {
                        <img class="slice-tile loaded" src=tile.src draggable="false"
                            style:left=format!("{}px", tile.source_x)
                            style:top=format!("{}px", tile.source_y)
                            style:width=format!("{}px", tile.source_width)
                            style:height=format!("{}px", tile.source_height)/>
                    }
                />
            </div>
            <div class="slice-tile-layer active" style:transform=move || active_tiles.get()
                .map(|set| tile_layer_transform(set.level)).unwrap_or_default()>
                <For
                    each=move || active_tiles.get().map(|set| set.tiles).unwrap_or_default()
                    key=|tile| tile.src.clone()
                    children=move |tile| {
                        let loaded = RwSignal::new(false);
                        let src = tile.src.clone();
                        let transition = tile_transition.get_untracked();
                        view! {
                            <img class="slice-tile" class:loaded=move || loaded.get()
                                src=tile.src draggable="false" on:load=move |_| {
                                    loaded.set(true);
                                    if tile_transition.get_untracked() != transition { return; }
                                    pending_tiles.update_value(|pending| { pending.remove(&src); });
                                    if pending_tiles.get_value().is_empty() {
                                        last_complete_tiles.set_value(active_tiles.get_untracked());
                                        previous_tiles.set(None);
                                    }
                                }
                                style:left=format!("{}px", tile.source_x)
                                style:top=format!("{}px", tile.source_y)
                                style:width=format!("{}px", tile.source_width)
                                style:height=format!("{}px", tile.source_height)/>
                        }
                    }
                />
            </div>
            {move||label_tile_sets().into_iter().map(|set|view!{
                <div class="slice-tile-layer label-tiles" data-layer=set.key style:transform=label_transform(set.level_shape)>
                    {set.tiles.into_iter().map(|tile|view!{<img class="slice-tile loaded" src=tile.src draggable="false" style:left=format!("{}px",tile.source_x) style:top=format!("{}px",tile.source_y) style:width=format!("{}px",tile.source_width) style:height=format!("{}px",tile.source_height)/>}).collect_view()}
                </div>
            }).collect_view()}
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
                let profile_display = session.line_profile.get().map(|profile| {
                    let midpoint = [
                        (profile.line[0][0] + profile.line[1][0]) * 0.5,
                        (profile.line[0][1] + profile.line[1][1]) * 0.5 - 8.0 / scale,
                    ];
                    let label = profile.response.as_ref().map(profile_length_label).unwrap_or_else(|| {
                        format!("{:.1} px", (profile.line[1][0] - profile.line[0][0])
                            .hypot(profile.line[1][1] - profile.line[0][1]))
                    });
                    (
                        annotation_svg_path(
                            &Geometry::LineString(profile.line.to_vec()),
                            current_style.point_size * 0.5 / scale,
                        ),
                        midpoint,
                        label,
                    )
                });
                let profile_path = profile_display.as_ref().map(|display| display.0.clone()).unwrap_or_default();
                let profile_midpoint = profile_display.as_ref().map(|display| display.1).unwrap_or_default();
                let profile_label = profile_display.map(|display| display.2).unwrap_or_default();
                let object_points=session.object_layers.get().into_iter().filter(|layer|layer.visible).flat_map(|layer|{
                    let selected_row=layer.selected.as_ref().and_then(|v|v.get("row")).and_then(|v|v.as_u64()).map(|v|v as usize);
                    layer.points.into_iter().filter_map(move|point|{
                        let passes=layer.filters.iter().enumerate().all(|(i,filter)|filter.is_none_or(|[lo,hi]|point.values.get(i).copied().flatten().is_some_and(|v|v>=lo&&v<=hi)));
                        if !passes{return None} let fade=if layer.summary.has_z&&layer.slab>0.0{(1.0-(point.z-z as f64).abs()/layer.slab).clamp(0.0,1.0)}else{1.0};if fade<=0.0{return None}
                        let color=layer.color_by.and_then(|i|layer.summary.columns.get(i).and_then(|c|c.range).zip(point.values.get(i).copied().flatten())).map(|([lo,hi],value)|measurement_color(if hi>lo{(value-lo)/(hi-lo)}else{0.5})).unwrap_or(layer.color);
                        Some((point.x,point.y,layer.size*0.5/scale,color,layer.opacity*fade,layer.hollow,selected_row==Some(point.row)))
                    }).collect::<Vec<_>>()
                }).collect::<Vec<_>>();
                view! {
                    <svg class="annotation-overlay" style:left=format!("{left}px") style:top=format!("{top}px")
                        style:width=format!("{width}px") style:height=format!("{height}px")
                        viewBox=format!("0 0 {} {}", shape[0], shape[1]) preserveAspectRatio="none">
                        {object_points.into_iter().map(|(x,y,r,color,opacity,hollow,selected)|{let paint=format!("rgb({},{},{})",color[0],color[1],color[2]);view!{<circle class="object-point" class:selected=selected cx=x cy=y r=r stroke=paint.clone() stroke-width=(if selected{3.0}else{1.5}/scale).to_string() fill=if hollow{"none".into()}else{paint} opacity=opacity.to_string()/>}}).collect_view()}
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
                        <path class="profile-measurement" d=profile_path stroke="#ffd848" stroke-width="2" fill="none"/>
                        <text class="profile-measurement-label" x=profile_midpoint[0] y=profile_midpoint[1]
                            font-size=12.0/scale text-anchor="middle">{profile_label}</text>
                        <path class="annotation-shape selected" d=draft_path stroke="#ffd848" stroke-width="2" fill="none"/>
                    </svg>
                }.into_any()
            }}
            {move || scale_bar().map(|(width, label)| view! {
                <div class="scale-bar" style:width=format!("{width}px")>
                    <span>{label}</span>
                </div>
            })}
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
        if let Some(target) = ev
            .current_target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        {
            let _ = target.set_pointer_capture(ev.pointer_id());
        }
    };
    let on_move = move |ev: web_sys::PointerEvent| {
        let Some((lx, ly)) = last.get_value() else {
            return;
        };
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
        CubeView::new(
            [shape[0] as f32, shape[1] as f32, shape[2] as f32],
            (rect.width() as f32, rect.height() as f32),
        )
    };
    let cut_fractions = move || {
        let shape = session.voxel_shape.get_untracked().unwrap_or([1; 3]);
        let c = session.crosshair.get_untracked();
        [0, 1, 2].map(|axis| (c[axis] as f32 + 0.5) / shape[axis].max(1) as f32)
    };
    let pointer = move |ev: &web_sys::MouseEvent| -> Option<(f32, f32)> {
        let target = ev.current_target()?.dyn_into::<web_sys::Element>().ok()?;
        let rect = target.get_bounding_client_rect();
        Some((
            (ev.client_x() as f64 - rect.left()) as f32,
            (ev.client_y() as f64 - rect.top()) as f32,
        ))
    };

    // Redraw whenever the crosshair, shape or hover changes.
    Effect::new(move |_| {
        let _ = (
            session.crosshair.get(),
            session.voxel_shape.get(),
            hover.get(),
            session.view_mode.get(),
            session.volume_png.get(),
        );
        let Some(canvas) = canvas_ref.get() else {
            return;
        };
        let canvas: web_sys::HtmlCanvasElement = canvas.clone();
        draw_cube(
            &canvas,
            &view_for(&canvas),
            cut_fractions(),
            hover.get_untracked(),
            drag.get_value().map(|d| d.0),
        );
    });

    let on_down = move |ev: web_sys::PointerEvent| {
        let Some(canvas) = canvas_ref.get_untracked() else {
            return;
        };
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
        let Some(canvas) = canvas_ref.get_untracked() else {
            return;
        };
        let canvas: web_sys::HtmlCanvasElement = canvas.clone();
        let Some(at) = pointer(&ev) else { return };
        let view = view_for(&canvas);
        match drag.get_value() {
            Some((axis, start, from)) => {
                if let Some(fraction) =
                    view.drag_fraction(axis, start, (at.0 - from.0, at.1 - from.1))
                {
                    let shape = session.voxel_shape.get_untracked().unwrap_or([1; 3]);
                    let mut next = session.crosshair.get_untracked();
                    next[axis] = ((fraction * shape[axis] as f32).floor() as u32)
                        .min(shape[axis].saturating_sub(1));
                    session.move_crosshair(next);
                }
            }
            None => {
                let picked = view.pick(cut_fractions(), at);
                if picked != hover.get_untracked() {
                    hover.set(picked);
                }
                let _ = canvas.set_attribute(
                    "style",
                    if picked.is_some() {
                        "cursor: grab"
                    } else {
                        "cursor: default"
                    },
                );
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

fn draw_cube(
    canvas: &web_sys::HtmlCanvasElement,
    view: &CubeView,
    cut: [f32; 3],
    hover: Option<usize>,
    dragging: Option<usize>,
) {
    let rect = canvas.get_bounding_client_rect();
    let scale = window().device_pixel_ratio().max(0.5);
    let (w, h) = (rect.width().max(1.0), rect.height().max(1.0));
    canvas.set_width((w * scale) as u32);
    canvas.set_height((h * scale) as u32);
    let Some(context) = canvas
        .get_context("2d")
        .ok()
        .flatten()
        .and_then(|c| c.dyn_into::<web_sys::CanvasRenderingContext2d>().ok())
    else {
        return;
    };
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
        context.set_stroke_style_str(&format!(
            "rgba({}, {})",
            colours[axis],
            if active { 1.0 } else { 0.7 }
        ));
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
        let (x, y) = (
            end.0 + (end.0 - start.0) * 0.06,
            end.1 + (end.1 - start.1) * 0.06,
        );
        let _ = context.fill_text(letter, x as f64, y as f64);
    }
}

#[component]
fn AxisSliders() -> impl IntoView {
    let session = expect_context::<Session>();
    let has_time = move || session.timepoint_count.get() > 1;
    let slider = move |axis: usize, label: &'static str, class: &'static str, show_value: bool| {
        let max = move || {
            session
                .voxel_shape
                .get()
                .map(|s| s[axis].saturating_sub(1))
                .unwrap_or(0)
        };
        view! {
            <div class=format!("slider-row {class}")>
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
                <Show when=move || show_value>
                    <span class="slider-value">{move || format!("{} / {}", session.crosshair.get()[axis], max())}</span>
                </Show>
            </div>
        }
    };
    view! {
        <div class="axis-sliders">
            <Show when=has_time>
                <div class="slider-row time-axis">
                    <span>"T"</span>
                    <input
                        aria-label="Timepoint"
                        type="range"
                        min="0"
                        max=move || session.timepoint_count.get().saturating_sub(1).to_string()
                        step="1"
                        prop:value=move || session.timepoint.get().to_string()
                        on:input=move |ev| {
                            if let Ok(value) = event_target_value(&ev).parse::<u32>() {
                                session.set_timepoint(value);
                            }
                        }
                    />
                    <span class="slider-value">{move || format!("{} / {}", session.timepoint.get(), session.timepoint_count.get().saturating_sub(1))}</span>
                </div>
            </Show>
            {slider(0, "X", "x-axis", false)}
            <Show when=move || session.voxel_shape.get().is_none_or(|shape| shape[2] > 1)>
                {slider(2, "Z", "z-axis", true)}
            </Show>
            {slider(1, "Y", "y-axis", false)}
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
            <Show when=move || session.voxel_shape.get().is_none_or(|shape| shape[2] > 1)>
                <div class="layer-block">
                    <h3>"Scene"</h3>
                    <div class="depth-control" title="How far light penetrates: a multiplier on the distance over which an opaque voxel absorbs everything (the scene diagonal / 256 at 1×). Larger sees deeper.">
                        <div class="depth-header">
                            <span>"Ray depth"</span>
                            <input type="number" min="0.05" max="100" step="0.05"
                                prop:value=move || session.depth_scale.get().to_string()
                                on:change=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f32>() {
                                        session.set_depth_scale(value.clamp(0.05, 100.0));
                                    }
                                }/>
                            <span>"×"</span>
                        </div>
                        <input
                            aria-label="3D ray depth"
                            type="range"
                            min="-1.30103"
                            max="2"
                            step="0.01"
                            prop:value=move || slider_from_depth_scale(session.depth_scale.get()).to_string()
                            on:input=move |ev| {
                                if let Ok(position) = event_target_value(&ev).parse::<f32>() {
                                    session.set_depth_scale(depth_scale_from_slider(position));
                                }
                            }
                        />
                    </div>
                    <div class="hint">"Each channel's window start is its transparency cutoff and its opacity scales alpha; depth changes how far the ray sees before it saturates."</div>
                </div>
            </Show>
            <FeatureControls/>
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
fn FeatureControls() -> impl IntoView {
    let session = expect_context::<Session>();
    view! {
        <Show when=move||!session.label_layers.get().is_empty()>
            <div class="layer-block"><h3>"Labels"</h3>
            {move||session.label_layers.get().into_iter().map(|label|{let name=label.summary.name.clone();let edit_name=name.clone();let opacity_name=name.clone();let outline_name=name.clone();let isolate_name=name.clone();view!{
                <div class="feature-card">
                    <div class="channel-header"><label><input type="checkbox" prop:checked=label.visible on:change=move|ev|{let checked=event_target_checked(&ev);session.label_layers.update(|layers|if let Some(v)=layers.iter_mut().find(|v|v.summary.name==edit_name){v.visible=checked});session.feature_generation.update(|v|*v=v.wrapping_add(1));}/>{name.clone()}</label><span class="layer-meta">{if label.summary.has_color_table{"image-label colors"}else{"hashed IDs"}}</span></div>
                    <label class="compact-control">"Opacity" <input type="range" min="0" max="1" step="0.05" prop:value=label.opacity.to_string() on:input=move|ev|if let Ok(value)=event_target_value(&ev).parse(){session.label_layers.update(|layers|if let Some(v)=layers.iter_mut().find(|v|v.summary.name==opacity_name){v.opacity=value});session.feature_generation.update(|v|*v=v.wrapping_add(1));}/></label>
                    <div class="row"><label><input type="checkbox" prop:checked=label.outline on:change=move|ev|{let checked=event_target_checked(&ev);session.label_layers.update(|layers|if let Some(v)=layers.iter_mut().find(|v|v.summary.name==outline_name){v.outline=checked});session.feature_generation.update(|v|*v=v.wrapping_add(1));}/>" Outlines"</label><label><input type="checkbox" prop:checked=label.isolate on:change=move|ev|{let checked=event_target_checked(&ev);session.label_layers.update(|layers|if let Some(v)=layers.iter_mut().find(|v|v.summary.name==isolate_name){v.isolate=checked});session.feature_generation.update(|v|*v=v.wrapping_add(1));}/>" Isolate selected"</label></div>
                    <div class="feature-inspection">{label.selected.map(|v|format!("ID {}{}",v.id,v.name.map(|name|format!(" · {}{}",v.acronym.map(|a|format!("{a} — ")).unwrap_or_default(),name)).unwrap_or_default())).unwrap_or_else(||"Click the image to inspect an ID".into())}</div>
                </div>
            }}).collect_view()}</div>
        </Show>
        <Show when=move||!session.measurement_tables.get().is_empty()>
            <div class="layer-block"><h3>"Measurement tables"</h3>
            {move||session.measurement_tables.get().into_iter().map(|table|{let name=table.summary.name.clone();let color_name=name.clone();view!{
                <div class="feature-card"><div class="channel-header"><span>{name.clone()}</span><span class="layer-meta">{format!("{} rows{}",table.summary.count,table.summary.region.as_ref().map(|v|format!(" · {v}")).unwrap_or_default())}</span></div>
                <label class="compact-control">"Paint labels"<select on:change=move|ev|{let selected=event_target_value(&ev).parse::<usize>().ok();session.measurement_tables.update(|tables|if let Some(v)=tables.iter_mut().find(|v|v.summary.name==color_name){v.color_by=selected;v.filter=selected.and_then(|i|v.summary.columns.get(i).and_then(|c|c.range))});session.feature_generation.update(|v|*v=v.wrapping_add(1));}><option value="">"off"</option>{table.summary.columns.iter().enumerate().map(|(i,column)|view!{<option value=i.to_string() prop:selected=table.color_by==Some(i)>{column.name.clone()}</option>}).collect_view()}</select></label>
                {table.color_by.and_then(|column|table.summary.columns.get(column).and_then(|c|c.range).map(|full|(column,full))).map(|(_column,full)|{let range=table.filter.unwrap_or(full);let low=name.clone();let high=name.clone();view!{<div class="measurement-filter"><span>"visible"</span><input type="number" step="any" prop:value=range[0].to_string() on:change=move|ev|{if let Ok(value)=event_target_value(&ev).parse::<f64>(){session.measurement_tables.update(|tables|if let Some(v)=tables.iter_mut().find(|v|v.summary.name==low){let mut range=v.filter.unwrap_or(full);range[0]=value.min(range[1]);v.filter=Some(range)});session.feature_generation.update(|v|*v=v.wrapping_add(1));}}/><span>"–"</span><input type="number" step="any" prop:value=range[1].to_string() on:change=move|ev|{if let Ok(value)=event_target_value(&ev).parse::<f64>(){session.measurement_tables.update(|tables|if let Some(v)=tables.iter_mut().find(|v|v.summary.name==high){let mut range=v.filter.unwrap_or(full);range[1]=value.max(range[0]);v.filter=Some(range)});session.feature_generation.update(|v|*v=v.wrapping_add(1));}}/></div>}})}
                </div>
            }}).collect_view()}</div>
        </Show>
        <Show when=move||!session.object_layers.get().is_empty()>
            <div class="layer-block"><h3>"Objects & measurements"</h3>
            {move||session.object_layers.get().into_iter().map(|layer|{let name=layer.summary.name.clone();let visible_name=name.clone();let color_name=name.clone();let size_name=name.clone();let opacity_name=name.clone();let hollow_name=name.clone();let slab_name=name.clone();let color_by_name=name.clone();view!{
                <div class="feature-card">
                    <div class="channel-header"><label><input type="checkbox" prop:checked=layer.visible on:change=move|ev|{let checked=event_target_checked(&ev);session.object_layers.update(|layers|if let Some(v)=layers.iter_mut().find(|v|v.summary.name==visible_name){v.visible=checked});session.feature_generation.update(|v|*v=v.wrapping_add(1));}/>{name.clone()}</label><span class="layer-meta">{format!("{} rows",layer.summary.count)}</span></div>
                    <div class="row"><label>"Color "<input type="color" prop:value=color_hex(layer.color) on:input=move|ev|if let Some(value)=parse_color_hex(&event_target_value(&ev)){session.object_layers.update(|layers|if let Some(v)=layers.iter_mut().find(|v|v.summary.name==color_name){v.color=value})}/></label><label><input type="checkbox" prop:checked=layer.hollow on:change=move|ev|{let value=event_target_checked(&ev);session.object_layers.update(|layers|if let Some(v)=layers.iter_mut().find(|v|v.summary.name==hollow_name){v.hollow=value})}/>" Rings"</label></div>
                    <label class="compact-control">"Size"<input type="range" min="2" max="40" step="1" prop:value=layer.size.to_string() on:input=move|ev|if let Ok(value)=event_target_value(&ev).parse(){session.object_layers.update(|layers|if let Some(v)=layers.iter_mut().find(|v|v.summary.name==size_name){v.size=value})}/></label>
                    <label class="compact-control">"Opacity"<input type="range" min="0" max="1" step="0.05" prop:value=layer.opacity.to_string() on:input=move|ev|if let Ok(value)=event_target_value(&ev).parse(){session.object_layers.update(|layers|if let Some(v)=layers.iter_mut().find(|v|v.summary.name==opacity_name){v.opacity=value})}/></label>
                    {layer.summary.has_z.then(||view!{<label class="compact-control">"Z slab"<input type="range" min="0" max="64" step="1" prop:value=layer.slab.to_string() on:input=move|ev|if let Ok(value)=event_target_value(&ev).parse(){session.object_layers.update(|layers|if let Some(v)=layers.iter_mut().find(|v|v.summary.name==slab_name){v.slab=value});session.feature_generation.update(|v|*v=v.wrapping_add(1));}/></label>})}
                    <label class="compact-control">"Color by"<select on:change=move|ev|{let value=event_target_value(&ev).parse().ok();session.object_layers.update(|layers|if let Some(v)=layers.iter_mut().find(|v|v.summary.name==color_by_name){v.color_by=value})}><option value="">"fixed"</option>{layer.summary.columns.iter().enumerate().filter(|(_,v)|v.numeric).map(|(i,v)|view!{<option value=i.to_string() prop:selected=layer.color_by==Some(i)>{v.name.clone()}</option>}).collect_view()}</select></label>
                    {layer.summary.columns.iter().enumerate().filter_map(|(column,meta)|meta.range.map(|range|{let filter=layer.filters[column].unwrap_or(range);let low_name=name.clone();let high_name=name.clone();view!{<div class="measurement-filter"><span>{meta.name.clone()}</span><input type="number" step="any" prop:value=filter[0].to_string() on:change=move|ev|{if let Ok(value)=event_target_value(&ev).parse::<f64>(){session.object_layers.update(|layers|if let Some(v)=layers.iter_mut().find(|v|v.summary.name==low_name){let mut f=v.filters[column].unwrap_or(range);f[0]=value.min(f[1]);v.filters[column]=Some(f)})}}/><span>"–"</span><input type="number" step="any" prop:value=filter[1].to_string() on:change=move|ev|{if let Ok(value)=event_target_value(&ev).parse::<f64>(){session.object_layers.update(|layers|if let Some(v)=layers.iter_mut().find(|v|v.summary.name==high_name){let mut f=v.filters[column].unwrap_or(range);f[1]=value.max(f[0]);v.filters[column]=Some(f)})}}/></div>}})).collect_view()}
                    <div class="feature-inspection">{format!("{} of {} in view",layer.points.len(),layer.total_in_view)}</div>
                    {layer.selected.as_ref().map(|selected|view!{<pre class="object-inspection">{serde_json::to_string_pretty(selected).unwrap_or_default()}</pre>})}
                </div>
            }}).collect_view()}
            <Show when=move||!session.label_layers.get().is_empty()><button class="plain-button" on:click=move|_|{let labels=session.label_layers.get_untracked();let objects=session.object_layers.get_untracked();if let(Some(label),Some(object))=(labels.first(),objects.first()){session.count_regions(label.summary.name.clone(),object.summary.name.clone())}}>"Count objects by atlas region"</button></Show>
            <Show when=move||!session.region_counts.get().is_empty()><div class="region-counts">{move||session.region_counts.get().into_iter().take(100).map(|row|view!{<div><span>{row.acronym.or(row.name).unwrap_or_else(||format!("ID {}",row.id))}</span><strong>{row.count}</strong></div>}).collect_view()}</div></Show>
            </div>
        </Show>
    }
}

#[component]
fn AnnotationControls() -> impl IntoView {
    let session = expect_context::<Session>();
    let new_name = RwSignal::new("manual".to_string());
    let import_text = RwSignal::new(String::new());
    let roi_choice = RwSignal::new(String::new());
    let active = move || {
        session
            .annotation_layers
            .get()
            .into_iter()
            .find(|layer| Some(layer.id) == session.annotation_layer.get())
    };
    let selected = move || {
        active().and_then(|layer| {
            layer
                .annotations
                .into_iter()
                .find(|item| Some(item.id) == session.selected_annotation.get())
        })
    };
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

fn contrast_pointer_value(ev: &web_sys::PointerEvent, bound: f64) -> Option<f64> {
    let target = ev.current_target()?.dyn_into::<web_sys::Element>().ok()?;
    let rect = target.get_bounding_client_rect();
    // Native range thumbs travel between their half-widths rather than the element's outer edges.
    let thumb_radius = 7.0_f64.min(rect.width() * 0.5);
    let travel = (rect.width() - 2.0 * thumb_radius).max(1.0);
    let fraction = ((ev.client_x() as f64 - rect.left() - thumb_radius) / travel).clamp(0.0, 1.0);
    Some((fraction * bound).round())
}

fn set_contrast_endpoint(window: RwSignal<[f64; 2]>, value: f64, endpoint: usize) {
    window.update(|current| {
        if endpoint == 0 {
            current[0] = value.min(current[1] - 1.0).max(0.0);
        } else {
            current[1] = value.max(current[0] + 1.0);
        }
    });
}

#[component]
fn ContrastRange(
    label: &'static str,
    window: RwSignal<[f64; 2]>,
    bound: f64,
    commit: Callback<[f64; 2]>,
) -> impl IntoView {
    let drag = StoredValue::new(None::<usize>);
    let on_down = move |ev: web_sys::PointerEvent| {
        if ev.button() != 0 {
            return;
        }
        let Some(value) = contrast_pointer_value(&ev, bound) else {
            return;
        };
        let current = window.get_untracked();
        let endpoint = usize::from((value - current[0]).abs() > (value - current[1]).abs());
        drag.set_value(Some(endpoint));
        set_contrast_endpoint(window, value, endpoint);
        if let Some(target) = ev
            .current_target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
        {
            let _ = target.set_pointer_capture(ev.pointer_id());
        }
        ev.prevent_default();
    };
    let on_move = move |ev: web_sys::PointerEvent| {
        let Some(endpoint) = drag.get_value() else {
            return;
        };
        if ev.buttons() & 1 == 0 {
            drag.set_value(None);
            return;
        }
        if let Some(value) = contrast_pointer_value(&ev, bound) {
            set_contrast_endpoint(window, value, endpoint);
        }
    };
    let finish = move |_: web_sys::PointerEvent| {
        if drag.get_value().is_some() {
            commit.run(window.get_untracked());
        }
        drag.set_value(None);
    };
    let input = move |ev: web_sys::Event, endpoint: usize| {
        if let Ok(value) = event_target_value(&ev).parse::<f64>() {
            set_contrast_endpoint(window, value, endpoint);
            commit.run(window.get_untracked());
        }
    };
    view! {
        <div class="slider-row contrast-row compact">
            <span>{label}</span>
            <div class="dual-range" on:pointerdown=on_down on:pointermove=on_move
                on:pointerup=finish on:pointercancel=finish>
                <input type="range" class="min" aria-label=format!("{label} black point")
                    min="0" max=bound.to_string() step="1"
                    prop:value=move || window.get()[0].to_string()
                    on:input=move |ev| input(ev, 0)/>
                <input type="range" class="max" aria-label=format!("{label} white point")
                    min="0" max=bound.to_string() step="1"
                    prop:value=move || window.get()[1].to_string()
                    on:input=move |ev| input(ev, 1)/>
            </div>
            <span class="slider-value contrast-value">{move || {
                let [start, end] = window.get();
                format!("{start:.0}–{end:.0}")
            }}</span>
        </div>
    }
}

#[component]
fn ChannelRow(layer_id: u64, channel: ChannelSummary) -> impl IntoView {
    let session = expect_context::<Session>();
    let index = channel.source_index;
    let channel_name = channel
        .label
        .as_deref()
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("Channel {}", index + 1));
    let state = RwSignal::new(channel.to_input());
    let slice_window = RwSignal::new(
        session
            .slice_windows
            .get_untracked()
            .get(&(layer_id, index))
            .copied()
            .unwrap_or([channel.window_start, channel.window_end]),
    );
    let volume_window = RwSignal::new([channel.window_start, channel.window_end]);
    // Use the conventional integer range enclosing the current window. Keeping 255 and 65535
    // exact matters: doubling a padded endpoint put a dtype-wide window halfway along its track.
    let bound = {
        let top = state.get_untracked().window_end.max(1.0);
        if top <= 255.0 {
            255.0
        } else if top <= 4_095.0 {
            4_095.0
        } else if top <= 65_535.0 {
            65_535.0
        } else {
            top
        }
    };
    let send = move || session.set_channel(layer_id, index, state.get_untracked());
    let commit_slice =
        Callback::new(move |window: [f64; 2]| session.set_slice_window(layer_id, index, window));
    let commit_volume = Callback::new(move |window: [f64; 2]| {
        let [start, end] = window;
        state.update(|current| {
            current.window_start = start;
            current.window_end = end;
        });
        send();
    });
    view! {
        <div class="image-channel-control">
            <div class="channel-header">
                <input
                    type="checkbox"
                    prop:checked=move || state.get().enabled
                    on:change=move |ev| { state.update(|s| s.enabled = event_target_checked(&ev)); send(); }
                />
                <span class="channel-name" title=channel_name.clone()>{channel_name.clone()}</span>
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
                <ContrastRange label="2D contrast" window=slice_window bound=bound commit=commit_slice/>
                <Show when=move || session.voxel_shape.get().is_some_and(|shape| shape[2] > 1)>
                    <ContrastRange label="3D contrast" window=volume_window bound=bound commit=commit_volume/>
                </Show>
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
