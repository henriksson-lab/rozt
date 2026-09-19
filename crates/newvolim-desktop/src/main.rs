//! Native desktop host. The UI remains the CSR bundle; this crate never renders Leptos SSR.

use std::{path::Path, sync::Mutex};

use newvolim_portable::routes::*;
use newvolim_portable::session::{
    LoadedLocalChunk, LocalLayerChunkPlan, LocalLayerRenderRequest, LocalSession, SessionSummary,
    SpatialChunkRegion,
};
use newvolim_render::{LayerRenderLimits, LayerRenderPlan};
use newvolim_scene::Annotation;
use palace_frame::{
    render_local_zarr_attachments, render_local_zarr_orthogonal_at_png,
    render_local_zarr_orthogonal_png, render_local_zarr_with_camera_attachments,
    render_synthetic_orthogonal_at_png, render_synthetic_orthogonal_png, render_synthetic_png,
    CameraControls, FrameSize,
};

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
) -> Result<newvolim_portable::session::LocalOmeZarrSource, String> {
    session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?
        .prepare_default_portable_image_layer()
        .map_err(|error| error.to_string())
}

/// Add an image layer reading its own local OME-Zarr, composited over the layers before it.
/// Like `open_local_omezarr`, the path is the user's own choice on their own machine.
#[tauri::command]
fn add_portable_image_layer(
    root: String,
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<Vec<newvolim_portable::session::LayerChannelSummary>, String> {
    let mut session = session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?;
    session
        .add_portable_image_layer(root)
        .map_err(|error| error.to_string())?;
    Ok(session.layer_channels())
}

/// Every image layer's channels for the transfer-function panel.
#[tauri::command]
fn layer_channels(
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<Vec<newvolim_portable::session::LayerChannelSummary>, String> {
    let session = session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?;
    Ok(session.layer_channels())
}

/// Replace one channel's transfer state; the next frame of every route renders it.
#[tauri::command]
fn set_channel_state(
    layer_id: u64,
    channel: usize,
    state: ChannelStateInput,
    session: tauri::State<'_, Mutex<LocalSession>>,
) -> Result<Vec<newvolim_portable::session::LayerChannelSummary>, String> {
    let mut session = session
        .lock()
        .map_err(|_| "desktop session lock was poisoned".to_owned())?;
    session
        .set_channel_state(newvolim_scene::LayerId(layer_id), channel, state.into_state()?)
        .map_err(|error| error.to_string())?;
    Ok(session.layer_channels())
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
    let owned = {
        let session = session
            .lock()
            .map_err(|_| "desktop session lock was poisoned".to_owned())?;
        session.clone()
    };
    let packet = native_portable_camera_draw_for_session(request, &owned)?;
    let rays = direct_route_physical_rays(&owned, &packet)?;
    let frame = direct_route_frame(&owned, &packet, &rays)?;
    FramePayload::portable_frame_attachments(frame.attachments)
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
            layer_channels,
            set_channel_state,
            add_portable_image_layer,
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

    /// The shipped webview drives the volume canvas through the scene route — render and pick
    /// alike — so the demand-driven frame is what users see and what their clicks are tested
    /// against. Pinned by reading the UI source: a wiring change is a contract change.
    #[test]
    fn webview_volume_canvas_is_wired_to_the_scene_route() {
        let source = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../newvolim-ui/index.html"),
        )
        .unwrap();
        assert!(source.contains(
            "const NEWVOLIM_NATIVE_VOLUME_COMMAND = \"render_native_portable_scene_camera_draw\";"
        ));
        assert!(source.contains(
            "const NEWVOLIM_NATIVE_VOLUME_PICK_COMMAND = \"pick_native_portable_scene_annotation\";"
        ));
        assert_eq!(source.matches("newvolimAdmitFrame(NEWVOLIM_NATIVE_VOLUME_COMMAND").count(), 1);
        assert_eq!(source.matches("newvolimDrawPayload(NEWVOLIM_NATIVE_VOLUME_COMMAND").count(), 1);
        assert_eq!(source.matches("invoke(NEWVOLIM_NATIVE_VOLUME_PICK_COMMAND").count(), 1);
        // The direct route is not invoked by the page any more, only named in a comment.
        for direct in ["\"render_native_portable_camera_draw\"", "\"pick_native_portable_annotation\""] {
            assert!(
                !source.contains(direct),
                "the webview still invokes the direct route: {direct}"
            );
        }
        // The desktop registers both scene commands the page names.
        let desktop = include_str!("main.rs");
        let handler = &desktop[desktop.find("tauri::generate_handler![").unwrap()..];
        let handler = &handler[..handler.find("])").unwrap()];
        assert!(handler.contains("render_native_portable_scene_camera_draw,"));
        assert!(handler.contains("pick_native_portable_scene_annotation,"));
    }

    /// The page's transfer panel talks to the two channel commands, and both are registered.
    #[test]
    fn webview_channel_panel_is_wired_to_the_channel_commands() {
        let source = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../newvolim-ui/index.html"),
        )
        .unwrap();
        assert!(source.contains("invoke(\"layer_channels\")"));
        assert!(source.contains("invoke(\"set_channel_state\", {"));
        assert!(source.contains("invoke(\"add_portable_image_layer\", {"));
        let ui = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../newvolim-ui/src/lib.rs"),
        )
        .unwrap();
        assert!(ui.contains("id=\"newvolim-channels\""));
        let desktop = include_str!("main.rs");
        let handler = &desktop[desktop.find("tauri::generate_handler![").unwrap()..];
        let handler = &handler[..handler.find("])").unwrap()];
        assert!(handler.contains("layer_channels,") && handler.contains("set_channel_state,"));
        assert!(handler.contains("add_portable_image_layer,"));
        assert!(ui.contains("id=\"newvolim-layer-path\""));
    }

    #[test]
    fn synthetic_orthogonal_payload_keeps_square_physical_panes() {
        let payload = render_synthetic_orthogonal_at(8, 8, 4, 4, 4).unwrap();
        assert_eq!(payload.aspect_ratios, [1.0; 3]);
        let json = serde_json::to_value(payload).unwrap();
        assert_eq!(json["aspectRatios"], serde_json::json!([1.0, 1.0, 1.0]));
    }
}
