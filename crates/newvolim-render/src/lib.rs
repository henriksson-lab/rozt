//! The contract between newvolim UI, local renderers, and frame servers.
//!
//! Extents are physical pixels, never CSS pixels. Colour encoding and the meaning of the depth
//! attachment are explicit, so a streamed palace Vulkan frame and a direct WebGPU frame cannot
//! silently disagree.

use std::{collections::HashMap, fmt};

use newvolim_scene::{
    Annotation, AnnotationGeometry, AnnotationId, ChannelState, LayerId, LayerKind, LayerTransform,
    PhysicalVec3, Scene,
};
use serde::Serialize;

/// Compose scalar image-channel samples into premultiplied linear-light RGBA. Disabled channels
/// contribute nothing; enabled channels use their declared native-unit display window and sRGB
/// colour before additive composition. This is the backend-neutral transfer-function contract
/// for future bounded multi-channel frame requests.
pub fn composite_additive_channels(channels: &[(ChannelState, f64)]) -> [f32; 4] {
    let mut output = [0.0; 4];
    for (channel, sample) in channels {
        if !channel.enabled || !sample.is_finite() {
            continue;
        }
        let density = if channel.window.start == channel.window.end {
            (*sample >= channel.window.end) as u8 as f32
        } else {
            ((*sample - channel.window.start) / (channel.window.end - channel.window.start))
                .clamp(0.0, 1.0) as f32
        };
        let alpha = density * channel.opacity;
        for (component, srgb) in output[..3].iter_mut().zip(channel.color_srgb) {
            *component += srgb_to_linear(srgb) * alpha;
        }
        output[3] = (output[3] + alpha).min(1.0);
    }
    output
}

/// Compose ordered image-layer samples in premultiplied linear light. Each inner slice is one
/// layer's channels; later slices are drawn over earlier slices, matching [`Scene`] ordering.
/// A mismatch is rejected rather than silently pairing a sample with the wrong C page.
pub fn composite_ordered_layers(
    layers: &[(&[ChannelState], &[f64])],
) -> Result<[f32; 4], LayerCompositeError> {
    let mut output = [0.0; 4];
    for (layer_index, (channels, samples)) in layers.iter().enumerate() {
        if channels.len() != samples.len() {
            return Err(LayerCompositeError::ChannelSampleCount {
                layer_index,
                channels: channels.len(),
                samples: samples.len(),
            });
        }
        let layer = composite_additive_channels(
            &channels
                .iter()
                .cloned()
                .zip(samples.iter().copied())
                .collect::<Vec<_>>(),
        );
        let remaining = 1.0 - layer[3];
        output = [
            layer[0] + output[0] * remaining,
            layer[1] + output[1] * remaining,
            layer[2] + output[2] * remaining,
            layer[3] + output[3] * remaining,
        ];
    }
    Ok(output)
}

/// Compose admitted portable scene samples using the same ordered premultiplied-linear rule as
/// [`composite_ordered_layers`], but directly from GPU-facing channel transfers. This is the
/// CPU oracle for native scene shader composition.
pub fn composite_portable_scene_samples(
    layers: &[(&[PortableChannelTransfer], &[f64])],
) -> Result<[f32; 4], LayerCompositeError> {
    let mut output = [0.0; 4];
    for (layer_index, (channels, samples)) in layers.iter().enumerate() {
        if channels.len() != samples.len() {
            return Err(LayerCompositeError::ChannelSampleCount {
                layer_index,
                channels: channels.len(),
                samples: samples.len(),
            });
        }
        let mut layer = [0.0; 4];
        for (channel, sample) in channels.iter().zip(*samples) {
            if !sample.is_finite() {
                continue;
            }
            let intensity = if channel.window_start == channel.window_end {
                (*sample >= channel.window_end) as u8 as f32
            } else {
                ((*sample - channel.window_start) / (channel.window_end - channel.window_start))
                    .clamp(0.0, 1.0) as f32
            };
            let alpha = intensity * channel.opacity;
            for (component, srgb) in layer[..3].iter_mut().zip(channel.color_srgb) {
                *component += srgb_to_linear(srgb) * alpha;
            }
            layer[3] = (layer[3] + alpha).min(1.0);
        }
        let remaining = 1.0 - layer[3];
        output = [
            layer[0] + output[0] * remaining,
            layer[1] + output[1] * remaining,
            layer[2] + output[2] * remaining,
            layer[3] + output[3] * remaining,
        ];
    }
    Ok(output)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayerCompositeError {
    ChannelSampleCount {
        layer_index: usize,
        channels: usize,
        samples: usize,
    },
}

impl fmt::Display for LayerCompositeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ChannelSampleCount {
                layer_index,
                channels,
                samples,
            } => write!(
                formatter,
                "layer {layer_index} has {channels} channels but {samples} samples"
            ),
        }
    }
}

impl std::error::Error for LayerCompositeError {}

fn srgb_to_linear(value: u8) -> f32 {
    let value = f32::from(value) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PhysicalExtent {
    pub width: u32,
    pub height: u32,
}

impl PhysicalExtent {
    pub fn new(width: u32, height: u32) -> Result<Self, RenderContractError> {
        if width == 0 || height == 0 {
            return Err(RenderContractError::ZeroExtent { width, height });
        }
        Ok(Self { width, height })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ColorFormat {
    Rgba8Unorm,
    Rgba16Float,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ColorEncoding {
    /// Samples are linear-light values. A display blit or encoder must apply its OETF.
    Linear,
    /// Samples are display-encoded sRGB values.
    Srgb,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DepthAttachment {
    /// No geometry/volume occlusion information is requested.
    None,
    /// A `f32` world-space distance from the camera ray origin to the first volume sample that
    /// contributes opacity. `+∞` represents no hit. It is the value annotations use as `t_max`.
    RayDistanceF32,
}

/// One validated value from a [`DepthAttachment::RayDistanceF32`] surface.
///
/// A renderer may use positive infinity for a ray that did not contribute volume opacity, but
/// must never pass NaN, negative infinity, or a negative distance into picking/compositing.
/// Keeping that check at the attachment boundary prevents an invalid GPU readback from quietly
/// becoming an unbounded annotation pick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayDistance(f32);

impl RayDistance {
    pub fn new(value: f32) -> Result<Self, RayDistanceError> {
        if value >= 0.0 && (value.is_finite() || value == f32::INFINITY) {
            Ok(Self(value))
        } else {
            Err(RayDistanceError::Invalid(value))
        }
    }

    pub fn no_hit() -> Self {
        Self(f32::INFINITY)
    }

    pub fn value(self) -> f32 {
        self.0
    }

    pub fn is_no_hit(self) -> bool {
        self.0 == f32::INFINITY
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderTarget {
    pub extent: PhysicalExtent,
    pub color_format: ColorFormat,
    pub color_encoding: ColorEncoding,
    pub depth: DepthAttachment,
}

impl RenderTarget {
    pub fn new(
        extent: PhysicalExtent,
        color_format: ColorFormat,
        color_encoding: ColorEncoding,
        depth: DepthAttachment,
    ) -> Result<Self, RenderContractError> {
        if color_format == ColorFormat::Rgba16Float && color_encoding == ColorEncoding::Srgb {
            return Err(RenderContractError::EncodedFloatTarget);
        }
        Ok(Self {
            extent,
            color_format,
            color_encoding,
            depth,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RenderGeneration(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FrameProgress {
    Preview,
    Refining { pass: u32 },
    Final,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderRequest {
    pub generation: RenderGeneration,
    pub target: RenderTarget,
}

/// Backend limits for the statically-bound image-layer resources in one render pass.
///
/// A caller must choose these from the actual backend pipeline limits.  Exceeding a limit is an
/// admission error, never an excuse to silently omit an image layer or a selected channel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LayerRenderLimits {
    pub max_image_layers: usize,
    pub max_channels_per_layer: usize,
}

impl LayerRenderLimits {
    pub const fn new(max_image_layers: usize, max_channels_per_layer: usize) -> Self {
        Self {
            max_image_layers,
            max_channels_per_layer,
        }
    }
}

/// One enabled channel, retaining its source index rather than renumbering selected channels.
/// That index is the stable mapping an IO adapter uses to select its C page.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectedChannel {
    pub source_index: u32,
    pub state: ChannelState,
}

/// Renderer-ready intent for one visible image layer.  Entries remain in scene order, so a
/// compositor can apply the documented "later over earlier" rule without inferring it from IDs.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageLayerRenderRequest {
    pub layer_id: LayerId,
    pub transform: LayerTransform,
    pub channels: Vec<SelectedChannel>,
}

/// A bounded, backend-neutral rendering projection of a [`Scene`].
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerRenderPlan {
    pub image_layers: Vec<ImageLayerRenderRequest>,
}

/// One statically-bound page range assigned to an ordered image layer.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeLayerDescriptor {
    pub layer_id: LayerId,
    pub page_offset: u32,
    pub page_count: u32,
    pub transform: LayerTransform,
}

/// One page upload for the portable four-storage-buffer capability floor.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortablePageUpload {
    pub page: u32,
    pub words: Vec<u32>,
}

/// Validated, fixed-order storage pages shared by native admission and the wgpu recorder.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortablePageSubmission {
    pub pages: [Vec<u32>; 4],
}

impl PortablePageSubmission {
    pub const PAGE_COUNT: u32 = 4;
    pub const PAGE_BYTES: u64 = 4 * 1024 * 1024;

    pub fn from_uploads(
        uploads: impl IntoIterator<Item = PortablePageUpload>,
    ) -> Result<Self, LayerRenderError> {
        let mut pages: [Option<Vec<u32>>; Self::PAGE_COUNT as usize] =
            std::array::from_fn(|_| None);
        for upload in uploads {
            let slot = usize::try_from(upload.page)
                .ok()
                .filter(|page| *page < Self::PAGE_COUNT as usize)
                .ok_or(LayerRenderError::PortablePageOutOfRange { page: upload.page })?;
            let bytes = upload.words.len() as u64 * std::mem::size_of::<u32>() as u64;
            if bytes > Self::PAGE_BYTES {
                return Err(LayerRenderError::PortablePageTooLarge {
                    page: upload.page,
                    bytes,
                    capacity: Self::PAGE_BYTES,
                });
            }
            if pages[slot].replace(upload.words).is_some() {
                return Err(LayerRenderError::DuplicatePortablePage { page: upload.page });
            }
        }
        Ok(Self {
            pages: pages.map(|page| page.unwrap_or_default()),
        })
    }
}

/// The complete portable native frame input.  Page bytes never travel without the ordered layer
/// descriptors that identify their scene layer, static page range, and physical transform.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativePortableFrameInput {
    pub descriptors: Vec<NativeLayerDescriptor>,
    pub page_submission: PortablePageSubmission,
}

impl NativePortableFrameInput {
    pub fn new(
        descriptors: Vec<NativeLayerDescriptor>,
        page_submission: PortablePageSubmission,
    ) -> Result<Self, LayerRenderError> {
        let mut next_page = 0_u32;
        for descriptor in &descriptors {
            let end = descriptor
                .page_offset
                .checked_add(descriptor.page_count)
                .ok_or(LayerRenderError::PortableDescriptorPageRange {
                    layer_id: descriptor.layer_id,
                    page_offset: descriptor.page_offset,
                    page_count: descriptor.page_count,
                })?;
            if descriptor.page_count == 0
                || descriptor.page_offset != next_page
                || end > PortablePageSubmission::PAGE_COUNT
            {
                return Err(LayerRenderError::PortableDescriptorPageRange {
                    layer_id: descriptor.layer_id,
                    page_offset: descriptor.page_offset,
                    page_count: descriptor.page_count,
                });
            }
            next_page = end;
        }
        Ok(Self {
            descriptors,
            page_submission,
        })
    }
}

/// Original scalar width retained after native admission expands samples to storage `u32` words.
/// The renderer needs this to normalize values correctly; it cannot infer it from the word
/// buffer alone.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PortableScalarType {
    Uint8,
    Uint16,
    Uint32,
}

/// The one selected channel's declared display transfer. Values stay in native sample units;
/// conversion to storage `u32` must not silently normalize a window.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableChannelTransfer {
    pub color_srgb: [u8; 3],
    pub window_start: f64,
    pub window_end: f64,
    pub opacity: f32,
}

impl From<&SelectedChannel> for PortableChannelTransfer {
    fn from(channel: &SelectedChannel) -> Self {
        Self {
            color_srgb: channel.state.color_srgb,
            window_start: channel.state.window.start,
            window_end: channel.state.window.end,
            opacity: channel.state.opacity,
        }
    }
}

/// One selected channel's statically allocated pages and declared display transfer. Each channel
/// uses the direct volume's shared XYZ dimensions, but its final partial page is never reused by
/// the next channel.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableVolumeChannel {
    pub page_offset: u32,
    pub page_count: u32,
    pub transfer: PortableChannelTransfer,
}

/// A directly sampleable single-layer specialization of [`NativePortableFrameInput`]. A logical
/// volume may occupy one to four consecutive static pages in linear voxel order. Explicit
/// channel ranges preserve transfer ownership while retaining the fixed page-pool bound.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativePortableVolumeInput {
    pub frame: NativePortableFrameInput,
    pub dimensions_xyz: [u32; 3],
    pub scalar_type: PortableScalarType,
    pub channels: Vec<PortableVolumeChannel>,
}

/// One explicitly admitted layer in a direct portable scene. Channel page offsets are absolute
/// offsets into the scene's one fixed page submission, while `voxel_origin_xyz` states where the
/// local page coordinates begin in that layer's source voxel space.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableSceneLayerInput {
    pub layer_id: LayerId,
    pub transform: LayerTransform,
    pub voxel_origin_xyz: [u64; 3],
    pub dimensions_xyz: [u32; 3],
    pub scalar_type: PortableScalarType,
    pub channels: Vec<PortableVolumeChannel>,
}

impl PortableSceneLayerInput {
    /// Return the physical-world ray interval intersecting this admitted local page. The ray is
    /// converted through the layer transform without normalizing the local direction, so the
    /// returned parameter remains directly comparable across transformed layers.
    pub fn world_ray_interval(&self, ray: PortableWorldRay) -> Option<(f64, f64)> {
        let local = ray.to_layer(self.transform, self.voxel_origin_xyz);
        let mut entry = f64::NEG_INFINITY;
        let mut exit = f64::INFINITY;
        for axis in 0..3 {
            let origin = local.origin_xyz[axis];
            let direction = local.direction_xyz[axis];
            let upper = f64::from(self.dimensions_xyz[axis]);
            if direction.abs() < f64::EPSILON {
                if origin < 0.0 || origin > upper {
                    return None;
                }
                continue;
            }
            let first = -origin / direction;
            let second = (upper - origin) / direction;
            entry = entry.max(first.min(second));
            exit = exit.min(first.max(second));
        }
        (exit > entry).then_some((entry.max(0.0), exit))
    }
}

/// A bounded ordered portable scene over the shared four-page pool. Unlike
/// [`NativePortableVolumeInput`], this retains all admitted layer transforms and page ownership
/// so a renderer can convert one world-space camera ray into every local layer independently.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativePortableSceneInput {
    pub frame: NativePortableFrameInput,
    pub layers: Vec<PortableSceneLayerInput>,
}

impl NativePortableSceneInput {
    pub fn new(
        frame: NativePortableFrameInput,
        layers: Vec<PortableSceneLayerInput>,
    ) -> Result<Self, LayerRenderError> {
        if layers.is_empty() || layers.len() != frame.descriptors.len() {
            return Err(LayerRenderError::PortableDirectVolumeRequiresOnePage);
        }
        let page_words =
            usize::try_from(PortablePageSubmission::PAGE_BYTES / std::mem::size_of::<u32>() as u64)
                .expect("portable page word capacity fits usize");
        for (layer, descriptor) in layers.iter().zip(&frame.descriptors) {
            if layer.layer_id != descriptor.layer_id || layer.transform != descriptor.transform {
                return Err(LayerRenderError::PortableDirectVolumeRequiresOnePage);
            }
            if layer.dimensions_xyz.contains(&0) || layer.channels.is_empty() {
                return Err(LayerRenderError::PortableVolumeDimensions {
                    dimensions_xyz: layer.dimensions_xyz,
                });
            }
            let expected_words = layer
                .dimensions_xyz
                .iter()
                .try_fold(1_usize, |total, dimension| {
                    total.checked_mul(*dimension as usize)
                })
                .ok_or(LayerRenderError::PortableVolumeWordCount {
                    dimensions_xyz: layer.dimensions_xyz,
                    words: 0,
                })?;
            let pages_per_channel = expected_words.div_ceil(page_words);
            let mut next_page = descriptor.page_offset;
            for channel in &layer.channels {
                if !channel.transfer.window_start.is_finite()
                    || !channel.transfer.window_end.is_finite()
                    || channel.transfer.window_start > channel.transfer.window_end
                    || !channel.transfer.opacity.is_finite()
                    || !(0.0..=1.0).contains(&channel.transfer.opacity)
                {
                    return Err(LayerRenderError::PortableVolumeTransfer);
                }
                let end = channel
                    .page_offset
                    .checked_add(channel.page_count)
                    .ok_or(LayerRenderError::PortableDirectVolumeRequiresOnePage)?;
                if channel.page_offset != next_page
                    || channel.page_count as usize != pages_per_channel
                    || end > descriptor.page_offset + descriptor.page_count
                {
                    return Err(LayerRenderError::PortableDirectVolumeRequiresOnePage);
                }
                let pages =
                    &frame.page_submission.pages[channel.page_offset as usize..end as usize];
                let words = pages
                    .iter()
                    .try_fold(0_usize, |total, page| total.checked_add(page.len()));
                if pages[..pages_per_channel.saturating_sub(1)]
                    .iter()
                    .any(|page| page.len() != page_words)
                    || words != Some(expected_words)
                {
                    return Err(LayerRenderError::PortableVolumeWordCount {
                        dimensions_xyz: layer.dimensions_xyz,
                        words: words.unwrap_or(usize::MAX),
                    });
                }
                next_page = end;
            }
            if next_page != descriptor.page_offset + descriptor.page_count {
                return Err(LayerRenderError::PortableDirectVolumeRequiresOnePage);
            }
        }
        Ok(Self { frame, layers })
    }

    /// A conservative physical-world march step for all admitted layers. Sampling at half the
    /// smallest voxel spacing cannot skip a layer merely because another layer is coarser or
    /// anisotropic; the shader uses the same scalar for its common world-distance loop.
    pub fn world_ray_step(&self) -> f64 {
        self.layers
            .iter()
            .flat_map(|layer| layer.transform.scale)
            .fold(f64::INFINITY, f64::min)
            * 0.5
    }
}

impl NativePortableVolumeInput {
    pub fn new(
        frame: NativePortableFrameInput,
        dimensions_xyz: [u32; 3],
        scalar_type: PortableScalarType,
        transfer: PortableChannelTransfer,
    ) -> Result<Self, LayerRenderError> {
        let page_count = frame
            .descriptors
            .first()
            .filter(|_| frame.descriptors.len() == 1)
            .map(|descriptor| descriptor.page_count)
            .unwrap_or(0);
        Self::new_channels(
            frame,
            dimensions_xyz,
            scalar_type,
            vec![PortableVolumeChannel {
                page_offset: 0,
                page_count,
                transfer,
            }],
        )
    }

    /// Admit ordered channel page ranges for one direct image layer. The legacy [`Self::new`]
    /// constructor remains the single-channel spelling of this exact contract.
    pub fn new_channels(
        frame: NativePortableFrameInput,
        dimensions_xyz: [u32; 3],
        scalar_type: PortableScalarType,
        channels: Vec<PortableVolumeChannel>,
    ) -> Result<Self, LayerRenderError> {
        if dimensions_xyz.contains(&0) {
            return Err(LayerRenderError::PortableVolumeDimensions { dimensions_xyz });
        }
        let descriptor = frame
            .descriptors
            .first()
            .filter(|_| frame.descriptors.len() == 1)
            .ok_or(LayerRenderError::PortableDirectVolumeRequiresOnePage)?;
        if descriptor.page_offset != 0 {
            return Err(LayerRenderError::PortableDirectVolumeRequiresOnePage);
        }
        let expected_words = dimensions_xyz
            .iter()
            .try_fold(1_usize, |total, dimension| {
                total.checked_mul(*dimension as usize)
            })
            .ok_or(LayerRenderError::PortableVolumeWordCount {
                dimensions_xyz,
                words: 0,
            })?;
        let page_words =
            usize::try_from(PortablePageSubmission::PAGE_BYTES / std::mem::size_of::<u32>() as u64)
                .expect("portable page word capacity fits usize");
        let pages_per_channel = expected_words.div_ceil(page_words);
        let mut next_page = 0_u32;
        if channels.is_empty() {
            return Err(LayerRenderError::PortableDirectVolumeRequiresOnePage);
        }
        for channel in &channels {
            if !channel.transfer.window_start.is_finite()
                || !channel.transfer.window_end.is_finite()
                || channel.transfer.window_start > channel.transfer.window_end
                || !channel.transfer.opacity.is_finite()
                || !(0.0..=1.0).contains(&channel.transfer.opacity)
            {
                return Err(LayerRenderError::PortableVolumeTransfer);
            }
            let end = channel
                .page_offset
                .checked_add(channel.page_count)
                .ok_or(LayerRenderError::PortableDirectVolumeRequiresOnePage)?;
            if channel.page_offset != next_page
                || channel.page_count as usize != pages_per_channel
                || end > descriptor.page_count
            {
                return Err(LayerRenderError::PortableDirectVolumeRequiresOnePage);
            }
            let pages = &frame.page_submission.pages[channel.page_offset as usize..end as usize];
            let words = pages
                .iter()
                .try_fold(0_usize, |total, page| total.checked_add(page.len()));
            if pages[..pages_per_channel.saturating_sub(1)]
                .iter()
                .any(|page| page.len() != page_words)
                || words != Some(expected_words)
            {
                return Err(LayerRenderError::PortableVolumeWordCount {
                    dimensions_xyz,
                    words: words.unwrap_or(usize::MAX),
                });
            }
            next_page = end;
        }
        if next_page != descriptor.page_count
            || frame.page_submission.pages[next_page as usize..]
                .iter()
                .any(|page| !page.is_empty())
        {
            return Err(LayerRenderError::PortableVolumeWordCount {
                dimensions_xyz,
                words: frame.page_submission.pages[..next_page as usize]
                    .iter()
                    .map(Vec::len)
                    .sum(),
            });
        }
        Ok(Self {
            frame,
            dimensions_xyz,
            scalar_type,
            channels,
        })
    }
}

/// One camera-specific native recorder input. Annotation words are accepted only in the fixed
/// projected-primitive layout, so a recorder cannot accidentally pair arbitrary bytes with a
/// volume/depth pass from another camera or extent.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableCameraControls {
    pub orbit_delta: [i32; 2],
    pub zoom: f32,
}

impl PortableCameraControls {
    pub const MAX_ORBIT_DELTA: i32 = 10_000;
    pub const MIN_ZOOM: f32 = 0.25;
    pub const MAX_ZOOM: f32 = 4.0;

    pub fn new(orbit_delta: [i32; 2], zoom: f32) -> Result<Self, LayerRenderError> {
        if !zoom.is_finite()
            || !(Self::MIN_ZOOM..=Self::MAX_ZOOM).contains(&zoom)
            || orbit_delta
                .iter()
                .any(|delta| delta.unsigned_abs() > Self::MAX_ORBIT_DELTA as u32)
        {
            return Err(LayerRenderError::PortableDrawCamera { orbit_delta, zoom });
        }
        Ok(Self { orbit_delta, zoom })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativePortableDrawInput {
    pub volume: NativePortableVolumeInput,
    pub extent_pixels: [u32; 2],
    pub camera: PortableCameraControls,
    pub annotation_words: Vec<u32>,
}

/// One normalized camera ray in the direct portable volume's XYZ voxel coordinates.
///
/// Palace's fitted camera is intentionally represented as rays rather than a backend-specific
/// matrix. This lets a portable recorder march exactly the camera used to project annotations,
/// including its perspective and trackball orientation.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableCameraRay {
    pub origin_xyz: [f32; 3],
    pub direction_xyz: [f32; 3],
}

/// One normalized physical-world camera ray shared by every layer in a portable scene packet.
/// Keeping it in world units prevents a layer's anisotropic scale or chunk translation from
/// changing the camera authority used by another layer.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableWorldRay {
    pub origin_world: PhysicalVec3,
    pub direction_world: PhysicalVec3,
}

/// One world ray expressed in an admitted layer page's local XYZ voxel coordinates. The local
/// direction deliberately is not normalized: retaining its inverse anisotropic scale makes the
/// ray parameter remain a physical-world distance through every layer.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableLayerRay {
    pub origin_xyz: PhysicalVec3,
    pub direction_xyz: PhysicalVec3,
}

impl PortableWorldRay {
    pub fn new(
        origin_world: PhysicalVec3,
        direction_world: PhysicalVec3,
    ) -> Result<Self, LayerRenderError> {
        let length_squared: f64 = direction_world.iter().map(|value| value * value).sum();
        if !origin_world.iter().all(|value| value.is_finite())
            || !direction_world.iter().all(|value| value.is_finite())
            || !(0.999..=1.001).contains(&length_squared.sqrt())
        {
            return Err(LayerRenderError::PortableCameraRay);
        }
        Ok(Self {
            origin_world,
            direction_world,
        })
    }

    /// Convert this shared physical ray into one layer page. `voxel_origin_xyz` is the global
    /// source-voxel start of that page, so both chunk residency and the layer transform remain
    /// explicit rather than being folded into a backend-specific camera matrix.
    pub fn to_layer(
        self,
        transform: LayerTransform,
        voxel_origin_xyz: [u64; 3],
    ) -> PortableLayerRay {
        let global_voxel = transform.world_to_voxel(self.origin_world);
        PortableLayerRay {
            origin_xyz: std::array::from_fn(|axis| {
                global_voxel[axis] - voxel_origin_xyz[axis] as f64
            }),
            direction_xyz: transform.world_direction_to_voxel(self.direction_world),
        }
    }
}

/// A portable scene draw before its physical camera ray table is attached. Annotation records
/// retain the same fixed, projected representation as direct-volume draws.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativePortableSceneDrawInput {
    pub scene: NativePortableSceneInput,
    pub extent_pixels: [u32; 2],
    pub annotation_words: Vec<u32>,
}

/// A camera-complete portable scene packet. Every ray is normalized in physical world units;
/// the renderer converts it to each scene layer through [`PortableWorldRay::to_layer`].
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativePortableSceneCameraDrawInput {
    pub draw: NativePortableSceneDrawInput,
    pub rays: Vec<PortableWorldRay>,
}

impl NativePortableSceneDrawInput {
    pub fn new(
        scene: NativePortableSceneInput,
        extent_pixels: [u32; 2],
        annotation_words: Vec<u32>,
    ) -> Result<Self, LayerRenderError> {
        if extent_pixels.contains(&0) {
            return Err(LayerRenderError::PortableDrawExtent { extent_pixels });
        }
        if !annotation_words
            .len()
            .is_multiple_of(PortableAnnotationPrimitive::WORDS)
        {
            return Err(LayerRenderError::PortableAnnotationWords {
                words: annotation_words.len(),
            });
        }
        if annotation_words.len() / PortableAnnotationPrimitive::WORDS
            > NativePortableDrawInput::MAX_ANNOTATION_PRIMITIVES
        {
            return Err(LayerRenderError::TooManyPortableAnnotations {
                requested: annotation_words.len() / PortableAnnotationPrimitive::WORDS,
                capacity: NativePortableDrawInput::MAX_ANNOTATION_PRIMITIVES,
            });
        }
        Ok(Self {
            scene,
            extent_pixels,
            annotation_words,
        })
    }
}

impl NativePortableSceneCameraDrawInput {
    pub fn new(
        draw: NativePortableSceneDrawInput,
        rays: Vec<PortableWorldRay>,
    ) -> Result<Self, LayerRenderError> {
        let expected = usize::try_from(draw.extent_pixels[0])
            .ok()
            .and_then(|width| width.checked_mul(draw.extent_pixels[1] as usize))
            .ok_or(LayerRenderError::PortableCameraRayCount {
                expected: usize::MAX,
                actual: rays.len(),
            })?;
        if expected > NativePortableCameraDrawInput::MAX_RAYS || rays.len() != expected {
            return Err(LayerRenderError::PortableCameraRayCount {
                expected,
                actual: rays.len(),
            });
        }
        if rays
            .iter()
            .any(|ray| PortableWorldRay::new(ray.origin_world, ray.direction_world).is_err())
        {
            return Err(LayerRenderError::PortableCameraRay);
        }
        Ok(Self { draw, rays })
    }
}

/// A camera-complete direct portable draw packet. The ray table has one entry per physical pixel
/// in top-to-bottom raster order, so annotations and first-opacity distances retain one camera
/// authority across desktop, headless, and portable GPU renderers.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativePortableCameraDrawInput {
    pub draw: NativePortableDrawInput,
    pub rays: Vec<PortableCameraRay>,
}

impl NativePortableCameraDrawInput {
    /// Prevent a malformed or enormous desktop handoff from turning a bounded frame request into
    /// an unbounded ray upload. Four million rays occupy 96 MiB in the fixed six-f32 layout.
    pub const MAX_RAYS: usize = 4_194_304;

    pub fn new(
        draw: NativePortableDrawInput,
        rays: Vec<PortableCameraRay>,
    ) -> Result<Self, LayerRenderError> {
        let expected = usize::try_from(draw.extent_pixels[0])
            .ok()
            .and_then(|width| width.checked_mul(draw.extent_pixels[1] as usize))
            .ok_or(LayerRenderError::PortableCameraRayCount {
                expected: usize::MAX,
                actual: rays.len(),
            })?;
        if expected > Self::MAX_RAYS || rays.len() != expected {
            return Err(LayerRenderError::PortableCameraRayCount {
                expected,
                actual: rays.len(),
            });
        }
        if rays.iter().any(|ray| {
            !ray.origin_xyz.iter().all(|value| value.is_finite())
                || !ray.direction_xyz.iter().all(|value| value.is_finite())
                || {
                    let length_squared: f32 =
                        ray.direction_xyz.iter().map(|value| value * value).sum();
                    !(0.999..=1.001).contains(&length_squared.sqrt())
                }
        }) {
            return Err(LayerRenderError::PortableCameraRay);
        }
        Ok(Self { draw, rays })
    }
}

impl NativePortableDrawInput {
    pub const MAX_ANNOTATION_PRIMITIVES: usize = 4_096;

    pub fn new(
        volume: NativePortableVolumeInput,
        extent_pixels: [u32; 2],
        camera: PortableCameraControls,
        annotation_words: Vec<u32>,
    ) -> Result<Self, LayerRenderError> {
        if extent_pixels.contains(&0) {
            return Err(LayerRenderError::PortableDrawExtent { extent_pixels });
        }
        if !annotation_words
            .len()
            .is_multiple_of(PortableAnnotationPrimitive::WORDS)
        {
            return Err(LayerRenderError::PortableAnnotationWords {
                words: annotation_words.len(),
            });
        }
        if annotation_words.len() / PortableAnnotationPrimitive::WORDS
            > Self::MAX_ANNOTATION_PRIMITIVES
        {
            return Err(LayerRenderError::TooManyPortableAnnotations {
                requested: annotation_words.len() / PortableAnnotationPrimitive::WORDS,
                capacity: Self::MAX_ANNOTATION_PRIMITIVES,
            });
        }
        Ok(Self {
            volume,
            extent_pixels,
            camera,
            annotation_words,
        })
    }
}

/// Allocate the portable four-page pool in scene order. A layer consumes one page per selected
/// channel; callers get an error instead of an accidental truncation.
pub fn native_layer_descriptors(
    plan: &LayerRenderPlan,
) -> Result<Vec<NativeLayerDescriptor>, LayerRenderError> {
    let mut page_offset = 0_u32;
    plan.image_layers
        .iter()
        .map(|layer| {
            let page_count = u32::try_from(layer.channels.len()).map_err(|_| {
                LayerRenderError::ChannelIndexOverflow {
                    layer_id: layer.layer_id,
                    source_index: layer.channels.len(),
                }
            })?;
            let end = page_offset.checked_add(page_count).ok_or(
                LayerRenderError::TooManyImageLayers {
                    requested: usize::MAX,
                    capacity: 4,
                },
            )?;
            if end > 4 {
                return Err(LayerRenderError::TooManyImageLayers {
                    requested: end as usize,
                    capacity: 4,
                });
            }
            let descriptor = NativeLayerDescriptor {
                layer_id: layer.layer_id,
                page_offset,
                page_count,
                transform: layer.transform,
            };
            page_offset = end;
            Ok(descriptor)
        })
        .collect()
}

impl LayerRenderPlan {
    /// Select visible image layers and their enabled channels in scene order.
    ///
    /// Invisible layers, non-image layers, and image layers with no enabled channels do not
    /// consume GPU bindings.  Disabled channels are deliberately absent, while their original
    /// C index is retained by every selected channel.
    pub fn from_scene(scene: &Scene, limits: LayerRenderLimits) -> Result<Self, LayerRenderError> {
        if limits.max_image_layers == 0 {
            return Err(LayerRenderError::ZeroImageLayerCapacity);
        }
        if limits.max_channels_per_layer == 0 {
            return Err(LayerRenderError::ZeroChannelCapacity);
        }

        let mut image_layers = Vec::new();
        for layer in scene.layers() {
            if !layer.visible || layer.kind != LayerKind::Image {
                continue;
            }
            let channels: Vec<_> = layer
                .channels
                .iter()
                .enumerate()
                .filter(|(_, channel)| channel.enabled)
                .map(|(source_index, state)| {
                    Ok(SelectedChannel {
                        source_index: u32::try_from(source_index).map_err(|_| {
                            LayerRenderError::ChannelIndexOverflow {
                                layer_id: layer.id,
                                source_index,
                            }
                        })?,
                        state: state.clone(),
                    })
                })
                .collect::<Result<_, _>>()?;
            if channels.is_empty() {
                continue;
            }
            if channels.len() > limits.max_channels_per_layer {
                return Err(LayerRenderError::TooManyChannels {
                    layer_id: layer.id,
                    requested: channels.len(),
                    capacity: limits.max_channels_per_layer,
                });
            }
            if image_layers.len() == limits.max_image_layers {
                return Err(LayerRenderError::TooManyImageLayers {
                    requested: image_layers.len() + 1,
                    capacity: limits.max_image_layers,
                });
            }
            image_layers.push(ImageLayerRenderRequest {
                layer_id: layer.id,
                transform: layer.transform,
                channels,
            });
        }
        Ok(Self { image_layers })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum LayerRenderError {
    ZeroImageLayerCapacity,
    ZeroChannelCapacity,
    TooManyImageLayers {
        requested: usize,
        capacity: usize,
    },
    TooManyChannels {
        layer_id: LayerId,
        requested: usize,
        capacity: usize,
    },
    ChannelIndexOverflow {
        layer_id: LayerId,
        source_index: usize,
    },
    PortablePageOutOfRange {
        page: u32,
    },
    DuplicatePortablePage {
        page: u32,
    },
    PortablePageTooLarge {
        page: u32,
        bytes: u64,
        capacity: u64,
    },
    PortableDescriptorPageRange {
        layer_id: LayerId,
        page_offset: u32,
        page_count: u32,
    },
    PortableDirectVolumeRequiresOnePage,
    PortableVolumeDimensions {
        dimensions_xyz: [u32; 3],
    },
    PortableVolumeWordCount {
        dimensions_xyz: [u32; 3],
        words: usize,
    },
    PortableVolumeTransfer,
    PortableDrawExtent {
        extent_pixels: [u32; 2],
    },
    PortableDrawCamera {
        orbit_delta: [i32; 2],
        zoom: f32,
    },
    PortableCameraRayCount {
        expected: usize,
        actual: usize,
    },
    PortableCameraRay,
    PortableAnnotationWords {
        words: usize,
    },
    TooManyPortableAnnotations {
        requested: usize,
        capacity: usize,
    },
}

impl fmt::Display for LayerRenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroImageLayerCapacity => write!(formatter, "image-layer capacity must be non-zero"),
            Self::ZeroChannelCapacity => write!(formatter, "per-layer channel capacity must be non-zero"),
            Self::TooManyImageLayers { requested, capacity } => write!(
                formatter,
                "render request has {requested} visible image layers, but backend capacity is {capacity}"
            ),
            Self::TooManyChannels { layer_id, requested, capacity } => write!(
                formatter,
                "image layer {} has {requested} enabled channels, but backend capacity is {capacity}",
                layer_id.0
            ),
            Self::ChannelIndexOverflow { layer_id, source_index } => write!(
                formatter,
                "channel index {source_index} for image layer {} cannot be represented by the GPU contract",
                layer_id.0
            ),
            Self::PortablePageOutOfRange { page } => {
                write!(formatter, "portable storage page {page} is outside 0..4")
            }
            Self::DuplicatePortablePage { page } => {
                write!(formatter, "portable storage page {page} was uploaded more than once")
            }
            Self::PortablePageTooLarge {
                page,
                bytes,
                capacity,
            } => write!(
                formatter,
                "portable storage page {page} has {bytes} bytes, exceeding its {capacity}-byte binding limit"
            ),
            Self::PortableDescriptorPageRange {
                layer_id,
                page_offset,
                page_count,
            } => write!(
                formatter,
                "image layer {} has invalid portable page range {page_offset}+{page_count}",
                layer_id.0
            ),
            Self::PortableDirectVolumeRequiresOnePage => write!(
                formatter,
                "direct portable volume input requires exactly one descriptor beginning at page 0"
            ),
            Self::PortableVolumeDimensions { dimensions_xyz } => write!(
                formatter,
                "portable volume dimensions {dimensions_xyz:?} must be non-zero"
            ),
            Self::PortableVolumeWordCount {
                dimensions_xyz,
                words,
            } => write!(
                formatter,
                "portable volume dimensions {dimensions_xyz:?} require a matching word count, got {words}"
            ),
            Self::PortableVolumeTransfer => write!(
                formatter,
                "portable volume transfer has an invalid native-unit window or opacity"
            ),
            Self::PortableDrawExtent { extent_pixels } => write!(
                formatter,
                "portable draw extent {extent_pixels:?} must be non-zero"
            ),
            Self::PortableDrawCamera { orbit_delta, zoom } => write!(
                formatter,
                "portable draw camera has orbit {orbit_delta:?} or zoom {zoom} outside the Palace control bounds"
            ),
            Self::PortableCameraRayCount { expected, actual } => write!(
                formatter,
                "portable camera has {actual} rays; its physical extent requires {expected}"
            ),
            Self::PortableCameraRay => write!(
                formatter,
                "portable camera ray has non-finite components or a non-unit direction"
            ),
            Self::PortableAnnotationWords { words } => write!(
                formatter,
                "portable annotation stream has {words} words, not a multiple of {}",
                PortableAnnotationPrimitive::WORDS
            ),
            Self::TooManyPortableAnnotations { requested, capacity } => write!(
                formatter,
                "portable draw has {requested} annotation primitives, exceeding capacity {capacity}"
            ),
        }
    }
}

impl std::error::Error for LayerRenderError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameDescriptor {
    pub generation: RenderGeneration,
    pub target: RenderTarget,
    pub progress: FrameProgress,
}

/// Coalesces input while preserving work already admitted to a non-cancellable renderer.
///
/// This is the Stage-2 boundary form of cancellation: a camera fling can replace queued work,
/// but an already-dispatched palace task remains a valid low-priority cache producer. A later
/// palace-native generation check can use the same [`RenderGeneration`] value.
#[derive(Clone, Debug, Default)]
pub struct RenderAdmission {
    in_flight: Option<RenderRequest>,
    pending: Option<RenderRequest>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Admission {
    Start(RenderRequest),
    Queued { replaced: Option<RenderGeneration> },
}

impl RenderAdmission {
    pub fn submit(&mut self, request: RenderRequest) -> Admission {
        if self.in_flight.is_none() {
            self.in_flight = Some(request);
            Admission::Start(request)
        } else {
            let replaced = self.pending.replace(request).map(|old| old.generation);
            Admission::Queued { replaced }
        }
    }

    /// Marks the active request complete. A stale or duplicate completion has no scheduling
    /// effect. Returns the newest queued request, if any, which becomes active atomically.
    pub fn complete(&mut self, generation: RenderGeneration) -> Option<RenderRequest> {
        if self.in_flight.map(|request| request.generation) != Some(generation) {
            return None;
        }
        self.in_flight = self.pending.take();
        self.in_flight
    }

    pub fn in_flight(&self) -> Option<RenderRequest> {
        self.in_flight
    }

    pub fn pending(&self) -> Option<RenderRequest> {
        self.pending
    }
}

/// Identity of one out-of-core brick. A pyramid level alone is deliberately insufficient: a
/// timepoint and channel may have entirely different content at the same brick coordinate.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BrickKey {
    pub timepoint: u32,
    pub channel: u32,
    pub level: u32,
    pub xyz: [u32; 3],
}

/// A portable location in the first wgpu brick-pool representation. The top bits select one of
/// four statically-bound storage pages; the low twenty bits are a word offset within that page.
/// It is intentionally not a Vulkan device address.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BrickLocation(u32);

impl BrickLocation {
    const OFFSET_BITS: u32 = 20;
    const OFFSET_MASK: u32 = (1 << Self::OFFSET_BITS) - 1;
    const MAX_PAGES: u32 = 4;

    fn new(page: u32, offset_words: u32) -> Self {
        debug_assert!(page < Self::MAX_PAGES);
        debug_assert!(offset_words <= Self::OFFSET_MASK);
        Self((page << Self::OFFSET_BITS) | offset_words)
    }

    pub fn packed(self) -> u32 {
        self.0
    }

    pub fn page(self) -> u32 {
        self.0 >> Self::OFFSET_BITS
    }

    pub fn offset_words(self) -> u32 {
        self.0 & Self::OFFSET_MASK
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ResidentBrick {
    slot: u32,
    last_used: u64,
    pinned: bool,
}

/// Fixed-capacity, portable residency state for a statically-bound wgpu page pool.
///
/// The GPU backend owns the byte upload itself. This type owns only the deterministic mapping and
/// eviction decision, so it can be shared by native and browser callers and tested without an
/// adapter. Coarse bricks may be pinned, guaranteeing that a refinement miss always has a
/// resident fallback.
#[derive(Clone, Debug)]
pub struct BrickPool {
    page_count: u32,
    slots_per_page: u32,
    words_per_slot: u32,
    slots: Vec<Option<BrickKey>>,
    resident: HashMap<BrickKey, ResidentBrick>,
    clock: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrickResidency {
    Hit(BrickLocation),
    Loaded {
        location: BrickLocation,
        evicted: Option<BrickKey>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrickPoolError {
    InvalidPageCount(u32),
    InvalidSlotsPerPage(u32),
    InvalidWordsPerSlot(u32),
    SlotFootprintDoesNotFitPortableLocation {
        slots_per_page: u32,
        words_per_slot: u32,
    },
    AllSlotsPinned,
}

impl fmt::Display for BrickPoolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPageCount(count) => write!(
                formatter,
                "portable brick pool requires 1..={} statically-bound pages, got {count}",
                BrickLocation::MAX_PAGES
            ),
            Self::InvalidSlotsPerPage(count) => {
                write!(
                    formatter,
                    "brick pool requires at least one slot per page, got {count}"
                )
            }
            Self::InvalidWordsPerSlot(count) => {
                write!(formatter, "brick pool requires at least one word per slot, got {count}")
            }
            Self::SlotFootprintDoesNotFitPortableLocation {
                slots_per_page,
                words_per_slot,
            } => write!(
                formatter,
                "{slots_per_page} slots of {words_per_slot} words exceed the portable 20-bit page offset"
            ),
            Self::AllSlotsPinned => write!(
                formatter,
                "brick pool cannot evict because every slot is pinned"
            ),
        }
    }
}

impl std::error::Error for BrickPoolError {}

impl BrickPool {
    /// Create a pool whose slots occupy one word. This is useful for page-table tests; a real
    /// voxel-brick backend should call [`Self::with_slot_words`] with its brick payload size.
    pub fn new(page_count: u32, slots_per_page: u32) -> Result<Self, BrickPoolError> {
        Self::with_slot_words(page_count, slots_per_page, 1)
    }

    /// Create a pool with a fixed word footprint for every slot. The packed location points to
    /// the first word of a slot, never merely to its ordinal, so it can be supplied directly to
    /// a shader page-table lookup.
    pub fn with_slot_words(
        page_count: u32,
        slots_per_page: u32,
        words_per_slot: u32,
    ) -> Result<Self, BrickPoolError> {
        if !(1..=BrickLocation::MAX_PAGES).contains(&page_count) {
            return Err(BrickPoolError::InvalidPageCount(page_count));
        }
        if slots_per_page == 0 {
            return Err(BrickPoolError::InvalidSlotsPerPage(slots_per_page));
        }
        if words_per_slot == 0 {
            return Err(BrickPoolError::InvalidWordsPerSlot(words_per_slot));
        }
        if u64::from(slots_per_page) * u64::from(words_per_slot)
            > u64::from(BrickLocation::OFFSET_MASK) + 1
        {
            return Err(BrickPoolError::SlotFootprintDoesNotFitPortableLocation {
                slots_per_page,
                words_per_slot,
            });
        }
        let capacity = page_count.checked_mul(slots_per_page).expect(
            "four portable pages and a 20-bit offset always fit a usize on supported targets",
        );
        Ok(Self {
            page_count,
            slots_per_page,
            words_per_slot,
            slots: vec![None; capacity as usize],
            resident: HashMap::new(),
            clock: 0,
        })
    }

    pub fn capacity(&self) -> u32 {
        self.page_count * self.slots_per_page
    }

    pub fn words_per_slot(&self) -> u32 {
        self.words_per_slot
    }

    pub fn resident_location(&self, key: BrickKey) -> Option<BrickLocation> {
        self.resident
            .get(&key)
            .map(|resident| self.location(resident.slot))
    }

    pub fn is_pinned(&self, key: BrickKey) -> bool {
        self.resident
            .get(&key)
            .is_some_and(|resident| resident.pinned)
    }

    /// Mark a brick used by a draw. This affects LRU order only; it does not invent a missing
    /// brick or modify its pin state.
    pub fn touch(&mut self, key: BrickKey) -> Option<BrickLocation> {
        self.clock = self.clock.wrapping_add(1);
        let slot = self.resident.get_mut(&key).map(|resident| {
            resident.last_used = self.clock;
            resident.slot
        })?;
        Some(self.location(slot))
    }

    pub fn reside(&mut self, key: BrickKey) -> Result<BrickResidency, BrickPoolError> {
        self.reside_inner(key, false)
    }

    /// Install a permanently resident coarse fallback. Re-submitting a normal brick as pinned
    /// upgrades it in place, which lets callers lock the coarsest pyramid after initial loading.
    pub fn reside_pinned(&mut self, key: BrickKey) -> Result<BrickResidency, BrickPoolError> {
        self.reside_inner(key, true)
    }

    fn reside_inner(
        &mut self,
        key: BrickKey,
        pinned: bool,
    ) -> Result<BrickResidency, BrickPoolError> {
        self.clock = self.clock.wrapping_add(1);
        if let Some(resident) = self.resident.get_mut(&key) {
            resident.last_used = self.clock;
            resident.pinned |= pinned;
            let slot = resident.slot;
            return Ok(BrickResidency::Hit(self.location(slot)));
        }

        let (slot, evicted) = if let Some(slot) = self.slots.iter().position(Option::is_none) {
            (slot as u32, None)
        } else {
            let (evicted_key, evicted) = self
                .resident
                .iter()
                .filter(|(_, resident)| !resident.pinned)
                .min_by_key(|(_, resident)| resident.last_used)
                .map(|(key, resident)| (*key, *resident))
                .ok_or(BrickPoolError::AllSlotsPinned)?;
            self.resident.remove(&evicted_key);
            self.slots[evicted.slot as usize] = None;
            (evicted.slot, Some(evicted_key))
        };
        debug_assert!(self.slots[slot as usize].is_none());
        self.slots[slot as usize] = Some(key);
        self.resident.insert(
            key,
            ResidentBrick {
                slot,
                last_used: self.clock,
                pinned,
            },
        );
        Ok(BrickResidency::Loaded {
            location: self.location(slot),
            evicted,
        })
    }

    fn location(&self, slot: u32) -> BrickLocation {
        BrickLocation::new(
            slot / self.slots_per_page,
            (slot % self.slots_per_page) * self.words_per_slot,
        )
    }
}

/// Backend-neutral geometric packet for annotation compositing. Positions remain in physical
/// `[x, y, z]` coordinates; a Vulkan or wgpu backend is responsible for camera-facing expansion
/// and testing it against [`DepthAttachment::RayDistanceF32`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OverlayPrimitive {
    Point {
        center: PhysicalVec3,
        radius: f32,
        color_srgb: [u8; 3],
    },
    Segment {
        start: PhysicalVec3,
        end: PhysicalVec3,
        radius: f32,
        color_srgb: [u8; 3],
    },
    /// A filled coplanar annotation face. The original boundary segments remain present as an
    /// outline, while a backend can render these triangles as a depth-tested mesh.
    Triangle {
        vertices: [PhysicalVec3; 3],
        color_srgb: [u8; 3],
    },
}

/// One annotation after expansion into draw-ready primitive intent. A renderer may batch packets
/// by colour and primitive kind but must retain `annotation_id` for future picking.
#[derive(Clone, Debug, PartialEq)]
pub struct AnnotationOverlay {
    pub annotation_id: AnnotationId,
    pub primitives: Vec<OverlayPrimitive>,
}

/// One physical annotation position after the active camera has projected it into the frame.
/// `ray_distance` is measured from the same ray origin and in the same physical units as the
/// volume's [`DepthAttachment::RayDistanceF32`] attachment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProjectedAnnotationVertex {
    pub pixel: [f32; 2],
    pub ray_distance: f32,
}

/// Camera-specific projection supplied by either the native or browser renderer. Keeping this
/// as a small trait prevents a backend matrix convention from leaking into persisted scene data.
pub trait AnnotationProjector {
    fn project(&self, position: PhysicalVec3) -> Option<ProjectedAnnotationVertex>;

    /// Convert a local physical sprite/capsule radius to physical frame pixels at `position`.
    /// This keeps anisotropic and perspective cameras from treating a world-unit radius as a
    /// CSS or framebuffer-pixel radius by accident.
    fn project_radius(&self, position: PhysicalVec3, radius: f32) -> Option<f32>;
}

/// Fixed portable GPU record shared with Palace's bounded WGPU annotation pass.
///
/// Its thirteen words are `{kind, nonzero_id, 0x00RRGGBB, radius_bits, ax, ay, ad, bx, by, bd,
/// cx, cy, cd}`, where every float is IEEE-754 bits. `nonzero_id` is the reversible scene ID
/// plus one because zero remains the GPU attachment's no-annotation sentinel. Per-vertex depth
/// permits the GPU to interpolate slanted lines and triangles before comparing them to
/// first-opacity depth.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PortableAnnotationPrimitive {
    pub kind: PortableAnnotationPrimitiveKind,
    pub annotation_id: u32,
    pub color_srgb: [u8; 3],
    pub radius: f32,
    pub vertices: [ProjectedAnnotationVertex; 3],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum PortableAnnotationPrimitiveKind {
    Point = 1,
    Segment = 2,
    Triangle = 3,
}

impl PortableAnnotationPrimitive {
    pub const WORDS: usize = 13;

    pub const fn words(self) -> [u32; Self::WORDS] {
        [
            self.kind as u32,
            self.annotation_id,
            u32::from_be_bytes([
                0,
                self.color_srgb[0],
                self.color_srgb[1],
                self.color_srgb[2],
            ]),
            self.radius.to_bits(),
            self.vertices[0].pixel[0].to_bits(),
            self.vertices[0].pixel[1].to_bits(),
            self.vertices[0].ray_distance.to_bits(),
            self.vertices[1].pixel[0].to_bits(),
            self.vertices[1].pixel[1].to_bits(),
            self.vertices[1].ray_distance.to_bits(),
            self.vertices[2].pixel[0].to_bits(),
            self.vertices[2].pixel[1].to_bits(),
            self.vertices[2].ray_distance.to_bits(),
        ]
    }
}

/// Project expanded physical annotation geometry into the portable Palace/WGPU record layout.
/// Projection is all-or-nothing for invalid IDs and camera data. Finite off-target vertices are
/// retained so the GPU can clip a partially visible line or polygon instead of silently dropping
/// the whole editable annotation.
pub fn project_annotation_overlays<P: AnnotationProjector>(
    overlays: &[AnnotationOverlay],
    target: PhysicalExtent,
    projector: &P,
) -> Result<Vec<PortableAnnotationPrimitive>, AnnotationProjectionError> {
    let mut output = Vec::new();
    for overlay in overlays {
        let annotation_id = overlay
            .annotation_id
            .0
            .checked_add(1)
            .and_then(|id| u32::try_from(id).ok())
            .ok_or(AnnotationProjectionError::InvalidAnnotationId(
                overlay.annotation_id,
            ))?;
        for primitive in &overlay.primitives {
            let (kind, color_srgb, radius, radius_position, positions) = match *primitive {
                OverlayPrimitive::Point {
                    center,
                    radius,
                    color_srgb,
                } => (
                    PortableAnnotationPrimitiveKind::Point,
                    color_srgb,
                    radius,
                    center,
                    [center; 3],
                ),
                OverlayPrimitive::Segment {
                    start,
                    end,
                    radius,
                    color_srgb,
                } => (
                    PortableAnnotationPrimitiveKind::Segment,
                    color_srgb,
                    radius,
                    scale(add(start, end), 0.5),
                    [start, end, end],
                ),
                OverlayPrimitive::Triangle {
                    vertices,
                    color_srgb,
                } => (
                    PortableAnnotationPrimitiveKind::Triangle,
                    color_srgb,
                    0.0,
                    vertices[0],
                    vertices,
                ),
            };
            if !radius.is_finite() || radius < 0.0 {
                return Err(AnnotationProjectionError::InvalidRadius(radius));
            }
            let radius = if radius == 0.0 {
                0.0
            } else {
                projector
                    .project_radius(radius_position, radius)
                    .filter(|value| value.is_finite() && *value >= 0.0)
                    .ok_or(AnnotationProjectionError::InvalidRadius(radius))?
            };
            let vertices = positions.map(|position| {
                let projected = projector
                    .project(position)
                    .ok_or(AnnotationProjectionError::ProjectionUnavailable(position))?;
                validate_projected_vertex(projected, target)
            });
            let vertices = [vertices[0]?, vertices[1]?, vertices[2]?];
            output.push(PortableAnnotationPrimitive {
                kind,
                annotation_id,
                color_srgb,
                radius,
                vertices,
            });
        }
    }
    Ok(output)
}

fn validate_projected_vertex(
    vertex: ProjectedAnnotationVertex,
    _target: PhysicalExtent,
) -> Result<ProjectedAnnotationVertex, AnnotationProjectionError> {
    if !vertex.pixel.iter().all(|value| value.is_finite()) || !vertex.ray_distance.is_finite() {
        return Err(AnnotationProjectionError::NonFiniteVertex(vertex));
    }
    if vertex.ray_distance < 0.0 {
        return Err(AnnotationProjectionError::NegativeRayDistance(
            vertex.ray_distance,
        ));
    }
    Ok(vertex)
}

/// A normalized physical-space camera ray used for annotation picking. Its parameter `distance`
/// is deliberately the same world-space ray distance carried by
/// [`DepthAttachment::RayDistanceF32`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PickRay {
    pub origin: PhysicalVec3,
    pub direction: PhysicalVec3,
}

impl PickRay {
    pub fn new(origin: PhysicalVec3, direction: PhysicalVec3) -> Result<Self, PickError> {
        if origin
            .iter()
            .chain(direction.iter())
            .any(|value| !value.is_finite())
        {
            return Err(PickError::NonFiniteRay);
        }
        let length_squared = dot(direction, direction);
        if length_squared <= f64::EPSILON {
            return Err(PickError::ZeroDirection);
        }
        let length = length_squared.sqrt();
        Ok(Self {
            origin,
            direction: std::array::from_fn(|axis| direction[axis] / length),
        })
    }
}

/// One nearest annotation hit. `distance` is compared directly with the volume's first-opacity
/// ray distance, so a caller can pass that depth as `max_ray_distance` to avoid selecting an
/// occluded overlay.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnnotationPick {
    pub annotation_id: AnnotationId,
    pub distance: f64,
}

/// Pick the nearest expanded annotation primitive on a camera ray.
///
/// Points are physical spheres, segments are physical capsules, and filled polygon packets use
/// a two-sided triangle intersection. Equal-distance hits resolve by stable annotation ID so
/// CPU and future GPU paths cannot flicker between candidates.
pub fn pick_annotation_overlays(
    overlays: &[AnnotationOverlay],
    ray: PickRay,
    max_ray_distance: f64,
) -> Result<Option<AnnotationPick>, PickError> {
    // `+∞` is the render contract's explicit no-volume-hit value and means annotations are not
    // depth-clipped. NaN and negative distances never have a useful occlusion meaning.
    if max_ray_distance.is_nan() || max_ray_distance < 0.0 {
        return Err(PickError::InvalidMaxRayDistance(max_ray_distance));
    }
    let mut nearest = None;
    for overlay in overlays {
        for primitive in &overlay.primitives {
            let distance = match *primitive {
                OverlayPrimitive::Point { center, radius, .. } => {
                    pick_point(ray, center, f64::from(radius))
                }
                OverlayPrimitive::Segment {
                    start, end, radius, ..
                } => pick_segment(ray, start, end, f64::from(radius)),
                OverlayPrimitive::Triangle { vertices, .. } => pick_triangle(ray, vertices),
            };
            let Some(distance) = distance.filter(|distance| *distance <= max_ray_distance) else {
                continue;
            };
            let candidate = AnnotationPick {
                annotation_id: overlay.annotation_id,
                distance,
            };
            if nearest.is_none_or(|current: AnnotationPick| {
                candidate.distance < current.distance
                    || (candidate.distance == current.distance
                        && candidate.annotation_id < current.annotation_id)
            }) {
                nearest = Some(candidate);
            }
        }
    }
    Ok(nearest)
}

/// As [`pick_annotation_overlays`], but accepts a validated renderer depth sample directly.
/// This is the preferred call site for a CPU fallback reading the S8 ray-distance attachment.
pub fn pick_annotation_overlays_at_depth(
    overlays: &[AnnotationOverlay],
    ray: PickRay,
    depth: RayDistance,
) -> Result<Option<AnnotationPick>, PickError> {
    pick_annotation_overlays(overlays, ray, f64::from(depth.value()))
}

fn pick_point(ray: PickRay, center: PhysicalVec3, radius: f64) -> Option<f64> {
    let offset = sub(center, ray.origin);
    let closest = dot(offset, ray.direction).max(0.0);
    let point_on_ray = add(ray.origin, scale(ray.direction, closest));
    (dot(sub(center, point_on_ray), sub(center, point_on_ray)) <= radius * radius)
        .then_some(closest)
}

fn pick_segment(ray: PickRay, start: PhysicalVec3, end: PhysicalVec3, radius: f64) -> Option<f64> {
    let segment = sub(end, start);
    let segment_length_squared = dot(segment, segment);
    if segment_length_squared <= f64::EPSILON {
        return pick_point(ray, start, radius);
    }
    let origin_to_start = sub(ray.origin, start);
    let ray_segment = dot(ray.direction, segment);
    let ray_origin = dot(ray.direction, origin_to_start);
    let segment_origin = dot(segment, origin_to_start);
    let denominator = segment_length_squared - ray_segment * ray_segment;
    let mut along_segment = if denominator.abs() > f64::EPSILON {
        ((ray_segment * ray_origin) - segment_origin) / denominator
    } else {
        0.0
    }
    .clamp(0.0, 1.0);
    let mut along_ray = dot(
        sub(add(start, scale(segment, along_segment)), ray.origin),
        ray.direction,
    )
    .max(0.0);
    along_segment = (dot(
        sub(add(ray.origin, scale(ray.direction, along_ray)), start),
        segment,
    ) / segment_length_squared)
        .clamp(0.0, 1.0);
    along_ray = dot(
        sub(add(start, scale(segment, along_segment)), ray.origin),
        ray.direction,
    )
    .max(0.0);
    let ray_point = add(ray.origin, scale(ray.direction, along_ray));
    let segment_point = add(start, scale(segment, along_segment));
    (dot(sub(ray_point, segment_point), sub(ray_point, segment_point)) <= radius * radius)
        .then_some(along_ray)
}

fn pick_triangle(ray: PickRay, vertices: [PhysicalVec3; 3]) -> Option<f64> {
    const EPSILON: f64 = 1e-9;
    let edge_one = sub(vertices[1], vertices[0]);
    let edge_two = sub(vertices[2], vertices[0]);
    let perpendicular = cross(ray.direction, edge_two);
    let determinant = dot(edge_one, perpendicular);
    if determinant.abs() <= EPSILON {
        return None;
    }
    let inverse = 1.0 / determinant;
    let origin_offset = sub(ray.origin, vertices[0]);
    let u = dot(origin_offset, perpendicular) * inverse;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = cross(origin_offset, edge_one);
    let v = dot(ray.direction, q) * inverse;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let distance = dot(edge_two, q) * inverse;
    (distance >= 0.0).then_some(distance)
}

fn add(left: PhysicalVec3, right: PhysicalVec3) -> PhysicalVec3 {
    std::array::from_fn(|axis| left[axis] + right[axis])
}

fn scale(value: PhysicalVec3, factor: f64) -> PhysicalVec3 {
    std::array::from_fn(|axis| value[axis] * factor)
}

/// Physical radii for point sprites and camera-facing line capsules. The values are deliberately
/// physical rather than pixel widths, avoiding an anisotropy-dependent implicit conversion.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlayStyle {
    pub point_radius: f32,
    pub line_radius: f32,
}

impl OverlayStyle {
    pub fn new(point_radius: f32, line_radius: f32) -> Result<Self, OverlayError> {
        if !point_radius.is_finite() || point_radius <= 0.0 {
            return Err(OverlayError::InvalidPointRadius(point_radius));
        }
        if !line_radius.is_finite() || line_radius <= 0.0 {
            return Err(OverlayError::InvalidLineRadius(line_radius));
        }
        Ok(Self {
            point_radius,
            line_radius,
        })
    }
}

/// Convert persisted annotation geometry into renderer-neutral primitive intent. Coplanar
/// polygons additionally emit an ear-clipped triangle mesh; non-coplanar or degenerate polygons
/// keep their closed boundary only. That makes a bad import visible without inventing a false
/// filled surface.
pub fn annotation_overlay(annotation: &Annotation, style: OverlayStyle) -> AnnotationOverlay {
    let mut primitives = Vec::new();
    if annotation.visible {
        match &annotation.geometry {
            AnnotationGeometry::Point(center) => primitives.push(OverlayPrimitive::Point {
                center: *center,
                radius: style.point_radius,
                color_srgb: annotation.color_srgb,
            }),
            AnnotationGeometry::Polyline(points) => append_segments(
                &mut primitives,
                points,
                false,
                style.line_radius,
                annotation.color_srgb,
            ),
            AnnotationGeometry::Polygon(points) => {
                append_filled_outline(
                    &mut primitives,
                    points,
                    style.line_radius,
                    annotation.color_srgb,
                );
            }
            AnnotationGeometry::Rectangle { center, half_axes } => {
                let points = rectangle_points(*center, *half_axes);
                append_filled_outline(
                    &mut primitives,
                    &points,
                    style.line_radius,
                    annotation.color_srgb,
                );
            }
            AnnotationGeometry::Ellipse { center, radii } => {
                let points = ellipse_points(*center, *radii, 32);
                append_filled_outline(
                    &mut primitives,
                    &points,
                    style.line_radius,
                    annotation.color_srgb,
                );
            }
        }
    }
    AnnotationOverlay {
        annotation_id: annotation.id,
        primitives,
    }
}

fn append_filled_outline(
    output: &mut Vec<OverlayPrimitive>,
    points: &[PhysicalVec3],
    line_radius: f32,
    color_srgb: [u8; 3],
) {
    append_segments(output, points, true, line_radius, color_srgb);
    output.extend(polygon_triangles(points, color_srgb));
}

fn rectangle_points(center: PhysicalVec3, half_axes: [PhysicalVec3; 2]) -> [PhysicalVec3; 4] {
    let [first, second] = half_axes;
    [
        sub(sub(center, first), second),
        add(sub(center, second), first),
        add(add(center, first), second),
        add(sub(center, first), second),
    ]
}

fn ellipse_points(
    center: PhysicalVec3,
    radii: [PhysicalVec3; 2],
    segments: usize,
) -> Vec<PhysicalVec3> {
    (0..segments)
        .map(|index| {
            let angle = std::f64::consts::TAU * index as f64 / segments as f64;
            add(
                center,
                add(scale(radii[0], angle.cos()), scale(radii[1], angle.sin())),
            )
        })
        .collect()
}

/// Triangulate a physical-space polygon after projecting its verified plane onto its most stable
/// coordinate plane. Ear clipping is intentionally used instead of a triangle fan because ROI
/// polygons may be concave. Invalid input returns no fill rather than guessing a surface.
fn polygon_triangles(points: &[PhysicalVec3], color_srgb: [u8; 3]) -> Vec<OverlayPrimitive> {
    const EPSILON: f64 = 1e-9;
    let Some(normal) = polygon_normal(points, EPSILON) else {
        return Vec::new();
    };
    let origin = points[0];
    if points
        .iter()
        .any(|&point| dot(sub(point, origin), normal).abs() > EPSILON)
    {
        return Vec::new();
    }
    let dropped_axis = normal
        .iter()
        .enumerate()
        .max_by(|(_, left), (_, right)| left.abs().total_cmp(&right.abs()))
        .map(|(axis, _)| axis)
        .expect("a nonzero normal has a dominant axis");
    let projected: Vec<[f64; 2]> = points
        .iter()
        .copied()
        .map(|point| project(point, dropped_axis))
        .collect();
    let signed_area = polygon_area(&projected);
    if signed_area.abs() <= EPSILON {
        return Vec::new();
    }
    let winding = signed_area.signum();
    let mut remaining: Vec<usize> = (0..points.len()).collect();
    let mut result = Vec::with_capacity(points.len().saturating_sub(2));
    while remaining.len() > 3 {
        let mut clipped = false;
        for position in 0..remaining.len() {
            let previous = remaining[(position + remaining.len() - 1) % remaining.len()];
            let current = remaining[position];
            let next = remaining[(position + 1) % remaining.len()];
            if winding * cross_2d(projected[previous], projected[current], projected[next])
                <= EPSILON
            {
                continue;
            }
            if remaining.iter().copied().any(|candidate| {
                candidate != previous
                    && candidate != current
                    && candidate != next
                    && point_in_triangle(
                        projected[candidate],
                        projected[previous],
                        projected[current],
                        projected[next],
                        winding,
                        EPSILON,
                    )
            }) {
                continue;
            }
            result.push(OverlayPrimitive::Triangle {
                vertices: [points[previous], points[current], points[next]],
                color_srgb,
            });
            remaining.remove(position);
            clipped = true;
            break;
        }
        if !clipped {
            return Vec::new();
        }
    }
    result.push(OverlayPrimitive::Triangle {
        vertices: [
            points[remaining[0]],
            points[remaining[1]],
            points[remaining[2]],
        ],
        color_srgb,
    });
    result
}

fn polygon_normal(points: &[PhysicalVec3], epsilon: f64) -> Option<PhysicalVec3> {
    let origin = *points.first()?;
    for middle in points.iter().skip(1) {
        for end in points.iter().skip(2) {
            let normal = cross(sub(*middle, origin), sub(*end, origin));
            if dot(normal, normal) > epsilon * epsilon {
                return Some(normal);
            }
        }
    }
    None
}

fn sub(left: PhysicalVec3, right: PhysicalVec3) -> PhysicalVec3 {
    std::array::from_fn(|axis| left[axis] - right[axis])
}

fn dot(left: PhysicalVec3, right: PhysicalVec3) -> f64 {
    left.iter()
        .zip(right)
        .map(|(left, right)| left * right)
        .sum()
}

fn cross(left: PhysicalVec3, right: PhysicalVec3) -> PhysicalVec3 {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

fn project(point: PhysicalVec3, dropped_axis: usize) -> [f64; 2] {
    match dropped_axis {
        0 => [point[1], point[2]],
        1 => [point[0], point[2]],
        2 => [point[0], point[1]],
        _ => unreachable!("a three-dimensional normal has only three axes"),
    }
}

fn polygon_area(points: &[[f64; 2]]) -> f64 {
    points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .take(points.len())
        .map(|(left, right)| left[0] * right[1] - left[1] * right[0])
        .sum::<f64>()
        * 0.5
}

fn cross_2d(origin: [f64; 2], middle: [f64; 2], end: [f64; 2]) -> f64 {
    (middle[0] - origin[0]) * (end[1] - origin[1]) - (middle[1] - origin[1]) * (end[0] - origin[0])
}

fn point_in_triangle(
    point: [f64; 2],
    first: [f64; 2],
    second: [f64; 2],
    third: [f64; 2],
    winding: f64,
    epsilon: f64,
) -> bool {
    winding * cross_2d(first, second, point) >= -epsilon
        && winding * cross_2d(second, third, point) >= -epsilon
        && winding * cross_2d(third, first, point) >= -epsilon
}

fn append_segments(
    output: &mut Vec<OverlayPrimitive>,
    points: &[PhysicalVec3],
    closed: bool,
    radius: f32,
    color_srgb: [u8; 3],
) {
    for pair in points.windows(2) {
        output.push(OverlayPrimitive::Segment {
            start: pair[0],
            end: pair[1],
            radius,
            color_srgb,
        });
    }
    if closed {
        if let (Some(&start), Some(&end)) = (points.first(), points.last()) {
            output.push(OverlayPrimitive::Segment {
                start: end,
                end: start,
                radius,
                color_srgb,
            });
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OverlayError {
    InvalidLineRadius(f32),
    InvalidPointRadius(f32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AnnotationProjectionError {
    InvalidAnnotationId(AnnotationId),
    InvalidRadius(f32),
    ProjectionUnavailable(PhysicalVec3),
    NonFiniteVertex(ProjectedAnnotationVertex),
    NegativeRayDistance(f32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PickError {
    InvalidMaxRayDistance(f64),
    NonFiniteRay,
    ZeroDirection,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RayDistanceError {
    Invalid(f32),
}

impl fmt::Display for RayDistanceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(value) => write!(
                formatter,
                "ray distance must be non-negative finite or +infinity, got {value}"
            ),
        }
    }
}

impl std::error::Error for RayDistanceError {}

impl fmt::Display for PickError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMaxRayDistance(distance) => {
                write!(formatter, "invalid maximum ray distance: {distance}")
            }
            Self::NonFiniteRay => write!(formatter, "annotation pick ray must be finite"),
            Self::ZeroDirection => {
                write!(formatter, "annotation pick ray direction must be non-zero")
            }
        }
    }
}

impl std::error::Error for PickError {}

impl fmt::Display for OverlayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLineRadius(radius) => {
                write!(
                    formatter,
                    "annotation line radius must be finite and positive, got {radius}"
                )
            }
            Self::InvalidPointRadius(radius) => {
                write!(
                    formatter,
                    "annotation point radius must be finite and positive, got {radius}"
                )
            }
        }
    }
}

impl std::error::Error for OverlayError {}

impl fmt::Display for AnnotationProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidAnnotationId(id) => write!(
                formatter,
                "annotation ID {id:?} cannot fit portable GPU record"
            ),
            Self::InvalidRadius(radius) => {
                write!(formatter, "invalid projected annotation radius: {radius}")
            }
            Self::ProjectionUnavailable(position) => write!(
                formatter,
                "camera cannot project annotation position {position:?}"
            ),
            Self::NonFiniteVertex(vertex) => write!(
                formatter,
                "camera produced non-finite annotation vertex {vertex:?}"
            ),
            Self::NegativeRayDistance(distance) => write!(
                formatter,
                "camera produced negative annotation ray distance: {distance}"
            ),
        }
    }
}

impl std::error::Error for AnnotationProjectionError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenderContractError {
    EncodedFloatTarget,
    ZeroExtent { width: u32, height: u32 },
}

impl fmt::Display for RenderContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EncodedFloatTarget => write!(
                formatter,
                "Rgba16Float targets must be linear; encode during a display or stream blit"
            ),
            Self::ZeroExtent { width, height } => {
                write!(
                    formatter,
                    "physical target extent must be non-zero, got {width}×{height}"
                )
            }
        }
    }
}

impl std::error::Error for RenderContractError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_page_submission_is_fixed_ordered_and_bounded() {
        let submission = PortablePageSubmission::from_uploads([
            PortablePageUpload {
                page: 2,
                words: vec![9],
            },
            PortablePageUpload {
                page: 0,
                words: vec![7],
            },
        ])
        .unwrap();
        assert_eq!(submission.pages, [vec![7], vec![], vec![9], vec![]]);
        assert!(matches!(
            PortablePageSubmission::from_uploads([
                PortablePageUpload {
                    page: 0,
                    words: vec![],
                },
                PortablePageUpload {
                    page: 0,
                    words: vec![],
                },
            ]),
            Err(LayerRenderError::DuplicatePortablePage { page: 0 })
        ));
    }

    #[test]
    fn portable_frame_input_requires_contiguous_ordered_descriptor_ranges() {
        let pages = PortablePageSubmission::from_uploads([]).unwrap();
        let descriptor = |page_offset, page_count| NativeLayerDescriptor {
            layer_id: LayerId(9),
            page_offset,
            page_count,
            transform: LayerTransform::IDENTITY,
        };
        assert!(NativePortableFrameInput::new(vec![descriptor(0, 1)], pages.clone()).is_ok());
        assert!(matches!(
            NativePortableFrameInput::new(vec![descriptor(1, 1)], pages),
            Err(LayerRenderError::PortableDescriptorPageRange {
                layer_id: LayerId(9),
                ..
            })
        ));
    }

    #[test]
    fn direct_portable_volume_retains_dimensions_scalar_type_and_exact_word_count() {
        let pages = PortablePageSubmission::from_uploads([PortablePageUpload {
            page: 0,
            words: vec![1; 8],
        }])
        .unwrap();
        let frame = NativePortableFrameInput::new(
            vec![NativeLayerDescriptor {
                layer_id: LayerId(2),
                page_offset: 0,
                page_count: 1,
                transform: LayerTransform::IDENTITY,
            }],
            pages,
        )
        .unwrap();
        let transfer = PortableChannelTransfer {
            color_srgb: [1, 2, 3],
            window_start: 0.0,
            window_end: 16.0,
            opacity: 0.5,
        };
        let input = NativePortableVolumeInput::new(
            frame.clone(),
            [2, 2, 2],
            PortableScalarType::Uint16,
            transfer,
        )
        .unwrap();
        assert_eq!(input.scalar_type, PortableScalarType::Uint16);
        assert!(matches!(
            NativePortableDrawInput::new(
                input.clone(),
                [32, 32],
                PortableCameraControls::new([0, 0], 1.0).unwrap(),
                vec![0; 12],
            ),
            Err(LayerRenderError::PortableAnnotationWords { words: 12 })
        ));
        let draw = NativePortableDrawInput::new(
            input.clone(),
            [32, 32],
            PortableCameraControls::new([37, -19], 1.2).unwrap(),
            vec![0; PortableAnnotationPrimitive::WORDS],
        )
        .unwrap();
        assert_eq!(draw.volume, input);
        assert_eq!(draw.camera.orbit_delta, [37, -19]);
        let rays = vec![
            PortableCameraRay {
                origin_xyz: [0.0, 0.0, -1.0],
                direction_xyz: [0.0, 0.0, 1.0],
            };
            32 * 32
        ];
        assert_eq!(
            NativePortableCameraDrawInput::new(draw.clone(), rays.clone())
                .unwrap()
                .rays,
            rays
        );
        assert!(matches!(
            NativePortableCameraDrawInput::new(draw.clone(), vec![]),
            Err(LayerRenderError::PortableCameraRayCount { .. })
        ));
        let mut invalid_rays = rays;
        invalid_rays[0].direction_xyz = [0.0, 0.0, 0.0];
        assert!(matches!(
            NativePortableCameraDrawInput::new(draw, invalid_rays),
            Err(LayerRenderError::PortableCameraRay)
        ));
        assert!(matches!(
            PortableCameraControls::new([0, 0], 5.0),
            Err(LayerRenderError::PortableDrawCamera { .. })
        ));
        assert!(matches!(
            NativePortableVolumeInput::new(frame, [2, 2, 3], PortableScalarType::Uint16, transfer,),
            Err(LayerRenderError::PortableVolumeWordCount { .. })
        ));
    }

    #[test]
    fn portable_world_ray_keeps_physical_parameterization_across_layer_transforms() {
        let world = PortableWorldRay::new([12.0, 23.0, 40.0], [0.0, 0.0, 1.0]).unwrap();
        let layer = world.to_layer(
            LayerTransform::new([0.5, 0.5, 5.0], [10.0, 20.0, 30.0]).unwrap(),
            [3, 4, 1],
        );
        assert_eq!(layer.origin_xyz, [1.0, 2.0, 1.0]);
        assert_eq!(layer.direction_xyz, [0.0, 0.0, 0.2]);
        assert!(matches!(
            PortableWorldRay::new([0.0; 3], [0.0; 3]),
            Err(LayerRenderError::PortableCameraRay)
        ));
    }

    #[test]
    fn portable_scene_packet_keeps_ordered_layer_transforms_and_absolute_page_ranges() {
        let transfer = PortableChannelTransfer {
            color_srgb: [255, 0, 0],
            window_start: 0.0,
            window_end: 1.0,
            opacity: 1.0,
        };
        let first_transform = LayerTransform::IDENTITY;
        let second_transform = LayerTransform::new([2.0, 3.0, 4.0], [5.0, 6.0, 7.0]).unwrap();
        let frame = NativePortableFrameInput::new(
            vec![
                NativeLayerDescriptor {
                    layer_id: LayerId(1),
                    page_offset: 0,
                    page_count: 1,
                    transform: first_transform,
                },
                NativeLayerDescriptor {
                    layer_id: LayerId(2),
                    page_offset: 1,
                    page_count: 1,
                    transform: second_transform,
                },
            ],
            PortablePageSubmission::from_uploads([
                PortablePageUpload {
                    page: 0,
                    words: vec![1; 8],
                },
                PortablePageUpload {
                    page: 1,
                    words: vec![2; 8],
                },
            ])
            .unwrap(),
        )
        .unwrap();
        let layers = vec![
            PortableSceneLayerInput {
                layer_id: LayerId(1),
                transform: first_transform,
                voxel_origin_xyz: [0, 0, 0],
                dimensions_xyz: [2, 2, 2],
                scalar_type: PortableScalarType::Uint16,
                channels: vec![PortableVolumeChannel {
                    page_offset: 0,
                    page_count: 1,
                    transfer,
                }],
            },
            PortableSceneLayerInput {
                layer_id: LayerId(2),
                transform: second_transform,
                voxel_origin_xyz: [9, 8, 7],
                dimensions_xyz: [2, 2, 2],
                scalar_type: PortableScalarType::Uint16,
                channels: vec![PortableVolumeChannel {
                    page_offset: 1,
                    page_count: 1,
                    transfer,
                }],
            },
        ];
        let scene = NativePortableSceneInput::new(frame, layers.clone()).unwrap();
        assert_eq!(scene.layers, layers);
        assert_eq!(scene.world_ray_step(), 0.5);
        assert_eq!(
            PortableWorldRay::new([7.0, 12.0, 19.0], [0.0, 0.0, 1.0])
                .unwrap()
                .to_layer(scene.layers[1].transform, scene.layers[1].voxel_origin_xyz),
            PortableLayerRay {
                origin_xyz: [-8.0, -6.0, -4.0],
                direction_xyz: [0.0, 0.0, 0.25],
            }
        );
        assert_eq!(
            scene.layers[1].world_ray_interval(
                PortableWorldRay::new([25.0, 33.0, 35.0], [0.0, 0.0, 1.0]).unwrap(),
            ),
            Some((0.0, 8.0))
        );
        assert_eq!(
            scene.layers[1].world_ray_interval(
                PortableWorldRay::new([1.0, 1.0, 1.0], [0.0, 0.0, 1.0]).unwrap(),
            ),
            None
        );
        let draw = NativePortableSceneDrawInput::new(scene, [2, 2], Vec::new()).unwrap();
        let rays = vec![PortableWorldRay::new([0.0; 3], [0.0, 0.0, 1.0]).unwrap(); 4];
        assert_eq!(
            NativePortableSceneCameraDrawInput::new(draw.clone(), rays.clone())
                .unwrap()
                .rays,
            rays
        );
        assert!(matches!(
            NativePortableSceneCameraDrawInput::new(draw, Vec::new()),
            Err(LayerRenderError::PortableCameraRayCount { .. })
        ));
    }

    #[test]
    fn portable_scene_samples_match_ordered_linear_layer_composition() {
        let red = PortableChannelTransfer {
            color_srgb: [255, 0, 0],
            window_start: 0.0,
            window_end: 1.0,
            opacity: 0.5,
        };
        let green = PortableChannelTransfer {
            color_srgb: [0, 255, 0],
            window_start: 0.0,
            window_end: 1.0,
            opacity: 0.5,
        };
        assert_eq!(
            composite_portable_scene_samples(&[(&[red], &[1.0]), (&[green], &[1.0])]).unwrap(),
            [0.25, 0.5, 0.0, 0.75]
        );
    }

    #[test]
    fn direct_portable_volume_keeps_channel_page_ranges_and_transfers_explicit() {
        let transfer = |color_srgb| PortableChannelTransfer {
            color_srgb,
            window_start: 0.0,
            window_end: 7.0,
            opacity: 0.5,
        };
        let pages = PortablePageSubmission::from_uploads([
            PortablePageUpload {
                page: 0,
                words: vec![1; 8],
            },
            PortablePageUpload {
                page: 1,
                words: vec![2; 8],
            },
        ])
        .unwrap();
        let frame = NativePortableFrameInput::new(
            vec![NativeLayerDescriptor {
                layer_id: LayerId(3),
                page_offset: 0,
                page_count: 2,
                transform: LayerTransform::IDENTITY,
            }],
            pages,
        )
        .unwrap();
        let channels = vec![
            PortableVolumeChannel {
                page_offset: 0,
                page_count: 1,
                transfer: transfer([255, 0, 0]),
            },
            PortableVolumeChannel {
                page_offset: 1,
                page_count: 1,
                transfer: transfer([0, 255, 0]),
            },
        ];
        let input = NativePortableVolumeInput::new_channels(
            frame.clone(),
            [2, 2, 2],
            PortableScalarType::Uint16,
            channels.clone(),
        )
        .unwrap();
        assert_eq!(input.channels, channels);
        assert!(matches!(
            NativePortableVolumeInput::new_channels(
                frame,
                [2, 2, 2],
                PortableScalarType::Uint16,
                vec![
                    PortableVolumeChannel {
                        page_offset: 1,
                        page_count: 1,
                        transfer: transfer([255, 0, 0]),
                    },
                    PortableVolumeChannel {
                        page_offset: 0,
                        page_count: 1,
                        transfer: transfer([0, 255, 0]),
                    },
                ],
            ),
            Err(LayerRenderError::PortableDirectVolumeRequiresOnePage)
        ));
    }

    #[test]
    fn direct_portable_volume_admits_ordered_multi_page_words_only() {
        let transfer = PortableChannelTransfer {
            color_srgb: [255, 255, 255],
            window_start: 0.0,
            window_end: 3.0,
            opacity: 1.0,
        };
        let descriptor = NativeLayerDescriptor {
            layer_id: LayerId(4),
            page_offset: 0,
            page_count: 2,
            transform: LayerTransform::IDENTITY,
        };
        let page_words = (PortablePageSubmission::PAGE_BYTES / 4) as usize;
        let pages = PortablePageSubmission::from_uploads([
            PortablePageUpload {
                page: 0,
                words: vec![1; page_words],
            },
            PortablePageUpload {
                page: 1,
                words: vec![3],
            },
        ])
        .unwrap();
        let frame = NativePortableFrameInput::new(vec![descriptor.clone()], pages).unwrap();
        let input = NativePortableVolumeInput::new(
            frame,
            [page_words as u32 + 1, 1, 1],
            PortableScalarType::Uint16,
            transfer,
        )
        .unwrap();
        assert_eq!(input.frame.page_submission.pages[0].len(), page_words);
        assert_eq!(input.frame.page_submission.pages[1], vec![3]);

        let trailing = PortablePageSubmission::from_uploads([
            PortablePageUpload {
                page: 0,
                words: vec![1; page_words],
            },
            PortablePageUpload {
                page: 1,
                words: vec![3],
            },
            PortablePageUpload {
                page: 2,
                words: vec![4],
            },
        ])
        .unwrap();
        let frame = NativePortableFrameInput::new(vec![descriptor], trailing).unwrap();
        assert!(matches!(
            NativePortableVolumeInput::new(
                frame,
                [page_words as u32 + 1, 1, 1],
                PortableScalarType::Uint16,
                transfer
            ),
            Err(LayerRenderError::PortableVolumeWordCount { .. })
        ));
    }

    fn request(generation: u64) -> RenderRequest {
        RenderRequest {
            generation: RenderGeneration(generation),
            target: RenderTarget::new(
                PhysicalExtent::new(2_880, 1_800).unwrap(),
                ColorFormat::Rgba16Float,
                ColorEncoding::Linear,
                DepthAttachment::RayDistanceF32,
            )
            .unwrap(),
        }
    }

    #[test]
    fn target_contract_is_physical_linear_and_depth_aware() {
        let frame = FrameDescriptor {
            generation: RenderGeneration(3),
            target: request(3).target,
            progress: FrameProgress::Refining { pass: 2 },
        };

        assert_eq!(
            frame.target.extent,
            PhysicalExtent::new(2_880, 1_800).unwrap()
        );
        assert_eq!(frame.target.depth, DepthAttachment::RayDistanceF32);
        assert!(RenderTarget::new(
            frame.target.extent,
            ColorFormat::Rgba16Float,
            ColorEncoding::Srgb,
            DepthAttachment::None,
        )
        .is_err());
    }

    #[test]
    fn admission_keeps_only_the_latest_queued_camera_request() {
        let mut admission = RenderAdmission::default();
        assert_eq!(admission.submit(request(1)), Admission::Start(request(1)));
        assert_eq!(
            admission.submit(request(2)),
            Admission::Queued { replaced: None }
        );
        assert_eq!(
            admission.submit(request(3)),
            Admission::Queued {
                replaced: Some(RenderGeneration(2))
            }
        );

        assert_eq!(admission.complete(RenderGeneration(1)), Some(request(3)));
        assert_eq!(admission.in_flight(), Some(request(3)));
        assert_eq!(admission.complete(RenderGeneration(2)), None);
        assert_eq!(admission.complete(RenderGeneration(3)), None);
    }

    fn brick(timepoint: u32, channel: u32, level: u32, xyz: [u32; 3]) -> BrickKey {
        BrickKey {
            timepoint,
            channel,
            level,
            xyz,
        }
    }

    #[test]
    fn portable_pool_packs_static_page_locations_and_keeps_full_cache_keys() {
        let mut pool = BrickPool::with_slot_words(2, 2, 8).unwrap();
        let a = brick(0, 0, 1, [0, 0, 0]);
        let b = brick(1, 0, 1, [0, 0, 0]);
        let c = brick(0, 1, 1, [0, 0, 0]);

        assert_eq!(pool.capacity(), 4);
        assert_eq!(pool.words_per_slot(), 8);
        assert_eq!(
            pool.reside(a).unwrap(),
            BrickResidency::Loaded {
                location: BrickLocation::new(0, 0),
                evicted: None,
            }
        );
        assert_eq!(
            pool.reside(b).unwrap(),
            BrickResidency::Loaded {
                location: BrickLocation::new(0, 8),
                evicted: None,
            }
        );
        assert_eq!(
            pool.reside(c).unwrap(),
            BrickResidency::Loaded {
                location: BrickLocation::new(1, 0),
                evicted: None,
            }
        );
        assert_ne!(pool.resident_location(a), pool.resident_location(b));
        assert_eq!(pool.resident_location(b).unwrap().offset_words(), 8);
        assert_eq!(pool.resident_location(c).unwrap().packed(), 1 << 20);
    }

    #[test]
    fn lru_eviction_preserves_a_pinned_coarse_fallback() {
        let mut pool = BrickPool::new(1, 3).unwrap();
        let coarse = brick(0, 0, 4, [0, 0, 0]);
        let old = brick(0, 0, 0, [0, 0, 0]);
        let recent = brick(0, 0, 0, [1, 0, 0]);
        let incoming = brick(0, 0, 0, [2, 0, 0]);

        pool.reside_pinned(coarse).unwrap();
        pool.reside(old).unwrap();
        pool.reside(recent).unwrap();
        pool.touch(recent);
        assert_eq!(
            pool.reside(incoming).unwrap(),
            BrickResidency::Loaded {
                location: BrickLocation::new(0, 1),
                evicted: Some(old),
            }
        );
        assert!(pool.resident_location(coarse).is_some());
        assert!(pool.is_pinned(coarse));
        assert!(pool.resident_location(old).is_none());
        assert!(pool.resident_location(recent).is_some());
    }

    #[test]
    fn pool_rejects_nonportable_shapes_and_all_pinned_eviction() {
        assert!(matches!(
            BrickPool::new(0, 1),
            Err(BrickPoolError::InvalidPageCount(0))
        ));
        assert!(matches!(
            BrickPool::new(5, 1),
            Err(BrickPoolError::InvalidPageCount(5))
        ));
        assert!(matches!(
            BrickPool::new(1, 0),
            Err(BrickPoolError::InvalidSlotsPerPage(0))
        ));
        assert!(matches!(
            BrickPool::with_slot_words(1, 1, 0),
            Err(BrickPoolError::InvalidWordsPerSlot(0))
        ));
        assert!(matches!(
            BrickPool::with_slot_words(1, 2, BrickLocation::OFFSET_MASK),
            Err(BrickPoolError::SlotFootprintDoesNotFitPortableLocation { .. })
        ));

        let mut pool = BrickPool::new(1, 1).unwrap();
        pool.reside_pinned(brick(0, 0, 4, [0, 0, 0])).unwrap();
        assert_eq!(
            pool.reside(brick(0, 0, 0, [0, 0, 0])),
            Err(BrickPoolError::AllSlotsPinned)
        );
    }

    #[test]
    fn portable_annotation_projection_preserves_style_and_per_vertex_depth() {
        struct TestProjector;
        impl AnnotationProjector for TestProjector {
            fn project(&self, position: PhysicalVec3) -> Option<ProjectedAnnotationVertex> {
                Some(ProjectedAnnotationVertex {
                    pixel: [position[0] as f32, position[1] as f32],
                    ray_distance: position[2] as f32,
                })
            }

            fn project_radius(&self, _: PhysicalVec3, radius: f32) -> Option<f32> {
                Some(radius * 2.0)
            }
        }
        let overlay = AnnotationOverlay {
            annotation_id: AnnotationId(9),
            primitives: vec![
                OverlayPrimitive::Segment {
                    start: [1.0, 2.0, 3.0],
                    end: [4.0, 5.0, 6.0],
                    radius: 0.25,
                    color_srgb: [7, 8, 9],
                },
                OverlayPrimitive::Triangle {
                    vertices: [[2.0, 3.0, 1.0], [5.0, 3.0, 2.0], [3.0, 7.0, 4.0]],
                    color_srgb: [10, 11, 12],
                },
            ],
        };
        let projected = project_annotation_overlays(
            &[overlay],
            PhysicalExtent::new(16, 16).unwrap(),
            &TestProjector,
        )
        .unwrap();
        assert_eq!(projected.len(), 2);
        assert_eq!(projected[0].kind, PortableAnnotationPrimitiveKind::Segment);
        assert_eq!(projected[0].color_srgb, [7, 8, 9]);
        assert_eq!(projected[0].radius, 0.5);
        assert_eq!(projected[0].vertices[1].ray_distance, 6.0);
        assert_eq!(projected[1].kind, PortableAnnotationPrimitiveKind::Triangle);
        assert_eq!(projected[1].vertices[2].pixel, [3.0, 7.0]);
        assert_eq!(projected[1].vertices[2].ray_distance, 4.0);
        assert_eq!(
            projected[0].words()[..4],
            [2, 10, 0x0007_0809, 0.5_f32.to_bits()]
        );
    }

    #[test]
    fn portable_annotation_projection_rejects_invalid_camera_output_and_ids() {
        struct BadProjector;
        impl AnnotationProjector for BadProjector {
            fn project(&self, _: PhysicalVec3) -> Option<ProjectedAnnotationVertex> {
                Some(ProjectedAnnotationVertex {
                    pixel: [32.0, 0.0],
                    ray_distance: -1.0,
                })
            }

            fn project_radius(&self, _: PhysicalVec3, radius: f32) -> Option<f32> {
                Some(radius)
            }
        }
        let overlay = AnnotationOverlay {
            annotation_id: AnnotationId(1),
            primitives: vec![OverlayPrimitive::Point {
                center: [0.0; 3],
                radius: 1.0,
                color_srgb: [0; 3],
            }],
        };
        assert!(matches!(
            project_annotation_overlays(
                std::slice::from_ref(&overlay),
                PhysicalExtent::new(16, 16).unwrap(),
                &BadProjector
            ),
            Err(AnnotationProjectionError::NegativeRayDistance(-1.0))
        ));
        let oversized = AnnotationOverlay {
            annotation_id: AnnotationId(u64::from(u32::MAX) + 1),
            primitives: Vec::new(),
        };
        assert!(matches!(
            project_annotation_overlays(
                &[oversized],
                PhysicalExtent::new(16, 16).unwrap(),
                &BadProjector
            ),
            Err(AnnotationProjectionError::InvalidAnnotationId(_))
        ));

        struct OffscreenProjector;
        impl AnnotationProjector for OffscreenProjector {
            fn project(&self, _: PhysicalVec3) -> Option<ProjectedAnnotationVertex> {
                Some(ProjectedAnnotationVertex {
                    pixel: [-4.0, 32.0],
                    ray_distance: 2.0,
                })
            }

            fn project_radius(&self, _: PhysicalVec3, radius: f32) -> Option<f32> {
                Some(radius)
            }
        }
        assert_eq!(
            project_annotation_overlays(
                &[overlay],
                PhysicalExtent::new(16, 16).unwrap(),
                &OffscreenProjector
            )
            .unwrap()[0]
                .vertices[0]
                .pixel,
            [-4.0, 32.0]
        );
    }

    #[test]
    fn physical_annotation_geometry_expands_without_voxel_assumptions() {
        let style = OverlayStyle::new(1.5, 0.25).unwrap();
        let point = Annotation::new(
            AnnotationId(1),
            "cell",
            AnnotationGeometry::Point([1.0, 2.0, 30.0]),
            [10, 20, 30],
        )
        .unwrap();
        assert_eq!(
            annotation_overlay(&point, style).primitives,
            vec![OverlayPrimitive::Point {
                center: [1.0, 2.0, 30.0],
                radius: 1.5,
                color_srgb: [10, 20, 30],
            }]
        );

        let polygon = Annotation::new(
            AnnotationId(2),
            "roi",
            AnnotationGeometry::Polygon(vec![[0.0, 0.0, 4.0], [2.0, 0.0, 4.0], [2.0, 1.0, 4.0]]),
            [255, 0, 0],
        )
        .unwrap();
        assert_eq!(
            annotation_overlay(&polygon, style).primitives,
            vec![
                OverlayPrimitive::Segment {
                    start: [0.0, 0.0, 4.0],
                    end: [2.0, 0.0, 4.0],
                    radius: 0.25,
                    color_srgb: [255, 0, 0],
                },
                OverlayPrimitive::Segment {
                    start: [2.0, 0.0, 4.0],
                    end: [2.0, 1.0, 4.0],
                    radius: 0.25,
                    color_srgb: [255, 0, 0],
                },
                OverlayPrimitive::Segment {
                    start: [2.0, 1.0, 4.0],
                    end: [0.0, 0.0, 4.0],
                    radius: 0.25,
                    color_srgb: [255, 0, 0],
                },
                OverlayPrimitive::Triangle {
                    vertices: [[0.0, 0.0, 4.0], [2.0, 0.0, 4.0], [2.0, 1.0, 4.0]],
                    color_srgb: [255, 0, 0],
                },
            ]
        );
    }

    #[test]
    fn annotation_ray_picker_uses_depth_distance_and_stable_ids() {
        let ray = PickRay::new([0.0, 0.0, 0.0], [0.0, 0.0, 2.0]).unwrap();
        let style = OverlayStyle::new(0.5, 0.25).unwrap();
        let far = Annotation::new(
            AnnotationId(9),
            "far",
            AnnotationGeometry::Point([0.0, 0.0, 8.0]),
            [1, 2, 3],
        )
        .unwrap();
        let near = Annotation::new(
            AnnotationId(4),
            "near",
            AnnotationGeometry::Polyline(vec![[-1.0, 0.0, 3.0], [1.0, 0.0, 3.0]]),
            [4, 5, 6],
        )
        .unwrap();
        let overlays = vec![
            annotation_overlay(&far, style),
            annotation_overlay(&near, style),
        ];
        assert_eq!(
            pick_annotation_overlays(&overlays, ray, f64::INFINITY),
            Ok(Some(AnnotationPick {
                annotation_id: AnnotationId(4),
                distance: 3.0,
            }))
        );
        assert_eq!(pick_annotation_overlays(&overlays, ray, 2.5), Ok(None));
        assert_eq!(
            pick_annotation_overlays_at_depth(&overlays, ray, RayDistance::no_hit()),
            pick_annotation_overlays(&overlays, ray, f64::INFINITY),
        );
        assert_eq!(
            pick_annotation_overlays_at_depth(&overlays, ray, RayDistance::new(2.5).unwrap()),
            Ok(None),
        );
        assert!(RayDistance::no_hit().is_no_hit());
        assert!(!RayDistance::new(2.5).unwrap().is_no_hit());
        assert!(RayDistance::new(f32::NAN).is_err());
        assert!(RayDistance::new(f32::NEG_INFINITY).is_err());
        assert!(RayDistance::new(-0.1).is_err());
        let equal = vec![
            AnnotationOverlay {
                annotation_id: AnnotationId(7),
                primitives: vec![OverlayPrimitive::Point {
                    center: [0.0, 0.0, 5.0],
                    radius: 1.0,
                    color_srgb: [0; 3],
                }],
            },
            AnnotationOverlay {
                annotation_id: AnnotationId(2),
                primitives: vec![OverlayPrimitive::Point {
                    center: [0.0, 0.0, 5.0],
                    radius: 1.0,
                    color_srgb: [0; 3],
                }],
            },
        ];
        assert_eq!(
            pick_annotation_overlays(&equal, ray, f64::INFINITY)
                .unwrap()
                .unwrap()
                .annotation_id,
            AnnotationId(2)
        );
        assert!(PickRay::new([0.0; 3], [0.0; 3]).is_err());
        assert!(pick_annotation_overlays(&equal, ray, -1.0).is_err());
    }

    #[test]
    fn coplanar_concave_polygons_tessellate_but_nonplanar_polygons_remain_outline_only() {
        let style = OverlayStyle::new(1.0, 0.25).unwrap();
        let concave = Annotation::new(
            AnnotationId(4),
            "concave roi",
            AnnotationGeometry::Polygon(vec![
                [0.0, 0.0, 2.0],
                [3.0, 0.0, 2.0],
                [3.0, 3.0, 2.0],
                [1.5, 1.0, 2.0],
                [0.0, 3.0, 2.0],
            ]),
            [3, 4, 5],
        )
        .unwrap();
        let concave_overlay = annotation_overlay(&concave, style);
        assert_eq!(
            concave_overlay
                .primitives
                .iter()
                .filter(|primitive| matches!(primitive, OverlayPrimitive::Triangle { .. }))
                .count(),
            3
        );

        let nonplanar = Annotation::new(
            AnnotationId(5),
            "slanted roi",
            AnnotationGeometry::Polygon(vec![
                [0.0, 0.0, 0.0],
                [2.0, 0.0, 0.0],
                [2.0, 2.0, 1.0],
                [0.0, 2.0, 0.0],
            ]),
            [3, 4, 5],
        )
        .unwrap();
        let nonplanar_overlay = annotation_overlay(&nonplanar, style);
        assert_eq!(nonplanar_overlay.primitives.len(), 4);
        assert!(nonplanar_overlay
            .primitives
            .iter()
            .all(|primitive| !matches!(primitive, OverlayPrimitive::Triangle { .. })));

        let self_intersecting = Annotation::new(
            AnnotationId(6),
            "bow tie roi",
            AnnotationGeometry::Polygon(vec![
                [0.0, 0.0, 2.0],
                [2.0, 2.0, 2.0],
                [0.0, 2.0, 2.0],
                [2.0, 0.0, 2.0],
            ]),
            [3, 4, 5],
        )
        .unwrap();
        assert!(annotation_overlay(&self_intersecting, style)
            .primitives
            .iter()
            .all(|primitive| !matches!(primitive, OverlayPrimitive::Triangle { .. })));
    }

    #[test]
    fn rectangle_and_ellipse_rois_expand_in_physical_space() {
        let style = OverlayStyle::new(1.0, 0.25).unwrap();
        let rectangle = Annotation::new(
            AnnotationId(10),
            "oblique rectangle",
            AnnotationGeometry::Rectangle {
                center: [10.0, 20.0, 30.0],
                half_axes: [[2.0, 0.0, 1.0], [0.0, 3.0, 2.0]],
            },
            [1, 2, 3],
        )
        .unwrap();
        let rectangle = annotation_overlay(&rectangle, style);
        assert_eq!(rectangle.primitives.len(), 6);
        assert!(matches!(
            rectangle.primitives[0],
            OverlayPrimitive::Segment {
                start: [8.0, 17.0, 27.0],
                end: [12.0, 17.0, 29.0],
                ..
            }
        ));
        assert_eq!(
            rectangle
                .primitives
                .iter()
                .filter(|primitive| matches!(primitive, OverlayPrimitive::Triangle { .. }))
                .count(),
            2
        );

        let ellipse = Annotation::new(
            AnnotationId(11),
            "oblique ellipse",
            AnnotationGeometry::Ellipse {
                center: [10.0, 20.0, 30.0],
                radii: [[2.0, 0.0, 1.0], [0.0, 3.0, 2.0]],
            },
            [1, 2, 3],
        )
        .unwrap();
        let ellipse = annotation_overlay(&ellipse, style);
        assert_eq!(ellipse.primitives.len(), 62);
        assert!(matches!(
            ellipse.primitives[0],
            OverlayPrimitive::Segment {
                start: [12.0, 20.0, 31.0],
                ..
            }
        ));
        assert_eq!(
            ellipse
                .primitives
                .iter()
                .filter(|primitive| matches!(primitive, OverlayPrimitive::Triangle { .. }))
                .count(),
            30
        );
    }

    #[test]
    fn invisible_annotations_do_not_emit_geometry_and_style_is_validated() {
        let mut annotation = Annotation::new(
            AnnotationId(3),
            "hidden line",
            AnnotationGeometry::Polyline(vec![[0.0, 0.0, 0.0], [1.0, 1.0, 1.0]]),
            [1, 2, 3],
        )
        .unwrap();
        annotation.visible = false;
        assert!(
            annotation_overlay(&annotation, OverlayStyle::new(1.0, 1.0).unwrap())
                .primitives
                .is_empty()
        );
        assert!(matches!(
            OverlayStyle::new(0.0, 1.0),
            Err(OverlayError::InvalidPointRadius(0.0))
        ));
        assert!(matches!(
            OverlayStyle::new(1.0, f32::NAN),
            Err(OverlayError::InvalidLineRadius(value)) if value.is_nan()
        ));
    }

    #[test]
    fn additive_channels_apply_window_opacity_and_linear_srgb_colour() {
        let red = ChannelState::new(
            true,
            [255, 0, 0],
            newvolim_scene::ChannelWindow::new(10.0, 20.0).unwrap(),
            0.5,
        )
        .unwrap();
        let disabled = ChannelState::new(
            false,
            [0, 255, 0],
            newvolim_scene::ChannelWindow::new(0.0, 1.0).unwrap(),
            1.0,
        )
        .unwrap();
        let rgba = composite_additive_channels(&[(red, 15.0), (disabled, 1.0)]);
        assert_eq!(rgba, [0.25, 0.0, 0.0, 0.25]);
        assert_eq!(
            composite_additive_channels(&[(
                ChannelState::new(
                    true,
                    [255, 255, 255],
                    newvolim_scene::ChannelWindow::new(4.0, 4.0).unwrap(),
                    1.0,
                )
                .unwrap(),
                3.0,
            )]),
            [0.0; 4]
        );
        let saturated = composite_additive_channels(&[
            (
                ChannelState::new(
                    true,
                    [255, 0, 0],
                    newvolim_scene::ChannelWindow::new(0.0, 1.0).unwrap(),
                    0.75,
                )
                .unwrap(),
                1.0,
            ),
            (
                ChannelState::new(
                    true,
                    [0, 255, 0],
                    newvolim_scene::ChannelWindow::new(0.0, 1.0).unwrap(),
                    0.75,
                )
                .unwrap(),
                1.0,
            ),
        ]);
        assert_eq!(saturated, [0.75, 0.75, 0.0, 1.0]);
    }

    #[test]
    fn ordered_layers_draw_later_linear_premultiplied_content_over_earlier_layers() {
        let red = ChannelState::new(
            true,
            [255, 0, 0],
            newvolim_scene::ChannelWindow::new(0.0, 1.0).unwrap(),
            0.5,
        )
        .unwrap();
        let green = ChannelState::new(
            true,
            [0, 255, 0],
            newvolim_scene::ChannelWindow::new(0.0, 1.0).unwrap(),
            0.5,
        )
        .unwrap();
        assert_eq!(
            composite_ordered_layers(&[
                (std::slice::from_ref(&red), &[1.0]),
                (std::slice::from_ref(&green), &[1.0]),
            ])
            .unwrap(),
            [0.25, 0.5, 0.0, 0.75]
        );
        assert!(matches!(
            composite_ordered_layers(&[(std::slice::from_ref(&red), &[])]),
            Err(LayerCompositeError::ChannelSampleCount { layer_index: 0, .. })
        ));
    }

    fn channel(enabled: bool, color_srgb: [u8; 3]) -> ChannelState {
        ChannelState::new(
            enabled,
            color_srgb,
            newvolim_scene::ChannelWindow::new(0.0, 100.0).unwrap(),
            0.75,
        )
        .unwrap()
    }

    #[test]
    fn layer_render_plan_preserves_image_order_transform_and_source_channel_indices() {
        use newvolim_scene::{LabelPalette, Layer, LayerId, LayerTransform, Scene};

        let first_transform = LayerTransform::new([0.5, 0.5, 3.0], [1.0, 2.0, 3.0]).unwrap();
        let mut scene = Scene::default();
        scene
            .insert_layer(Layer::image(
                LayerId(7),
                "first",
                first_transform,
                vec![channel(true, [255, 0, 0]), channel(false, [0, 255, 0])],
            ))
            .unwrap();
        scene
            .insert_layer(Layer::labels(
                LayerId(8),
                "segmentation",
                LayerTransform::IDENTITY,
                LabelPalette::default(),
            ))
            .unwrap();
        let mut hidden = Layer::image(
            LayerId(9),
            "hidden",
            LayerTransform::IDENTITY,
            vec![channel(true, [0, 0, 255])],
        );
        hidden.visible = false;
        scene.insert_layer(hidden).unwrap();
        scene
            .insert_layer(Layer::image(
                LayerId(10),
                "second",
                LayerTransform::IDENTITY,
                vec![
                    channel(false, [255, 255, 255]),
                    channel(true, [3, 4, 5]),
                    channel(true, [6, 7, 8]),
                ],
            ))
            .unwrap();

        let plan = LayerRenderPlan::from_scene(&scene, LayerRenderLimits::new(2, 2)).unwrap();
        assert_eq!(plan.image_layers.len(), 2);
        assert_eq!(plan.image_layers[0].layer_id, LayerId(7));
        assert_eq!(plan.image_layers[0].transform, first_transform);
        assert_eq!(plan.image_layers[0].channels[0].source_index, 0);
        assert_eq!(plan.image_layers[1].layer_id, LayerId(10));
        assert_eq!(
            plan.image_layers[1]
                .channels
                .iter()
                .map(|channel| channel.source_index)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn layer_render_plan_rejects_capacity_overflow_without_dropping_scene_content() {
        use newvolim_scene::{Layer, LayerId, LayerTransform, Scene};

        let mut scene = Scene::default();
        scene
            .insert_layer(Layer::image(
                LayerId(1),
                "a",
                LayerTransform::IDENTITY,
                vec![channel(true, [1, 2, 3]), channel(true, [4, 5, 6])],
            ))
            .unwrap();
        assert_eq!(
            LayerRenderPlan::from_scene(&scene, LayerRenderLimits::new(1, 1)),
            Err(LayerRenderError::TooManyChannels {
                layer_id: LayerId(1),
                requested: 2,
                capacity: 1,
            })
        );

        scene
            .insert_layer(Layer::image(
                LayerId(2),
                "b",
                LayerTransform::IDENTITY,
                vec![channel(true, [7, 8, 9])],
            ))
            .unwrap();
        assert_eq!(
            LayerRenderPlan::from_scene(&scene, LayerRenderLimits::new(1, 2)),
            Err(LayerRenderError::TooManyImageLayers {
                requested: 2,
                capacity: 1,
            })
        );
        assert!(matches!(
            LayerRenderPlan::from_scene(&scene, LayerRenderLimits::new(0, 2)),
            Err(LayerRenderError::ZeroImageLayerCapacity)
        ));
        assert!(matches!(
            LayerRenderPlan::from_scene(&scene, LayerRenderLimits::new(2, 0)),
            Err(LayerRenderError::ZeroChannelCapacity)
        ));
    }

    #[test]
    fn native_descriptors_allocate_static_pages_in_scene_order() {
        use newvolim_scene::{Layer, LayerId, LayerTransform, Scene};
        let mut scene = Scene::default();
        for (id, count) in [(1, 1), (2, 2)] {
            scene
                .insert_layer(Layer::image(
                    LayerId(id),
                    "layer",
                    LayerTransform::IDENTITY,
                    (0..count).map(|_| channel(true, [1, 2, 3])).collect(),
                ))
                .unwrap();
        }
        let descriptors = native_layer_descriptors(
            &LayerRenderPlan::from_scene(&scene, LayerRenderLimits::new(4, 4)).unwrap(),
        )
        .unwrap();
        assert_eq!(descriptors[0].page_offset, 0);
        assert_eq!(descriptors[1].page_offset, 1);
        assert_eq!(descriptors[1].page_count, 2);
    }
}
