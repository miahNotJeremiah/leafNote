//! Geometry helpers for the infinite freeform canvas.
//!
//! Kept GUI-free so both the renderer (`leafnote-app`, which tessellates with
//! `lyon`) and headless tools can share the same math: bounding boxes,
//! hit-testing, stroke smoothing, and eraser intersection.

use crate::model::{PenKind, Point, Rect, Stroke};

/// Axis-aligned bounding box of a stroke (with half-width padding).
pub fn stroke_bounding_box(s: &Stroke) -> Rect {
    let pad = s.width_mm.max(0.5);
    if s.points.is_empty() {
        return Rect::default();
    }
    let mut min = Point::new(f32::MAX, f32::MAX);
    let mut max = Point::new(f32::MIN, f32::MIN);
    for p in &s.points {
        min.x = min.x.min(p.x);
        min.y = min.y.min(p.y);
        max.x = max.x.max(p.x);
        max.y = max.y.max(p.y);
    }
    Rect::new(
        min.x - pad,
        min.y - pad,
        (max.x - min.x) + pad * 2.0,
        (max.y - min.y) + pad * 2.0,
    )
}

/// Bounding box covering many strokes (used to frame imported ink groups).
pub fn strokes_bounds<'a>(strokes: impl IntoIterator<Item = &'a Stroke>) -> Rect {
    let mut out = Rect::default();
    let mut first = true;
    for s in strokes {
        let b = stroke_bounding_box(s);
        if b.w == 0.0 && b.h == 0.0 && s.points.is_empty() {
            continue;
        }
        out = if first {
            first = false;
            b
        } else {
            out.union(b)
        };
    }
    out
}

/// Distance from point `p` to segment `ab`.
fn dist_to_segment(p: Point, a: Point, b: Point) -> f32 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len2 = dx * dx + dy * dy;
    if len2 == 0.0 {
        return p.distance(a);
    }
    let t = (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0);
    let proj = Point::new(a.x + t * dx, a.y + t * dy);
    p.distance(proj)
}

/// True if the pointer is within `tol_mm` of the stroke's polyline.
pub fn stroke_hit_test(s: &Stroke, p: Point, tol_mm: f32) -> bool {
    let tol = tol_mm + s.width_mm / 2.0;
    // Fast reject via bbox.
    let bb = stroke_bounding_box(s);
    if !bb.contains(Point::new(p.x, p.y)) && !expanded(&bb, tol).contains(p) {
        return false;
    }
    if s.points.len() == 1 {
        return s.points[0].distance(p) <= tol;
    }
    s.points.windows(2).any(|w| dist_to_segment(p, w[0], w[1]) <= tol)
}

fn expanded(r: &Rect, by: f32) -> Rect {
    Rect::new(r.x - by, r.y - by, r.w + by * 2.0, r.h + by * 2.0)
}

/// Remove near-duplicate raw input samples (jitter filter for pen input).
pub fn decimate(points: &[Point], min_spacing: f32) -> Vec<Point> {
    let mut out: Vec<Point> = Vec::with_capacity(points.len());
    for p in points {
        match out.last() {
            Some(last) if last.distance(*p) < min_spacing => {}
            _ => out.push(*p),
        }
    }
    if let Some(last) = points.last() {
        if out.last() != Some(last) {
            out.push(*last);
        }
    }
    out
}

/// Chaikin corner-cutting smoothing — gives ink strokes a natural feel while
/// keeping them cheap to store (we persist raw points, smooth at render time).
pub fn smooth(points: &[Point], iterations: usize) -> Vec<Point> {
    let mut pts = points.to_vec();
    for _ in 0..iterations {
        if pts.len() < 3 {
            break;
        }
        let mut next = vec![pts[0]];
        for w in pts.windows(2) {
            let a = Point::new(w[0].x * 0.75 + w[1].x * 0.25, w[0].y * 0.75 + w[1].y * 0.25);
            let b = Point::new(w[0].x * 0.25 + w[1].x * 0.75, w[0].y * 0.25 + w[1].y * 0.75);
            next.push(a);
            next.push(b);
        }
        next.push(*pts.last().unwrap());
        pts = next;
    }
    pts
}

/// Default widths per pen tool (millimetres).
pub fn default_width(kind: PenKind) -> f32 {
    match kind {
        PenKind::Pen => 0.5,
        PenKind::Pencil => 0.35,
        PenKind::Highlighter => 4.0,
        PenKind::Eraser => 6.0,
    }
}

/// Alpha applied when rendering a tool's ink.
pub fn effective_alpha(s: &Stroke) -> u8 {
    match s.kind {
        PenKind::Highlighter => 90,
        PenKind::Pencil => 200,
        _ => s.color.a,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Color;

    fn line(a: (f32, f32), b: (f32, f32)) -> Stroke {
        Stroke {
            points: vec![Point::new(a.0, a.1), Point::new(b.0, b.1)],
            color: Color::BLACK,
            width_mm: 1.0,
            kind: PenKind::Pen,
        }
    }

    #[test]
    fn bbox_of_diagonal_line() {
        let bb = stroke_bounding_box(&line((0.0, 0.0), (10.0, 10.0)));
        assert_eq!(bb, Rect::new(-0.5, -0.5, 11.0, 11.0));
    }

    #[test]
    fn hit_test_on_and_off_line() {
        let s = line((0.0, 0.0), (100.0, 0.0));
        assert!(stroke_hit_test(&s, Point::new(50.0, 0.4), 0.5));
        assert!(!stroke_hit_test(&s, Point::new(50.0, 20.0), 0.5));
    }

    #[test]
    fn decimate_drops_jitter() {
        let pts = vec![
            Point::new(0.0, 0.0),
            Point::new(0.01, 0.0),
            Point::new(5.0, 0.0),
            Point::new(10.0, 0.0),
        ];
        let out = decimate(&pts, 0.1);
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn smoothing_preserves_endpoints() {
        let pts = vec![
            Point::new(0.0, 0.0),
            Point::new(10.0, 5.0),
            Point::new(20.0, 0.0),
        ];
        let sm = smooth(&pts, 2);
        assert_eq!(sm.first().unwrap(), &pts[0]);
        assert_eq!(sm.last().unwrap(), pts.last().unwrap());
        assert!(sm.len() > pts.len());
    }
}
