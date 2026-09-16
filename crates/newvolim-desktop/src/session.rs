//! Desktop-owned local-data session boundary.
//!
//! The webview can ask the host to inspect a user-selected local OME-Zarr root, but it never
//! receives a filesystem handle or arbitrary server-side path access. Pixel transport is a later
//! concern; this module establishes the small, serializable state that transport will consume.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use newvolim_io::{
    level_transform, read_array_info, read_dataset_metadata, read_local_asset,
    CoordinateTransformation, DatasetMetadata, MetadataError, Multiscale,
};
use newvolim_render::{
    native_layer_descriptors, ImageLayerRenderRequest, LayerRenderError, LayerRenderLimits,
    LayerRenderPlan, NativeLayerDescriptor, NativePortableFrameInput, NativePortableSceneInput,
    NativePortableVolumeInput, PickRay, PortableChannelTransfer, PortablePageSubmission,
    PortablePageUpload, PortableScalarType, PortableSceneLayerInput, PortableVolumeChannel,
};
use newvolim_scene::{
    Annotation, AnnotationGeometry, AnnotationId, ChannelState, ChannelWindow, Layer, LayerId,
    LayerTransform, Scene,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default)]
pub struct LocalSession {
    dataset_root: Option<PathBuf>,
    metadata: Option<DatasetMetadata>,
    voxel_shape_xyz: Option<[u64; 3]>,
    scene: Scene,
    layer_sources: HashMap<LayerId, LocalOmeZarrSource>,
    next_annotation_id: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub dataset_root: Option<String>,
    pub multiscale_count: usize,
    pub channel_count: usize,
    pub renderer_connected: bool,
    pub voxel_shape_xyz: Option<[u64; 3]>,
}

/// A Palace camera ray expressed in the physical `[x, y, z]` coordinate system that owns
/// persisted annotations. `physical_distance_per_palace_unit` converts the renderer-owned
/// first-opacity sidecar before it is used as an annotation occlusion bound.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PalacePhysicalRay {
    pub ray: PickRay,
    pub physical_distance_per_palace_unit: f64,
}

/// A local, canonicalized OME-Zarr address for one image layer.  The layer plan selects display
/// channels; this source states exactly which array axis and timepoint those indices address.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalOmeZarrSource {
    pub root: String,
    pub multiscale_index: u32,
    pub level: u32,
    pub array_path: String,
    pub axes: Vec<String>,
    pub shape: Vec<u64>,
    pub chunk_shape: Vec<u64>,
    pub dtype: String,
    pub chunk_key_encoding: LocalChunkKeyEncoding,
    pub spatial_axes_xyz: [u32; 3],
    pub channel_axis: Option<u32>,
    pub time_axis: Option<u32>,
    pub timepoint: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LocalChunkKeyEncoding {
    V2Dot,
    V3Slash,
}

/// An image-layer request coupled to the one local array it is allowed to read.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalLayerRenderRequest {
    pub layer: ImageLayerRenderRequest,
    pub source: LocalOmeZarrSource,
}

/// One concrete asset read, preserving the array's declared coordinate order.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalChunkAddress {
    pub asset_path: String,
    pub coordinates: Vec<u64>,
    pub channel: u32,
    pub timepoint: u32,
    pub spatial_chunk_xyz: [u64; 3],
    pub logical_extent: Vec<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadedLocalChunk {
    pub address: LocalChunkAddress,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalLayerChunkPlan {
    pub request: LocalLayerRenderRequest,
    pub chunks: Vec<LocalChunkAddress>,
}

/// An inclusive-start, exclusive-end box in spatial *chunk*, not voxel, coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpatialChunkRegion {
    pub origin_xyz: [u64; 3],
    pub extent_xyz: [u32; 3],
}

impl SpatialChunkRegion {
    pub const fn new(origin_xyz: [u64; 3], extent_xyz: [u32; 3]) -> Self {
        Self {
            origin_xyz,
            extent_xyz,
        }
    }
}

/// A portable, deliberately small annotation document. The canonical dataset root prevents an
/// import from silently attaching physical coordinates to a different volume.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct AnnotationDocument {
    format_version: u32,
    dataset_root: String,
    annotations: Vec<Annotation>,
}

const ANNOTATION_DOCUMENT_VERSION: u32 = 1;
const MAX_ANNOTATION_VERTICES: usize = 4_096;

#[derive(Debug)]
pub enum SessionError {
    Metadata(MetadataError),
    NotDirectory(PathBuf),
    LayerRender(LayerRenderError),
    LayerSource(String),
    Annotation(String),
    AnnotationDocument(String),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Metadata(error) => write!(formatter, "could not open OME-Zarr metadata: {error}"),
            Self::NotDirectory(path) => write!(
                formatter,
                "OME-Zarr root is not a directory: {}",
                path.display()
            ),
            Self::LayerRender(error) => {
                write!(formatter, "could not prepare layer render plan: {error}")
            }
            Self::LayerSource(message) => write!(formatter, "layer source error: {message}"),
            Self::Annotation(message) => write!(formatter, "could not place annotation: {message}"),
            Self::AnnotationDocument(message) => {
                write!(formatter, "annotation document error: {message}")
            }
        }
    }
}

impl std::error::Error for SessionError {}

impl From<MetadataError> for SessionError {
    fn from(value: MetadataError) -> Self {
        Self::Metadata(value)
    }
}

impl From<LayerRenderError> for SessionError {
    fn from(value: LayerRenderError) -> Self {
        Self::LayerRender(value)
    }
}

impl LocalSession {
    /// Explicitly prepare one level-zero image layer for the bounded native portable recorder.
    /// Opening metadata alone never grants a renderer a source binding; the UI calls this once
    /// the user elects the local native route.
    pub fn prepare_default_portable_image_layer(
        &mut self,
    ) -> Result<LocalOmeZarrSource, SessionError> {
        if !self.scene.layers().is_empty() {
            return Err(SessionError::LayerSource(
                "default portable layer can only be prepared for an empty scene".into(),
            ));
        }
        let root = self.dataset_root.as_ref().ok_or_else(|| {
            SessionError::LayerSource(
                "open a local OME-Zarr dataset before preparing a layer".into(),
            )
        })?;
        let metadata = self.metadata.as_ref().ok_or_else(|| {
            SessionError::LayerSource("opened dataset has no parsed metadata".into())
        })?;
        let source = LocalOmeZarrSource::level_zero(root, metadata)?;
        let multiscale = metadata.multiscales.first().ok_or_else(|| {
            SessionError::LayerSource("dataset has no multiscale metadata".into())
        })?;
        let transform = portable_axis_aligned_transform(multiscale)?;
        let channel_count = source
            .channel_axis
            .map(|axis| source.shape[axis as usize])
            .unwrap_or(1);
        if channel_count == 0 {
            return Err(SessionError::LayerSource(
                "source has zero display channels".into(),
            ));
        }
        let mut active_channels = metadata
            .omero
            .as_ref()
            .map(|omero| {
                omero
                    .channels
                    .iter()
                    .enumerate()
                    .filter_map(|(index, channel)| channel.active.then_some(index))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if active_channels.is_empty() {
            active_channels.push(0);
        }
        let channel_count = usize::try_from(channel_count).map_err(|_| {
            SessionError::LayerSource("source channel count does not fit this platform".into())
        })?;
        if active_channels.iter().any(|&index| index >= channel_count) {
            return Err(SessionError::LayerSource(format!(
                "an OME display channel exceeds source channel count {channel_count}"
            )));
        }
        if active_channels.len() > 4 {
            return Err(SessionError::LayerSource(format!(
                "OME selects {} display channels but the portable pool has four page bindings",
                active_channels.len()
            )));
        }
        // This vector's position is the source C address. Preserve a disabled prefix rather
        // than collapsing the selected OME channel to slot zero.
        let disabled_window = ChannelWindow::new(0.0, 65_535.0)
            .map_err(|error| SessionError::LayerSource(error.to_string()))?;
        let channels = (0..channel_count)
            .map(|index| {
                let display = metadata
                    .omero
                    .as_ref()
                    .and_then(|omero| omero.channels.get(index));
                let enabled = active_channels.contains(&index);
                let color_srgb = display
                    .and_then(|channel| channel.color.as_deref())
                    .and_then(parse_srgb_hex)
                    .unwrap_or([255, 255, 255]);
                let window = display
                    .and_then(|channel| channel.window)
                    .map(|window| [window.start, window.end])
                    .unwrap_or([0.0, 65_535.0]);
                let selected_window =
                    ChannelWindow::new(window[0], window[1]).map_err(|error| {
                        SessionError::LayerSource(format!("invalid OME display window: {error}"))
                    })?;
                ChannelState::new(
                    enabled,
                    if enabled { color_srgb } else { [255, 255, 255] },
                    if enabled {
                        selected_window
                    } else {
                        disabled_window
                    },
                    1.0,
                )
                .map_err(|error| SessionError::LayerSource(error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let layer_id = LayerId(0);
        self.scene
            .insert_layer(Layer::image(
                layer_id,
                "OME-Zarr level 0",
                transform,
                channels,
            ))
            .map_err(|error| SessionError::LayerSource(error.to_string()))?;
        self.layer_sources.insert(layer_id, source.clone());
        Ok(source)
    }
    pub fn open_local_omezarr(
        &mut self,
        root: impl AsRef<Path>,
    ) -> Result<SessionSummary, SessionError> {
        // Canonicalization makes the persisted/displayed path unambiguous and rejects dangling
        // paths before parsing untrusted metadata.
        let root = root
            .as_ref()
            .canonicalize()
            .map_err(|error| SessionError::Metadata(MetadataError::Io(error)))?;
        if !root.is_dir() {
            return Err(SessionError::NotDirectory(root));
        }
        let metadata = read_dataset_metadata(&root)?;
        let voxel_shape_xyz = metadata
            .multiscales
            .first()
            .and_then(|multiscale| {
                multiscale
                    .datasets
                    .first()
                    .map(|dataset| (multiscale, dataset))
            })
            .and_then(|(multiscale, dataset)| {
                read_array_info(&root, &dataset.path)
                    .ok()
                    .and_then(|array| {
                        let axis = |name| {
                            multiscale
                                .axes
                                .iter()
                                .position(|axis| axis.name.eq_ignore_ascii_case(name))
                        };
                        Some([
                            array.shape.get(axis("x")?)?.to_owned(),
                            array.shape.get(axis("y")?)?.to_owned(),
                            array.shape.get(axis("z")?)?.to_owned(),
                        ])
                    })
            });
        self.dataset_root = Some(root);
        self.metadata = Some(metadata);
        self.voxel_shape_xyz = voxel_shape_xyz;
        // Physical annotation coordinates are meaningful only for the dataset that supplied
        // their NGFF transform. Never carry them into a newly opened root.
        self.scene = Scene::default();
        self.next_annotation_id = 0;
        Ok(self.summary())
    }

    pub fn summary(&self) -> SessionSummary {
        self.summary_with_shape(self.voxel_shape_xyz)
    }

    fn summary_with_shape(&self, voxel_shape_xyz: Option<[u64; 3]>) -> SessionSummary {
        let metadata = self.metadata.as_ref();
        SessionSummary {
            dataset_root: self
                .dataset_root
                .as_ref()
                .map(|path| path.display().to_string()),
            multiscale_count: metadata.map_or(0, |value| value.multiscales.len()),
            channel_count: metadata.map_or(0, |value| {
                value.omero.as_ref().map_or(0, |omero| omero.channels.len())
            }),
            // Opening metadata alone is not a completed render or streamed frame. The separate
            // render commands provide Palace readback, so do not use this flag as a renderer
            // capability probe.
            renderer_connected: false,
            voxel_shape_xyz,
        }
    }

    pub fn dataset_root(&self) -> Option<PathBuf> {
        self.dataset_root.clone()
    }

    /// Clamp an untrusted UI/IPC crosshair to the opened level-zero array. This keeps the
    /// desktop and remote render paths aligned: pane clicks select a voxel, while direct IPC
    /// callers cannot turn an oversized coordinate into an invalid slice request.
    pub fn clamp_crosshair_xyz(&self, requested: [u32; 3]) -> Result<[u32; 3], SessionError> {
        let shape = self.voxel_shape_xyz.ok_or_else(|| {
            SessionError::Annotation(
                "opened OME-Zarr dataset has no usable level-zero X/Y/Z array shape".into(),
            )
        })?;
        let clamp_axis = |axis: usize| {
            let maximum = shape[axis].checked_sub(1).ok_or_else(|| {
                SessionError::Annotation("level-zero array has an empty spatial axis".into())
            })?;
            u32::try_from(u64::from(requested[axis]).min(maximum)).map_err(|_| {
                SessionError::Annotation(
                    "level-zero array extent cannot be represented by the slice API".into(),
                )
            })
        };
        Ok([clamp_axis(0)?, clamp_axis(1)?, clamp_axis(2)?])
    }

    pub fn annotations(&self) -> &[Annotation] {
        self.scene.annotations()
    }

    /// Translate the session-owned scene state into the bounded request consumed by a local
    /// renderer.  The host owns this policy boundary so the webview cannot quietly select a
    /// different layer order or channel page from the persisted scene.
    pub fn layer_render_plan(
        &self,
        limits: LayerRenderLimits,
    ) -> Result<LayerRenderPlan, SessionError> {
        Ok(LayerRenderPlan::from_scene(&self.scene, limits)?)
    }

    /// Bind an existing image layer to level zero of the currently opened local OME-Zarr root.
    /// Callers that add another source must bind it explicitly; a layer ID alone never implies a
    /// filesystem path or channel-axis convention.
    pub fn bind_layer_to_open_dataset(&mut self, layer_id: LayerId) -> Result<(), SessionError> {
        let layer = self.scene.layer(layer_id).ok_or_else(|| {
            SessionError::LayerSource(format!("scene has no layer {}", layer_id.0))
        })?;
        if layer.kind != newvolim_scene::LayerKind::Image {
            return Err(SessionError::LayerSource(format!(
                "layer {} is not an image layer",
                layer_id.0
            )));
        }
        let root = self.dataset_root.as_ref().ok_or_else(|| {
            SessionError::LayerSource("open a local OME-Zarr dataset before binding a layer".into())
        })?;
        let metadata = self.metadata.as_ref().ok_or_else(|| {
            SessionError::LayerSource("opened dataset has no parsed metadata".into())
        })?;
        let source = LocalOmeZarrSource::level_zero(root, metadata)?;
        self.layer_sources.insert(layer_id, source);
        Ok(())
    }

    /// Produce renderer-ready image requests coupled to their authorized local OME-Zarr array.
    /// A selected C index is checked against the source's C axis here, after scene policy has
    /// selected visible/enabled content but before any renderer allocates or reads a chunk.
    pub fn local_layer_render_requests(
        &self,
        limits: LayerRenderLimits,
    ) -> Result<Vec<LocalLayerRenderRequest>, SessionError> {
        let plan = self.layer_render_plan(limits)?;
        plan.image_layers
            .into_iter()
            .map(|layer| {
                let source = self.layer_sources.get(&layer.layer_id).cloned().ok_or_else(|| {
                    SessionError::LayerSource(format!(
                        "image layer {} has no bound local OME-Zarr source",
                        layer.layer_id.0
                    ))
                })?;
                let channel_count = source
                    .channel_axis
                    .map(|axis| source.shape[axis as usize])
                    .unwrap_or(1);
                for channel in &layer.channels {
                    if u64::from(channel.source_index) >= channel_count {
                        return Err(SessionError::LayerSource(format!(
                            "image layer {} selected C={} but its source has {channel_count} channel(s)",
                            layer.layer_id.0, channel.source_index
                        )));
                    }
                }
                Ok(LocalLayerRenderRequest { layer, source })
            })
            .collect()
    }

    /// Native portable-renderer admission: descriptors and local source bindings are derived
    /// from the same ordered scene plan, so page allocation cannot diverge from chunk selection.
    pub fn native_layer_admission(
        &self,
        limits: LayerRenderLimits,
    ) -> Result<(Vec<NativeLayerDescriptor>, Vec<LocalLayerRenderRequest>), SessionError> {
        let plan = self.layer_render_plan(limits)?;
        let descriptors = native_layer_descriptors(&plan)?;
        let requests = self.local_layer_render_requests(limits)?;
        if descriptors.len() != requests.len()
            || descriptors
                .iter()
                .zip(&requests)
                .any(|(descriptor, request)| descriptor.layer_id != request.layer.layer_id)
        {
            return Err(SessionError::LayerSource(
                "native layer descriptor order does not match local source bindings".into(),
            ));
        }
        Ok((descriptors, requests))
    }

    /// Turns ordered local chunk reads into the exact four-page input used by the portable wgpu
    /// recorder.  A descriptor page is assigned to its corresponding enabled channel, preserving
    /// scene order and the descriptor's physical transform for the later draw step.
    pub fn native_portable_page_admission(
        &self,
        descriptors: Vec<NativeLayerDescriptor>,
        plans: &[LocalLayerChunkPlan],
        loaded: &[LoadedLocalChunk],
    ) -> Result<NativePortableVolumeInput, SessionError> {
        if descriptors.len() != plans.len() {
            return Err(SessionError::LayerSource(
                "native descriptors do not match local chunk plans".into(),
            ));
        }
        if let ([descriptor], [plan]) = (descriptors.as_slice(), plans) {
            if descriptor.layer_id != plan.request.layer.layer_id {
                return Err(SessionError::LayerSource(
                    "native descriptor does not match its local layer request".into(),
                ));
            }
            if loaded.len() != plan.chunks.len()
                || loaded
                    .iter()
                    .zip(&plan.chunks)
                    .any(|(loaded, planned)| loaded.address != *planned)
            {
                return Err(SessionError::LayerSource(
                    "loaded chunks are not in the approved local chunk-plan order".into(),
                ));
            }
            let page_words = (PortablePageSubmission::PAGE_BYTES / 4) as usize;
            let mut dimensions_xyz = None;
            let mut uploads = Vec::new();
            let mut channels = Vec::new();
            let mut next_page = 0_u32;
            for channel in &plan.request.layer.channels {
                let tiles = loaded
                    .iter()
                    .filter(|loaded| loaded.address.channel == channel.source_index)
                    .map(|loaded| {
                        Ok((
                            loaded.address.clone(),
                            portable_words_xyz(
                                &loaded.bytes,
                                &loaded.address,
                                &plan.request.source,
                            )?,
                        ))
                    })
                    .collect::<Result<Vec<_>, SessionError>>()?;
                let (words, dimensions, _) =
                    assemble_portable_xyz_tiles(&tiles, &plan.request.source)?;
                if dimensions_xyz
                    .replace(dimensions)
                    .is_some_and(|previous| previous != dimensions)
                {
                    return Err(SessionError::LayerSource(
                        "selected portable channels do not share one XYZ extent".into(),
                    ));
                }
                let page_count = u32::try_from(words.chunks(page_words).len()).map_err(|_| {
                    SessionError::LayerSource("portable channel page count overflows u32".into())
                })?;
                let end = next_page.checked_add(page_count).ok_or_else(|| {
                    SessionError::LayerSource("portable channel pages overflow u32".into())
                })?;
                if page_count == 0 || end > PortablePageSubmission::PAGE_COUNT {
                    return Err(SessionError::LayerSource(
                        "selected portable channels exceed four static pages".into(),
                    ));
                }
                uploads.extend(words.chunks(page_words).enumerate().map(|(offset, words)| {
                    PortablePageUpload {
                        page: next_page + offset as u32,
                        words: words.to_vec(),
                    }
                }));
                channels.push(PortableVolumeChannel {
                    page_offset: next_page,
                    page_count,
                    transfer: PortableChannelTransfer::from(channel),
                });
                next_page = end;
            }
            let frame = NativePortableFrameInput::new(
                vec![NativeLayerDescriptor {
                    page_offset: 0,
                    page_count: next_page,
                    ..descriptor.clone()
                }],
                PortablePageSubmission::from_uploads(uploads)?,
            )?;
            return NativePortableVolumeInput::new_channels(
                frame,
                dimensions_xyz.ok_or_else(|| {
                    SessionError::LayerSource(
                        "direct portable volume has no channel dimensions".into(),
                    )
                })?,
                portable_scalar_type(&plan.request.source.dtype)?,
                channels,
            )
            .map_err(SessionError::from);
        }
        let mut dimensions_xyz = None;
        let mut scalar_type = None;
        let mut transfer = None;
        let mut loaded_offset = 0_usize;
        let mut uploads = Vec::new();
        for (descriptor, plan) in descriptors.iter().zip(plans) {
            if descriptor.layer_id != plan.request.layer.layer_id
                || descriptor.page_count as usize != plan.request.layer.channels.len()
            {
                return Err(SessionError::LayerSource(
                    "native descriptor does not match its local layer request".into(),
                ));
            }
            for (channel_offset, channel) in plan.request.layer.channels.iter().enumerate() {
                let chunk = plan
                    .chunks
                    .iter()
                    .find(|chunk| chunk.channel == channel.source_index)
                    .ok_or_else(|| {
                        SessionError::LayerSource(format!(
                            "layer {} has no chunk for selected channel {}",
                            descriptor.layer_id.0, channel.source_index
                        ))
                    })?;
                if plan
                    .chunks
                    .iter()
                    .filter(|candidate| candidate.channel == channel.source_index)
                    .count()
                    != 1
                {
                    return Err(SessionError::LayerSource(format!(
                        "layer {} channel {} needs multiple chunks; portable page admission accepts one spatial chunk per channel",
                        descriptor.layer_id.0, channel.source_index
                    )));
                }
                let loaded_chunk = loaded.get(loaded_offset).ok_or_else(|| {
                    SessionError::LayerSource(
                        "loaded chunk list is shorter than its local chunk plan".into(),
                    )
                })?;
                loaded_offset += 1;
                if loaded_chunk.address != *chunk {
                    return Err(SessionError::LayerSource(
                        "loaded chunks are not in the approved local chunk-plan order".into(),
                    ));
                }
                let words = portable_words_xyz(
                    &loaded_chunk.bytes,
                    &loaded_chunk.address,
                    &plan.request.source,
                )?;
                let chunk_dimensions = portable_dimensions_xyz(
                    &loaded_chunk.address,
                    &plan.request.source,
                    descriptor.layer_id,
                )?;
                let chunk_scalar = portable_scalar_type(&plan.request.source.dtype)?;
                let chunk_transfer = PortableChannelTransfer::from(channel);
                if dimensions_xyz.replace(chunk_dimensions).is_some()
                    || scalar_type.replace(chunk_scalar).is_some()
                    || transfer.replace(chunk_transfer).is_some()
                {
                    return Err(SessionError::LayerSource(
                        "direct portable admission currently accepts one selected image layer; ordered multi-layer composition needs its own scene packet".into(),
                    ));
                }
                uploads.push(PortablePageUpload {
                    page: descriptor.page_offset + channel_offset as u32,
                    words,
                });
            }
        }
        if loaded_offset != loaded.len() {
            return Err(SessionError::LayerSource(
                "loaded chunk list contains entries outside its approved local chunk plan".into(),
            ));
        }
        let submission = PortablePageSubmission::from_uploads(uploads)?;
        let frame = NativePortableFrameInput::new(descriptors, submission)?;
        NativePortableVolumeInput::new(
            frame,
            dimensions_xyz.ok_or_else(|| {
                SessionError::LayerSource("direct portable volume has no chunk dimensions".into())
            })?,
            scalar_type.ok_or_else(|| {
                SessionError::LayerSource("direct portable volume has no scalar type".into())
            })?,
            transfer.ok_or_else(|| {
                SessionError::LayerSource("direct portable volume has no channel transfer".into())
            })?,
        )
        .map_err(SessionError::from)
    }

    /// Admit every selected local image layer into one ordered scene packet for the world-ray
    /// recorder. Unlike the direct-volume specialization, each layer retains its own XYZ extent,
    /// voxel origin, transform, and channel ranges while all payloads share four static pages.
    pub fn native_portable_scene_page_admission(
        &self,
        descriptors: Vec<NativeLayerDescriptor>,
        plans: &[LocalLayerChunkPlan],
        loaded: &[LoadedLocalChunk],
    ) -> Result<NativePortableSceneInput, SessionError> {
        if descriptors.len() != plans.len() {
            return Err(SessionError::LayerSource(
                "native descriptors do not match local chunk plans".into(),
            ));
        }
        let page_words = (PortablePageSubmission::PAGE_BYTES / 4) as usize;
        let mut loaded_offset = 0_usize;
        let mut next_page = 0_u32;
        let mut uploads = Vec::new();
        let mut scene_descriptors = Vec::new();
        let mut layers = Vec::new();
        for (descriptor, plan) in descriptors.iter().zip(plans) {
            let plan_loaded = loaded
                .get(loaded_offset..loaded_offset + plan.chunks.len())
                .ok_or_else(|| {
                    SessionError::LayerSource(
                        "loaded chunk list is shorter than its approved local chunk plan".into(),
                    )
                })?;
            loaded_offset += plan.chunks.len();
            if descriptor.layer_id != plan.request.layer.layer_id
                || plan_loaded
                    .iter()
                    .zip(&plan.chunks)
                    .any(|(loaded, planned)| loaded.address != *planned)
            {
                return Err(SessionError::LayerSource(
                    "loaded chunks are not in approved local layer-plan order".into(),
                ));
            }
            let first_chunk = plan.chunks.first().ok_or_else(|| {
                SessionError::LayerSource("portable scene layer has no chunks".into())
            })?;
            let voxel_origin_xyz = std::array::from_fn(|axis| {
                let source_axis = plan.request.source.spatial_axes_xyz[axis] as usize;
                first_chunk.spatial_chunk_xyz[axis]
                    .checked_mul(plan.request.source.chunk_shape[source_axis])
                    .ok_or_else(|| {
                        SessionError::LayerSource(
                            "portable scene voxel origin overflows u64".into(),
                        )
                    })
            });
            let [origin_x, origin_y, origin_z] = voxel_origin_xyz;
            let voxel_origin_xyz = [origin_x?, origin_y?, origin_z?];
            let mut dimensions_xyz = None;
            let mut channels = Vec::new();
            for channel in &plan.request.layer.channels {
                let tiles = plan_loaded
                    .iter()
                    .filter(|loaded| loaded.address.channel == channel.source_index)
                    .map(|loaded| {
                        Ok((
                            loaded.address.clone(),
                            portable_words_xyz(
                                &loaded.bytes,
                                &loaded.address,
                                &plan.request.source,
                            )?,
                        ))
                    })
                    .collect::<Result<Vec<_>, SessionError>>()?;
                let (words, dimensions, _) =
                    assemble_portable_xyz_tiles(&tiles, &plan.request.source)?;
                if dimensions_xyz
                    .replace(dimensions)
                    .is_some_and(|previous| previous != dimensions)
                {
                    return Err(SessionError::LayerSource(
                        "portable scene channels do not share one XYZ extent".into(),
                    ));
                }
                let page_count = u32::try_from(words.chunks(page_words).len()).map_err(|_| {
                    SessionError::LayerSource(
                        "portable scene channel page count overflows u32".into(),
                    )
                })?;
                let end = next_page.checked_add(page_count).ok_or_else(|| {
                    SessionError::LayerSource("portable scene pages overflow u32".into())
                })?;
                if page_count == 0 || end > PortablePageSubmission::PAGE_COUNT {
                    return Err(SessionError::LayerSource(
                        "selected portable scene exceeds four static pages".into(),
                    ));
                }
                uploads.extend(words.chunks(page_words).enumerate().map(|(offset, words)| {
                    PortablePageUpload {
                        page: next_page + offset as u32,
                        words: words.to_vec(),
                    }
                }));
                channels.push(PortableVolumeChannel {
                    page_offset: next_page,
                    page_count,
                    transfer: PortableChannelTransfer::from(channel),
                });
                next_page = end;
            }
            let dimensions_xyz = dimensions_xyz.ok_or_else(|| {
                SessionError::LayerSource("portable scene layer has no channel dimensions".into())
            })?;
            scene_descriptors.push(NativeLayerDescriptor {
                layer_id: descriptor.layer_id,
                page_offset: channels
                    .first()
                    .map(|channel| channel.page_offset)
                    .unwrap_or(next_page),
                page_count: channels.iter().map(|channel| channel.page_count).sum(),
                transform: descriptor.transform,
            });
            layers.push(PortableSceneLayerInput {
                layer_id: descriptor.layer_id,
                transform: descriptor.transform,
                voxel_origin_xyz,
                dimensions_xyz,
                scalar_type: portable_scalar_type(&plan.request.source.dtype)?,
                channels,
            });
        }
        if loaded_offset != loaded.len() {
            return Err(SessionError::LayerSource(
                "loaded chunks contain entries outside approved local chunk plans".into(),
            ));
        }
        NativePortableSceneInput::new(
            NativePortableFrameInput::new(
                scene_descriptors,
                PortablePageSubmission::from_uploads(uploads)?,
            )?,
            layers,
        )
        .map_err(SessionError::from)
    }

    /// Expand bound layer requests into a bounded set of concrete Zarr chunk assets.  `region`
    /// is expressed in XYZ chunk coordinates, while every resulting `coordinates` vector keeps
    /// the dataset's original axis order for direct use by a v2/v3 store adapter.
    pub fn local_layer_chunk_plan(
        &self,
        limits: LayerRenderLimits,
        region: SpatialChunkRegion,
        max_chunk_addresses: usize,
    ) -> Result<Vec<LocalLayerChunkPlan>, SessionError> {
        if region.extent_xyz.contains(&0) {
            return Err(SessionError::LayerSource(
                "spatial chunk region extent must be non-zero".into(),
            ));
        }
        if max_chunk_addresses == 0 {
            return Err(SessionError::LayerSource(
                "chunk-address capacity must be non-zero".into(),
            ));
        }
        let requests = self.local_layer_render_requests(limits)?;
        let mut remaining = max_chunk_addresses;
        requests
            .into_iter()
            .map(|request| {
                let source = &request.source;
                let spatial_counts: [u64; 3] = std::array::from_fn(|xyz| {
                    let axis = source.spatial_axes_xyz[xyz] as usize;
                    source.shape[axis].div_ceil(source.chunk_shape[axis])
                });
                let end_xyz = [
                    region.origin_xyz[0]
                        .checked_add(u64::from(region.extent_xyz[0]))
                        .ok_or_else(|| {
                            SessionError::LayerSource("spatial chunk region overflows u64".into())
                        })?,
                    region.origin_xyz[1]
                        .checked_add(u64::from(region.extent_xyz[1]))
                        .ok_or_else(|| {
                            SessionError::LayerSource("spatial chunk region overflows u64".into())
                        })?,
                    region.origin_xyz[2]
                        .checked_add(u64::from(region.extent_xyz[2]))
                        .ok_or_else(|| {
                            SessionError::LayerSource("spatial chunk region overflows u64".into())
                        })?,
                ];
                if region
                    .origin_xyz
                    .iter()
                    .zip(end_xyz.iter().zip(spatial_counts.iter()))
                    .any(|(origin, (end, count))| *origin >= *count || *end > *count)
                {
                    return Err(SessionError::LayerSource(format!(
                        "spatial chunk region {:?}+{:?} is outside source chunk grid {:?}",
                        region.origin_xyz, region.extent_xyz, spatial_counts
                    )));
                }
                let count = usize::try_from(region.extent_xyz[0])
                    .ok()
                    .and_then(|count| count.checked_mul(region.extent_xyz[1] as usize))
                    .and_then(|count| count.checked_mul(region.extent_xyz[2] as usize))
                    .and_then(|count| count.checked_mul(request.layer.channels.len()))
                    .ok_or_else(|| SessionError::LayerSource("chunk address count overflows usize".into()))?;
                if count > remaining {
                    return Err(SessionError::LayerSource(format!(
                        "chunk request needs {count} addresses but only {remaining} remain in its bound"
                    )));
                }
                remaining -= count;
                let mut chunks = Vec::with_capacity(count);
                for channel in &request.layer.channels {
                    for z in region.origin_xyz[2]..end_xyz[2] {
                        for y in region.origin_xyz[1]..end_xyz[1] {
                            for x in region.origin_xyz[0]..end_xyz[0] {
                                let mut coordinates = vec![0_u64; source.axes.len()];
                                coordinates[source.spatial_axes_xyz[0] as usize] = x;
                                coordinates[source.spatial_axes_xyz[1] as usize] = y;
                                coordinates[source.spatial_axes_xyz[2] as usize] = z;
                                if let Some(axis) = source.channel_axis {
                                    coordinates[axis as usize] = u64::from(channel.source_index);
                                } else if channel.source_index != 0 {
                                    return Err(SessionError::LayerSource(format!(
                                        "layer {} selects C={} but source has no C axis",
                                        request.layer.layer_id.0, channel.source_index
                                    )));
                                }
                                if let Some(axis) = source.time_axis {
                                    coordinates[axis as usize] = u64::from(source.timepoint);
                                }
                                let coordinate_key = match source.chunk_key_encoding {
                                    LocalChunkKeyEncoding::V2Dot => coordinates
                                        .iter()
                                        .map(u64::to_string)
                                        .collect::<Vec<_>>()
                                        .join("."),
                                    LocalChunkKeyEncoding::V3Slash => coordinates
                                        .iter()
                                        .map(u64::to_string)
                                        .collect::<Vec<_>>()
                                        .join("/"),
                                };
                                let asset_path = match source.chunk_key_encoding {
                                    LocalChunkKeyEncoding::V2Dot => {
                                        format!("{}/{}", source.array_path, coordinate_key)
                                    }
                                    LocalChunkKeyEncoding::V3Slash => {
                                        format!("{}/c/{}", source.array_path, coordinate_key)
                                    }
                                };
                                let logical_extent = coordinates
                                    .iter()
                                    .zip(source.shape.iter().zip(source.chunk_shape.iter()))
                                    .map(|(coordinate, (shape, chunk))| {
                                        shape.saturating_sub(coordinate.saturating_mul(*chunk)).min(*chunk)
                                    })
                                    .collect();
                                chunks.push(LocalChunkAddress {
                                    asset_path,
                                    coordinates,
                                    channel: channel.source_index,
                                    timepoint: source.timepoint,
                                    spatial_chunk_xyz: [x, y, z],
                                    logical_extent,
                                });
                            }
                        }
                    }
                }
                Ok(LocalLayerChunkPlan { request, chunks })
            })
            .collect()
    }

    /// Read only addresses emitted by [`Self::local_layer_chunk_plan`].  The plan remains the
    /// authority for both path traversal protection and the edge-aware byte count expected from
    /// an uncompressed scalar chunk.
    pub fn read_local_layer_chunks(
        &self,
        plans: &[LocalLayerChunkPlan],
        max_asset_bytes: u64,
        max_total_bytes: u64,
    ) -> Result<Vec<LoadedLocalChunk>, SessionError> {
        if max_asset_bytes == 0 || max_total_bytes == 0 {
            return Err(SessionError::LayerSource(
                "chunk byte budgets must be non-zero".into(),
            ));
        }
        let root = self.dataset_root.as_ref().ok_or_else(|| {
            SessionError::LayerSource("open a local OME-Zarr dataset before reading chunks".into())
        })?;
        let mut remaining = max_total_bytes;
        let mut loaded = Vec::new();
        for plan in plans {
            if plan.request.source.root != root.display().to_string() {
                return Err(SessionError::LayerSource(
                    "chunk plan source does not match the opened canonical dataset root".into(),
                ));
            }
            let bytes_per_element = match plan.request.source.dtype.as_str() {
                "uint8" | "|u1" => 1_u64,
                "uint16" | "<u2" => 2,
                "uint32" | "<u4" => 4,
                dtype => {
                    return Err(SessionError::LayerSource(format!(
                        "native chunk loader supports uncompressed uint8/uint16/uint32, not {dtype}"
                    )))
                }
            };
            for address in &plan.chunks {
                let expected = address
                    .logical_extent
                    .iter()
                    .try_fold(bytes_per_element, |bytes, extent| {
                        bytes.checked_mul(*extent)
                    })
                    .ok_or_else(|| {
                        SessionError::LayerSource("chunk byte length overflows u64".into())
                    })?;
                if expected > max_asset_bytes || expected > remaining {
                    return Err(SessionError::LayerSource(format!(
                        "chunk {} requires {expected} bytes, exceeding remaining budget {} or per-asset budget {max_asset_bytes}",
                        address.asset_path, remaining
                    )));
                }
                let bytes = read_local_asset(root, &address.asset_path, expected)
                    .map_err(|error| SessionError::LayerSource(error.to_string()))?;
                if bytes.len() as u64 != expected {
                    return Err(SessionError::LayerSource(format!(
                        "chunk {} has {} bytes, expected edge-aware {expected}",
                        address.asset_path,
                        bytes.len()
                    )));
                }
                remaining -= expected;
                loaded.push(LoadedLocalChunk {
                    address: address.clone(),
                    bytes,
                });
            }
        }
        Ok(loaded)
    }

    /// Converts a unit Palace camera ray in the opened level-zero array's native axis order to
    /// physical annotation coordinates. Palace currently embeds an NGFF source with the first
    /// *dataset-local* scale operation; this bridge intentionally mirrors that representation,
    /// then applies the complete NGFF transform (including shared transforms and translation)
    /// before reordering to `[x, y, z]`.
    ///
    /// The returned distance factor is essential: the PFM attachment measures Palace-local
    /// world distance, while annotation picking runs in the physical NGFF coordinate system.
    /// A translated or anisotropic transform therefore cannot silently compare incompatible
    /// units.
    pub fn palace_ray_to_physical(
        &self,
        palace_origin: [f64; 3],
        palace_direction: [f64; 3],
    ) -> Result<PalacePhysicalRay, SessionError> {
        if palace_origin
            .iter()
            .chain(palace_direction.iter())
            .any(|value| !value.is_finite())
        {
            return Err(SessionError::Annotation(
                "Palace camera ray contains a non-finite component".into(),
            ));
        }
        let metadata = self.metadata.as_ref().ok_or_else(|| {
            SessionError::Annotation(
                "open an OME-Zarr dataset before converting a camera ray".into(),
            )
        })?;
        let multiscale = metadata
            .multiscales
            .first()
            .ok_or_else(|| SessionError::Annotation("dataset has no multiscale metadata".into()))?;
        if multiscale.axes.len() != 3 {
            return Err(SessionError::Annotation(format!(
                "Palace ray conversion requires exactly three array axes, found {}",
                multiscale.axes.len()
            )));
        }
        let dataset = multiscale.datasets.first().ok_or_else(|| {
            SessionError::Annotation("dataset has no level-zero array metadata".into())
        })?;
        let spacing = dataset
            .coordinate_transformations
            .iter()
            .find_map(|transform| match transform {
                CoordinateTransformation::Scale { scale } => Some(scale.as_slice()),
                _ => None,
            })
            .unwrap_or(&[1.0, 1.0, 1.0]);
        if spacing.len() != multiscale.axes.len()
            || spacing
                .iter()
                .any(|value| !value.is_finite() || *value <= 0.0)
        {
            return Err(SessionError::Annotation(
                "Palace level-zero scale must be finite, positive, and match the array axes".into(),
            ));
        }
        let axis_index = |name: &str| {
            multiscale
                .axes
                .iter()
                .position(|axis| axis.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| SessionError::Annotation(format!("multiscale lacks a {name} axis")))
        };
        let x = axis_index("x")?;
        let y = axis_index("y")?;
        let z = axis_index("z")?;
        let voxel_origin = palace_origin
            .iter()
            .zip(spacing)
            .map(|(coordinate, scale)| coordinate / scale)
            .collect::<Vec<_>>();
        let voxel_endpoint = voxel_origin
            .iter()
            .zip(palace_direction.iter().zip(spacing))
            .map(|(origin, (direction, scale))| origin + direction / scale)
            .collect::<Vec<_>>();
        let transform = level_transform(multiscale, 0)
            .map_err(|error| SessionError::Annotation(error.to_string()))?;
        let physical_origin = transform
            .apply(&voxel_origin)
            .map_err(|error| SessionError::Annotation(error.to_string()))?;
        let physical_endpoint = transform
            .apply(&voxel_endpoint)
            .map_err(|error| SessionError::Annotation(error.to_string()))?;
        let origin = [physical_origin[x], physical_origin[y], physical_origin[z]];
        let direction = [
            physical_endpoint[x] - physical_origin[x],
            physical_endpoint[y] - physical_origin[y],
            physical_endpoint[z] - physical_origin[z],
        ];
        let physical_distance_per_palace_unit = direction
            .iter()
            .map(|value| value * value)
            .sum::<f64>()
            .sqrt();
        if physical_distance_per_palace_unit <= f64::EPSILON {
            return Err(SessionError::Annotation(
                "Palace camera ray collapses under the NGFF transform".into(),
            ));
        }
        Ok(PalacePhysicalRay {
            ray: PickRay::new(origin, direction)
                .map_err(|error| SessionError::Annotation(error.to_string()))?,
            physical_distance_per_palace_unit,
        })
    }

    /// Converts a normalized direct-portable XYZ voxel ray to physical annotation coordinates.
    /// Unlike Palace's source-local ray, this starts in level-zero voxel units already; its
    /// distance factor therefore converts one unit of the portable ray parameter to NGFF space.
    pub fn portable_voxel_ray_to_physical(
        &self,
        origin_xyz: [f64; 3],
        direction_xyz: [f64; 3],
    ) -> Result<PalacePhysicalRay, SessionError> {
        if origin_xyz
            .iter()
            .chain(direction_xyz.iter())
            .any(|value| !value.is_finite())
        {
            return Err(SessionError::Annotation(
                "portable camera ray contains a non-finite component".into(),
            ));
        }
        let length = direction_xyz
            .iter()
            .map(|value| value * value)
            .sum::<f64>()
            .sqrt();
        if !(0.999..=1.001).contains(&length) {
            return Err(SessionError::Annotation(
                "portable camera ray direction is not normalized".into(),
            ));
        }
        let metadata = self.metadata.as_ref().ok_or_else(|| {
            SessionError::Annotation(
                "open an OME-Zarr dataset before converting a camera ray".into(),
            )
        })?;
        let multiscale = metadata
            .multiscales
            .first()
            .ok_or_else(|| SessionError::Annotation("dataset has no multiscale metadata".into()))?;
        let axis = |name: &str| {
            multiscale
                .axes
                .iter()
                .position(|candidate| candidate.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| SessionError::Annotation(format!("multiscale lacks a {name} axis")))
        };
        let xyz = [axis("x")?, axis("y")?, axis("z")?];
        let mut voxel_origin = vec![0.0; multiscale.axes.len()];
        let mut voxel_endpoint = vec![0.0; multiscale.axes.len()];
        for component in 0..3 {
            voxel_origin[xyz[component]] = origin_xyz[component];
            voxel_endpoint[xyz[component]] = origin_xyz[component] + direction_xyz[component];
        }
        let transform = level_transform(multiscale, 0)
            .map_err(|error| SessionError::Annotation(error.to_string()))?;
        let physical_origin = transform
            .apply(&voxel_origin)
            .map_err(|error| SessionError::Annotation(error.to_string()))?;
        let physical_endpoint = transform
            .apply(&voxel_endpoint)
            .map_err(|error| SessionError::Annotation(error.to_string()))?;
        let origin = std::array::from_fn(|component| physical_origin[xyz[component]]);
        let direction = std::array::from_fn(|component| {
            physical_endpoint[xyz[component]] - physical_origin[xyz[component]]
        });
        let physical_distance_per_palace_unit = direction
            .iter()
            .map(|value| value * value)
            .sum::<f64>()
            .sqrt();
        if physical_distance_per_palace_unit <= f64::EPSILON {
            return Err(SessionError::Annotation(
                "portable camera ray collapses under the NGFF transform".into(),
            ));
        }
        Ok(PalacePhysicalRay {
            ray: PickRay::new(origin, direction)
                .map_err(|error| SessionError::Annotation(error.to_string()))?,
            physical_distance_per_palace_unit,
        })
    }

    /// Converts one physical `[x, y, z]` coordinate into a nearest level-zero voxel for a
    /// display overlay. The physical coordinate remains the persisted authority.
    pub fn physical_point_voxel_xyz(&self, physical: [f64; 3]) -> Result<[u64; 3], SessionError> {
        Ok(self
            .physical_point_voxel_xyz_f64(physical)?
            .map(|value| value.round() as u64))
    }

    /// Converts one physical point into an unrounded level-zero voxel position. This is the
    /// camera-projection form: rounding first would visibly move an annotation before Palace's
    /// perspective transform is applied.
    pub fn physical_point_voxel_xyz_f64(
        &self,
        physical: [f64; 3],
    ) -> Result<[f64; 3], SessionError> {
        let metadata = self.metadata.as_ref().ok_or_else(|| {
            SessionError::Annotation(
                "open an OME-Zarr dataset before projecting annotations".into(),
            )
        })?;
        let multiscale = metadata
            .multiscales
            .first()
            .ok_or_else(|| SessionError::Annotation("dataset has no multiscale metadata".into()))?;
        let axis_index = |name: &str| {
            multiscale
                .axes
                .iter()
                .position(|axis| axis.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| SessionError::Annotation(format!("multiscale lacks a {name} axis")))
        };
        let x = axis_index("x")?;
        let y = axis_index("y")?;
        let z = axis_index("z")?;
        let mut world = vec![0.0; multiscale.axes.len()];
        world[x] = physical[0];
        world[y] = physical[1];
        world[z] = physical[2];
        let voxel = level_transform(multiscale, 0)
            .and_then(|transform| transform.inverse_apply(&world))
            .map_err(|error| SessionError::Annotation(error.to_string()))?;
        let xyz = [voxel[x], voxel[y], voxel[z]];
        if xyz.iter().any(|value| !value.is_finite() || *value < 0.0) {
            return Err(SessionError::Annotation(
                "physical point maps outside voxel space".into(),
            ));
        }
        if let Some(shape) = self.voxel_shape_xyz {
            if xyz
                .iter()
                .zip(shape)
                .any(|(value, extent)| *value >= extent as f64)
            {
                return Err(SessionError::Annotation(
                    "physical point maps outside array bounds".into(),
                ));
            }
        }
        Ok(xyz)
    }

    /// Projects the visible perimeter of a persisted annotation to display-only level-zero voxel
    /// points. Rectangles and ellipses deliberately become their outlines here: physical scene
    /// geometry remains authoritative while the 2D canvas receives enough points to draw the
    /// same shape without inventing a voxel-aligned ROI.
    /// A caller must keep the physical geometry as the source of truth and avoid drawing a
    /// marker if any vertex cannot be inverted safely.
    pub fn annotation_voxel_points(
        &self,
        annotation: &Annotation,
    ) -> Result<Vec<[u64; 3]>, SessionError> {
        let points: Vec<[f64; 3]> = match &annotation.geometry {
            AnnotationGeometry::Point(point) => vec![*point],
            AnnotationGeometry::Polyline(points) | AnnotationGeometry::Polygon(points) => {
                points.clone()
            }
            AnnotationGeometry::Rectangle { center, half_axes } => {
                let [first, second] = *half_axes;
                vec![
                    std::array::from_fn(|axis| center[axis] - first[axis] - second[axis]),
                    std::array::from_fn(|axis| center[axis] + first[axis] - second[axis]),
                    std::array::from_fn(|axis| center[axis] + first[axis] + second[axis]),
                    std::array::from_fn(|axis| center[axis] - first[axis] + second[axis]),
                ]
            }
            AnnotationGeometry::Ellipse { center, radii } => (0..32)
                .map(|index| {
                    let angle = std::f64::consts::TAU * index as f64 / 32.0;
                    std::array::from_fn(|axis| {
                        center[axis] + radii[0][axis] * angle.cos() + radii[1][axis] * angle.sin()
                    })
                })
                .collect(),
        };
        points
            .iter()
            .copied()
            .map(|physical| self.physical_point_voxel_xyz(physical))
            .collect()
    }

    /// Removes an annotation by its stable session-local identifier. Coordinates remain owned by
    /// the scene until this explicit operation; reopening a dataset is the other deliberate
    /// reset boundary.
    pub fn remove_annotation(&mut self, id: AnnotationId) -> Result<Annotation, SessionError> {
        self.scene
            .remove_annotation(id)
            .ok_or_else(|| SessionError::Annotation(format!("annotation {} does not exist", id.0)))
    }

    /// Export only stable physical annotation state. This is an explicit desktop action; the
    /// app never writes annotations merely by opening a dataset.
    pub fn export_annotations(&self, path: impl AsRef<Path>) -> Result<(), SessionError> {
        let dataset_root = self.dataset_root.as_ref().ok_or_else(|| {
            SessionError::AnnotationDocument(
                "open an OME-Zarr dataset before exporting annotations".into(),
            )
        })?;
        let document = AnnotationDocument {
            format_version: ANNOTATION_DOCUMENT_VERSION,
            dataset_root: dataset_root.display().to_string(),
            annotations: self.annotations().to_vec(),
        };
        let encoded = serde_json::to_vec_pretty(&document)
            .map_err(|error| SessionError::AnnotationDocument(error.to_string()))?;
        fs::write(path, encoded)
            .map_err(|error| SessionError::AnnotationDocument(error.to_string()))
    }

    /// Validate the entire document before changing session state. Importing annotations for a
    /// different canonical root is rejected rather than relying on coincidental voxel geometry.
    pub fn import_annotations(&mut self, path: impl AsRef<Path>) -> Result<(), SessionError> {
        let bytes =
            fs::read(path).map_err(|error| SessionError::AnnotationDocument(error.to_string()))?;
        let document: AnnotationDocument = serde_json::from_slice(&bytes)
            .map_err(|error| SessionError::AnnotationDocument(error.to_string()))?;
        if document.format_version != ANNOTATION_DOCUMENT_VERSION {
            return Err(SessionError::AnnotationDocument(format!(
                "unsupported annotation document version {}",
                document.format_version
            )));
        }
        let dataset_root = self.dataset_root.as_ref().ok_or_else(|| {
            SessionError::AnnotationDocument(
                "open an OME-Zarr dataset before importing annotations".into(),
            )
        })?;
        if document.dataset_root != dataset_root.display().to_string() {
            return Err(SessionError::AnnotationDocument(
                "annotation document belongs to a different dataset root".into(),
            ));
        }
        let mut scene = Scene::default();
        let mut next_annotation_id = 0_u64;
        for annotation in document.annotations {
            annotation
                .geometry
                .validate()
                .map_err(|error| SessionError::AnnotationDocument(error.to_string()))?;
            next_annotation_id =
                next_annotation_id.max(annotation.id.0.checked_add(1).ok_or_else(|| {
                    SessionError::AnnotationDocument(
                        "annotation identifier space is exhausted".into(),
                    )
                })?);
            scene
                .insert_annotation(annotation)
                .map_err(|error| SessionError::AnnotationDocument(error.to_string()))?;
        }
        self.scene = scene;
        self.next_annotation_id = next_annotation_id;
        Ok(())
    }

    /// Add a point from the linked slice crosshair. `voxel_xyz` is converted through the first
    /// displayed NGFF level before being retained, so annotations never encode an accidental
    /// cubic-voxel assumption.
    pub fn add_point_annotation(
        &mut self,
        label: impl Into<String>,
        voxel_xyz: [u64; 3],
    ) -> Result<Annotation, SessionError> {
        self.validate_voxel_point(voxel_xyz)?;
        let physical = self.physical_point(voxel_xyz)?;
        self.insert_annotation(label, AnnotationGeometry::Point(physical))
    }

    /// Add a polygon from level-zero voxel vertices. The persisted geometry is converted through
    /// the declared NGFF transform point-by-point, so a planar 2D ROI remains physically correct
    /// on anisotropic data rather than storing display-pixel or cubic-voxel coordinates.
    pub fn add_polygon_annotation(
        &mut self,
        label: impl Into<String>,
        voxel_points: impl IntoIterator<Item = [u64; 3]>,
    ) -> Result<Annotation, SessionError> {
        // Stop consuming as soon as the bounded document format is exceeded.  This method is
        // also used below the Tauri command boundary, so callers passing a streaming iterator
        // cannot force an unbounded intermediate allocation before validation.
        let voxel_points = voxel_points
            .into_iter()
            .take(MAX_ANNOTATION_VERTICES + 1)
            .collect::<Vec<_>>();
        if voxel_points.len() > MAX_ANNOTATION_VERTICES {
            return Err(SessionError::Annotation(format!(
                "annotation has more than {MAX_ANNOTATION_VERTICES} vertices"
            )));
        }
        let physical = voxel_points
            .into_iter()
            .map(|point| {
                self.validate_voxel_point(point)?;
                self.physical_point(point)
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.insert_annotation(label, AnnotationGeometry::Polygon(physical))
    }

    /// Add an axis-aligned-on-the-selected-slice rectangle from opposite level-zero voxel
    /// corners. The resulting centre and half axes remain fully physical, so anisotropic and
    /// affine NGFF datasets do not acquire display-pixel geometry.
    pub fn add_rectangle_annotation(
        &mut self,
        label: impl Into<String>,
        first: [u64; 3],
        opposite: [u64; 3],
    ) -> Result<Annotation, SessionError> {
        let (center, half_axes) = self.planar_roi_basis(first, opposite)?;
        self.insert_annotation(label, AnnotationGeometry::Rectangle { center, half_axes })
    }

    /// Add an ellipse inscribed in the rectangle defined by two opposite voxel corners.
    pub fn add_ellipse_annotation(
        &mut self,
        label: impl Into<String>,
        first: [u64; 3],
        opposite: [u64; 3],
    ) -> Result<Annotation, SessionError> {
        let (center, radii) = self.planar_roi_basis(first, opposite)?;
        self.insert_annotation(label, AnnotationGeometry::Ellipse { center, radii })
    }

    fn planar_roi_basis(
        &self,
        first: [u64; 3],
        opposite: [u64; 3],
    ) -> Result<([f64; 3], [[f64; 3]; 2]), SessionError> {
        self.validate_voxel_point(first)?;
        self.validate_voxel_point(opposite)?;
        let fixed_axes = (0..3).filter(|&axis| first[axis] == opposite[axis]).count();
        if fixed_axes != 1 {
            return Err(SessionError::Annotation(
                "rectangle and ellipse corners must share exactly one slice axis".into(),
            ));
        }
        let varying = (0..3)
            .filter(|&axis| first[axis] != opposite[axis])
            .collect::<Vec<_>>();
        let mut first_axis = first;
        first_axis[varying[0]] = opposite[varying[0]];
        let mut second_axis = first;
        second_axis[varying[1]] = opposite[varying[1]];
        let first_physical = self.physical_point(first)?;
        let opposite_physical = self.physical_point(opposite)?;
        let first_axis_physical = self.physical_point(first_axis)?;
        let second_axis_physical = self.physical_point(second_axis)?;
        Ok((
            std::array::from_fn(|axis| (first_physical[axis] + opposite_physical[axis]) * 0.5),
            [
                std::array::from_fn(|axis| {
                    (first_axis_physical[axis] - first_physical[axis]) * 0.5
                }),
                std::array::from_fn(|axis| {
                    (second_axis_physical[axis] - first_physical[axis]) * 0.5
                }),
            ],
        ))
    }

    fn insert_annotation(
        &mut self,
        label: impl Into<String>,
        geometry: AnnotationGeometry,
    ) -> Result<Annotation, SessionError> {
        let id = AnnotationId(self.next_annotation_id);
        let next_annotation_id = self.next_annotation_id.checked_add(1).ok_or_else(|| {
            SessionError::Annotation("annotation identifier space is exhausted".into())
        })?;
        let annotation = Annotation::new(id, label, geometry, [255, 216, 72])
            .map_err(|error| SessionError::Annotation(error.to_string()))?;
        self.scene
            .insert_annotation(annotation.clone())
            .map_err(|error| SessionError::Annotation(error.to_string()))?;
        self.next_annotation_id = next_annotation_id;
        Ok(annotation)
    }

    fn validate_voxel_point(&self, voxel_xyz: [u64; 3]) -> Result<(), SessionError> {
        if let Some(shape) = self.voxel_shape_xyz {
            if voxel_xyz
                .iter()
                .zip(shape)
                .any(|(value, extent)| *value >= extent)
            {
                return Err(SessionError::Annotation(
                    "annotation voxel point lies outside array bounds".into(),
                ));
            }
        }
        Ok(())
    }

    fn physical_point(&self, voxel_xyz: [u64; 3]) -> Result<[f64; 3], SessionError> {
        let metadata = self
            .metadata
            .as_ref()
            .ok_or_else(|| SessionError::Annotation("open an OME-Zarr dataset first".into()))?;
        let multiscale = metadata
            .multiscales
            .first()
            .ok_or_else(|| SessionError::Annotation("dataset has no multiscale metadata".into()))?;
        if multiscale.datasets.is_empty() {
            return Err(SessionError::Annotation(
                "dataset has no level-zero array metadata".into(),
            ));
        }
        let axis_index = |name: &str| {
            multiscale
                .axes
                .iter()
                .position(|axis| axis.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| SessionError::Annotation(format!("multiscale lacks a {name} axis")))
        };
        let x = axis_index("x")?;
        let y = axis_index("y")?;
        let z = axis_index("z")?;
        let mut point = vec![0.0; multiscale.axes.len()];
        point[x] = voxel_xyz[0] as f64;
        point[y] = voxel_xyz[1] as f64;
        point[z] = voxel_xyz[2] as f64;
        let transformed = level_transform(multiscale, 0)
            .and_then(|transform| transform.apply(&point))
            .map_err(|error| SessionError::Annotation(error.to_string()))?;
        Ok([transformed[x], transformed[y], transformed[z]])
    }
}

impl LocalOmeZarrSource {
    fn level_zero(root: &Path, metadata: &DatasetMetadata) -> Result<Self, SessionError> {
        let multiscale = metadata.multiscales.first().ok_or_else(|| {
            SessionError::LayerSource("dataset has no multiscale metadata".into())
        })?;
        let dataset = multiscale.datasets.first().ok_or_else(|| {
            SessionError::LayerSource("dataset has no level-zero array metadata".into())
        })?;
        let array = read_array_info(root, &dataset.path)?;
        let array_directory = root.join(&dataset.path);
        let chunk_key_encoding = if array_directory.join("zarr.json").is_file() {
            LocalChunkKeyEncoding::V3Slash
        } else if array_directory.join(".zarray").is_file() {
            LocalChunkKeyEncoding::V2Dot
        } else {
            return Err(SessionError::LayerSource(format!(
                "array {} has neither zarr.json nor .zarray metadata",
                dataset.path
            )));
        };
        if array.shape.len() != multiscale.axes.len() {
            return Err(SessionError::LayerSource(format!(
                "array {} has {} dimensions but NGFF declares {} axes",
                dataset.path,
                array.shape.len(),
                multiscale.axes.len()
            )));
        }
        let axis_index = |name: &str| -> Result<u32, SessionError> {
            let matches = multiscale
                .axes
                .iter()
                .enumerate()
                .filter(|(_, axis)| axis.name.eq_ignore_ascii_case(name))
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [index] => u32::try_from(*index).map_err(|_| {
                    SessionError::LayerSource(format!(
                        "{name} axis index cannot fit the wire contract"
                    ))
                }),
                [] => Err(SessionError::LayerSource(format!(
                    "multiscale lacks a {name} axis"
                ))),
                _ => Err(SessionError::LayerSource(format!(
                    "multiscale declares {name} more than once"
                ))),
            }
        };
        let optional_axis = |name: &str| -> Result<Option<u32>, SessionError> {
            let matches = multiscale
                .axes
                .iter()
                .enumerate()
                .filter(|(_, axis)| axis.name.eq_ignore_ascii_case(name))
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [] => Ok(None),
                [index] => u32::try_from(*index).map(Some).map_err(|_| {
                    SessionError::LayerSource(format!(
                        "{name} axis index cannot fit the wire contract"
                    ))
                }),
                _ => Err(SessionError::LayerSource(format!(
                    "multiscale declares {name} more than once"
                ))),
            }
        };
        if multiscale.axes.iter().any(|axis| {
            !["x", "y", "z", "c", "t"]
                .iter()
                .any(|supported| axis.name.eq_ignore_ascii_case(supported))
        }) {
            return Err(SessionError::LayerSource(
                "local chunk planner supports only X/Y/Z/C/T NGFF axes".into(),
            ));
        }
        if array.chunks.len() != array.shape.len() || array.chunks.contains(&0) {
            return Err(SessionError::LayerSource(
                "array chunk shape must match array dimensions and be non-zero".into(),
            ));
        }
        Ok(Self {
            root: root.display().to_string(),
            multiscale_index: 0,
            level: 0,
            array_path: dataset.path.clone(),
            axes: multiscale
                .axes
                .iter()
                .map(|axis| axis.name.clone())
                .collect(),
            shape: array.shape,
            chunk_shape: array.chunks,
            dtype: array.dtype,
            chunk_key_encoding,
            spatial_axes_xyz: [axis_index("x")?, axis_index("y")?, axis_index("z")?],
            channel_axis: optional_axis("c")?,
            time_axis: optional_axis("t")?,
            timepoint: 0,
        })
    }
}

fn portable_axis_aligned_transform(
    multiscale: &Multiscale,
) -> Result<LayerTransform, SessionError> {
    let transform = level_transform(multiscale, 0)
        .map_err(|error| SessionError::LayerSource(error.to_string()))?;
    let axis = |name: &str| {
        multiscale
            .axes
            .iter()
            .position(|candidate| candidate.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| SessionError::LayerSource(format!("multiscale lacks a {name} axis")))
    };
    let xyz = [axis("x")?, axis("y")?, axis("z")?];
    let matrix = transform.matrix();
    let mut scale = [0.0; 3];
    let mut translation = [0.0; 3];
    for (target, &row) in xyz.iter().enumerate() {
        for (column, value) in matrix[row].iter().take(multiscale.axes.len()).enumerate() {
            if column != xyz[target] && value.abs() > 1e-10 {
                return Err(SessionError::LayerSource(
                    "native portable default layer requires an axis-aligned NGFF transform".into(),
                ));
            }
        }
        scale[target] = matrix[row][xyz[target]];
        translation[target] = matrix[row][multiscale.axes.len()];
    }
    LayerTransform::new(scale, translation)
        .map_err(|error| SessionError::LayerSource(error.to_string()))
}

fn parse_srgb_hex(value: &str) -> Option<[u8; 3]> {
    let value = value.strip_prefix('#').unwrap_or(value);
    if value.len() != 6 || !value.as_bytes().iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    Some([
        u8::from_str_radix(&value[0..2], 16).ok()?,
        u8::from_str_radix(&value[2..4], 16).ok()?,
        u8::from_str_radix(&value[4..6], 16).ok()?,
    ])
}

fn portable_words(bytes: &[u8], dtype: &str) -> Result<Vec<u32>, SessionError> {
    let width = match dtype {
        "uint8" | "|u1" => 1,
        "uint16" | "<u2" => 2,
        "uint32" | "<u4" => 4,
        dtype => {
            return Err(SessionError::LayerSource(format!(
                "portable page admission supports uint8/uint16/uint32, not {dtype}"
            )))
        }
    };
    if !bytes.len().is_multiple_of(width) {
        return Err(SessionError::LayerSource(format!(
            "{dtype} chunk has {} bytes, not a multiple of {width}",
            bytes.len()
        )));
    }
    Ok(match width {
        1 => bytes.iter().map(|&value| u32::from(value)).collect(),
        2 => bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|value| u32::from(u16::from_le_bytes([value[0], value[1]])))
            .collect(),
        4 => bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|value| u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
            .collect(),
        _ => unreachable!(),
    })
}

fn portable_scalar_type(dtype: &str) -> Result<PortableScalarType, SessionError> {
    match dtype {
        "uint8" | "|u1" => Ok(PortableScalarType::Uint8),
        "uint16" | "<u2" => Ok(PortableScalarType::Uint16),
        "uint32" | "<u4" => Ok(PortableScalarType::Uint32),
        dtype => Err(SessionError::LayerSource(format!(
            "portable volume admission supports uint8/uint16/uint32, not {dtype}"
        ))),
    }
}

fn portable_dimensions_xyz(
    address: &LocalChunkAddress,
    source: &LocalOmeZarrSource,
    layer_id: LayerId,
) -> Result<[u32; 3], SessionError> {
    let dimension = |xyz: usize| {
        u32::try_from(address.logical_extent[source.spatial_axes_xyz[xyz] as usize]).map_err(|_| {
            SessionError::LayerSource(format!(
                "layer {} chunk extent does not fit portable u32 dimensions",
                layer_id.0
            ))
        })
    };
    Ok([dimension(0)?, dimension(1)?, dimension(2)?])
}

/// Decode one array-order Zarr chunk into the portable recorder's X-fastest XYZ word order.
/// C/T axes have logical extent one for every admitted chunk, but remain in the stride
/// calculation so arbitrary declared X/Y/Z ordering cannot silently transpose a volume.
fn portable_words_xyz(
    bytes: &[u8],
    address: &LocalChunkAddress,
    source: &LocalOmeZarrSource,
) -> Result<Vec<u32>, SessionError> {
    let source_words = portable_words(bytes, &source.dtype)?;
    if address.logical_extent.len() != source.axes.len() {
        return Err(SessionError::LayerSource(
            "chunk logical extent does not match its declared source axes".into(),
        ));
    }
    let expected = address
        .logical_extent
        .iter()
        .try_fold(1_usize, |count, extent| count.checked_mul(*extent as usize))
        .ok_or_else(|| SessionError::LayerSource("chunk word count overflows usize".into()))?;
    if source_words.len() != expected {
        return Err(SessionError::LayerSource(format!(
            "chunk contains {} scalar words but declared logical extent requires {expected}",
            source_words.len()
        )));
    }
    let dimensions = portable_dimensions_xyz(address, source, LayerId(0))?;
    let mut strides = vec![1_usize; source.axes.len()];
    for axis in (0..source.axes.len()).rev().skip(1) {
        strides[axis] = strides[axis + 1]
            .checked_mul(address.logical_extent[axis + 1] as usize)
            .ok_or_else(|| SessionError::LayerSource("chunk stride overflows usize".into()))?;
    }
    let [x_axis, y_axis, z_axis] = source.spatial_axes_xyz.map(|axis| axis as usize);
    let mut result = Vec::with_capacity(expected);
    for z in 0..dimensions[2] as usize {
        for y in 0..dimensions[1] as usize {
            for x in 0..dimensions[0] as usize {
                let index = x * strides[x_axis] + y * strides[y_axis] + z * strides[z_axis];
                result.push(source_words[index]);
            }
        }
    }
    Ok(result)
}

/// Assemble canonical XYZ chunk words into one bounded X-fastest volume. Callers retain the
/// chunk plan as authority; this helper rejects a hole or overlap rather than guessing a fill.
type PortableTileAssembly = (Vec<u32>, [u32; 3], [u64; 3]);

fn assemble_portable_xyz_tiles(
    tiles: &[(LocalChunkAddress, Vec<u32>)],
    source: &LocalOmeZarrSource,
) -> Result<PortableTileAssembly, SessionError> {
    let first = tiles
        .first()
        .ok_or_else(|| SessionError::LayerSource("portable tile set is empty".into()))?;
    let chunk_shape: [u64; 3] =
        std::array::from_fn(|axis| source.chunk_shape[source.spatial_axes_xyz[axis] as usize]);
    let origin_chunk: [u64; 3] = std::array::from_fn(|axis| {
        tiles
            .iter()
            .map(|(address, _)| address.spatial_chunk_xyz[axis])
            .min()
            .unwrap()
    });
    let origin = std::array::from_fn(|axis| origin_chunk[axis] * chunk_shape[axis]);
    let end: [u64; 3] = std::array::from_fn(|axis| {
        tiles
            .iter()
            .map(|(address, _)| {
                address.spatial_chunk_xyz[axis] * chunk_shape[axis]
                    + address.logical_extent[source.spatial_axes_xyz[axis] as usize]
            })
            .max()
            .unwrap()
    });
    let dimensions: [u32; 3] =
        std::array::from_fn(|axis| u32::try_from(end[axis] - origin[axis]).unwrap_or(0));
    if dimensions.contains(&0) {
        return Err(SessionError::LayerSource(
            "portable tile extent is invalid".into(),
        ));
    }
    let count = dimensions
        .iter()
        .try_fold(1_usize, |n, &d| n.checked_mul(d as usize))
        .ok_or_else(|| SessionError::LayerSource("portable tile volume overflows usize".into()))?;
    if count > 4 * 1024 * 1024 {
        return Err(SessionError::LayerSource(
            "portable tile volume exceeds four-page word capacity".into(),
        ));
    }
    let mut words = vec![0; count];
    let mut covered = vec![false; count];
    for (address, tile) in tiles {
        let dims = portable_dimensions_xyz(address, source, LayerId(0))?;
        if tile.len()
            != dims
                .iter()
                .try_fold(1_usize, |n, &d| n.checked_mul(d as usize))
                .unwrap_or(usize::MAX)
        {
            return Err(SessionError::LayerSource(
                "portable tile word count disagrees with its dimensions".into(),
            ));
        }
        let offset: [usize; 3] = std::array::from_fn(|axis| {
            (address.spatial_chunk_xyz[axis] * chunk_shape[axis] - origin[axis]) as usize
        });
        for z in 0..dims[2] as usize {
            for y in 0..dims[1] as usize {
                for x in 0..dims[0] as usize {
                    let dst = (offset[2] + z) * dimensions[1] as usize * dimensions[0] as usize
                        + (offset[1] + y) * dimensions[0] as usize
                        + offset[0]
                        + x;
                    let src = z * dims[1] as usize * dims[0] as usize + y * dims[0] as usize + x;
                    if covered[dst] {
                        return Err(SessionError::LayerSource("portable tiles overlap".into()));
                    }
                    words[dst] = tile[src];
                    covered[dst] = true;
                }
            }
        }
    }
    if covered.iter().any(|covered| !covered) {
        return Err(SessionError::LayerSource(
            "portable tiles leave a spatial gap".into(),
        ));
    }
    let _ = first;
    Ok((words, dimensions, origin))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn local_open_reports_metadata_without_claiming_a_renderer() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join(".zattrs"),
            r#"{"multiscales":[{"axes":[],"datasets":[]}],"omero":{"channels":[{},{}]}}"#,
        )
        .unwrap();

        let mut session = LocalSession::default();
        let summary = session.open_local_omezarr(root.path()).unwrap();

        assert_eq!(summary.multiscale_count, 1);
        assert_eq!(summary.channel_count, 2);
        assert!(!summary.renderer_connected);
        assert!(summary.dataset_root.is_some());
    }

    #[test]
    fn local_open_maps_ngff_axis_order_to_xyz_shape() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join(".zattrs"),
            r#"{"multiscales":[{"axes":[{"name":"z"},{"name":"y"},{"name":"x"}],"datasets":[{"path":"0"}]}]}"#,
        )
        .unwrap();
        fs::create_dir(root.path().join("0")).unwrap();
        fs::write(
            root.path().join("0/zarr.json"),
            r#"{"shape":[7,11,13],"data_type":"uint8","chunk_grid":{"configuration":{"chunk_shape":[1,2,3]}}}"#,
        )
        .unwrap();

        let mut session = LocalSession::default();
        let summary = session.open_local_omezarr(root.path()).unwrap();
        assert_eq!(summary.voxel_shape_xyz, Some([13, 11, 7]));
        assert_eq!(session.summary().voxel_shape_xyz, Some([13, 11, 7]));
    }

    #[test]
    fn portable_words_reorders_declared_non_xyz_storage_axes() {
        let source = LocalOmeZarrSource {
            root: "fixture".into(),
            multiscale_index: 0,
            level: 0,
            array_path: "0".into(),
            axes: vec!["x".into(), "z".into(), "y".into()],
            shape: vec![2, 2, 2],
            chunk_shape: vec![2, 2, 2],
            dtype: "uint16".into(),
            chunk_key_encoding: LocalChunkKeyEncoding::V3Slash,
            spatial_axes_xyz: [0, 2, 1],
            channel_axis: None,
            time_axis: None,
            timepoint: 0,
        };
        let address = LocalChunkAddress {
            asset_path: "0/c/0/0/0".into(),
            coordinates: vec![0, 0, 0],
            channel: 0,
            timepoint: 0,
            spatial_chunk_xyz: [0, 0, 0],
            logical_extent: vec![2, 2, 2],
        };
        // Storage order is X,Z,Y, so Y is contiguous. Portable order must be X,Y,Z.
        let bytes = [0_u16, 1, 2, 3, 4, 5, 6, 7]
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        let words = portable_words_xyz(&bytes, &address, &source).unwrap();
        assert_eq!(words, vec![0, 4, 1, 5, 2, 6, 3, 7]);
        let (assembled, dimensions, origin) =
            assemble_portable_xyz_tiles(&[(address, words)], &source).unwrap();
        assert_eq!(assembled, vec![0, 4, 1, 5, 2, 6, 3, 7]);
        assert_eq!(dimensions, [2, 2, 2]);
        assert_eq!(origin, [0, 0, 0]);
    }

    #[test]
    fn annotation_crosshair_is_stored_in_physical_ngff_coordinates() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join(".zattrs"),
            r#"{"multiscales":[{"axes":[{"name":"z"},{"name":"y"},{"name":"x"}],"datasets":[{"path":"0","coordinateTransformations":[{"type":"scale","scale":[5.0,0.5,0.25]},{"type":"translation","translation":[10.0,20.0,30.0]}]}]}]}"#,
        )
        .unwrap();

        let mut session = LocalSession::default();
        session.open_local_omezarr(root.path()).unwrap();
        let annotation = session.add_point_annotation("landmark", [4, 6, 2]).unwrap();

        assert_eq!(annotation.id, AnnotationId(0));
        assert_eq!(
            annotation.geometry,
            AnnotationGeometry::Point([31.0, 23.0, 20.0])
        );
        assert_eq!(
            session.annotation_voxel_points(&annotation).unwrap(),
            [[4, 6, 2]]
        );
        let polygon = session
            .add_polygon_annotation("roi", [[4, 6, 2], [5, 6, 2], [5, 7, 2]])
            .unwrap();
        assert_eq!(polygon.id, AnnotationId(1));
        assert_eq!(
            polygon.geometry,
            AnnotationGeometry::Polygon(vec![
                [31.0, 23.0, 20.0],
                [31.25, 23.0, 20.0],
                [31.25, 23.5, 20.0],
            ])
        );
        assert_eq!(
            session.annotation_voxel_points(&polygon).unwrap(),
            vec![[4, 6, 2], [5, 6, 2], [5, 7, 2]]
        );
        assert!(session
            .add_polygon_annotation("too few", [[4, 6, 2], [5, 6, 2]])
            .is_err());
        let too_many = session
            .add_polygon_annotation("too many", std::iter::repeat([4, 6, 2]))
            .unwrap_err();
        assert!(too_many.to_string().contains(&format!(
            "annotation has more than {MAX_ANNOTATION_VERTICES} vertices"
        )));
        assert_eq!(
            session.annotations(),
            &[annotation.clone(), polygon.clone()]
        );
        let line = Annotation::new(
            AnnotationId(9),
            "segment",
            AnnotationGeometry::Polyline(vec![[31.0, 23.0, 20.0], [31.25, 23.5, 25.0]]),
            [255, 0, 0],
        )
        .unwrap();
        assert_eq!(
            session.annotation_voxel_points(&line).unwrap(),
            vec![[4, 6, 2], [5, 7, 3]]
        );
        let rectangle = Annotation::new(
            AnnotationId(10),
            "rectangle",
            AnnotationGeometry::Rectangle {
                center: [31.0, 23.0, 20.0],
                half_axes: [[0.25, 0.0, 0.0], [0.0, 0.5, 0.0]],
            },
            [255, 0, 0],
        )
        .unwrap();
        assert_eq!(
            session.annotation_voxel_points(&rectangle).unwrap(),
            vec![[3, 5, 2], [5, 5, 2], [5, 7, 2], [3, 7, 2]]
        );
        let ellipse = Annotation::new(
            AnnotationId(11),
            "ellipse",
            AnnotationGeometry::Ellipse {
                center: [31.0, 23.0, 20.0],
                radii: [[0.25, 0.0, 0.0], [0.0, 0.5, 0.0]],
            },
            [255, 0, 0],
        )
        .unwrap();
        let ellipse_points = session.annotation_voxel_points(&ellipse).unwrap();
        assert_eq!(ellipse_points.len(), 32);
        assert_eq!(ellipse_points[0], [5, 6, 2]);
        assert_eq!(session.annotations(), &[annotation, polygon]);
    }

    #[test]
    fn slice_rois_keep_anisotropic_physical_half_axes() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join(".zattrs"),
            r#"{"multiscales":[{"axes":[{"name":"z"},{"name":"y"},{"name":"x"}],"datasets":[{"path":"0","coordinateTransformations":[{"type":"scale","scale":[5.0,0.5,0.25]},{"type":"translation","translation":[10.0,20.0,30.0]}]}]}]}"#,
        )
        .unwrap();
        let mut session = LocalSession::default();
        session.open_local_omezarr(root.path()).unwrap();

        let rectangle = session
            .add_rectangle_annotation("roi", [4, 6, 2], [8, 10, 2])
            .unwrap();
        assert_eq!(
            rectangle.geometry,
            AnnotationGeometry::Rectangle {
                center: [31.5, 24.0, 20.0],
                half_axes: [[0.5, 0.0, 0.0], [0.0, 1.0, 0.0]],
            }
        );
        assert_eq!(
            session.annotation_voxel_points(&rectangle).unwrap().len(),
            4
        );
        let ellipse = session
            .add_ellipse_annotation("ellipse", [4, 6, 2], [8, 10, 2])
            .unwrap();
        assert!(matches!(
            ellipse.geometry,
            AnnotationGeometry::Ellipse { .. }
        ));
        assert_eq!(session.annotation_voxel_points(&ellipse).unwrap().len(), 32);
        let xz_ellipse = session
            .add_ellipse_annotation("xz ellipse", [4, 6, 2], [8, 6, 5])
            .unwrap();
        assert_eq!(
            xz_ellipse.geometry,
            AnnotationGeometry::Ellipse {
                center: [31.5, 23.0, 27.5],
                radii: [[0.5, 0.0, 0.0], [0.0, 0.0, 7.5]],
            }
        );
        assert!(session
            .add_rectangle_annotation("invalid", [4, 6, 2], [4, 10, 2])
            .is_err());
    }

    #[test]
    fn palace_ray_bridge_reorders_axes_and_converts_depth_distance() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join(".zattrs"),
            r#"{"multiscales":[{"axes":[{"name":"z"},{"name":"y"},{"name":"x"}],"coordinateTransformations":[{"type":"scale","scale":[2.0,2.0,2.0]}],"datasets":[{"path":"0","coordinateTransformations":[{"type":"scale","scale":[5.0,0.5,0.25]},{"type":"translation","translation":[10.0,20.0,30.0]}]}]}]}"#,
        )
        .unwrap();
        let mut session = LocalSession::default();
        session.open_local_omezarr(root.path()).unwrap();

        // Palace coordinates follow the Z/Y/X source axis order and use only its local scale.
        // The session restores both the shared scale and translation before yielding X/Y/Z.
        let bridge = session
            .palace_ray_to_physical([10.0, 3.0, 1.0], [0.0, 0.0, 1.0])
            .unwrap();
        assert_eq!(bridge.ray.origin, [62.0, 46.0, 40.0]);
        assert_eq!(bridge.ray.direction, [1.0, 0.0, 0.0]);
        assert_eq!(bridge.physical_distance_per_palace_unit, 2.0);
    }

    #[test]
    fn portable_voxel_ray_bridge_preserves_xyz_origin_and_anisotropic_distance() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join(".zattrs"),
            r#"{"multiscales":[{"axes":[{"name":"z"},{"name":"y"},{"name":"x"}],"datasets":[{"path":"0","coordinateTransformations":[{"type":"scale","scale":[5.0,0.5,0.25]},{"type":"translation","translation":[10.0,20.0,30.0]}]}]}]}"#,
        )
        .unwrap();
        let mut session = LocalSession::default();
        session.open_local_omezarr(root.path()).unwrap();

        let physical = session
            .portable_voxel_ray_to_physical([2.0, 6.0, 4.0], [1.0, 0.0, 0.0])
            .unwrap();
        assert_eq!(physical.ray.origin, [30.5, 23.0, 30.0]);
        assert_eq!(physical.ray.direction, [1.0, 0.0, 0.0]);
        assert_eq!(physical.physical_distance_per_palace_unit, 0.25);
    }

    #[test]
    fn annotation_deletion_uses_the_stable_scene_identifier() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join(".zattrs"),
            r#"{"multiscales":[{"axes":[{"name":"z"},{"name":"y"},{"name":"x"}],"datasets":[{"path":"0","coordinateTransformations":[{"type":"scale","scale":[1,1,1]}]}]}]}"#,
        )
        .unwrap();
        let mut session = LocalSession::default();
        session.open_local_omezarr(root.path()).unwrap();
        session.add_point_annotation("first", [1, 2, 3]).unwrap();
        let second = session.add_point_annotation("second", [4, 5, 6]).unwrap();

        let removed = session.remove_annotation(AnnotationId(0)).unwrap();
        assert_eq!(removed.label, "first");
        assert_eq!(session.annotations(), &[second]);
        assert!(session.remove_annotation(AnnotationId(0)).is_err());
    }

    #[test]
    fn session_exposes_the_shared_bounded_layer_render_plan() {
        use newvolim_scene::{ChannelState, ChannelWindow, Layer, LayerId, LayerTransform};

        let mut session = LocalSession::default();
        session
            .scene
            .insert_layer(Layer::image(
                LayerId(42),
                "raw",
                LayerTransform::new([0.5, 0.5, 2.0], [10.0, 20.0, 30.0]).unwrap(),
                vec![
                    ChannelState::new(false, [0, 0, 0], ChannelWindow::new(0.0, 1.0).unwrap(), 1.0)
                        .unwrap(),
                    ChannelState::new(true, [1, 2, 3], ChannelWindow::new(4.0, 5.0).unwrap(), 0.5)
                        .unwrap(),
                ],
            ))
            .unwrap();

        let plan = session
            .layer_render_plan(LayerRenderLimits::new(4, 4))
            .unwrap();
        assert_eq!(plan.image_layers.len(), 1);
        assert_eq!(plan.image_layers[0].layer_id, LayerId(42));
        assert_eq!(plan.image_layers[0].channels[0].source_index, 1);
        assert_eq!(
            plan.image_layers[0].transform.translation,
            [10.0, 20.0, 30.0]
        );
    }

    #[test]
    fn local_layer_request_binds_c_axis_and_rejects_an_unbound_layer() {
        use newvolim_scene::{ChannelState, ChannelWindow, Layer, LayerId, LayerTransform};

        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join(".zattrs"),
            r#"{"multiscales":[{"axes":[{"name":"c"},{"name":"z"},{"name":"y"},{"name":"x"}],"datasets":[{"path":"0"}]}]}"#,
        )
        .unwrap();
        fs::create_dir(root.path().join("0")).unwrap();
        fs::write(
            root.path().join("0/zarr.json"),
            r#"{"shape":[2,5,7,11],"data_type":"uint16","chunk_grid":{"configuration":{"chunk_shape":[1,1,7,11]}}}"#,
        )
        .unwrap();
        let mut session = LocalSession::default();
        session.open_local_omezarr(root.path()).unwrap();
        session
            .scene
            .insert_layer(Layer::image(
                LayerId(17),
                "bound raw",
                LayerTransform::IDENTITY,
                vec![
                    ChannelState::new(false, [1, 2, 3], ChannelWindow::new(0.0, 1.0).unwrap(), 1.0)
                        .unwrap(),
                    ChannelState::new(true, [4, 5, 6], ChannelWindow::new(2.0, 9.0).unwrap(), 0.5)
                        .unwrap(),
                ],
            ))
            .unwrap();
        assert!(matches!(
            session.local_layer_render_requests(LayerRenderLimits::new(4, 4)),
            Err(SessionError::LayerSource(message)) if message.contains("has no bound")
        ));

        session.bind_layer_to_open_dataset(LayerId(17)).unwrap();
        let requests = session
            .local_layer_render_requests(LayerRenderLimits::new(4, 4))
            .unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].layer.layer_id, LayerId(17));
        assert_eq!(requests[0].layer.channels[0].source_index, 1);
        assert_eq!(requests[0].source.array_path, "0");
        assert_eq!(requests[0].source.spatial_axes_xyz, [3, 2, 1]);
        assert_eq!(requests[0].source.channel_axis, Some(0));
        assert_eq!(requests[0].source.shape, vec![2, 5, 7, 11]);
        assert_eq!(requests[0].source.chunk_shape, vec![1, 1, 7, 11]);
        assert_eq!(
            requests[0].source.chunk_key_encoding,
            LocalChunkKeyEncoding::V3Slash
        );

        let plans = session
            .local_layer_chunk_plan(
                LayerRenderLimits::new(4, 4),
                SpatialChunkRegion::new([0, 0, 0], [1, 1, 1]),
                1,
            )
            .unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].chunks.len(), 1);
        assert_eq!(plans[0].chunks[0].coordinates, vec![1, 0, 0, 0]);
        assert_eq!(plans[0].chunks[0].asset_path, "0/c/1/0/0/0");
        assert_eq!(plans[0].chunks[0].spatial_chunk_xyz, [0, 0, 0]);
        assert_eq!(plans[0].chunks[0].logical_extent, vec![1, 1, 7, 11]);
        fs::create_dir_all(root.path().join("0/c/1/0/0")).unwrap();
        fs::write(root.path().join("0/c/1/0/0/0"), vec![9_u8; 154]).unwrap();
        let loaded = session.read_local_layer_chunks(&plans, 154, 154).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].bytes, vec![9_u8; 154]);
        let (descriptors, _) = session
            .native_layer_admission(LayerRenderLimits::new(4, 4))
            .unwrap();
        let portable = session
            .native_portable_page_admission(descriptors.clone(), &plans, &loaded)
            .unwrap();
        let scene = session
            .native_portable_scene_page_admission(descriptors, &plans, &loaded)
            .unwrap();
        assert_eq!(portable.frame.descriptors[0].page_offset, 0);
        assert_eq!(portable.frame.descriptors[0].page_count, 1);
        assert_eq!(portable.frame.page_submission.pages[0], vec![0x0909; 77]);
        assert!(portable.frame.page_submission.pages[1].is_empty());
        assert_eq!(portable.dimensions_xyz, [11, 7, 1]);
        assert_eq!(portable.scalar_type, PortableScalarType::Uint16);
        assert_eq!(portable.channels.len(), 1);
        assert_eq!(portable.channels[0].transfer.color_srgb, [4, 5, 6]);
        assert_eq!(portable.channels[0].transfer.window_start, 2.0);
        assert_eq!(portable.channels[0].transfer.window_end, 9.0);
        assert_eq!(scene.layers.len(), 1);
        assert_eq!(scene.layers[0].dimensions_xyz, [11, 7, 1]);
        assert_eq!(scene.layers[0].channels[0].page_offset, 0);
        assert_eq!(portable.channels[0].transfer.opacity, 0.5);
        assert!(matches!(
            session.read_local_layer_chunks(&plans, 153, 154),
            Err(SessionError::LayerSource(message)) if message.contains("per-asset budget")
        ));
        fs::write(root.path().join("0/c/1/0/0/0"), vec![9_u8; 153]).unwrap();
        assert!(matches!(
            session.read_local_layer_chunks(&plans, 154, 154),
            Err(SessionError::LayerSource(message)) if message.contains("has 153 bytes")
        ));
        assert!(matches!(
            session.local_layer_chunk_plan(
                LayerRenderLimits::new(4, 4),
                SpatialChunkRegion::new([0, 0, 0], [1, 1, 1]),
                0,
            ),
            Err(SessionError::LayerSource(message)) if message.contains("capacity")
        ));
        assert!(matches!(
            session.local_layer_chunk_plan(
                LayerRenderLimits::new(4, 4),
                SpatialChunkRegion::new([1, 0, 0], [1, 1, 1]),
                4,
            ),
            Err(SessionError::LayerSource(message)) if message.contains("outside source chunk grid")
        ));
    }

    #[test]
    fn annotation_document_round_trip_is_dataset_bound_and_preserves_next_id() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join(".zattrs"),
            r#"{"multiscales":[{"axes":[{"name":"z"},{"name":"y"},{"name":"x"}],"datasets":[{"path":"0","coordinateTransformations":[{"type":"scale","scale":[1,1,1]}]}]}]}"#,
        )
        .unwrap();
        let document_path = root.path().join("annotations.newvolim.json");
        let mut source = LocalSession::default();
        source.open_local_omezarr(root.path()).unwrap();
        let annotation = source.add_point_annotation("saved", [3, 4, 5]).unwrap();
        let rectangle = source
            .add_rectangle_annotation("rectangle", [1, 1, 2], [4, 5, 2])
            .unwrap();
        let ellipse = source
            .add_ellipse_annotation("ellipse", [1, 1, 2], [4, 5, 2])
            .unwrap();
        source.export_annotations(&document_path).unwrap();

        let mut restored = LocalSession::default();
        restored.open_local_omezarr(root.path()).unwrap();
        restored.import_annotations(&document_path).unwrap();
        assert_eq!(restored.annotations(), &[annotation, rectangle, ellipse]);
        assert_eq!(
            restored
                .annotation_voxel_points(&restored.annotations()[0])
                .unwrap(),
            [[3, 4, 5]]
        );
        assert_eq!(
            restored.add_point_annotation("next", [1, 1, 1]).unwrap().id,
            AnnotationId(3)
        );

        let other = tempfile::tempdir().unwrap();
        fs::write(other.path().join(".zattrs"), r#"{"multiscales":[]}"#).unwrap();
        let mut wrong_dataset = LocalSession::default();
        wrong_dataset.open_local_omezarr(other.path()).unwrap();
        assert!(wrong_dataset.import_annotations(&document_path).is_err());
    }

    #[test]
    fn committed_cells3d_fixture_opens_with_its_real_xyz_extent() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        let mut session = LocalSession::default();

        let summary = session.open_local_omezarr(root).unwrap();

        assert_eq!(summary.voxel_shape_xyz, Some([128, 128, 32]));
        assert_eq!(summary.channel_count, 1);
        assert_eq!(summary.multiscale_count, 1);
    }

    #[test]
    fn explicit_portable_preparation_binds_one_metadata_derived_image_layer() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        let mut session = LocalSession::default();
        session.open_local_omezarr(root).unwrap();

        session.prepare_default_portable_image_layer().unwrap();
        let (descriptors, requests) = session
            .native_layer_admission(LayerRenderLimits::new(4, 4))
            .unwrap();
        assert_eq!(descriptors.len(), 1);
        assert_eq!(requests.len(), 1);
        assert_eq!(descriptors[0].layer_id, LayerId(0));
        assert!(session.prepare_default_portable_image_layer().is_err());
    }

    #[test]
    fn portable_preparation_and_admission_keep_two_active_ome_channels_separate() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join(".zattrs"),
            r#"{"multiscales":[{"axes":[{"name":"c"},{"name":"z"},{"name":"y"},{"name":"x"}],"datasets":[{"path":"0"}]}],"omero":{"channels":[{"active":true,"color":"FF0000","window":{"start":0,"end":1,"min":0,"max":1}},{"active":true,"color":"00FF00","window":{"start":0,"end":1,"min":0,"max":1}}]}}"#,
        )
        .unwrap();
        fs::create_dir(root.path().join("0")).unwrap();
        fs::write(
            root.path().join("0/zarr.json"),
            r#"{"shape":[2,1,2,2],"data_type":"uint16","chunk_grid":{"configuration":{"chunk_shape":[1,1,2,2]}}}"#,
        )
        .unwrap();
        for (channel, value) in [(0, 1_u8), (1, 2_u8)] {
            let directory = root.path().join(format!("0/c/{channel}/0/0"));
            fs::create_dir_all(&directory).unwrap();
            fs::write(
                directory.join("0"),
                vec![value, 0, value, 0, value, 0, value, 0],
            )
            .unwrap();
        }
        let mut session = LocalSession::default();
        session.open_local_omezarr(root.path()).unwrap();
        session.prepare_default_portable_image_layer().unwrap();
        let limits = LayerRenderLimits::new(4, 4);
        let (descriptors, requests) = session.native_layer_admission(limits).unwrap();
        assert_eq!(requests[0].layer.channels.len(), 2);
        assert_eq!(descriptors[0].page_count, 2);
        let plans = session
            .local_layer_chunk_plan(limits, SpatialChunkRegion::new([0, 0, 0], [1, 1, 1]), 2)
            .unwrap();
        let loaded = session.read_local_layer_chunks(&plans, 8, 16).unwrap();
        let volume = session
            .native_portable_page_admission(descriptors, &plans, &loaded)
            .unwrap();
        assert_eq!(volume.dimensions_xyz, [2, 2, 1]);
        assert_eq!(volume.channels.len(), 2);
        assert_eq!(volume.channels[0].page_offset, 0);
        assert_eq!(volume.channels[1].page_offset, 1);
        assert_eq!(volume.frame.page_submission.pages[0], vec![1; 4]);
        assert_eq!(volume.frame.page_submission.pages[1], vec![2; 4]);
        assert_eq!(volume.channels[0].transfer.color_srgb, [255, 0, 0]);
        assert_eq!(volume.channels[1].transfer.color_srgb, [0, 255, 0]);
    }

    #[test]
    fn portable_admission_assembles_adjacent_authorized_spatial_chunks() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        let mut session = LocalSession::default();
        session.open_local_omezarr(root).unwrap();
        session.prepare_default_portable_image_layer().unwrap();
        let limits = LayerRenderLimits::new(4, 4);
        let (descriptors, _) = session.native_layer_admission(limits).unwrap();
        let plans = session
            .local_layer_chunk_plan(limits, SpatialChunkRegion::new([0, 0, 0], [2, 1, 1]), 4)
            .unwrap();
        let loaded = session
            .read_local_layer_chunks(&plans, 16 * 1024 * 1024, 64 * 1024 * 1024)
            .unwrap();
        let volume = session
            .native_portable_page_admission(descriptors, &plans, &loaded)
            .unwrap();
        assert_eq!(volume.dimensions_xyz, [64, 32, 8]);
        assert_eq!(volume.frame.descriptors[0].page_count, 1);
    }

    #[test]
    fn opened_fixture_clamps_direct_slice_crosshairs_to_its_declared_extent() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        let mut session = LocalSession::default();
        session.open_local_omezarr(root).unwrap();

        assert_eq!(
            session.clamp_crosshair_xyz([15, 31, 16]).unwrap(),
            [15, 31, 16]
        );
        assert_eq!(
            session.clamp_crosshair_xyz([u32::MAX, 128, 32]).unwrap(),
            [127, 127, 31]
        );
    }
}
