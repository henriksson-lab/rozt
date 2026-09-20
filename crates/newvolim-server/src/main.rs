//! Frame service for authorized local datasets.
//!
//! This is intentionally bound to loopback by default and accepts only canonicalized paths below
//! explicitly configured roots. Remote stores, credentials, and multi-tenant authorization are
//! separate Stage-3/4 work; they must not be silently inferred from this local service.

use std::{
    collections::{HashMap, VecDeque},
    net::SocketAddr,
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path as AxumPath, Query, State,
    },
    http::{header, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use clap::Parser;
use newvolim_io::{read_array_info, read_dataset_metadata, LocalSourcePolicy};
use newvolim_portable::{
    routes::{
        composite_palace_scene_annotations, demand_scene_plan, full_level_scene_inputs_fitting, layer_chunk_words,
        portable_orthogonal_slice_pngs, scene_route_frame, ChannelStateInput, NativePortableDrawRequest, RouteRenderer,
    },
    session::{LayerChannelSummary, LocalSession},
};
use newvolim_render::{ColorEncoding, ColorFormat, DepthAttachment, PhysicalExtent, RenderTarget};
use palace_frame::{
    render_local_zarr_orthogonal_at_png, render_local_zarr_with_camera_attachments, CameraControls,
    FrameSize,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tower_http::{cors::CorsLayer, services::ServeDir};

/// Bounds a single unauthenticated loopback frame request before it reaches the renderer.
const MAX_FRAME_PIXELS: u64 = 16 * 1024 * 1024;

/// There is still only one non-cancellable Palace render at a time, but one noisy WebSocket
/// client must not evict another client's newest pending view. Bound the number of independently
/// retained sessions so this protection cannot become an unbounded queue.
const MAX_PENDING_SESSIONS: usize = 8;
const PENDING_SESSION_LIMIT_MESSAGE: &str = "render session queue is full; retry the newest view";

#[derive(Parser)]
struct Args {
    /// Loopback-only default. Do not expose this service beyond a trusted boundary without an
    /// authentication, tenancy, audit, and remote-source policy.
    #[arg(long, default_value = "127.0.0.1:9876")]
    bind: SocketAddr,

    /// A filesystem root that clients may request datasets below. Repeat for multiple roots.
    #[arg(long, required = true)]
    allow_root: Vec<PathBuf>,

    /// Named Zarr dataset made available to frame and browser chunk clients as `name`. The path
    /// must be contained by an `--allow-root`. Repeat for multiple datasets.
    #[arg(long, value_name = "NAME=PATH", required = true)]
    dataset: Vec<String>,

    /// Explicit browser origins allowed to read configured chunk datasets. Omit for same-origin
    /// use; wildcard CORS is never enabled by this service.
    #[arg(long)]
    cors_origin: Vec<HeaderValue>,

    /// Directory holding the built web page (the Trunk `dist/` of `newvolim-ui`). When given,
    /// the page is served at `/` from the same origin as the API, so a browser needs one port
    /// and no `--cors-origin`. API routes take precedence over files.
    #[arg(long, value_name = "DIR")]
    page_dir: Option<PathBuf>,
}

#[derive(Clone)]
struct AppState {
    /// Browser clients receive only these stable names, never an arbitrary filesystem path.
    datasets: Arc<HashMap<String, PathBuf>>,
    /// One portable-renderer session per configured dataset, opened on first use. It is the
    /// same `LocalSession` the desktop runs, so a frame served here is the frame the desktop
    /// would display, rendered by the same route, and channel edits sent to it persist across
    /// requests from every client of that dataset.
    sessions: SessionStore,
    /// Palace tasks are not yet cancellable. The dispatcher retains one active task and only
    /// the newest waiting request, preventing an unbounded stale-render backlog.
    render_queue: mpsc::Sender<FrameJob>,
    next_session_id: Arc<AtomicU64>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct SessionId(u64);

/// Portable-renderer sessions keyed by configured dataset name.
#[derive(Clone, Default)]
struct SessionStore {
    sessions: Arc<Mutex<HashMap<String, LocalSession>>>,
}

impl SessionStore {
    /// A snapshot of the dataset's session, opening it on first use: the dataset is opened and
    /// its default image layer prepared exactly as the desktop's open command does. The
    /// snapshot is a clone; it shares the store's WGPU device and carries the channel state as
    /// of now, so a render never holds the store's lock.
    fn session_for(&self, dataset: &str, root: &Path) -> Result<LocalSession, String> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| "portable session store lock was poisoned".to_owned())?;
        if let Some(session) = sessions.get(dataset) {
            return Ok(session.clone());
        }
        let mut session = LocalSession::default();
        session
            .open_local_omezarr(root)
            .map_err(|error| error.to_string())?;
        session
            .prepare_default_portable_image_layer()
            .map_err(|error| error.to_string())?;
        sessions.insert(dataset.to_owned(), session.clone());
        Ok(session)
    }

    /// Add a configured dataset as a further image layer of another dataset's session. Only
    /// registry names are accepted, never paths: the registry stays the single authority.
    fn add_layer(
        &self,
        dataset: &str,
        root: &Path,
        layer_root: &Path,
    ) -> Result<Vec<LayerChannelSummary>, String> {
        self.session_for(dataset, root)?;
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| "portable session store lock was poisoned".to_owned())?;
        let session = sessions
            .get_mut(dataset)
            .ok_or_else(|| "portable session vanished while adding a layer".to_owned())?;
        session
            .add_portable_image_layer(layer_root)
            .map_err(|error| error.to_string())?;
        Ok(session.layer_channels())
    }

    /// Apply one channel edit to the dataset's session and return every layer's channels.
    /// Set the dataset session's see-through depth scale; returns the settings as stored.
    fn set_depth_scale(&self, dataset: &str, root: &Path, scale: f32) -> Result<SceneSettings, String> {
        self.session_for(dataset, root)?;
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| "portable session store lock was poisoned".to_owned())?;
        let session = sessions
            .get_mut(dataset)
            .ok_or_else(|| "session vanished while setting the depth scale".to_owned())?;
        session.set_depth_scale(scale).map_err(|error| error.to_string())?;
        Ok(SceneSettings { depth_scale: session.depth_scale() })
    }

    fn set_channel_state(
        &self,
        dataset: &str,
        root: &Path,
        layer_id: u64,
        channel: usize,
        state: ChannelStateInput,
    ) -> Result<Vec<LayerChannelSummary>, String> {
        self.session_for(dataset, root)?;
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| "portable session store lock was poisoned".to_owned())?;
        let session = sessions
            .get_mut(dataset)
            .ok_or_else(|| "portable session vanished while editing".to_owned())?;
        session
            .set_channel_state(newvolim_scene::LayerId(layer_id), channel, state.into_state()?)
            .map_err(|error| error.to_string())?;
        Ok(session.layer_channels())
    }
}

struct FrameJob {
    session_id: SessionId,
    dataset: String,
    sessions: SessionStore,
    root: PathBuf,
    size: FrameSize,
    controls: CameraControls,
    view: RenderView,
    crosshair: Option<[u32; 3]>,
    response: oneshot::Sender<RenderResult>,
}

struct RenderedFrame {
    png: Vec<u8>,
    ray_distance_pfm: Option<Vec<u8>>,
    render_ms: f64,
    /// Which route produced the frame, for the `Server-Timing` header and the socket reply.
    renderer: &'static str,
}

/// The volume frame for one request. With the dataset's portable session available this is
/// `scene_route_frame` — the same function the desktop displays and picks against, in its own
/// order of preference (demand-driven, static Palace scene, native recorder) — with the
/// session's annotations composited over a Palace frame. Without a session (a dataset that
/// cannot be opened as a portable layer, or a route error) it is Palace's Vulkan raycaster,
/// which is what every server frame was before.
fn render_volume_frame(
    session: Option<&LocalSession>,
    root: &Path,
    size: FrameSize,
    controls: CameraControls,
) -> Result<(Vec<u8>, Option<Vec<u8>>, &'static str), String> {
    if let Some(session) = session {
        let request = NativePortableDrawRequest {
            origin_xyz: [0; 3],
            extent_xyz: [1; 3],
            width: size.width,
            height: size.height,
            orbit_x: controls.orbit_delta[0],
            orbit_y: controls.orbit_delta[1],
            zoom: controls.zoom,
        };
        match scene_route_frame(session, request) {
            Ok(frame) => {
                let renderer = match frame.renderer {
                    RouteRenderer::Demand => "portable-demand",
                    RouteRenderer::Palace => "portable-palace",
                    RouteRenderer::Native => "portable-native",
                };
                let attachments = match frame.renderer {
                    RouteRenderer::Demand | RouteRenderer::Palace => {
                        composite_palace_scene_annotations(session, request, frame.attachments)?
                    }
                    RouteRenderer::Native => frame.attachments,
                };
                let (png, pfm) =
                    palace_png::encode_portable_frame_attachments(&attachments).into_parts();
                return Ok((png, pfm, renderer));
            }
            Err(portable_error) => {
                // Vulkan is the last resort; when it fails too, the portable route's reason is
                // the one worth reading.
                let attachments = render_local_zarr_with_camera_attachments(root, size, controls)
                    .map_err(|error| format!("{error} (portable route: {portable_error})"))?;
                let (png, pfm) = palace_png::encode_attachments(&attachments).into_parts();
                return Ok((png, pfm, "vulkan"));
            }
        }
    }
    let attachments = render_local_zarr_with_camera_attachments(root, size, controls)
        .map_err(|error| error.to_string())?;
    let (png, pfm) = palace_png::encode_attachments(&attachments).into_parts();
    Ok((png, pfm, "vulkan"))
}

struct RenderedOrthogonal {
    png: [Vec<u8>; 3],
    render_ms: f64,
    voxel_shape_xyz: [u32; 3],
    crosshair_xyz: [u32; 3],
}

enum RenderOutput {
    Volume(RenderedFrame),
    Orthogonal(RenderedOrthogonal),
}

type RenderResult = Result<RenderOutput, String>;

struct RenderCompletion {
    response: oneshot::Sender<RenderResult>,
    result: RenderResult,
}

/// Maximum accepted control message. Frame bytes travel server-to-client and are separately
/// bounded by [`MAX_FRAME_PIXELS`]; this limits untrusted JSON before deserialization.
const MAX_SOCKET_REQUEST_BYTES: usize = 64 * 1024;

/// Hard upper bound for one metadata document or chunk served to browser clients. The direct
/// browser page pool is 4 MiB today, but metadata can be larger; callers must not turn this
/// local service into an unbounded file reader.
const MAX_ZARR_ASSET_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FrameRequest {
    /// An opaque configured dataset name. Deliberately do not accept a filesystem path from a
    /// frame client: the server's registry remains the single authority for both frame and
    /// browser-chunk requests.
    dataset: String,
    width: u32,
    height: u32,
    /// Optional cumulative trackball controls. Omitted fields retain the fitted-volume camera.
    #[serde(default)]
    orbit_x: i32,
    #[serde(default)]
    orbit_y: i32,
    #[serde(default = "default_zoom")]
    zoom: f32,
    /// Opaque client sequence number echoed in WebSocket replies. It lets clients drop an old
    /// final frame without assigning semantic meaning to the server's renderer generation.
    #[serde(default)]
    request_id: u64,
    #[serde(default)]
    view: RenderView,
    #[serde(default)]
    x: Option<u32>,
    #[serde(default)]
    y: Option<u32>,
    #[serde(default)]
    z: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
enum RenderView {
    #[default]
    Volume,
    Orthogonal,
}

fn default_zoom() -> f32 {
    1.0
}

#[derive(Debug, Serialize)]
struct Health {
    status: &'static str,
}

/// Browser-safe dataset discovery. Paths and arbitrary root metadata are deliberately absent.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DatasetList {
    datasets: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SocketFrame {
    #[serde(rename = "type")]
    kind: &'static str,
    request_id: u64,
    width: u32,
    height: u32,
    mime_type: &'static str,
    target: RenderTarget,
    progress: &'static str,
    /// Renderer wall time only, excluding dispatcher wait, socket transfer, and client decode.
    render_ms: f64,
    data_base64: String,
    /// Optional first-opacity distance attachment, encoded as a little-endian grayscale PFM.
    /// It is omitted only for renderer paths that do not produce a paired attachment.
    #[serde(skip_serializing_if = "Option::is_none")]
    ray_distance_pfm_base64: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SocketOrthogonal {
    #[serde(rename = "type")]
    kind: &'static str,
    request_id: u64,
    width: u32,
    height: u32,
    mime_type: &'static str,
    target: RenderTarget,
    progress: &'static str,
    render_ms: f64,
    xy_base64: String,
    xz_base64: String,
    yz_base64: String,
    voxel_shape_xyz: [u32; 3],
    crosshair_xyz: [u32; 3],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SocketError {
    #[serde(rename = "type")]
    kind: &'static str,
    request_id: Option<u64>,
    status: u16,
    message: String,
}

fn png_target(width: u32, height: u32, has_ray_distance: bool) -> RenderTarget {
    RenderTarget::new(
        PhysicalExtent::new(width, height).expect("render extent was validated before encoding"),
        ColorFormat::Rgba8Unorm,
        ColorEncoding::Srgb,
        if has_ray_distance {
            DepthAttachment::RayDistanceF32
        } else {
            DepthAttachment::None
        },
    )
    .expect("sRGB RGBA8 PNG transport target is valid")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let (render_queue, queue_receiver) = mpsc::channel(8);
    tokio::spawn(render_dispatcher(queue_receiver));
    let policy = LocalSourcePolicy::new(args.allow_root)?;
    let datasets = parse_dataset_registry(args.dataset, &policy)?;
    let state = AppState {
        datasets: Arc::new(datasets),
        render_queue,
        next_session_id: Arc::new(AtomicU64::new(1)),
        sessions: SessionStore::default(),
    };
    let app = app_router(state, args.cors_origin, args.page_dir);
    let listener = tokio::net::TcpListener::bind(args.bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn app_router(
    state: AppState,
    cors_origins: Vec<HeaderValue>,
    page_dir: Option<PathBuf>,
) -> Router {
    let router = Router::new()
        .route("/health", get(health))
        .route("/v1/frame", post(render_frame))
        .route("/v1/frames", get(frame_socket))
        .route("/v1/datasets", get(list_datasets))
        .route("/v1/datasets/{dataset}/zarr/{*asset}", get(read_zarr_asset))
        .route(
            "/v1/datasets/{dataset}/channels",
            get(dataset_channels).post(set_dataset_channel),
        )
        .route("/v1/datasets/{dataset}/layers", post(add_dataset_layer))
        .route(
            "/v1/datasets/{dataset}/settings",
            get(dataset_settings).post(set_dataset_settings),
        )
        .route("/v1/datasets/{dataset}/portable/scene", get(browser_scene))
        .route("/v1/datasets/{dataset}/portable/plan", get(browser_scene_plan))
        .route("/v1/datasets/{dataset}/portable/rays", get(browser_scene_rays))
        .route("/v1/datasets/{dataset}/portable/chunks", post(browser_scene_chunks))
        .with_state(state)
        .layer(CorsLayer::new().allow_origin(cors_origins));
    match page_dir {
        // Static files only where no API route matched; `index.html` for directory requests.
        Some(dir) => router.fallback_service(ServeDir::new(dir)),
        None => router,
    }
}

/// One channel edit for a dataset's portable session, over HTTP or the frame socket.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChannelEdit {
    layer_id: u64,
    channel: usize,
    state: ChannelStateInput,
}

/// A frame socket message: a channel edit (which carries `state`) or a frame request. The edit
/// is tried first because a frame request never has a `state` field.
#[derive(Deserialize)]
#[serde(untagged)]
enum SocketRequest {
    Channel(ChannelRequest),
    Frame(FrameRequest),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChannelRequest {
    dataset: String,
    #[serde(default)]
    request_id: u64,
    #[serde(flatten)]
    edit: ChannelEdit,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SocketChannels {
    #[serde(rename = "type")]
    kind: &'static str,
    request_id: u64,
    dataset: String,
    layers: Vec<LayerChannelSummary>,
}

/// Every image layer's channels of a dataset's portable session, as the desktop's
/// `layer_channels` command reports them.
async fn dataset_channels(
    State(state): State<AppState>,
    AxumPath(dataset): AxumPath<String>,
) -> Result<Json<Vec<LayerChannelSummary>>, (StatusCode, String)> {
    let root = resolve_frame_dataset(&state.datasets, &dataset)?;
    let session = tokio::task::spawn_blocking(move || state.sessions.session_for(&dataset, &root))
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "session task failed".to_owned()))?
        .map_err(|message| (StatusCode::BAD_REQUEST, message))?;
    Ok(Json(session.layer_channels()))
}

/// Scene-wide render settings of a dataset's session.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SceneSettings {
    /// See-through depth: a multiplier on the opacity reference, 1 by default.
    depth_scale: f32,
}

async fn dataset_settings(
    State(state): State<AppState>,
    AxumPath(dataset): AxumPath<String>,
) -> Result<Json<SceneSettings>, (StatusCode, String)> {
    let root = resolve_frame_dataset(&state.datasets, &dataset)?;
    let session = tokio::task::spawn_blocking(move || state.sessions.session_for(&dataset, &root))
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "session task failed".to_owned()))?
        .map_err(|message| (StatusCode::BAD_REQUEST, message))?;
    Ok(Json(SceneSettings { depth_scale: session.depth_scale() }))
}

async fn set_dataset_settings(
    State(state): State<AppState>,
    AxumPath(dataset): AxumPath<String>,
    Json(settings): Json<SceneSettings>,
) -> Result<Json<SceneSettings>, (StatusCode, String)> {
    let root = resolve_frame_dataset(&state.datasets, &dataset)?;
    let sessions = state.sessions.clone();
    tokio::task::spawn_blocking(move || sessions.set_depth_scale(&dataset, &root, settings.depth_scale))
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "settings task failed".to_owned()))?
        .map_err(|message| (StatusCode::BAD_REQUEST, message))
        .map(Json)
}

/// The camera for a browser scene packet; the same controls a frame request carries.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SceneQuery {
    width: u32,
    height: u32,
    #[serde(default)]
    orbit_x: i32,
    #[serde(default)]
    orbit_y: i32,
    #[serde(default = "default_zoom")]
    zoom: f32,
}

/// Everything a browser uploads to run the desktop's scene shader itself, byte for byte what
/// the desktop's recorder would upload for the same session and camera: the WGSL, the four
/// static pages (bindings 0–3), the metadata + LUT + residency words (4), the rays (5), the
/// 16-word uniform (7), the request-table capacity whose buffer starts as all `0xFFFFFFFF` (8),
/// and the output word count (6) with the workgroup count. Word arrays are little-endian `u32`,
/// base64. Every chunk of each layer's level is resident, so one dispatch renders the whole
/// scene; the levels are the ones the demand route would choose for this camera.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserScenePacket {
    shader: &'static str,
    width: u32,
    height: u32,
    levels: Vec<u32>,
    params: [u32; 16],
    pages: [String; 4],
    scene_data: String,
    rays: String,
    request_capacity: u32,
    output_words: u32,
    workgroups: u32,
}

fn words_base64(words: &[u32]) -> String {
    STANDARD.encode(
        words
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>(),
    )
}

fn browser_scene_packet(
    session: &LocalSession,
    size: FrameSize,
    controls: CameraControls,
) -> Result<BrowserScenePacket, String> {
    let request = NativePortableDrawRequest {
        origin_xyz: [0; 3],
        extent_xyz: [1; 3],
        width: size.width,
        height: size.height,
        orbit_x: controls.orbit_delta[0],
        orbit_y: controls.orbit_delta[1],
        zoom: controls.zoom,
    };
    // The camera's levels, stepped coarser until the whole level fits the static pages.
    let (input, table, _, levels) = full_level_scene_inputs_fitting(session, request)?;
    let dispatch = palace_wgpu::scene_dvr_dispatch(&input, Some(&table), 4_096, 16)?;
    Ok(BrowserScenePacket {
        shader: palace_wgpu::SCENE_DVR_SHADER,
        width: size.width,
        height: size.height,
        levels,
        params: dispatch.params,
        pages: std::array::from_fn(|index| words_base64(&dispatch.pages[index])),
        scene_data: words_base64(&dispatch.scene_data),
        rays: words_base64(&dispatch.rays),
        request_capacity: u32::try_from(dispatch.request_capacity)
            .map_err(|_| "request table is too large for the wire".to_owned())?,
        output_words: u32::try_from(dispatch.output_words)
            .map_err(|_| "output is too large for the wire".to_owned())?,
        workgroups: dispatch.workgroups,
    })
}

/// Client residency, step one: the plan for a camera — layers, channels, tags, owners,
/// transfers, step, the levels chosen (or those given as `levels=a,b`) — without pages or rays.
async fn browser_scene_plan(
    State(state): State<AppState>,
    AxumPath(dataset): AxumPath<String>,
    Query(query): Query<ScenePlanQuery>,
) -> Result<Json<newvolim_residency::ScenePlan>, (StatusCode, String)> {
    let (request, levels) = scene_plan_request(&query)?;
    let root = resolve_frame_dataset(&state.datasets, &dataset)?;
    let sessions = state.sessions.clone();
    tokio::task::spawn_blocking(move || {
        let session = sessions.session_for(&dataset, &root)?;
        demand_scene_plan(&session, request, levels.as_deref()).map(|(plan, _)| plan)
    })
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "plan task failed".to_owned()))?
    .map_err(|message| (StatusCode::BAD_REQUEST, message))
    .map(Json)
}

/// Client residency, step two: the rays for a camera as little-endian `u32` words, eight per
/// pixel (origin, direction, near, far as `f32` bits), binary.
async fn browser_scene_rays(
    State(state): State<AppState>,
    AxumPath(dataset): AxumPath<String>,
    Query(query): Query<ScenePlanQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let (request, levels) = scene_plan_request(&query)?;
    let root = resolve_frame_dataset(&state.datasets, &dataset)?;
    let sessions = state.sessions.clone();
    let words = tokio::task::spawn_blocking(move || {
        let session = sessions.session_for(&dataset, &root)?;
        demand_scene_plan(&session, request, levels.as_deref()).map(|(_, rays)| newvolim_residency::ray_words(&rays))
    })
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "rays task failed".to_owned()))?
    .map_err(|message| (StatusCode::BAD_REQUEST, message))?;
    Ok(([(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"))], words_le_bytes(&words)))
}

/// Client residency, step three: the words of the chunks the shader missed. The body names one
/// layer, level and source channel and the chunk grid coordinates; the reply is binary, in
/// request order: per chunk a `u32` word count, then the words, little-endian.
async fn browser_scene_chunks(
    State(state): State<AppState>,
    AxumPath(dataset): AxumPath<String>,
    Json(body): Json<SceneChunksRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    if body.chunks.is_empty() || body.chunks.len() > MAX_SCENE_CHUNKS_PER_REQUEST {
        return Err((StatusCode::BAD_REQUEST, format!("ask for 1..={MAX_SCENE_CHUNKS_PER_REQUEST} chunks per request")));
    }
    let root = resolve_frame_dataset(&state.datasets, &dataset)?;
    let sessions = state.sessions.clone();
    let chunks = tokio::task::spawn_blocking(move || {
        let session = sessions.session_for(&dataset, &root)?;
        layer_chunk_words(&session, body.layer_id, body.level, body.source_index, &body.chunks)
    })
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "chunks task failed".to_owned()))?
    .map_err(|message| (StatusCode::BAD_REQUEST, message))?;
    let mut bytes = Vec::new();
    for words in &chunks {
        bytes.extend_from_slice(&(words.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&words_le_bytes(words));
    }
    Ok(([(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"))], bytes))
}

/// One request's worth of chunks: a bound on the work one client can queue at once.
const MAX_SCENE_CHUNKS_PER_REQUEST: usize = 256;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScenePlanQuery {
    width: u32,
    height: u32,
    #[serde(default)]
    orbit_x: i32,
    #[serde(default)]
    orbit_y: i32,
    #[serde(default = "default_zoom")]
    zoom: f32,
    /// Comma-separated pyramid levels per visible layer; omitted, the camera chooses.
    #[serde(default)]
    levels: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SceneChunksRequest {
    layer_id: u64,
    level: u32,
    source_index: u32,
    chunks: Vec<[u32; 3]>,
}

fn scene_plan_request(query: &ScenePlanQuery) -> Result<(NativePortableDrawRequest, Option<Vec<u32>>), (StatusCode, String)> {
    validate_render_extent(query.width, query.height, RenderView::Volume)
        .map_err(|message| (StatusCode::BAD_REQUEST, message))?;
    let levels = match &query.levels {
        None => None,
        Some(text) => Some(
            text.split(',')
                .map(|level| level.trim().parse::<u32>().map_err(|_| (StatusCode::BAD_REQUEST, format!("levels {text:?} are not integers"))))
                .collect::<Result<Vec<_>, _>>()?,
        ),
    };
    Ok((
        NativePortableDrawRequest {
            origin_xyz: [0; 3],
            extent_xyz: [1; 3],
            width: query.width,
            height: query.height,
            orbit_x: query.orbit_x,
            orbit_y: query.orbit_y,
            zoom: query.zoom,
        },
        levels,
    ))
}

fn words_le_bytes(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_le_bytes()).collect()
}

async fn browser_scene(
    State(state): State<AppState>,
    AxumPath(dataset): AxumPath<String>,
    Query(query): Query<SceneQuery>,
) -> Result<Json<BrowserScenePacket>, (StatusCode, String)> {
    validate_render_extent(query.width, query.height, RenderView::Volume)
        .map_err(|message| (StatusCode::BAD_REQUEST, message))?;
    let size = FrameSize::new(query.width, query.height)
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    let controls = CameraControls {
        orbit_delta: [query.orbit_x, query.orbit_y],
        zoom: query.zoom,
    }
    .validate()
    .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    let root = resolve_frame_dataset(&state.datasets, &dataset)?;
    let sessions = state.sessions.clone();
    tokio::task::spawn_blocking(move || {
        let session = sessions.session_for(&dataset, &root)?;
        browser_scene_packet(&session, size, controls)
    })
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "scene task failed".to_owned()))?
    .map_err(|message| (StatusCode::BAD_REQUEST, message))
    .map(Json)
}

/// A further image layer for a dataset's session, named by its own registry entry.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LayerRequest {
    dataset: String,
}

async fn add_dataset_layer(
    State(state): State<AppState>,
    AxumPath(dataset): AxumPath<String>,
    Json(layer): Json<LayerRequest>,
) -> Result<Json<Vec<LayerChannelSummary>>, (StatusCode, String)> {
    let root = resolve_frame_dataset(&state.datasets, &dataset)?;
    let layer_root = resolve_frame_dataset(&state.datasets, &layer.dataset)?;
    let sessions = state.sessions.clone();
    tokio::task::spawn_blocking(move || sessions.add_layer(&dataset, &root, &layer_root))
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "session task failed".to_owned()))?
        .map_err(|message| (StatusCode::BAD_REQUEST, message))
        .map(Json)
}

/// Replace one channel's transfer state; every later frame of the dataset renders it.
async fn set_dataset_channel(
    State(state): State<AppState>,
    AxumPath(dataset): AxumPath<String>,
    Json(edit): Json<ChannelEdit>,
) -> Result<Json<Vec<LayerChannelSummary>>, (StatusCode, String)> {
    apply_channel_edit(&state, dataset, edit).await.map(Json)
}

async fn apply_channel_edit(
    state: &AppState,
    dataset: String,
    edit: ChannelEdit,
) -> Result<Vec<LayerChannelSummary>, (StatusCode, String)> {
    let root = resolve_frame_dataset(&state.datasets, &dataset)?;
    let sessions = state.sessions.clone();
    tokio::task::spawn_blocking(move || {
        sessions.set_channel_state(&dataset, &root, edit.layer_id, edit.channel, edit.state)
    })
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "session task failed".to_owned()))?
    .map_err(|message| (StatusCode::BAD_REQUEST, message))
}

async fn health() -> Json<Health> {
    Json(Health { status: "ok" })
}

async fn list_datasets(State(state): State<AppState>) -> Json<DatasetList> {
    let mut datasets: Vec<_> = state.datasets.keys().cloned().collect();
    datasets.sort_unstable();
    Json(DatasetList { datasets })
}

/// Serve a configured dataset asset without exposing its local path. This is intentionally a
/// narrow byte transport: NGFF metadata interpretation and codec selection stay in the browser
/// client, while source authorization remains at the server boundary.
async fn read_zarr_asset(
    State(state): State<AppState>,
    AxumPath((dataset, asset)): AxumPath<(String, String)>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let root = state
        .datasets
        .get(&dataset)
        .ok_or_else(|| (StatusCode::NOT_FOUND, "unknown browser dataset".to_owned()))?;
    let path =
        zarr_asset_path(root, &asset).map_err(|message| (StatusCode::BAD_REQUEST, message))?;
    let metadata = std::fs::metadata(&path).map_err(|_| {
        (
            StatusCode::NOT_FOUND,
            "Zarr asset does not exist".to_owned(),
        )
    })?;
    if !metadata.is_file() {
        return Err((StatusCode::NOT_FOUND, "Zarr asset is not a file".to_owned()));
    }
    if metadata.len() > MAX_ZARR_ASSET_BYTES {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("Zarr asset exceeds {MAX_ZARR_ASSET_BYTES}-byte service limit"),
        ));
    }
    let bytes = tokio::task::spawn_blocking(move || std::fs::read(path))
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Zarr read task failed".to_owned(),
            )
        })?
        .map_err(|_| {
            (
                StatusCode::NOT_FOUND,
                "Zarr asset could not be read".to_owned(),
            )
        })?;
    let content_type = if asset.ends_with("zarr.json") {
        HeaderValue::from_static("application/json")
    } else {
        HeaderValue::from_static("application/octet-stream")
    };
    Ok(([(header::CONTENT_TYPE, content_type)], bytes))
}

fn parse_dataset_registry(
    entries: Vec<String>,
    policy: &LocalSourcePolicy,
) -> Result<HashMap<String, PathBuf>, Box<dyn std::error::Error>> {
    let mut datasets = HashMap::new();
    for entry in entries {
        let (name, path) = entry
            .split_once('=')
            .ok_or("--dataset must use NAME=PATH")?;
        if !is_dataset_name(name) {
            return Err(format!("invalid browser dataset name: {name}").into());
        }
        let path = policy.authorize(path)?;
        if datasets.insert(name.to_owned(), path).is_some() {
            return Err(format!("duplicate browser dataset name: {name}").into());
        }
    }
    Ok(datasets)
}

fn is_dataset_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn zarr_asset_path(root: &Path, asset: &str) -> Result<PathBuf, String> {
    let relative = Path::new(asset);
    if asset.is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("Zarr asset path must be a non-empty relative path".to_owned());
    }
    let path = root
        .join(relative)
        .canonicalize()
        .map_err(|_| "Zarr asset does not exist")?;
    if !path.starts_with(root) {
        return Err("Zarr asset escapes configured dataset root".to_owned());
    }
    Ok(path)
}

async fn render_frame(
    State(state): State<AppState>,
    Json(request): Json<FrameRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    if request.view != RenderView::Volume {
        return Err((
            StatusCode::BAD_REQUEST,
            "HTTP /v1/frame returns one volume PNG; use WebSocket for orthogonal frames".into(),
        ));
    }
    // Stateless HTTP callers share the anonymous session. WebSocket connections receive a
    // unique server-assigned session below, which prevents cross-connection supersession.
    let rendered = enqueue_render(&state, SessionId(0), request).await?;
    let RenderOutput::Volume(rendered) = rendered else {
        unreachable!("volume HTTP request cannot produce slices")
    };
    // Two Server-Timing metrics: the renderer's wall time, and which route produced the frame.
    let server_timing = HeaderValue::try_from(format!(
        "render;dur={:.3}, route;desc=\"{}\"",
        rendered.render_ms, rendered.renderer
    ))
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not encode render timing header".to_owned(),
            )
        })?;
    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("image/png")),
            (
                header::HeaderName::from_static("server-timing"),
                server_timing,
            ),
        ],
        rendered.png,
    ))
}

/// Shares admission, extent, source authorization, and renderer controls across HTTP and
/// WebSocket transports. The dispatcher's response is final today; the protocol makes that
/// explicit so later preview/refining messages do not alter a client's frame contract.
async fn enqueue_render(
    state: &AppState,
    session_id: SessionId,
    request: FrameRequest,
) -> Result<RenderOutput, (StatusCode, String)> {
    validate_render_extent(request.width, request.height, request.view)
        .map_err(|message| (StatusCode::BAD_REQUEST, message))?;
    let size = FrameSize::new(request.width, request.height)
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    let controls = CameraControls {
        orbit_delta: [request.orbit_x, request.orbit_y],
        zoom: request.zoom,
    }
    .validate()
    .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    let crosshair =
        request_crosshair(&request).map_err(|message| (StatusCode::BAD_REQUEST, message))?;
    let root = resolve_frame_dataset(&state.datasets, &request.dataset)?;
    let (response, response_receiver) = oneshot::channel();
    state
        .render_queue
        .try_send(FrameJob {
            session_id,
            dataset: request.dataset.clone(),
            sessions: state.sessions.clone(),
            root,
            size,
            controls,
            view: request.view,
            crosshair,
            response,
        })
        .map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => (
                StatusCode::TOO_MANY_REQUESTS,
                "render admission queue is full; retry the newest view".to_owned(),
            ),
            mpsc::error::TrySendError::Closed(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "renderer is shutting down".to_owned(),
            ),
        })?;
    let rendered = response_receiver
        .await
        .map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "renderer stopped before completing the request".to_owned(),
            )
        })?
        .map_err(|error| {
            let status = if error == SUPERSEDED_MESSAGE {
                StatusCode::CONFLICT
            } else if error == PENDING_SESSION_LIMIT_MESSAGE {
                StatusCode::TOO_MANY_REQUESTS
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (status, error)
        })?;

    Ok(rendered)
}

/// Crosshair coordinates are meaningful only for orthogonal views. Accepting a partial tuple
/// would silently substitute the centre slice for a user-selected location, so the wire format
/// is deliberately all-or-nothing.
fn request_crosshair(request: &FrameRequest) -> Result<Option<[u32; 3]>, String> {
    match (request.view, request.x, request.y, request.z) {
        (RenderView::Volume, None, None, None) => Ok(None),
        (RenderView::Volume, _, _, _) => {
            Err("volume frame requests must not include crosshair coordinates".to_owned())
        }
        (RenderView::Orthogonal, None, None, None) => Ok(None),
        (RenderView::Orthogonal, Some(x), Some(y), Some(z)) => Ok(Some([x, y, z])),
        (RenderView::Orthogonal, _, _, _) => Err(
            "orthogonal frame requests must provide all of x, y, and z or omit all three"
                .to_owned(),
        ),
    }
}

fn resolve_frame_dataset(
    datasets: &HashMap<String, PathBuf>,
    dataset: &str,
) -> Result<PathBuf, (StatusCode, String)> {
    if !is_dataset_name(dataset) {
        return Err((
            StatusCode::BAD_REQUEST,
            "frame dataset name is invalid".to_owned(),
        ));
    }
    datasets.get(dataset).cloned().ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            "unknown configured frame dataset".to_owned(),
        )
    })
}

async fn frame_socket(
    upgrade: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let session_id = SessionId(state.next_session_id.fetch_add(1, Ordering::Relaxed));
    upgrade.on_upgrade(move |socket| serve_frame_socket(socket, state, session_id))
}

/// A deliberately sequential socket connection: the global dispatcher already retains the
/// newest request while a Palace task is active, and sequential replies keep per-connection
/// backpressure bounded. Clients may send the next camera state after receiving a final/error.
async fn serve_frame_socket(mut socket: WebSocket, state: AppState, session_id: SessionId) {
    while let Some(message) = socket.recv().await {
        let message = match message {
            Ok(Message::Text(text)) => {
                if text.len() > MAX_SOCKET_REQUEST_BYTES {
                    if send_socket_error(
                        &mut socket,
                        None,
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "frame request is too large",
                    )
                    .await
                    .is_err()
                    {
                        break;
                    }
                    continue;
                }
                text
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => {
                if send_socket_error(
                    &mut socket,
                    None,
                    StatusCode::BAD_REQUEST,
                    "frame request must be JSON text",
                )
                .await
                .is_err()
                {
                    break;
                }
                continue;
            }
            Err(_) => break,
        };
        let request = match serde_json::from_str::<SocketRequest>(&message) {
            Ok(SocketRequest::Channel(edit)) => {
                let request_id = edit.request_id;
                let dataset = edit.dataset.clone();
                let reply = match apply_channel_edit(&state, edit.dataset, edit.edit).await {
                    Ok(layers) => serde_json::to_string(&SocketChannels {
                        kind: "channels",
                        request_id,
                        dataset,
                        layers,
                    })
                    .expect("SocketChannels is serializable"),
                    Err((status, message)) => {
                        if send_socket_error(&mut socket, Some(request_id), status, &message)
                            .await
                            .is_err()
                        {
                            break;
                        }
                        continue;
                    }
                };
                if socket.send(Message::Text(reply.into())).await.is_err() {
                    break;
                }
                continue;
            }
            Ok(SocketRequest::Frame(request)) => request,
            Err(error) => {
                if send_socket_error(
                    &mut socket,
                    None,
                    StatusCode::BAD_REQUEST,
                    &format!("invalid frame request: {error}"),
                )
                .await
                .is_err()
                {
                    break;
                }
                continue;
            }
        };
        let request_id = request.request_id;
        let (width, height) = (request.width, request.height);
        match enqueue_render(&state, session_id, request).await {
            Ok(RenderOutput::Volume(rendered)) => {
                let frame = SocketFrame {
                    kind: "frame",
                    request_id,
                    width,
                    height,
                    mime_type: "image/png",
                    target: png_target(width, height, rendered.ray_distance_pfm.is_some()),
                    progress: "final",
                    render_ms: rendered.render_ms,
                    data_base64: STANDARD.encode(rendered.png),
                    ray_distance_pfm_base64: rendered
                        .ray_distance_pfm
                        .map(|pfm| STANDARD.encode(pfm)),
                };
                let encoded = serde_json::to_string(&frame).expect("SocketFrame is serializable");
                if socket.send(Message::Text(encoded.into())).await.is_err() {
                    break;
                }
            }
            Ok(RenderOutput::Orthogonal(rendered)) => {
                let frame = SocketOrthogonal {
                    kind: "orthogonal",
                    request_id,
                    width,
                    height,
                    mime_type: "image/png",
                    target: png_target(width, height, false),
                    progress: "final",
                    render_ms: rendered.render_ms,
                    xy_base64: STANDARD.encode(&rendered.png[0]),
                    xz_base64: STANDARD.encode(&rendered.png[1]),
                    yz_base64: STANDARD.encode(&rendered.png[2]),
                    voxel_shape_xyz: rendered.voxel_shape_xyz,
                    crosshair_xyz: rendered.crosshair_xyz,
                };
                if socket
                    .send(Message::Text(
                        serde_json::to_string(&frame)
                            .expect("SocketOrthogonal is serializable")
                            .into(),
                    ))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            Err((status, message)) => {
                if send_socket_error(&mut socket, Some(request_id), status, &message)
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }
    }
}

async fn send_socket_error(
    socket: &mut WebSocket,
    request_id: Option<u64>,
    status: StatusCode,
    message: &str,
) -> Result<(), axum::Error> {
    let error = SocketError {
        kind: "error",
        request_id,
        status: status.as_u16(),
        message: message.to_owned(),
    };
    socket
        .send(Message::Text(
            serde_json::to_string(&error)
                .expect("SocketError is serializable")
                .into(),
        ))
        .await
}

const SUPERSEDED_MESSAGE: &str = "superseded by a newer frame request";

/// Owns the non-cancellable Palace admission boundary. A single active render is allowed to
/// finish; while it runs, new requests replace the one pending request and notify the displaced
/// client, so only the newest camera state is rendered next.
async fn render_dispatcher(mut jobs: mpsc::Receiver<FrameJob>) {
    let (completed_tx, mut completed_rx) = mpsc::unbounded_channel::<RenderCompletion>();
    let mut active = false;
    let mut pending = PendingJobs::default();

    while let Some(job) = jobs.recv().await {
        if active {
            admit_pending(&mut pending, job);
            continue;
        }
        start_render(job, &completed_tx);
        active = true;

        while active {
            tokio::select! {
                Some(job) = jobs.recv() => admit_pending(&mut pending, job),
                Some(completion) = completed_rx.recv() => {
                    let _ = completion.response.send(completion.result);
                    if let Some(job) = pending.take_next() {
                        start_render(job, &completed_tx);
                    } else {
                        active = false;
                    }
                }
                else => return,
            }
        }
    }
}

fn start_render(job: FrameJob, completed_tx: &mpsc::UnboundedSender<RenderCompletion>) {
    let completed_tx = completed_tx.clone();
    tokio::task::spawn_blocking(move || {
        let started = Instant::now();
        let result = match job.view {
            RenderView::Volume => {
                let session = job.sessions.session_for(&job.dataset, &job.root).ok();
                render_volume_frame(session.as_ref(), &job.root, job.size, job.controls).map(
                    |(png, ray_distance_pfm, renderer)| {
                        RenderOutput::Volume(RenderedFrame {
                            png,
                            ray_distance_pfm,
                            render_ms: started.elapsed().as_secs_f64() * 1_000.0,
                            renderer,
                        })
                    },
                )
            }
            RenderView::Orthogonal => {
                // The portable session slices the whole base layer on the CPU at the finest
                // level that fits its pages, on any store it can open; Palace's Vulkan slicer
                // remains the fallback for a dataset the portable session cannot hold.
                let portable = job
                    .sessions
                    .session_for(&job.dataset, &job.root)
                    .and_then(|session| portable_orthogonal_slice_pngs(&session, job.crosshair));
                match portable {
                    Ok((png, slices)) => Ok(RenderOutput::Orthogonal(RenderedOrthogonal {
                        png,
                        voxel_shape_xyz: slices.voxel_shape_xyz,
                        crosshair_xyz: slices.crosshair_xyz,
                        render_ms: started.elapsed().as_secs_f64() * 1_000.0,
                    })),
                    Err(portable_error) => dataset_xyz_extent(&job.root).and_then(|shape| {
                        let requested = job
                            .crosshair
                            .unwrap_or(std::array::from_fn(|axis| shape[axis] / 2));
                        let crosshair =
                            std::array::from_fn(|axis| requested[axis].min(shape[axis] - 1));
                        render_local_zarr_orthogonal_at_png(
                            job.root,
                            job.size,
                            [crosshair[2], crosshair[1], crosshair[0]],
                        )
                        .map_err(|error| format!("{error} (portable slices: {portable_error})"))
                        .map(|png| {
                            RenderOutput::Orthogonal(RenderedOrthogonal {
                                png,
                                voxel_shape_xyz: shape,
                                crosshair_xyz: crosshair,
                                render_ms: started.elapsed().as_secs_f64() * 1_000.0,
                            })
                        })
                    }),
                }
            }
        };
        let _ = completed_tx.send(RenderCompletion {
            response: job.response,
            result,
        });
    });
}

fn dataset_xyz_extent(root: &Path) -> Result<[u32; 3], String> {
    let metadata = read_dataset_metadata(root).map_err(|error| error.to_string())?;
    let multiscale = metadata
        .multiscales
        .first()
        .ok_or_else(|| "dataset has no OME-NGFF multiscale".to_owned())?;
    let level = multiscale
        .datasets
        .first()
        .ok_or_else(|| "multiscale has no dataset level".to_owned())?;
    let array = read_array_info(root, &level.path).map_err(|error| error.to_string())?;
    let mut xyz = [0_u32; 3];
    for (output_axis, name) in ["x", "y", "z"].into_iter().enumerate() {
        let input_axis = multiscale
            .axes
            .iter()
            .position(|axis| axis.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| format!("multiscale is missing {name} axis"))?;
        xyz[output_axis] = array
            .shape
            .get(input_axis)
            .copied()
            .and_then(|value| u32::try_from(value).ok())
            .filter(|&value| value != 0)
            .ok_or_else(|| format!("{name} axis does not have a nonzero u32 extent"))?;
    }
    Ok(xyz)
}

#[derive(Default)]
struct PendingJobs {
    by_session: HashMap<SessionId, FrameJob>,
    order: VecDeque<SessionId>,
}

/// Admit a frame as the one newest pending request for its session. Existing sessions retain
/// their FIFO turn; a new session is rejected once the fixed cross-client queue is full.
fn admit_pending(pending: &mut PendingJobs, job: FrameJob) {
    let session_id = job.session_id;
    if let Some(replaced) = pending.by_session.remove(&session_id) {
        pending.by_session.insert(session_id, job);
        let _ = replaced.response.send(Err(SUPERSEDED_MESSAGE.to_owned()));
    } else if pending.by_session.len() < MAX_PENDING_SESSIONS {
        pending.by_session.insert(session_id, job);
        pending.order.push_back(session_id);
    } else {
        let _ = job
            .response
            .send(Err(PENDING_SESSION_LIMIT_MESSAGE.to_owned()));
    }
}

impl PendingJobs {
    fn take_next(&mut self) -> Option<FrameJob> {
        let session_id = self.order.pop_front()?;
        self.by_session.remove(&session_id)
    }
}

fn validate_frame_extent(width: u32, height: u32) -> Result<(), String> {
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_FRAME_PIXELS {
        return Err(format!(
            "requested frame is {pixels} pixels; limit is {MAX_FRAME_PIXELS}"
        ));
    }
    Ok(())
}

/// Bound aggregate renderer output before a request enters the single Palace queue. An
/// orthogonal reply contains three independently allocated PNG frames, so treating it as one
/// frame would let a client bypass the same memory/backpressure limit applied to volume views.
fn validate_render_extent(width: u32, height: u32, view: RenderView) -> Result<(), String> {
    if view == RenderView::Volume {
        return validate_frame_extent(width, height);
    }
    let multiplier = match view {
        RenderView::Volume => 1,
        RenderView::Orthogonal => 3,
    };
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(multiplier))
        .ok_or_else(|| {
            "requested frame dimensions overflow the aggregate pixel budget".to_owned()
        })?;
    if pixels > MAX_FRAME_PIXELS {
        return Err(format!(
            "requested {view:?} output is {pixels} pixels; aggregate limit is {MAX_FRAME_PIXELS}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use tower::ServiceExt;

    fn test_job(session_id: u64, response: oneshot::Sender<RenderResult>) -> FrameJob {
        FrameJob {
            session_id: SessionId(session_id),
            dataset: "test".to_owned(),
            sessions: SessionStore::default(),
            root: PathBuf::from("/tmp/test.ome.zarr"),
            size: FrameSize::new(1, 1).unwrap(),
            controls: CameraControls::default(),
            view: RenderView::Volume,
            crosshair: None,
            response,
        }
    }

    fn png_dimensions(png: &[u8]) -> [u32; 2] {
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(png.len() >= 24 && &png[12..16] == b"IHDR");
        [
            u32::from_be_bytes(png[16..20].try_into().unwrap()),
            u32::from_be_bytes(png[20..24].try_into().unwrap()),
        ]
    }

    #[test]
    fn frame_budget_uses_wide_multiplication_and_rejects_excess() {
        assert!(validate_frame_extent(4_096, 4_096).is_ok());
        assert!(validate_frame_extent(u32::MAX, u32::MAX).is_err());
        assert!(validate_render_extent(2_000, 2_000, RenderView::Orthogonal).is_ok());
        assert!(validate_render_extent(4_096, 4_096, RenderView::Orthogonal).is_err());
    }

    #[test]
    fn authorized_fixture_extent_uses_declared_ngff_axis_order() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        assert_eq!(dataset_xyz_extent(&root).unwrap(), [128, 128, 32]);
    }

    #[test]
    fn socket_requests_parse_frames_and_channel_edits() {
        let frame = serde_json::from_str::<SocketRequest>(
            r#"{"dataset":"cells3d","width":32,"height":24,"requestId":7,"zoom":1.5}"#,
        )
        .unwrap();
        assert!(matches!(frame, SocketRequest::Frame(request) if request.request_id == 7 && request.zoom == 1.5));
        let edit = serde_json::from_str::<SocketRequest>(
            r#"{"dataset":"cells3d","requestId":9,"layerId":1,"channel":0,"state":{"enabled":true,"colorSrgb":[255,0,0],"windowStart":100,"windowEnd":4000,"opacity":0.5}}"#,
        )
        .unwrap();
        match edit {
            SocketRequest::Channel(request) => {
                assert_eq!((request.request_id, request.edit.layer_id, request.edit.channel), (9, 1, 0));
                assert_eq!(request.edit.state.into_state().unwrap().opacity, 0.5);
            }
            SocketRequest::Frame(_) => panic!("a message carrying `state` is a channel edit"),
        }
    }

    /// The store opens a dataset once, and a channel edit reaches the session every later frame
    /// snapshot is taken from.
    #[test]
    fn channel_edits_persist_in_the_dataset_session() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        let store = SessionStore::default();
        let first = store.session_for("cells3d", &root).unwrap();
        let layers = first.layer_channels();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].channels[0].opacity, 1.0);
        let edited = store
            .set_channel_state(
                "cells3d",
                &root,
                layers[0].layer_id,
                0,
                ChannelStateInput {
                    enabled: true,
                    color_srgb: [1, 2, 3],
                    window_start: 100.0,
                    window_end: 4000.0,
                    opacity: 0.25,
                },
            )
            .unwrap();
        assert_eq!(edited[0].channels[0].opacity, 0.25);
        assert_eq!(edited[0].channels[0].color_srgb, [1, 2, 3]);
        // The earlier snapshot is unchanged; a new snapshot carries the edit.
        assert_eq!(first.layer_channels()[0].channels[0].opacity, 1.0);
        let again = store.session_for("cells3d", &root).unwrap();
        assert_eq!(again.layer_channels()[0].channels[0].opacity, 0.25);
        assert_eq!(store.sessions.lock().unwrap().len(), 1);
        assert!(store
            .set_channel_state(
                "cells3d",
                &root,
                layers[0].layer_id,
                5,
                ChannelStateInput {
                    enabled: true,
                    color_srgb: [0; 3],
                    window_start: 0.0,
                    window_end: 1.0,
                    opacity: 1.0
                }
            )
            .is_err());
    }

    #[test]
    fn a_registry_dataset_can_join_another_dataset_session_as_a_layer() {
        let cells = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        let gradient = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/two-channel-gradient.ome.zarr");
        let store = SessionStore::default();
        let layers = store.add_layer("cells3d", &cells, &gradient).unwrap();
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[1].channels.len(), 2);
        assert_eq!(store.session_for("cells3d", &cells).unwrap().layer_channels().len(), 2);
    }

    /// The browser packet, consumed exactly as the page consumes it — decoded from base64,
    /// nine bindings in the documented order, the packet's own WGSL, the packet's workgroup
    /// count, the output decoded as colour words then depth bits — renders the frame the
    /// desktop displays for the same session and camera, pixel for pixel.
    #[test]
    #[ignore = "requires a local WGPU adapter"]
    fn browser_scene_packet_reproduces_the_desktop_frame_on_the_local_adapter() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        let store = SessionStore::default();
        let session = store.session_for("cells3d", &root).unwrap();
        let size = FrameSize::new(96, 64).unwrap();
        let controls = CameraControls {
            orbit_delta: [12, -7],
            zoom: 1.3,
        }
        .validate()
        .unwrap();
        let packet = browser_scene_packet(&session, size, controls).unwrap();
        assert_eq!((packet.width, packet.height), (96, 64));
        assert_eq!(packet.output_words, 96 * 64 * 2 + palace_wgpu::SCENE_TRACE_WORDS as u32);
        let words = |encoded: &str| -> Vec<u32> {
            STANDARD
                .decode(encoded)
                .unwrap()
                .chunks_exact(4)
                .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
                .collect()
        };

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .unwrap();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            ..Default::default()
        }))
        .unwrap();
        let storage = |label: &str, words: &[u32], writable: bool| {
            let bytes = words.iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<_>>();
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: bytes.len().max(4) as u64,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_DST
                    | if writable { wgpu::BufferUsages::COPY_SRC } else { wgpu::BufferUsages::empty() },
                mapped_at_creation: false,
            });
            if !bytes.is_empty() {
                queue.write_buffer(&buffer, 0, &bytes);
            }
            buffer
        };
        let pages: Vec<_> = packet
            .pages
            .iter()
            .enumerate()
            .map(|(index, page)| storage(&format!("page {index}"), &words(page), false))
            .collect();
        let scene_data = storage("scene data", &words(&packet.scene_data), false);
        let rays = storage("rays", &words(&packet.rays), false);
        let output = storage("output", &vec![0; packet.output_words as usize], true);
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("params"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(
            &uniform,
            0,
            &packet.params.iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<_>>(),
        );
        let requests = storage("requests", &vec![u32::MAX; packet.request_capacity as usize], true);
        let entry = |binding: u32, read_only: bool| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("browser scene layout"),
            entries: &[
                entry(0, true),
                entry(1, true),
                entry(2, true),
                entry(3, true),
                entry(4, true),
                entry(5, true),
                entry(6, false),
                wgpu::BindGroupLayoutEntry {
                    binding: 7,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                entry(8, false),
            ],
        });
        fn bind(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
            wgpu::BindGroupEntry {
                binding,
                resource: buffer.as_entire_binding(),
            }
        }
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("browser scene group"),
            layout: &layout,
            entries: &[
                bind(0, &pages[0]),
                bind(1, &pages[1]),
                bind(2, &pages[2]),
                bind(3, &pages[3]),
                bind(4, &scene_data),
                bind(5, &rays),
                bind(6, &output),
                bind(7, &uniform),
                bind(8, &requests),
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("browser scene shader"),
            source: wgpu::ShaderSource::Wgsl(packet.shader.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: None,
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: u64::from(packet.output_words) * 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(packet.workgroups, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, u64::from(packet.output_words) * 4);
        queue.submit([encoder.finish()]);
        let (sender, receiver) = std::sync::mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        receiver.recv().unwrap().unwrap();
        let output_words = {
            let mapped = readback.slice(..).get_mapped_range().unwrap();
            mapped
                .chunks_exact(4)
                .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
                .collect::<Vec<_>>()
        };
        readback.unmap();
        let (frame, _) = palace_wgpu::scene_frame_from_output(96, 64, &output_words).unwrap();

        let desktop = scene_route_frame(
            &session,
            NativePortableDrawRequest {
                origin_xyz: [0; 3],
                extent_xyz: [1; 3],
                width: 96,
                height: 64,
                orbit_x: 12,
                orbit_y: -7,
                zoom: 1.3,
            },
        )
        .unwrap();
        assert_eq!(desktop.renderer, RouteRenderer::Demand);
        assert_eq!(frame.first_opacity_distance, desktop.attachments.first_opacity_distance);
        assert_eq!(frame.rgba, desktop.attachments.rgba);
        assert!(frame.first_opacity_distance.iter().any(|d| d.is_finite()));
    }

    /// The page dispatches the packet exactly as the test above does: the endpoint path, and
    /// the nine bindings in order.
    #[test]
    fn webview_dispatches_the_browser_scene_packet_with_the_documented_bindings() {
        let source = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../newvolim-ui/scene-webgpu.js"),
        )
        .unwrap();
        let start = source.find("async function dispatch(").unwrap();
        let body = &source[start..];
        let body = &body[..body.find("async function present(").unwrap_or(body.len())];
        // The page fetches the plan, rays and chunks itself (client-side residency).
        let api = std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../newvolim-ui/src/api.rs")).unwrap();
        for route in ["/portable/plan?", "/portable/rays?", "/portable/chunks"] {
            assert!(api.contains(route), "the page does not name {route}");
        }
        let mut last = 0;
        for (binding, resource) in [
            (0, "pageBuffers[0]"),
            (1, "pageBuffers[1]"),
            (2, "pageBuffers[2]"),
            (3, "pageBuffers[3]"),
            (4, "sceneBuffer"),
            (5, "rayBuffer"),
            (6, "output"),
            (7, "uniform"),
            (8, "requests"),
        ] {
            let needle = format!("{{ binding: {binding}, resource: {{ buffer: {resource} }} }}");
            let at = body.find(&needle).unwrap_or_else(|| panic!("missing {needle}"));
            assert!(at > last, "binding {binding} is out of order");
            last = at;
        }
        assert!(body.contains("dispatchWorkgroups(workgroups)"));
        assert!(body.contains("window.newvolimSceneWebGpu.shader"));
    }

    /// A server volume frame is the portable route frame: its PFM decodes to exactly the depth
    /// `scene_route_frame` produces for the same session and camera, and the route is named.
    #[test]
    #[ignore = "requires a local WGPU adapter"]
    fn server_volume_frame_is_the_portable_route_frame() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        let store = SessionStore::default();
        let session = store.session_for("cells3d", &root).unwrap();
        let size = FrameSize::new(48, 32).unwrap();
        let controls = CameraControls {
            orbit_delta: [3, -2],
            zoom: 1.2,
        }
        .validate()
        .unwrap();
        let (png, pfm, renderer) =
            render_volume_frame(Some(&session), &root, size, controls).unwrap();
        assert_eq!(renderer, "portable-demand");
        assert_eq!(png_dimensions(&png), [48, 32]);
        let pfm = pfm.expect("portable frames carry the paired depth");
        let header = b"Pf\n48 32\n-1.0\n";
        assert!(pfm.starts_with(header));
        let distances = pfm[header.len()..]
            .chunks_exact(4)
            .map(|word| f32::from_le_bytes([word[0], word[1], word[2], word[3]]))
            .collect::<Vec<_>>()
            .chunks_exact(48)
            .rev()
            .flat_map(|row| row.to_vec())
            .collect::<Vec<_>>();
        let frame = scene_route_frame(
            &session,
            NativePortableDrawRequest {
                origin_xyz: [0; 3],
                extent_xyz: [1; 3],
                width: 48,
                height: 32,
                orbit_x: 3,
                orbit_y: -2,
                zoom: 1.2,
            },
        )
        .unwrap();
        assert_eq!(distances, frame.attachments.first_opacity_distance);
        assert!(distances.iter().any(|distance| distance.is_finite()));
        // Without a session the same request is Palace's Vulkan frame, as before.
        let (_, pfm, renderer) = render_volume_frame(None, &root, size, controls).unwrap();
        assert_eq!(renderer, "vulkan");
        assert!(pfm.is_some());
    }

    #[test]
    fn fixture_volume_response_carries_the_paired_palace_depth_attachment() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        let size = FrameSize::new(32, 24).unwrap();
        let attachments =
            render_local_zarr_with_camera_attachments(root, size, CameraControls::default())
                .unwrap();
        // The header alone is not the contract. A surface that is entirely `+infinity` encodes
        // to a perfectly valid PFM, and that is exactly what every real dataset produced until
        // the raycaster recorded a NaN `t` on saturation; the header check passed throughout.
        let depth = attachments
            .ray_distance()
            .expect("Palace volume response must carry a paired first-opacity surface");
        let finite = depth
            .distances()
            .iter()
            .filter(|distance| distance.is_finite())
            .count();
        assert!(
            finite > 0,
            "the fixture paints volume, so its first-opacity surface must have finite distances"
        );
        assert!(depth
            .distances()
            .iter()
            .all(|d| (d.is_finite() && *d >= 0.0) || *d == f32::INFINITY));
        let (png, ray_distance_pfm) = palace_png::encode_attachments(&attachments).into_parts();
        let ray_distance_pfm = ray_distance_pfm.expect("Palace volume response must carry PFM");
        assert_eq!(png_dimensions(&png), [32, 24]);
        assert!(ray_distance_pfm.starts_with(b"Pf\n32 24\n-1.0\n"));

        let envelope = serde_json::to_value(SocketFrame {
            kind: "frame",
            request_id: 9,
            width: 32,
            height: 24,
            mime_type: "image/png",
            target: png_target(32, 24, true),
            progress: "final",
            render_ms: 0.0,
            data_base64: STANDARD.encode(png),
            ray_distance_pfm_base64: Some(STANDARD.encode(ray_distance_pfm)),
        })
        .unwrap();
        assert_eq!(envelope["target"]["depth"], "rayDistanceF32");
        assert!(envelope["rayDistancePfmBase64"]
            .as_str()
            .unwrap()
            .starts_with("UGY"));
    }

    #[test]
    fn frame_request_requires_a_named_dataset_and_defaults_camera_controls() {
        let legacy: FrameRequest =
            serde_json::from_str(r#"{"dataset":"cells3d","width":32,"height":24}"#).unwrap();
        assert_eq!(legacy.dataset, "cells3d");
        assert_eq!((legacy.orbit_x, legacy.orbit_y, legacy.zoom), (0, 0, 1.0));

        let controlled: FrameRequest = serde_json::from_str(
            r#"{"dataset":"cells3d","width":32,"height":24,"orbitX":11,"orbitY":-7,"zoom":1.5}"#,
        )
        .unwrap();
        assert_eq!(
            (controlled.orbit_x, controlled.orbit_y, controlled.zoom),
            (11, -7, 1.5)
        );
        assert_eq!(controlled.request_id, 0);

        let socket_request: FrameRequest =
            serde_json::from_str(r#"{"dataset":"cells3d","width":32,"height":24,"requestId":73}"#)
                .unwrap();
        assert_eq!(socket_request.request_id, 73);
        let orthogonal: FrameRequest = serde_json::from_str(
            r#"{"dataset":"cells3d","width":32,"height":24,"view":"orthogonal","x":5,"y":6,"z":7}"#,
        )
        .unwrap();
        assert_eq!(orthogonal.view, RenderView::Orthogonal);
        assert_eq!(
            orthogonal.x.zip(orthogonal.y).zip(orthogonal.z),
            Some(((5, 6), 7))
        );
        assert!(serde_json::from_str::<FrameRequest>(
            r#"{"datasetPath":"/tmp/a.ome.zarr","width":32,"height":24}"#
        )
        .is_err());
    }

    #[test]
    fn frame_controls_are_rejected_before_renderer_admission() {
        assert!(CameraControls {
            orbit_delta: [10_000, -10_000],
            zoom: 0.25,
        }
        .validate()
        .is_ok());
        assert!(CameraControls {
            orbit_delta: [10_001, 0],
            zoom: 1.0,
        }
        .validate()
        .is_err());
        assert!(CameraControls {
            orbit_delta: [0, 0],
            zoom: f32::NAN,
        }
        .validate()
        .is_err());
        assert!(CameraControls {
            orbit_delta: [0, 0],
            zoom: 4.01,
        }
        .validate()
        .is_err());
    }

    #[test]
    fn frame_crosshair_is_all_or_nothing_and_orthogonal_only() {
        let request = |view, x, y, z| FrameRequest {
            dataset: "cells3d".to_owned(),
            width: 32,
            height: 24,
            orbit_x: 0,
            orbit_y: 0,
            zoom: 1.0,
            request_id: 0,
            view,
            x,
            y,
            z,
        };
        assert_eq!(
            request_crosshair(&request(RenderView::Orthogonal, Some(5), Some(6), Some(7))),
            Ok(Some([5, 6, 7]))
        );
        assert_eq!(
            request_crosshair(&request(RenderView::Orthogonal, None, None, None)),
            Ok(None)
        );
        assert!(
            request_crosshair(&request(RenderView::Orthogonal, Some(5), None, Some(7))).is_err()
        );
        assert!(
            request_crosshair(&request(RenderView::Volume, Some(5), Some(6), Some(7))).is_err()
        );
    }

    #[tokio::test]
    async fn newest_pending_request_replaces_only_its_own_session() {
        let (first_sender, first_receiver) = oneshot::channel();
        let (same_session_sender, _same_session_receiver) = oneshot::channel();
        let (other_session_sender, mut other_session_receiver) = oneshot::channel();
        let mut pending = PendingJobs::default();
        admit_pending(&mut pending, test_job(3, first_sender));
        admit_pending(&mut pending, test_job(4, other_session_sender));

        admit_pending(&mut pending, test_job(3, same_session_sender));

        assert!(matches!(
            first_receiver.await.unwrap(),
            Err(message) if message == SUPERSEDED_MESSAGE
        ));
        assert!(other_session_receiver.try_recv().is_err());
        assert_eq!(pending.order, VecDeque::from([SessionId(3), SessionId(4)]));
        assert_eq!(pending.take_next().unwrap().session_id, SessionId(3));
        assert_eq!(pending.take_next().unwrap().session_id, SessionId(4));
    }

    #[tokio::test]
    async fn pending_session_bound_rejects_a_new_session_without_evicting_another() {
        let mut pending = PendingJobs::default();
        for session_id in 0..MAX_PENDING_SESSIONS as u64 {
            let (sender, _receiver) = oneshot::channel();
            admit_pending(&mut pending, test_job(session_id, sender));
        }
        let (sender, receiver) = oneshot::channel();
        admit_pending(&mut pending, test_job(99, sender));

        assert!(matches!(
            receiver.await.unwrap(),
            Err(message) if message == PENDING_SESSION_LIMIT_MESSAGE
        ));
        assert_eq!(pending.by_session.len(), MAX_PENDING_SESSIONS);
        assert!(!pending.by_session.contains_key(&SessionId(99)));
    }

    #[test]
    fn socket_frame_envelope_has_an_explicit_final_png_contract() {
        let json = serde_json::to_value(SocketFrame {
            kind: "frame",
            request_id: 7,
            width: 2,
            height: 1,
            mime_type: "image/png",
            target: png_target(2, 1, false),
            progress: "final",
            render_ms: 12.5,
            data_base64: STANDARD.encode([137, 80, 78, 71]),
            ray_distance_pfm_base64: None,
        })
        .unwrap();
        assert_eq!(json["type"], "frame");
        assert_eq!(json["requestId"], 7);
        assert_eq!(json["width"], 2);
        assert_eq!(json["height"], 1);
        assert_eq!(json["mimeType"], "image/png");
        assert_eq!(
            json["target"]["extent"],
            serde_json::json!({ "width": 2, "height": 1 })
        );
        assert_eq!(json["target"]["colorFormat"], "rgba8Unorm");
        assert_eq!(json["target"]["colorEncoding"], "srgb");
        assert_eq!(json["target"]["depth"], "none");
        assert_eq!(json["progress"], "final");
        assert_eq!(json["renderMs"], 12.5);
        assert_eq!(json["dataBase64"], "iVBORw==");
        assert!(json.get("rayDistancePfmBase64").is_none());

        let with_depth = serde_json::to_value(SocketFrame {
            kind: "frame",
            request_id: 8,
            width: 2,
            height: 1,
            mime_type: "image/png",
            target: png_target(2, 1, true),
            progress: "final",
            render_ms: 1.0,
            data_base64: "colour".into(),
            ray_distance_pfm_base64: Some(STANDARD.encode(b"Pf\n2 1\n-1.0\n")),
        })
        .unwrap();
        assert_eq!(with_depth["target"]["depth"], "rayDistanceF32");
        assert_eq!(with_depth["rayDistancePfmBase64"], "UGYKMiAxCi0xLjAK");
    }

    #[test]
    fn socket_orthogonal_envelope_carries_three_final_pngs() {
        let json = serde_json::to_value(SocketOrthogonal {
            kind: "orthogonal",
            request_id: 8,
            width: 2,
            height: 1,
            mime_type: "image/png",
            target: png_target(2, 1, false),
            progress: "final",
            render_ms: 1.5,
            xy_base64: "eHk=".into(),
            xz_base64: "eHo=".into(),
            yz_base64: "eXo=".into(),
            voxel_shape_xyz: [128, 128, 32],
            crosshair_xyz: [64, 64, 16],
        })
        .unwrap();
        assert_eq!(json["type"], "orthogonal");
        assert_eq!(json["xyBase64"], "eHk=");
        assert_eq!(json["voxelShapeXyz"], serde_json::json!([128, 128, 32]));
        assert_eq!(json["crosshairXyz"], serde_json::json!([64, 64, 16]));
        assert_eq!(json["target"]["depth"], "none");
        assert_eq!(json["progress"], "final");
    }

    #[test]
    fn browser_dataset_names_are_stable_route_segments() {
        assert!(is_dataset_name("cells3d"));
        assert!(is_dataset_name("study_2026-09"));
        assert!(!is_dataset_name(""));
        assert!(!is_dataset_name("../cells"));
        assert!(!is_dataset_name("cells/other"));
        assert!(!is_dataset_name(&"a".repeat(65)));
    }

    #[test]
    fn frame_dataset_resolution_never_accepts_a_client_path() {
        let datasets = HashMap::from([(
            "cells3d".to_owned(),
            PathBuf::from("/private/cells3d.ome.zarr"),
        )]);
        assert_eq!(
            resolve_frame_dataset(&datasets, "cells3d").unwrap(),
            PathBuf::from("/private/cells3d.ome.zarr")
        );
        assert_eq!(
            resolve_frame_dataset(&datasets, "missing").unwrap_err().0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            resolve_frame_dataset(&datasets, "../cells3d")
                .unwrap_err()
                .0,
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn zarr_asset_paths_reject_non_normal_components_before_file_access() {
        let root = Path::new("/tmp/newvolim-zarr-test-root");
        assert!(zarr_asset_path(root, "../secret").is_err());
        assert!(zarr_asset_path(root, "/etc/passwd").is_err());
        assert!(zarr_asset_path(root, "").is_err());
        assert!(zarr_asset_path(root, "c/0/0/0").is_err());
    }

    #[test]
    fn browser_dataset_list_is_name_only_and_deterministic() {
        let mut datasets = HashMap::new();
        datasets.insert("zeta".to_owned(), PathBuf::from("/private/zeta.ome.zarr"));
        datasets.insert("alpha".to_owned(), PathBuf::from("/private/alpha.ome.zarr"));
        let mut names: Vec<_> = datasets.keys().cloned().collect();
        names.sort_unstable();
        let json = serde_json::to_value(DatasetList { datasets: names }).unwrap();
        assert_eq!(json, serde_json::json!({"datasets": ["alpha", "zeta"]}));
        assert!(!json.to_string().contains("private"));
    }

    #[tokio::test]
    async fn cors_allows_only_configured_origins_on_discovery_and_chunk_routes() {
        let (render_queue, _receiver) = mpsc::channel(1);
        let state = AppState {
            datasets: Arc::new(HashMap::from([(
                "cells3d".to_owned(),
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../test-data/cells3d-anisotropic.ome.zarr")
                    .canonicalize()
                    .unwrap(),
            )])),
            render_queue,
            next_session_id: Arc::new(AtomicU64::new(1)),
            sessions: SessionStore::default(),
        };
        let app = app_router(
            state,
            vec![HeaderValue::from_static("https://viewer.example")],
            None,
        );
        let allowed = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/datasets")
                    .header(header::ORIGIN, "https://viewer.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(allowed.status(), StatusCode::OK);
        assert_eq!(
            allowed.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&HeaderValue::from_static("https://viewer.example"))
        );

        let chunk = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/datasets/cells3d/zarr/zarr.json")
                    .header(header::ORIGIN, "https://viewer.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(chunk.status(), StatusCode::OK);
        assert_eq!(
            chunk.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&HeaderValue::from_static("https://viewer.example"))
        );
        assert_eq!(
            chunk.headers().get(header::CONTENT_TYPE),
            Some(&HeaderValue::from_static("application/json"))
        );

        let denied = app
            .oneshot(
                Request::builder()
                    .uri("/v1/datasets")
                    .header(header::ORIGIN, "https://untrusted.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::OK);
        assert!(denied
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none());
    }

    #[tokio::test]
    async fn loopback_routes_expose_only_named_dataset_metadata_and_normal_assets() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr")
            .canonicalize()
            .unwrap();
        let (render_queue, _receiver) = mpsc::channel(1);
        let app = app_router(
            AppState {
                datasets: Arc::new(HashMap::from([("cells3d".to_owned(), root)])),
                render_queue,
                next_session_id: Arc::new(AtomicU64::new(1)),
                sessions: SessionStore::default(),
            },
            vec![],
            None,
        );

        let discovery = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/datasets")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(discovery.status(), StatusCode::OK);
        let discovery = to_bytes(discovery.into_body(), 4 * 1024).await.unwrap();
        assert_eq!(discovery.as_ref(), br#"{"datasets":["cells3d"]}"#);

        let metadata = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/datasets/cells3d/zarr/zarr.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(metadata.status(), StatusCode::OK);
        assert_eq!(
            metadata.headers().get(header::CONTENT_TYPE),
            Some(&HeaderValue::from_static("application/json"))
        );
        let metadata = to_bytes(metadata.into_body(), MAX_ZARR_ASSET_BYTES as usize)
            .await
            .unwrap();
        assert!(std::str::from_utf8(&metadata)
            .unwrap()
            .contains("multiscales"));

        for route in [
            "/v1/datasets/missing/zarr/zarr.json",
            "/v1/datasets/cells3d/zarr/../Cargo.toml",
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(route).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_ne!(
                response.status(),
                StatusCode::OK,
                "unsafe route {route} was served"
            );
        }
    }

    /// With `--page-dir` the built page is served from the API's own origin: `/` is the
    /// directory's `index.html`, its assets resolve beside it, API routes still win, unknown
    /// paths are 404, and without the option `/` is 404 as before.
    #[tokio::test]
    async fn page_dir_serves_the_built_page_beside_the_api() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr")
            .canonicalize()
            .unwrap();
        let page = tempfile::tempdir().unwrap();
        std::fs::write(page.path().join("index.html"), "<title>newvolim page</title>").unwrap();
        std::fs::write(page.path().join("app.js"), "window.newvolim = 1;").unwrap();
        let state = || {
            let (render_queue, _receiver) = mpsc::channel(1);
            AppState {
                datasets: Arc::new(HashMap::from([("cells3d".to_owned(), root.clone())])),
                render_queue,
                next_session_id: Arc::new(AtomicU64::new(1)),
                sessions: SessionStore::default(),
            }
        };
        let get = |app: Router, uri: &str| {
            let request = Request::builder().uri(uri).body(Body::empty()).unwrap();
            async move {
                let response = app.oneshot(request).await.unwrap();
                let status = response.status();
                let content_type = response
                    .headers()
                    .get(header::CONTENT_TYPE)
                    .map(|value| value.to_str().unwrap().to_owned());
                let body = axum::body::to_bytes(response.into_body(), 1 << 20)
                    .await
                    .unwrap();
                (status, content_type, String::from_utf8_lossy(&body).into_owned())
            }
        };

        let app = app_router(state(), vec![], Some(page.path().to_path_buf()));
        let (status, content_type, body) = get(app.clone(), "/").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type.as_deref(), Some("text/html"));
        assert_eq!(body, "<title>newvolim page</title>");
        let (status, content_type, body) = get(app.clone(), "/app.js").await;
        assert_eq!(status, StatusCode::OK);
        assert!(content_type.as_deref().unwrap().starts_with("text/javascript"), "{content_type:?}");
        assert_eq!(body, "window.newvolim = 1;");
        let (status, _, body) = get(app.clone(), "/v1/datasets").await;
        assert_eq!(status, StatusCode::OK, "API routes take precedence over files");
        assert!(body.contains("cells3d"));
        let (status, _, _) = get(app.clone(), "/missing.js").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _, _) = get(app, "/../Cargo.toml").await;
        assert_ne!(status, StatusCode::OK, "no path escapes the page directory");

        let bare = app_router(state(), vec![], None);
        let (status, _, _) = get(bare, "/").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "no page without --page-dir");
    }

    /// The settings route reads and writes the session's depth scale, refuses a scale outside
    /// the session's bounds without changing it, and the scale then reaches the browser plan.
    #[tokio::test]
    async fn settings_route_sets_the_depth_scale_the_plan_renders_with() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr")
            .canonicalize()
            .unwrap();
        let (render_queue, _receiver) = mpsc::channel(1);
        let state = AppState {
            datasets: Arc::new(HashMap::from([("cells3d".to_owned(), root)])),
            render_queue,
            next_session_id: Arc::new(AtomicU64::new(1)),
            sessions: SessionStore::default(),
        };
        let app = app_router(state, vec![], None);
        let call = |app: Router, method: &str, uri: &str, body: Option<String>| {
            let mut request = Request::builder().method(method).uri(uri);
            if body.is_some() {
                request = request.header(header::CONTENT_TYPE, "application/json");
            }
            let request = request.body(body.map(Body::from).unwrap_or_else(Body::empty)).unwrap();
            async move {
                let response = app.oneshot(request).await.unwrap();
                let status = response.status();
                let bytes = axum::body::to_bytes(response.into_body(), 1 << 24).await.unwrap();
                (status, String::from_utf8_lossy(&bytes).into_owned())
            }
        };
        let (status, body) = call(app.clone(), "GET", "/v1/datasets/cells3d/settings", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap(), serde_json::json!({"depthScale": 1.0}));
        let plan_at_one: serde_json::Value = serde_json::from_str(&call(app.clone(), "GET", "/v1/datasets/cells3d/portable/plan?width=32&height=24", None).await.1).unwrap();
        let (status, body) = call(app.clone(), "POST", "/v1/datasets/cells3d/settings", Some(r#"{"depthScale": 2.5}"#.into())).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap(), serde_json::json!({"depthScale": 2.5}));
        let (status, body) = call(app.clone(), "POST", "/v1/datasets/cells3d/settings", Some(r#"{"depthScale": 0}"#.into())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        let (_, body) = call(app.clone(), "GET", "/v1/datasets/cells3d/settings", None).await;
        assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap()["depthScale"], 2.5, "a refused write leaves the old scale");
        let plan_at_two_and_a_half: serde_json::Value = serde_json::from_str(&call(app, "GET", "/v1/datasets/cells3d/portable/plan?width=32&height=24", None).await.1).unwrap();
        let ratio = plan_at_two_and_a_half["opacityReference"].as_f64().unwrap() / plan_at_one["opacityReference"].as_f64().unwrap();
        assert!((ratio - 2.5).abs() < 1e-4, "the plan's opacity reference scales: {ratio}");
    }

    #[tokio::test]
    async fn named_local_frame_route_renders_a_real_palace_png_without_a_tcp_listener() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr")
            .canonicalize()
            .unwrap();
        let (render_queue, queue_receiver) = mpsc::channel(1);
        tokio::spawn(render_dispatcher(queue_receiver));
        let state = AppState {
            datasets: Arc::new(HashMap::from([("cells3d".to_owned(), root)])),
            render_queue,
            next_session_id: Arc::new(AtomicU64::new(1)),
            sessions: SessionStore::default(),
        };
        let app = app_router(state.clone(), vec![], None);

        let invalid_controls = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/frame")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"dataset":"cells3d","width":32,"height":24,"zoom":4.01}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid_controls.status(), StatusCode::BAD_REQUEST);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/frame")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"dataset":"cells3d","width":32,"height":24,"orbitX":24,"orbitY":-12,"zoom":1.1}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE),
            Some(&HeaderValue::from_static("image/png"))
        );
        let timing = response
            .headers()
            .get(header::HeaderName::from_static("server-timing"))
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            timing
                .strip_prefix("render;dur=")
                .and_then(|value| value.split(',').next())
                .and_then(|value| value.trim().parse::<f64>().ok())
                .is_some_and(|value| value.is_finite() && value >= 0.0),
            "frame route returned an invalid Server-Timing value: {timing:?}"
        );
        // The second metric names the route; on a host with a WGPU adapter that is the portable
        // demand route, otherwise Palace's Vulkan raycaster.
        assert!(
            ["portable-demand", "portable-palace", "portable-native", "vulkan"]
                .iter()
                .any(|route| timing.contains(&format!("route;desc=\"{route}\""))),
            "frame route did not name its renderer: {timing:?}"
        );
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        assert!(body.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!(png_dimensions(&body), [32, 24]);

        let default_view = enqueue_render(
            &state,
            SessionId(1),
            FrameRequest {
                dataset: "cells3d".to_owned(),
                width: 32,
                height: 24,
                orbit_x: 0,
                orbit_y: 0,
                zoom: 1.0,
                request_id: 1,
                view: RenderView::Volume,
                x: None,
                y: None,
                z: None,
            },
        )
        .await
        .unwrap();
        let RenderOutput::Volume(default_view) = default_view else {
            panic!("default volume request returned orthogonal panes")
        };
        assert!(default_view.png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!(png_dimensions(&default_view.png), [32, 24]);
        assert_ne!(
            default_view.png,
            body.as_ref(),
            "non-default orbit/zoom must alter the rendered Palace frame"
        );

        let orthogonal = enqueue_render(
            &state,
            SessionId(2),
            FrameRequest {
                dataset: "cells3d".to_owned(),
                width: 32,
                height: 24,
                orbit_x: 0,
                orbit_y: 0,
                zoom: 1.0,
                request_id: 2,
                view: RenderView::Orthogonal,
                // Deliberately outside the fixture extent: the shared renderer boundary must
                // clamp all three linked panes before it reaches Palace.
                x: Some(u32::MAX),
                y: Some(u32::MAX),
                z: Some(u32::MAX),
            },
        )
        .await
        .unwrap();
        let RenderOutput::Orthogonal(orthogonal) = orthogonal else {
            panic!("orthogonal request returned a volume frame")
        };
        assert_eq!(orthogonal.voxel_shape_xyz, [128, 128, 32]);
        assert_eq!(orthogonal.crosshair_xyz, [127, 127, 31]);
        assert!(orthogonal.render_ms.is_finite() && orthogonal.render_ms >= 0.0);
        assert!(orthogonal
            .png
            .iter()
            .all(|png| png.starts_with(b"\x89PNG\r\n\x1a\n")));
        // The portable session slices the volume at the finest level that fits its pages —
        // level zero for this fixture — so each plane is at voxel resolution, not the
        // requested frame size: XY is x by y, XZ is x by z, YZ is y by z. The page stretches
        // whatever it gets, so the crosshair overlay stays a fraction of the pane.
        assert_eq!(png_dimensions(&orthogonal.png[0]), [128, 128], "XY");
        assert_eq!(png_dimensions(&orthogonal.png[1]), [128, 32], "XZ");
        assert_eq!(png_dimensions(&orthogonal.png[2]), [128, 32], "YZ");
    }
}
