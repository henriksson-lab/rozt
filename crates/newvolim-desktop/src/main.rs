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
    Ok(
        project_session_annotation_records(session, root, size, controls)?
            .into_iter()
            .flat_map(|record| record.words())
            .collect(),
    )
}

/// Convert the same projected records into Palace's annotation primitives.
///
/// The desktop and Palace records have the identical thirteen-word layout, but converting from
/// the typed record rather than re-parsing the word stream keeps one projection the single
/// source: a future layout change cannot silently desynchronize the two decoders.
fn palace_annotation_primitives(
    session: &LocalSession,
    root: &Path,
    size: FrameSize,
    controls: CameraControls,
) -> Result<Vec<palace_core::gpu::ProjectedAnnotationPrimitive>, String> {
    project_session_annotation_records(session, root, size, controls)?
        .into_iter()
        .map(|record| {
            let vertex = |index: usize| {
                let vertex = record.vertices[index];
                [vertex.pixel[0], vertex.pixel[1], vertex.ray_distance]
            };
            let id = u64::from(record.annotation_id);
            match record.kind {
                newvolim_render::PortableAnnotationPrimitiveKind::Point => {
                    palace_core::gpu::ProjectedAnnotationPrimitive::point(
                        id,
                        record.color_srgb,
                        record.radius,
                        vertex(0),
                    )
                }
                newvolim_render::PortableAnnotationPrimitiveKind::Segment => {
                    palace_core::gpu::ProjectedAnnotationPrimitive::segment(
                        id,
                        record.color_srgb,
                        record.radius,
                        vertex(0),
                        vertex(1),
                    )
                }
                newvolim_render::PortableAnnotationPrimitiveKind::Triangle => {
                    palace_core::gpu::ProjectedAnnotationPrimitive::triangle(
                        id,
                        record.color_srgb,
                        [vertex(0), vertex(1), vertex(2)],
                    )
                }
            }
            .ok_or_else(|| "projected annotation record is not admitted by Palace".to_owned())
        })
        .collect()
}

fn project_session_annotation_records(
    session: &LocalSession,
    root: &Path,
    size: FrameSize,
    controls: CameraControls,
) -> Result<Vec<newvolim_render::PortableAnnotationPrimitive>, String> {
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
    Ok(records)
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
    let session = {
        let session = session
            .lock()
            .map_err(|_| "desktop session lock was poisoned".to_owned())?;
        session.clone()
    };
    native_portable_camera_draw_for_session(request, &session)
}

fn native_portable_camera_draw_for_session(
    request: NativePortableDrawRequest,
    session: &LocalSession,
) -> Result<newvolim_render::NativePortableCameraDrawInput, String> {
    let (draw, root, voxel_origin_xyz) = native_portable_draw_for_session(request, session)?;
    let size = FrameSize::new(draw.extent_pixels[0], draw.extent_pixels[1])
        .map_err(|error| error.to_string())?;
    let controls = CameraControls {
        orbit_delta: draw.camera.orbit_delta,
        zoom: draw.camera.zoom,
    };
    let spacing = level_zero_spacing_xyz(session)?;
    let rays = portable_camera_rays_xyz(&root, size, controls, voxel_origin_xyz, spacing)?;
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
    session: &LocalSession,
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
    let opacity_reference = portable_opacity_reference(std::array::from_fn(|axis| {
        (level.maximum()[axis] - level.minimum()[axis]).abs()
    }))?;
    // The oracle is the fallback, not a preamble: this path has local-adapter parity, so rendering
    // every frame twice would be pure cost.
    if let Some((device, queue)) = session.portable_device() {
        if let Ok(frame) = palace_wgpu::WgpuOperatorRecorder::new(device, queue)
            .record_dvr_page_frame(&transfer, &page_input, opacity_reference)
        {
            return Ok(frame);
        }
    }
    page_input
        .render_cpu(&transfer, opacity_reference)
        .ok_or_else(|| "portable Palace DVR CPU oracle rejected its admitted packet".to_owned())
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
    session: &LocalSession,
    volume: &newvolim_render::NativePortableVolumeInput,
    axis: u32,
    index: u32,
) -> Result<Vec<u32>, String> {
    palace_slice_words_from_channel_with_local_wgpu(session, volume, 0, axis, index)
}

fn palace_slice_words_from_channel_with_local_wgpu(
    session: &LocalSession,
    volume: &newvolim_render::NativePortableVolumeInput,
    channel_index: usize,
    axis: u32,
    index: u32,
) -> Result<Vec<u32>, String> {
    let (layout, pages) =
        palace_slice_layout_and_channel_pages(volume, channel_index, axis, index)?;
    if let Some((device, queue)) = session.portable_device() {
        if let Ok(words) = palace_wgpu::WgpuOperatorRecorder::new(device, queue)
            .record_orthogonal_slice_pages(&layout, &pages)
        {
            return Ok(words);
        }
    }
    layout
        .slice_page_words(&pages)
        .ok_or_else(|| "portable Palace slice extraction failed".to_owned())
}

fn palace_slice_payload(
    session: &LocalSession,
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
        let words =
            palace_slice_words_from_admitted_volume_with_local_wgpu(session, volume, axis, index)?;
        let transfer = palace_transfer_from_native_volume(volume)?;
        words
            .into_iter()
            .flat_map(|word| transfer.classify(word as f32))
            .collect::<Vec<_>>()
    } else {
        let channel_words = (0..volume.channels.len())
            .map(|channel| {
                palace_slice_words_from_channel_with_local_wgpu(
                    session, volume, channel, axis, index,
                )
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
    // The portable slice routes share this session's WGPU device; clone the handle rather than
    // holding the session lock across rendering.
    let slice_session = {
        let session = session
            .lock()
            .map_err(|_| "desktop session lock was poisoned".to_owned())?;
        session.clone()
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
        xy: palace_slice_payload(&slice_session, volume, 2, local[2])?,
        xz: palace_slice_payload(&slice_session, volume, 1, local[1])?,
        yz: palace_slice_payload(&slice_session, volume, 0, local[0])?,
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
    let owned = {
        let session = session
            .lock()
            .map_err(|_| "desktop session lock was poisoned".to_owned())?;
        session.clone()
    };
    let packet = native_portable_camera_draw_for_session(request, &owned)?;
    if packet.draw.annotation_words.is_empty() {
        match render_palace_portable_camera_draw(&owned, &packet) {
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

/// Convert the already-admitted ordered scene and its one-authority world rays to Palace's
/// bounded physical scene contract.  Page owners are global static binding slots, never channel
/// ordinals, so a channel cannot accidentally read a neighbour's resident scalar range.
fn palace_dvr_scene_from_native_camera(
    input: &newvolim_render::NativePortableSceneCameraDrawInput,
) -> Result<palace_core::gpu::PortableDvrSceneFrameInput, String> {
    let scene = &input.draw.scene;
    let pages = &scene.frame.page_submission.pages;
    let mut layers = Vec::new();
    for layer in &scene.layers {
        let minimum = layer
            .transform
            .voxel_to_world(layer.voxel_origin_xyz.map(|value| value as f64))
            .map(|value| value as f32);
        let maximum = std::array::from_fn(|axis| {
            (layer.transform.translation[axis]
                + layer.transform.scale[axis]
                    * (layer.voxel_origin_xyz[axis] + u64::from(layer.dimensions_xyz[axis])) as f64)
                as f32
        });
        let mut channels = Vec::new();
        for channel in &layer.channels {
            let first = channel.page_offset as usize;
            let end = first
                .checked_add(channel.page_count as usize)
                .ok_or("scene page range overflows")?;
            let channel_pages = pages
                .get(first..end)
                .ok_or("scene page range is outside admission")?
                .iter()
                .enumerate()
                .map(|(offset, words)| {
                    palace_core::gpu::PortableTensorPage::new(
                        (first + offset + 1) as u64,
                        words.clone(),
                    )
                    .ok_or("scene page is not admitted")
                })
                .collect::<Result<Vec<_>, _>>()?;
            let volume = palace_core::gpu::PortableDvrVolumeLevel::new(
                layer.dimensions_xyz,
                minimum,
                maximum,
                channel_pages,
            )
            .ok_or("scene volume is invalid")?;
            let transfer = channel.transfer;
            let lo = transfer.window_start as f32;
            let hi = transfer.window_end as f32;
            if !lo.is_finite() || !hi.is_finite() || hi <= lo {
                return Err("scene transfer window is invalid".into());
            }
            let lut = (0..256)
                .map(|i| {
                    [
                        transfer.color_srgb[0],
                        transfer.color_srgb[1],
                        transfer.color_srgb[2],
                        ((i as f32 / 255.0) * transfer.opacity * 255.0) as u8,
                    ]
                })
                .collect();
            let transfer = palace_core::gpu::PortableTransferFunction::new(lo, hi, lut)
                .ok_or("scene transfer is invalid")?;
            channels.push(palace_core::gpu::PortableDvrSceneChannel::new(
                volume, transfer,
            ));
        }
        layers.push(
            palace_core::gpu::PortableDvrSceneLayer::new(channels)
                .ok_or("scene channels exceed bound")?,
        );
    }
    let scene_minimum = scene
        .layers
        .iter()
        .fold([f32::INFINITY; 3], |minimum, layer| {
            let origin = layer
                .transform
                .voxel_to_world(layer.voxel_origin_xyz.map(|value| value as f64));
            std::array::from_fn(|axis| minimum[axis].min(origin[axis] as f32))
        });
    let scene_maximum = scene
        .layers
        .iter()
        .fold([f32::NEG_INFINITY; 3], |maximum, layer| {
            let end = std::array::from_fn(|axis| {
                layer.voxel_origin_xyz[axis] + u64::from(layer.dimensions_xyz[axis])
            });
            let corner = layer
                .transform
                .voxel_to_world(end.map(|value| value as f64));
            std::array::from_fn(|axis| maximum[axis].max(corner[axis] as f32))
        });
    // A framed camera necessarily has pixels that miss the admitted scene.  Such a ray is
    // admitted as a degenerate `near == far` interval, which carries no samples and renders as a
    // transparent pixel with `+infinity` depth on both the core oracle and the WGPU shader.
    // Rejecting the packet instead would push almost every realistic camera back onto the native
    // renderer because one pixel missed.
    let rays = input
        .rays
        .iter()
        .map(|ray| {
            let origin = ray.origin_world.map(|v| v as f32);
            let direction = ray.direction_world.map(|v| v as f32);
            let ray = palace_core::gpu::PortableRayInterval::new(origin, direction, 0.0, f32::MAX)
                .ok_or("scene world ray is invalid")?;
            ray.clipped_to_aabb(scene_minimum, scene_maximum)
                .or_else(|| {
                    palace_core::gpu::PortableRayInterval::new(origin, direction, 0.0, 0.0)
                })
                .ok_or("scene world ray is not representable as a transparent interval")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let step = scene
        .layers
        .iter()
        .flat_map(|layer| layer.transform.scale)
        .map(|v| v.abs() as f32 * 0.5)
        .filter(|v| v.is_finite() && *v > 0.0)
        .fold(f32::INFINITY, f32::min);
    palace_core::gpu::PortableDvrSceneFrameInput::new(
        input.draw.extent_pixels[0],
        input.draw.extent_pixels[1],
        layers,
        rays,
        step,
        portable_opacity_reference(std::array::from_fn(|axis| {
            (scene_maximum[axis] - scene_minimum[axis]).abs()
        }))?,
    )
    .ok_or_else(|| "scene DVR packet is not admitted".into())
}

/// Render the trusted multi-layer scene packet and return the same bounded colour/depth payload
/// as the direct recorder route.
#[tauri::command]
fn render_native_portable_scene_camera_draw(
    request: NativePortableDrawRequest,
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<FramePayload, String> {
    let session = session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?;
    render_native_portable_scene_camera_draw_for_session(request, &session)
}

/// Palace now owns both passes of an ordered scene frame: the volume raymarch and the
/// depth-tested projected-annotation composite over its own first-opacity attachment. An
/// annotation-bearing packet is no longer a reason to leave the portable route. The native scene
/// renderer remains the fallback for a packet Palace cannot admit, a host without an eligible
/// adapter, or a projected record Palace rejects.
fn render_native_portable_scene_camera_draw_for_session(
    request: NativePortableDrawRequest,
    session: &LocalSession,
) -> Result<FramePayload, String> {
    let width = request.width;
    let height = request.height;
    // Renderer-driven demand first: the scene pass decides which chunks it needs, so the
    // webview-supplied chunk region is not consulted at all. Anything this route refuses — more
    // than one layer or channel, no eligible adapter, a working set beyond the portable bound —
    // falls back to the established region-based routes below.
    if let Ok(frame) = render_demand_driven_scene_camera_draw(session, request) {
        if let Ok(frame) = composite_palace_scene_annotations(session, request, frame) {
            return FramePayload::portable_frame_attachments(frame);
        }
    }
    let packet = native_portable_scene_camera_draw_for_session(request, session)?;
    if let Ok(scene) = palace_dvr_scene_from_native_camera(&packet) {
        if let Ok(frame) = render_palace_portable_scene_camera_draw(session, &scene) {
            if let Ok(frame) = composite_palace_scene_annotations(session, request, frame) {
                return FramePayload::portable_frame_attachments(frame);
            }
        }
    }
    let frame = newvolim_wgpu_frame::render_portable_scene_camera_draw(&packet, 0)?;
    FramePayload::native_wgpu_camera(width, height, frame)
}

/// Composite this session's projected annotations over a Palace-rendered scene frame.
///
/// The returned frame keeps the volume's first-opacity attachment untouched, so the picker and
/// any later pass still test against the surface the raymarch produced. An empty annotation set
/// is a no-op, which is why this is on the common path rather than behind a branch.
fn composite_palace_scene_annotations(
    session: &LocalSession,
    request: NativePortableDrawRequest,
    frame: palace_core::gpu::PortableFrameAttachments,
) -> Result<palace_core::gpu::PortableFrameAttachments, String> {
    let size = desktop_frame_size(request.width, request.height, 1)?;
    let controls = CameraControls {
        orbit_delta: [request.orbit_x, request.orbit_y],
        zoom: request.zoom,
    }
    .validate()
    .map_err(|error| error.to_string())?;
    let root = session
        .dataset_root()
        .ok_or_else(|| "open a local OME-Zarr dataset before compositing annotations".to_owned())?;
    let primitives = palace_annotation_primitives(session, &root, size, controls)?;
    let input = palace_core::gpu::PortableAnnotationCompositeInput::new(frame, primitives)
        .ok_or_else(|| "projected annotation packet exceeds the portable bound".to_owned())?;
    if input.primitives().is_empty() {
        return Ok(input.frame().clone());
    }
    if let Some(composited) = palace_annotation_composite_on_adapter(session, &input) {
        return Ok(composited);
    }
    input
        .composite_cpu()
        .ok_or_else(|| "portable annotation composite oracle rejected its admitted input".to_owned())
}

/// Acquire a local adapter and composite, returning `None` for every host-capability or recording
/// failure so the caller can fall back to the core oracle.
fn palace_annotation_composite_on_adapter(
    session: &LocalSession,
    input: &palace_core::gpu::PortableAnnotationCompositeInput,
) -> Option<palace_core::gpu::PortableFrameAttachments> {
    let (device, queue) = session.portable_device()?;
    palace_wgpu::WgpuOperatorRecorder::new(device, queue)
        .record_annotation_composite(input)
        .ok()
}

/// Render one scene frame with chunk demand coming from the **renderer** rather than the webview.
///
/// This is PLAN.md §9.3.1 item 4 on the desktop: no caller-supplied chunk region. The scene pass
/// runs against a residency map, records every chunk it misses, and the host plans, reads and
/// uploads exactly those chunks until the frame reports no misses. Occluded chunks are therefore
/// never read at all, which a region-based plan cannot express.
///
/// Restricted to a single admitted layer. A multi-layer demand render needs one residency loop per
/// layer sharing the four-page budget, and that planning is not designed yet; the caller keeps the
/// existing route for anything this refuses.
fn render_demand_driven_scene_camera_draw(
    session: &LocalSession,
    request: NativePortableDrawRequest,
) -> Result<palace_core::gpu::PortableFrameAttachments, String> {
    let size = desktop_frame_size(request.width, request.height, 1)?;
    let controls = CameraControls {
        orbit_delta: [request.orbit_x, request.orbit_y],
        zoom: request.zoom,
    }
    .validate()
    .map_err(|error| error.to_string())?;
    // Choose the level from the camera before touching any chunk. Level selection is a per-frame
    // decision and must not mutate the session: the source array and its physical transform are
    // pure functions of the dataset metadata and the level index, so the renderer asks for a level
    // and the user's scene is untouched.
    let level = demand_scene_level(session, size, controls)?;
    render_demand_driven_scene_camera_draw_at_level(session, request, level)
}

/// The demand route at an explicit pyramid level. The desktop always lets the camera choose (see
/// [`render_demand_driven_scene_camera_draw`]); a comparison against another renderer needs the
/// level pinned so that a level difference is not mistaken for a renderer difference.
fn render_demand_driven_scene_camera_draw_at_level(
    session: &LocalSession,
    request: NativePortableDrawRequest,
    level: u32,
) -> Result<palace_core::gpu::PortableFrameAttachments, String> {
    let size = desktop_frame_size(request.width, request.height, 1)?;
    let controls = CameraControls {
        orbit_delta: [request.orbit_x, request.orbit_y],
        zoom: request.zoom,
    }
    .validate()
    .map_err(|error| error.to_string())?;
    session
        .dataset_root()
        .ok_or_else(|| "open a local OME-Zarr dataset before rendering".to_owned())?;
    let limits = LayerRenderLimits::new(4, 4);
    // One probe plan yields the chosen level's source geometry, transform and channels without
    // committing to any particular chunk region.
    let probe = session
        .local_layer_chunk_plan_for_chunks_at_level(limits, level, &[[0, 0, 0]], 8)
        .map_err(|error| error.to_string())?;
    let [probe] = probe.as_slice() else {
        return Err("demand-driven scene rendering admits exactly one layer".into());
    };
    let channels = probe.request.layer.channels.clone();
    if channels.is_empty()
        || channels.len() > palace_core::gpu::PortableResidencyTag::MAX_CHANNELS as usize
    {
        return Err("demand-driven scene rendering admits one to four channels".into());
    }
    let source = &probe.request.source;
    let spatial: [usize; 3] = source.spatial_axes_xyz.map(|axis| axis as usize);
    let dimensions: [u32; 3] = std::array::from_fn(|axis| source.shape[spatial[axis]] as u32);
    let chunk_shape: [u32; 3] =
        std::array::from_fn(|axis| source.chunk_shape[spatial[axis]] as u32);
    let grid = palace_core::gpu::PortableChunkGrid::new(dimensions, chunk_shape)
        .ok_or("source chunk grid is not admitted")?;
    // Every demand-resident channel resolves through one page table, so each carries its own
    // residency tag: the same chunk index at the same pyramid level would otherwise collide
    // between channels and one would read the other's scalars.
    let tags = (0..channels.len())
        .map(|ordinal| {
            palace_core::gpu::PortableResidencyTag::compose(ordinal as u32, source.level)
                .ok_or_else(|| "source level is outside the portable residency key".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;

    // The layer occupies the same physical box at every level, so its AABB and the camera rays
    // are independent of which level is resident — but the *transform* must be the chosen level's
    // own, or a coarser array would be rendered at the finest level's extent.
    let transform = session
        .portable_level_transform(level)
        .map_err(|error| error.to_string())?;
    let minimum = transform
        .voxel_to_world([0.0; 3])
        .map(|value| value as f32);
    let maximum: [f32; 3] = std::array::from_fn(|axis| {
        (transform.translation[axis] + transform.scale[axis] * f64::from(dimensions[axis])) as f32
    });
    let rays = portable_demand_world_rays(dimensions, size, controls, transform, minimum, maximum)?;
    let transfers = channels
        .iter()
        .map(|channel| palace_transfer_from_channel_state(&channel.state))
        .collect::<Result<Vec<_>, _>>()?;

    // One residency loop per channel. Owner ranges are spaced so two channels' pages can never
    // alias, which `PortableDvrSceneFrameInput` rejects outright.
    let mut loops = tags
        .iter()
        .enumerate()
        .map(|(ordinal, tag)| {
            palace_core::gpu::PortableResidencyLoop::new(
                *tag,
                grid,
                1 + (ordinal as u64) * DEMAND_SCENE_OWNERS_PER_CHANNEL,
                4_096,
                16,
                DEMAND_SCENE_MAX_ITERATIONS,
            )
            .ok_or_else(|| "demand-driven residency loop could not be started".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;

    for _ in 0..=DEMAND_SCENE_MAX_ITERATIONS {
        // The four static page bindings are shared by every channel, so each channel's pages sit
        // at its own offset and the total is checked before anything is rendered.
        let mut table = palace_core::gpu::PortablePageTable::new(4_096, 16)
            .ok_or("demand-driven residency map could not be built")?;
        let mut scene_channels = Vec::with_capacity(loops.len());
        let mut bound_pages = 0_usize;
        for (ordinal, residency) in loops.iter().enumerate() {
            table
                .insert_plan(residency.plan())
                .ok_or("two demand-resident channels collided in one residency map")?;
            let pages = demand_scene_pages(
                session,
                limits,
                level,
                residency,
                channels[ordinal].source_index,
                1 + (ordinal as u64) * DEMAND_SCENE_OWNERS_PER_CHANNEL,
            )?;
            bound_pages += pages.len();
            if bound_pages > palace_core::gpu::PortableDvrPageFrameInput::MAX_PAGES {
                return Err(format!(
                    "demand-driven scene needs {bound_pages} static pages, beyond the portable bound"
                ));
            }
            let volume = palace_core::gpu::PortableDvrVolumeLevel::new_demand_resident(
                dimensions, minimum, maximum, pages,
            )
            .ok_or("demand-driven scene volume is invalid")?;
            scene_channels.push(palace_core::gpu::PortableDvrSceneChannel::with_residency(
                volume,
                transfers[ordinal].clone(),
                palace_core::gpu::PortableDvrSceneResidency::new(chunk_shape, tags[ordinal])
                    .ok_or("residency tag is outside the portable key")?,
            ));
        }
        let scene = palace_core::gpu::PortableDvrSceneFrameInput::new(
            size.width,
            size.height,
            vec![
                palace_core::gpu::PortableDvrSceneLayer::new(scene_channels)
                    .ok_or("demand-driven scene layer is invalid")?,
            ],
            rays.clone(),
            demand_scene_step_size(transform),
            portable_opacity_reference(std::array::from_fn(|axis| {
                (maximum[axis] - minimum[axis]).abs()
            }))?,
        )
        .ok_or("demand-driven scene packet is not admitted")?;
        let (frame, _, requests) = palace_demand_scene_on_adapter(session, &scene, &table)
            .ok_or("demand-driven scene render needs an eligible WGPU adapter")?;

        // Route each recorded key back to the channel whose tag it carries.
        let mut per_channel = vec![Vec::new(); loops.len()];
        for packed in requests.into_iter().filter(|word| *word != u32::MAX) {
            let key =
                palace_core::gpu::PortableFeedbackKey::new(packed & 0x00ff_ffff, packed >> 24)
                    .ok_or("scene shader recorded an invalid chunk key")?;
            let ordinal =
                palace_core::gpu::PortableResidencyTag::channel(key.level()) as usize;
            per_channel
                .get_mut(ordinal)
                .ok_or("scene shader recorded a key for an unadmitted channel")?
                .push(key);
        }
        // The frame is finished only when *every* channel reported no misses.
        let mut complete = true;
        for (residency, keys) in loops.iter_mut().zip(per_channel) {
            match residency
                .absorb(keys)
                .ok_or("scene shader demanded a chunk outside its own channel")?
            {
                palace_core::gpu::PortableResidencyStep::Complete => {}
                palace_core::gpu::PortableResidencyStep::Planned => complete = false,
                // A working set beyond the portable bound is a deterministic Vulkan fallback, and
                // an exhausted or desynchronized loop must not be presented as a finished frame.
                other => return Err(format!("demand-driven scene stopped: {other:?}")),
            }
        }
        if complete {
            return Ok(frame);
        }
    }
    Err("demand-driven scene exceeded its iteration budget".into())
}

/// Owner identifiers reserved per channel, so two channels' planned pages can never alias.
const DEMAND_SCENE_OWNERS_PER_CHANNEL: u64 = 1_000_000;

/// Choose the pyramid level this frame should sample.
///
/// The footprint is measured between two horizontally neighbouring pixel rays at the near plane
/// of the layer's physical box, which is the finest footprint the frame contains and therefore the
/// conservative choice: it never selects a level coarser than some visible pixel warrants.
fn demand_scene_level(
    session: &LocalSession,
    size: FrameSize,
    controls: CameraControls,
) -> Result<u32, String> {
    let spacings = session
        .portable_level_spacings()
        .map_err(|error| error.to_string())?;
    if spacings.len() == 1 {
        return Ok(0);
    }
    // Level zero's geometry fits the camera; the physical box is level-invariant, so this footprint
    // is the same whichever level ends up resident.
    let source = session
        .local_layer_render_requests_at_level(LayerRenderLimits::new(4, 4), 0)
        .map_err(|error| error.to_string())?;
    let [source] = source.as_slice() else {
        return Err("demand-driven level selection admits exactly one layer".into());
    };
    let source = &source.source;
    let spatial: [usize; 3] = source.spatial_axes_xyz.map(|axis| axis as usize);
    let dimensions_zyx = [
        source.shape[spatial[2]] as u32,
        source.shape[spatial[1]] as u32,
        source.shape[spatial[0]] as u32,
    ];
    let spacing_zyx = [spacings[0][2], spacings[0][1], spacings[0][0]];
    let centre = [size.width / 2, size.height / 2];
    if centre[0] + 1 >= size.width {
        return Ok(0);
    }
    // The physical box is level-invariant, so level zero's box is the box.
    let transform = session
        .portable_level_transform(0)
        .map_err(|error| error.to_string())?;
    let minimum = transform
        .voxel_to_world([0.0; 3])
        .map(|value| value as f32);
    let maximum: [f32; 3] = std::array::from_fn(|axis| {
        (transform.translation[axis]
            + transform.scale[axis] * f64::from(dimensions_zyx[2 - axis])) as f32
    });
    let ray = |x: u32| {
        demand_world_ray(
            dimensions_zyx,
            spacing_zyx,
            transform,
            size,
            controls,
            [x, centre[1]],
        )
    };
    // Unclipped, so both rays share one parameterization; the entry distance comes from clipping
    // the centre ray only.
    let first = ray(centre[0])?;
    let second = ray(centre[0] + 1)?;
    // Measure where the ray *enters the volume*. Two neighbouring rays are coincident at the eye,
    // so measuring near zero yields a zero footprint and would always select the finest level.
    let Some(entry) = first
        .clipped_to_aabb(minimum, maximum)
        .map(|clipped| clipped.near())
        .filter(|near| near.is_finite() && *near > 0.0)
    else {
        // The centre ray misses the layer, so there is nothing to size a level against.
        return Ok(0);
    };
    let footprint = palace_core::gpu::portable_pixel_footprint(first, second, entry)
        .filter(|value| value.is_finite() && *value > 0.0)
        .ok_or_else(|| "demand-driven pixel footprint is not measurable".to_owned())?;
    let direction = first.direction();
    let selected = palace_core::gpu::select_portable_level(&spacings, &[direction], footprint, 1.0)
        .ok_or_else(|| "demand-driven level selection rejected its input".to_owned())?;
    u32::try_from(selected).map_err(|_| "selected level does not fit the wire contract".to_owned())
}

/// Physical distance over which a transfer alpha is defined.
///
/// Palace's raycaster normalizes its step by the volume's physical diagonal before applying the
/// 256x reference correction (`norm_step = step / diag` in `raycaster.glsl`), so an alpha means
/// "this opacity per 1/256 of the diagonal". Passing a physical step straight into that
/// correction, as the portable path did, overstates the exponent by the whole diagonal — 48x on
/// the committed fixture — and saturates the image.
fn portable_opacity_reference(extent_physical: [f32; 3]) -> Result<f32, String> {
    let diagonal = extent_physical
        .iter()
        .map(|value| value * value)
        .sum::<f32>()
        .sqrt();
    (diagonal.is_finite() && diagonal > 0.0)
        .then_some(diagonal / 256.0)
        .ok_or_else(|| "layer has no measurable physical diagonal".to_owned())
}

/// Bounded re-render budget. Palace's Vulkan viewers degrade to a preview version rather than
/// spinning; this caller falls back to the established native route instead.
const DEMAND_SCENE_MAX_ITERATIONS: usize = 24;

/// Read and assemble whatever the loop has planned so far. Before anything is planned there is
/// nothing to bind, so one placeholder word stands in — no residency lookup can reach it.
fn demand_scene_pages(
    session: &LocalSession,
    limits: LayerRenderLimits,
    level: u32,
    residency: &palace_core::gpu::PortableResidencyLoop,
    channel: u32,
    owner_base: u64,
) -> Result<Vec<palace_core::gpu::PortableTensorPage>, String> {
    if residency.plan().chunks().is_empty() {
        // The placeholder still needs this channel's own owner: page owners must not alias across
        // the whole scene, so a shared placeholder owner makes a multi-channel bootstrap frame
        // unadmittable.
        return Ok(vec![
            palace_core::gpu::PortableTensorPage::new(owner_base, vec![0])
                .ok_or("placeholder page is not admitted")?,
        ]);
    }
    let counts = residency.plan().grid().counts_xyz();
    let chunks = residency
        .plan()
        .chunks()
        .iter()
        .map(|chunk| {
            let index = chunk.chunk_index;
            [
                u64::from(index % counts[0]),
                u64::from((index / counts[0]) % counts[1]),
                u64::from(index / (counts[0] * counts[1])),
            ]
        })
        .collect::<Vec<_>>();
    let plans = session
        .local_layer_chunk_plan_for_chunks_at_level(limits, level, &chunks, 4_096)
        .map_err(|error| error.to_string())?;
    let loaded = session
        .read_local_layer_chunks(&plans, 16 * 1024 * 1024, 256 * 1024 * 1024)
        .map_err(|error| error.to_string())?;
    let words = session
        .portable_chunk_plan_pages(&plans[0], &loaded, residency.plan(), channel)
        .map_err(|error| error.to_string())?;
    words
        .into_iter()
        .zip(residency.plan().page_owners())
        .map(|(words, owner)| {
            palace_core::gpu::PortableTensorPage::new(*owner, words)
                .ok_or_else(|| "planned page exceeds the portable page bound".to_owned())
        })
        .collect()
}

/// Physical step size: half the finest voxel spacing, matching the existing scene adapter.
fn demand_scene_step_size(transform: newvolim_scene::LayerTransform) -> f32 {
    transform
        .scale
        .iter()
        .map(|value| value.abs() as f32 * 0.5)
        .filter(|value| value.is_finite() && *value > 0.0)
        .fold(f32::INFINITY, f32::min)
}

fn palace_demand_scene_on_adapter(
    session: &LocalSession,
    scene: &palace_core::gpu::PortableDvrSceneFrameInput,
    page_table: &palace_core::gpu::PortablePageTable,
) -> Option<(palace_core::gpu::PortableFrameAttachments, Vec<u32>, Vec<u32>)> {
    let (device, queue) = session.portable_device()?;
    palace_wgpu::WgpuOperatorRecorder::new(device, queue)
        .record_dvr_scene_frame_with_residency(scene, Some(page_table), 4_096, 16)
        .ok()
}

/// Physical world rays for a level-independent layer box, at voxel origin zero.
///
/// Rays are clipped to the layer's physical AABB, and a ray that misses it becomes a degenerate
/// `near == far` interval — the same rule the region-based adapter uses. Without clipping, an
/// unbounded `far` would exceed the admitted sample count for every pixel and the whole packet
/// would be refused.
fn portable_demand_world_rays(
    dimensions_xyz: [u32; 3],
    size: FrameSize,
    controls: CameraControls,
    transform: newvolim_scene::LayerTransform,
    minimum: [f32; 3],
    maximum: [f32; 3],
) -> Result<Vec<palace_core::gpu::PortableRayInterval>, String> {
    // Derive the camera from the admitted layer's own geometry rather than by re-opening the
    // dataset. Palace's file-opening camera helper is three-dimensional, so a source with a
    // channel axis cannot be opened through it at all; the camera only needs ZYX dimensions and
    // spacing, and `camera_ray_for_geometry` is proven to fit the identical camera.
    //
    // The physical box is level-invariant, so an admitted coarser level's dimensions paired with
    // that level's spacing fit the same camera as level zero.
    let dimensions_zyx = [dimensions_xyz[2], dimensions_xyz[1], dimensions_xyz[0]];
    let spacing_zyx = [
        transform.scale[2].abs() as f32,
        transform.scale[1].abs() as f32,
        transform.scale[0].abs() as f32,
    ];
    let count = usize::try_from(size.width)
        .ok()
        .and_then(|width| width.checked_mul(size.height as usize))
        .ok_or_else(|| "demand-driven ray count overflows usize".to_owned())?;
    // One camera fit for the whole frame. Refitting per pixel was 62% of a 256x192 frame.
    let camera = palace_frame::camera_rays_for_geometry(dimensions_zyx, spacing_zyx, size, controls)
        .map_err(|error| error.to_string())?;
    if camera.len() != count {
        return Err("demand-driven camera produced the wrong ray count".into());
    }
    camera
        .into_iter()
        .map(|ray| {
            let ray = demand_world_ray_from_camera(ray, transform)?;
            demand_clipped_ray(ray, minimum, maximum)
        })
        .collect()
}

/// One physical world ray for a pixel, **unclipped**.
///
/// Shared by the frame's ray table and by level selection so the two cannot disagree about the
/// camera. Palace returns its ray in ZYX voxel coordinates, so the axis swap and the
/// voxel-to-world mapping both belong here rather than being repeated at each call site — getting
/// either wrong silently compares a ray in one space against a box in another.
///
/// Clipping is the caller's step, and deliberately so. Two neighbouring rays clipped to the same
/// box generally have *different* near distances, and `portable_pixel_footprint` measures both
/// rays at one shared distance; handing it independently clipped rays makes the measurement
/// unrepresentable for whichever ray enters later.
fn demand_world_ray(
    dimensions_zyx: [u32; 3],
    spacing_zyx: [f32; 3],
    transform: newvolim_scene::LayerTransform,
    size: FrameSize,
    controls: CameraControls,
    pixel: [u32; 2],
) -> Result<palace_core::gpu::PortableRayInterval, String> {
    let ray =
        palace_frame::camera_ray_for_geometry(dimensions_zyx, spacing_zyx, size, controls, pixel)
            .map_err(|error| error.to_string())?;
    demand_world_ray_from_camera(ray, transform)
}

/// The ZYX-voxel to XYZ-physical conversion, shared by the single-ray and whole-frame paths.
/// A fitted Palace camera ray as a physical world ray of the admitted layer.
///
/// Palace fits its camera in **physical** units: `CameraState::for_volume` places the eye 1.5
/// diagonals from the centre of `spacing × dimensions`, and the ray's direction is a unit vector
/// in that frame. The frame has voxel `i` centred at `i × spacing` and no translation, which is
/// exactly NGFF's `translation + i × scale` minus its translation — so the world ray is the
/// Palace ray plus the layer translation, and the scale must **not** be applied again. It was,
/// until the first real Palace depth surface exposed it: on the anisotropic fixture the eye was
/// scaled 0.26× into a corner of the box and every ray hit.
fn demand_world_ray_from_camera(
    ray: palace_frame::CameraRay,
    transform: newvolim_scene::LayerTransform,
) -> Result<palace_core::gpu::PortableRayInterval, String> {
    let origin_xyz = [ray.origin[2], ray.origin[1], ray.origin[0]];
    let direction = [ray.direction[2], ray.direction[1], ray.direction[0]];
    let origin = std::array::from_fn(|axis| {
        (f64::from(origin_xyz[axis]) + transform.translation[axis]) as f32
    });
    palace_core::gpu::PortableRayInterval::new(origin, direction, 0.0, f32::MAX)
        .ok_or_else(|| "demand-driven scene ray is invalid".to_owned())
}

/// Restrict a ray to the layer's box, or make it a degenerate transparent interval if it misses.
fn demand_clipped_ray(
    ray: palace_core::gpu::PortableRayInterval,
    minimum: [f32; 3],
    maximum: [f32; 3],
) -> Result<palace_core::gpu::PortableRayInterval, String> {
    ray.clipped_to_aabb(minimum, maximum)
        .or_else(|| {
            palace_core::gpu::PortableRayInterval::new(ray.origin(), ray.direction(), 0.0, 0.0)
        })
        .ok_or_else(|| {
            "demand-driven scene ray is not representable as a transparent interval".to_owned()
        })
}

/// The same window/colour/opacity LUT policy the existing scene adapter declares.
fn palace_transfer_from_channel_state(
    state: &newvolim_scene::ChannelState,
) -> Result<palace_core::gpu::PortableTransferFunction, String> {
    let lo = state.window.start as f32;
    let hi = state.window.end as f32;
    if !lo.is_finite() || !hi.is_finite() || hi <= lo {
        return Err("demand-driven scene transfer window is invalid".into());
    }
    let lut = (0..256)
        .map(|index| {
            [
                state.color_srgb[0],
                state.color_srgb[1],
                state.color_srgb[2],
                ((index as f32 / 255.0) * state.opacity * 255.0) as u8,
            ]
        })
        .collect();
    palace_core::gpu::PortableTransferFunction::new(lo, hi, lut)
        .ok_or_else(|| "demand-driven scene transfer is invalid".to_owned())
}

/// Run the ordered Palace scene path on the local fixed-binding recorder when available.  The
/// core oracle remains the exact fallback for hosts without an eligible WGPU adapter, but it is
/// only evaluated when the recorder is unavailable or fails: the fixed-binding shader has
/// local-adapter parity with it, so rendering every frame twice would be pure cost.
fn render_palace_portable_scene_camera_draw(
    session: &LocalSession,
    scene: &palace_core::gpu::PortableDvrSceneFrameInput,
) -> Result<palace_core::gpu::PortableFrameAttachments, String> {
    let recorded = palace_portable_scene_camera_draw_on_adapter(session, scene);
    if let Some(frame) = recorded {
        return Ok(frame);
    }
    scene.render_cpu().ok_or_else(|| {
        "portable Palace scene DVR CPU oracle rejected its admitted packet".to_owned()
    })
}

/// Acquire a local adapter and record one ordered scene frame, returning `None` for every
/// host-capability or recording failure so the caller can fall back to the core oracle.
fn palace_portable_scene_camera_draw_on_adapter(
    session: &LocalSession,
    scene: &palace_core::gpu::PortableDvrSceneFrameInput,
) -> Option<palace_core::gpu::PortableFrameAttachments> {
    let (device, queue) = session.portable_device()?;
    palace_wgpu::WgpuOperatorRecorder::new(device, queue)
        .record_dvr_scene_frame(scene)
        .ok()
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
        let spacing = level_zero_spacing_xyz(session)?;
        let rays = portable_camera_rays_xyz(&root, size, controls, voxel_origin_xyz, spacing)?;
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
    match render_palace_portable_camera_draw(session, &packet) {
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
    let distance = portable_scene_pick_depth(session, &packet, index)?;
    depth_aware_annotation_pick(session.annotations(), physical_ray, distance)
}

/// Read the occluding physical distance for one scene pixel from the renderer that owns depth.
///
/// Palace's ordered-scene attachment is preferred, and it is deliberately volume-only: an
/// annotation must not occlude itself, so the surface an annotation is tested against is the
/// admitted volume's first-opacity distance, never a depth buffer that already has projected
/// annotations composited into it. The native scene renderer remains the fallback for packets
/// Palace cannot admit, or for a host without an eligible adapter.
fn portable_scene_pick_depth(
    session: &LocalSession,
    packet: &newvolim_render::NativePortableSceneCameraDrawInput,
    index: usize,
) -> Result<f64, String> {
    if let Ok(scene) = palace_dvr_scene_from_native_camera(packet) {
        if let Some(frame) = palace_portable_scene_camera_draw_on_adapter(session, &scene) {
            return frame
                .first_opacity_distance
                .get(index)
                .copied()
                .map(f64::from)
                .ok_or_else(|| {
                    "Palace scene renderer omitted the requested depth pixel".to_owned()
                });
        }
    }
    let frame = newvolim_wgpu_frame::render_portable_scene_camera_draw(packet, 0)?;
    frame
        .ray_distances
        .get(index)
        .copied()
        .map(f64::from)
        .ok_or_else(|| "portable scene recorder omitted the requested depth pixel".to_owned())
}

/// Level-zero voxel spacing in XYZ order, the divisor that takes a fitted Palace camera ray into
/// the direct portable volume's voxel coordinates.
fn level_zero_spacing_xyz(session: &LocalSession) -> Result<[f32; 3], String> {
    let transform = session
        .portable_level_transform(0)
        .map_err(|error| error.to_string())?;
    Ok(transform.scale.map(|value| value.abs() as f32))
}

/// Palace's fitted camera is physical (see [`demand_world_ray_from_camera`]); the direct portable
/// volume marches in level-zero voxel coordinates, so the ray is divided by `spacing_xyz` here.
/// Passing the physical ray through unchanged put the camera `spacing` times too close on every
/// dataset whose spacing is not one voxel.
fn portable_camera_rays_xyz(
    root: &Path,
    size: FrameSize,
    controls: CameraControls,
    volume_origin_xyz: [u64; 3],
    spacing_xyz: [f32; 3],
) -> Result<Vec<newvolim_render::PortableCameraRay>, String> {
    if spacing_xyz
        .iter()
        .any(|value| !value.is_finite() || *value <= 0.0)
    {
        return Err("portable camera requires a positive finite level-zero spacing".into());
    }
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
    // Open the dataset once for the whole frame. `camera_ray_for_local_zarr` opens it per call,
    // which is right for a single pick and pathological for a ray table: measured in release, a
    // 256x192 frame spent 16.2 s here before this change and 5.2 ms after.
    let rays = palace_frame::camera_rays_for_local_zarr(root, size, controls)
        .map_err(|error| error.to_string())?;
    Ok(rays
        .into_iter()
        .map(|ray| {
            // Palace rays are ZYX and physical. The portable page is the requested XYZ
            // subvolume in voxel units, so reorder, divide by the spacing, and translate only
            // origins into that local space; a direction rescaled per axis is renormalized.
            let raw = [
                ray.direction[2] / spacing_xyz[0],
                ray.direction[1] / spacing_xyz[1],
                ray.direction[0] / spacing_xyz[2],
            ];
            let length = raw.iter().map(|value| value * value).sum::<f32>().sqrt();
            newvolim_render::PortableCameraRay {
                origin_xyz: [
                    ray.origin[2] / spacing_xyz[0] - volume_origin_xyz[0] as f32,
                    ray.origin[1] / spacing_xyz[1] - volume_origin_xyz[1] as f32,
                    ray.origin[0] / spacing_xyz[2] - volume_origin_xyz[2] as f32,
                ],
                direction_xyz: raw.map(|value| value / length),
            }
        })
        .collect())
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
    let local = portable_camera_rays_xyz(
        root,
        size,
        controls,
        layer.voxel_origin_xyz,
        layer.transform.scale.map(|value| value.abs() as f32),
    )?;
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

    /// The direct portable volume's rays are the fitted Palace ray reordered ZYX → XYZ **and**
    /// taken from physical units into level-zero voxel units. The fixture's anisotropic
    /// `0.29 × 0.26 × 0.26` spacing makes the two conversions distinguishable: an identity
    /// mapping, or a scale applied on the wrong axis, changes the origin by a factor of four.
    #[test]
    fn fitted_palace_camera_rays_are_reordered_and_rescaled_for_the_portable_xyz_volume() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        let size = FrameSize::new(3, 2).unwrap();
        let controls = CameraControls {
            orbit_delta: [37, -19],
            zoom: 1.2,
        };
        let spacing = [0.26_f32, 0.26, 0.29];
        let palace = camera_ray_for_local_zarr(&root, size, controls, [0, 0]).unwrap();
        let rays = portable_camera_rays_xyz(&root, size, controls, [0, 0, 0], spacing).unwrap();
        assert_eq!(rays.len(), 6);
        assert_eq!(
            rays[0].origin_xyz,
            [
                palace.origin[2] / 0.26,
                palace.origin[1] / 0.26,
                palace.origin[0] / 0.29
            ]
        );
        let translated =
            portable_camera_rays_xyz(&root, size, controls, [5, 7, 2], spacing).unwrap();
        assert_eq!(
            translated[0].origin_xyz,
            [
                palace.origin[2] / 0.26 - 5.0,
                palace.origin[1] / 0.26 - 7.0,
                palace.origin[0] / 0.29 - 2.0,
            ]
        );
        assert_eq!(translated[0].direction_xyz, rays[0].direction_xyz);
        let raw = [
            palace.direction[2] / 0.26,
            palace.direction[1] / 0.26,
            palace.direction[0] / 0.29,
        ];
        let length = raw.iter().map(|value| value * value).sum::<f32>().sqrt();
        for axis in 0..3 {
            assert!((rays[0].direction_xyz[axis] - raw[axis] / length).abs() < 1e-6);
        }
        assert!(
            portable_camera_rays_xyz(&root, size, controls, [0, 0, 0], [0.0, 1.0, 1.0]).is_err()
        );
    }

    /// The demand route's world ray is the physical Palace ray plus the layer translation —
    /// nothing else. Applying the layer scale to an already-physical origin was the defect that
    /// put the camera inside the fixture's box; a translation-only mapping under an anisotropic
    /// scale distinguishes the two.
    #[test]
    fn demand_world_ray_adds_only_the_layer_translation_to_the_physical_palace_ray() {
        let transform =
            newvolim_scene::LayerTransform::new([0.5, 0.25, 2.0], [10.0, 20.0, 30.0]).unwrap();
        let ray = palace_frame::CameraRay {
            origin: [1.0, 2.0, 3.0],
            direction: [0.0, 0.6, 0.8],
        };
        let world = demand_world_ray_from_camera(ray, transform).unwrap();
        assert_eq!(world.origin(), [13.0, 22.0, 31.0]);
        assert_eq!(world.direction(), [0.8, 0.6, 0.0]);
        assert_eq!((world.near(), world.far()), (0.0, f32::MAX));
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
            input.render_cpu(&transfer, 1.0 / 256.0).unwrap().first_opacity_distance,
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
            input.render_cpu(&transfer, 1.0 / 256.0).unwrap().first_opacity_distance,
            [2.0]
        );
        let mut transformed = camera.clone();
        transformed.draw.volume.frame.descriptors[0].transform =
            newvolim_scene::LayerTransform::new([2.0, 1.0, 1.0], [0.0; 3]).unwrap();
        let (level, rays) = palace_dvr_packet_from_native_camera(&transformed).unwrap();
        let input = level.raymarch_input(1, 1, rays, 1.0).unwrap();
        assert_eq!(
            input.render_cpu(&transfer, 1.0 / 256.0).unwrap().first_opacity_distance,
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
            palace_slice_words_from_admitted_volume_with_local_wgpu(&LocalSession::default(), &volume, 2, 0).unwrap(),
            vec![0, 1, 2, 3]
        );
        let payload = palace_slice_payload(&LocalSession::default(), &volume, 2, 0).unwrap();
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
            palace_slice_words_from_channel_with_local_wgpu(&LocalSession::default(), &volume, 0, 2, 0).unwrap(),
            vec![1]
        );
        assert_eq!(
            palace_slice_words_from_channel_with_local_wgpu(&LocalSession::default(), &volume, 1, 2, 0).unwrap(),
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
        assert!(palace_slice_payload(&LocalSession::default(), &volume, 2, 0)
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

    /// The direct portable route's pick occludes behind, and admits in front of, the surface the
    /// same packet's Palace page DVR produces, on the packet's own voxel-space rays taken back to
    /// the physical annotation frame.
    #[test]
    #[ignore = "requires a local WGPU adapter"]
    fn native_portable_picker_uses_its_matching_camera_packet_and_depth() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        let mut session = LocalSession::default();
        session.open_local_omezarr(&root).unwrap();
        session.prepare_default_portable_image_layer().unwrap();
        let draw = NativePortableDrawRequest {
            origin_xyz: [2, 2, 0],
            extent_xyz: [1, 1, 1],
            width: 64,
            height: 48,
            orbit_x: 0,
            orbit_y: 0,
            zoom: 1.0,
        };
        let packet = native_portable_camera_draw_for_session(draw, &session).unwrap();
        let physical_rays = packet
            .rays
            .iter()
            .map(|ray| {
                let global_origin = std::array::from_fn(|axis| {
                    f64::from(ray.origin_xyz[axis]) + packet.voxel_origin_xyz[axis] as f64
                });
                session
                    .portable_voxel_ray_to_physical(global_origin, ray.direction_xyz.map(f64::from))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        // The same depth source, in the same order of preference, as the pick itself.
        let depths: Vec<f64> = match render_palace_portable_camera_draw(&session, &packet) {
            Ok(frame) => frame
                .first_opacity_distance
                .iter()
                .map(|distance| f64::from(*distance))
                .collect(),
            Err(_) => {
                let frame = newvolim_wgpu_frame::render_portable_camera_draw(&packet, 0).unwrap();
                frame
                    .ray_distances
                    .iter()
                    .zip(&physical_rays)
                    .map(|(distance, ray)| f64::from(*distance) * ray.physical_distance_per_palace_unit)
                    .collect()
            }
        };
        let (index, origin, direction, near, surface, far) = roomiest_pixel(
            physical_rays.len(),
            |pixel| (physical_rays[pixel].ray.origin, physical_rays[pixel].ray.direction),
            |pixel| depths[pixel],
        );
        let request = NativePortablePickRequest {
            draw,
            x: (index % 64) as u32,
            y: (index / 64) as u32,
        };

        let behind = place_on_ray(
            &mut session,
            "behind",
            origin,
            direction,
            surface + (far - surface) / 2.0,
        );
        assert_eq!(
            pick_native_portable_annotation_for_session(request, &session).unwrap(),
            None,
            "an annotation behind the first-opacity surface must be occluded"
        );
        let front_distance = near + (surface - near) / 2.0;
        let front = place_on_ray(&mut session, "front", origin, direction, front_distance);
        assert_ne!(front, behind);
        let hit = pick_native_portable_annotation_for_session(request, &session)
            .unwrap()
            .expect("an annotation in front of the first-opacity surface must be selectable");
        assert_eq!(hit.annotation_id, front);
        assert!((hit.distance - front_distance).abs() < 0.5);
    }

    #[test]
    #[ignore = "requires a local WGPU adapter"]
    fn demand_driven_scene_admits_the_two_channel_fixture() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/two-channel-gradient.ome.zarr");
        let mut session = LocalSession::default();
        session.open_local_omezarr(&root).unwrap();
        session.prepare_default_portable_image_layer().unwrap();
        let request = NativePortableDrawRequest {
            origin_xyz: [0, 0, 0],
            extent_xyz: [1, 1, 1],
            width: 16,
            height: 12,
            orbit_x: 0,
            orbit_y: 0,
            zoom: 1.0,
        };
        let frame = render_demand_driven_scene_camera_draw(&session, request).unwrap();
        assert_eq!((frame.width, frame.height), (16, 12));
        assert!(frame.rgba.chunks_exact(4).any(|pixel| pixel[3] > 0));
    }

    /// Level selection must actually drive the frame, not answer zero forever.
    ///
    /// `select_portable_level` and the per-level geometry were implemented and tested separately;
    /// what this covers is that the desktop route consults them, that the answer responds to the
    /// camera in the right direction, and that a *coarse* level renders — which exercises a
    /// different source array, transform and chunk grid than level zero.
    #[test]
    fn demand_scene_level_follows_the_camera_and_frame_extent() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        let mut session = LocalSession::default();
        session.open_local_omezarr(&root).unwrap();
        session.prepare_default_portable_image_layer().unwrap();
        let level = |width: u32, height: u32, zoom: f32| {
            let controls = CameraControls {
                orbit_delta: [0, 0],
                zoom,
            }
            .validate()
            .unwrap();
            demand_scene_level(&session, FrameSize::new(width, height).unwrap(), controls).unwrap()
        };

        // More pixels over the same volume means a finer footprint, so a larger frame must never
        // choose a coarser level than a smaller one.
        let small = level(16, 12, 1.0);
        let large = level(256, 192, 1.0);
        assert!(
            large <= small,
            "a {large}-level 256x192 frame is coarser than a {small}-level 16x12 frame"
        );
        assert!(small > large, "this fixture must distinguish the two extents");

        // Pulling the camera back enlarges the footprint, which may only coarsen the level. At
        // 16x12 every zoom already sits on the coarsest level (two microns per pixel against a
        // one-micron coarsest spacing), so the distance pair uses a frame where the fixture's
        // three levels are all reachable: 128x96 selects 0, 1 and 2 at zoom 0.5, 1 and 2.
        let near = level(128, 96, 0.5);
        let far = level(128, 96, 2.0);
        assert!(far >= near, "moving out chose a finer level: {far} after {near}");
        assert!(far > near, "this fixture must distinguish the two distances");

        // The fixture declares three levels and selection must reach beyond the finest.
        assert_eq!(session.portable_level_spacings().unwrap().len(), 3);
        assert!(far >= 1, "selection never left level zero");
    }

    /// A coarse level must render end to end: a different source array, a different physical
    /// transform, and a different chunk grid than level zero all have to line up.
    #[test]
    #[ignore = "requires a local WGPU adapter"]
    fn demand_driven_scene_renders_a_coarse_level() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        let mut session = LocalSession::default();
        session.open_local_omezarr(&root).unwrap();
        session.prepare_default_portable_image_layer().unwrap();
        // A small frame pulled back selects a coarse level for this fixture.
        let request = NativePortableDrawRequest {
            origin_xyz: [0, 0, 0],
            extent_xyz: [1, 1, 1],
            width: 16,
            height: 12,
            orbit_x: 0,
            orbit_y: 0,
            zoom: 2.0,
        };
        let size = desktop_frame_size(request.width, request.height, 1).unwrap();
        let controls = CameraControls {
            orbit_delta: [request.orbit_x, request.orbit_y],
            zoom: request.zoom,
        }
        .validate()
        .unwrap();
        let chosen = demand_scene_level(&session, size, controls).unwrap();
        assert!(chosen > 0, "this fixture must select a coarse level here");

        let frame = render_demand_driven_scene_camera_draw(&session, request).unwrap();
        assert_eq!((frame.width, frame.height), (16, 12));
        assert!(
            frame.rgba.chunks_exact(4).any(|pixel| pixel[3] > 0),
            "the coarse-level frame must actually render volume"
        );
        // Renderer-owned depth still pairs with colour at a coarse level.
        for (pixel, distance) in frame
            .rgba
            .chunks_exact(4)
            .zip(&frame.first_opacity_distance)
        {
            if pixel[3] > 0 {
                assert!(distance.is_finite() && *distance > 0.0, "got {distance}");
            } else {
                assert_eq!(*distance, f32::INFINITY);
            }
        }
    }

    /// Palace's own Vulkan raycaster against the desktop's portable demand route, with everything
    /// that is *not* the renderer matched: the same fitted camera, the same transfer table, no
    /// shading, and level zero forced on both sides.
    ///
    /// What still differs is the compositor itself, and each difference is known and measured
    /// (report in `STAGE0.md`):
    ///
    /// - **Hit set.** Identical: every ray that finds volume on one side finds it on the other.
    /// - **Depth.** Palace samples the nearest voxel at `round(p / spacing)` stepping
    ///   `|dir ⊙ spacing|` from the entry face; its sample *on* the face rounds to an index
    ///   outside the array and is skipped, so its first contribution is one step in. The portable
    ///   pass samples `floor((p − min) / extent × dims)` at half the finest spacing, starting half
    ///   a step in. The difference is therefore a constant `−(palace_step − portable_step / 2)`
    ///   per ray, `−0.224` on the fixture's centre rays, and never exceeds one Palace step.
    /// - **Colour.** Palace keeps its accumulated colour in 8 bits between steps and truncates
    ///   (`from_uniform` is `uint(v * 255)`), which loses small increments — most of all in the
    ///   weak channels, so the fixture's `[255, 51, 85]` transfer comes back with green and blue
    ///   proportionally low. The portable pass accumulates in `f32`, so its hue is the
    ///   transfer's exactly. Per-pixel alpha scatters by tens of levels either way from the
    ///   different sample positions through membrane-thin structure; in aggregate the two agree.
    ///
    /// Run with `--nocapture` for the full report. The assertions are the bounds those
    /// differences permit, set from the measured report.
    #[test]
    #[ignore = "comparison; requires a local WGPU adapter and Vulkan"]
    fn compare_desktop_portable_and_server_renderers() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        let size = FrameSize::new(64, 48).unwrap();
        let controls = CameraControls::default().validate().unwrap();
        let mut session = LocalSession::default();
        session.open_local_omezarr(&root).unwrap();
        session.prepare_default_portable_image_layer().unwrap();

        // The demand route's transfer, rebuilt as Palace's table so both classify identically.
        let probe = session
            .local_layer_chunk_plan_for_chunks_at_level(LayerRenderLimits::new(4, 4), 0, &[[0, 0, 0]], 8)
            .unwrap();
        let state = probe[0].request.layer.channels[0].state.clone();
        let portable_transfer = palace_transfer_from_channel_state(&state).unwrap();
        let entries = portable_transfer.entries().to_vec();
        let transfer = palace_frame::TransFuncOperator::gen(
            portable_transfer.min(),
            portable_transfer.max(),
            entries.len(),
            |index| palace_core::data::Vector::<palace_core::dim::D4, u8>::from(entries[index]),
        );

        let server = palace_frame::render_local_zarr_with_camera_attachments_using(
            &root,
            size,
            controls,
            palace_frame::CameraRenderOptions {
                shading: palace_frame::Shading::None,
                transfer,
                lod_coarseness: 0.0,
            },
        )
        .unwrap();
        let server_colour = server.color().pixels().to_vec();
        let server_depth = server.ray_distance().unwrap();

        let portable = render_demand_driven_scene_camera_draw_at_level(
            &session,
            NativePortableDrawRequest {
                origin_xyz: [0, 0, 0],
                extent_xyz: [1, 1, 1],
                width: 64,
                height: 48,
                orbit_x: 0,
                orbit_y: 0,
                zoom: 1.0,
            },
            0,
        )
        .unwrap();

        assert_eq!(server_colour.len(), portable.rgba.len());
        let pixels = portable.first_opacity_distance.len();
        let mut colour_differs = 0_usize;
        let mut max_channel = 0_i32;
        let mut sum_channel = 0_i64;
        let mut server_hits = 0_usize;
        let mut portable_hits = 0_usize;
        let mut both = 0_usize;
        let mut only_one = 0_usize;
        let mut depth_delta_sum = 0.0_f64;
        let mut depth_delta_max = 0.0_f32;
        let mut signed_depth_sum = 0.0_f64;
        let mut within_a_voxel = 0_usize;
        for pixel in 0..pixels {
            let a = &portable.rgba[pixel * 4..pixel * 4 + 4];
            let b = &server_colour[pixel * 4..pixel * 4 + 4];
            if a != b {
                colour_differs += 1;
            }
            for channel in 0..4 {
                let delta = (i32::from(a[channel]) - i32::from(b[channel])).abs();
                max_channel = max_channel.max(delta);
                sum_channel += i64::from(delta);
            }
            let pd = portable.first_opacity_distance[pixel];
            let sd = server_depth.distances()[pixel];
            server_hits += usize::from(sd.is_finite());
            portable_hits += usize::from(pd.is_finite());
            match (sd.is_finite(), pd.is_finite()) {
                (true, true) => {
                    both += 1;
                    let delta = (pd - sd).abs();
                    depth_delta_sum += f64::from(delta);
                    depth_delta_max = depth_delta_max.max(delta);
                    signed_depth_sum += f64::from(pd - sd);
                    within_a_voxel += usize::from(delta <= 0.29);
                }
                (false, false) => {}
                _ => only_one += 1,
            }
        }
        let mean_depth_delta = if both == 0 { 0.0 } else { depth_delta_sum / both as f64 };
        let mean_signed = if both == 0 { 0.0 } else { signed_depth_sum / both as f64 };
        println!("pixels: {pixels}");
        println!(
            "colour: {colour_differs} differing ({:.1}%), max channel delta {max_channel}, mean \
             channel delta {:.2}",
            100.0 * colour_differs as f64 / pixels as f64,
            sum_channel as f64 / (pixels * 4) as f64
        );
        println!(
            "finite depth: server {server_hits}, portable {portable_hits}, both {both}, only one \
             side {only_one}; where both: mean |delta| {mean_depth_delta:.4}, max {depth_delta_max:.4}, \
             mean signed (portable - server) {mean_signed:+.4}, within one z voxel (0.29) \
             {within_a_voxel}"
        );
        println!(
            "transfer: window [{}, {}], colour {:?}, opacity {}, {} entries, entry 128 = {:?}",
            portable_transfer.min(),
            portable_transfer.max(),
            state.color_srgb,
            state.opacity,
            entries.len(),
            entries[128]
        );
        let hits = (0..pixels)
            .filter(|pixel| server_depth.distances()[*pixel].is_finite())
            .collect::<Vec<_>>();
        let mean_alpha = |colour: &[u8]| {
            hits.iter()
                .map(|pixel| f64::from(colour[pixel * 4 + 3]))
                .sum::<f64>()
                / hits.len() as f64
        };
        let mut alpha_delta = hits
            .iter()
            .map(|pixel| i32::from(portable.rgba[pixel * 4 + 3]) - i32::from(server_colour[pixel * 4 + 3]))
            .collect::<Vec<_>>();
        alpha_delta.sort_unstable();
        println!(
            "alpha over {} hit pixels: mean server {:.1}, mean portable {:.1}; portable - server \
             percentiles 5/25/50/75/95: {} {} {} {} {}",
            hits.len(),
            mean_alpha(&server_colour),
            mean_alpha(&portable.rgba),
            alpha_delta[hits.len() / 20],
            alpha_delta[hits.len() / 4],
            alpha_delta[hits.len() / 2],
            alpha_delta[3 * hits.len() / 4],
            alpha_delta[19 * hits.len() / 20]
        );
        for &pixel in hits.iter().step_by(hits.len() / 8).take(8) {
            println!(
                "  pixel {pixel}: server {:?} d={} | portable {:?} d={}",
                &server_colour[pixel * 4..pixel * 4 + 4],
                server_depth.distances()[pixel],
                &portable.rgba[pixel * 4..pixel * 4 + 4],
                portable.first_opacity_distance[pixel]
            );
        }

        // The same camera over the same box must find volume on the same rays: a pixel that is
        // finite on one side only can be a boundary voxel caught by one sampling grid and not the
        // other, never a systematic set.
        assert!(server_hits > 0 && portable_hits > 0);
        assert!(
            only_one * 20 <= both,
            "{only_one} pixels have a first-opacity surface on one side only against {both} shared"
        );
        // Depth: Palace's skipped face sample puts its first contribution one step (at most the
        // largest spacing, 0.29) further in than the portable pass's half-step start; nothing
        // else moves the surface, so every shared depth agrees within that step, and the signed
        // offset is bounded by it too.
        assert!(
            within_a_voxel == both,
            "only {within_a_voxel} of {both} shared depths agree within one voxel"
        );
        assert!(
            mean_signed.abs() <= 0.29,
            "depth differs systematically by {mean_signed:+.4} (portable - server)"
        );
        // Colour: the same transfer over the same rays must accumulate the same opacity in
        // aggregate. This is what caught the Vulkan raycaster compositing exactly one sample per
        // ray (mean alpha 19 against 208); after that fix the means are 202 and 208.
        let server_alpha = mean_alpha(&server_colour);
        let portable_alpha = mean_alpha(&portable.rgba);
        assert!(
            (server_alpha - portable_alpha).abs() <= 0.1 * portable_alpha,
            "mean alpha over hit pixels: server {server_alpha:.1}, portable {portable_alpha:.1}"
        );
        // The portable pass accumulates in f32, so its hue is the transfer's own at every pixel
        // with measurable colour; Palace's 8-bit truncation is what makes this untrue for it.
        for &pixel in &hits {
            let colour = &portable.rgba[pixel * 4..pixel * 4 + 4];
            if colour[0] >= 64 {
                let expected_g = f64::from(colour[0]) * f64::from(state.color_srgb[1]) / 255.0;
                assert!(
                    (f64::from(colour[1]) - expected_g).abs() <= 2.0,
                    "pixel {pixel}: portable colour {colour:?} is not the transfer's hue"
                );
            }
        }
    }

    /// Side-by-side portable-versus-native rendering of the **same packet**, reported rather than
    /// asserted.
    ///
    /// Rendering one packet both ways isolates the two things that actually differ in the
    /// compositor — the blend rule and the first-opacity rule — from residency and level choice,
    /// which are properties of the demand route rather than of the renderer. Findings are recorded
    /// in `STAGE0.md`; run with `--nocapture`.
    #[test]
    #[ignore = "comparison report; requires a local WGPU adapter"]
    fn compare_portable_and_native_scene_rendering() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        let mut session = LocalSession::default();
        session.open_local_omezarr(&root).unwrap();
        session.prepare_default_portable_image_layer().unwrap();
        let request = NativePortableDrawRequest {
            origin_xyz: [0, 0, 0],
            extent_xyz: [2, 2, 2],
            width: 64,
            height: 48,
            orbit_x: 0,
            orbit_y: 0,
            zoom: 1.0,
        };
        let packet = native_portable_scene_camera_draw_for_session(request, &session).unwrap();
        assert!(
            packet.draw.annotation_words.is_empty(),
            "compare volume rendering only"
        );
        let scene = palace_dvr_scene_from_native_camera(&packet).unwrap();
        let palace = render_palace_portable_scene_camera_draw(&session, &scene).unwrap();
        let native = newvolim_wgpu_frame::render_portable_scene_camera_draw(&packet, 0).unwrap();

        assert_eq!(palace.first_opacity_distance.len(), native.ray_distances.len());
        let pixels = native.ray_distances.len();
        let mut colour_differs = 0_usize;
        let mut max_channel = 0_i32;
        let mut sum_channel = 0_i64;
        let mut palace_opaque = 0_usize;
        let mut native_opaque = 0_usize;
        let mut depth_differs = 0_usize;
        let mut max_depth = 0.0_f32;
        let mut palace_hits = 0_usize;
        let mut native_hits = 0_usize;
        for pixel in 0..pixels {
            let a = &palace.rgba[pixel * 4..pixel * 4 + 4];
            let b = native.rgba[pixel];
            if a != b {
                colour_differs += 1;
            }
            for channel in 0..4 {
                let delta = i32::from(a[channel]) - i32::from(b[channel]);
                max_channel = max_channel.max(delta.abs());
                sum_channel += i64::from(delta.abs());
            }
            if a[3] == 255 {
                palace_opaque += 1;
            }
            if b[3] == 255 {
                native_opaque += 1;
            }
            let pd = palace.first_opacity_distance[pixel];
            let nd = native.ray_distances[pixel];
            if pd.is_finite() {
                palace_hits += 1;
            }
            if nd.is_finite() {
                native_hits += 1;
            }
            if pd.is_finite() && nd.is_finite() {
                let delta = (pd - nd).abs();
                if delta > 1e-4 {
                    depth_differs += 1;
                }
                max_depth = max_depth.max(delta);
            } else if pd.is_finite() != nd.is_finite() {
                depth_differs += 1;
            }
        }
        println!("pixels: {pixels}");
        println!(
            "colour: {colour_differs} differing pixels ({:.1}%), max channel delta {max_channel}, \
             mean channel delta {:.2}",
            100.0 * colour_differs as f64 / pixels as f64,
            sum_channel as f64 / (pixels * 4) as f64
        );
        println!("fully opaque pixels: palace {palace_opaque}, native {native_opaque}");
        println!(
            "depth: {depth_differs} differing pixels, max finite delta {max_depth}; \
             finite-depth pixels palace {palace_hits}, native {native_hits}"
        );
        println!("step size: {}", scene.step_size());
        for pixel in [0_usize, pixels / 4, pixels / 2, pixels / 2 + 7, pixels - 1] {
            println!(
                "  pixel {pixel}: palace {:?} d={} | native {:?} d={}",
                &palace.rgba[pixel * 4..pixel * 4 + 4],
                palace.first_opacity_distance[pixel],
                native.rgba[pixel],
                native.ray_distances[pixel]
            );
        }
    }

    /// Cost of building a frame's native ray table. Run in **release** with `--nocapture`.
    #[test]
    #[ignore = "measurement, not an assertion"]
    fn measure_native_camera_ray_table_cost() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        let controls = CameraControls::default().validate().unwrap();
        for (width, height) in [(64_u32, 48_u32), (256, 192)] {
            let size = FrameSize::new(width, height).unwrap();
            let start = std::time::Instant::now();
            let rays =
                portable_camera_rays_xyz(&root, size, controls, [0, 0, 0], [0.26, 0.26, 0.29])
                    .unwrap();
            println!(
                "{width}x{height}: {:?} for {} rays",
                start.elapsed(),
                rays.len()
            );
        }
    }

    /// Interactive cost of the demand route, reported rather than asserted.
    ///
    /// Run with `--nocapture`. The numbers are recorded in `STAGE0.md`; this exists so the claim
    /// "the portable route is interactive" can be checked rather than assumed, and so a regression
    /// has something to be compared against. It asserts only that a frame completes, because a
    /// timing threshold on a shared machine would be a flaky test rather than evidence.
    #[test]
    #[ignore = "measurement, not an assertion; requires a local WGPU adapter"]
    fn measure_demand_driven_scene_frame_cost() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        let mut session = LocalSession::default();
        session.open_local_omezarr(&root).unwrap();
        session.prepare_default_portable_image_layer().unwrap();
        for (width, height) in [(64_u32, 48_u32), (256, 192)] {
            let request = NativePortableDrawRequest {
                origin_xyz: [0, 0, 0],
                extent_xyz: [1, 1, 1],
                width,
                height,
                orbit_x: 0,
                orbit_y: 0,
                zoom: 1.0,
            };
            let size = desktop_frame_size(width, height, 1).unwrap();
            let controls = CameraControls {
                orbit_delta: [0, 0],
                zoom: 1.0,
            }
            .validate()
            .unwrap();
            let level = demand_scene_level(&session, size, controls).unwrap();

            // Breakdown probe: how much of a frame is camera-ray construction?
            {
                let transform = session.portable_level_transform(level).unwrap();
                let probe = session
                    .local_layer_chunk_plan_for_chunks_at_level(
                        LayerRenderLimits::new(4, 4),
                        level,
                        &[[0, 0, 0]],
                        8,
                    )
                    .unwrap();
                let source = &probe[0].request.source;
                let spatial: [usize; 3] = source.spatial_axes_xyz.map(|axis| axis as usize);
                let dimensions: [u32; 3] =
                    std::array::from_fn(|axis| source.shape[spatial[axis]] as u32);
                let minimum = transform.voxel_to_world([0.0; 3]).map(|value| value as f32);
                let maximum: [f32; 3] = std::array::from_fn(|axis| {
                    (transform.translation[axis]
                        + transform.scale[axis] * f64::from(dimensions[axis]))
                        as f32
                });
                let start = std::time::Instant::now();
                let rays = portable_demand_world_rays(
                    dimensions, size, controls, transform, minimum, maximum,
                )
                .unwrap();
                println!("  rays only: {:?} for {} rays", start.elapsed(), rays.len());
            }
            let cold = std::time::Instant::now();
            let frame = render_demand_driven_scene_camera_draw(&session, request).unwrap();
            let cold = cold.elapsed();
            assert!(frame.rgba.chunks_exact(4).any(|pixel| pixel[3] > 0));

            let mut warm = Vec::new();
            for orbit in 1..=5 {
                let request = NativePortableDrawRequest {
                    orbit_x: orbit,
                    ..request
                };
                let start = std::time::Instant::now();
                render_demand_driven_scene_camera_draw(&session, request).unwrap();
                warm.push(start.elapsed());
            }
            let total: std::time::Duration = warm.iter().sum();
            let mean = total / warm.len() as u32;
            println!(
                "{width}x{height} level {level}: cold {cold:?}, warm mean {mean:?} over {} frames \
                 (min {:?}, max {:?})",
                warm.len(),
                warm.iter().min().unwrap(),
                warm.iter().max().unwrap()
            );
        }
    }

    /// The device cache, proven by counting acquisitions rather than by timing.
    ///
    /// The device is owned by the **session**, not by a process-wide static. A static is never
    /// dropped, which leaves the graphics driver's background threads alive at `exit()`; on this
    /// host that faulted in the driver's `[vkps] Update` thread on roughly a third of runs, and a
    /// minimal probe confirmed the cause — dropping a device before exit crashed 0/20 times,
    /// leaking one crashed 8/20. Session ownership keeps the sharing that matters, every pass of
    /// one frame, while guaranteeing the device is destroyed before the process tears down.
    #[test]
    #[ignore = "requires a local WGPU adapter"]
    fn portable_routes_share_one_wgpu_device_per_session() {
        use crate::session::PORTABLE_DEVICE_ACQUISITIONS;
        use std::sync::atomic::Ordering;

        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        let mut session = LocalSession::default();
        session.open_local_omezarr(&root).unwrap();
        session.prepare_default_portable_image_layer().unwrap();
        session.add_point_annotation("centre", [16, 16, 4]).unwrap();

        let before = PORTABLE_DEVICE_ACQUISITIONS.load(Ordering::Relaxed);
        let first = session.portable_device();
        assert!(first.is_some(), "this host has an eligible WGPU adapter");
        assert_eq!(
            PORTABLE_DEVICE_ACQUISITIONS.load(Ordering::Relaxed) - before,
            1,
            "the first use must acquire exactly once"
        );
        for _ in 0..8 {
            assert!(std::ptr::eq(first.unwrap(), session.portable_device().unwrap()));
        }
        assert_eq!(
            PORTABLE_DEVICE_ACQUISITIONS.load(Ordering::Relaxed) - before,
            1
        );

        // A converging demand frame re-renders several times and then composites annotations,
        // exercising several routes; none of them may acquire again.
        let request = NativePortableDrawRequest {
            origin_xyz: [0, 0, 0],
            extent_xyz: [1, 1, 1],
            width: 24,
            height: 18,
            orbit_x: 0,
            orbit_y: 0,
            zoom: 1.0,
        };
        let frame = render_demand_driven_scene_camera_draw(&session, request).unwrap();
        let composited = composite_palace_scene_annotations(&session, request, frame).unwrap();
        assert_eq!(composited.width, 24);
        assert_eq!(
            PORTABLE_DEVICE_ACQUISITIONS.load(Ordering::Relaxed) - before,
            1,
            "a converging demand frame plus an annotation composite must reuse the one device"
        );

        // A clone shares the same device, so handing the session to a render path costs nothing.
        let shared = session.clone();
        assert!(std::ptr::eq(first.unwrap(), shared.portable_device().unwrap()));
        assert_eq!(
            PORTABLE_DEVICE_ACQUISITIONS.load(Ordering::Relaxed) - before,
            1
        );

        // A genuinely separate session acquires its own, which is the scope that makes teardown
        // deterministic.
        let other = LocalSession::default();
        assert!(other.portable_device().is_some());
        assert_eq!(
            PORTABLE_DEVICE_ACQUISITIONS.load(Ordering::Relaxed) - before,
            2
        );
    }

    /// The milestone claim, tested directly: chunk demand comes from the renderer, so the
    /// webview-supplied chunk region no longer influences what is rendered. Two requests with
    /// deliberately different regions must produce byte-identical frames, and the frame must
    /// actually contain volume — an all-transparent frame would pass trivially.
    #[test]
    #[ignore = "requires a local WGPU adapter"]
    fn demand_driven_scene_ignores_the_webview_chunk_region() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        let mut session = LocalSession::default();
        session.open_local_omezarr(&root).unwrap();
        session.prepare_default_portable_image_layer().unwrap();
        let camera = |origin_xyz, extent_xyz| NativePortableDrawRequest {
            origin_xyz,
            extent_xyz,
            width: 24,
            height: 18,
            orbit_x: 0,
            orbit_y: 0,
            zoom: 1.0,
        };
        let first =
            render_demand_driven_scene_camera_draw(&session, camera([0, 0, 0], [1, 1, 1])).unwrap();
        let second =
            render_demand_driven_scene_camera_draw(&session, camera([3, 2, 1], [1, 1, 1])).unwrap();
        assert_eq!(
            first, second,
            "the webview chunk region must no longer decide what is rendered"
        );
        let wider =
            render_demand_driven_scene_camera_draw(&session, camera([0, 0, 0], [4, 4, 4])).unwrap();
        assert_eq!(first, wider);

        assert_eq!(first.width, 24);
        assert_eq!(first.height, 18);
        assert!(
            first.rgba.chunks_exact(4).any(|pixel| pixel[3] > 0),
            "the demand-driven frame must actually render volume"
        );
        // Renderer-owned depth still pairs with that colour: an opacified pixel carries a finite
        // physical distance, and a transparent one stays at positive infinity.
        for (pixel, distance) in first
            .rgba
            .chunks_exact(4)
            .zip(&first.first_opacity_distance)
        {
            if pixel[3] > 0 {
                assert!(distance.is_finite() && *distance > 0.0, "got {distance}");
            } else {
                assert_eq!(*distance, f32::INFINITY);
            }
        }
    }

    /// Palace owns both passes of an ordered scene frame.  This covers the desktop wiring of the
    /// second one: the session's projected annotations reach Palace as admitted primitives, the
    /// composite paints over the raymarched colour, the local adapter agrees with the core
    /// oracle exactly, and the volume's first-opacity attachment survives untouched — the picker
    /// depends on that last property.
    #[test]
    #[ignore = "requires a local WGPU adapter"]
    fn palace_scene_annotation_composite_paints_over_its_own_frame_and_keeps_volume_depth() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        let mut session = LocalSession::default();
        session.open_local_omezarr(&root).unwrap();
        session.prepare_default_portable_image_layer().unwrap();
        session.add_point_annotation("centre", [16, 16, 4]).unwrap();
        let request = NativePortableDrawRequest {
            origin_xyz: [0, 0, 0],
            extent_xyz: [1, 1, 1],
            width: 32,
            height: 24,
            orbit_x: 0,
            orbit_y: 0,
            zoom: 1.0,
        };
        let size = desktop_frame_size(request.width, request.height, 1).unwrap();
        let controls = CameraControls {
            orbit_delta: [request.orbit_x, request.orbit_y],
            zoom: request.zoom,
        }
        .validate()
        .unwrap();

        // The session's own annotations must be admissible as Palace primitives.
        let projected = palace_annotation_primitives(&session, &root, size, controls).unwrap();
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].annotation_id(), 1);

        let packet = native_portable_scene_camera_draw_for_session(request, &session).unwrap();
        let scene = palace_dvr_scene_from_native_camera(&packet).unwrap();
        let frame = render_palace_portable_scene_camera_draw(&session, &scene).unwrap();
        let hit = frame
            .first_opacity_distance
            .iter()
            .position(|distance| distance.is_finite() && *distance > 0.0)
            .expect("the admitted fixture camera must opacify at least one pixel");

        // A primitive at distance zero over that pixel is unconditionally in front of the
        // volume, so the composite must paint it whatever the fixture's depths happen to be.
        let painted = palace_core::gpu::ProjectedAnnotationPrimitive::point(
            1,
            [255, 0, 255],
            0.5,
            [
                (hit % frame.width as usize) as f32,
                (hit / frame.width as usize) as f32,
                0.0,
            ],
        )
        .unwrap();
        let input = palace_core::gpu::PortableAnnotationCompositeInput::new(
            frame.clone(),
            vec![painted, projected[0]],
        )
        .unwrap();
        let expected = input.composite_cpu().unwrap();
        assert_eq!(
            &expected.rgba[hit * 4..hit * 4 + 4],
            &[255, 0, 255, 255],
            "the composite must paint an annotation in front of the first-opacity surface"
        );
        assert_eq!(
            expected.first_opacity_distance, frame.first_opacity_distance,
            "the composite must leave the volume first-opacity attachment untouched"
        );
        let composited = palace_annotation_composite_on_adapter(&session, &input)
            .expect("this host has an eligible WGPU adapter");
        assert_eq!(composited, expected);

        // The desktop wiring itself renders and composites without falling back.
        let payload =
            render_native_portable_scene_camera_draw_for_session(request, &session).unwrap();
        assert_eq!(payload.target.depth, DepthAttachment::RayDistanceF32);
    }

    /// Pins the routing the loose picker smoke cannot: the occluding distance a scene annotation
    /// pick is tested against must be Palace's own ordered-scene attachment, bit for bit, and it
    /// must be volume-only.  With that real depth in hand the fixture then proves the intended
    /// behaviour directly — an annotation in front of the first-opacity surface is selectable and
    /// one behind it is not.
    #[test]
    #[ignore = "requires a local WGPU adapter"]
    fn portable_scene_pick_depth_is_palace_volume_depth_and_occludes_behind_annotations() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        let mut session = LocalSession::default();
        session.open_local_omezarr(&root).unwrap();
        session.prepare_default_portable_image_layer().unwrap();
        let packet = native_portable_scene_camera_draw_for_session(
            NativePortableDrawRequest {
                origin_xyz: [0, 0, 0],
                extent_xyz: [1, 1, 1],
                width: 32,
                height: 24,
                orbit_x: 0,
                orbit_y: 0,
                zoom: 1.0,
            },
            &session,
        )
        .unwrap();
        let scene = palace_dvr_scene_from_native_camera(&packet).unwrap();
        let palace = palace_portable_scene_camera_draw_on_adapter(&session, &scene)
            .expect("this host has an eligible WGPU adapter");

        // A framed camera must contain both kinds of pixel, so the degenerate transparent-ray
        // admission is genuinely exercised here rather than only in the core fixtures.
        let hit = palace
            .first_opacity_distance
            .iter()
            .position(|distance| distance.is_finite() && *distance > 0.0)
            .expect("the admitted fixture camera must opacify at least one pixel");
        let miss = palace
            .first_opacity_distance
            .iter()
            .position(|distance| *distance == f32::INFINITY)
            .expect("the admitted fixture camera must have at least one ray miss the scene");

        // Routing: a hit pixel, a missed pixel and both frame edges resolve to the Palace word.
        // Each call re-renders the frame, so this samples rather than sweeping all of them.
        for index in [hit, miss, 0, palace.first_opacity_distance.len() - 1] {
            assert_eq!(
                portable_scene_pick_depth(&session, &packet, index).unwrap(),
                f64::from(palace.first_opacity_distance[index]),
                "pick depth at pixel {index} did not come from the Palace scene attachment"
            );
        }

        // Behaviour: straddle the first-opacity surface of a pixel that actually hit the volume.
        let index = hit;
        let depth = portable_scene_pick_depth(&session, &packet, index).unwrap();
        assert!(depth.is_finite() && depth > 0.0);
        let ray = packet.rays[index];
        let at = |factor: f64| -> [f64; 3] {
            std::array::from_fn(|axis| {
                f64::from(ray.origin_world[axis])
                    + f64::from(ray.direction_world[axis]) * depth * factor
            })
        };
        let front = Annotation::new(
            newvolim_scene::AnnotationId(7),
            "front",
            newvolim_scene::AnnotationGeometry::Point(at(0.5)),
            [255, 216, 72],
        )
        .unwrap();
        let behind = Annotation::new(
            newvolim_scene::AnnotationId(9),
            "behind",
            newvolim_scene::AnnotationGeometry::Point(at(1.5)),
            [255, 216, 72],
        )
        .unwrap();
        let pick_ray =
            newvolim_render::PickRay::new(ray.origin_world, ray.direction_world).unwrap();
        let hit = depth_aware_annotation_pick(
            &[front.clone(), behind.clone()],
            pick_ray,
            depth,
        )
        .unwrap()
        .expect("an annotation in front of the first-opacity surface must be selectable");
        assert_eq!(hit.annotation_id, 7);
        assert_eq!(
            depth_aware_annotation_pick(&[behind], pick_ray, depth).unwrap(),
            None,
            "an annotation behind the Palace first-opacity surface must stay occluded"
        );
    }

    /// The ordered-scene pick occludes behind, and admits in front of, the Palace scene
    /// attachment for the same packet, on the packet's own physical world rays.
    #[test]
    #[ignore = "requires a local WGPU adapter"]
    fn native_portable_scene_picker_uses_ordered_scene_renderer_depth() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        let mut session = LocalSession::default();
        session.open_local_omezarr(&root).unwrap();
        session.prepare_default_portable_image_layer().unwrap();
        let draw = NativePortableDrawRequest {
            origin_xyz: [2, 2, 0],
            extent_xyz: [1, 1, 1],
            width: 64,
            height: 48,
            orbit_x: 0,
            orbit_y: 0,
            zoom: 1.0,
        };
        let packet = native_portable_scene_camera_draw_for_session(draw, &session).unwrap();
        // One render of the packet's Palace scene gives every pixel's depth; the pick reads the
        // same attachment (checked below at the chosen pixel) but re-renders per call.
        let scene = palace_dvr_scene_from_native_camera(&packet).unwrap();
        let frame = palace_portable_scene_camera_draw_on_adapter(&session, &scene)
            .expect("this host has an eligible WGPU adapter");
        let (index, origin, direction, near, surface, far) = roomiest_pixel(
            packet.rays.len(),
            |pixel| {
                let ray = packet.rays[pixel];
                (ray.origin_world.map(f64::from), ray.direction_world.map(f64::from))
            },
            |pixel| f64::from(frame.first_opacity_distance[pixel]),
        );
        assert_eq!(
            portable_scene_pick_depth(&session, &packet, index).unwrap(),
            surface,
            "the pick must read the same Palace scene attachment"
        );
        let request = NativePortableScenePickRequest {
            draw,
            x: (index % 64) as u32,
            y: (index / 64) as u32,
        };

        let behind = place_on_ray(
            &mut session,
            "behind",
            origin,
            direction,
            surface + (far - surface) / 2.0,
        );
        assert_eq!(
            pick_native_portable_scene_annotation_for_session(request, &session).unwrap(),
            None,
            "an annotation behind the first-opacity surface must be occluded"
        );
        let front_distance = near + (surface - near) / 2.0;
        let front = place_on_ray(&mut session, "front", origin, direction, front_distance);
        assert_ne!(front, behind);
        let hit = pick_native_portable_scene_annotation_for_session(request, &session)
            .unwrap()
            .expect("an annotation in front of the first-opacity surface must be selectable");
        assert_eq!(hit.annotation_id, front);
        assert!((hit.distance - front_distance).abs() < 0.5);
    }

    /// Place a point annotation on a physical world ray at `distance` from its origin, rounded to
    /// the nearest level-zero voxel, and return its id. Rounding moves the point by at most half
    /// a voxel diagonal, 0.24 um on the fixture: well inside the 0.5 um pick radius and inside
    /// the margins the pick tests keep on either side of the first-opacity surface.
    fn place_on_ray(
        session: &mut LocalSession,
        label: &str,
        origin: [f64; 3],
        direction: [f64; 3],
        distance: f64,
    ) -> u64 {
        let physical = std::array::from_fn(|axis| origin[axis] + direction[axis] * distance);
        let voxel = session.physical_point_voxel_xyz(physical).unwrap();
        session.add_point_annotation(label, voxel).unwrap().id.0
    }

    /// Entry and exit distances of a physical world ray through the fixture's level-zero box:
    /// 128x128x32 voxels at 0.26x0.26x0.29 um with no translation, in the desktop's corner
    /// convention.
    fn fixture_box_crossing(origin: [f64; 3], direction: [f64; 3]) -> Option<(f64, f64)> {
        let maximum = [128.0 * 0.26, 128.0 * 0.26, 32.0 * 0.29];
        let mut near = 0.0_f64;
        let mut far = f64::INFINITY;
        for axis in 0..3 {
            if direction[axis].abs() < 1e-12 {
                if origin[axis] < 0.0 || origin[axis] > maximum[axis] {
                    return None;
                }
                continue;
            }
            let first = -origin[axis] / direction[axis];
            let second = (maximum[axis] - origin[axis]) / direction[axis];
            near = near.max(first.min(second));
            far = far.min(first.max(second));
        }
        (far >= near).then_some((near, far))
    }

    /// The pixel whose ray leaves the most room on both sides of the route's first-opacity
    /// surface inside the fixture box, with that ray and its entry/surface/exit distances. A
    /// pick test needs room: an annotation in front must be clearly in front, one behind clearly
    /// behind, after both are rounded to voxels. The annotations are placed at half the room, so
    /// the margin against the surface is half the room too; requiring 0.6 um of room keeps that
    /// margin above the 0.235 um rounding radius. The server's saturating transfer puts its
    /// surface at the first non-zero voxel, typically well under a micron behind the entry
    /// face, which is why the bound is not a full voxel-free micron.
    fn roomiest_pixel(
        pixels: usize,
        ray: impl Fn(usize) -> ([f64; 3], [f64; 3]),
        depth: impl Fn(usize) -> f64,
    ) -> (usize, [f64; 3], [f64; 3], f64, f64, f64) {
        let mut best: Option<(usize, [f64; 3], [f64; 3], f64, f64, f64, f64)> = None;
        for pixel in 0..pixels {
            let surface = depth(pixel);
            if !surface.is_finite() {
                continue;
            }
            let (origin, direction) = ray(pixel);
            let Some((near, far)) = fixture_box_crossing(origin, direction) else {
                continue;
            };
            let room = (surface - near).min(far - surface);
            if best.is_none_or(|current| room > current.6) {
                best = Some((pixel, origin, direction, near, surface, far, room));
            }
        }
        let (pixel, origin, direction, near, surface, far, room) =
            best.expect("the fixture camera must find volume on at least one ray");
        assert!(
            room >= 0.6,
            "no ray has 0.6 um of room on both sides of its surface (best {room})"
        );
        (pixel, origin, direction, near, surface, far)
    }

    /// The legacy pick against the server's own Vulkan frame: its paired attachment occludes an
    /// annotation behind the first-opacity surface and admits one in front, at the pixel the
    /// annotation projects to.
    ///
    /// This asserted `None`, which every camera and depth defect found on 2026-09-19 satisfied.
    /// Now that the depth is real and eye-relative and the NGFF bridge takes the physical Palace
    /// ray into the annotation frame, it pins the contract instead.
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
        let palace_rays = palace_frame::camera_rays_for_local_zarr(&root, size, controls).unwrap();
        let physical_rays = palace_rays
            .iter()
            .map(|ray| {
                let bridged = session
                    .palace_ray_to_physical(ray.origin.map(f64::from), ray.direction.map(f64::from))
                    .unwrap();
                // Palace's frame is already physical, so the bridge's unit factor is one.
                assert!((bridged.physical_distance_per_palace_unit - 1.0).abs() < 1e-6);
                (bridged.ray.origin, bridged.ray.direction)
            })
            .collect::<Vec<_>>();
        let (index, origin, direction, near, surface, far) = roomiest_pixel(
            physical_rays.len(),
            |pixel| physical_rays[pixel],
            |pixel| f64::from(depth.distances()[pixel]),
        );
        let pixel = [(index % 32) as u32, (index / 32) as u32];

        let behind = place_on_ray(
            &mut session,
            "behind",
            origin,
            direction,
            surface + (far - surface) / 2.0,
        );
        assert_eq!(
            pick_local_dataset_annotation(&session, &root, size, controls, pixel, depth).unwrap(),
            None,
            "an annotation behind the first-opacity surface must be occluded"
        );
        let front_distance = near + (surface - near) / 2.0;
        let front = place_on_ray(&mut session, "front", origin, direction, front_distance);
        assert_ne!(front, behind);
        let hit = pick_local_dataset_annotation(&session, &root, size, controls, pixel, depth)
            .unwrap()
            .expect("an annotation in front of the first-opacity surface must be selectable");
        assert_eq!(hit.annotation_id, front);
        assert!(
            (hit.distance - front_distance).abs() < 0.5,
            "picked at {} for an annotation placed at {front_distance}",
            hit.distance
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
