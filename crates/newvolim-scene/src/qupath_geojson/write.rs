//! Writing QuPath's GeoJSON dialect.
//!
//! The output is what QuPath itself reads: plain RFC 7946 geometry, its two
//! foreign members, and its `properties`. A member is written only when it says
//! something — an absent `isLocked` and a `false` one mean the same thing, and
//! the smaller file is the one a human can read.

use crate::qupath::Annotation;
use anyhow::{Context, Result};
use serde_json::{json, Map, Value};

use super::{DENSE_REGION, STROKE_WIDTH, T_EXTENT, Z_EXTENT};

/// Serialise as a `FeatureCollection`, nesting children under their parents.
pub fn write(annotations: &[Annotation]) -> Result<Vec<u8>> {
    // Roots first, then each child under the parent it names. A parent that is
    // not in the set at all leaves its children at the top level rather than
    // dropping them: losing an annotation because its parent was deleted would
    // be the worst possible reading of a dangling id.
    let known: Vec<u64> = annotations.iter().map(|a| a.id).collect();
    let features: Vec<Value> = annotations
        .iter()
        .filter(|a| !a.parent.is_some_and(|p| known.contains(&p)))
        .map(|a| write_feature(a, annotations))
        .collect::<Result<_>>()?;

    let collection = json!({
        "type": "FeatureCollection",
        "features": features,
    });
    serde_json::to_vec_pretty(&collection).context("serialising the annotations")
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

fn write_feature(annotation: &Annotation, all: &[Annotation]) -> Result<Value> {
    let mut geometry =
        serde_json::to_value(&annotation.geometry).context("serialising a geometry")?;
    if let Some(object) = geometry.as_object_mut() {
        // Foreign members, in QuPath's spelling. `plane` is omitted when it is
        // the default, exactly as QuPath omits it.
        if !annotation.plane.is_default() {
            object.insert(
                "plane".into(),
                json!({
                    "c": annotation.plane.c,
                    "z": annotation.plane.z,
                    "t": annotation.plane.t,
                }),
            );
        }
        if annotation.is_ellipse {
            object.insert("isEllipse".into(), json!(true));
        }
    }

    let mut properties = Map::new();
    properties.insert("objectType".into(), json!(annotation.object_type.as_str()));
    if let Some(name) = &annotation.name {
        properties.insert("name".into(), json!(name));
    }
    if let Some([r, g, b]) = annotation.color {
        properties.insert("color".into(), json!([r, g, b]));
    }
    if let Some(width) = annotation.stroke_width {
        properties.insert(STROKE_WIDTH.into(), json!(width));
    }
    if annotation.dense_region {
        properties.insert(DENSE_REGION.into(), json!(true));
    }
    if !annotation.label.is_empty() {
        let mut classification = Map::new();
        // A derived class round-trips as QuPath writes it: `names` when the
        // label has parts, `name` when it is one.
        let parts: Vec<&str> = annotation.label.split(": ").collect();
        if parts.len() > 1 {
            classification.insert("names".into(), json!(parts));
        } else {
            classification.insert("name".into(), json!(annotation.label));
        }
        if let Some([r, g, b]) = annotation.class_color {
            classification.insert("color".into(), json!([r, g, b]));
        }
        properties.insert("classification".into(), Value::Object(classification));
    }
    if annotation.locked {
        // Written only when true, as QuPath does.
        properties.insert("isLocked".into(), json!(true));
    }
    if annotation.missing {
        properties.insert("isMissing".into(), json!(true));
    }
    if !annotation.measurements.is_empty() {
        properties.insert(
            "measurements".into(),
            Value::Object(
                annotation
                    .measurements
                    .iter()
                    .map(|(k, v)| (k.clone(), json!(v)))
                    .collect(),
            ),
        );
    }
    if !annotation.metadata.is_empty() {
        properties.insert(
            "metadata".into(),
            Value::Object(
                annotation
                    .metadata
                    .iter()
                    .map(|(k, v)| (k.clone(), json!(v)))
                    .collect(),
            ),
        );
    }
    // Ours. Written only when set, so a file of ordinary one-plane shapes is
    // byte-for-byte what QuPath would have written.
    if annotation.z_extent > 0 {
        properties.insert(Z_EXTENT.into(), json!(annotation.z_extent));
    }
    if annotation.t_extent > 0 {
        properties.insert(T_EXTENT.into(), json!(annotation.t_extent));
    }

    let children: Vec<Value> = all
        .iter()
        .filter(|child| child.parent == Some(annotation.id))
        .map(|child| write_feature(child, all))
        .collect::<Result<_>>()?;
    if !children.is_empty() {
        properties.insert("childObjects".into(), Value::Array(children));
    }

    let mut feature = Map::new();
    feature.insert("type".into(), json!("Feature"));
    // QuPath's own UUID when the annotation came from there, so a round trip
    // does not renumber somebody else's objects.
    if let Some(uuid) = &annotation.uuid {
        feature.insert("id".into(), json!(uuid));
    }
    feature.insert("geometry".into(), geometry);
    if let Some(nucleus) = &annotation.nucleus {
        feature.insert(
            "nucleusGeometry".into(),
            serde_json::to_value(nucleus).context("serialising a nucleus")?,
        );
    }
    feature.insert("properties".into(), Value::Object(properties));
    Ok(Value::Object(feature))
}
