//! Read ngio's AnnData ROI layout: numeric geometry in X and classes in obs.

use std::{path::Path, sync::Arc};

use zarrs::{array::{Array, DataType}, array_subset::ArraySubset, filesystem::FilesystemStore, group::Group};

use super::RoiRow;

pub(super) fn roi_rows_from_anndata(root: &Path, name: &str) -> Result<Vec<RoiRow>, String> {
    let store = Arc::new(FilesystemStore::new(root).map_err(|error| error.to_string())?);
    let prefix = format!("/tables/{name}");
    let x_path = format!("{prefix}/X");
    let x = Array::open(store.clone(), &x_path).map_err(|error| error.to_string())?;
    let shape = x.shape();
    if shape.len() != 2 { return Err("AnnData X must have two dimensions".into()); }
    let (count, width) = (shape[0] as usize, shape[1] as usize);
    if count > 100_000 { return Err("ROI table exceeds 100,000 objects".into()); }
    let names = read_strings(&store, &format!("{prefix}/var/_index"))?;
    if names.len() != width { return Err("AnnData var/_index width does not match X".into()); }
    let values = read_numbers(&store, &x_path)?;
    if values.len() != count.saturating_mul(width) { return Err("AnnData X length does not match its shape".into()); }
    let mut rows = (0..count).map(|index| {
        names.iter().enumerate().map(|(column, name)| (name.clone(), values[index * width + column].to_string())).collect::<RoiRow>()
    }).collect::<Vec<_>>();

    if !names.iter().any(|name| name == "x_micrometer") {
        let spatial_path = format!("{prefix}/obsm/spatial");
        if let Ok(spatial) = Array::open(store.clone(), &spatial_path) {
            let shape = spatial.shape();
            if shape.len() == 2 && shape[0] as usize == count && (shape[1] == 2 || shape[1] == 3) {
                let width = shape[1] as usize;
                let values = read_numbers(&store, &spatial_path)?;
                for (index, row) in rows.iter_mut().enumerate() {
                    row.insert("__voxel_x".into(), values[index * width].to_string());
                    row.insert("__voxel_y".into(), values[index * width + 1].to_string());
                    row.insert("__voxel_z".into(), if width == 3 { values[index * width + 2].to_string() } else { "0".into() });
                }
            }
        }
    }

    // Categorical class columns are common in ngio AnnData tables.
    if let Ok(obs) = Group::open(store.clone(), &format!("{prefix}/obs")) {
        let mut columns = obs.attributes().get("column-order")
            .and_then(|value| serde_json::from_value::<Vec<String>>(value.clone()).ok())
            .unwrap_or_default();
        if columns.is_empty() {
            columns = obs.child_paths(false).unwrap_or_default().iter()
                .chain(obs.child_group_paths(false).unwrap_or_default().iter())
                .filter_map(|path| path.as_str().rsplit('/').next().map(str::to_string))
                .filter(|name| name != "_index").collect();
            columns.sort(); columns.dedup();
        }
        for column in columns {
            let path = format!("{prefix}/obs/{column}");
            let decoded = read_categorical(&store, &path).or_else(|_| read_strings(&store, &path))
                .or_else(|_| read_numbers(&store, &path).map(|values| values.iter().map(ToString::to_string).collect()));
            if let Ok(values) = decoded {
                if values.len() == count {
                    for (row, value) in rows.iter_mut().zip(values) { row.insert(column.clone(), value); }
                }
            }
        }
    }
    Ok(rows)
}

fn read_strings(store: &Arc<FilesystemStore>, path: &str) -> Result<Vec<String>, String> {
    let array = Array::open(store.clone(), path).map_err(|error| error.to_string())?;
    array.retrieve_array_subset_elements::<String>(&ArraySubset::new_with_shape(array.shape().to_vec()))
        .map_err(|error| error.to_string())
}

fn read_numbers(store: &Arc<FilesystemStore>, path: &str) -> Result<Vec<f64>, String> {
    let array = Array::open(store.clone(), path).map_err(|error| error.to_string())?;
    let subset = ArraySubset::new_with_shape(array.shape().to_vec());
    macro_rules! widen {
        ($type:ty) => { array.retrieve_array_subset_elements::<$type>(&subset)
            .map(|values| values.into_iter().map(|value| value as f64).collect())
            .map_err(|error| error.to_string()) };
    }
    match array.data_type() {
        DataType::Float64 => widen!(f64), DataType::Float32 => widen!(f32),
        DataType::Int8 => widen!(i8), DataType::Int16 => widen!(i16),
        DataType::Int32 => widen!(i32), DataType::Int64 => widen!(i64),
        DataType::UInt8 => widen!(u8), DataType::UInt16 => widen!(u16),
        DataType::UInt32 => widen!(u32), DataType::UInt64 => widen!(u64),
        DataType::Bool => array.retrieve_array_subset_elements::<bool>(&subset)
            .map(|values| values.into_iter().map(f64::from).collect()).map_err(|error| error.to_string()),
        other => Err(format!("AnnData {path} holds {other}, expected numbers")),
    }
}

fn read_categorical(store: &Arc<FilesystemStore>, path: &str) -> Result<Vec<String>, String> {
    let categories = read_strings(store, &format!("{path}/categories"))?;
    let codes = read_numbers(store, &format!("{path}/codes"))?;
    Ok(codes.into_iter().map(|code| {
        if code.is_finite() && code >= 0.0 && code.fract() == 0.0 {
            categories.get(code as usize).cloned().unwrap_or_default()
        } else { String::new() }
    }).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zarrs::{array::{ArrayBuilder, FillValue}, group::GroupBuilder};

    #[test]
    fn reads_numeric_x_and_categorical_obs() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(FilesystemStore::new(root.path()).unwrap());
        for path in ["/", "/tables", "/tables/foreign", "/tables/foreign/var", "/tables/foreign/obs", "/tables/foreign/obs/class"] {
            let mut builder = GroupBuilder::new();
            if path == "/tables/foreign/obs" { builder.attributes(serde_json::json!({"column-order":["class"]}).as_object().unwrap().clone()); }
            builder.build(store.clone(), path).unwrap().store_metadata().unwrap();
        }
        let numbers = |path: &str, shape: Vec<u64>, values: &[f64]| {
            let array = ArrayBuilder::new(shape.clone(), DataType::Float64, shape.clone().try_into().unwrap(), FillValue::from(0.0f64))
                .build(store.clone(), path).unwrap();
            array.store_metadata().unwrap();
            array.store_array_subset_elements::<f64>(&ArraySubset::new_with_shape(shape), values).unwrap();
        };
        let strings = |path: &str, values: &[&str]| {
            let shape = vec![values.len() as u64];
            let array = ArrayBuilder::new(shape.clone(), DataType::String, shape.clone().try_into().unwrap(), FillValue::from(""))
                .build(store.clone(), path).unwrap();
            array.store_metadata().unwrap();
            array.store_array_subset_elements::<String>(&ArraySubset::new_with_shape(shape), &values.iter().map(|v| v.to_string()).collect::<Vec<_>>()).unwrap();
        };
        let names = ["x_micrometer", "y_micrometer", "z_micrometer", "len_x_micrometer", "len_y_micrometer", "len_z_micrometer"];
        numbers("/tables/foreign/X", vec![1, 6], &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        strings("/tables/foreign/var/_index", &names);
        strings("/tables/foreign/obs/class/categories", &["cell", "spot"]);
        let codes = ArrayBuilder::new(vec![1], DataType::Int8, vec![1].try_into().unwrap(), FillValue::from(-1i8))
            .build(store.clone(), "/tables/foreign/obs/class/codes").unwrap();
        codes.store_metadata().unwrap();
        codes.store_array_subset_elements::<i8>(&ArraySubset::new_with_shape(vec![1]), &[1]).unwrap();
        let rows = roi_rows_from_anndata(root.path(), "foreign").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["x_micrometer"], "1");
        assert_eq!(rows[0]["class"], "spot");
    }
}
