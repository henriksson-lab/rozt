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
        if let Some(portable) = attachments.portable_frame_attachments() {
            return Self::portable_frame_attachments(portable);
        }
        let (color, _) = attachments.into_parts();
        let width = color.width();
        let height = color.height();
        let png = palace_png::encode_rgba(&color);
        let target = RenderTarget::new(
            PhysicalExtent::new(width, height).map_err(|error| error.to_string())?,
            ColorFormat::Rgba8Unorm,
            ColorEncoding::Srgb,
            DepthAttachment::None,
        )
        .map_err(|error| error.to_string())?;
        Ok(Self {
            mime_type: "image/png",
            width,
            height,
            target,
            progress: FrameProgress::Final,
            data_url: format!("data:image/png;base64,{}", STANDARD.encode(png)),
            ray_distance_pfm_base64: None,
        })
    }

    /// Encode one renderer-owned portable colour/depth result without routing it through a
    /// Vulkan-specific readback representation.
    fn portable_frame_attachments(
        attachments: palace_core::gpu::PortableFrameAttachments,
    ) -> Result<Self, String> {
        let width = attachments.width;
        let height = attachments.height;
        let (png, ray_distance_pfm) =
            palace_png::encode_portable_frame_attachments(&attachments).into_parts();
        let target = RenderTarget::new(
            PhysicalExtent::new(width, height).map_err(|error| error.to_string())?,
            ColorFormat::Rgba8Unorm,
            ColorEncoding::Srgb,
            DepthAttachment::RayDistanceF32,
        )
        .map_err(|error| error.to_string())?;
        Ok(Self {
            mime_type: "image/png",
            width,
            height,
            target,
            progress: FrameProgress::Final,
            data_url: format!("data:image/png;base64,{}", STANDARD.encode(png)),
            ray_distance_pfm_base64: Some(
                STANDARD.encode(ray_distance_pfm.expect(
                    "portable frame encoding always preserves its paired depth attachment",
                )),
            ),
        })
    }

    fn native_wgpu_camera(
        width: u32,
        height: u32,
        frame: newvolim_wgpu_frame::RenderedProjection,
    ) -> Result<Self, String> {
        let rgba = frame.rgba.into_iter().flatten().collect();
        let attachments = palace_core::gpu::PortableFrameAttachments::new(
            width,
            height,
            rgba,
            frame.ray_distances,
        )
        .ok_or_else(|| "native WGPU renderer returned an invalid paired frame".to_owned())?;
        Self::portable_frame_attachments(attachments)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct OrthogonalPayload {
    xy: FramePayload,
    xz: FramePayload,
    yz: FramePayload,
    /// Physical horizontal-to-vertical canvas ratios in XY, XZ, YZ order. Pixel buffers retain
    /// their requested size; the webview applies this only to presentation and hit geometry.
    aspect_ratios: [f64; 3],
    /// Present only for the bounded portable route. Crosshair overlays use this local voxel box
    /// instead of incorrectly treating a chunk-resident pane as the whole dataset.
    #[serde(skip_serializing_if = "Option::is_none")]
    viewport_origin_xyz: Option<[u64; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    viewport_dimensions_xyz: Option<[u32; 3]>,
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

/// Scene equivalent of [`NativePortablePickRequest`]. The host reconstructs the ordered scene,
/// its physical world rays, and its paired depth before considering one webview pixel.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativePortableScenePickRequest {
    draw: NativePortableDrawRequest,
    x: u32,
    y: u32,
}

/// Transformed portable scene slices deliberately use the same floor-nearest rule as the
/// portable resample contract. Linear filtering is a separate future policy because it changes
/// integer-label and transfer-function semantics at physical layer boundaries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PortableSceneSliceSampling {
    FloorNearest,
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
    native_portable_scene_camera_draw_for_session(request, &session)
}

fn native_portable_scene_camera_draw_for_session(
    request: NativePortableDrawRequest,
    session: &LocalSession,
) -> Result<newvolim_render::NativePortableSceneCameraDrawInput, String> {
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
    newvolim_render::NativePortableCameraDrawInput::new_with_voxel_origin(
        draw,
        voxel_origin_xyz,
        rays,
    )
    .map_err(|error| error.to_string())
}

/// Adapt the already admitted direct-volume page submission and fitted local-XYZ camera rays to
/// Palace's bounded page-DVR contract. Local fitted rays and the admitted page extent are
/// converted through the layer's anisotropic physical transform before they reach the core.
/// Multi-channel scene packets retain their existing renderer/Vulkan fallback.
fn palace_dvr_packet_from_native_camera(
    input: &newvolim_render::NativePortableCameraDrawInput,
) -> Result<
    (
        palace_core::gpu::PortableDvrVolumeLevel,
        Vec<palace_core::gpu::PortableRayInterval>,
    ),
    String,
> {
    let volume = &input.draw.volume;
    let transform = volume
        .frame
        .descriptors
        .first()
        .ok_or_else(|| "portable Palace DVR has no layer descriptor".to_owned())?
        .transform;
    let [channel] = volume.channels.as_slice() else {
        return Err("portable Palace DVR currently requires one admitted channel".into());
    };
    let first = usize::try_from(channel.page_offset)
        .map_err(|_| "portable Palace DVR page offset overflows usize")?;
    let end = first
        .checked_add(channel.page_count as usize)
        .ok_or_else(|| "portable Palace DVR page range overflows usize".to_owned())?;
    let source_pages = volume
        .frame
        .page_submission
        .pages
        .get(first..end)
        .ok_or_else(|| "portable Palace DVR page range is outside the submission".to_owned())?;
    let pages = source_pages
        .iter()
        .enumerate()
        .map(|(index, words)| {
            palace_core::gpu::PortableTensorPage::new(
                u64::try_from(index + 1).expect("four portable pages fit owner tags"),
                words.clone(),
            )
            .ok_or_else(|| "portable Palace DVR page is not admitted".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let minimum = transform
        .voxel_to_world(input.voxel_origin_xyz.map(|value| value as f64))
        .map(|value| value as f32);
    let maximum = std::array::from_fn(|axis| {
        (transform.translation[axis]
            + transform.scale[axis]
                * (input.voxel_origin_xyz[axis] + u64::from(volume.dimensions_xyz[axis])) as f64)
            as f32
    });
    let level = palace_core::gpu::PortableDvrVolumeLevel::new(
        volume.dimensions_xyz,
        minimum,
        maximum,
        pages,
    )
    .ok_or_else(|| "portable Palace DVR volume level is invalid".to_owned())?;
    let rays = input
        .rays
        .iter()
        .map(|ray| {
            let origin = std::array::from_fn(|axis| {
                minimum[axis] + ray.origin_xyz[axis] * transform.scale[axis] as f32
            });
            let raw_direction =
                std::array::from_fn(|axis| ray.direction_xyz[axis] * transform.scale[axis] as f32);
            let length = raw_direction
                .iter()
                .map(|value| value * value)
                .sum::<f32>()
                .sqrt();
            let direction = raw_direction.map(|value| value / length);
            let far_squared: f32 = (0..3)
                .map(|axis| {
                    origin[axis]
                        .abs()
                        .max((origin[axis] - maximum[axis]).abs())
                        .powi(2)
                })
                .sum();
            palace_core::gpu::PortableRayInterval::new(
                origin,
                direction,
                0.0,
                far_squared.sqrt() + 1.0,
            )
            .ok_or_else(|| "fitted portable camera ray is invalid for Palace DVR".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((level, rays))
}

/// Convert the admitted direct channel display policy to Palace's explicit DVR LUT.  The table
/// retains the declared scalar window while making below-window samples transparent and scaling
/// opacity linearly through the window.
fn palace_transfer_from_native_camera(
    input: &newvolim_render::NativePortableCameraDrawInput,
) -> Result<palace_core::gpu::PortableTransferFunction, String> {
    let [channel] = input.draw.volume.channels.as_slice() else {
        return Err("portable Palace DVR currently requires one admitted channel".into());
    };
    let min = channel.transfer.window_start as f32;
    let max = channel.transfer.window_end as f32;
    if !min.is_finite() || !max.is_finite() || max <= min {
        return Err("portable Palace DVR requires a finite non-empty transfer window".into());
    }
    let entries = (0..256)
        .map(|index| {
            let alpha = ((index as f32 / 255.0) * channel.transfer.opacity * 255.0) as u8;
            [
                channel.transfer.color_srgb[0],
                channel.transfer.color_srgb[1],
                channel.transfer.color_srgb[2],
                alpha,
            ]
        })
        .collect();
    palace_core::gpu::PortableTransferFunction::new(min, max, entries)
        .ok_or_else(|| "portable Palace DVR transfer is invalid".to_owned())
}

/// Execute the direct portable camera packet through Palace's bounded page-DVR contract. The
/// pages remain owner-tagged until the recorder dispatch; a host without an eligible WGPU
/// adapter uses the exact core CPU oracle rather than changing camera, transfer, or depth
/// semantics. Palace does not yet own the projected-annotation overlay pass, so the caller keeps
/// the existing scene-capable native recorder for packets which carry annotation words.
fn render_palace_portable_camera_draw(
    input: &newvolim_render::NativePortableCameraDrawInput,
) -> Result<palace_core::gpu::PortableFrameAttachments, String> {
    let (level, rays) = palace_dvr_packet_from_native_camera(input)?;
    let transform = input
        .draw
        .volume
        .frame
        .descriptors
        .first()
        .ok_or_else(|| "portable Palace DVR has no layer descriptor".to_owned())?
        .transform;
    // Half the smallest physical voxel spacing is conservative for an anisotropic admitted
    // level and matches the bounded scene recorder's no-skip sampling rule.
    let step_size = transform
        .scale
        .into_iter()
        .map(|value| value.abs() as f32 * 0.5)
        .reduce(f32::min)
        .filter(|value| value.is_finite() && *value > 0.0)
        .ok_or_else(|| "portable Palace DVR layer spacing is invalid".to_owned())?;
    let page_input = level
        .raymarch_input(
            input.draw.extent_pixels[0],
            input.draw.extent_pixels[1],
            rays,
            step_size,
        )
        .ok_or_else(|| "portable Palace DVR raymarch input is not admitted".to_owned())?;
    let transfer = palace_transfer_from_native_camera(input)?;
    let cpu = page_input
        .render_cpu(&transfer)
        .ok_or_else(|| "portable Palace DVR CPU oracle rejected its admitted packet".to_owned())?;
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        return Ok(cpu);
    };
    let Ok((device, queue)) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        ..Default::default()
    })) else {
        return Ok(cpu);
    };
    Ok(palace_wgpu::WgpuOperatorRecorder::new(&device, &queue)
        .record_dvr_page_frame(&transfer, &page_input)
        .unwrap_or(cpu))
}

fn palace_slice_words_from_admitted_volume(
    volume: &newvolim_render::NativePortableVolumeInput,
    axis: u32,
    index: u32,
) -> Result<Vec<u32>, String> {
    let (layout, pages) = palace_slice_layout_and_pages(volume, axis, index)?;
    layout
        .slice_page_words(&pages)
        .ok_or_else(|| "portable Palace slice extraction failed".to_owned())
}

fn palace_slice_layout_and_pages(
    volume: &newvolim_render::NativePortableVolumeInput,
    axis: u32,
    index: u32,
) -> Result<
    (
        palace_core::gpu::PortableOrthogonalSliceLayout,
        Vec<palace_core::gpu::PortableTensorPage>,
    ),
    String,
> {
    palace_slice_layout_and_channel_pages(volume, 0, axis, index)
}

fn palace_slice_layout_and_channel_pages(
    volume: &newvolim_render::NativePortableVolumeInput,
    channel_index: usize,
    axis: u32,
    index: u32,
) -> Result<
    (
        palace_core::gpu::PortableOrthogonalSliceLayout,
        Vec<palace_core::gpu::PortableTensorPage>,
    ),
    String,
> {
    let channel = volume
        .channels
        .get(channel_index)
        .ok_or_else(|| "portable Palace slice channel is outside the admission".to_owned())?;
    if channel.page_count == 0 || channel.page_count > 4 {
        return Err("portable Palace slice page range is not admitted".into());
    }
    let layout = palace_core::gpu::PortableOrthogonalSliceLayout::new(
        volume.dimensions_xyz,
        axis,
        index,
        match axis {
            0 => [volume.dimensions_xyz[1], volume.dimensions_xyz[2]],
            1 => [volume.dimensions_xyz[0], volume.dimensions_xyz[2]],
            2 => [volume.dimensions_xyz[0], volume.dimensions_xyz[1]],
            _ => return Err("portable Palace slice axis is outside XYZ".into()),
        },
    )
    .ok_or_else(|| "portable Palace slice layout is not admitted".to_owned())?;
    let first = channel.page_offset as usize;
    let end = first
        .checked_add(channel.page_count as usize)
        .ok_or_else(|| "portable Palace slice page range overflows usize".to_owned())?;
    let pages = volume
        .frame
        .page_submission
        .pages
        .get(first..end)
        .ok_or_else(|| "portable Palace slice page range is outside the admission".to_owned())?
        .iter()
        .enumerate()
        .map(|(index, words)| {
            palace_core::gpu::PortableTensorPage::new(index as u64 + 1, words.clone())
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| "portable Palace slice page is not admitted".to_owned())?;
    Ok((layout, pages))
}

/// Prefer the fixed-binding WGPU recorder for an already admitted pane. Device discovery and
/// recording are intentionally best-effort here: the CPU oracle is the portable fallback for a
/// headless host or an adapter that declines this small dispatch.
fn palace_slice_words_from_admitted_volume_with_local_wgpu(
    volume: &newvolim_render::NativePortableVolumeInput,
    axis: u32,
    index: u32,
) -> Result<Vec<u32>, String> {
    palace_slice_words_from_channel_with_local_wgpu(volume, 0, axis, index)
}

fn palace_slice_words_from_channel_with_local_wgpu(
    volume: &newvolim_render::NativePortableVolumeInput,
    channel_index: usize,
    axis: u32,
    index: u32,
) -> Result<Vec<u32>, String> {
    let (layout, pages) =
        palace_slice_layout_and_channel_pages(volume, channel_index, axis, index)?;
    let cpu = layout
        .slice_page_words(&pages)
        .ok_or_else(|| "portable Palace slice extraction failed".to_owned())?;
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        return Ok(cpu);
    };
    let Ok((device, queue)) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        ..Default::default()
    })) else {
        return Ok(cpu);
    };
    Ok(palace_wgpu::WgpuOperatorRecorder::new(&device, &queue)
        .record_orthogonal_slice_pages(&layout, &pages)
        .unwrap_or(cpu))
}

fn palace_slice_payload(
    volume: &newvolim_render::NativePortableVolumeInput,
    axis: u32,
    index: u32,
) -> Result<FramePayload, String> {
    let [width, height] = match axis {
        0 => [volume.dimensions_xyz[1], volume.dimensions_xyz[2]],
        1 => [volume.dimensions_xyz[0], volume.dimensions_xyz[2]],
        2 => [volume.dimensions_xyz[0], volume.dimensions_xyz[1]],
        _ => return Err("portable Palace slice axis is outside XYZ".into()),
    };
    let rgba = if volume.channels.len() == 1 {
        let words = palace_slice_words_from_admitted_volume_with_local_wgpu(volume, axis, index)?;
        let transfer = palace_transfer_from_native_volume(volume)?;
        words
            .into_iter()
            .flat_map(|word| transfer.classify(word as f32))
            .collect::<Vec<_>>()
    } else {
        let channel_words = (0..volume.channels.len())
            .map(|channel| {
                palace_slice_words_from_channel_with_local_wgpu(volume, channel, axis, index)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let transfers = volume
            .channels
            .iter()
            .map(|channel| channel.transfer)
            .collect::<Vec<_>>();
        (0..channel_words[0].len())
            .flat_map(|pixel| {
                let samples = channel_words
                    .iter()
                    .map(|words| words[pixel] as f64)
                    .collect::<Vec<_>>();
                let linear =
                    newvolim_render::composite_portable_scene_samples(&[(&transfers, &samples)])
                        .expect("matching channel slices");
                portable_linear_premultiplied_to_srgb8(linear)
            })
            .collect::<Vec<_>>()
    };
    let frame =
        palace_png::RgbaFrame::new(width, height, rgba).map_err(|error| error.to_string())?;
    Ok(FramePayload::png(
        width,
        height,
        palace_png::encode_rgba(&frame),
    ))
}

fn portable_linear_premultiplied_to_srgb8(linear: [f32; 4]) -> [u8; 4] {
    let alpha = linear[3].clamp(0.0, 1.0);
    let encode = |component: f32| {
        let straight = if alpha == 0.0 { 0.0 } else { component / alpha }.clamp(0.0, 1.0);
        let srgb = if straight <= 0.003_130_8 {
            straight * 12.92
        } else {
            1.055 * straight.powf(1.0 / 2.4) - 0.055
        };
        (srgb * 255.0).round() as u8
    };
    [
        encode(linear[0]),
        encode(linear[1]),
        encode(linear[2]),
        (alpha * 255.0).round() as u8,
    ]
}

/// Compose one portable scene plane in the first layer's physical voxel-center plane. Every
/// other axis-aligned layer is nearest-sampled at that same physical point; out-of-bounds points
/// are transparent, never extrapolated.
fn palace_scene_slice_rgba(
    scene: &newvolim_render::NativePortableSceneInput,
    axis: u32,
    index: u32,
) -> Result<(u32, u32, Vec<u8>), String> {
    palace_scene_slice_rgba_with_sampling(
        scene,
        axis,
        index,
        PortableSceneSliceSampling::FloorNearest,
    )
}

fn palace_scene_slice_rgba_with_sampling(
    scene: &newvolim_render::NativePortableSceneInput,
    axis: u32,
    index: u32,
    sampling: PortableSceneSliceSampling,
) -> Result<(u32, u32, Vec<u8>), String> {
    let first = scene
        .layers
        .first()
        .ok_or_else(|| "portable Palace scene slice has no layers".to_owned())?;
    let [width, height] = match axis {
        0 => [first.dimensions_xyz[1], first.dimensions_xyz[2]],
        1 => [first.dimensions_xyz[0], first.dimensions_xyz[2]],
        2 => [first.dimensions_xyz[0], first.dimensions_xyz[1]],
        _ => return Err("portable Palace scene slice axis is outside XYZ".into()),
    };
    let layers = scene
        .layers
        .iter()
        .map(|layer| {
            let volume = newvolim_render::NativePortableVolumeInput {
                frame: scene.frame.clone(),
                dimensions_xyz: layer.dimensions_xyz,
                scalar_type: layer.scalar_type,
                channels: layer.channels.clone(),
            };
            let pages = (0..layer.channels.len())
                .map(|channel| {
                    palace_slice_layout_and_channel_pages(&volume, channel, axis, index)
                        .map(|(_, pages)| pages)
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok((
                layer
                    .channels
                    .iter()
                    .map(|channel| channel.transfer)
                    .collect::<Vec<_>>(),
                pages,
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let pixels = usize::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(height as usize))
        .ok_or_else(|| "portable Palace scene slice dimensions overflow".to_owned())?;
    let rgba = (0..pixels)
        .flat_map(|pixel| {
            let horizontal = pixel % width as usize;
            let vertical = pixel / width as usize;
            let reference_coordinate = match axis {
                0 => [index as usize, vertical, horizontal],
                1 => [vertical, index as usize, horizontal],
                2 => [horizontal, vertical, index as usize],
                _ => unreachable!("validated slice axis"),
            };
            let physical: [f64; 3] = std::array::from_fn(|component| {
                first.transform.translation[component]
                    + (first.voxel_origin_xyz[component] as f64
                        + reference_coordinate[component] as f64
                        + 0.5)
                        * first.transform.scale[component]
            });
            let samples = scene
                .layers
                .iter()
                .zip(&layers)
                .map(|(layer, (_, pages))| {
                    let coordinate: [i64; 3] = std::array::from_fn(|component| match sampling {
                        PortableSceneSliceSampling::FloorNearest => {
                            ((physical[component] - layer.transform.translation[component])
                                / layer.transform.scale[component])
                                .floor() as i64
                                - layer.voxel_origin_xyz[component] as i64
                        }
                    });
                    pages
                        .iter()
                        .map(|pages| {
                            let [x, y, z] = coordinate;
                            if x < 0
                                || y < 0
                                || z < 0
                                || x as u32 >= layer.dimensions_xyz[0]
                                || y as u32 >= layer.dimensions_xyz[1]
                                || z as u32 >= layer.dimensions_xyz[2]
                            {
                                return f64::NAN;
                            }
                            let source = (z as usize * layer.dimensions_xyz[1] as usize
                                + y as usize)
                                * layer.dimensions_xyz[0] as usize
                                + x as usize;
                            let mut remaining = source;
                            for page in pages {
                                if remaining < page.words().len() {
                                    return page.words()[remaining] as f64;
                                }
                                remaining -= page.words().len();
                            }
                            f64::NAN
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            let inputs = layers
                .iter()
                .zip(&samples)
                .map(|((transfers, _), samples)| (transfers.as_slice(), samples.as_slice()))
                .collect::<Vec<_>>();
            portable_linear_premultiplied_to_srgb8(
                newvolim_render::composite_portable_scene_samples(&inputs)
                    .expect("matching scene channel samples"),
            )
        })
        .collect();
    Ok((width, height, rgba))
}

fn palace_transfer_from_native_volume(
    volume: &newvolim_render::NativePortableVolumeInput,
) -> Result<palace_core::gpu::PortableTransferFunction, String> {
    let [channel] = volume.channels.as_slice() else {
        return Err("portable Palace slice currently requires one admitted channel".into());
    };
    let min = channel.transfer.window_start as f32;
    let max = channel.transfer.window_end as f32;
    if !min.is_finite() || !max.is_finite() || max <= min {
        return Err("portable Palace slice requires a finite non-empty transfer window".into());
    }
    let entries = (0..256)
        .map(|index| {
            let alpha = ((index as f32 / 255.0) * channel.transfer.opacity * 255.0) as u8;
            [
                channel.transfer.color_srgb[0],
                channel.transfer.color_srgb[1],
                channel.transfer.color_srgb[2],
                alpha,
            ]
        })
        .collect();
    palace_core::gpu::PortableTransferFunction::new(min, max, entries)
        .ok_or_else(|| "portable Palace slice transfer is invalid".to_owned())
}

fn palace_slice_aspect_ratios(volume: &newvolim_render::NativePortableVolumeInput) -> [f64; 3] {
    let Some(descriptor) = volume.frame.descriptors.first() else {
        return [1.0; 3];
    };
    let scale = descriptor.transform.scale.map(f64::abs);
    let extent: [f64; 3] =
        std::array::from_fn(|axis| scale[axis] * volume.dimensions_xyz[axis] as f64);
    let ratio = |horizontal: usize, vertical: usize| {
        (extent[horizontal].is_finite()
            && extent[vertical].is_finite()
            && extent[horizontal] > 0.0
            && extent[vertical] > 0.0)
            .then_some(extent[horizontal] / extent[vertical])
            .filter(|value| value.is_finite() && *value > 0.0)
            .unwrap_or(1.0)
    };
    [ratio(0, 1), ratio(0, 2), ratio(1, 2)]
}

/// Render the three linked panes directly from the crosshair-selected bounded portable page
/// admission. Inputs outside the selected resident box return an error so the caller can retain
/// the established full-volume Palace route instead of rendering a misleading slice.
#[tauri::command]
fn render_native_portable_orthogonal(
    request: NativePortableDrawRequest,
    x: u32,
    y: u32,
    z: u32,
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<OrthogonalPayload, String> {
    let (draw, voxel_origin_xyz) = {
        let session = session
            .lock()
            .map_err(|_| "desktop session lock was poisoned".to_owned())?;
        let (draw, _, origin) = native_portable_draw_for_session(request, &session)?;
        (draw, origin)
    };
    let local = std::array::from_fn(|axis| {
        u64::from([x, y, z][axis])
            .checked_sub(voxel_origin_xyz[axis])
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| "crosshair is outside the admitted portable slice viewport".to_owned())
    });
    let [local_x, local_y, local_z] = local;
    let local = [local_x?, local_y?, local_z?];
    if local
        .iter()
        .zip(draw.volume.dimensions_xyz)
        .any(|(&coordinate, dimension)| coordinate >= dimension)
    {
        return Err("crosshair is outside the admitted portable slice viewport".into());
    }
    let volume = &draw.volume;
    Ok(OrthogonalPayload {
        xy: palace_slice_payload(volume, 2, local[2])?,
        xz: palace_slice_payload(volume, 1, local[1])?,
        yz: palace_slice_payload(volume, 0, local[0])?,
        aspect_ratios: palace_slice_aspect_ratios(volume),
        viewport_origin_xyz: Some(voxel_origin_xyz),
        viewport_dimensions_xyz: Some(volume.dimensions_xyz),
    })
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
    if packet.draw.annotation_words.is_empty() {
        match render_palace_portable_camera_draw(&packet) {
            Ok(frame) => FramePayload::portable_frame_attachments(frame),
            // A fitted Palace camera can legitimately exceed the bounded page-DVR packet's
            // current sample limit. Keep the established native route for that packet rather
            // than silently truncating its depth or rejecting an otherwise valid frame.
            Err(_) => {
                let frame = newvolim_wgpu_frame::render_portable_camera_draw(&packet, 0)?;
                FramePayload::native_wgpu_camera(width, height, frame)
            }
        }
    } else {
        // Palace's page-DVR owns the direct-volume colour/depth route. Until its overlay pass is
        // migrated, retain the existing recorder only when it must composite trusted projected
        // annotation records into the returned colour attachment.
        let frame = newvolim_wgpu_frame::render_portable_camera_draw(&packet, 0)?;
        FramePayload::native_wgpu_camera(width, height, frame)
    }
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

/// Render ordered scene layers as linked portable panes. The compositor nearest-samples each
/// axis-aligned layer at the reference layer's physical voxel centers; unrepresentable scene
/// admissions still return an error so callers can retain the Palace slice route.
#[tauri::command]
fn render_native_portable_scene_orthogonal(
    request: NativePortableDrawRequest,
    x: u32,
    y: u32,
    z: u32,
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<OrthogonalPayload, String> {
    let scene = {
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
                SpatialChunkRegion::new(request.origin_xyz, request.extent_xyz),
                4_096,
            )
            .map_err(|error| error.to_string())?;
        let loaded = session
            .read_local_layer_chunks(&plans, 16 * 1024 * 1024, 64 * 1024 * 1024)
            .map_err(|error| error.to_string())?;
        session
            .native_portable_scene_page_admission(descriptors, &plans, &loaded)
            .map_err(|error| error.to_string())?
    };
    let first = scene
        .layers
        .first()
        .ok_or_else(|| "portable Palace scene slice has no layers".to_owned())?;
    let local = std::array::from_fn(|axis| {
        u64::from([x, y, z][axis])
            .checked_sub(first.voxel_origin_xyz[axis])
            .and_then(|value| u32::try_from(value).ok())
            .filter(|&value| value < first.dimensions_xyz[axis])
            .ok_or_else(|| "crosshair is outside the admitted portable scene viewport".to_owned())
    });
    let [local_x, local_y, local_z] = local;
    let local = [local_x?, local_y?, local_z?];
    let encode = |axis, index| {
        let (width, height, rgba) = palace_scene_slice_rgba(&scene, axis, index)?;
        let frame =
            palace_png::RgbaFrame::new(width, height, rgba).map_err(|error| error.to_string())?;
        Ok::<FramePayload, String>(FramePayload::png(
            width,
            height,
            palace_png::encode_rgba(&frame),
        ))
    };
    let volume = newvolim_render::NativePortableVolumeInput {
        frame: scene.frame.clone(),
        dimensions_xyz: first.dimensions_xyz,
        scalar_type: first.scalar_type,
        channels: first.channels.clone(),
    };
    Ok(OrthogonalPayload {
        xy: encode(2, local[2])?,
        xz: encode(1, local[1])?,
        yz: encode(0, local[0])?,
        aspect_ratios: palace_slice_aspect_ratios(&volume),
        viewport_origin_xyz: Some(first.voxel_origin_xyz),
        viewport_dimensions_xyz: Some(first.dimensions_xyz),
    })
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
        let packet = newvolim_render::NativePortableCameraDrawInput::new_with_voxel_origin(
            draw,
            voxel_origin_xyz,
            rays,
        )
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
    // Selection is volume-occluded by the same Palace page-DVR invocation that owns an admitted
    // direct frame. Its distances are physical units. A packet outside the current bounded DVR
    // sample limit takes the established native recorder fallback, whose local distance retains
    // the host-owned physical conversion captured with this exact camera.
    match render_palace_portable_camera_draw(&packet) {
        Ok(frame) => {
            let distance = *frame
                .first_opacity_distance
                .get(index)
                .ok_or_else(|| "portable recorder omitted the requested depth pixel".to_owned())?;
            depth_aware_annotation_pick(&annotations, physical_ray.ray, f64::from(distance))
        }
        Err(_) => {
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
    }
}

/// Pick an annotation against a newly rendered ordered portable scene. Scene DVR rays and
/// first-opacity distances are physical world units, so no single-layer voxel conversion or
/// browser-provided depth may enter this path.
#[tauri::command]
fn pick_native_portable_scene_annotation(
    request: NativePortableScenePickRequest,
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<Option<AnnotationPickPayload>, String> {
    let session = session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?;
    pick_native_portable_scene_annotation_for_session(request, &session)
}

fn pick_native_portable_scene_annotation_for_session(
    request: NativePortableScenePickRequest,
    session: &LocalSession,
) -> Result<Option<AnnotationPickPayload>, String> {
    let packet = native_portable_scene_camera_draw_for_session(request.draw, session)?;
    if request.x >= packet.draw.extent_pixels[0] || request.y >= packet.draw.extent_pixels[1] {
        return Err("portable scene annotation-pick pixel is outside the admitted extent".into());
    }
    let index = usize::try_from(request.y)
        .ok()
        .and_then(|row| row.checked_mul(packet.draw.extent_pixels[0] as usize))
        .and_then(|row| row.checked_add(request.x as usize))
        .ok_or_else(|| "portable scene annotation-pick pixel offset overflows usize".to_owned())?;
    let ray = *packet
        .rays
        .get(index)
        .ok_or_else(|| "portable scene camera packet omitted the requested pixel ray".to_owned())?;
    let physical_ray = newvolim_render::PickRay::new(ray.origin_world, ray.direction_world)
        .map_err(|error| error.to_string())?;
    let frame = newvolim_wgpu_frame::render_portable_scene_camera_draw(&packet, 0)?;
    let distance = *frame
        .ray_distances
        .get(index)
        .ok_or_else(|| "portable scene recorder omitted the requested depth pixel".to_owned())?;
    depth_aware_annotation_pick(session.annotations(), physical_ray, f64::from(distance))
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
            let raw_direction = std::array::from_fn(|axis| {
                f64::from(ray.direction_xyz[axis]) * layer.transform.scale[axis]
            });
            let length = raw_direction
                .iter()
                .map(|component| component * component)
                .sum::<f64>()
                .sqrt();
            let direction = raw_direction.map(|component| component / length);
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
        aspect_ratios: [1.0; 3],
        viewport_origin_xyz: None,
        viewport_dimensions_xyz: None,
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
        aspect_ratios: [1.0; 3],
        viewport_origin_xyz: None,
        viewport_dimensions_xyz: None,
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
    let (root, aspect_ratios) = {
        let session = session
            .lock()
            .map_err(|_| "desktop session lock was poisoned".to_owned())?;
        (
            session.dataset_root().ok_or_else(|| {
                "open a local OME-Zarr dataset before requesting slices".to_owned()
            })?,
            session.orthogonal_physical_aspect_ratios(),
        )
    };
    let size = desktop_frame_size(width, height, 3)?;
    let [xy, xz, yz] =
        render_local_zarr_orthogonal_png(root, size).map_err(|error| error.to_string())?;
    Ok(OrthogonalPayload {
        xy: FramePayload::png(width, height, xy),
        xz: FramePayload::png(width, height, xz),
        yz: FramePayload::png(width, height, yz),
        aspect_ratios,
        viewport_origin_xyz: None,
        viewport_dimensions_xyz: None,
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
    let (root, crosshair_xyz, aspect_ratios) = {
        let session = session
            .lock()
            .map_err(|_| "desktop session lock was poisoned".to_owned())?;
        let root = session
            .dataset_root()
            .ok_or_else(|| "open a local OME-Zarr dataset before requesting slices".to_owned())?;
        let crosshair_xyz = session
            .clamp_crosshair_xyz([x, y, z])
            .map_err(|error| error.to_string())?;
        (
            root,
            crosshair_xyz,
            session.orthogonal_physical_aspect_ratios(),
        )
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
        aspect_ratios,
        viewport_origin_xyz: None,
        viewport_dimensions_xyz: None,
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
            render_native_portable_orthogonal,
            render_native_portable_scene_camera_draw,
            render_native_portable_scene_orthogonal,
            pick_native_portable_annotation,
            pick_native_portable_scene_annotation,
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
    fn synthetic_orthogonal_payload_keeps_square_physical_panes() {
        let payload = render_synthetic_orthogonal_at(8, 8, 4, 4, 4).unwrap();
        assert_eq!(payload.aspect_ratios, [1.0; 3]);
        let json = serde_json::to_value(payload).unwrap();
        assert_eq!(json["aspectRatios"], serde_json::json!([1.0, 1.0, 1.0]));
    }

    #[test]
    fn portable_frame_payload_preserves_its_paired_depth_sidecar() {
        let payload = FramePayload::portable_frame_attachments(
            palace_core::gpu::PortableFrameAttachments::new(
                2,
                1,
                vec![1, 2, 3, 255, 4, 5, 6, 255],
                vec![0.25, f32::INFINITY],
            )
            .unwrap(),
        )
        .unwrap();

        assert_eq!(payload.target.depth, DepthAttachment::RayDistanceF32);
        assert_eq!(
            STANDARD
                .decode(payload.ray_distance_pfm_base64.unwrap())
                .unwrap(),
            b"Pf\n2 1\n-1.0\n\0\0\x80>\0\0\x80\x7f"
        );
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
    fn fitted_native_camera_and_page_submission_adapt_to_palace_dvr() {
        let submission = newvolim_render::PortablePageSubmission::from_uploads([
            newvolim_render::PortablePageUpload {
                page: 0,
                words: vec![0, 1],
            },
        ])
        .unwrap();
        let frame = newvolim_render::NativePortableFrameInput::new(
            vec![newvolim_render::NativeLayerDescriptor {
                layer_id: newvolim_scene::LayerId(1),
                page_offset: 0,
                page_count: 1,
                transform: newvolim_scene::LayerTransform::IDENTITY,
            }],
            submission,
        )
        .unwrap();
        let volume = newvolim_render::NativePortableVolumeInput::new(
            frame,
            [2, 1, 1],
            newvolim_render::PortableScalarType::Uint16,
            newvolim_render::PortableChannelTransfer {
                color_srgb: [255, 0, 0],
                window_start: 0.0,
                window_end: 1.0,
                opacity: 1.0,
            },
        )
        .unwrap();
        let draw = newvolim_render::NativePortableDrawInput::new(
            volume,
            [1, 1],
            newvolim_render::PortableCameraControls::new([0, 0], 1.0).unwrap(),
            vec![],
        )
        .unwrap();
        let camera = newvolim_render::NativePortableCameraDrawInput::new(
            draw,
            vec![newvolim_render::PortableCameraRay {
                origin_xyz: [-1.0, 0.5, 0.5],
                direction_xyz: [1.0, 0.0, 0.0],
            }],
        )
        .unwrap();
        assert_eq!(
            palace_slice_words_from_admitted_volume(&camera.draw.volume, 2, 0).unwrap(),
            vec![0, 1]
        );
        let (level, rays) = palace_dvr_packet_from_native_camera(&camera).unwrap();
        let input = level.raymarch_input(1, 1, rays, 1.0).unwrap();
        let transfer = palace_transfer_from_native_camera(&camera).unwrap();
        assert_eq!(transfer.classify(0.0), [255, 0, 0, 0]);
        assert_eq!(transfer.classify(1.0), [255, 0, 0, 255]);
        assert_eq!(
            input.render_cpu(&transfer).unwrap().first_opacity_distance,
            [2.0]
        );
        let resident_chunk_camera =
            newvolim_render::NativePortableCameraDrawInput::new_with_voxel_origin(
                camera.draw.clone(),
                [7, 0, 0],
                camera.rays.clone(),
            )
            .unwrap();
        let (_, rays) = palace_dvr_packet_from_native_camera(&resident_chunk_camera).unwrap();
        assert_eq!(rays[0].point_at(0.0), Some([6.0, 0.5, 0.5]));
        let (level, rays) = palace_dvr_packet_from_native_camera(&resident_chunk_camera).unwrap();
        let input = level.raymarch_input(1, 1, rays, 1.0).unwrap();
        assert_eq!(
            input.render_cpu(&transfer).unwrap().first_opacity_distance,
            [2.0]
        );
        let mut transformed = camera.clone();
        transformed.draw.volume.frame.descriptors[0].transform =
            newvolim_scene::LayerTransform::new([2.0, 1.0, 1.0], [0.0; 3]).unwrap();
        let (level, rays) = palace_dvr_packet_from_native_camera(&transformed).unwrap();
        let input = level.raymarch_input(1, 1, rays, 1.0).unwrap();
        assert_eq!(
            input.render_cpu(&transfer).unwrap().first_opacity_distance,
            [4.0]
        );
    }

    #[test]
    fn palace_slice_adapter_reads_an_admitted_four_page_channel_in_order() {
        let transfer = newvolim_render::PortableChannelTransfer {
            color_srgb: [255, 255, 255],
            window_start: 0.0,
            window_end: 1.0,
            opacity: 1.0,
        };
        let volume = newvolim_render::NativePortableVolumeInput {
            frame: newvolim_render::NativePortableFrameInput {
                descriptors: vec![],
                page_submission: newvolim_render::PortablePageSubmission {
                    pages: [vec![0], vec![1], vec![2], vec![3]],
                },
            },
            dimensions_xyz: [2, 2, 1],
            scalar_type: newvolim_render::PortableScalarType::Uint16,
            channels: vec![newvolim_render::PortableVolumeChannel {
                page_offset: 0,
                page_count: 4,
                transfer,
            }],
        };

        assert_eq!(
            palace_slice_words_from_admitted_volume(&volume, 2, 0).unwrap(),
            vec![0, 1, 2, 3]
        );
        assert_eq!(
            palace_slice_words_from_admitted_volume_with_local_wgpu(&volume, 2, 0).unwrap(),
            vec![0, 1, 2, 3]
        );
        let payload = palace_slice_payload(&volume, 2, 0).unwrap();
        assert_eq!((payload.width, payload.height), (2, 2));
        assert!(payload.data_url.starts_with("data:image/png;base64,"));
    }

    #[test]
    fn portable_ball_fixture_matches_palaces_linked_orthogonal_panes() {
        const EDGE: u32 = 32;
        let words = (0..EDGE)
            .flat_map(|z| {
                (0..EDGE).flat_map(move |y| {
                    (0..EDGE).map(move |x| {
                        let centered =
                            [x, y, z].map(|coordinate| coordinate as f32 / EDGE as f32 - 0.5);
                        let distance = centered
                            .iter()
                            .map(|value| value * value)
                            .sum::<f32>()
                            .sqrt();
                        (10.0 * (0.5 - distance))
                            .clamp(0.0, 1.0)
                            .mul_add(65535.0, 0.0) as u32
                    })
                })
            })
            .collect::<Vec<_>>();
        let volume = newvolim_render::NativePortableVolumeInput {
            frame: newvolim_render::NativePortableFrameInput {
                descriptors: vec![],
                page_submission: newvolim_render::PortablePageSubmission {
                    pages: [words, vec![], vec![], vec![]],
                },
            },
            dimensions_xyz: [EDGE, EDGE, EDGE],
            scalar_type: newvolim_render::PortableScalarType::Uint16,
            channels: vec![newvolim_render::PortableVolumeChannel {
                page_offset: 0,
                page_count: 1,
                transfer: newvolim_render::PortableChannelTransfer {
                    color_srgb: [255, 255, 255],
                    window_start: 0.0,
                    window_end: 65535.0,
                    opacity: 1.0,
                },
            }],
        };
        let transfer = palace_core::gpu::PortableTransferFunction::new(
            0.0,
            65535.0,
            // Palace's `grey_ramp` slice fixture uses the ramp intensity for both RGB and
            // alpha, preserving transparent empty pixels without a separate compositing pass.
            (0..=255)
                .map(|value| [value, value, value, value])
                .collect(),
        )
        .unwrap();
        let portable = [0, 1, 2].map(|axis| {
            palace_slice_words_from_admitted_volume(&volume, axis, EDGE / 2)
                .unwrap()
                .into_iter()
                .flat_map(|word| transfer.classify(word as f32))
                .collect::<Vec<_>>()
        });
        let palace = palace_frame::render_synthetic_orthogonal_at_rgba(
            EDGE,
            FrameSize::new(EDGE, EDGE).unwrap(),
            [EDGE / 2; 3],
        )
        .unwrap();
        for (portable, palace) in portable.into_iter().zip(palace) {
            assert_eq!(portable.len(), palace.pixels().len());
            let errors = portable
                .iter()
                .copied()
                .zip(palace.pixels())
                .enumerate()
                .fold([0_u8; 4], |mut errors, (index, (left, right))| {
                    errors[index % 4] = errors[index % 4].max(left.abs_diff(*right));
                    errors
                });
            assert!(
                errors.iter().all(|&error| error <= 2),
                "portable/PALACE RGBA errors: {errors:?}; first portable {:?}, Palace {:?}",
                &portable[..4],
                &palace.pixels()[..4],
            );
        }

        let red_transfer = palace_core::gpu::PortableTransferFunction::new(
            0.0,
            65535.0,
            (0..=255).map(|value| [value, 0, 0, value]).collect(),
        )
        .unwrap();
        let portable = [0, 1, 2].map(|axis| {
            palace_slice_words_from_admitted_volume(&volume, axis, EDGE / 2)
                .unwrap()
                .into_iter()
                .flat_map(|word| red_transfer.classify(word as f32))
                .collect::<Vec<_>>()
        });
        let palace = palace_frame::render_synthetic_orthogonal_red_ramp_at_rgba(
            EDGE,
            FrameSize::new(EDGE, EDGE).unwrap(),
            [EDGE / 2; 3],
        )
        .unwrap();
        for (portable, palace) in portable.into_iter().zip(palace) {
            let maximum_error = portable
                .into_iter()
                .zip(palace.pixels())
                .map(|(left, right)| left.abs_diff(*right))
                .max()
                .unwrap();
            assert!(
                maximum_error <= 1,
                "portable/Palace red-ramp fixture differed by {maximum_error}"
            );
        }

        let mut anisotropic = volume.clone();
        anisotropic.frame.descriptors = vec![newvolim_render::NativeLayerDescriptor {
            layer_id: newvolim_scene::LayerId(1),
            page_offset: 0,
            page_count: 1,
            transform: newvolim_scene::LayerTransform::new([0.26, 0.26, 0.29], [0.0; 3]).unwrap(),
        }];
        let [xy, xz, yz] = palace_slice_aspect_ratios(&anisotropic);
        assert!((xy - 1.0).abs() < 1e-12);
        assert!((xz - (0.26 / 0.29)).abs() < 1e-12);
        assert!((yz - (0.26 / 0.29)).abs() < 1e-12);
    }

    #[test]
    fn declared_channel_window_colour_and_opacity_map_to_the_portable_slice_lut() {
        let volume = newvolim_render::NativePortableVolumeInput {
            frame: newvolim_render::NativePortableFrameInput {
                descriptors: vec![],
                page_submission: newvolim_render::PortablePageSubmission {
                    pages: [vec![10], vec![], vec![], vec![]],
                },
            },
            dimensions_xyz: [1, 1, 1],
            scalar_type: newvolim_render::PortableScalarType::Uint16,
            channels: vec![newvolim_render::PortableVolumeChannel {
                page_offset: 0,
                page_count: 1,
                transfer: newvolim_render::PortableChannelTransfer {
                    color_srgb: [255, 32, 64],
                    window_start: 10.0,
                    window_end: 20.0,
                    opacity: 0.5,
                },
            }],
        };
        let transfer = palace_transfer_from_native_volume(&volume).unwrap();
        assert_eq!(transfer.classify(10.0), [255, 32, 64, 0]);
        assert_eq!(transfer.classify(15.0), [255, 32, 64, 64]);
        assert_eq!(transfer.classify(20.0), [255, 32, 64, 127]);
    }

    #[test]
    fn ordered_multi_channel_slice_composites_its_distinct_page_ranges() {
        let transfer = |color_srgb| newvolim_render::PortableChannelTransfer {
            color_srgb,
            window_start: 0.0,
            window_end: 1.0,
            opacity: 0.5,
        };
        let volume = newvolim_render::NativePortableVolumeInput {
            frame: newvolim_render::NativePortableFrameInput {
                descriptors: vec![],
                page_submission: newvolim_render::PortablePageSubmission {
                    pages: [vec![1], vec![1], vec![], vec![]],
                },
            },
            dimensions_xyz: [1, 1, 1],
            scalar_type: newvolim_render::PortableScalarType::Uint16,
            channels: vec![
                newvolim_render::PortableVolumeChannel {
                    page_offset: 0,
                    page_count: 1,
                    transfer: transfer([255, 0, 0]),
                },
                newvolim_render::PortableVolumeChannel {
                    page_offset: 1,
                    page_count: 1,
                    transfer: transfer([0, 255, 0]),
                },
            ],
        };
        assert_eq!(
            palace_slice_words_from_channel_with_local_wgpu(&volume, 0, 2, 0).unwrap(),
            vec![1]
        );
        assert_eq!(
            palace_slice_words_from_channel_with_local_wgpu(&volume, 1, 2, 0).unwrap(),
            vec![1]
        );
        let linear = newvolim_render::composite_portable_scene_samples(&[(
            &[volume.channels[0].transfer, volume.channels[1].transfer],
            &[1.0, 1.0],
        )])
        .unwrap();
        assert_eq!(linear, [0.5, 0.5, 0.0, 1.0]);
        assert_eq!(
            portable_linear_premultiplied_to_srgb8(linear),
            [188, 188, 0, 255]
        );
        assert!(palace_slice_payload(&volume, 2, 0)
            .unwrap()
            .data_url
            .starts_with("data:image/png;base64,"));
    }

    #[test]
    fn co_registered_portable_scene_slice_composites_layers_in_declared_order() {
        let layer = |id, page, color_srgb| newvolim_render::PortableSceneLayerInput {
            layer_id: newvolim_scene::LayerId(id),
            transform: newvolim_scene::LayerTransform::IDENTITY,
            voxel_origin_xyz: [0; 3],
            dimensions_xyz: [1, 1, 1],
            scalar_type: newvolim_render::PortableScalarType::Uint16,
            channels: vec![newvolim_render::PortableVolumeChannel {
                page_offset: page,
                page_count: 1,
                transfer: newvolim_render::PortableChannelTransfer {
                    color_srgb,
                    window_start: 0.0,
                    window_end: 1.0,
                    opacity: 0.5,
                },
            }],
        };
        let scene = newvolim_render::NativePortableSceneInput {
            frame: newvolim_render::NativePortableFrameInput {
                descriptors: vec![],
                page_submission: newvolim_render::PortablePageSubmission {
                    pages: [vec![1], vec![1], vec![], vec![]],
                },
            },
            layers: vec![layer(1, 0, [255, 0, 0]), layer(2, 1, [0, 255, 0])],
        };
        assert_eq!(
            palace_scene_slice_rgba(&scene, 2, 0).unwrap(),
            // The shared oracle is premultiplied `[0.25, 0.5, 0.0, 0.75]`; PNG stores its
            // RGB components straight, after un-premultiplication and sRGB encoding.
            (1, 1, vec![156, 213, 0, 191])
        );
        let mut transformed = scene.clone();
        transformed.layers[1].transform =
            newvolim_scene::LayerTransform::new([2.0, 1.0, 1.0], [0.0; 3]).unwrap();
        assert_eq!(
            palace_scene_slice_rgba(&transformed, 2, 0).unwrap(),
            (1, 1, vec![156, 213, 0, 191])
        );
        transformed.layers[1].transform =
            newvolim_scene::LayerTransform::new([1.0; 3], [2.0, 0.0, 0.0]).unwrap();
        assert_eq!(
            palace_scene_slice_rgba(&transformed, 2, 0).unwrap(),
            (1, 1, vec![255, 0, 0, 128])
        );
    }

    #[test]
    fn transformed_portable_scene_slice_uses_floor_nearest_physical_sampling() {
        let layer = |id, page, color_srgb| newvolim_render::PortableSceneLayerInput {
            layer_id: newvolim_scene::LayerId(id),
            transform: newvolim_scene::LayerTransform::IDENTITY,
            voxel_origin_xyz: [0; 3],
            dimensions_xyz: [1, 1, 1],
            scalar_type: newvolim_render::PortableScalarType::Uint16,
            channels: vec![newvolim_render::PortableVolumeChannel {
                page_offset: page,
                page_count: 1,
                transfer: newvolim_render::PortableChannelTransfer {
                    color_srgb,
                    window_start: 0.0,
                    window_end: 1.0,
                    opacity: 0.5,
                },
            }],
        };
        let mut scene = newvolim_render::NativePortableSceneInput {
            frame: newvolim_render::NativePortableFrameInput {
                descriptors: vec![],
                // The second layer's first target voxel is green and its second is transparent.
                // At physical x=0.5 with target scale=0.6, floor(0.5 / 0.6) selects voxel 0;
                // a round-to-nearest implementation would incorrectly select voxel 1.
                page_submission: newvolim_render::PortablePageSubmission {
                    pages: [vec![1], vec![1, 0], vec![], vec![]],
                },
            },
            layers: vec![layer(1, 0, [255, 0, 0]), layer(2, 1, [0, 255, 0])],
        };
        scene.layers[1].dimensions_xyz = [2, 1, 1];
        scene.layers[1].transform =
            newvolim_scene::LayerTransform::new([0.6, 1.0, 1.0], [0.0; 3]).unwrap();
        assert_eq!(
            palace_scene_slice_rgba(&scene, 2, 0).unwrap(),
            (1, 1, vec![156, 213, 0, 191])
        );

        // Translating the target's voxel-0 boundary onto the reference centre still selects its
        // first voxel. This fixes the convention at an exact transformed boundary.
        scene.layers[1].transform =
            newvolim_scene::LayerTransform::new([0.6, 1.0, 1.0], [0.5, 0.0, 0.0]).unwrap();
        assert_eq!(
            palace_scene_slice_rgba(&scene, 2, 0).unwrap(),
            (1, 1, vec![156, 213, 0, 191])
        );
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
    #[ignore = "requires a local WGPU adapter"]
    fn native_portable_scene_picker_uses_ordered_scene_renderer_depth() {
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
        let result = pick_native_portable_scene_annotation_for_session(
            NativePortableScenePickRequest {
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
