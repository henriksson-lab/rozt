//! Annotations: hand-drawn geometry, in QuPath's model.
//!
//! # Why this shape
//!
//! OME-Zarr specifies no vector annotation at all (`info_roi.md`), so the
//! question is whose model to borrow. `info_annotation_formats.md` works it
//! through; the answer is **QuPath's GeoJSON dialect**, because it is what the
//! tool we want to replace reads and writes, because its coordinate system is
//! already ours — full-resolution pixels, origin top-left, y down — and because
//! OME-XML's ROI model cannot express a polygon with a hole at all, its only
//! composition operator being `Union`.
//!
//! So [`Geometry`] is RFC 7946's geometry set, serialising as GeoJSON verbatim,
//! and [`Annotation`] carries QuPath's per-object properties beside it.
//!
//! # What we add, and why it is written down
//!
//! QuPath and OME-XML both say **one plane per shape**: a 3D region is several
//! shapes. This viewer allows a shape to span a *range* of z planes
//! ([`Annotation::z_extent`]), which is more expressive than either, because a
//! box drawn through a stack is the common case here and one row per plane is a
//! poor way to say it. That is a deviation, and the group attributes record it
//! so a reader is told rather than left to infer.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A world-pixel coordinate pair, `[x, y]`.
///
/// x and y only: z and t are properties of the *plane* an annotation sits on,
/// which is how both QuPath and OME-XML model them, and mixing them into the
/// coordinate list is what makes a geometry stop being GeoJSON.
pub type Point = [f64; 2];

/// One closed ring of a polygon.
pub type Ring = Vec<Point>;

/// Stable class colour from the source viewer. Class order never changes its colour.
pub fn class_color(class: &str) -> [u8; 3] {
    let mut hash: u32 = 2_166_136_261;
    for byte in class.as_bytes() {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(16_777_619);
    }
    let h = ((hash >> 8) & 1023) as f32 / 1023.0;
    let s = 0.55 + ((hash >> 18) & 63) as f32 / 63.0 * 0.35;
    let v = 0.75 + ((hash >> 24) & 63) as f32 / 63.0 * 0.25;
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - f * s), v * (1.0 - (1.0 - f) * s));
    let rgb = match (i as i32).rem_euclid(6) {
        0 => [v,t,p], 1 => [q,v,p], 2 => [p,v,t],
        3 => [p,q,v], 4 => [t,p,v], _ => [v,p,q],
    };
    rgb.map(|channel| (channel * 255.0).round() as u8)
}

/// The geometry of one annotation, in world pixels.
///
/// Serialises as an RFC 7946 geometry object — `{"type": …, "coordinates": …}`
/// — so the wire form between server and client is already the on-disk form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "coordinates")]
pub enum Geometry {
    Point(Point),
    MultiPoint(Vec<Point>),
    LineString(Vec<Point>),
    MultiLineString(Vec<Vec<Point>>),
    /// Ring 0 is the exterior; every further ring is a hole. This is the one
    /// thing OME-XML cannot represent, and the reason it was not chosen.
    Polygon(Vec<Ring>),
    MultiPolygon(Vec<Vec<Ring>>),
}

impl Geometry {
    /// An axis-aligned rectangle, as the four corners of a closed ring.
    pub fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        let (x0, x1) = (x0.min(x1), x0.max(x1));
        let (y0, y1) = (y0.min(y1), y0.max(y1));
        Geometry::Polygon(vec![vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1], [x0, y0]]])
    }

    /// `[x0, y0, x1, y1]`, or `None` for a geometry with no coordinates.
    pub fn bounds(&self) -> Option<[f64; 4]> {
        let mut lo = [f64::INFINITY; 2];
        let mut hi = [f64::NEG_INFINITY; 2];
        self.for_each_point(&mut |p| {
            for axis in 0..2 {
                lo[axis] = lo[axis].min(p[axis]);
                hi[axis] = hi[axis].max(p[axis]);
            }
        });
        lo[0].is_finite().then_some([lo[0], lo[1], hi[0], hi[1]])
    }

    /// Every coordinate, whatever the shape.
    pub fn for_each_point(&self, visit: &mut impl FnMut(Point)) {
        match self {
            Geometry::Point(p) => visit(*p),
            Geometry::MultiPoint(points) | Geometry::LineString(points) => {
                points.iter().copied().for_each(&mut *visit)
            }
            Geometry::MultiLineString(lines) => {
                lines.iter().flatten().copied().for_each(&mut *visit)
            }
            Geometry::Polygon(rings) => rings.iter().flatten().copied().for_each(&mut *visit),
            Geometry::MultiPolygon(polygons) => polygons
                .iter()
                .flatten()
                .flatten()
                .copied()
                .for_each(&mut *visit),
        }
    }

    /// Every polygon in this geometry, as its rings. Empty for points and lines.
    pub fn polygons(&self) -> Vec<&Vec<Ring>> {
        match self {
            Geometry::Polygon(rings) => vec![rings],
            Geometry::MultiPolygon(polygons) => polygons.iter().collect(),
            _ => Vec::new(),
        }
    }

    /// Every open path to stroke: polygon rings and line strings alike.
    ///
    /// Rings come back closed — the last point repeats the first — because that
    /// is what draws a complete outline, and a `LineString` does not, because
    /// closing one would draw an edge nobody asked for.
    pub fn outlines(&self) -> Vec<Vec<Point>> {
        match self {
            Geometry::Point(_) | Geometry::MultiPoint(_) => Vec::new(),
            Geometry::LineString(points) => vec![points.clone()],
            Geometry::MultiLineString(lines) => lines.clone(),
            Geometry::Polygon(rings) => rings.iter().map(|r| closed(r)).collect(),
            Geometry::MultiPolygon(polygons) => polygons
                .iter()
                .flat_map(|rings| rings.iter().map(|r| closed(r)))
                .collect(),
        }
    }

    /// The points to draw as sprites: the point geometries, and nothing else.
    pub fn markers(&self) -> Vec<Point> {
        match self {
            Geometry::Point(p) => vec![*p],
            Geometry::MultiPoint(points) => points.clone(),
            _ => Vec::new(),
        }
    }

    /// Is this a geometry with no area — a point set or an open path?
    pub fn is_puncta(&self) -> bool {
        matches!(self, Geometry::Point(_) | Geometry::MultiPoint(_))
    }

    /// Total unsigned area, by the shoelace formula, holes subtracted.
    ///
    /// Used to order overlapping hits: the smallest thing containing a click is
    /// the one meant. Zero for points and lines, which therefore always win.
    pub fn area(&self) -> f64 {
        self.polygons()
            .iter()
            .map(|rings| {
                rings
                    .iter()
                    .enumerate()
                    .map(|(index, ring)| {
                        let area = ring_area(ring).abs();
                        if index == 0 {
                            area
                        } else {
                            -area
                        }
                    })
                    .sum::<f64>()
                    .max(0.0)
            })
            .sum()
    }

    /// Is `(x, y)` inside, or within `pad` world pixels of, this geometry?
    ///
    /// The padding is what makes a point or a line clickable at all: neither
    /// has any area, and a click is never exactly on a coordinate.
    pub fn contains(&self, x: f64, y: f64, pad: f64) -> bool {
        // Anything within the pad of a drawn coordinate or edge counts as a hit,
        // whatever the shape — so the border of a filled region is grabbable
        // from either side.
        let mut near = false;
        self.for_each_point(&mut |p| {
            near |= (p[0] - x).abs() <= pad && (p[1] - y).abs() <= pad;
        });
        if near {
            return true;
        }
        for path in self.outlines() {
            for segment in path.windows(2) {
                if distance_to_segment(x, y, segment[0], segment[1]) <= pad {
                    return true;
                }
            }
        }
        self.polygons().iter().any(|rings| inside(rings, x, y))
    }

    /// Move every coordinate by `(dx, dy)`.
    pub fn translate(&mut self, dx: f64, dy: f64) {
        self.map_points(&mut |p| [p[0] + dx, p[1] + dy]);
    }

    /// Scale about `(ox, oy)` — what a corner drag does to a whole shape.
    ///
    /// A scale of exactly 1 is left alone rather than computed: `ox + (p - ox)`
    /// is not always `p` in floating point, and a resize that moved nothing
    /// should move nothing.
    pub fn scale_about(&mut self, ox: f64, oy: f64, sx: f64, sy: f64) {
        if sx == 1.0 && sy == 1.0 {
            return;
        }
        self.map_points(&mut |p| {
            [
                if sx == 1.0 {
                    p[0]
                } else {
                    ox + (p[0] - ox) * sx
                },
                if sy == 1.0 {
                    p[1]
                } else {
                    oy + (p[1] - oy) * sy
                },
            ]
        });
    }

    /// Every editable path — polygon rings and line strings — in the order
    /// [`Geometry::outlines`] produces them, but *open*: the repeated closing
    /// vertex of a ring is not a separate thing to edit.
    ///
    /// Editing addresses a vertex as `(path, index)` into this, so the canvas's
    /// handles and the edit that follows are numbered the same way.
    pub fn paths_mut(&mut self) -> Vec<&mut Vec<Point>> {
        match self {
            Geometry::Point(_) | Geometry::MultiPoint(_) => Vec::new(),
            Geometry::LineString(points) => vec![points],
            Geometry::MultiLineString(lines) => lines.iter_mut().collect(),
            Geometry::Polygon(rings) => rings.iter_mut().collect(),
            Geometry::MultiPolygon(polygons) => polygons.iter_mut().flatten().collect(),
        }
    }

    /// Is this path a closed ring — its last vertex repeating its first?
    fn is_ring(path: &[Point]) -> bool {
        path.len() > 1 && path.first() == path.last()
    }

    /// Move one vertex by `(dx, dy)`.
    ///
    /// A closed ring repeats its first vertex last; moving one end moves the
    /// other, or the ring springs open at the seam.
    pub fn move_vertex(&mut self, path: usize, vertex: usize, dx: f64, dy: f64) -> bool {
        let Some(ring) = self.paths_mut().into_iter().nth(path) else {
            return false;
        };
        let Some(point) = ring.get(vertex).copied() else {
            return false;
        };
        // Asked *before* the move: once one end has shifted the ring no longer
        // looks closed, and the seam would be left open.
        let closed = Geometry::is_ring(ring);
        let last = ring.len() - 1;
        let moved = [point[0] + dx, point[1] + dy];
        ring[vertex] = moved;
        if closed && (vertex == 0 || vertex == last) {
            ring[0] = moved;
            ring[last] = moved;
        }
        true
    }

    /// Add a vertex at `at`, just after `vertex` on `path`.
    pub fn insert_vertex(&mut self, path: usize, vertex: usize, at: Point) -> bool {
        let Some(ring) = self.paths_mut().into_iter().nth(path) else {
            return false;
        };
        if vertex >= ring.len() {
            return false;
        }
        ring.insert(vertex + 1, at);
        true
    }

    /// Remove one vertex, refusing to take a shape below the minimum that keeps
    /// it one: three corners for a ring, two ends for a line.
    pub fn remove_vertex(&mut self, path: usize, vertex: usize) -> bool {
        let Some(ring) = self.paths_mut().into_iter().nth(path) else {
            return false;
        };
        let closed = Geometry::is_ring(ring);
        let corners = if closed { ring.len() - 1 } else { ring.len() };
        if corners <= if closed { 3 } else { 2 } || vertex >= ring.len() {
            return false;
        }
        ring.remove(vertex);
        // The seam has to stay a seam: removing either end leaves the ring
        // ending on a vertex that is no longer its first.
        if closed {
            let last = ring.len() - 1;
            ring[last] = ring[0];
        }
        true
    }

    fn map_points(&mut self, f: &mut impl FnMut(Point) -> Point) {
        match self {
            Geometry::Point(p) => *p = f(*p),
            Geometry::MultiPoint(points) | Geometry::LineString(points) => {
                points.iter_mut().for_each(|p| *p = f(*p))
            }
            Geometry::MultiLineString(lines) => lines.iter_mut().flatten().for_each(|p| *p = f(*p)),
            Geometry::Polygon(rings) => rings.iter_mut().flatten().for_each(|p| *p = f(*p)),
            Geometry::MultiPolygon(polygons) => polygons
                .iter_mut()
                .flatten()
                .flatten()
                .for_each(|p| *p = f(*p)),
        }
    }
}

/// A ring with its first point repeated at the end, for stroking.
fn closed(ring: &[Point]) -> Vec<Point> {
    let mut out = ring.to_vec();
    match (out.first().copied(), out.last().copied()) {
        (Some(first), Some(last)) if first != last => out.push(first),
        _ => {}
    }
    out
}

fn ring_area(ring: &[Point]) -> f64 {
    let mut sum = 0.0;
    for i in 0..ring.len() {
        let a = ring[i];
        let b = ring[(i + 1) % ring.len()];
        sum += a[0] * b[1] - b[0] * a[1];
    }
    sum / 2.0
}

/// Even-odd point-in-polygon over every ring, so a hole excludes.
fn inside(rings: &[Ring], x: f64, y: f64) -> bool {
    let mut within = false;
    for ring in rings {
        let mut crossings = false;
        for i in 0..ring.len() {
            let a = ring[i];
            let b = ring[(i + 1) % ring.len()];
            if (a[1] > y) != (b[1] > y) {
                let at = (b[0] - a[0]) * (y - a[1]) / (b[1] - a[1]) + a[0];
                if x < at {
                    crossings = !crossings;
                }
            }
        }
        // Exterior and holes alike flip the answer, which is exactly the
        // even-odd rule: inside the outer ring and inside a hole is outside.
        within ^= crossings;
    }
    within
}

fn distance_to_segment(x: f64, y: f64, a: Point, b: Point) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let length = dx * dx + dy * dy;
    let t = if length > 0.0 {
        (((x - a[0]) * dx + (y - a[1]) * dy) / length).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (cx, cy) = (a[0] + t * dx, a[1] + t * dy);
    ((x - cx).powi(2) + (y - cy).powi(2)).sqrt()
}

/// Which plane an annotation sits on, as QuPath's `ImagePlane`.
///
/// `c = -1` means "every channel", which is QuPath's default and the same thing
/// OME-XML says by leaving `TheC` out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plane {
    pub c: i32,
    pub z: i32,
    pub t: i32,
}

impl Default for Plane {
    fn default() -> Self {
        Plane { c: -1, z: 0, t: 0 }
    }
}

impl Plane {
    pub fn at(z: i32, t: i32) -> Self {
        Plane { c: -1, z, t }
    }

    pub fn is_default(&self) -> bool {
        *self == Plane::default()
    }
}

/// What kind of object this is, in QuPath's vocabulary.
///
/// Carried through even for the kinds this viewer does not itself create, so a
/// file that came from QuPath goes back unchanged rather than having every
/// detection turned into an annotation on the way through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ObjectType {
    #[default]
    Annotation,
    Detection,
    Cell,
    Tile,
    TmaCore,
    Root,
}

impl ObjectType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ObjectType::Annotation => "annotation",
            ObjectType::Detection => "detection",
            ObjectType::Cell => "cell",
            ObjectType::Tile => "tile",
            ObjectType::TmaCore => "tmaCore",
            ObjectType::Root => "root",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "detection" => ObjectType::Detection,
            "cell" => ObjectType::Cell,
            "tile" => ObjectType::Tile,
            "tmaCore" => ObjectType::TmaCore,
            "root" => ObjectType::Root,
            _ => ObjectType::Annotation,
        }
    }
}

/// One annotation: a geometry, the plane it is on, and what is known about it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    /// Assigned by the server, stable for the life of the layer.
    #[serde(default)]
    pub id: u64,
    pub geometry: Geometry,
    #[serde(default)]
    pub plane: Plane,
    /// How many *further* z planes this covers beyond `plane.z`. Zero is one
    /// plane, which is all QuPath and OME-XML can say. See the module docs.
    #[serde(default)]
    pub z_extent: u32,
    /// Likewise for time.
    #[serde(default)]
    pub t_extent: u32,
    /// The classification, as QuPath's `PathClass`: a derived class is written
    /// with `": "` between its parts, which is how QuPath itself renders one.
    #[serde(default)]
    pub label: String,
    /// The classification's colour, when the file gave one.
    #[serde(default)]
    pub class_color: Option<[u8; 3]>,
    /// The object's own name, which QuPath shows instead of the class.
    #[serde(default)]
    pub name: Option<String>,
    /// The object's own colour, overriding the class's.
    #[serde(default)]
    pub color: Option<[u8; 3]>,
    #[serde(default)]
    pub object_type: ObjectType,
    #[serde(default)]
    pub locked: bool,
    /// QuPath draws this as an ellipse; the geometry is its polygon
    /// approximation. Kept so a round trip through here does not flatten it.
    #[serde(default)]
    pub is_ellipse: bool,
    /// A cell object's *second* geometry: the nucleus inside the membrane.
    ///
    /// QuPath's cell segmentation produces both, and a file that has been
    /// through a viewer that only knew about one of them has lost half of every
    /// cell. Drawn as an inner outline, and written back where it came from.
    #[serde(default)]
    pub nucleus: Option<Geometry>,
    /// A TMA core the grid has but the slide does not.
    #[serde(default)]
    pub missing: bool,
    #[serde(default)]
    pub measurements: BTreeMap<String, f64>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
    /// The annotation this is a child of, when the file had a hierarchy.
    #[serde(default)]
    pub parent: Option<u64>,
    /// QuPath's own UUID, preserved so a round trip does not renumber objects.
    #[serde(default)]
    pub uuid: Option<String>,
    /// The width this shape's outline covers, in the world's own pixels.
    ///
    /// `None` is a *geometric* line — a mathematical curve of no area, which is
    /// what QuPath and GeoJSON mean by a `LineString`. `Some(w)` is a **stroke**:
    /// the region within `w / 2` of the path, which is what a brush produces and
    /// what a pixel classifier trains on.
    ///
    /// The distinction is the whole reason this is an `Option` rather than a
    /// width defaulting to zero. A scribble asserts something about the pixels
    /// it covers and nothing about any other pixel — that is *partial*
    /// supervision, and it is what a closed region cannot express, because a
    /// closed region implies background outside it. Storing the path and its
    /// width rather than a painted mask keeps the assertion resolution-free:
    /// whoever rasterises does so at the level they train at, instead of
    /// inheriting whatever zoom the curator happened to be at.
    #[serde(default)]
    pub stroke_width: Option<f64>,
    /// This shape asserts that **everything inside it is annotated**.
    ///
    /// Without such an assertion a trainer cannot interpret an uncovered pixel.
    /// Sparse is the safe default — a scribble says something about the pixels
    /// it covers and nothing about any other — but a model that never sees a
    /// negative learns nothing, so somewhere the curator has to be able to say
    /// "in *this* box I marked every one". Inside such a shape, uncovered means
    /// background; outside every one of them, uncovered means unexamined.
    ///
    /// A region rather than a per-layer flag because one image is routinely both:
    /// a few exhaustively-labelled crops, and casual scribbles elsewhere.
    #[serde(default)]
    pub dense_region: bool,
}

impl Default for Annotation {
    fn default() -> Self {
        Annotation {
            id: 0,
            geometry: Geometry::Point([0.0, 0.0]),
            plane: Plane::default(),
            z_extent: 0,
            t_extent: 0,
            label: String::new(),
            class_color: None,
            name: None,
            color: None,
            object_type: ObjectType::Annotation,
            locked: false,
            is_ellipse: false,
            nucleus: None,
            missing: false,
            measurements: BTreeMap::new(),
            metadata: BTreeMap::new(),
            parent: None,
            uuid: None,
            stroke_width: None,
            dense_region: false,
        }
    }
}

impl Annotation {
    /// A point at `(x, y)` on one plane.
    pub fn point(x: f64, y: f64, plane: Plane) -> Self {
        Annotation {
            geometry: Geometry::Point([x, y]),
            plane,
            ..Default::default()
        }
    }

    /// An axis-aligned rectangle on one plane.
    pub fn rect(x0: f64, y0: f64, x1: f64, y1: f64, plane: Plane) -> Self {
        Annotation {
            geometry: Geometry::rect(x0, y0, x1, y1),
            plane,
            ..Default::default()
        }
    }

    /// A geometry with no area — a marker rather than a region.
    pub fn is_point(&self) -> bool {
        self.geometry.is_puncta()
    }

    /// `[x0, y0, x1, y1]` in world pixels.
    pub fn bounds(&self) -> Option<[f64; 4]> {
        self.geometry.bounds()
    }

    /// The z planes this covers, inclusive.
    pub fn z_range(&self) -> (i32, i32) {
        (self.plane.z, self.plane.z + self.z_extent as i32)
    }

    /// Is this drawn at plane `z`, frame `t`?
    ///
    /// Exact rather than padded: a slice view interpolates between planes, but
    /// frame 4 of a series is a different picture, not a nearby one — and a
    /// shape's z span is something the user set, not something to fuzz.
    pub fn at_plane(&self, z: i32, t: i32) -> bool {
        let (z0, z1) = self.z_range();
        z >= z0 && z <= z1 && t >= self.plane.t && t <= self.plane.t + self.t_extent as i32
    }

    /// Is `(x, y)` inside, allowing `pad` world pixels of slack?
    pub fn contains(&self, x: f64, y: f64, pad: f64) -> bool {
        self.geometry.contains(x, y, pad)
    }

    /// The colour to draw this in: the object's own, then its class's.
    pub fn effective_color(&self) -> Option<[u8; 3]> {
        self.color.or(self.class_color)
    }

    /// What to show as this object's identity: its own name, else its class.
    ///
    /// The two are different things — a name identifies *this* object, a class
    /// says what kind it is — but a list needs one string, and a named object is
    /// named for a reason.
    pub fn display_name(&self) -> &str {
        match self.name.as_deref() {
            Some(name) if !name.is_empty() => name,
            _ => &self.label,
        }
    }
}

/// The annotation under a click: the smallest one that contains the point.
///
/// Smallest-first is what makes a shape drawn inside another selectable — with
/// a topmost-first rule the outer one would swallow every click in its area.
/// A point or a line has zero area and therefore always wins over a region that
/// surrounds it, which is the behaviour you want when marking cells inside a
/// drawn boundary.
///
/// Shared between server and client rather than being an endpoint, because the
/// client holds every row and should not pay a round trip to learn what it
/// already knows — and two implementations of "which shape did they mean" would
/// disagree the first time either changed.
pub fn pick_annotation(
    annotations: &[Annotation],
    x: f64,
    y: f64,
    pad: f64,
) -> Option<&Annotation> {
    annotations
        .iter()
        .filter(|item| item.contains(x, y, pad))
        .min_by(|a, b| a.geometry.area().total_cmp(&b.geometry.area()))
}

/// The annotation a new shape belongs *inside*: the smallest one that contains
/// it and is bigger than it.
///
/// This is QuPath's rule. Its hierarchy is spatial rather than something you
/// assemble by hand — an object goes under the smallest annotation that covers
/// it — which is why drawing a cell inside a region makes it a child of that
/// region without anybody saying so.
///
/// "Contains" is tested on every vertex, not on a centroid: a shape that pokes
/// out of a region is not inside it, and a crescent's centroid is not even in
/// itself.
pub fn containing_parent(annotations: &[Annotation], child: &Annotation) -> Option<u64> {
    let area = child.geometry.area();
    let mut best: Option<&Annotation> = None;
    for candidate in annotations {
        if candidate.id == child.id || candidate.geometry.area() <= area {
            continue;
        }
        // Only something drawn on the same plane can contain it; a region on
        // slice 3 does not enclose a cell on slice 40.
        if !candidate.at_plane(child.plane.z, child.plane.t) {
            continue;
        }
        let mut inside = true;
        child.geometry.for_each_point(&mut |p| {
            inside &= candidate.geometry.contains(p[0], p[1], 0.0);
        });
        if !inside {
            continue;
        }
        if best.is_none_or(|current| candidate.geometry.area() < current.geometry.area()) {
            best = Some(candidate);
        }
    }
    best.map(|parent| parent.id)
}

/// Every annotation in nesting order: each parent immediately followed by its
/// children, with the depth each one sits at.
///
/// Depth-first so a list reads as a tree, and iterative so a file that claims a
/// cycle — which a hand-edited one can — does not recurse forever. Anything
/// whose parent is missing or circular comes back at the top level rather than
/// vanishing.
pub fn in_tree_order(annotations: &[Annotation]) -> Vec<(&Annotation, usize)> {
    let mut out: Vec<(&Annotation, usize)> = Vec::with_capacity(annotations.len());
    let mut placed = vec![false; annotations.len()];

    fn children(annotations: &[Annotation], parent: u64) -> Vec<usize> {
        annotations
            .iter()
            .enumerate()
            .filter(|(_, a)| a.parent == Some(parent))
            .map(|(index, _)| index)
            .collect()
    }

    let known: Vec<u64> = annotations.iter().map(|a| a.id).collect();
    // Roots: no parent, or a parent nothing in the set answers to.
    let mut stack: Vec<(usize, usize)> = annotations
        .iter()
        .enumerate()
        .filter(|(_, a)| a.parent.is_none_or(|p| !known.contains(&p)))
        .map(|(index, _)| (index, 0))
        .rev()
        .collect();

    while let Some((index, depth)) = stack.pop() {
        if placed[index] {
            continue;
        }
        placed[index] = true;
        out.push((&annotations[index], depth));
        for child in children(annotations, annotations[index].id)
            .into_iter()
            .rev()
        {
            if !placed[child] {
                stack.push((child, depth + 1));
            }
        }
    }
    // A cycle leaves rows unplaced; they belong in the list all the same.
    for (index, annotation) in annotations.iter().enumerate() {
        if !placed[index] {
            out.push((annotation, 0));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_colours_are_stable_by_name() {
        assert_eq!(class_color("vessel"), class_color("vessel"));
        assert_ne!(class_color("vessel"), class_color("cell"));
    }

    fn square(x0: f64, y0: f64, size: f64) -> Vec<Point> {
        vec![
            [x0, y0],
            [x0 + size, y0],
            [x0 + size, y0 + size],
            [x0, y0 + size],
        ]
    }

    #[test]
    fn a_geometry_serialises_as_geojson() {
        let geometry = Geometry::Polygon(vec![square(0.0, 0.0, 10.0)]);
        let json = serde_json::to_value(&geometry).unwrap();
        assert_eq!(json["type"], "Polygon");
        assert_eq!(json["coordinates"][0][1], serde_json::json!([10.0, 0.0]));
        // And back, which is what makes the wire form and the file form one form.
        let back: Geometry = serde_json::from_value(json).unwrap();
        assert_eq!(back, geometry);
    }

    #[test]
    fn a_hole_excludes_the_middle() {
        let with_hole = Geometry::Polygon(vec![square(0.0, 0.0, 100.0), square(40.0, 40.0, 20.0)]);
        assert!(with_hole.contains(10.0, 10.0, 0.0), "inside the ring");
        assert!(!with_hole.contains(50.0, 50.0, 0.0), "inside the hole");
        assert!(!with_hole.contains(150.0, 50.0, 0.0), "outside entirely");
        // The hole's own edge is still grabbable, or it could not be edited.
        assert!(with_hole.contains(50.0, 40.0, 2.0));
        // Area is the ring minus the hole: 10000 - 400.
        assert_eq!(with_hole.area(), 9600.0);
    }

    #[test]
    fn a_line_is_hit_along_its_length_not_just_at_its_vertices() {
        let line = Geometry::LineString(vec![[0.0, 0.0], [100.0, 0.0]]);
        assert!(line.contains(50.0, 0.0, 1.0), "on the segment");
        assert!(line.contains(50.0, 3.0, 4.0), "within the pad");
        assert!(!line.contains(50.0, 20.0, 4.0), "beyond it");
        assert_eq!(line.area(), 0.0);
    }

    #[test]
    fn a_point_wins_over_a_region_that_surrounds_it() {
        let region = Annotation {
            geometry: Geometry::Polygon(vec![square(0.0, 0.0, 100.0)]),
            ..Default::default()
        };
        let cell = Annotation {
            id: 1,
            geometry: Geometry::Point([50.0, 50.0]),
            ..Default::default()
        };
        let set = [region, cell.clone()];
        assert_eq!(pick_annotation(&set, 51.0, 50.0, 4.0), Some(&cell));
    }

    #[test]
    fn the_smallest_containing_shape_is_the_one_meant() {
        let outer = Annotation {
            geometry: Geometry::Polygon(vec![square(0.0, 0.0, 100.0)]),
            ..Default::default()
        };
        let inner = Annotation {
            id: 1,
            geometry: Geometry::Polygon(vec![square(40.0, 40.0, 20.0)]),
            ..Default::default()
        };
        let set = [outer.clone(), inner.clone()];
        assert_eq!(
            pick_annotation(&set, 50.0, 50.0, 0.0).map(|a| a.id),
            Some(1)
        );
        assert_eq!(pick_annotation(&set, 5.0, 5.0, 0.0).map(|a| a.id), Some(0));
        assert!(pick_annotation(&set, 500.0, 5.0, 0.0).is_none());
    }

    #[test]
    fn a_z_extent_spans_a_range_of_planes_and_nothing_else() {
        let mut item = Annotation::rect(0.0, 0.0, 10.0, 10.0, Plane::at(4, 0));
        assert!(item.at_plane(4, 0));
        assert!(!item.at_plane(5, 0), "one plane by default");
        item.z_extent = 3;
        assert!(item.at_plane(4, 0) && item.at_plane(7, 0));
        assert!(!item.at_plane(8, 0) && !item.at_plane(3, 0));
        assert!(
            !item.at_plane(5, 1),
            "a different frame is a different picture"
        );
    }

    #[test]
    fn dragging_moves_every_coordinate_and_a_corner_scales_about_the_other() {
        let mut geometry = Geometry::Polygon(vec![square(10.0, 10.0, 10.0)]);
        geometry.translate(5.0, -5.0);
        assert_eq!(geometry.bounds(), Some([15.0, 5.0, 25.0, 15.0]));
        // Grow by two about the top-left corner.
        geometry.scale_about(15.0, 5.0, 2.0, 2.0);
        assert_eq!(geometry.bounds(), Some([15.0, 5.0, 35.0, 25.0]));
    }

    #[test]
    fn moving_a_vertex_keeps_a_ring_closed() {
        let mut ring = Geometry::Polygon(vec![square(0.0, 0.0, 10.0)]);
        // `square` is open; close it, as a file would.
        if let Geometry::Polygon(rings) = &mut ring {
            let first = rings[0][0];
            rings[0].push(first);
        }
        assert!(ring.move_vertex(0, 0, 5.0, 5.0));
        let last = {
            let Geometry::Polygon(rings) = &ring else {
                panic!()
            };
            assert_eq!(rings[0][0], [5.0, 5.0]);
            assert_eq!(
                rings[0].last(),
                rings[0].first(),
                "the seam must not spring open"
            );
            rings[0].len() - 1
        };
        // Moving the closing vertex moves the first one too.
        assert!(ring.move_vertex(0, last, -5.0, -5.0));
        let Geometry::Polygon(rings) = &ring else {
            panic!()
        };
        assert_eq!(rings[0][0], [0.0, 0.0]);
        assert_eq!(rings[0].last(), rings[0].first());
    }

    #[test]
    fn a_vertex_can_be_inserted_and_removed_but_not_below_a_shape() {
        let mut triangle =
            Geometry::Polygon(vec![vec![[0.0, 0.0], [10.0, 0.0], [5.0, 10.0], [0.0, 0.0]]]);
        assert!(triangle.insert_vertex(0, 0, [5.0, -2.0]));
        let Geometry::Polygon(rings) = &triangle else {
            panic!()
        };
        assert_eq!(rings[0][1], [5.0, -2.0]);
        assert_eq!(rings[0].len(), 5);

        assert!(triangle.remove_vertex(0, 1), "back to a triangle");
        // A triangle is the floor: taking another corner stops it being a ring.
        assert!(!triangle.remove_vertex(0, 1));

        let mut line = Geometry::LineString(vec![[0.0, 0.0], [1.0, 1.0], [2.0, 0.0]]);
        assert!(line.remove_vertex(0, 1));
        assert!(
            !line.remove_vertex(0, 0),
            "two ends is the floor for a line"
        );

        // A point set has no path to edit.
        let mut dot = Geometry::Point([1.0, 1.0]);
        assert!(!dot.move_vertex(0, 0, 1.0, 1.0));
        assert!(!dot.remove_vertex(0, 0));
    }

    #[test]
    fn vertices_are_addressed_the_same_way_handles_are_drawn() {
        // `paths_mut` and `outlines` must walk rings in the same order, or a
        // handle index means one shape to the canvas and another to the edit.
        let geometry = Geometry::MultiPolygon(vec![
            vec![square(0.0, 0.0, 10.0), square(2.0, 2.0, 3.0)],
            vec![square(50.0, 50.0, 10.0)],
        ]);
        let outlines = geometry.outlines();
        let mut geometry = geometry;
        let paths = geometry.paths_mut();
        assert_eq!(paths.len(), outlines.len());
        for (path, outline) in paths.iter().zip(&outlines) {
            assert_eq!(path[0], outline[0], "same ring, same order");
        }
    }

    #[test]
    fn a_new_shape_nests_under_the_smallest_thing_that_covers_it() {
        let region = Annotation {
            id: 1,
            geometry: Geometry::Polygon(vec![square(0.0, 0.0, 100.0)]),
            ..Default::default()
        };
        let inner = Annotation {
            id: 2,
            geometry: Geometry::Polygon(vec![square(10.0, 10.0, 50.0)]),
            parent: Some(1),
            ..Default::default()
        };
        let set = [region, inner];

        let cell = Annotation {
            id: 3,
            geometry: Geometry::Point([20.0, 20.0]),
            ..Default::default()
        };
        assert_eq!(
            containing_parent(&set, &cell),
            Some(2),
            "the smallest that covers it"
        );

        // Outside the inner one, still inside the region.
        let elsewhere = Annotation {
            id: 4,
            geometry: Geometry::Point([90.0, 90.0]),
            ..Default::default()
        };
        assert_eq!(containing_parent(&set, &elsewhere), Some(1));

        // Outside everything.
        let away = Annotation {
            id: 5,
            geometry: Geometry::Point([500.0, 500.0]),
            ..Default::default()
        };
        assert_eq!(containing_parent(&set, &away), None);

        // Poking out of a region is not being inside it.
        let straddles = Annotation {
            id: 6,
            geometry: Geometry::LineString(vec![[20.0, 20.0], [200.0, 20.0]]),
            ..Default::default()
        };
        assert_eq!(containing_parent(&set, &straddles), None);

        // A region on another slice does not enclose anything here.
        let deeper = Annotation {
            id: 7,
            geometry: Geometry::Point([20.0, 20.0]),
            plane: Plane::at(40, 0),
            ..Default::default()
        };
        assert_eq!(containing_parent(&set, &deeper), None);
    }

    #[test]
    fn tree_order_puts_each_parent_before_its_own_children() {
        let make = |id: u64, parent: Option<u64>| Annotation {
            id,
            parent,
            geometry: Geometry::Point([id as f64, 0.0]),
            ..Default::default()
        };
        // Deliberately out of order, and with a dangling parent.
        let set = [
            make(3, Some(1)),
            make(1, None),
            make(4, Some(3)),
            make(2, Some(1)),
            make(9, Some(99)),
        ];
        let order: Vec<(u64, usize)> = in_tree_order(&set)
            .into_iter()
            .map(|(a, depth)| (a.id, depth))
            .collect();
        assert_eq!(order, vec![(1, 0), (3, 1), (4, 2), (2, 1), (9, 0)]);
    }

    #[test]
    fn a_cycle_does_not_hang_or_lose_a_row() {
        // A hand-edited file can claim this; the list still has to render.
        let make = |id: u64, parent: u64| Annotation {
            id,
            parent: Some(parent),
            geometry: Geometry::Point([0.0, 0.0]),
            ..Default::default()
        };
        let set = [make(1, 2), make(2, 1)];
        let order = in_tree_order(&set);
        assert_eq!(order.len(), 2, "both rows are listed");
    }

    #[test]
    fn a_name_identifies_one_object_and_a_class_says_what_kind_it_is() {
        let mut item = Annotation::rect(0.0, 0.0, 10.0, 10.0, Plane::default());
        item.label = "Tumor".into();
        assert_eq!(
            item.display_name(),
            "Tumor",
            "a class, when there is no name"
        );
        item.name = Some("Region 3".into());
        assert_eq!(item.display_name(), "Region 3", "the name wins");
        item.name = Some(String::new());
        assert_eq!(item.display_name(), "Tumor", "an empty name is no name");
    }

    #[test]
    fn a_scale_of_one_moves_nothing_at_all() {
        // Not merely "moves nothing visible": `ox + (p - ox)` is not always `p`,
        // and a resize drag that ends where it began must be a no-op so that
        // undoing it restores the original bit for bit.
        let original = Geometry::Polygon(vec![vec![
            [318.9508056640625, 109.11475372314453],
            [261.6368103027344, 123.95240020751953],
            [309.1172790527344, 123.95240020751953],
            [318.9508056640625, 109.11475372314453],
        ]]);
        let mut scaled = original.clone();
        scaled.scale_about(100.0, 100.0, 1.0, 1.0);
        assert_eq!(scaled, original);
    }

    #[test]
    fn a_rectangle_is_a_closed_ring_of_four_corners() {
        let Geometry::Polygon(rings) = Geometry::rect(30.0, 40.0, 10.0, 20.0) else {
            panic!("not a polygon");
        };
        // Backwards corners come out the right way round.
        assert_eq!(rings[0][0], [10.0, 20.0]);
        assert_eq!(rings[0].len(), 5, "closed");
        assert_eq!(rings[0].first(), rings[0].last());
    }
}
