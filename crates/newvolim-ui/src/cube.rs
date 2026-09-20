//! The orientation box: a fixed near-isometric orthographic camera onto a box drawn to the
//! volume's true proportions, with the three crosshair planes cutting it. Ported from
//! `omezarr_viewers-rs` (`app/src/cube_pane.rs`), where it is pure Rust so the picture and the
//! hit test cannot disagree. Everything here is CPU maths; the pane draws the result on a 2D
//! canvas and feeds pointer motion back through [`CubeView::drag_fraction`].

/// Yaw about the vertical and pitch above the horizon. Near-isometric but deliberately not
/// isometric: at 45°/35.26° the three axes project to the same length and the box reads as a
/// hexagon with no way to tell x from z. No plane is edge-on at these angles.
pub const YAW: f32 = 38.0 * std::f32::consts::PI / 180.0;
pub const PITCH: f32 = 26.0 * std::f32::consts::PI / 180.0;

/// How far from a plane a press may land and still grab it, in screen pixels.
pub const GRAB_PX: f32 = 6.0;

/// Below this the axis is too close to edge-on for a drag to mean anything.
pub const MIN_AXIS_SPAN_PX: f32 = 6.0;

#[derive(Clone, Debug)]
pub struct CubeView {
    right: [f32; 3],
    up: [f32; 3],
    eye: [f32; 3],
    extent: [f32; 3],
    half: [f32; 3],
    scale_px: f32,
    size: (f32, f32),
}

impl CubeView {
    /// `world` is the box in world units per axis (x, y, z); `canvas` the pane in CSS pixels.
    pub fn new(world: [f32; 3], canvas: (f32, f32)) -> Self {
        Self::with_camera(world, canvas, YAW, PITCH)
    }

    pub fn with_camera(world: [f32; 3], canvas: (f32, f32), yaw: f32, pitch: f32) -> Self {
        let sizes = [world[0].max(1e-6), world[1].max(1e-6), world[2].max(1e-6)];
        let longest = sizes[0].max(sizes[1]).max(sizes[2]);
        let extent = [sizes[0] / longest, sizes[1] / longest, sizes[2] / longest];
        let half = [extent[0] * 0.5, extent[1] * 0.5, extent[2] * 0.5];

        let (sy, cy) = (yaw.sin(), yaw.cos());
        let (sp, cp) = (pitch.sin(), pitch.cos());
        let eye = [cp * sy, sp, cp * cy];
        let right = [cy, 0.0, -sy];
        let up = [-sp * sy, cp, -sp * cy];
        // Image y runs down and z runs into the screen; flip both so the box is seen from the
        // front, above and to the right.
        let flip = |v: [f32; 3]| [v[0], -v[1], -v[2]];
        let (right, up, eye) = (flip(right), flip(up), flip(eye));

        let mut max_u: f32 = 1e-6;
        let mut max_v: f32 = 1e-6;
        for corner in corners(half) {
            max_u = max_u.max(dot(corner, right).abs());
            max_v = max_v.max(dot(corner, up).abs());
        }
        let (w, h) = (canvas.0.max(1.0), canvas.1.max(1.0));
        let scale_px = 0.86 * (w / (2.0 * max_u)).min(h / (2.0 * max_v));
        Self {
            right,
            up,
            eye,
            extent,
            half,
            scale_px,
            size: (w, h),
        }
    }

    /// A plane's position along its axis, in box units centred on the origin.
    pub fn cut_coord(&self, axis: usize, fraction: f32) -> f32 {
        (fraction.clamp(0.0, 1.0) - 0.5) * self.extent[axis]
    }

    /// Box coordinates to canvas pixels.
    pub fn project(&self, a: [f32; 3]) -> (f32, f32) {
        let u = dot(a, self.right) * self.scale_px;
        let v = dot(a, self.up) * self.scale_px;
        (self.size.0 * 0.5 + u, self.size.1 * 0.5 - v)
    }

    /// Distance along the view direction; larger is nearer the camera.
    pub fn depth_of(&self, a: [f32; 3]) -> f32 {
        dot(a, self.eye)
    }

    /// The box's eight corners, projected.
    pub fn corner_points(&self) -> [([f32; 3], (f32, f32)); 8] {
        let mut out = [([0.0; 3], (0.0, 0.0)); 8];
        for (slot, corner) in out.iter_mut().zip(corners(self.half)) {
            *slot = (corner, self.project(corner));
        }
        out
    }

    /// The four corners of the plane cutting `axis` at `fraction`, projected, in drawing order.
    pub fn plane_quad(&self, axis: usize, fraction: f32) -> [(f32, f32); 4] {
        let at = self.cut_coord(axis, fraction);
        let (a, b) = ((axis + 1) % 3, (axis + 2) % 3);
        let mut quad = [(0.0, 0.0); 4];
        for (index, (sa, sb)) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)].into_iter().enumerate() {
            let mut point = [0.0; 3];
            point[axis] = at;
            point[a] = sa * self.half[a];
            point[b] = sb * self.half[b];
            quad[index] = self.project(point);
        }
        quad
    }

    /// Where the ray under `pointer` meets the plane, if within the box plus `margin_px`.
    /// Returns the ray parameter (smaller is nearer the camera) and the point.
    pub fn plane_hit(&self, axis: usize, fraction: f32, pointer: (f32, f32), margin_px: f32) -> Option<(f32, [f32; 3])> {
        let u = (pointer.0 - self.size.0 * 0.5) / self.scale_px;
        let v = (self.size.1 * 0.5 - pointer.1) / self.scale_px;
        let origin = [
            u * self.right[0] + v * self.up[0],
            u * self.right[1] + v * self.up[1],
            u * self.right[2] + v * self.up[2],
        ];
        let dir = [-self.eye[0], -self.eye[1], -self.eye[2]];
        if dir[axis].abs() < 1e-3 {
            return None;
        }
        let t = (self.cut_coord(axis, fraction) - origin[axis]) / dir[axis];
        let point = [origin[0] + t * dir[0], origin[1] + t * dir[1], origin[2] + t * dir[2]];
        let margin = margin_px / self.scale_px;
        for (other, at) in point.iter().enumerate() {
            if other != axis && at.abs() > self.half[other] + margin {
                return None;
            }
        }
        Some((t, point))
    }

    /// The plane a press at `pointer` grabs: the nearest to the camera among those hit.
    pub fn pick(&self, cut: [f32; 3], pointer: (f32, f32)) -> Option<usize> {
        let mut best: Option<(f32, usize)> = None;
        for (axis, fraction) in cut.iter().enumerate() {
            if let Some((t, _)) = self.plane_hit(axis, *fraction, pointer, GRAB_PX) {
                if best.is_none_or(|(bt, _)| t < bt) {
                    best = Some((t, axis));
                }
            }
        }
        best.map(|(_, axis)| axis)
    }

    /// The screen vector of the whole axis, fraction 0 to 1.
    pub fn axis_span_px(&self, axis: usize) -> (f32, f32) {
        let e = self.extent[axis];
        (e * self.right[axis] * self.scale_px, -e * self.up[axis] * self.scale_px)
    }

    /// The fraction after dragging by `moved` pixels since the press that found `start`.
    /// Measured from the press, not accumulated, so quantising the result back to a voxel
    /// index between events cannot drift.
    pub fn drag_fraction(&self, axis: usize, start: f32, moved: (f32, f32)) -> Option<f32> {
        let (sx, sy) = self.axis_span_px(axis);
        let len2 = sx * sx + sy * sy;
        if len2 < MIN_AXIS_SPAN_PX * MIN_AXIS_SPAN_PX {
            return None;
        }
        Some((start + (moved.0 * sx + moved.1 * sy) / len2).clamp(0.0, 1.0))
    }
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn corners(half: [f32; 3]) -> [[f32; 3]; 8] {
    let mut out = [[0.0; 3]; 8];
    for (i, corner) in out.iter_mut().enumerate() {
        for (axis, c) in corner.iter_mut().enumerate() {
            *c = if i >> axis & 1 == 1 { half[axis] } else { -half[axis] };
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_box_fits_the_canvas_and_no_axis_is_edge_on() {
        let view = CubeView::new([256.0, 256.0, 64.0], (200.0, 160.0));
        for (_, (x, y)) in view.corner_points() {
            assert!((0.0..=200.0).contains(&x) && (0.0..=160.0).contains(&y), "corner {x},{y} off canvas");
        }
        // The camera, not the proportions, decides edge-on: every axis of a plausible volume
        // has a screen direction long enough to drag along.
        for axis in 0..3 {
            let (sx, sy) = view.axis_span_px(axis);
            assert!((sx * sx + sy * sy).sqrt() > MIN_AXIS_SPAN_PX, "axis {axis} is edge-on");
        }
        // A slab stays a slab: drawn to true proportions, a 512×512×8 volume's z axis is a
        // pixel or two, and a drag along it is refused rather than made to leap.
        let slab = CubeView::new([512.0, 512.0, 8.0], (200.0, 160.0));
        let span = |view: &CubeView, axis: usize| {
            let (sx, sy) = view.axis_span_px(axis);
            (sx * sx + sy * sy).sqrt()
        };
        assert!(span(&slab, 2) * 8.0 < span(&slab, 0), "z span {} vs x span {}", span(&slab, 2), span(&slab, 0));
        assert_eq!(slab.drag_fraction(2, 0.5, (10.0, 10.0)), None);
    }

    #[test]
    fn a_press_on_a_plane_grabs_it_and_the_nearest_wins() {
        let view = CubeView::new([100.0, 100.0, 100.0], (300.0, 300.0));
        let cut = [0.5, 0.5, 0.5];
        // The centre of the box is on all three planes; the pick is the one nearest the camera,
        // which for this viewpoint (front, above, right) must be a plane facing it.
        let centre = view.project([0.0, 0.0, 0.0]);
        assert!(view.pick(cut, centre).is_some());
        // A point on the z plane away from the others, within the box.
        let at = view.cut_coord(2, 0.5);
        let on_z = view.project([0.3 * 0.5, 0.3 * 0.5, at]);
        let (_, hit) = view.plane_hit(2, 0.5, on_z, 0.0).expect("hit the z plane");
        assert!((hit[2] - at).abs() < 1e-4);
        // Far outside the box nothing is grabbed.
        assert_eq!(view.pick(cut, (1.0, 1.0)), None);
    }

    #[test]
    fn dragging_the_whole_axis_span_moves_the_cut_from_one_face_to_the_other() {
        let view = CubeView::new([256.0, 128.0, 64.0], (240.0, 240.0));
        for axis in 0..3 {
            let span = view.axis_span_px(axis);
            assert_eq!(view.drag_fraction(axis, 0.0, span), Some(1.0));
            assert_eq!(view.drag_fraction(axis, 1.0, (-span.0, -span.1)), Some(0.0));
            let half = view.drag_fraction(axis, 0.0, (span.0 * 0.5, span.1 * 0.5)).unwrap();
            assert!((half - 0.5).abs() < 1e-5, "axis {axis}: {half}");
            // Motion perpendicular to the axis does not move it.
            let perpendicular = (-span.1, span.0);
            let unchanged = view.drag_fraction(axis, 0.25, perpendicular).unwrap();
            assert!((unchanged - 0.25).abs() < 1e-5);
        }
        // A degenerate box on the drawn axis refuses the drag rather than leaping.
        let flat = CubeView::new([256.0, 256.0, 1e-9], (240.0, 240.0));
        assert_eq!(flat.drag_fraction(2, 0.5, (10.0, 10.0)), None);
    }
}
