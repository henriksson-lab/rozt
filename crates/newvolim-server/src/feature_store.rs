//! Dataset-local label images, object tables, and atlas metadata.
//!
//! The image renderer treats intensities as floats. Labels deliberately use a separate path so
//! an id above 2^24 is never rounded on its way to picking, outlining, or an atlas lookup.

use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use newvolim_io::{read_array_info, read_array_region, read_dataset_metadata};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureManifest {
    pub labels: Vec<LabelSummary>,
    pub objects: Vec<ObjectSummary>,
    pub tables: Vec<MeasurementSummary>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelSummary {
    pub name: String,
    pub shape_xyz: [u32; 3],
    pub levels: Vec<[u32; 3]>,
    pub has_color_table: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectSummary {
    pub name: String,
    pub count: usize,
    pub has_z: bool,
    pub columns: Vec<ObjectColumn>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectColumn {
    pub name: String,
    pub numeric: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<[f64; 2]>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeasurementSummary {
    pub name: String,
    pub count: usize,
    pub region: Option<String>,
    pub columns: Vec<ObjectColumn>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectPoint {
    pub row: usize,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub values: Vec<Option<f64>>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectQueryResult {
    pub points: Vec<ObjectPoint>,
    pub total: usize,
    pub too_dense: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelInspection {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acronym: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegionCount {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acronym: Option<String>,
    pub count: usize,
}

#[derive(Clone, Debug)]
struct LabelLayer {
    name: String,
    root: PathBuf,
    paths: Vec<String>,
    levels: Vec<[u32; 3]>,
    axes: Vec<String>,
    colors: HashMap<u64, [u8; 4]>,
    properties: HashMap<u64, AtlasEntry>,
}

#[derive(Clone, Debug)]
enum ColumnValues {
    Number(Vec<f64>),
    Text(Vec<String>),
}

#[derive(Clone, Debug)]
struct ObjectTable {
    name: String,
    positions: Vec<[f64; 3]>,
    has_z: bool,
    columns: Vec<(String, ColumnValues)>,
    grid: ObjectGrid,
}

#[derive(Clone, Debug)]
struct ObjectGrid {
    origin: [f64; 2],
    cell: [f64; 2],
    shape: [usize; 2],
    offsets: Vec<u32>,
    rows: Vec<u32>,
}

impl ObjectGrid {
    fn build(positions: &[[f64; 3]]) -> Self {
        let mut lo = [f64::INFINITY; 2];
        let mut hi = [f64::NEG_INFINITY; 2];
        for p in positions {
            lo[0] = lo[0].min(p[1]);
            lo[1] = lo[1].min(p[2]);
            hi[0] = hi[0].max(p[1]);
            hi[1] = hi[1].max(p[2]);
        }
        if !lo[0].is_finite() {
            lo = [0.0; 2];
            hi = [1.0; 2]
        }
        let target = ((positions.len() as f64 / 64.0).sqrt().ceil() as usize).clamp(1, 512);
        let cell = [
            ((hi[0] - lo[0]).max(1.0)) / target as f64,
            ((hi[1] - lo[1]).max(1.0)) / target as f64,
        ];
        let shape = [target, target];
        let bucket_count = target * target;
        let mut offsets = vec![0u32; bucket_count + 1];
        for p in positions {
            let (by, bx) = Self::bucket(lo, cell, shape, p[1], p[2]);
            offsets[by * target + bx + 1] += 1;
        }
        for bucket in 0..bucket_count {
            offsets[bucket + 1] += offsets[bucket];
        }
        let mut cursor = offsets[..bucket_count].to_vec();
        let mut rows = vec![0u32; positions.len()];
        for (row, p) in positions.iter().enumerate() {
            let (by, bx) = Self::bucket(lo, cell, shape, p[1], p[2]);
            let bucket = by * target + bx;
            rows[cursor[bucket] as usize] = row as u32;
            cursor[bucket] += 1;
        }
        Self {
            origin: lo,
            cell,
            shape,
            offsets,
            rows,
        }
    }
    fn bucket(
        origin: [f64; 2],
        cell: [f64; 2],
        shape: [usize; 2],
        y: f64,
        x: f64,
    ) -> (usize, usize) {
        (
            (((y - origin[0]) / cell[0]).floor().max(0.0) as usize).min(shape[0] - 1),
            (((x - origin[1]) / cell[1]).floor().max(0.0) as usize).min(shape[1] - 1),
        )
    }
    fn candidates(&self, y0: f64, y1: f64, x0: f64, x1: f64) -> impl Iterator<Item = u32> + '_ {
        let (by0, bx0) = Self::bucket(self.origin, self.cell, self.shape, y0, x0);
        let (by1, bx1) = Self::bucket(self.origin, self.cell, self.shape, y1, x1);
        (by0..=by1).flat_map(move |by| {
            (bx0..=bx1).flat_map(move |bx| {
                let bucket = by * self.shape[1] + bx;
                self.rows[self.offsets[bucket] as usize..self.offsets[bucket + 1] as usize]
                    .iter()
                    .copied()
            })
        })
    }
}

#[derive(Clone, Debug)]
struct MeasurementTable {
    name: String,
    region: Option<String>,
    ids: Vec<u64>,
    rows_by_id: HashMap<u64, usize>,
    columns: Vec<(String, Vec<Option<f64>>)>,
}

#[derive(Clone, Debug, Default)]
struct AtlasEntry {
    name: Option<String>,
    acronym: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct DatasetFeatures {
    labels: Vec<LabelLayer>,
    objects: Vec<ObjectTable>,
    tables: Vec<MeasurementTable>,
    atlas: HashMap<u64, AtlasEntry>,
}

#[derive(Clone, Default)]
pub struct FeatureStore(Arc<Mutex<HashMap<PathBuf, Arc<DatasetFeatures>>>>);

impl FeatureStore {
    fn dataset(&self, root: &Path) -> Result<Arc<DatasetFeatures>, String> {
        if let Some(found) = self
            .0
            .lock()
            .map_err(|_| "feature cache lock was poisoned")?
            .get(root)
            .cloned()
        {
            return Ok(found);
        }
        let loaded = Arc::new(discover(root)?);
        self.0
            .lock()
            .map_err(|_| "feature cache lock was poisoned")?
            .insert(root.to_owned(), loaded.clone());
        Ok(loaded)
    }

    pub fn manifest(&self, root: &Path) -> Result<FeatureManifest, String> {
        let data = self.dataset(root)?;
        Ok(FeatureManifest {
            labels: data
                .labels
                .iter()
                .map(|label| LabelSummary {
                    name: label.name.clone(),
                    shape_xyz: label.levels[0],
                    levels: label.levels.clone(),
                    has_color_table: !label.colors.is_empty(),
                })
                .collect(),
            objects: data.objects.iter().map(ObjectTable::summary).collect(),
            tables: data.tables.iter().map(MeasurementTable::summary).collect(),
        })
    }

    pub fn label_tile(
        &self,
        root: &Path,
        name: &str,
        level: usize,
        tile_x: u32,
        tile_y: u32,
        edge: u32,
        outline: bool,
        selected: Option<u64>,
        opacity: f32,
        z: u32,
        measurement: Option<(&str, usize, [f64; 2])>,
    ) -> Result<Vec<u8>, String> {
        let data = self.dataset(root)?;
        let label = data
            .labels
            .iter()
            .find(|item| item.name == name)
            .ok_or("unknown label layer")?;
        let level = level.min(label.paths.len().saturating_sub(1));
        let shape = label.levels[level];
        let x0 = tile_x.saturating_mul(edge);
        let y0 = tile_y.saturating_mul(edge);
        if x0 >= shape[0] || y0 >= shape[1] {
            return Err("label tile lies outside the array".into());
        }
        let width = edge.min(shape[0] - x0);
        let height = edge.min(shape[1] - y0);
        let halo_x0 = x0.saturating_sub(1);
        let halo_y0 = y0.saturating_sub(1);
        let halo_x1 = (x0 + width + 1).min(shape[0]);
        let halo_y1 = (y0 + height + 1).min(shape[1]);
        let scaled_z =
            ((z as u64 * label.levels[level][2] as u64) / label.levels[0][2].max(1) as u64) as u32;
        let (ids, halo_w, _halo_h) = read_label_region(
            label,
            level,
            halo_x0,
            halo_y0,
            halo_x1 - halo_x0,
            halo_y1 - halo_y0,
            scaled_z,
        )?;
        let alpha = (opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
        let mut rgba = vec![0u8; width as usize * height as usize * 4];
        let paint = measurement.and_then(|(name, column, range)| {
            data.tables
                .iter()
                .find(|v| v.name == name)
                .and_then(|table| {
                    table
                        .columns
                        .get(column)
                        .map(|(_, values)| (table, values, range))
                })
        });
        for y in 0..height {
            for x in 0..width {
                let hx = (x0 + x - halo_x0) as usize;
                let hy = (y0 + y - halo_y0) as usize;
                let id = ids[hy * halo_w as usize + hx];
                if id == 0 || selected.is_some_and(|wanted| wanted != id) {
                    continue;
                }
                if outline {
                    let at = |xx: isize, yy: isize| -> u64 {
                        if xx < 0 || yy < 0 || xx >= halo_w as isize {
                            0
                        } else {
                            ids.get(yy as usize * halo_w as usize + xx as usize)
                                .copied()
                                .unwrap_or(0)
                        }
                    };
                    if at(hx as isize - 1, hy as isize) == id
                        && at(hx as isize + 1, hy as isize) == id
                        && at(hx as isize, hy as isize - 1) == id
                        && at(hx as isize, hy as isize + 1) == id
                    {
                        continue;
                    }
                }
                let color = if let Some((table, values, [lo, hi])) = paint {
                    let Some(&row) = table.rows_by_id.get(&id) else {
                        continue;
                    };
                    let Some(value) = values[row] else { continue };
                    if value < lo || value > hi {
                        continue;
                    }
                    measurement_color((value - lo) / (hi - lo).max(f64::EPSILON))
                } else {
                    label
                        .colors
                        .get(&id)
                        .copied()
                        .unwrap_or_else(|| hashed_color(id))
                };
                let out = (y as usize * width as usize + x as usize) * 4;
                rgba[out..out + 3].copy_from_slice(&color[..3]);
                rgba[out + 3] = ((alpha as u16 * color[3] as u16) / 255) as u8;
            }
        }
        let frame = palace_png::RgbaFrame::new(width, height, rgba).map_err(|e| e.to_string())?;
        Ok(palace_png::encode_rgba(&frame))
    }

    pub fn inspect_label(
        &self,
        root: &Path,
        name: &str,
        x: u32,
        y: u32,
        z: u32,
    ) -> Result<LabelInspection, String> {
        let data = self.dataset(root)?;
        let label = data
            .labels
            .iter()
            .find(|item| item.name == name)
            .ok_or("unknown label layer")?;
        let (values, _, _) = read_label_region(label, 0, x, y, 1, 1, z)?;
        let id = values[0];
        let atlas = label.properties.get(&id).or_else(|| data.atlas.get(&id));
        Ok(LabelInspection {
            id,
            name: atlas.and_then(|v| v.name.clone()),
            acronym: atlas.and_then(|v| v.acronym.clone()),
        })
    }

    pub fn query_objects(
        &self,
        root: &Path,
        name: &str,
        bounds: [f64; 6],
        max: usize,
    ) -> Result<ObjectQueryResult, String> {
        let data = self.dataset(root)?;
        let table = data
            .objects
            .iter()
            .find(|item| item.name == name)
            .ok_or("unknown object table")?;
        let mut matches = Vec::with_capacity(max.min(5_000));
        let mut total = 0usize;
        for row in table
            .grid
            .candidates(bounds[2], bounds[3], bounds[0], bounds[1])
            .map(|v| v as usize)
        {
            let p = &table.positions[row];
            if p[2] >= bounds[0]
                && p[2] <= bounds[1]
                && p[1] >= bounds[2]
                && p[1] <= bounds[3]
                && (!table.has_z || (p[0] >= bounds[4] && p[0] <= bounds[5]))
            {
                total += 1;
                if matches.len() < max {
                    matches.push(row);
                }
            }
        }
        if total > max {
            return Ok(ObjectQueryResult {
                points: Vec::new(),
                total,
                too_dense: true,
            });
        }
        let points = matches
            .into_iter()
            .map(|row| ObjectPoint {
                row,
                x: table.positions[row][2],
                y: table.positions[row][1],
                z: table.positions[row][0],
                values: table
                    .columns
                    .iter()
                    .map(|(_, values)| match values {
                        ColumnValues::Number(v) => v[row].is_finite().then_some(v[row]),
                        ColumnValues::Text(_) => None,
                    })
                    .collect(),
            })
            .collect();
        Ok(ObjectQueryResult {
            points,
            total,
            too_dense: false,
        })
    }

    pub fn inspect_object(
        &self,
        root: &Path,
        name: &str,
        row: usize,
    ) -> Result<serde_json::Value, String> {
        let data = self.dataset(root)?;
        let table = data
            .objects
            .iter()
            .find(|item| item.name == name)
            .ok_or("unknown object table")?;
        let p = *table
            .positions
            .get(row)
            .ok_or("object row is outside the table")?;
        let mut columns = serde_json::Map::new();
        for (name, values) in &table.columns {
            let value = match values {
                ColumnValues::Number(v) => {
                    if v[row].is_finite() {
                        serde_json::json!(v[row])
                    } else {
                        serde_json::Value::Null
                    }
                }
                ColumnValues::Text(v) => serde_json::json!(v[row]),
            };
            columns.insert(name.clone(), value);
        }
        Ok(serde_json::json!({"row":row,"x":p[2],"y":p[1],"z":p[0],"columns":columns}))
    }

    pub fn region_counts(
        &self,
        root: &Path,
        label_name: &str,
        object_name: &str,
    ) -> Result<Vec<RegionCount>, String> {
        let data = self.dataset(root)?;
        let label = data
            .labels
            .iter()
            .find(|v| v.name == label_name)
            .ok_or("unknown label layer")?;
        let table = data
            .objects
            .iter()
            .find(|v| v.name == object_name)
            .ok_or("unknown object table")?;
        let shape = label.levels[0];
        let mut counts = HashMap::<u64, usize>::new();
        let mut tiles = HashMap::<(u32, u32, u32), (Vec<u64>, u32)>::new();
        const EDGE: u32 = 256;
        for p in &table.positions {
            let x = p[2].round() as i64;
            let y = p[1].round() as i64;
            let z = p[0].round() as i64;
            if x < 0
                || y < 0
                || z < 0
                || x >= shape[0] as i64
                || y >= shape[1] as i64
                || z >= shape[2] as i64
            {
                continue;
            }
            let (x, y, z) = (x as u32, y as u32, z as u32);
            let key = (z, y / EDGE, x / EDGE);
            if !tiles.contains_key(&key) {
                let x0 = key.2 * EDGE;
                let y0 = key.1 * EDGE;
                let w = EDGE.min(shape[0] - x0);
                let h = EDGE.min(shape[1] - y0);
                let (ids, _, _) = read_label_region(label, 0, x0, y0, w, h, z)?;
                tiles.insert(key, (ids, w));
            }
            let (ids, w) = &tiles[&key];
            let id = ids[((y % EDGE) * *w + (x % EDGE)) as usize];
            *counts.entry(id).or_default() += 1;
        }
        let mut result = counts
            .into_iter()
            .map(|(id, count)| {
                let atlas = label.properties.get(&id).or_else(|| data.atlas.get(&id));
                RegionCount {
                    id,
                    name: atlas.and_then(|v| v.name.clone()),
                    acronym: atlas.and_then(|v| v.acronym.clone()),
                    count,
                }
            })
            .collect::<Vec<_>>();
        result.sort_by(|a, b| b.count.cmp(&a.count).then(a.id.cmp(&b.id)));
        Ok(result)
    }
}

impl ObjectTable {
    fn summary(&self) -> ObjectSummary {
        ObjectSummary {
            name: self.name.clone(),
            count: self.positions.len(),
            has_z: self.has_z,
            columns: self
                .columns
                .iter()
                .map(|(name, v)| match v {
                    ColumnValues::Number(values) => {
                        let mut lo = f64::INFINITY;
                        let mut hi = f64::NEG_INFINITY;
                        for n in values.iter().filter(|v| v.is_finite()) {
                            lo = lo.min(*n);
                            hi = hi.max(*n)
                        }
                        ObjectColumn {
                            name: name.clone(),
                            numeric: true,
                            range: (lo <= hi).then_some([lo, hi]),
                        }
                    }
                    ColumnValues::Text(_) => ObjectColumn {
                        name: name.clone(),
                        numeric: false,
                        range: None,
                    },
                })
                .collect(),
        }
    }
}

impl MeasurementTable {
    fn summary(&self) -> MeasurementSummary {
        MeasurementSummary {
            name: self.name.clone(),
            count: self.ids.len(),
            region: self.region.clone(),
            columns: self
                .columns
                .iter()
                .map(|(name, values)| {
                    let mut lo = f64::INFINITY;
                    let mut hi = f64::NEG_INFINITY;
                    for v in values.iter().flatten().filter(|v| v.is_finite()) {
                        lo = lo.min(*v);
                        hi = hi.max(*v)
                    }
                    ObjectColumn {
                        name: name.clone(),
                        numeric: true,
                        range: (lo <= hi).then_some([lo, hi]),
                    }
                })
                .collect(),
        }
    }
}

fn discover(root: &Path) -> Result<DatasetFeatures, String> {
    let mut out = DatasetFeatures::default();
    let label_dir = root.join("labels");
    if label_dir.is_dir() {
        for entry in fs::read_dir(&label_dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.path().is_dir() {
                if let Ok(label) = open_label(
                    &entry.path(),
                    entry.file_name().to_string_lossy().into_owned(),
                ) {
                    out.labels.push(label)
                }
            }
        }
    }
    let table_dir = root.join("tables");
    if table_dir.is_dir() {
        for entry in fs::read_dir(&table_dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let csv = if path.is_dir() {
                path.join("table.csv")
            } else {
                path.clone()
            };
            let parquet = path.join("table.parquet");
            if csv.extension().and_then(|v| v.to_str()) == Some("csv") && csv.is_file() {
                match read_csv_table(&csv, name.clone()) {
                    Ok(table) => out.objects.push(table),
                    Err(_) => {
                        if let Ok(table) = read_measurement_csv(&csv, name, &path) {
                            out.tables.push(table)
                        }
                    }
                }
            } else if parquet.is_file() {
                match read_parquet_table(&parquet, name.clone()) {
                    Ok(table) => out.objects.push(table),
                    Err(_) => {
                        if let Ok(table) = read_measurement_parquet(&parquet, name, &path) {
                            out.tables.push(table)
                        }
                    }
                }
            } else {
                continue;
            }
        }
    }
    for candidate in [
        root.join("atlas.jsonl"),
        root.join("ontology.jsonl"),
        root.join("ABA_annotation_last.jsonl"),
    ] {
        if candidate.is_file() {
            read_atlas(&candidate, &mut out.atlas)?;
            break;
        }
    }
    out.labels.sort_by(|a, b| a.name.cmp(&b.name));
    out.objects.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn open_label(root: &Path, name: String) -> Result<LabelLayer, String> {
    let metadata = read_dataset_metadata(root).map_err(|e| e.to_string())?;
    let scale = metadata
        .multiscales
        .first()
        .ok_or("label has no multiscales")?;
    let axes = scale
        .axes
        .iter()
        .map(|a| a.name.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let mut paths = Vec::new();
    let mut levels = Vec::new();
    for d in &scale.datasets {
        let info = read_array_info(root, &d.path).map_err(|e| e.to_string())?;
        levels.push(shape_xyz(&info.shape, &axes)?);
        paths.push(d.path.clone())
    }
    if paths.is_empty() {
        return Err("label has no arrays".into());
    }
    let colors = read_label_colors(root);
    let properties = read_label_properties(root);
    Ok(LabelLayer {
        name,
        root: root.to_owned(),
        paths,
        levels,
        axes,
        colors,
        properties,
    })
}
fn shape_xyz(shape: &[u64], axes: &[String]) -> Result<[u32; 3], String> {
    let axis = |name: &str| axes.iter().position(|v| v == name);
    let get = |name: &str, default: u64| {
        axis(name)
            .and_then(|i| shape.get(i).copied())
            .unwrap_or(default)
    };
    Ok([
        u32::try_from(get("x", 1)).map_err(|_| "label x dimension is too large")?,
        u32::try_from(get("y", 1)).map_err(|_| "label y dimension is too large")?,
        u32::try_from(get("z", 1)).map_err(|_| "label z dimension is too large")?,
    ])
}

fn read_label_region(
    label: &LabelLayer,
    level: usize,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    z: u32,
) -> Result<(Vec<u64>, u32, u32), String> {
    let info = read_array_info(&label.root, &label.paths[level]).map_err(|e| e.to_string())?;
    let mut start = vec![0; info.shape.len()];
    let mut shape = vec![1; info.shape.len()];
    for (i, axis) in label.axes.iter().enumerate() {
        match axis.as_str() {
            "x" => {
                start[i] = x as u64;
                shape[i] = width as u64
            }
            "y" => {
                start[i] = y as u64;
                shape[i] = height as u64
            }
            "z" => start[i] = z.min(label.levels[level][2].saturating_sub(1)) as u64,
            _ => {}
        }
    }
    let bytes = read_array_region(&label.root, &label.paths[level], &start, &shape)?;
    let values = decode_ids(&bytes, &info.dtype)?;
    Ok((values, width, height))
}
fn decode_ids(bytes: &[u8], dtype: &str) -> Result<Vec<u64>, String> {
    let little = !dtype.starts_with('>');
    let width = if dtype.contains("uint8") || dtype.ends_with("u1") {
        1
    } else if dtype.contains("uint16") || dtype.ends_with("u2") {
        2
    } else if dtype.contains("uint32") || dtype.ends_with("u4") {
        4
    } else if dtype.contains("uint64") || dtype.ends_with("u8") {
        8
    } else {
        return Err(format!("label dtype {dtype} is not an unsigned integer"));
    };
    if bytes.len() % width != 0 {
        return Err("label byte length is not a whole number of values".into());
    }
    Ok(bytes
        .chunks_exact(width)
        .map(|b| {
            let mut a = [0u8; 8];
            a[..width].copy_from_slice(b);
            if little {
                u64::from_le_bytes(a)
            } else {
                a[..width].reverse();
                u64::from_le_bytes(a)
            }
        })
        .collect())
}
fn hashed_color(id: u64) -> [u8; 4] {
    let mut x = id.wrapping_mul(0x9E3779B97F4A7C15);
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58476D1CE4E5B9);
    x ^= x >> 27;
    [
        (80 + (x & 127)) as u8,
        (80 + ((x >> 8) & 127)) as u8,
        (80 + ((x >> 16) & 127)) as u8,
        255,
    ]
}
fn measurement_color(value: f64) -> [u8; 4] {
    let t = value.clamp(0.0, 1.0);
    let stops = [
        [68., 1., 84.],
        [59., 82., 139.],
        [33., 145., 140.],
        [94., 201., 98.],
        [253., 231., 37.],
    ];
    let s = t * 4.;
    let i = (s.floor() as usize).min(3);
    let f = s - i as f64;
    [
        (stops[i][0] * (1. - f) + stops[i + 1][0] * f) as u8,
        (stops[i][1] * (1. - f) + stops[i + 1][1] * f) as u8,
        (stops[i][2] * (1. - f) + stops[i + 1][2] * f) as u8,
        255,
    ]
}
fn read_label_colors(root: &Path) -> HashMap<u64, [u8; 4]> {
    let path = if root.join("zarr.json").is_file() {
        root.join("zarr.json")
    } else {
        root.join(".zattrs")
    };
    let Ok(bytes) = fs::read(path) else {
        return HashMap::new();
    };
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return HashMap::new();
    };
    let attrs = json.get("attributes").unwrap_or(&json);
    let colors = attrs
        .pointer("/image-label/colors")
        .or_else(|| attrs.pointer("/image-label/colormap"));
    let mut out = HashMap::new();
    if let Some(rows) = colors.and_then(|v| v.as_array()) {
        for row in rows {
            if let Some(a) = row.as_array() {
                if a.len() >= 4 {
                    if let (Some(id), Some(r), Some(g), Some(b)) =
                        (a[0].as_u64(), a[1].as_u64(), a[2].as_u64(), a[3].as_u64())
                    {
                        out.insert(
                            id,
                            [
                                r as u8,
                                g as u8,
                                b as u8,
                                a.get(4).and_then(|v| v.as_u64()).unwrap_or(255) as u8,
                            ],
                        );
                    }
                }
            } else if let Some(o) = row.as_object() {
                if let (Some(id), Some(c)) = (
                    o.get("label-value").and_then(|v| v.as_u64()),
                    o.get("rgba").and_then(|v| v.as_array()),
                ) {
                    if c.len() >= 3 {
                        out.insert(
                            id,
                            [
                                c[0].as_u64().unwrap_or(0) as u8,
                                c[1].as_u64().unwrap_or(0) as u8,
                                c[2].as_u64().unwrap_or(0) as u8,
                                c.get(3).and_then(|v| v.as_u64()).unwrap_or(255) as u8,
                            ],
                        );
                    }
                }
            }
        }
    }
    out
}
fn read_label_properties(root: &Path) -> HashMap<u64, AtlasEntry> {
    let path = if root.join("zarr.json").is_file() {
        root.join("zarr.json")
    } else {
        root.join(".zattrs")
    };
    let Ok(bytes) = fs::read(path) else {
        return HashMap::new();
    };
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return HashMap::new();
    };
    let attrs = json.get("attributes").unwrap_or(&json);
    let Some(properties) = attrs.pointer("/image-label/properties") else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    if let Some(rows) = properties.as_array() {
        for row in rows {
            let Some(object) = row.as_object() else {
                continue;
            };
            let Some(id) = object
                .get("label-value")
                .or_else(|| object.get("id"))
                .and_then(|v| v.as_u64())
            else {
                continue;
            };
            out.insert(
                id,
                AtlasEntry {
                    name: object
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned),
                    acronym: object
                        .get("acronym")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned),
                },
            );
        }
    } else if let Some(rows) = properties.as_object() {
        for (id, row) in rows {
            let Ok(id) = id.parse() else { continue };
            out.insert(
                id,
                AtlasEntry {
                    name: row.get("name").and_then(|v| v.as_str()).map(str::to_owned),
                    acronym: row
                        .get("acronym")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned),
                },
            );
        }
    }
    out
}

fn read_csv_table(path: &Path, name: String) -> Result<ObjectTable, String> {
    let cache = path.with_extension("rozt-object-cache");
    if let Ok(table) = read_object_cache(&cache, path, name.clone()) {
        return Ok(table);
    }
    let table = read_csv_bytes(&fs::read(path).map_err(|e| e.to_string())?, name)?;
    // A read-only dataset is valid; it merely misses the startup optimization.
    let _ = write_object_cache(&cache, path, &table);
    Ok(table)
}

const OBJECT_CACHE_MAGIC: &[u8; 8] = b"ROZTOBJ1";

fn source_stamp(path: &Path) -> Result<(u64, u64, u32), String> {
    let metadata = fs::metadata(path).map_err(|e| e.to_string())?;
    let modified = metadata
        .modified()
        .map_err(|e| e.to_string())?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?;
    Ok((metadata.len(), modified.as_secs(), modified.subsec_nanos()))
}

fn write_object_cache(path: &Path, source: &Path, table: &ObjectTable) -> Result<(), String> {
    if table.positions.len() > u32::MAX as usize
        || table
            .columns
            .iter()
            .any(|(_, values)| !matches!(values, ColumnValues::Number(_)))
    {
        return Err(
            "binary object cache currently requires numeric columns and at most 2^32 rows".into(),
        );
    }
    let (source_len, source_secs, source_nanos) = source_stamp(source)?;
    let temporary = path.with_extension(format!("rozt-object-cache.{}.tmp", std::process::id()));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|e| e.to_string())?;
    let mut out = BufWriter::new(file);
    out.write_all(OBJECT_CACHE_MAGIC)
        .map_err(|e| e.to_string())?;
    write_u64(&mut out, source_len)?;
    write_u64(&mut out, source_secs)?;
    write_u32(&mut out, source_nanos)?;
    write_u64(&mut out, table.positions.len() as u64)?;
    out.write_all(&[u8::from(table.has_z)])
        .map_err(|e| e.to_string())?;
    write_u32(&mut out, table.columns.len() as u32)?;
    for (name, _) in &table.columns {
        write_u32(&mut out, name.len() as u32)?;
        out.write_all(name.as_bytes()).map_err(|e| e.to_string())?;
    }
    for position in &table.positions {
        for value in position {
            write_f64(&mut out, *value)?;
        }
    }
    for (_, values) in &table.columns {
        let ColumnValues::Number(values) = values else {
            unreachable!()
        };
        for value in values {
            write_f64(&mut out, *value)?;
        }
    }
    for value in table.grid.origin.into_iter().chain(table.grid.cell) {
        write_f64(&mut out, value)?;
    }
    write_u32(&mut out, table.grid.shape[0] as u32)?;
    write_u32(&mut out, table.grid.shape[1] as u32)?;
    write_u32(&mut out, table.grid.offsets.len() as u32)?;
    for value in &table.grid.offsets {
        write_u32(&mut out, *value)?;
    }
    for value in &table.grid.rows {
        write_u32(&mut out, *value)?;
    }
    out.flush().map_err(|e| e.to_string())?;
    out.get_ref().sync_all().map_err(|e| e.to_string())?;
    fs::rename(&temporary, path).map_err(|e| e.to_string())
}

fn read_object_cache(path: &Path, source: &Path, name: String) -> Result<ObjectTable, String> {
    let mut input = BufReader::new(File::open(path).map_err(|e| e.to_string())?);
    let mut magic = [0u8; 8];
    input.read_exact(&mut magic).map_err(|e| e.to_string())?;
    if &magic != OBJECT_CACHE_MAGIC {
        return Err("unrecognized object cache".into());
    }
    let expected = source_stamp(source)?;
    let actual = (
        read_u64(&mut input)?,
        read_u64(&mut input)?,
        read_u32(&mut input)?,
    );
    if actual != expected {
        return Err("stale object cache".into());
    }
    let rows = usize::try_from(read_u64(&mut input)?).map_err(|_| "object count is too large")?;
    if rows > u32::MAX as usize {
        return Err("object cache has too many rows".into());
    }
    let mut flag = [0u8; 1];
    input.read_exact(&mut flag).map_err(|e| e.to_string())?;
    let has_z = flag[0] != 0;
    let column_count = read_u32(&mut input)? as usize;
    if column_count > 4096 {
        return Err("object cache has too many columns".into());
    }
    let mut names = Vec::with_capacity(column_count);
    for _ in 0..column_count {
        let len = read_u32(&mut input)? as usize;
        if len > 1_048_576 {
            return Err("object cache column name is too long".into());
        }
        let mut bytes = vec![0; len];
        input.read_exact(&mut bytes).map_err(|e| e.to_string())?;
        names.push(String::from_utf8(bytes).map_err(|e| e.to_string())?);
    }
    let mut positions = Vec::with_capacity(rows);
    for _ in 0..rows {
        positions.push([
            read_f64(&mut input)?,
            read_f64(&mut input)?,
            read_f64(&mut input)?,
        ]);
    }
    let mut columns = Vec::with_capacity(column_count);
    for column_name in names {
        let mut values = Vec::with_capacity(rows);
        for _ in 0..rows {
            values.push(read_f64(&mut input)?);
        }
        columns.push((column_name, ColumnValues::Number(values)));
    }
    let origin = [read_f64(&mut input)?, read_f64(&mut input)?];
    let cell = [read_f64(&mut input)?, read_f64(&mut input)?];
    let shape = [
        read_u32(&mut input)? as usize,
        read_u32(&mut input)? as usize,
    ];
    let expected_offsets = shape[0]
        .checked_mul(shape[1])
        .and_then(|v| v.checked_add(1))
        .ok_or("object cache grid is too large")?;
    let offset_count = read_u32(&mut input)? as usize;
    if offset_count != expected_offsets || shape.contains(&0) {
        return Err("object cache grid shape is invalid".into());
    }
    let mut offsets = Vec::with_capacity(offset_count);
    for _ in 0..offset_count {
        offsets.push(read_u32(&mut input)?);
    }
    let mut indexed_rows = Vec::with_capacity(rows);
    for _ in 0..rows {
        indexed_rows.push(read_u32(&mut input)?);
    }
    if offsets.last().copied() != Some(rows as u32)
        || indexed_rows.iter().any(|row| *row as usize >= rows)
    {
        return Err("object cache index is invalid".into());
    }
    Ok(ObjectTable {
        name,
        positions,
        has_z,
        columns,
        grid: ObjectGrid {
            origin,
            cell,
            shape,
            offsets,
            rows: indexed_rows,
        },
    })
}

fn write_u32(out: &mut impl Write, value: u32) -> Result<(), String> {
    out.write_all(&value.to_le_bytes())
        .map_err(|e| e.to_string())
}
fn write_u64(out: &mut impl Write, value: u64) -> Result<(), String> {
    out.write_all(&value.to_le_bytes())
        .map_err(|e| e.to_string())
}
fn write_f64(out: &mut impl Write, value: f64) -> Result<(), String> {
    out.write_all(&value.to_le_bytes())
        .map_err(|e| e.to_string())
}
fn read_u32(input: &mut impl Read) -> Result<u32, String> {
    let mut bytes = [0; 4];
    input.read_exact(&mut bytes).map_err(|e| e.to_string())?;
    Ok(u32::from_le_bytes(bytes))
}
fn read_u64(input: &mut impl Read) -> Result<u64, String> {
    let mut bytes = [0; 8];
    input.read_exact(&mut bytes).map_err(|e| e.to_string())?;
    Ok(u64::from_le_bytes(bytes))
}
fn read_f64(input: &mut impl Read) -> Result<f64, String> {
    Ok(f64::from_bits(read_u64(input)?))
}

fn read_measurement_csv(
    path: &Path,
    name: String,
    group: &Path,
) -> Result<MeasurementTable, String> {
    read_measurement_bytes(&fs::read(path).map_err(|e| e.to_string())?, name, group)
}
fn read_measurement_parquet(
    path: &Path,
    name: String,
    group: &Path,
) -> Result<MeasurementTable, String> {
    read_measurement_bytes(&parquet_csv_bytes(path)?, name, group)
}
fn read_measurement_bytes(
    bytes: &[u8],
    name: String,
    group: &Path,
) -> Result<MeasurementTable, String> {
    let mut reader = csv::Reader::from_reader(bytes);
    let headers = reader
        .headers()
        .map_err(|e| e.to_string())?
        .iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let key = headers
        .iter()
        .position(|h| {
            ["label", "label_id", "instance_id", "id"]
                .iter()
                .any(|n| h.eq_ignore_ascii_case(n))
        })
        .ok_or("measurement table has no label ID column")?;
    let mut ids = Vec::new();
    let mut raw = vec![Vec::<Option<f64>>::new(); headers.len()];
    for record in reader.records() {
        let record = record.map_err(|e| e.to_string())?;
        let Some(id) = record.get(key).and_then(|v| v.trim().parse::<u64>().ok()) else {
            continue;
        };
        ids.push(id);
        for (i, column) in raw.iter_mut().enumerate() {
            column.push(record.get(i).and_then(|v| v.trim().parse().ok()))
        }
    }
    let columns = headers
        .into_iter()
        .enumerate()
        .filter(|(i, _)| *i != key && raw[*i].iter().any(Option::is_some))
        .map(|(i, name)| (name, raw[i].clone()))
        .collect();
    let region = table_region(group);
    let rows_by_id = ids
        .iter()
        .copied()
        .enumerate()
        .map(|(row, id)| (id, row))
        .collect();
    Ok(MeasurementTable {
        name,
        region,
        ids,
        rows_by_id,
        columns,
    })
}
fn table_region(group: &Path) -> Option<String> {
    let path = if group.join("zarr.json").is_file() {
        group.join("zarr.json")
    } else {
        group.join(".zattrs")
    };
    let bytes = fs::read(path).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let attrs = json.get("attributes").unwrap_or(&json);
    let path = attrs
        .pointer("/region/path")
        .or_else(|| attrs.get("region"))
        .and_then(|v| v.as_str())?;
    Path::new(path)
        .file_name()
        .map(|v| v.to_string_lossy().into_owned())
}

fn read_parquet_table(path: &Path, name: String) -> Result<ObjectTable, String> {
    read_csv_bytes(&parquet_csv_bytes(path)?, name)
}
fn parquet_csv_bytes(path: &Path) -> Result<Vec<u8>, String> {
    use parquet::{
        file::reader::{FileReader, SerializedFileReader},
        record::Field,
    };
    let bytes = bytes::Bytes::from(fs::read(path).map_err(|e| e.to_string())?);
    let reader = SerializedFileReader::new(bytes).map_err(|e| e.to_string())?;
    let mut rows = reader.get_row_iter(None).map_err(|e| e.to_string())?;
    let Some(first) = rows.next() else {
        return Err("empty Parquet table".into());
    };
    let first = first.map_err(|e| e.to_string())?;
    let headers = first
        .get_column_iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    let mut writer = csv::Writer::from_writer(Vec::new());
    writer.write_record(&headers).map_err(|e| e.to_string())?;
    let text = |field: &Field| match field {
        Field::Null => String::new(),
        Field::Str(v) => v.clone(),
        other => other.to_string(),
    };
    writer
        .write_record(first.get_column_iter().map(|(_, v)| text(v)))
        .map_err(|e| e.to_string())?;
    for row in rows {
        let row = row.map_err(|e| e.to_string())?;
        writer
            .write_record(row.get_column_iter().map(|(_, v)| text(v)))
            .map_err(|e| e.to_string())?
    }
    writer.into_inner().map_err(|e| e.to_string())
}

fn read_csv_bytes(bytes: &[u8], name: String) -> Result<ObjectTable, String> {
    let mut reader = csv::Reader::from_reader(bytes);
    let headers = reader
        .headers()
        .map_err(|e| e.to_string())?
        .iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let find = |names: &[&str]| {
        headers
            .iter()
            .position(|h| names.iter().any(|n| h.eq_ignore_ascii_case(n)))
    };
    let xi = find(&["x", "centroid_x", "pos_x", "x_um", "x_micrometer", "col"]);
    let yi = find(&["y", "centroid_y", "pos_y", "y_um", "y_micrometer", "row"]);
    let zi = find(&[
        "z",
        "centroid_z",
        "pos_z",
        "z_um",
        "z_micrometer",
        "slice",
        "plane",
    ]);
    let xmin = find(&["x_min", "x0"]);
    let xmax = find(&["x_max", "x1"]);
    let ymin = find(&["y_min", "y0"]);
    let ymax = find(&["y_max", "y1"]);
    if (xi.is_none() || yi.is_none())
        && (xmin.is_none() || xmax.is_none() || ymin.is_none() || ymax.is_none())
    {
        return Err("table has no x/y positions".into());
    }
    enum Builder {
        Number(Vec<f64>),
        Text(Vec<String>),
    }
    let mut builders = (0..headers.len())
        .map(|_| Builder::Number(Vec::new()))
        .collect::<Vec<_>>();
    let mut positions = Vec::new();
    for record in reader.records() {
        let r = record.map_err(|e| e.to_string())?;
        let num = |i: usize| r.get(i)?.trim().parse::<f64>().ok();
        let x = xi
            .and_then(num)
            .or_else(|| Some((num(xmin?)? + num(xmax?)?) * 0.5));
        let y = yi
            .and_then(num)
            .or_else(|| Some((num(ymin?)? + num(ymax?)?) * 0.5));
        if let (Some(x), Some(y)) = (x, y) {
            positions.push([zi.and_then(num).unwrap_or(0.), y, x]);
            for (index, builder) in builders.iter_mut().enumerate() {
                let text = r.get(index).unwrap_or("").trim();
                match builder {
                    Builder::Number(values) => match text.parse::<f64>() {
                        Ok(value) => values.push(value),
                        Err(_) if text.is_empty() => values.push(f64::NAN),
                        Err(_) => {
                            let mut strings = values
                                .iter()
                                .map(|v| {
                                    if v.is_finite() {
                                        v.to_string()
                                    } else {
                                        String::new()
                                    }
                                })
                                .collect::<Vec<_>>();
                            strings.push(text.to_owned());
                            *builder = Builder::Text(strings)
                        }
                    },
                    Builder::Text(values) => values.push(text.to_owned()),
                }
            }
        }
    }
    let skip = [xi, yi, zi, xmin, xmax, ymin, ymax];
    let columns = headers
        .iter()
        .enumerate()
        .filter(|(i, _)| !skip.contains(&Some(*i)))
        .map(|(i, h)| {
            let values = match std::mem::replace(&mut builders[i], Builder::Number(Vec::new())) {
                Builder::Number(values) => ColumnValues::Number(values),
                Builder::Text(values) => ColumnValues::Text(values),
            };
            (h.clone(), values)
        })
        .collect();
    let grid = ObjectGrid::build(&positions);
    Ok(ObjectTable {
        name,
        positions,
        has_z: zi.is_some(),
        columns,
        grid,
    })
}
fn read_atlas(path: &Path, out: &mut HashMap<u64, AtlasEntry>) -> Result<(), String> {
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(id) = v
            .get("id")
            .or_else(|| v.get("structure_id"))
            .and_then(|n| n.as_u64().or_else(|| n.as_f64().map(|x| x as u64)))
        else {
            continue;
        };
        out.insert(
            id,
            AtlasEntry {
                name: v
                    .get("name")
                    .or_else(|| v.get("safe_name"))
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
                acronym: v.get("acronym").and_then(|v| v.as_str()).map(str::to_owned),
            },
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let label = root.path().join("labels/nuclei");
        fs::create_dir_all(label.join("0")).unwrap();
        fs::create_dir_all(root.path().join("tables/cells")).unwrap();
        fs::create_dir_all(root.path().join("tables/nucleus_features")).unwrap();
        fs::write(label.join(".zattrs"),r#"{"multiscales":[{"axes":[{"name":"z"},{"name":"y"},{"name":"x"}],"datasets":[{"path":"0"}]}],"image-label":{"colors":[[16777217,10,20,30,255]]}}"#).unwrap();
        fs::write(label.join("0/.zarray"),r#"{"zarr_format":2,"shape":[1,2,3],"chunks":[1,2,3],"dtype":"<u4","compressor":null,"fill_value":0,"order":"C","filters":null}"#).unwrap();
        let ids = [0u32, 16_777_217, 2, 16_777_217, 2, 2];
        let bytes = ids
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>();
        fs::write(label.join("0/0.0.0"), bytes).unwrap();
        fs::write(
            root.path().join("tables/cells/table.csv"),
            "id,x,y,score,class\n7,1,0,0.25,A\n8,2,1,0.9,B\n9,0,1,0.5,A\n",
        )
        .unwrap();
        fs::write(
            root.path().join("tables/nucleus_features/.zattrs"),
            r#"{"type":"feature_table","region":{"path":"../labels/nuclei"}}"#,
        )
        .unwrap();
        fs::write(
            root.path().join("tables/nucleus_features/table.csv"),
            "label_id,area\n16777217,42\n2,9\n",
        )
        .unwrap();
        fs::write(root.path().join("atlas.jsonl"),"{\"id\":16777217,\"name\":\"Exact region\",\"acronym\":\"EX\"}\n{\"id\":2,\"name\":\"Other\"}\n").unwrap();
        root
    }

    #[test]
    fn labels_objects_and_atlas_share_exact_coordinates_and_ids() {
        let root = fixture();
        let store = FeatureStore::default();
        let manifest = store.manifest(root.path()).unwrap();
        assert_eq!(manifest.labels[0].shape_xyz, [3, 2, 1]);
        assert_eq!(manifest.objects[0].count, 3);
        assert_eq!(manifest.tables[0].region.as_deref(), Some("nuclei"));
        assert_eq!(manifest.tables[0].columns[0].range, Some([9.0, 42.0]));
        assert!(manifest.labels[0].has_color_table);
        let picked = store.inspect_label(root.path(), "nuclei", 1, 0, 0).unwrap();
        assert_eq!(
            picked.id, 16_777_217,
            "u32 label IDs must not pass through f32"
        );
        assert_eq!(picked.name.as_deref(), Some("Exact region"));
        let queried = store
            .query_objects(root.path(), "cells", [0.5, 2.5, -0.5, 1.5, -1.0, 1.0], 10)
            .unwrap();
        assert_eq!(queried.total, 2);
        let dense = store
            .query_objects(root.path(), "cells", [0.5, 2.5, -0.5, 1.5, -1.0, 1.0], 1)
            .unwrap();
        assert!(dense.too_dense);
        assert!(dense.points.is_empty());
        assert_eq!(queried.points[0].values[1], Some(0.25));
        let row = store.inspect_object(root.path(), "cells", 0).unwrap();
        assert_eq!(
            row.pointer("/columns/class").and_then(|v| v.as_str()),
            Some("A")
        );
        let counts = store.region_counts(root.path(), "nuclei", "cells").unwrap();
        assert_eq!(
            counts.iter().find(|v| v.id == 16_777_217).map(|v| v.count),
            Some(2)
        );
        assert_eq!(counts.iter().find(|v| v.id == 2).map(|v| v.count), Some(1));
        let png = store
            .label_tile(
                root.path(),
                "nuclei",
                0,
                0,
                0,
                512,
                true,
                None,
                1.0,
                0,
                None,
            )
            .unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn numeric_csv_uses_a_validated_columnar_cache() {
        let root = tempfile::tempdir().unwrap();
        let csv = root.path().join("table.csv");
        fs::write(
            &csv,
            "label_id,centroid_y,centroid_x,area\n1,10,20,4.5\n2,11,21,8.5\n",
        )
        .unwrap();
        let first = read_csv_table(&csv, "cells".into()).unwrap();
        let cache = csv.with_extension("rozt-object-cache");
        assert!(cache.is_file());
        let cached = read_object_cache(&cache, &csv, "cells".into()).unwrap();
        assert_eq!(cached.positions, first.positions);
        assert_eq!(cached.grid.offsets, first.grid.offsets);
        assert_eq!(cached.grid.rows, first.grid.rows);
        assert_eq!(cached.summary().columns[1].range, Some([4.5, 8.5]));

        fs::write(&csv, "label_id,centroid_y,centroid_x,area\n1,10,20,4.5\n").unwrap();
        assert!(read_object_cache(&cache, &csv, "cells".into()).is_err());
        assert_eq!(
            read_csv_table(&csv, "cells".into())
                .unwrap()
                .positions
                .len(),
            1
        );
    }
}
