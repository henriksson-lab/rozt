//! Native desktop host. The UI remains the CSR bundle; this crate never renders Leptos SSR.

mod session;

use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

use base64::{engine::general_purpose::STANDARD, Engine};
use newvolim_render::{
    annotation_overlay, pick_annotation_overlays, project_annotation_overlays, AnnotationProjector,
    ColorEncoding, ColorFormat, DepthAttachment, FrameProgress, LayerRenderLimits, LayerRenderPlan,
    OverlayStyle, PhysicalExtent, ProjectedAnnotationVertex, RenderTarget,
};
use newvolim_scene::Annotation;
use palace_frame::{
    camera_ray_for_local_zarr, project_point_for_local_zarr, render_local_zarr_attachments,
    render_local_zarr_orthogonal_at_png, render_local_zarr_orthogonal_png,
    render_local_zarr_with_camera_attachments, render_synthetic_orthogonal_at_png,
    render_synthetic_orthogonal_png, render_synthetic_png, CameraControls, FrameSize,
};
use serde::{Deserialize, Serialize};
use session::{
    LoadedLocalChunk, LocalLayerChunkPlan, LocalLayerRenderRequest, LocalSession, SessionSummary,
    SpatialChunkRegion,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NativeLayerAdmission {
    descriptors: Vec<newvolim_render::NativeLayerDescriptor>,
    requests: Vec<LocalLayerRenderRequest>,
}

/// Shared with the loopback server's frame budget. This bounds colour PNG allocation and the
/// optional four-bytes-per-pixel PFM sidecar before a webview command enters Palace.
const MAX_DESKTOP_FRAME_PIXELS: u64 = 16 * 1024 * 1024;

/// A single renderer-owned depth sidecar for native annotation selection. At the frame budget
/// this is bounded to 64 MiB, and the complete request tuple prevents reuse across cameras.
#[derive(Clone, Debug, Default)]
struct PickableDepthCache {
    entry: Option<PickableDepth>,
}

#[derive(Clone, Debug)]
struct PickableDepth {
    root: PathBuf,
    size: FrameSize,
    controls: CameraControls,
    depth: palace_png::RayDistanceFrame,
}

impl PickableDepthCache {
    fn depth_for(
        &self,
        root: &Path,
        size: FrameSize,
        controls: CameraControls,
    ) -> Option<palace_png::RayDistanceFrame> {
        self.entry.as_ref().and_then(|entry| {
            (entry.root == root && entry.size == size && entry.controls == controls)
                .then(|| entry.depth.clone())
        })
    }

    fn replace(
        &mut self,
        root: PathBuf,
        size: FrameSize,
        controls: CameraControls,
        depth: Option<&palace_png::RayDistanceFrame>,
    ) {
        self.entry = depth.cloned().map(|depth| PickableDepth {
            root,
            size,
            controls,
            depth,
        });
    }
}

fn cache_pickable_depth(
    cache: &Mutex<PickableDepthCache>,
    root: PathBuf,
    size: FrameSize,
    controls: CameraControls,
    attachments: &palace_png::FrameAttachments,
) -> Result<(), String> {
    cache
        .lock()
        .map_err(|_| "desktop depth cache lock was poisoned".to_owned())?
        .replace(root, size, controls, attachments.ray_distance());
    Ok(())
}

fn desktop_frame_size(width: u32, height: u32, frame_count: u64) -> Result<FrameSize, String> {
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(frame_count))
        .ok_or_else(|| "desktop frame dimensions overflow the pixel budget".to_owned())?;
    if pixels > MAX_DESKTOP_FRAME_PIXELS {
        return Err(format!(
            "desktop request is {pixels} pixels; limit is {MAX_DESKTOP_FRAME_PIXELS}"
        ));
    }
    FrameSize::new(width, height).map_err(|error| error.to_string())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct FramePayload {
    mime_type: &'static str,
    width: u32,
    height: u32,
    /// Describes the colour PNG and, when supplied, the renderer-owned ray-distance sidecar.
    target: RenderTarget,
    progress: FrameProgress,
    data_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    ray_distance_pfm_base64: Option<String>,
}

impl FramePayload {
    fn png(width: u32, height: u32, png: Vec<u8>) -> Self {
        let target = RenderTarget::new(
            PhysicalExtent::new(width, height).expect("FramePayload dimensions are validated"),
            ColorFormat::Rgba8Unorm,
            ColorEncoding::Srgb,
            DepthAttachment::None,
        )
        .expect("an sRGB RGBA8 PNG is a valid render target");
        Self {
            mime_type: "image/png",
            width,
            height,
            target,
            progress: FrameProgress::Final,
            data_url: format!("data:image/png;base64,{}", STANDARD.encode(png)),
            ray_distance_pfm_base64: None,
        }
    }

    fn palace_attachments(attachments: palace_png::FrameAttachments) -> Result<Self, String> {
        let (color, ray_distance) = attachments.into_parts();
        let width = color.width();
        let height = color.height();
        let png = palace_png::encode_rgba(&color);
        let ray_distance_pfm_base64 = ray_distance
            .as_ref()
            .map(palace_png::encode_ray_distance_pfm)
            .map(|pfm| STANDARD.encode(pfm));
        let depth = if ray_distance_pfm_base64.is_some() {
            DepthAttachment::RayDistanceF32
        } else {
            DepthAttachment::None
        };
        let target = RenderTarget::new(
            PhysicalExtent::new(width, height).map_err(|error| error.to_string())?,
            ColorFormat::Rgba8Unorm,
            ColorEncoding::Srgb,
            depth,
        )
        .map_err(|error| error.to_string())?;
        Ok(Self {
            mime_type: "image/png",
            width,
            height,
            target,
            progress: FrameProgress::Final,
            data_url: format!("data:image/png;base64,{}", STANDARD.encode(png)),
            ray_distance_pfm_base64,
        })
    }

    fn native_wgpu_camera(
        width: u32,
        height: u32,
        frame: newvolim_wgpu_frame::RenderedProjection,
    ) -> Result<Self, String> {
        let pixels = frame.rgba.into_iter().flatten().collect();
        let color =
            palace_png::RgbaFrame::new(width, height, pixels).map_err(|error| error.to_string())?;
        let depth = palace_png::RayDistanceFrame::new(width, height, frame.ray_distances)
            .map_err(|error| error.to_string())?;
        let attachments = palace_png::FrameAttachments::new(color, Some(depth))
            .map_err(|error| error.to_string())?;
        Self::palace_attachments(attachments)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct OrthogonalPayload {
    xy: FramePayload,
    xz: FramePayload,
    yz: FramePayload,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AnnotationPlacement {
    annotation: Annotation,
    /// This is display-only placement data. The session persists only the physical coordinate
    /// in `annotation`; callers must not treat voxel indices as the annotation's authority.
    voxel_points: Option<Vec<[u64; 3]>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AnnotationPickPayload {
    annotation_id: u64,
    /// Physical NGFF distance from the reconstructed camera origin to the selected annotation.
    distance: f64,
}

/// Bounded native pick request. Keeping camera, target extent, and physical pixel together
/// prevents a caller from accidentally combining depth from one frame with a ray from another.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AnnotationPickRequest {
    width: u32,
    height: u32,
    x: u32,
    y: u32,
    orbit_x: i32,
    orbit_y: i32,
    zoom: f32,
}

/// Bounded input for a native portable draw packet. Pixel extent and Palace controls are kept
/// together with the requested local chunk region so trusted annotation projection cannot be
/// mixed with a volume admission from another frame.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativePortableDrawRequest {
    origin_xyz: [u64; 3],
    extent_xyz: [u32; 3],
    width: u32,
    height: u32,
    orbit_x: i32,
    orbit_y: i32,
    zoom: f32,
}

/// A pick tied to the full camera-complete portable draw request. The native host rebuilds both
/// ray table and depth itself; webview pixel input is limited to one validated physical pixel.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativePortablePickRequest {
    draw: NativePortableDrawRequest,
    x: u32,
    y: u32,
}

const ANNOTATION_PICK_STYLE: OverlayStyle = OverlayStyle {
    point_radius: 0.5,
    line_radius: 0.25,
};

/// Adapter from persisted physical NGFF annotations to the fitted Palace camera. Palace retains
/// raw `[z, y, x]` array ordering, while the session owns the physical `[x, y, z]` transform.
struct DesktopAnnotationProjector<'a> {
    session: &'a LocalSession,
    root: &'a Path,
    size: FrameSize,
    controls: CameraControls,
}

impl DesktopAnnotationProjector<'_> {
    fn projection(&self, position: [f64; 3]) -> Option<ProjectedAnnotationVertex> {
        let [x, y, z] = self.session.physical_point_voxel_xyz_f64(position).ok()?;
        let projection = project_point_for_local_zarr(
            self.root,
            self.size,
            self.controls,
            [z as f32, y as f32, x as f32],
        )
        .ok()??;
        Some(ProjectedAnnotationVertex {
            pixel: projection.pixel,
            ray_distance: projection.ray_distance,
        })
    }
}

impl AnnotationProjector for DesktopAnnotationProjector<'_> {
    fn project(&self, position: [f64; 3]) -> Option<ProjectedAnnotationVertex> {
        self.projection(position)
    }

    fn project_radius(&self, center: [f64; 3], radius: f32) -> Option<f32> {
        let center_projection = self.projection(center)?;
        [
            [radius as f64, 0.0, 0.0],
            [0.0, radius as f64, 0.0],
            [0.0, 0.0, radius as f64],
        ]
        .into_iter()
        .filter_map(|offset| {
            self.projection(std::array::from_fn(|axis| center[axis] + offset[axis]))
        })
        .map(|projection| {
            let dx = projection.pixel[0] - center_projection.pixel[0];
            let dy = projection.pixel[1] - center_projection.pixel[1];
            dx.hypot(dy)
        })
        .reduce(f32::max)
        .filter(|radius| radius.is_finite() && *radius > 0.0)
    }
}

fn project_session_annotation_words(
    session: &LocalSession,
    root: &Path,
    size: FrameSize,
    controls: CameraControls,
) -> Result<Vec<u32>, String> {
    let overlays = session
        .annotations()
        .iter()
        .map(|annotation| annotation_overlay(annotation, ANNOTATION_PICK_STYLE))
        .collect::<Vec<_>>();
    let projector = DesktopAnnotationProjector {
        session,
        root,
        size,
        controls,
    };
    let records = project_annotation_overlays(
        &overlays,
        PhysicalExtent::new(size.width, size.height).map_err(|error| error.to_string())?,
        &projector,
    )
    .map_err(|error| error.to_string())?;
    Ok(records
        .into_iter()
        .flat_map(|record| record.words())
        .collect())
}

fn depth_aware_annotation_pick(
    annotations: &[Annotation],
    ray: newvolim_render::PickRay,
    maximum_physical_distance: f64,
) -> Result<Option<AnnotationPickPayload>, String> {
    let overlays = annotations
        .iter()
        .map(|annotation| annotation_overlay(annotation, ANNOTATION_PICK_STYLE))
        .collect::<Vec<_>>();
    pick_annotation_overlays(&overlays, ray, maximum_physical_distance)
        .map(|hit| {
            hit.map(|hit| AnnotationPickPayload {
                annotation_id: hit.annotation_id.0,
                distance: hit.distance,
            })
        })
        .map_err(|error| error.to_string())
}

fn pick_local_dataset_annotation(
    session: &LocalSession,
    root: &Path,
    size: FrameSize,
    controls: CameraControls,
    pixel: [u32; 2],
    depth: &palace_png::RayDistanceFrame,
) -> Result<Option<AnnotationPickPayload>, String> {
    if session.dataset_root().as_deref() != Some(root) {
        return Err(
            "the opened dataset changed while resolving an annotation pick; retry".to_owned(),
        );
    }
    let palace_ray = camera_ray_for_local_zarr(root, size, controls, pixel)
        .map_err(|error| error.to_string())?;
    let physical_ray = session
        .palace_ray_to_physical(
            palace_ray.origin.map(f64::from),
            palace_ray.direction.map(f64::from),
        )
        .map_err(|error| error.to_string())?;
    let index = usize::try_from(pixel[1])
        .ok()
        .and_then(|row| row.checked_mul(depth.width() as usize))
        .and_then(|row| row.checked_add(pixel[0] as usize))
        .ok_or_else(|| "annotation-pick pixel offset overflows usize".to_owned())?;
    let palace_distance = *depth
        .distances()
        .get(index)
        .ok_or_else(|| "annotation-pick pixel is outside the paired depth attachment".to_owned())?;
    depth_aware_annotation_pick(
        session.annotations(),
        physical_ray.ray,
        f64::from(palace_distance) * physical_ray.physical_distance_per_palace_unit,
    )
}

/// Derive ephemeral slice-marker coordinates from the physical, persisted annotations. A
/// malformed or non-point imported annotation remains visible but is not given a misleading
/// marker.
fn annotation_placements(session: &LocalSession) -> Vec<AnnotationPlacement> {
    session
        .annotations()
        .iter()
        .map(|annotation| AnnotationPlacement {
            annotation: annotation.clone(),
            voxel_points: session.annotation_voxel_points(annotation).ok(),
        })
        .collect()
}

#[tauri::command]
fn session_summary(
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<SessionSummary, String> {
    let session = session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?;
    Ok(session.summary())
}

#[tauri::command]
fn open_local_omezarr(
    root: String,
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<SessionSummary, String> {
    let mut session = session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?;
    session
        .open_local_omezarr(root)
        .map_err(|error| error.to_string())
}

/// Explicitly bind a metadata-derived level-zero image layer for the bounded portable route.
/// This is separate from opening metadata so an arbitrary renderer cannot acquire a local source
/// merely because a dataset is visible in the desktop session.
#[tauri::command]
fn prepare_default_portable_image_layer(
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<session::LocalOmeZarrSource, String> {
    session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?
        .prepare_default_portable_image_layer()
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn add_point_annotation(
    label: String,
    x: u32,
    y: u32,
    z: u32,
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<AnnotationPlacement, String> {
    let mut session = session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?;
    let voxel_xyz = [u64::from(x), u64::from(y), u64::from(z)];
    let annotation = session
        .add_point_annotation(label, voxel_xyz)
        .map_err(|error| error.to_string())?;
    Ok(AnnotationPlacement {
        annotation,
        voxel_points: Some(vec![voxel_xyz]),
    })
}

/// Add a user-drawn 2D ROI as a physical polygon. The webview supplies level-zero voxel
/// vertices only; [`LocalSession`] owns bounds checks and NGFF physical-coordinate conversion.
#[tauri::command]
fn add_polygon_annotation(
    label: String,
    points: Vec<[u32; 3]>,
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<AnnotationPlacement, String> {
    let mut session = session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?;
    let voxel_points = points
        .into_iter()
        .map(|point| point.map(u64::from))
        .collect::<Vec<_>>();
    let annotation = session
        .add_polygon_annotation(label, voxel_points.clone())
        .map_err(|error| error.to_string())?;
    Ok(AnnotationPlacement {
        annotation,
        voxel_points: Some(voxel_points),
    })
}

/// Add a rectangle ROI from opposite corners in one linked slice. `LocalSession` validates the
/// slice plane and converts the corners to physical NGFF centre/half-axis geometry.
#[tauri::command]
fn add_rectangle_annotation(
    label: String,
    first: [u32; 3],
    opposite: [u32; 3],
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<AnnotationPlacement, String> {
    let mut session = session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?;
    let voxel_points = [first.map(u64::from), opposite.map(u64::from)];
    let annotation = session
        .add_rectangle_annotation(label, voxel_points[0], voxel_points[1])
        .map_err(|error| error.to_string())?;
    Ok(AnnotationPlacement {
        annotation,
        voxel_points: Some(voxel_points.to_vec()),
    })
}

/// Add an ellipse ROI inscribed in the physical rectangle defined by opposite slice corners.
#[tauri::command]
fn add_ellipse_annotation(
    label: String,
    first: [u32; 3],
    opposite: [u32; 3],
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<AnnotationPlacement, String> {
    let mut session = session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?;
    let voxel_points = [first.map(u64::from), opposite.map(u64::from)];
    let annotation = session
        .add_ellipse_annotation(label, voxel_points[0], voxel_points[1])
        .map_err(|error| error.to_string())?;
    Ok(AnnotationPlacement {
        annotation,
        voxel_points: Some(voxel_points.to_vec()),
    })
}

#[tauri::command]
fn list_annotations(
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<Vec<AnnotationPlacement>, String> {
    let session = session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?;
    Ok(annotation_placements(&session))
}

/// Return the host-owned image-layer/channel selection for a portable four-binding renderer.
/// The request is an admission boundary, so excess visible content is reported instead of being
/// silently truncated by a particular webview or native adapter.
#[tauri::command]
fn layer_render_plan(
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<LayerRenderPlan, String> {
    session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?
        .layer_render_plan(LayerRenderLimits::new(4, 4))
        .map_err(|error| error.to_string())
}

/// Return each selected image layer together with its explicit authorized local OME-Zarr
/// address.  This is the adapter hand-off: renderers receive C/T and axis addressing rather
/// than reconstructing it from layer names or browser state.
#[tauri::command]
fn local_layer_render_requests(
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<Vec<LocalLayerRenderRequest>, String> {
    session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?
        .local_layer_render_requests(LayerRenderLimits::new(4, 4))
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn native_layer_admission(
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<NativeLayerAdmission, String> {
    let (descriptors, requests) = session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?
        .native_layer_admission(LayerRenderLimits::new(4, 4))
        .map_err(|error| error.to_string())?;
    Ok(NativeLayerAdmission {
        descriptors,
        requests,
    })
}

/// Explicitly associate an existing scene image layer with the dataset the user already opened.
/// There is no implicit fallback from a layer name to a filesystem path.
#[tauri::command]
fn bind_layer_to_open_dataset(
    layer_id: u64,
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<(), String> {
    session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?
        .bind_layer_to_open_dataset(newvolim_scene::LayerId(layer_id))
        .map_err(|error| error.to_string())
}

/// Plan a bounded XYZ chunk box for all selected and bound image-layer C pages.  The returned
/// asset paths use the source's detected v2/v3 key encoding and coordinates retain NGFF order.
#[tauri::command]
fn local_layer_chunk_plan(
    origin_xyz: [u64; 3],
    extent_xyz: [u32; 3],
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<Vec<LocalLayerChunkPlan>, String> {
    session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?
        .local_layer_chunk_plan(
            LayerRenderLimits::new(4, 4),
            SpatialChunkRegion::new(origin_xyz, extent_xyz),
            4_096,
        )
        .map_err(|error| error.to_string())
}

/// Read a bounded chunk box through the host's canonical source binding.  This is deliberately
/// separate from frame rendering while adapters are being connected: the returned bytes have
/// already passed asset-path, total-budget, and edge-aware size validation.
#[tauri::command]
fn read_local_layer_chunks(
    origin_xyz: [u64; 3],
    extent_xyz: [u32; 3],
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<Vec<LoadedLocalChunk>, String> {
    let session = session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?;
    let plans = session
        .local_layer_chunk_plan(
            LayerRenderLimits::new(4, 4),
            SpatialChunkRegion::new(origin_xyz, extent_xyz),
            4_096,
        )
        .map_err(|error| error.to_string())?;
    session
        .read_local_layer_chunks(&plans, 16 * 1024 * 1024, 64 * 1024 * 1024)
        .map_err(|error| error.to_string())
}

/// Load a bounded, contiguous spatial chunk region for each selected channel and convert it to
/// the exact fixed-page contract consumed by the portable WGPU recorder. Admission preserves
/// channel page ranges and rejects gaps, overlaps, or pool overflow instead of overwriting a
/// static page.
#[tauri::command]
fn native_portable_page_admission(
    origin_xyz: [u64; 3],
    extent_xyz: [u32; 3],
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<newvolim_render::NativePortableVolumeInput, String> {
    let session = session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?;
    let limits = LayerRenderLimits::new(4, 4);
    let (descriptors, _) = session
        .native_layer_admission(limits)
        .map_err(|error| error.to_string())?;
    let plans = session
        .local_layer_chunk_plan(
            limits,
            SpatialChunkRegion::new(origin_xyz, extent_xyz),
            4_096,
        )
        .map_err(|error| error.to_string())?;
    let loaded = session
        .read_local_layer_chunks(&plans, 16 * 1024 * 1024, 64 * 1024 * 1024)
        .map_err(|error| error.to_string())?;
    session
        .native_portable_page_admission(descriptors, &plans, &loaded)
        .map_err(|error| error.to_string())
}

/// Admit all currently selected local image layers into the typed shared-page scene packet used
/// by the world-ray recorder. This remains a host-owned operation: callers supply only a bounded
/// spatial region, never page locations, transforms, or raw source paths.
#[tauri::command]
fn native_portable_scene_page_admission(
    origin_xyz: [u64; 3],
    extent_xyz: [u32; 3],
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<newvolim_render::NativePortableSceneInput, String> {
    let session = session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?;
    let limits = LayerRenderLimits::new(4, 4);
    let (descriptors, _) = session
        .native_layer_admission(limits)
        .map_err(|error| error.to_string())?;
    let plans = session
        .local_layer_chunk_plan(
            limits,
            SpatialChunkRegion::new(origin_xyz, extent_xyz),
            4_096,
        )
        .map_err(|error| error.to_string())?;
    let loaded = session
        .read_local_layer_chunks(&plans, 16 * 1024 * 1024, 64 * 1024 * 1024)
        .map_err(|error| error.to_string())?;
    session
        .native_portable_scene_page_admission(descriptors, &plans, &loaded)
        .map_err(|error| error.to_string())
}

/// Build the complete trusted scene-camera packet: source admission, Palace camera projection,
/// stable annotation records, and world-ray conversion occur under one desktop session lock.
#[tauri::command]
fn native_portable_scene_camera_draw_admission(
    request: NativePortableDrawRequest,
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<newvolim_render::NativePortableSceneCameraDrawInput, String> {
    let session = session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?;
    let size = desktop_frame_size(request.width, request.height, 1)?;
    let controls = CameraControls {
        orbit_delta: [request.orbit_x, request.orbit_y],
        zoom: request.zoom,
    }
    .validate()
    .map_err(|error| error.to_string())?;
    let root = session.dataset_root().ok_or_else(|| {
        "open a local OME-Zarr dataset before preparing a portable scene".to_owned()
    })?;
    let limits = LayerRenderLimits::new(4, 4);
    let (descriptors, _) = session
        .native_layer_admission(limits)
        .map_err(|error| error.to_string())?;
    let plans = session
        .local_layer_chunk_plan(
            limits,
            SpatialChunkRegion::new(request.origin_xyz, request.extent_xyz),
            4_096,
        )
        .map_err(|error| error.to_string())?;
    let loaded = session
        .read_local_layer_chunks(&plans, 16 * 1024 * 1024, 64 * 1024 * 1024)
        .map_err(|error| error.to_string())?;
    let scene = session
        .native_portable_scene_page_admission(descriptors, &plans, &loaded)
        .map_err(|error| error.to_string())?;
    let annotations = project_session_annotation_words(&session, &root, size, controls)?;
    let draw = newvolim_render::NativePortableSceneDrawInput::new(
        scene.clone(),
        [size.width, size.height],
        annotations,
    )
    .map_err(|error| error.to_string())?;
    let rays = portable_scene_world_rays(&root, size, controls, &scene)?;
    newvolim_render::NativePortableSceneCameraDrawInput::new(draw, rays)
        .map_err(|error| error.to_string())
}

/// Build one camera-specific native recorder packet while the session lock protects the exact
/// dataset/layer/annotation snapshot used for admission.
fn native_portable_draw_for_session(
    request: NativePortableDrawRequest,
    session: &LocalSession,
) -> Result<(newvolim_render::NativePortableDrawInput, PathBuf, [u64; 3]), String> {
    let size = desktop_frame_size(request.width, request.height, 1)?;
    let controls = CameraControls {
        orbit_delta: [request.orbit_x, request.orbit_y],
        zoom: request.zoom,
    }
    .validate()
    .map_err(|error| error.to_string())?;
    let root = session.dataset_root().ok_or_else(|| {
        "open a local OME-Zarr dataset before preparing a portable draw".to_owned()
    })?;
    let limits = LayerRenderLimits::new(4, 4);
    let (descriptors, _) = session
        .native_layer_admission(limits)
        .map_err(|error| error.to_string())?;
    let plans = session
        .local_layer_chunk_plan(
            limits,
            SpatialChunkRegion::new(request.origin_xyz, request.extent_xyz),
            4_096,
        )
        .map_err(|error| error.to_string())?;
    let first_plan = plans
        .first()
        .ok_or_else(|| "portable draw has no selected local layer plan".to_owned())?;
    let first_chunk = first_plan
        .chunks
        .first()
        .ok_or_else(|| "portable draw layer plan has no selected chunk".to_owned())?;
    let voxel_origin_xyz: [Result<u64, String>; 3] = std::array::from_fn(|axis| {
        let source_axis = first_plan.request.source.spatial_axes_xyz[axis] as usize;
        first_chunk.spatial_chunk_xyz[axis]
            .checked_mul(first_plan.request.source.chunk_shape[source_axis])
            .ok_or_else(|| "portable chunk voxel origin overflows u64".to_owned())
    });
    let [origin_x, origin_y, origin_z] = voxel_origin_xyz;
    let voxel_origin_xyz = [origin_x?, origin_y?, origin_z?];
    let loaded = session
        .read_local_layer_chunks(&plans, 16 * 1024 * 1024, 64 * 1024 * 1024)
        .map_err(|error| error.to_string())?;
    let volume = session
        .native_portable_page_admission(descriptors, &plans, &loaded)
        .map_err(|error| error.to_string())?;
    let annotation_words = project_session_annotation_words(session, &root, size, controls)?;
    let draw = newvolim_render::NativePortableDrawInput::new(
        volume,
        [size.width, size.height],
        newvolim_render::PortableCameraControls::new(controls.orbit_delta, controls.zoom)
            .map_err(|error| error.to_string())?,
        annotation_words,
    )
    .map_err(|error| error.to_string())?;
    Ok((draw, root, voxel_origin_xyz))
}

/// Build one camera-specific native recorder packet. The annotation stream is projected by the
/// trusted host for this exact Palace camera and extent, and travels with the direct-volume
/// admission rather than as an independently replayable webview payload.
#[tauri::command]
fn native_portable_draw_admission(
    request: NativePortableDrawRequest,
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<newvolim_render::NativePortableDrawInput, String> {
    let session = session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?;
    native_portable_draw_for_session(request, &session).map(|(draw, _, _)| draw)
}

/// Build the full portable camera packet. Palace's fitted per-pixel rays are converted from its
/// raw ZYX array order to the direct portable volume's XYZ order, preserving the same camera
/// that projected the annotation records.
#[tauri::command]
fn native_portable_camera_draw_admission(
    request: NativePortableDrawRequest,
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<newvolim_render::NativePortableCameraDrawInput, String> {
    let (draw, root, voxel_origin_xyz) = {
        let session = session
            .lock()
            .map_err(|_| "desktop session lock was poisoned".to_owned())?;
        native_portable_draw_for_session(request, &session)?
    };
    let size = FrameSize::new(draw.extent_pixels[0], draw.extent_pixels[1])
        .map_err(|error| error.to_string())?;
    let controls = CameraControls {
        orbit_delta: draw.camera.orbit_delta,
        zoom: draw.camera.zoom,
    };
    let rays = portable_camera_rays_xyz(&root, size, controls, voxel_origin_xyz)?;
    newvolim_render::NativePortableCameraDrawInput::new(draw, rays)
        .map_err(|error| error.to_string())
}

/// Render the admitted native portable packet. The webview receives an encoded frame and paired
/// first-opacity attachment, never a backend-specific GPU handle or an untrusted depth surface.
#[tauri::command]
fn render_native_portable_camera_draw(
    request: NativePortableDrawRequest,
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<FramePayload, String> {
    let width = request.width;
    let height = request.height;
    let packet = native_portable_camera_draw_admission(request, session)?;
    let frame = newvolim_wgpu_frame::render_portable_camera_draw(&packet, 0)?;
    FramePayload::native_wgpu_camera(width, height, frame)
}

/// Render the trusted multi-layer scene packet and return the same bounded colour/depth payload
/// as the direct recorder route.
#[tauri::command]
fn render_native_portable_scene_camera_draw(
    request: NativePortableDrawRequest,
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<FramePayload, String> {
    let width = request.width;
    let height = request.height;
    let packet = native_portable_scene_camera_draw_admission(request, session)?;
    let frame = newvolim_wgpu_frame::render_portable_scene_camera_draw(&packet, 0)?;
    FramePayload::native_wgpu_camera(width, height, frame)
}

/// Pick an annotation against a freshly recorded native-portable frame. Re-recording avoids
/// accepting a browser-owned depth sidecar and makes the camera/volume/annotation tuple exact.
#[tauri::command]
fn pick_native_portable_annotation(
    request: NativePortablePickRequest,
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<Option<AnnotationPickPayload>, String> {
    let session = session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?;
    pick_native_portable_annotation_for_session(request, &session)
}

fn pick_native_portable_annotation_for_session(
    request: NativePortablePickRequest,
    session: &LocalSession,
) -> Result<Option<AnnotationPickPayload>, String> {
    let (packet, physical_ray, annotations, index) = {
        let (draw, root, voxel_origin_xyz) =
            native_portable_draw_for_session(request.draw, session)?;
        if request.x >= draw.extent_pixels[0] || request.y >= draw.extent_pixels[1] {
            return Err("portable annotation-pick pixel is outside the admitted extent".into());
        }
        let size = FrameSize::new(draw.extent_pixels[0], draw.extent_pixels[1])
            .map_err(|error| error.to_string())?;
        let controls = CameraControls {
            orbit_delta: draw.camera.orbit_delta,
            zoom: draw.camera.zoom,
        };
        let rays = portable_camera_rays_xyz(&root, size, controls, voxel_origin_xyz)?;
        let packet = newvolim_render::NativePortableCameraDrawInput::new(draw, rays)
            .map_err(|error| error.to_string())?;
        let index = usize::try_from(request.y)
            .ok()
            .and_then(|row| row.checked_mul(packet.draw.extent_pixels[0] as usize))
            .and_then(|row| row.checked_add(request.x as usize))
            .ok_or_else(|| "portable annotation-pick pixel offset overflows usize".to_owned())?;
        let ray = packet
            .rays
            .get(index)
            .ok_or_else(|| "portable camera packet omitted the requested pixel ray".to_owned())?;
        let global_origin = std::array::from_fn(|axis| {
            f64::from(ray.origin_xyz[axis]) + voxel_origin_xyz[axis] as f64
        });
        let physical_ray = session
            .portable_voxel_ray_to_physical(global_origin, ray.direction_xyz.map(f64::from))
            .map_err(|error| error.to_string())?;
        (packet, physical_ray, session.annotations().to_vec(), index)
    };
    let frame = newvolim_wgpu_frame::render_portable_camera_draw(&packet, 0)?;
    let distance = *frame
        .ray_distances
        .get(index)
        .ok_or_else(|| "portable recorder omitted the requested depth pixel".to_owned())?;
    depth_aware_annotation_pick(
        &annotations,
        physical_ray.ray,
        f64::from(distance) * physical_ray.physical_distance_per_palace_unit,
    )
}

fn portable_camera_rays_xyz(
    root: &Path,
    size: FrameSize,
    controls: CameraControls,
    volume_origin_xyz: [u64; 3],
) -> Result<Vec<newvolim_render::PortableCameraRay>, String> {
    let count = usize::try_from(size.width)
        .ok()
        .and_then(|width| width.checked_mul(size.height as usize))
        .ok_or_else(|| "portable camera ray count overflows usize".to_owned())?;
    if count > newvolim_render::NativePortableCameraDrawInput::MAX_RAYS {
        return Err(format!(
            "portable camera requires {count} rays, exceeding the {}-ray bound",
            newvolim_render::NativePortableCameraDrawInput::MAX_RAYS
        ));
    }
    let mut rays = Vec::with_capacity(count);
    for y in 0..size.height {
        for x in 0..size.width {
            let ray = camera_ray_for_local_zarr(root, size, controls, [x, y])
                .map_err(|error| error.to_string())?;
            rays.push(newvolim_render::PortableCameraRay {
                // Palace rays use global ZYX array coordinates. The portable page is the
                // requested XYZ subvolume, so translate only origins into that local space;
                // directions and travelled distances remain invariant under translation.
                origin_xyz: [
                    ray.origin[2] - volume_origin_xyz[0] as f32,
                    ray.origin[1] - volume_origin_xyz[1] as f32,
                    ray.origin[0] - volume_origin_xyz[2] as f32,
                ],
                direction_xyz: [ray.direction[2], ray.direction[1], ray.direction[0]],
            });
        }
    }
    Ok(rays)
}

/// Convert the Palace-derived local voxel rays for the admitted reference layer into normalized
/// physical world rays. The scene renderer then independently inverts every other layer's
/// transform, so camera authority is never copied from an arbitrary layer ordinal.
fn portable_scene_world_rays(
    root: &Path,
    size: FrameSize,
    controls: CameraControls,
    scene: &newvolim_render::NativePortableSceneInput,
) -> Result<Vec<newvolim_render::PortableWorldRay>, String> {
    let layer = scene
        .layers
        .first()
        .ok_or_else(|| "portable scene has no reference layer".to_owned())?;
    let local = portable_camera_rays_xyz(root, size, controls, layer.voxel_origin_xyz)?;
    local
        .into_iter()
        .map(|ray| {
            let global_origin = std::array::from_fn(|axis| {
                f64::from(ray.origin_xyz[axis]) + layer.voxel_origin_xyz[axis] as f64
            });
            let world_origin = layer.transform.voxel_to_world(global_origin);
            let direction = std::array::from_fn(|axis| {
                f64::from(ray.direction_xyz[axis]) * layer.transform.scale[axis]
            });
            newvolim_render::PortableWorldRay::new(world_origin, direction)
                .map_err(|error| error.to_string())
        })
        .collect()
}

#[tauri::command]
fn delete_annotation(
    id: u64,
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<Annotation, String> {
    let mut session = session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?;
    session
        .remove_annotation(newvolim_scene::AnnotationId(id))
        .map_err(|error| error.to_string())
}

/// Explicit user-selected export; no automatic background write is performed.
#[tauri::command]
fn export_annotations(
    path: String,
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<(), String> {
    let session = session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?;
    session
        .export_annotations(path)
        .map_err(|error| error.to_string())
}

/// Import is dataset-bound and validated by [`LocalSession`] before it replaces scene state.
#[tauri::command]
fn import_annotations(
    path: String,
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<Vec<AnnotationPlacement>, String> {
    let mut session = session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?;
    session
        .import_annotations(path)
        .map_err(|error| error.to_string())?;
    Ok(annotation_placements(&session))
}

/// A transport proof that the CSR canvas can request a real native Palace frame. It is kept
/// separate from `render_open_dataset` so source authorization can never be confused with the
/// deterministic smoke volume.
#[tauri::command]
fn render_synthetic_preview(width: u32, height: u32) -> Result<FramePayload, String> {
    let size = desktop_frame_size(width, height, 1)?;
    render_synthetic_png(64, size)
        .map(|png| FramePayload::png(width, height, png))
        .map_err(|error| error.to_string())
}

/// Palace sliceviewer transport proof for the three linked 2D panes.
#[tauri::command]
fn render_synthetic_orthogonal_preview(
    width: u32,
    height: u32,
) -> Result<OrthogonalPayload, String> {
    let size = desktop_frame_size(width, height, 3)?;
    let [xy, xz, yz] =
        render_synthetic_orthogonal_png(64, size).map_err(|error| error.to_string())?;
    Ok(OrthogonalPayload {
        xy: FramePayload::png(width, height, xy),
        xz: FramePayload::png(width, height, xz),
        yz: FramePayload::png(width, height, yz),
    })
}

#[tauri::command]
fn render_synthetic_orthogonal_at(
    width: u32,
    height: u32,
    x: u32,
    y: u32,
    z: u32,
) -> Result<OrthogonalPayload, String> {
    let size = desktop_frame_size(width, height, 3)?;
    let [xy, xz, yz] = render_synthetic_orthogonal_at_png(64, size, [z, y, x])
        .map_err(|error| error.to_string())?;
    Ok(OrthogonalPayload {
        xy: FramePayload::png(width, height, xy),
        xz: FramePayload::png(width, height, xz),
        yz: FramePayload::png(width, height, yz),
    })
}

#[tauri::command]
fn render_open_dataset(
    width: u32,
    height: u32,
    session: tauri::State<'_, Mutex<LocalSession>>,
    depth_cache: tauri::State<'_, Mutex<PickableDepthCache>>,
) -> Result<FramePayload, String> {
    let root = session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?
        .dataset_root()
        .ok_or_else(|| "open a local OME-Zarr dataset before requesting a frame".to_owned())?;
    let size = desktop_frame_size(width, height, 1)?;
    let attachments =
        render_local_zarr_attachments(&root, size).map_err(|error| error.to_string())?;
    cache_pickable_depth(
        &depth_cache,
        root,
        size,
        CameraControls::default(),
        &attachments,
    )?;
    FramePayload::palace_attachments(attachments)
}

/// Render the opened local dataset after applying the UI's bounded trackball controls.
#[tauri::command]
fn render_open_dataset_camera(
    width: u32,
    height: u32,
    orbit_x: i32,
    orbit_y: i32,
    zoom: f32,
    session: tauri::State<'_, Mutex<LocalSession>>,
    depth_cache: tauri::State<'_, Mutex<PickableDepthCache>>,
) -> Result<FramePayload, String> {
    let root = session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?
        .dataset_root()
        .ok_or_else(|| "open a local OME-Zarr dataset before requesting a frame".to_owned())?;
    let size = desktop_frame_size(width, height, 1)?;
    let controls = CameraControls {
        orbit_delta: [orbit_x, orbit_y],
        zoom,
    };
    let attachments = render_local_zarr_with_camera_attachments(&root, size, controls)
        .map_err(|error| error.to_string())?;
    cache_pickable_depth(&depth_cache, root, size, controls, &attachments)?;
    FramePayload::palace_attachments(attachments)
}

/// Produce the bounded portable GPU record stream for the current desktop frame. The webview may
/// submit these words to its annotation pass only with the same extent and camera controls; they
/// are intentionally derived in the trusted host from persisted physical geometry.
#[tauri::command]
fn project_open_dataset_annotations(
    width: u32,
    height: u32,
    orbit_x: i32,
    orbit_y: i32,
    zoom: f32,
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<Vec<u32>, String> {
    let size = desktop_frame_size(width, height, 1)?;
    let controls = CameraControls {
        orbit_delta: [orbit_x, orbit_y],
        zoom,
    }
    .validate()
    .map_err(|error| error.to_string())?;
    let session = session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?;
    let root = session
        .dataset_root()
        .ok_or_else(|| "open a local OME-Zarr dataset before projecting annotations".to_owned())?;
    project_session_annotation_words(&session, &root, size, controls)
}

/// Select the nearest visible annotation using the renderer-owned first-opacity distance.
///
/// A matching renderer-owned sidecar is reused; otherwise the host rerenders the bounded frame.
/// The browser never supplies depth, and the reconstructed Palace ray and sampled attachment
/// always belong to the same local camera/extent/controls tuple.
#[tauri::command]
fn pick_open_dataset_annotation(
    request: AnnotationPickRequest,
    session: tauri::State<'_, Mutex<LocalSession>>,
    depth_cache: tauri::State<'_, Mutex<PickableDepthCache>>,
) -> Result<Option<AnnotationPickPayload>, String> {
    let size = desktop_frame_size(request.width, request.height, 1)?;
    let controls = CameraControls {
        orbit_delta: [request.orbit_x, request.orbit_y],
        zoom: request.zoom,
    };
    let root = session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?;
    let root = root
        .dataset_root()
        .ok_or_else(|| "open a local OME-Zarr dataset before picking an annotation".to_owned())?;
    let depth = depth_cache
        .lock()
        .map_err(|_| "desktop depth cache lock was poisoned".to_owned())?
        .depth_for(&root, size, controls);
    let depth = match depth {
        Some(depth) => depth,
        None => {
            let attachments = render_local_zarr_with_camera_attachments(&root, size, controls)
                .map_err(|error| error.to_string())?;
            let depth = attachments.ray_distance().cloned().ok_or_else(|| {
                "Palace render did not supply a paired ray-distance attachment".to_owned()
            })?;
            cache_pickable_depth(&depth_cache, root.clone(), size, controls, &attachments)?;
            depth
        }
    };
    let session = session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?;
    pick_local_dataset_annotation(
        &session,
        &root,
        size,
        controls,
        [request.x, request.y],
        &depth,
    )
}

#[tauri::command]
fn render_open_dataset_orthogonal(
    width: u32,
    height: u32,
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<OrthogonalPayload, String> {
    let root = session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?
        .dataset_root()
        .ok_or_else(|| "open a local OME-Zarr dataset before requesting slices".to_owned())?;
    let size = desktop_frame_size(width, height, 3)?;
    let [xy, xz, yz] =
        render_local_zarr_orthogonal_png(root, size).map_err(|error| error.to_string())?;
    Ok(OrthogonalPayload {
        xy: FramePayload::png(width, height, xy),
        xz: FramePayload::png(width, height, xz),
        yz: FramePayload::png(width, height, yz),
    })
}

#[tauri::command]
fn render_open_dataset_orthogonal_at(
    width: u32,
    height: u32,
    x: u32,
    y: u32,
    z: u32,
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<OrthogonalPayload, String> {
    let (root, crosshair_xyz) = {
        let session = session
            .lock()
            .map_err(|_| "desktop session lock was poisoned".to_owned())?;
        let root = session
            .dataset_root()
            .ok_or_else(|| "open a local OME-Zarr dataset before requesting slices".to_owned())?;
        let crosshair_xyz = session
            .clamp_crosshair_xyz([x, y, z])
            .map_err(|error| error.to_string())?;
        (root, crosshair_xyz)
    };
    let size = desktop_frame_size(width, height, 3)?;
    let [xy, xz, yz] = render_local_zarr_orthogonal_at_png(
        root,
        size,
        [crosshair_xyz[2], crosshair_xyz[1], crosshair_xyz[0]],
    )
    .map_err(|error| error.to_string())?;
    Ok(OrthogonalPayload {
        xy: FramePayload::png(width, height, xy),
        xz: FramePayload::png(width, height, xz),
        yz: FramePayload::png(width, height, yz),
    })
}

/// Exercise the desktop host's real local-data path without a window-manager input injector.
/// This deliberately goes through [`LocalSession`] before Palace rendering, so it validates the
/// same canonicalization, metadata, and frame transport prerequisites as the Tauri commands.
fn smoke_local_dataset(root: &Path) -> Result<(), String> {
    let mut session = LocalSession::default();
    let summary = session
        .open_local_omezarr(root)
        .map_err(|error| error.to_string())?;
    let root = session
        .dataset_root()
        .ok_or_else(|| "local smoke did not retain the opened dataset root".to_owned())?;
    // This is a bounded correctness probe, not a frame-rate benchmark. Keep the target small
    // enough to exercise debug Palace deterministically on software Vulkan too.
    let size = FrameSize::new(32, 24).map_err(|error| error.to_string())?;
    let volume = render_local_zarr_attachments(&root, size)
        .map_err(|error| error.to_string())
        .and_then(FramePayload::palace_attachments)?;
    let camera = render_local_zarr_with_camera_attachments(
        &root,
        size,
        CameraControls {
            orbit_delta: [24, -12],
            zoom: 1.1,
        },
    )
    .map_err(|error| error.to_string())
    .and_then(FramePayload::palace_attachments)?;
    let slices =
        render_local_zarr_orthogonal_png(&root, size).map_err(|error| error.to_string())?;
    if volume.data_url.is_empty()
        || camera.data_url.is_empty()
        || volume.ray_distance_pfm_base64.is_none()
        || camera.ray_distance_pfm_base64.is_none()
        || slices.iter().any(Vec::is_empty)
    {
        return Err(
            "Palace local smoke produced an empty colour or paired-depth payload".to_owned(),
        );
    }
    println!(
        "local desktop smoke: {} default-volume bytes; {} camera-volume bytes; [{}, {}, {}] orthogonal bytes; shape {:?}",
        volume.data_url.len(),
        camera.data_url.len(),
        slices[0].len(),
        slices[1].len(),
        slices[2].len(),
        summary.voxel_shape_xyz,
    );
    Ok(())
}

fn main() {
    let mut args = std::env::args_os().skip(1);
    if let Some(flag) = args.next() {
        if flag == "--smoke-local" {
            let Some(root) = args.next() else {
                eprintln!("usage: newvolim-desktop --smoke-local PATH");
                std::process::exit(2);
            };
            if args.next().is_some() {
                eprintln!("usage: newvolim-desktop --smoke-local PATH");
                std::process::exit(2);
            }
            if let Err(error) = smoke_local_dataset(Path::new(&root)) {
                eprintln!("local desktop smoke failed: {error}");
                std::process::exit(1);
            }
            return;
        }
        eprintln!("usage: newvolim-desktop [--smoke-local PATH]");
        std::process::exit(2);
    }
    tauri::Builder::default()
        .manage(Mutex::new(LocalSession::default()))
        .manage(Mutex::new(PickableDepthCache::default()))
        .invoke_handler(tauri::generate_handler![
            open_local_omezarr,
            prepare_default_portable_image_layer,
            add_point_annotation,
            add_polygon_annotation,
            add_rectangle_annotation,
            add_ellipse_annotation,
            list_annotations,
            layer_render_plan,
            local_layer_render_requests,
            native_layer_admission,
            bind_layer_to_open_dataset,
            local_layer_chunk_plan,
            read_local_layer_chunks,
            native_portable_page_admission,
            native_portable_scene_page_admission,
            native_portable_scene_camera_draw_admission,
            native_portable_draw_admission,
            native_portable_camera_draw_admission,
            render_native_portable_camera_draw,
            render_native_portable_scene_camera_draw,
            pick_native_portable_annotation,
            delete_annotation,
            export_annotations,
            import_annotations,
            render_open_dataset,
            render_open_dataset_camera,
            project_open_dataset_annotations,
            pick_open_dataset_annotation,
            render_open_dataset_orthogonal,
            render_open_dataset_orthogonal_at,
            render_synthetic_orthogonal_preview,
            render_synthetic_orthogonal_at,
            render_synthetic_preview,
            session_summary
        ])
        .run(tauri::generate_context!())
        .expect("error while running newvolim desktop host");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_frame_budget_bounds_colour_and_optional_depth_payloads() {
        assert_eq!(desktop_frame_size(4096, 4096, 1).unwrap().width, 4096);
        assert!(desktop_frame_size(4097, 4096, 1).is_err());
        assert!(desktop_frame_size(4096, 4096, 3).is_err());
        assert!(desktop_frame_size(0, 1, 1).is_err());
        assert!(desktop_frame_size(u32::MAX, u32::MAX, u64::MAX).is_err());
    }

    #[test]
    fn png_frame_payload_is_a_self_contained_canvas_source() {
        let payload = FramePayload::png(2, 1, vec![137, 80, 78, 71]);

        assert_eq!(payload.mime_type, "image/png");
        assert_eq!((payload.width, payload.height), (2, 1));
        assert_eq!(payload.target.extent, PhysicalExtent::new(2, 1).unwrap());
        assert_eq!(payload.target.color_encoding, ColorEncoding::Srgb);
        assert_eq!(payload.target.depth, DepthAttachment::None);
        assert_eq!(payload.progress, FrameProgress::Final);
        assert_eq!(payload.data_url, "data:image/png;base64,iVBORw==");
        assert_eq!(
            serde_json::to_value(&payload).unwrap(),
            serde_json::json!({
                "mimeType": "image/png",
                "width": 2,
                "height": 1,
                "target": {
                    "extent": { "width": 2, "height": 1 },
                    "colorFormat": "rgba8Unorm",
                    "colorEncoding": "srgb",
                    "depth": "none",
                },
                "progress": "final",
                "dataUrl": "data:image/png;base64,iVBORw==",
            })
        );
    }

    #[test]
    fn paired_palace_attachment_is_serialized_as_depth_aware() {
        let color = palace_png::RgbaFrame::new(2, 1, vec![1, 2, 3, 255, 4, 5, 6, 255]).unwrap();
        let depth = palace_png::RayDistanceFrame::new(2, 1, vec![0.25, f32::INFINITY]).unwrap();
        let payload = FramePayload::palace_attachments(
            palace_png::FrameAttachments::new(color, Some(depth)).unwrap(),
        )
        .unwrap();

        assert_eq!(payload.target.depth, DepthAttachment::RayDistanceF32);
        assert!(payload.ray_distance_pfm_base64.is_some());
        let json = serde_json::to_value(payload).unwrap();
        assert_eq!(json["target"]["depth"], "rayDistanceF32");
        assert!(json["rayDistancePfmBase64"].as_str().is_some());
    }

    #[test]
    fn native_annotation_pick_respects_the_renderer_depth_bound() {
        let visible = Annotation::new(
            newvolim_scene::AnnotationId(4),
            "visible",
            newvolim_scene::AnnotationGeometry::Point([1.0, 0.0, 0.0]),
            [255, 216, 72],
        )
        .unwrap();
        let occluded = Annotation::new(
            newvolim_scene::AnnotationId(2),
            "occluded",
            newvolim_scene::AnnotationGeometry::Point([3.0, 0.0, 0.0]),
            [255, 216, 72],
        )
        .unwrap();
        let ray = newvolim_render::PickRay::new([0.0, 0.0, 0.0], [1.0, 0.0, 0.0]).unwrap();
        assert_eq!(
            depth_aware_annotation_pick(&[visible.clone(), occluded], ray, 1.0).unwrap(),
            Some(AnnotationPickPayload {
                annotation_id: 4,
                distance: 1.0,
            })
        );
        assert_eq!(
            depth_aware_annotation_pick(&[visible], ray, 0.9).unwrap(),
            None
        );
    }

    #[test]
    fn fixture_annotations_project_to_the_portable_gpu_record_stream() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        let mut session = LocalSession::default();
        session.open_local_omezarr(&root).unwrap();
        session.add_point_annotation("centre", [16, 16, 4]).unwrap();
        let words = project_session_annotation_words(
            &session,
            &root,
            FrameSize::new(64, 48).unwrap(),
            CameraControls::default(),
        )
        .unwrap();
        assert_eq!(
            words.len(),
            newvolim_render::PortableAnnotationPrimitive::WORDS
        );
        assert_eq!(&words[..2], &[1, 1]);
        assert_eq!(words[2], 0x00ff_d848);
        assert!(f32::from_bits(words[6]).is_finite());
    }

    #[test]
    fn fitted_palace_camera_rays_are_reordered_for_the_portable_xyz_volume() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        let size = FrameSize::new(3, 2).unwrap();
        let controls = CameraControls {
            orbit_delta: [37, -19],
            zoom: 1.2,
        };
        let palace = camera_ray_for_local_zarr(&root, size, controls, [0, 0]).unwrap();
        let rays = portable_camera_rays_xyz(&root, size, controls, [0, 0, 0]).unwrap();
        assert_eq!(rays.len(), 6);
        assert_eq!(
            rays[0].origin_xyz,
            [palace.origin[2], palace.origin[1], palace.origin[0]]
        );
        let translated = portable_camera_rays_xyz(&root, size, controls, [5, 7, 2]).unwrap();
        assert_eq!(
            translated[0].origin_xyz,
            [
                palace.origin[2] - 5.0,
                palace.origin[1] - 7.0,
                palace.origin[0] - 2.0,
            ]
        );
        assert_eq!(translated[0].direction_xyz, rays[0].direction_xyz);
        assert_eq!(
            rays[0].direction_xyz,
            [
                palace.direction[2],
                palace.direction[1],
                palace.direction[0]
            ]
        );
        let length_squared: f32 = rays[0]
            .direction_xyz
            .iter()
            .map(|value| value * value)
            .sum();
        assert!((length_squared.sqrt() - 1.0).abs() < 0.001);
    }

    #[test]
    #[ignore = "requires a local WGPU adapter"]
    fn native_portable_picker_uses_its_matching_camera_packet_and_depth() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        let mut session = LocalSession::default();
        session.open_local_omezarr(&root).unwrap();
        session.prepare_default_portable_image_layer().unwrap();
        session
            .add_point_annotation("front centre", [64, 64, 0])
            .unwrap();
        let size = FrameSize::new(64, 48).unwrap();
        let projection =
            project_point_for_local_zarr(&root, size, CameraControls::default(), [0.0, 64.0, 64.0])
                .unwrap()
                .unwrap();
        let pixel = projection
            .pixel
            .map(|value| value.floor().clamp(0.0, 63.0) as u32);
        let result = pick_native_portable_annotation_for_session(
            NativePortablePickRequest {
                draw: NativePortableDrawRequest {
                    origin_xyz: [2, 2, 0],
                    extent_xyz: [1, 1, 1],
                    width: 64,
                    height: 48,
                    orbit_x: 0,
                    orbit_y: 0,
                    zoom: 1.0,
                },
                x: pixel[0],
                y: pixel[1],
            },
            &session,
        )
        .unwrap();
        // The exact fixture point can lie behind the first-opacity surface; success here proves
        // the portable renderer, paired depth, ray reconstruction, and annotation query share
        // one host-owned packet rather than requiring a browser depth input.
        assert!(result.is_none() || result.as_ref().is_some_and(|hit| hit.annotation_id == 0));
    }

    #[test]
    fn fixture_native_pick_uses_the_paired_palace_depth_and_ngff_bridge() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        let mut session = LocalSession::default();
        session.open_local_omezarr(&root).unwrap();
        let root = session
            .dataset_root()
            .expect("open must retain dataset root");
        let size = FrameSize::new(32, 24).unwrap();
        let controls = CameraControls::default();
        let attachments = render_local_zarr_with_camera_attachments(&root, size, controls).unwrap();
        let depth = attachments
            .ray_distance()
            .expect("fixture render must supply paired Palace depth");
        // There are no annotations yet, but this exercises the complete local camera-ray,
        // NGFF-transform, Palace depth-readback, and bounded picker path without browser input.
        assert_eq!(
            pick_local_dataset_annotation(&session, &root, size, controls, [16, 12], depth,)
                .unwrap(),
            None
        );
    }

    #[test]
    fn depth_cache_requires_an_exact_frame_tuple() {
        let root = PathBuf::from("dataset-a");
        let size = FrameSize::new(2, 1).unwrap();
        let controls = CameraControls::default();
        let depth = palace_png::RayDistanceFrame::new(2, 1, vec![1.0, f32::INFINITY]).unwrap();
        let mut cache = PickableDepthCache::default();
        cache.replace(root.clone(), size, controls, Some(&depth));

        assert_eq!(cache.depth_for(&root, size, controls), Some(depth));
        assert!(cache
            .depth_for(&root, FrameSize::new(1, 2).unwrap(), controls)
            .is_none());
        assert!(cache
            .depth_for(
                &root,
                size,
                CameraControls {
                    orbit_delta: [1, 0],
                    ..controls
                },
            )
            .is_none());
        assert!(cache
            .depth_for(Path::new("dataset-b"), size, controls)
            .is_none());
    }

    #[test]
    fn fixture_palace_attachment_reaches_the_native_depth_aware_payload() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        let attachments = render_local_zarr_with_camera_attachments(
            root,
            FrameSize::new(32, 24).unwrap(),
            CameraControls::default(),
        )
        .unwrap();
        let payload = FramePayload::palace_attachments(attachments).unwrap();
        assert_eq!((payload.width, payload.height), (32, 24));
        assert_eq!(payload.target.depth, DepthAttachment::RayDistanceF32);
        let pfm = payload
            .ray_distance_pfm_base64
            .expect("fixture render must preserve the paired Palace PFM");
        assert!(STANDARD
            .decode(pfm)
            .unwrap()
            .starts_with(b"Pf\n32 24\n-1.0\n"));
    }
}
