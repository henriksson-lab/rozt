//! QuPath GeoJSON reader and writer ported from `omezarr_viewers-rs`.

mod read;
mod write;

pub use read::parse;
pub use write::write;

const Z_EXTENT: &str = "zExtent";
const T_EXTENT: &str = "tExtent";
const STROKE_WIDTH: &str = "strokeWidth";
const DENSE_REGION: &str = "denseRegion";

#[cfg(test)]
mod tests {
    use crate::qupath::{Annotation, Geometry, Plane};

    #[test]
    fn qupath_geojson_round_trips_holes_classes_and_plane() {
        let mut annotation = Annotation {
            id: 7,
            geometry: Geometry::Polygon(vec![
                vec![
                    [0.0, 0.0],
                    [10.0, 0.0],
                    [10.0, 10.0],
                    [0.0, 10.0],
                    [0.0, 0.0],
                ],
                vec![[2.0, 2.0], [2.0, 4.0], [4.0, 4.0], [2.0, 2.0]],
            ]),
            plane: Plane::at(3, 0),
            label: "cell: mitotic".into(),
            ..Annotation::default()
        };
        annotation.uuid = Some("1234".into());
        annotation.measurements.insert("area".into(), 96.0);
        let written = super::write(&[annotation.clone()]).unwrap();
        let read = super::parse(&written).unwrap();
        assert_eq!(read.len(), 1);
        let mut expected = annotation;
        expected.id = 0;
        assert_eq!(read[0], expected);
    }
}
