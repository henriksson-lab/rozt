//! The GPU- and IO-independent vocabulary shared by newvolim components.
//!
//! Coordinates are always ordered `[x, y, z]` in physical space.  In particular, `z` is not
//! assumed to have the same scale as `x` and `y`.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Stable identifier for a layer within one viewing session.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct LayerId(pub u64);

/// A physical coordinate or physical per-axis extent, in `[x, y, z]` order.
pub type PhysicalVec3 = [f64; 3];

/// Stable identifier for an annotation within a session.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct AnnotationId(pub u64);

/// Backend-neutral geometry. Coordinates are physical `[x, y, z]`, so an overlay never silently
/// treats anisotropic voxels as cubic.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum AnnotationGeometry {
    Point(PhysicalVec3),
    Polyline(Vec<PhysicalVec3>),
    Polygon(Vec<PhysicalVec3>),
    /// A planar rectangle specified in physical space by its centre and two half-edge vectors.
    /// The vectors need not be axis-aligned, which keeps oblique ROIs correct for anisotropic
    /// volumes.
    Rectangle {
        center: PhysicalVec3,
        half_axes: [PhysicalVec3; 2],
    },
    /// A planar ellipse specified in physical space by its centre and two radius vectors.
    Ellipse {
        center: PhysicalVec3,
        radii: [PhysicalVec3; 2],
    },
}

impl AnnotationGeometry {
    pub fn validate(&self) -> Result<(), SceneError> {
        let points: &[PhysicalVec3] = match self {
            Self::Point(point) => std::slice::from_ref(point),
            Self::Polyline(points) => {
                if points.len() < 2 {
                    return Err(SceneError::TooFewAnnotationPoints);
                }
                points
            }
            Self::Polygon(points) => {
                if points.len() < 3 {
                    return Err(SceneError::TooFewAnnotationPoints);
                }
                points
            }
            Self::Rectangle { center, half_axes }
            | Self::Ellipse {
                center,
                radii: half_axes,
            } => {
                if half_axes
                    .iter()
                    .any(|axis| dot(*axis, *axis) <= f64::EPSILON)
                    || dot(
                        cross(half_axes[0], half_axes[1]),
                        cross(half_axes[0], half_axes[1]),
                    ) <= f64::EPSILON
                {
                    return Err(SceneError::InvalidAnnotationBasis);
                }
                // Validation below also rejects non-finite components in the centre and axes.
                // Keeping this slice avoids allocating merely to validate a persisted ROI.
                let _ = center;
                half_axes
            }
        };
        if points.iter().flatten().any(|value| !value.is_finite())
            || matches!(self, Self::Rectangle { center, .. } | Self::Ellipse { center, .. }
                if center.iter().any(|value| !value.is_finite()))
        {
            return Err(SceneError::InvalidAnnotationCoordinate);
        }
        Ok(())
    }
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

/// A labelled annotation with style intentionally independent of a renderer backend.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Annotation {
    pub id: AnnotationId,
    pub label: String,
    pub geometry: AnnotationGeometry,
    pub color_srgb: [u8; 3],
    pub visible: bool,
}

impl Annotation {
    pub fn new(
        id: AnnotationId,
        label: impl Into<String>,
        geometry: AnnotationGeometry,
        color_srgb: [u8; 3],
    ) -> Result<Self, SceneError> {
        geometry.validate()?;
        Ok(Self {
            id,
            label: label.into(),
            geometry,
            color_srgb,
            visible: true,
        })
    }
}

/// Per-layer mapping from voxel coordinates to physical coordinates.
///
/// This deliberately retains all three scale factors. Reducing this to a scalar would corrupt
/// camera, picking, and orthogonal-view geometry for the common anisotropic case.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct LayerTransform {
    pub scale: PhysicalVec3,
    pub translation: PhysicalVec3,
}

impl LayerTransform {
    pub const IDENTITY: Self = Self {
        scale: [1.0, 1.0, 1.0],
        translation: [0.0, 0.0, 0.0],
    };

    pub fn new(scale: PhysicalVec3, translation: PhysicalVec3) -> Result<Self, SceneError> {
        if scale
            .iter()
            .any(|value| !value.is_finite() || *value <= 0.0)
        {
            return Err(SceneError::InvalidScale(scale));
        }
        if translation.iter().any(|value| !value.is_finite()) {
            return Err(SceneError::InvalidTranslation(translation));
        }
        Ok(Self { scale, translation })
    }

    /// Maps a voxel coordinate to physical space without assuming cubic voxels.
    pub fn voxel_to_world(self, voxel: PhysicalVec3) -> PhysicalVec3 {
        std::array::from_fn(|axis| self.translation[axis] + voxel[axis] * self.scale[axis])
    }

    /// Maps a physical world coordinate into this layer's local voxel space. Layer scales are
    /// strictly positive by construction, so this is the exact inverse of [`Self::voxel_to_world`]
    /// without introducing a cubic-voxel assumption.
    pub fn world_to_voxel(self, world: PhysicalVec3) -> PhysicalVec3 {
        std::array::from_fn(|axis| (world[axis] - self.translation[axis]) / self.scale[axis])
    }

    /// Maps a physical world-space direction into local voxel units. Translation intentionally
    /// does not participate, which lets camera and pick rays share this transform safely.
    pub fn world_direction_to_voxel(self, direction: PhysicalVec3) -> PhysicalVec3 {
        std::array::from_fn(|axis| direction[axis] / self.scale[axis])
    }
}

/// Display interval in native sample units.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct ChannelWindow {
    pub start: f64,
    pub end: f64,
}

impl ChannelWindow {
    pub fn new(start: f64, end: f64) -> Result<Self, SceneError> {
        if !start.is_finite() || !end.is_finite() || start > end {
            return Err(SceneError::InvalidWindow { start, end });
        }
        Ok(Self { start, end })
    }
}

/// Transfer-function state for one image channel.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ChannelState {
    pub enabled: bool,
    pub color_srgb: [u8; 3],
    pub window: ChannelWindow,
    pub opacity: f32,
}

/// One semantic label's display intent. Label `0` is conventionally background, but the palette
/// makes its opacity explicit rather than relying on a renderer-specific special case.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct LabelPaletteEntry {
    pub value: u32,
    pub color_srgb: [u8; 3],
    pub opacity: f32,
}

impl LabelPaletteEntry {
    pub fn new(value: u32, color_srgb: [u8; 3], opacity: f32) -> Result<Self, SceneError> {
        if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
            return Err(SceneError::InvalidOpacity(opacity));
        }
        Ok(Self {
            value,
            color_srgb,
            opacity,
        })
    }
}

/// A deterministic sparse label palette. Unlisted values are transparent, avoiding accidental
/// pseudo-colouring of segmentation IDs that were not intentionally configured.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct LabelPalette {
    entries: Vec<LabelPaletteEntry>,
}

impl LabelPalette {
    pub fn new(entries: Vec<LabelPaletteEntry>) -> Result<Self, SceneError> {
        let mut seen = std::collections::HashSet::new();
        for entry in &entries {
            if !seen.insert(entry.value) {
                return Err(SceneError::DuplicateLabelValue(entry.value));
            }
        }
        Ok(Self { entries })
    }

    pub fn entries(&self) -> &[LabelPaletteEntry] {
        &self.entries
    }

    pub fn color_for(&self, value: u32) -> Option<LabelPaletteEntry> {
        self.entries
            .iter()
            .copied()
            .find(|entry| entry.value == value)
    }

    /// Renderer-ready straight-alpha sRGB. Sparse palettes intentionally make unknown label
    /// values transparent; this avoids assigning an accidental colour to newly encountered
    /// segmentation IDs while preserving exact configured opacity.
    pub fn rgba_srgb_for(&self, value: u32) -> [u8; 4] {
        let Some(entry) = self.color_for(value) else {
            return [0, 0, 0, 0];
        };
        let alpha = (entry.opacity * 255.0).round() as u8;
        [
            entry.color_srgb[0],
            entry.color_srgb[1],
            entry.color_srgb[2],
            alpha,
        ]
    }
}

impl ChannelState {
    pub fn new(
        enabled: bool,
        color_srgb: [u8; 3],
        window: ChannelWindow,
        opacity: f32,
    ) -> Result<Self, SceneError> {
        if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
            return Err(SceneError::InvalidOpacity(opacity));
        }
        Ok(Self {
            enabled,
            color_srgb,
            window,
            opacity,
        })
    }
}

/// The rendering and interaction behaviour associated with a layer.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum LayerKind {
    Image,
    Labels,
    Annotations,
    Detections,
}

/// One independently transformable item in a session.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Layer {
    pub id: LayerId,
    pub name: String,
    pub kind: LayerKind,
    pub visible: bool,
    pub transform: LayerTransform,
    pub channels: Vec<ChannelState>,
    #[serde(default)]
    pub label_palette: Option<LabelPalette>,
}

impl Layer {
    pub fn image(
        id: LayerId,
        name: impl Into<String>,
        transform: LayerTransform,
        channels: Vec<ChannelState>,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            kind: LayerKind::Image,
            visible: true,
            transform,
            channels,
            label_palette: None,
        }
    }

    pub fn labels(
        id: LayerId,
        name: impl Into<String>,
        transform: LayerTransform,
        palette: LabelPalette,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            kind: LayerKind::Labels,
            visible: true,
            transform,
            channels: Vec::new(),
            label_palette: Some(palette),
        }
    }
}

/// Ordered session state. Later image layers are composited over earlier ones.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Scene {
    layers: Vec<Layer>,
    annotations: Vec<Annotation>,
}

impl Scene {
    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    pub fn layer(&self, id: LayerId) -> Option<&Layer> {
        self.layers.iter().find(|layer| layer.id == id)
    }

    pub fn annotations(&self) -> &[Annotation] {
        &self.annotations
    }

    pub fn annotation(&self, id: AnnotationId) -> Option<&Annotation> {
        self.annotations
            .iter()
            .find(|annotation| annotation.id == id)
    }

    pub fn insert_annotation(&mut self, annotation: Annotation) -> Result<(), SceneError> {
        if self.annotation(annotation.id).is_some() {
            return Err(SceneError::DuplicateAnnotationId(annotation.id));
        }
        self.annotations.push(annotation);
        Ok(())
    }

    pub fn remove_annotation(&mut self, id: AnnotationId) -> Option<Annotation> {
        self.annotations
            .iter()
            .position(|annotation| annotation.id == id)
            .map(|index| self.annotations.remove(index))
    }

    pub fn insert_layer(&mut self, layer: Layer) -> Result<(), SceneError> {
        if self.layer(layer.id).is_some() {
            return Err(SceneError::DuplicateLayerId(layer.id));
        }
        self.layers.push(layer);
        Ok(())
    }

    pub fn remove_layer(&mut self, id: LayerId) -> Option<Layer> {
        self.layers
            .iter()
            .position(|layer| layer.id == id)
            .map(|index| self.layers.remove(index))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum SceneError {
    DuplicateLayerId(LayerId),
    DuplicateLabelValue(u32),
    DuplicateAnnotationId(AnnotationId),
    InvalidOpacity(f32),
    InvalidScale(PhysicalVec3),
    InvalidTranslation(PhysicalVec3),
    InvalidWindow { start: f64, end: f64 },
    InvalidAnnotationCoordinate,
    InvalidAnnotationBasis,
    TooFewAnnotationPoints,
}

impl fmt::Display for SceneError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateLayerId(id) => write!(formatter, "duplicate layer id {}", id.0),
            Self::DuplicateLabelValue(value) => {
                write!(formatter, "duplicate label palette value {value}")
            }
            Self::DuplicateAnnotationId(id) => {
                write!(formatter, "duplicate annotation id {}", id.0)
            }
            Self::InvalidOpacity(opacity) => write!(formatter, "invalid opacity {opacity}"),
            Self::InvalidScale(scale) => write!(formatter, "invalid physical scale {scale:?}"),
            Self::InvalidTranslation(translation) => {
                write!(formatter, "invalid physical translation {translation:?}")
            }
            Self::InvalidWindow { start, end } => {
                write!(formatter, "invalid channel window [{start}, {end}]")
            }
            Self::InvalidAnnotationCoordinate => {
                write!(formatter, "annotation coordinates must be finite")
            }
            Self::InvalidAnnotationBasis => write!(
                formatter,
                "rectangle and ellipse axes must be finite, non-zero, and non-collinear"
            ),
            Self::TooFewAnnotationPoints => {
                write!(formatter, "annotation geometry has too few points")
            }
        }
    }
}

impl std::error::Error for SceneError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anisotropic_transform_preserves_each_axis() {
        let transform = LayerTransform::new([0.5, 0.5, 5.0], [10.0, 20.0, 30.0]).unwrap();

        assert_eq!(
            transform.voxel_to_world([4.0, 6.0, 2.0]),
            [12.0, 23.0, 40.0]
        );
        assert_eq!(
            transform.world_to_voxel([12.0, 23.0, 40.0]),
            [4.0, 6.0, 2.0]
        );
        assert_eq!(
            transform.world_direction_to_voxel([1.0, 2.0, 10.0]),
            [2.0, 4.0, 2.0]
        );
    }

    #[test]
    fn rejects_non_physical_scale_and_invalid_display_state() {
        assert!(LayerTransform::new([1.0, 0.0, 1.0], [0.0; 3]).is_err());
        assert!(ChannelWindow::new(4.0, 3.0).is_err());
        let window = ChannelWindow::new(0.0, 1.0).unwrap();
        assert!(ChannelState::new(true, [255; 3], window, 1.1).is_err());
    }

    #[test]
    fn label_palettes_are_sparse_deterministic_and_attached_only_to_label_layers() {
        let background = LabelPaletteEntry::new(0, [0, 0, 0], 0.0).unwrap();
        let nucleus = LabelPaletteEntry::new(7, [255, 32, 16], 0.75).unwrap();
        let palette = LabelPalette::new(vec![background, nucleus]).unwrap();
        assert_eq!(palette.color_for(7), Some(nucleus));
        assert_eq!(palette.color_for(8), None);
        assert_eq!(palette.rgba_srgb_for(7), [255, 32, 16, 191]);
        assert_eq!(palette.rgba_srgb_for(8), [0, 0, 0, 0]);
        assert!(matches!(
            LabelPalette::new(vec![background, background]),
            Err(SceneError::DuplicateLabelValue(0))
        ));
        let layer = Layer::labels(
            LayerId(8),
            "segmentation",
            LayerTransform::IDENTITY,
            palette,
        );
        assert_eq!(layer.kind, LayerKind::Labels);
        assert!(layer.channels.is_empty());
        assert!(layer.label_palette.is_some());
        assert!(
            Layer::image(LayerId(9), "image", LayerTransform::IDENTITY, Vec::new())
                .label_palette
                .is_none()
        );
    }

    #[test]
    fn layers_are_ordered_and_ids_are_unique() {
        let transform = LayerTransform::IDENTITY;
        let mut scene = Scene::default();
        scene
            .insert_layer(Layer::image(LayerId(7), "kidney", transform, Vec::new()))
            .unwrap();

        assert_eq!(scene.layers()[0].name, "kidney");
        assert!(scene
            .insert_layer(Layer::image(LayerId(7), "duplicate", transform, Vec::new()))
            .is_err());
        assert!(scene.remove_layer(LayerId(7)).is_some());
        assert!(scene.layers().is_empty());
    }

    #[test]
    fn annotations_preserve_physical_coordinates_and_reject_degenerate_geometry() {
        let annotation = Annotation::new(
            AnnotationId(1),
            "vessel",
            AnnotationGeometry::Polyline(vec![[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]),
            [0, 255, 128],
        )
        .unwrap();
        assert_eq!(annotation.id, AnnotationId(1));
        assert!(Annotation::new(
            AnnotationId(2),
            "bad",
            AnnotationGeometry::Polygon(vec![[0.0; 3]; 2]),
            [0; 3]
        )
        .is_err());
    }

    #[test]
    fn planar_roi_shapes_keep_oblique_physical_axes_and_reject_degenerate_bases() {
        let ellipse = Annotation::new(
            AnnotationId(3),
            "oblique nucleus",
            AnnotationGeometry::Ellipse {
                center: [1.0, 2.0, 30.0],
                radii: [[4.0, 0.0, 1.0], [0.0, 3.0, 2.0]],
            },
            [1, 2, 3],
        )
        .unwrap();
        assert_eq!(ellipse.geometry.validate(), Ok(()));
        assert!(matches!(
            Annotation::new(
                AnnotationId(4),
                "line not an ellipse",
                AnnotationGeometry::Ellipse {
                    center: [0.0; 3],
                    radii: [[1.0, 0.0, 0.0], [2.0, 0.0, 0.0]],
                },
                [0; 3],
            ),
            Err(SceneError::InvalidAnnotationBasis)
        ));
    }

    #[test]
    fn scene_owns_annotations_independently_of_image_layers() {
        let mut scene = Scene::default();
        let point = Annotation::new(
            AnnotationId(9),
            "landmark",
            AnnotationGeometry::Point([1.0, 2.0, 3.0]),
            [255, 255, 0],
        )
        .unwrap();
        scene.insert_annotation(point).unwrap();
        assert_eq!(scene.annotations().len(), 1);
        assert!(scene
            .insert_annotation(scene.annotations()[0].clone())
            .is_err());
        assert!(scene.remove_annotation(AnnotationId(9)).is_some());
    }
}
