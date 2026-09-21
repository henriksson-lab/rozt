//! Dataset-scoped editable QuPath annotation layers. Edits remain in memory until Save.

use std::{collections::HashMap, fs, path::Path, sync::{Arc, Mutex}};

use newvolim_scene::{qupath::{containing_parent, Annotation}, qupath_geojson};
use newvolim_portable::session::LocalSession;
use serde::{Deserialize, Serialize};

mod anndata;

pub(crate) const MAX_ANNOTATION_BYTES: usize = 128 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotationLayer {
    pub id: u64,
    pub name: String,
    pub visible: bool,
    pub annotations: Vec<Annotation>,
    pub dirty: bool,
    pub save_target: String,
    #[serde(skip)]
    next_id: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoiSaveReport { pub target: String, pub flattened: usize }

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoiTableSummary { pub name: String, pub backend: String, pub supported: bool }

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotationSaveReport { pub target: String, pub format: &'static str, pub flattened: usize, pub rows: usize }

impl AnnotationLayer {
    fn new(id: u64, name: String, annotations: Vec<Annotation>) -> Self {
        // The parser numbers rows by position. Remap parent references with the rows.
        let assigned: Vec<u64> = (1..=annotations.len() as u64).collect();
        let annotations = annotations.into_iter().enumerate().map(|(index, mut item)| {
            item.parent = item.parent.and_then(|old| assigned.get(old as usize).copied());
            item.id = assigned[index];
            item
        }).collect();
        let save_target = format!("annotations/{name}");
        Self { id, name, visible: true, annotations, dirty: false, save_target, next_id: assigned.len() as u64 + 1 }
    }

    pub(crate) fn add(&mut self, mut item: Annotation) -> Result<Annotation, String> {
        if self.annotations.len() >= 100_000 { return Err("annotation layer exceeds 100,000 objects".into()); }
        validate_annotation(&item)?;
        item.parent = containing_parent(&self.annotations, &item);
        item.id = self.next_id;
        self.next_id = self.next_id.checked_add(1).ok_or("annotation IDs exhausted")?;
        self.annotations.push(item.clone());
        self.dirty = true;
        Ok(item)
    }

    pub(crate) fn update(&mut self, id: u64, mut item: Annotation) -> Result<Annotation, String> {
        validate_annotation(&item)?;
        let slot = self.annotations.iter_mut().find(|old| old.id == id).ok_or("annotation not found")?;
        item.id = id;
        item.parent = slot.parent;
        *slot = item.clone();
        self.dirty = true;
        Ok(item)
    }

    pub(crate) fn remove(&mut self, id: u64) -> Result<(), String> {
        let parent = self.annotations.iter().find(|item| item.id == id).ok_or("annotation not found")?.parent;
        self.annotations.retain(|item| item.id != id);
        for item in &mut self.annotations {
            if item.parent == Some(id) { item.parent = parent; }
        }
        self.dirty = true;
        Ok(())
    }

    pub(crate) fn detach(&mut self, id: u64) -> Result<(), String> {
        let item = self.annotations.iter_mut().find(|item| item.id == id).ok_or("annotation not found")?;
        item.parent = None;
        self.dirty = true;
        Ok(())
    }

    pub(crate) fn renest(&mut self) {
        let original = self.annotations.clone();
        for item in &mut self.annotations {
            let mut without = original.clone();
            without.retain(|other| other.id != item.id && !is_descendant(&original, other.id, item.id));
            item.parent = containing_parent(&without, item);
        }
        self.dirty = true;
    }

    pub(crate) fn replace_items(&mut self, items: Vec<Annotation>) -> Result<(), String> {
        if items.len() > 100_000 { return Err("annotation layer exceeds 100,000 objects".into()); }
        let mut ids = std::collections::HashSet::new();
        for item in &items {
            validate_annotation(item)?;
            if item.id == 0 || !ids.insert(item.id) { return Err("annotation IDs must be unique and nonzero".into()); }
        }
        if items.iter().any(|item| item.parent.is_some_and(|parent| !ids.contains(&parent))) {
            return Err("annotation parent ID does not exist".into());
        }
        self.next_id = items.iter().map(|item| item.id).max().unwrap_or(0).checked_add(1).ok_or("annotation IDs exhausted")?;
        self.annotations = items;
        self.dirty = true;
        Ok(())
    }
}

fn is_descendant(items: &[Annotation], child: u64, ancestor: u64) -> bool {
    let mut current = Some(child);
    for _ in 0..items.len() {
        current = current.and_then(|id| items.iter().find(|item| item.id == id)).and_then(|item| item.parent);
        if current == Some(ancestor) { return true; }
        if current.is_none() { break; }
    }
    false
}

fn validate_annotation(item: &Annotation) -> Result<(), String> {
    let mut count = 0;
    let mut valid = true;
    item.geometry.for_each_point(&mut |point| {
        count += 1;
        valid &= point.iter().all(|coordinate| coordinate.is_finite());
    });
    if !valid || count == 0 || count > 65_536 { return Err("annotation coordinates must be finite and contain 1–65,536 points".into()); }
    if item.stroke_width.is_some_and(|width| !width.is_finite() || width <= 0.0) {
        return Err("stroke width must be positive and finite".into());
    }
    if item.measurements.values().any(|value| !value.is_finite()) { return Err("measurements must be finite".into()); }
    Ok(())
}

#[derive(Default)]
struct AnnotationDataset { layers: Vec<AnnotationLayer>, next_layer_id: u64 }

#[derive(Clone, Default)]
pub struct AnnotationStore { datasets: Arc<Mutex<HashMap<String, AnnotationDataset>>> }

impl AnnotationStore {
    fn with_dataset<T>(&self, dataset: &str, root: &Path, action: impl FnOnce(&mut AnnotationDataset) -> Result<T, String>) -> Result<T, String> {
        let mut guard = self.datasets.lock().map_err(|_| "annotation store lock poisoned".to_owned())?;
        if !guard.contains_key(dataset) {
            guard.insert(dataset.into(), load_dataset(root)?);
        }
        action(guard.get_mut(dataset).expect("dataset was inserted"))
    }

    pub fn layers(&self, dataset: &str, root: &Path) -> Result<Vec<AnnotationLayer>, String> {
        self.with_dataset(dataset, root, |state| Ok(state.layers.clone()))
    }

    pub fn create(&self, dataset: &str, root: &Path, name: String) -> Result<AnnotationLayer, String> {
        let name = safe_name(&name)?;
        self.with_dataset(dataset, root, |state| {
            if state.layers.iter().any(|layer| layer.name == name) { return Err("annotation layer name already exists".into()); }
            let id = state.next_layer_id;
            state.next_layer_id += 1;
            let layer = AnnotationLayer::new(id, name, vec![]);
            state.layers.push(layer.clone());
            Ok(layer)
        })
    }

    /// Remove a layer from this session. Saved files remain in the dataset and can be opened
    /// again after a restart, matching the viewer's layer removal rather than file deletion.
    pub fn remove_layer(&self, dataset: &str, root: &Path, id: u64) -> Result<(), String> {
        self.with_dataset(dataset, root, |state| {
            let old_len = state.layers.len();
            state.layers.retain(|layer| layer.id != id);
            if state.layers.len() == old_len { Err("annotation layer not found".into()) } else { Ok(()) }
        })
    }

    pub fn edit<T>(&self, dataset: &str, root: &Path, id: u64, action: impl FnOnce(&mut AnnotationLayer) -> Result<T, String>) -> Result<T, String> {
        self.with_dataset(dataset, root, |state| {
            let layer = state.layers.iter_mut().find(|layer| layer.id == id).ok_or("annotation layer not found")?;
            action(layer)
        })
    }

    pub fn save(&self, dataset: &str, root: &Path, id: u64) -> Result<String, String> {
        let name = self.edit(dataset, root, id, |layer| Ok(layer.name.clone()))?;
        self.save_named(dataset, root, id, &name)
    }

    fn save_named(&self, dataset: &str, root: &Path, id: u64, name: &str) -> Result<String, String> {
        let name = safe_name(name)?;
        self.edit(dataset, root, id, |layer| {
            let path = prepare_child_directory(root, "annotations", &name)?;
            let target = path.join("annotations.geojson");
            let bytes = qupath_geojson::write(&layer.annotations).map_err(|error| error.to_string())?;
            let temporary = path.join("annotations.geojson.tmp");
            fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
            fs::rename(&temporary, &target).map_err(|error| error.to_string())?;
            write_group_metadata(root, &name)?;
            layer.dirty = false;
            Ok(target.display().to_string())
        })
    }

    /// Export axis-aligned physical bounding boxes in ngio's CSV ROI table convention. The
    /// report makes every geometry downgrade visible to the caller before it treats this table
    /// as equivalent to the lossless GeoJSON.
    pub fn save_roi_csv(&self, dataset: &str, root: &Path, id: u64, session: &LocalSession) -> Result<RoiSaveReport, String> {
        let name = self.edit(dataset, root, id, |layer| Ok(format!("{}_roi", layer.name)))?;
        self.save_roi_csv_named(dataset, root, id, session, &name)
    }

    fn save_roi_csv_named(&self, dataset: &str, root: &Path, id: u64, session: &LocalSession, name: &str) -> Result<RoiSaveReport, String> {
        let name = safe_name(name)?;
        self.edit(dataset, root, id, |layer| {
            let path = prepare_child_directory(root, "tables", &name)?;
            let mut writer = csv::Writer::from_writer(Vec::new());
            writer.write_record(["FieldIndex", "x_micrometer", "y_micrometer", "z_micrometer",
                "len_x_micrometer", "len_y_micrometer", "len_z_micrometer", "t_second", "len_t_second", "class"])
                .map_err(|error| error.to_string())?;
            let mut flattened = 0;
            for item in &layer.annotations {
                let Some([x0,y0,x1,y1]) = item.bounds() else { continue; };
                if !is_axis_aligned_roi(item) { flattened += 1; }
                let a = session.voxel_point_physical_f64([x0,y0,item.plane.z as f64]).map_err(|error| error.to_string())?;
                let b = session.voxel_point_physical_f64([x1,y1,item.plane.z as f64 + item.z_extent as f64]).map_err(|error| error.to_string())?;
                writer.write_record([
                    format!("roi_{}", item.id), a[0].min(b[0]).to_string(), a[1].min(b[1]).to_string(), a[2].min(b[2]).to_string(),
                    (a[0]-b[0]).abs().to_string(), (a[1]-b[1]).abs().to_string(), (a[2]-b[2]).abs().to_string(),
                    item.plane.t.to_string(), item.t_extent.to_string(), item.label.clone(),
                ]).map_err(|error| error.to_string())?;
            }
            let bytes = writer.into_inner().map_err(|error| error.to_string())?;
            let target = path.join("table.csv");
            let temporary = path.join("table.csv.tmp");
            fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
            fs::rename(&temporary, &target).map_err(|error| error.to_string())?;
            write_roi_group_metadata(root, &name)?;
            Ok(RoiSaveReport { target: target.display().to_string(), flattened })
        })
    }

    /// Save to a named set in this dataset and remember that target for the next Save.
    pub fn save_to(&self, dataset: &str, root: &Path, id: u64, session: &LocalSession, target: &str) -> Result<AnnotationSaveReport, String> {
        let (group, name) = target.split_once('/').ok_or("target must be annotations/<name> or tables/<name>")?;
        let name = safe_name(name)?;
        let rows = self.edit(dataset, root, id, |layer| Ok(layer.annotations.len()))?;
        let (path, format, flattened) = match group {
            "annotations" => (self.save_named(dataset, root, id, &name)?, "geojson", 0),
            "tables" => {
                let report = self.save_roi_csv_named(dataset, root, id, session, &name)?;
                (report.target, "roi_table", report.flattened)
            }
            _ => return Err("target must be annotations/<name> or tables/<name>".into()),
        };
        self.edit(dataset, root, id, |layer| {
            layer.save_target = format!("{group}/{name}");
            layer.dirty = false;
            Ok(())
        })?;
        Ok(AnnotationSaveReport { target: path, format, flattened, rows })
    }

    pub fn replace_from_geojson(&self, dataset: &str, root: &Path, id: u64, bytes: &[u8]) -> Result<AnnotationLayer, String> {
        if bytes.len() > MAX_ANNOTATION_BYTES { return Err("GeoJSON exceeds 128 MiB".into()); }
        let parsed = qupath_geojson::parse(bytes).map_err(|error| error.to_string())?;
        for item in &parsed { validate_annotation(item)?; }
        self.edit(dataset, root, id, |layer| {
            let mut next = AnnotationLayer::new(id, layer.name.clone(), parsed);
            next.dirty = true;
            next.visible = layer.visible;
            next.save_target = layer.save_target.clone();
            *layer = next.clone();
            Ok(next)
        })
    }

    pub fn roi_tables(root: &Path) -> Result<Vec<RoiTableSummary>, String> {
        let dir = root.join("tables");
        if !dir.is_dir() { return Ok(Vec::new()); }
        let mut tables = Vec::new();
        for entry in fs::read_dir(dir).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if safe_name(&name).is_err() || !entry.file_type().map_err(|error| error.to_string())?.is_dir() { continue; }
            let attrs = table_attributes(&entry.path())?;
            if !matches!(attrs.get("type").and_then(serde_json::Value::as_str), Some("roi_table" | "masking_roi_table")) { continue; }
            let backend = roi_backend(&attrs).to_string();
            tables.push(RoiTableSummary { name, supported: matches!(backend.as_str(), "csv" | "json" | "parquet" | "anndata"), backend });
        }
        tables.sort_by(|a,b| a.name.cmp(&b.name));
        Ok(tables)
    }

    pub fn import_roi_table(&self, dataset: &str, root: &Path, session: &LocalSession, name: &str) -> Result<AnnotationLayer, String> {
        let name = safe_name(name)?;
        let path = root.join("tables").join(&name);
        if fs::symlink_metadata(&path).map_err(|error| error.to_string())?.file_type().is_symlink()
            || !path.canonicalize().map_err(|error| error.to_string())?.starts_with(root.canonicalize().map_err(|error| error.to_string())?) {
            return Err("ROI table escapes the configured dataset".into());
        }
        let attrs = table_attributes(&path)?;
        if !matches!(attrs.get("type").and_then(serde_json::Value::as_str), Some("roi_table" | "masking_roi_table")) { return Err("table is not an ROI table".into()); }
        let backend = roi_backend(&attrs);
        let scale = RoiScale::from_attributes(&attrs)?;
        let payload = |filename: &str| -> Result<Vec<u8>, String> {
            let file = path.join(filename);
            if !file.canonicalize().map_err(|error| error.to_string())?.starts_with(root.canonicalize().map_err(|error| error.to_string())?) {
                return Err("ROI table payload escapes the configured dataset".into());
            }
            fs::read(file).map_err(|error| error.to_string())
        };
        let rows = match backend {
            "csv" => roi_rows_from_csv(&payload("table.csv")?)?,
            "json" => roi_rows_from_json(&payload("table.json")?)?,
            "parquet" => roi_rows_from_parquet(&payload("table.parquet")?)?,
            "anndata" => anndata::roi_rows_from_anndata(root, &name)?,
            other => return Err(format!("ROI table backend {other} is not supported by this importer")),
        };
        let annotations = rows.into_iter().map(|row| roi_row_to_annotation(&row, session, scale)).collect::<Result<Vec<_>, _>>()?;
        self.with_dataset(dataset, root, |state| {
            let candidate = format!("roi_{name}");
            let mut layer_name = candidate.clone();
            let mut suffix = 2;
            while state.layers.iter().any(|layer| layer.name == layer_name) {
                layer_name = format!("{candidate}_{suffix}"); suffix += 1;
            }
            let id = state.next_layer_id;
            state.next_layer_id += 1;
            let mut layer = AnnotationLayer::new(id, layer_name, annotations);
            layer.dirty = true;
            layer.save_target = format!("tables/{name}");
            state.layers.push(layer.clone());
            Ok(layer)
        })
    }
}

fn table_attributes(path: &Path) -> Result<serde_json::Value, String> {
    let v3 = path.join("zarr.json");
    if v3.is_file() {
        let metadata: serde_json::Value = serde_json::from_slice(&fs::read(v3).map_err(|error| error.to_string())?).map_err(|error: serde_json::Error| error.to_string())?;
        Ok(metadata.get("attributes").cloned().unwrap_or_default())
    } else {
        let v2 = path.join(".zattrs");
        if v2.is_file() { serde_json::from_slice(&fs::read(v2).map_err(|error| error.to_string())?).map_err(|error: serde_json::Error| error.to_string()) }
        else { Ok(serde_json::Value::Null) }
    }
}

fn roi_backend(attrs: &serde_json::Value) -> &str {
    match attrs.get("backend").and_then(serde_json::Value::as_str).unwrap_or("csv") {
        "experimental_csv_v1" => "csv",
        "experimental_json_v1" => "json",
        "experimental_parquet_v1" => "parquet",
        "anndata_v1" => "anndata",
        other => other,
    }
}

/// The source viewer records how its level-zero pixels became table file units. When present,
/// that declared mapping takes precedence over the currently opened image's NGFF transform.
#[derive(Clone, Copy)]
struct RoiScale { voxel_xyz: [f64; 3], seconds: f64 }

impl RoiScale {
    fn from_attributes(attrs: &serde_json::Value) -> Result<Option<Self>, String> {
        let Some(ours) = attrs.get("omezarr_viewer") else { return Ok(None); };
        let Some(zyx) = ours.get("world_pixel_size_zyx") else { return Ok(None); };
        let zyx: [f64; 3] = serde_json::from_value(zyx.clone()).map_err(|_| "ROI table has invalid world_pixel_size_zyx")?;
        if zyx.iter().any(|value| !value.is_finite() || *value <= 0.0) {
            return Err("ROI table pixel scale must be positive and finite".into());
        }
        let seconds = ours.get("world_seconds_per_frame").and_then(serde_json::Value::as_f64).unwrap_or(1.0);
        if !seconds.is_finite() || seconds <= 0.0 { return Err("ROI table time scale must be positive and finite".into()); }
        Ok(Some(Self { voxel_xyz: [zyx[2], zyx[1], zyx[0]], seconds }))
    }
}

type RoiRow = HashMap<String, String>;

fn roi_rows_from_csv(bytes: &[u8]) -> Result<Vec<RoiRow>, String> {
    let mut reader = csv::Reader::from_reader(bytes);
    let headers = reader.headers().map_err(|error| error.to_string())?.iter().map(str::to_string).collect::<Vec<_>>();
    reader.records().map(|record| {
        let record = record.map_err(|error| error.to_string())?;
        Ok(headers.iter().cloned().zip(record.iter().map(str::to_string)).collect())
    }).collect()
}

fn roi_rows_from_json(bytes: &[u8]) -> Result<Vec<RoiRow>, String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    let stringify = |value: &serde_json::Value| value.as_str().map(str::to_string).unwrap_or_else(|| value.to_string());
    match value {
        serde_json::Value::Array(rows) => rows.into_iter().map(|row| {
            let object = row.as_object().ok_or("JSON ROI rows must be objects")?;
            Ok(object.iter().map(|(key,value)| (key.clone(), stringify(value))).collect())
        }).collect(),
        serde_json::Value::Object(columns) => {
            let count = columns.values().filter_map(serde_json::Value::as_array).map(Vec::len).max().unwrap_or(0);
            Ok((0..count).map(|index| columns.iter().filter_map(|(key, values)| {
                values.as_array().and_then(|values| values.get(index)).map(|value| (key.clone(), stringify(value)))
            }).collect()).collect())
        }
        _ => Err("JSON ROI table must be rows or columns".into()),
    }
}

fn roi_rows_from_parquet(bytes: &[u8]) -> Result<Vec<RoiRow>, String> {
    use parquet::file::reader::{FileReader, SerializedFileReader};
    use parquet::record::Field;
    let reader = SerializedFileReader::new(bytes::Bytes::copy_from_slice(bytes)).map_err(|error| error.to_string())?;
    reader.get_row_iter(None).map_err(|error| error.to_string())?.map(|row| {
        let row = row.map_err(|error| error.to_string())?;
        Ok(row.get_column_iter().map(|(name,field)| {
            let value = match field {
                Field::Null => String::new(), Field::Str(text) => text.clone(), other => other.to_string(),
            };
            (name.clone(), value)
        }).collect())
    }).collect()
}

fn roi_row_to_annotation(row: &RoiRow, session: &LocalSession, scale: Option<RoiScale>) -> Result<Annotation, String> {
    let number = |name: &str| -> Result<f64, String> {
        row.get(name).ok_or_else(|| format!("ROI table lacks {name}"))?.parse::<f64>()
            .map_err(|_| format!("ROI table has invalid {name}"))
    };
    if row.contains_key("__voxel_x") {
        let (x,y,z) = (number("__voxel_x")?, number("__voxel_y")?, number("__voxel_z")?);
        if [x,y,z].iter().any(|value| !value.is_finite()) { return Err("AnnData spatial position must be finite".into()); }
        return Ok(Annotation {
            geometry: newvolim_scene::qupath::Geometry::Point([x,y]),
            plane: newvolim_scene::qupath::Plane::at(z.round() as i32, 0),
            label: row.get("class").or_else(|| row.get("label")).cloned().unwrap_or_default(),
            ..Annotation::default()
        });
    }
    let (x,y,z) = (number("x_micrometer")?,number("y_micrometer")?,number("z_micrometer")?);
    let (lx,ly,lz) = (number("len_x_micrometer")?,number("len_y_micrometer")?,number("len_z_micrometer")?);
    if [x,y,z,lx,ly,lz].iter().any(|value| !value.is_finite()) || [lx,ly,lz].iter().any(|value| *value < 0.0) {
        return Err("ROI table has non-finite or negative bounds".into());
    }
    let (a, b) = if let Some(scale) = scale {
        let [sx,sy,sz] = scale.voxel_xyz;
        ([x/sx,y/sy,z/sz],[(x+lx)/sx,(y+ly)/sy,(z+lz)/sz])
    } else {
        (session.physical_point_voxel_xyz_f64([x,y,z]).map_err(|error| error.to_string())?,
            session.physical_point_voxel_xyz_f64([x+lx,y+ly,z+lz]).map_err(|error| error.to_string())?)
    };
    let geometry = if (a[0]-b[0]).abs() < 1e-9 && (a[1]-b[1]).abs() < 1e-9 {
        newvolim_scene::qupath::Geometry::Point([a[0],a[1]])
    } else { newvolim_scene::qupath::Geometry::rect(a[0],a[1],b[0],b[1]) };
    let z_start = a[2].round().max(0.0) as i32;
    let z_extent = (b[2]-a[2]).abs().round().max(0.0) as u32;
    Ok(Annotation {
        geometry, plane: newvolim_scene::qupath::Plane::at(z_start, (number("t_second").unwrap_or(0.0) / scale.map_or(1.0, |scale| scale.seconds)).round() as i32),
        z_extent, t_extent: (number("len_t_second").unwrap_or(0.0) / scale.map_or(1.0, |scale| scale.seconds)).round().max(0.0) as u32,
        label: row.get("class").or_else(|| row.get("label")).cloned().unwrap_or_default(),
        ..Annotation::default()
    })
}

fn is_axis_aligned_roi(item: &Annotation) -> bool {
    use newvolim_scene::qupath::Geometry;
    match &item.geometry {
        Geometry::Point(_) => true,
        Geometry::Polygon(rings) if !item.is_ellipse && rings.len() == 1 && rings[0].len() == 5 => {
            let p = &rings[0];
            p[0] == p[4] && p[0][1] == p[1][1] && p[1][0] == p[2][0] && p[2][1] == p[3][1] && p[3][0] == p[0][0]
        }
        _ => false,
    }
}

fn write_roi_group_metadata(root: &Path, name: &str) -> Result<(), String> {
    let parent = root.join("tables");
    let set = parent.join(name);
    let v3 = root.join("zarr.json").is_file();
    let parent_file = parent.join(if v3 { "zarr.json" } else { ".zattrs" });
    let mut parent_metadata: serde_json::Value = if parent_file.exists() {
        serde_json::from_slice(&fs::read(&parent_file).map_err(|error| error.to_string())?).map_err(|error: serde_json::Error| error.to_string())?
    } else if v3 { serde_json::json!({"zarr_format":3,"node_type":"group","attributes":{}}) }
    else { serde_json::json!({}) };
    let listed = if v3 { &mut parent_metadata["attributes"]["tables"] } else { &mut parent_metadata["tables"] };
    let mut names = listed.as_array().cloned().unwrap_or_default();
    if !names.iter().any(|value| value.as_str() == Some(name)) { names.push(serde_json::json!(name)); }
    *listed = serde_json::Value::Array(names);
    fs::write(&parent_file, serde_json::to_vec_pretty(&parent_metadata).map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
    let attrs = serde_json::json!({"type":"roi_table", "table_version":"1", "backend":"csv", "index_key":"FieldIndex", "index_type":"str"});
    if v3 {
        fs::write(set.join("zarr.json"), serde_json::to_vec_pretty(&serde_json::json!({"zarr_format":3,"node_type":"group","attributes":attrs})).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    } else {
        fs::write(parent.join(".zgroup"), b"{\"zarr_format\":2}").map_err(|error| error.to_string())?;
        fs::write(set.join(".zgroup"), b"{\"zarr_format\":2}").map_err(|error| error.to_string())?;
        fs::write(set.join(".zattrs"), serde_json::to_vec_pretty(&attrs).map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn write_group_metadata(root: &Path, name: &str) -> Result<(), String> {
    let parent = root.join("annotations");
    let set = parent.join(name);
    let v3 = root.join("zarr.json").is_file();
    let attrs = serde_json::json!({
        "type": "geojson_annotations", "version": "1", "dialect": "qupath",
        "coordinate_space": {"axes": ["x", "y"], "units": "pixel", "level": 0, "origin": "top-left", "y_axis": "down"},
        "extensions": ["zExtent", "tExtent", "strokeWidth", "denseRegion"],
        "supervision": {"default": "sparse", "dense_within": "denseRegion"},
        "rasterisation": {"stroke": "pixels within strokeWidth/2 of the path", "cap": "round", "join": "round",
            "region": "even-odd over the rings; ring 0 is the exterior", "sampling": "4x4 subsamples per pixel, on at 7 of 16 or more",
            "pixel_centre": "the integer coordinate", "sampling_applies_to": ["stroke", "region"],
            "fill_and_stroke": "union", "collision": "highest shape id", "level": 0},
        "written_by": "newvolim"
    });
    let parent_file = parent.join(if v3 { "zarr.json" } else { ".zattrs" });
    let mut parent_metadata: serde_json::Value = if parent_file.exists() {
        serde_json::from_slice(&fs::read(&parent_file).map_err(|error| error.to_string())?).map_err(|error: serde_json::Error| error.to_string())?
    } else if v3 {
        serde_json::json!({"zarr_format":3,"node_type":"group","attributes":{}})
    } else { serde_json::json!({}) };
    let listed = if v3 { &mut parent_metadata["attributes"]["annotations"] } else { &mut parent_metadata["annotations"] };
    let mut names = listed.as_array().cloned().unwrap_or_default();
    if !names.iter().any(|value| value.as_str() == Some(name)) { names.push(serde_json::json!(name)); }
    *listed = serde_json::Value::Array(names);
    fs::write(&parent_file, serde_json::to_vec_pretty(&parent_metadata).map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
    if v3 {
        fs::write(set.join("zarr.json"), serde_json::to_vec_pretty(&serde_json::json!({"zarr_format":3,"node_type":"group","attributes":attrs})).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    } else {
        fs::write(parent.join(".zgroup"), b"{\"zarr_format\":2}").map_err(|error| error.to_string())?;
        fs::write(set.join(".zgroup"), b"{\"zarr_format\":2}").map_err(|error| error.to_string())?;
        fs::write(set.join(".zattrs"), serde_json::to_vec_pretty(&attrs).map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn safe_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." || name.len() > 120
        || name.chars().any(|ch| ch == '/' || ch == '\\' || ch.is_control()) {
        return Err("layer name must be 1–120 characters without path separators or controls".into());
    }
    Ok(name.into())
}

fn prepare_child_directory(root: &Path, group: &str, name: &str) -> Result<std::path::PathBuf, String> {
    let canonical_root = root.canonicalize().map_err(|error| error.to_string())?;
    let parent = root.join(group);
    if parent.exists() && fs::symlink_metadata(&parent).map_err(|error| error.to_string())?.file_type().is_symlink() {
        return Err("annotation group is a symlink".into());
    }
    fs::create_dir_all(&parent).map_err(|error| error.to_string())?;
    if !parent.canonicalize().map_err(|error| error.to_string())?.starts_with(&canonical_root) {
        return Err("annotation group escapes the configured dataset".into());
    }
    let child = parent.join(name);
    if child.exists() && fs::symlink_metadata(&child).map_err(|error| error.to_string())?.file_type().is_symlink() {
        return Err("annotation target is a symlink".into());
    }
    fs::create_dir_all(&child).map_err(|error| error.to_string())?;
    if !child.canonicalize().map_err(|error| error.to_string())?.starts_with(canonical_root) {
        return Err("annotation target escapes the configured dataset".into());
    }
    for file in [parent.join(".zattrs"), parent.join(".zgroup"), parent.join("zarr.json"),
        child.join(".zattrs"), child.join(".zgroup"), child.join("zarr.json"),
        child.join("annotations.geojson.tmp"), child.join("table.csv.tmp")] {
        if fs::symlink_metadata(&file).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(format!("annotation target {} is a symlink", file.display()));
        }
    }
    Ok(child)
}

fn load_dataset(root: &Path) -> Result<AnnotationDataset, String> {
    let mut state = AnnotationDataset::default();
    let dir = root.join("annotations");
    if !dir.exists() { return Ok(state); }
    let mut entries = fs::read_dir(dir).map_err(|error| error.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if safe_name(&name).is_err() || !entry.file_type().map_err(|error| error.to_string())?.is_dir() { continue; }
        let file = entry.path().join("annotations.geojson");
        if !file.is_file() { continue; }
        if !file.canonicalize().map_err(|error| error.to_string())?.starts_with(root.canonicalize().map_err(|error| error.to_string())?) {
            return Err(format!("{} escapes the configured dataset", file.display()));
        }
        if fs::metadata(&file).map_err(|error| error.to_string())?.len() > MAX_ANNOTATION_BYTES as u64 {
            return Err(format!("{} exceeds 128 MiB", file.display()));
        }
        let bytes = fs::read(&file).map_err(|error| error.to_string())?;
        let rows = qupath_geojson::parse(&bytes).map_err(|error| format!("{}: {error}", file.display()))?;
        let id = state.next_layer_id;
        state.next_layer_id += 1;
        state.layers.push(AnnotationLayer::new(id, name, rows));
    }
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use newvolim_scene::qupath::{Geometry, Plane};

    #[cfg(unix)]
    #[test]
    fn annotation_save_rejects_symlinked_group_before_creating_a_child() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("annotations")).unwrap();
        let store = AnnotationStore::default();
        let layer = store.create("d", root.path(), "cells".into()).unwrap();
        assert!(store.save("d", root.path(), layer.id).unwrap_err().contains("symlink"));
        assert!(!outside.path().join("cells").exists());
    }

    #[test]
    fn layers_nest_edit_save_and_reload_without_reusing_ids() {
        let root = tempfile::tempdir().unwrap();
        let store = AnnotationStore::default();
        let layer = store.create("d", root.path(), "cells".into()).unwrap();
        let parent = store.edit("d", root.path(), layer.id, |l| l.add(Annotation::rect(0.0, 0.0, 10.0, 10.0, Plane::default()))).unwrap();
        let child = store.edit("d", root.path(), layer.id, |l| l.add(Annotation::point(5.0, 5.0, Plane::default()))).unwrap();
        assert_eq!(child.parent, Some(parent.id));
        store.edit("d", root.path(), layer.id, |l| l.remove(parent.id)).unwrap();
        store.edit("d", root.path(), layer.id, |l| {
            let snapshot = l.annotations.clone();
            l.replace_items(snapshot)
        }).unwrap();
        let third = store.edit("d", root.path(), layer.id, |l| l.add(Annotation { geometry: Geometry::Point([6.0, 6.0]), ..Annotation::default() })).unwrap();
        assert!(third.id > child.id);
        store.save("d", root.path(), layer.id).unwrap();
        let attrs: serde_json::Value = serde_json::from_slice(&fs::read(root.path().join("annotations/cells/.zattrs")).unwrap()).unwrap();
        assert_eq!(attrs["coordinate_space"]["units"], "pixel");
        let index: serde_json::Value = serde_json::from_slice(&fs::read(root.path().join("annotations/.zattrs")).unwrap()).unwrap();
        assert_eq!(index["annotations"], serde_json::json!(["cells"]));
        let reopened = AnnotationStore::default().layers("d", root.path()).unwrap();
        assert_eq!(reopened[0].annotations.len(), 2);
        assert!(!reopened[0].dirty);
        assert_eq!(reopened[0].annotations[0].parent, None);
    }

    #[test]
    fn v3_save_declares_the_annotation_group_and_coordinate_space() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("zarr.json"), r#"{"zarr_format":3,"node_type":"group","attributes":{}}"#).unwrap();
        let store = AnnotationStore::default();
        let layer = store.create("v3", root.path(), "cell regions".into()).unwrap();
        store.save("v3", root.path(), layer.id).unwrap();
        let index: serde_json::Value = serde_json::from_slice(&fs::read(root.path().join("annotations/zarr.json")).unwrap()).unwrap();
        assert_eq!(index["attributes"]["annotations"], serde_json::json!(["cell regions"]));
        let metadata: serde_json::Value = serde_json::from_slice(&fs::read(root.path().join("annotations/cell regions/zarr.json")).unwrap()).unwrap();
        assert_eq!(metadata["attributes"]["type"], "geojson_annotations");
    }

    #[test]
    fn roi_csv_export_uses_physical_spacing_and_reports_flattened_polygons() {
        let root = tempfile::tempdir().unwrap();
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test-data/cells3d-anisotropic.ome.zarr");
        fs::copy(source.join("zarr.json"), root.path().join("zarr.json")).unwrap();
        for level in ["0", "1", "2"] {
            fs::create_dir(root.path().join(level)).unwrap();
            fs::copy(source.join(level).join("zarr.json"), root.path().join(level).join("zarr.json")).unwrap();
        }
        let mut session = LocalSession::default();
        session.open_local_omezarr(root.path()).unwrap();
        let store = AnnotationStore::default();
        let layer = store.create("v3", root.path(), "regions".into()).unwrap();
        let annotation = Annotation {
            geometry: Geometry::Polygon(vec![vec![[2.0, 3.0], [6.0, 3.0], [4.0, 7.0], [2.0, 3.0]]]),
            plane: Plane::at(2, 0), label: "mitotic, cell".into(), ..Annotation::default()
        };
        store.edit("v3", root.path(), layer.id, |layer| layer.add(annotation)).unwrap();
        let report = store.save_roi_csv("v3", root.path(), layer.id, &session).unwrap();
        assert_eq!(report.flattened, 1);
        let mut reader = csv::Reader::from_path(root.path().join("tables/regions_roi/table.csv")).unwrap();
        let row = reader.records().next().unwrap().unwrap();
        assert_eq!(&row[0], "roi_1");
        assert_eq!(&row[1], "0.52");
        assert_eq!(&row[2], "0.78");
        assert_eq!(&row[4], "1.04");
        assert_eq!(&row[9], "mitotic, cell");
        assert_eq!(&row[6], "0", "a single Z plane has no further Z extent");
        assert_eq!(&row[8], "0", "a single timepoint has no further T extent");
        assert_eq!(AnnotationStore::roi_tables(root.path()).unwrap()[0].backend, "csv");
        let imported = store.import_roi_table("v3", root.path(), &session, "regions_roi").unwrap();
        assert_eq!(imported.annotations.len(), 1);
        assert_eq!(imported.annotations[0].bounds(), Some([2.0, 3.0, 6.0, 7.0]));
        assert_eq!(imported.annotations[0].label, "mitotic, cell");
        assert_eq!(imported.save_target, "tables/regions_roi");
        let saved = store.save_to("v3", root.path(), layer.id, &session, "annotations/curated").unwrap();
        assert_eq!(saved.format, "geojson");
        assert_eq!(saved.flattened, 0);
        assert!(root.path().join("annotations/curated/annotations.geojson").is_file());
        assert_eq!(store.layers("v3", root.path()).unwrap()[0].save_target, "annotations/curated");
        let saved = store.save_to("v3", root.path(), layer.id, &session, "tables/regions_flat").unwrap();
        assert_eq!(saved.format, "roi_table");
        assert_eq!(saved.flattened, 1);
        assert!(root.path().join("tables/regions_flat/table.csv").is_file());
        assert_eq!(store.layers("v3", root.path()).unwrap()[0].save_target, "tables/regions_flat");
    }

    #[test]
    fn roi_table_aliases_and_recorded_pixel_scale_follow_the_source_viewer() {
        assert_eq!(roi_backend(&serde_json::json!({"backend":"experimental_csv_v1"})), "csv");
        assert_eq!(roi_backend(&serde_json::json!({"backend":"experimental_json_v1"})), "json");
        assert_eq!(roi_backend(&serde_json::json!({"backend":"experimental_parquet_v1"})), "parquet");
        assert_eq!(roi_backend(&serde_json::json!({"backend":"anndata_v1"})), "anndata");
        assert_eq!(roi_backend(&serde_json::json!({})), "csv");
        let attrs = serde_json::json!({"omezarr_viewer":{"world_pixel_size_zyx":[2.0,5.0,2.0],"world_seconds_per_frame":2.0}});
        let scale = RoiScale::from_attributes(&attrs).unwrap();
        let row = RoiRow::from([
            ("x_micrometer".into(), "20".into()), ("y_micrometer".into(), "30".into()),
            ("z_micrometer".into(), "40".into()), ("len_x_micrometer".into(), "10".into()),
            ("len_y_micrometer".into(), "20".into()), ("len_z_micrometer".into(), "4".into()),
            ("t_second".into(), "6".into()), ("len_t_second".into(), "4".into()),
            ("class".into(), "cell".into()),
        ]);
        let item = roi_row_to_annotation(&row, &LocalSession::default(), scale).unwrap();
        assert_eq!(item.bounds(), Some([10.0, 6.0, 15.0, 10.0]));
        assert_eq!(item.plane, Plane::at(20, 3));
        assert_eq!((item.z_extent, item.t_extent), (2, 2));
        assert_eq!(item.label, "cell");
    }
}
