//! Transformation matrices and the geometry that hangs off them.
//!
//! PDF matrices are `[a b c d e f]`, standing for
//!
//! ```text
//! | a  b  0 |
//! | c  d  0 |
//! | e  f  1 |
//! ```
//!
//! with row vectors, so a point maps as `(x·a + y·c + e, x·b + y·d + f)` and
//! composition `A × B` means "apply A, then B".
//!
//! Held as `f64` rather than `f32`. Matrices compose repeatedly -- page CTM,
//! then form XObject `/Matrix`, then `Tm`, then the text-space scale -- and
//! `f32` drift over a deep nesting shows up as glyphs a fraction of a point out
//! of place. Spec 14.3 asks the pixel-diff harness to catch shifts above a
//! quarter of a pixel, which is a tighter bound than accumulated `f32` error.
//! Conversion to `f32` happens at the layout boundary, once.

/// A point in whatever space the surrounding code is working in.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Point { x, y }
    }
}

/// An axis-aligned rectangle, normalised so `x0 <= x1` and `y0 <= y1`.
///
/// PDF rectangles are written as two opposite corners in any order, so
/// normalising on construction removes a whole class of "why is this height
/// negative" bugs.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Rect {
    pub fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Rect { x0: x0.min(x1), y0: y0.min(y1), x1: x0.max(x1), y1: y0.max(y1) }
    }

    /// Read a `[x0 y0 x1 y1]` array, as `/MediaBox` and friends are written.
    pub fn from_array(items: &[rasura_cos::Object]) -> Option<Self> {
        if items.len() < 4 {
            return None;
        }
        let v: Vec<f64> = items.iter().take(4).filter_map(rasura_cos::Object::as_f64).collect();
        if v.len() < 4 {
            return None;
        }
        Some(Rect::new(v[0], v[1], v[2], v[3]))
    }

    pub fn width(&self) -> f64 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f64 {
        self.y1 - self.y0
    }

    pub fn is_empty(&self) -> bool {
        self.width() <= 0.0 || self.height() <= 0.0
    }

    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x0 && p.x <= self.x1 && p.y >= self.y0 && p.y <= self.y1
    }

    /// The smallest rectangle containing both.
    pub fn union(&self, other: &Rect) -> Rect {
        Rect {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    pub fn intersects(&self, other: &Rect) -> bool {
        self.x0 <= other.x1 && other.x0 <= self.x1 && self.y0 <= other.y1 && other.y0 <= self.y1
    }

    /// The overlap of two rectangles, or `None` when they do not overlap.
    ///
    /// `None` and an empty rectangle are different answers and the distinction
    /// is load-bearing for clipping: a clip that excludes everything is a real
    /// state a page can be in, and returning a zero-sized rectangle for it
    /// would be indistinguishable from a degenerate one that clips nothing.
    pub fn intersect(&self, other: &Rect) -> Option<Rect> {
        let out = Rect {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        };
        (out.x0 <= out.x1 && out.y0 <= out.y1).then_some(out)
    }

    /// The bounding box of this rectangle's four corners after `m`.
    ///
    /// A rotated rectangle is not a rectangle, so this is the bounding box of
    /// the image, not the image itself.
    pub fn transform(&self, m: &Matrix) -> Rect {
        let corners = [
            m.apply(Point::new(self.x0, self.y0)),
            m.apply(Point::new(self.x1, self.y0)),
            m.apply(Point::new(self.x0, self.y1)),
            m.apply(Point::new(self.x1, self.y1)),
        ];
        let mut out = Rect { x0: f64::MAX, y0: f64::MAX, x1: f64::MIN, y1: f64::MIN };
        for c in corners {
            out.x0 = out.x0.min(c.x);
            out.y0 = out.y0.min(c.y);
            out.x1 = out.x1.max(c.x);
            out.y1 = out.y1.max(c.y);
        }
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Default for Matrix {
    fn default() -> Self {
        Matrix::IDENTITY
    }
}

impl Matrix {
    pub const IDENTITY: Matrix = Matrix { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 };

    pub const fn new(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Self {
        Matrix { a, b, c, d, e, f }
    }

    pub const fn translate(tx: f64, ty: f64) -> Self {
        Matrix::new(1.0, 0.0, 0.0, 1.0, tx, ty)
    }

    pub const fn scale(sx: f64, sy: f64) -> Self {
        Matrix::new(sx, 0.0, 0.0, sy, 0.0, 0.0)
    }

    pub fn rotate(radians: f64) -> Self {
        let (s, c) = radians.sin_cos();
        Matrix::new(c, s, -s, c, 0.0, 0.0)
    }

    /// Read a `[a b c d e f]` array, as `cm`, `Tm` and `/Matrix` are written.
    pub fn from_array(items: &[rasura_cos::Object]) -> Option<Self> {
        if items.len() < 6 {
            return None;
        }
        let v: Vec<f64> = items.iter().take(6).filter_map(rasura_cos::Object::as_f64).collect();
        if v.len() < 6 {
            return None;
        }
        Some(Matrix::new(v[0], v[1], v[2], v[3], v[4], v[5]))
    }

    /// `self` then `other`. Note the order: this is `self × other` in PDF's
    /// row-vector convention, which reads as "apply self first".
    pub fn then(&self, other: &Matrix) -> Matrix {
        Matrix {
            a: self.a * other.a + self.b * other.c,
            b: self.a * other.b + self.b * other.d,
            c: self.c * other.a + self.d * other.c,
            d: self.c * other.b + self.d * other.d,
            e: self.e * other.a + self.f * other.c + other.e,
            f: self.e * other.b + self.f * other.d + other.f,
        }
    }

    pub fn apply(&self, p: Point) -> Point {
        Point { x: p.x * self.a + p.y * self.c + self.e, y: p.x * self.b + p.y * self.d + self.f }
    }

    /// Apply the linear part only, leaving translation out. Correct for
    /// displacements, which are vectors rather than positions.
    pub fn apply_vector(&self, p: Point) -> Point {
        Point { x: p.x * self.a + p.y * self.c, y: p.x * self.b + p.y * self.d }
    }

    pub fn determinant(&self) -> f64 {
        self.a * self.d - self.b * self.c
    }

    /// `None` for a singular matrix, which real files do contain -- a zero
    /// scale is a legitimate way to make content invisible.
    pub fn invert(&self) -> Option<Matrix> {
        let det = self.determinant();
        if det.abs() < f64::EPSILON {
            return None;
        }
        let inv = 1.0 / det;
        Some(Matrix {
            a: self.d * inv,
            b: -self.b * inv,
            c: -self.c * inv,
            d: self.a * inv,
            e: (self.c * self.f - self.d * self.e) * inv,
            f: (self.b * self.e - self.a * self.f) * inv,
        })
    }

    /// How much this matrix scales lengths along x and y. Used to turn a
    /// text-space font size into a device-space one.
    pub fn expansion(&self) -> (f64, f64) {
        ((self.a * self.a + self.b * self.b).sqrt(), (self.c * self.c + self.d * self.d).sqrt())
    }

    /// The rotation this matrix applies to the x axis, in radians.
    pub fn rotation(&self) -> f64 {
        self.b.atan2(self.a)
    }

    /// True when the matrix has no rotation or skew, which is the overwhelming
    /// majority of real content and permits several fast paths.
    pub fn is_axis_aligned(&self) -> bool {
        self.b.abs() < 1e-9 && self.c.abs() < 1e-9
    }

    pub fn is_finite(&self) -> bool {
        [self.a, self.b, self.c, self.d, self.e, self.f].iter().all(|v| v.is_finite())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn identity_is_a_no_op() {
        let p = Point::new(3.0, 4.0);
        assert_eq!(Matrix::IDENTITY.apply(p), p);
    }

    #[test]
    fn composition_order_is_self_then_other() {
        // Scale by 2, then translate by 10. A point at 1 should land at 12,
        // not at 22 (which is what the other order gives).
        let m = Matrix::scale(2.0, 2.0).then(&Matrix::translate(10.0, 0.0));
        assert_eq!(m.apply(Point::new(1.0, 0.0)).x, 12.0);
    }

    #[test]
    fn translation_does_not_affect_vectors() {
        let m = Matrix::translate(100.0, 100.0);
        assert_eq!(m.apply_vector(Point::new(1.0, 0.0)), Point::new(1.0, 0.0));
        assert_eq!(m.apply(Point::new(1.0, 0.0)), Point::new(101.0, 100.0));
    }

    #[test]
    fn inverse_round_trips() {
        let m = Matrix::new(2.0, 0.5, -0.3, 1.5, 20.0, -7.0);
        let inv = m.invert().unwrap();
        let p = Point::new(13.0, -4.0);
        let back = inv.apply(m.apply(p));
        assert!(close(back.x, p.x) && close(back.y, p.y), "{back:?}");
    }

    #[test]
    fn singular_matrices_have_no_inverse() {
        assert!(Matrix::scale(0.0, 1.0).invert().is_none());
        assert!(Matrix::new(1.0, 2.0, 2.0, 4.0, 0.0, 0.0).invert().is_none());
    }

    #[test]
    fn rotation_is_measurable() {
        let m = Matrix::rotate(std::f64::consts::FRAC_PI_2);
        let p = m.apply(Point::new(1.0, 0.0));
        assert!(close(p.x, 0.0) && close(p.y, 1.0), "{p:?}");
        assert!(close(m.rotation(), std::f64::consts::FRAC_PI_2));
        assert!(!m.is_axis_aligned());
    }

    #[test]
    fn expansion_reports_scale() {
        let (sx, sy) = Matrix::scale(3.0, 7.0).expansion();
        assert!(close(sx, 3.0) && close(sy, 7.0));
        // Rotation does not change lengths.
        let (sx, sy) = Matrix::rotate(0.9).expansion();
        assert!(close(sx, 1.0) && close(sy, 1.0));
    }

    #[test]
    fn rect_normalises_its_corners() {
        let r = Rect::new(10.0, 20.0, 0.0, 5.0);
        assert_eq!((r.x0, r.y0, r.x1, r.y1), (0.0, 5.0, 10.0, 20.0));
        assert_eq!(r.width(), 10.0);
        assert_eq!(r.height(), 15.0);
    }

    #[test]
    fn rect_transform_bounds_a_rotation() {
        // A unit square rotated 45 degrees has a bounding box of sqrt(2).
        let r = Rect::new(0.0, 0.0, 1.0, 1.0);
        let m = Matrix::rotate(std::f64::consts::FRAC_PI_4);
        let b = r.transform(&m);
        assert!(close(b.width(), 2f64.sqrt()), "{b:?}");
    }

    #[test]
    fn deep_composition_stays_accurate() {
        // The reason this module uses f64: a form XObject nested twenty deep,
        // each level scaling and translating, must not drift.
        let step = Matrix::new(1.01, 0.02, -0.02, 0.99, 3.0, -1.0);
        let mut m = Matrix::IDENTITY;
        for _ in 0..20 {
            m = m.then(&step);
        }
        let inv = m.invert().unwrap();
        let p = Point::new(72.0, 720.0);
        let back = inv.apply(m.apply(p));
        assert!((back.x - p.x).abs() < 1e-6, "drift {}", (back.x - p.x).abs());
        assert!((back.y - p.y).abs() < 1e-6, "drift {}", (back.y - p.y).abs());
    }
}
