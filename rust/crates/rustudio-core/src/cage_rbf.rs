//! Cage deformation as Studio does it (measured through
//! `WrapDeformer:CreateEditableMeshAsync` with single moved cage vertices,
//! checks/mesh-deform/AUDIT.md (e)):
//!
//! - The output is linear in the cage's displacement: a mesh point moves by
//!   Σ_j w_j(x) · Δc_j, along the displacement only (no rotation of
//!   offsets), with small negative weights and support across the cage.
//! - The weights are radial basis interpolation with the kernel φ(r) = r
//!   plus a linear polynomial (the system [Φ P; Pᵀ 0]): a single moved
//!   vertex's response matches it to 1e-4 of the move.
//! - Cage vertices at the same position are one centre; the last of them
//!   in the file is the one read (moving only the others changes nothing).
//! - Only cage points near the mesh are centres: a cage vertex counts when
//!   it is among the nearest few to some mesh vertex (moving all cage
//!   vertices farther than 0.15 studs from the mesh changes nothing).
//!
//! `Rbf::new` solves once for a reference cage and the mesh it moves;
//! `deform` then takes any displaced cage.

use glam::Vec3;
use std::collections::HashMap;

/// How many nearest cage points each mesh vertex adds to the centres.
pub const NEAREST: usize = 6;

pub struct Rbf {
    /// Per centre: the cage vertex index read for it.
    pub centre_vertex: Vec<usize>,
    centres: Vec<Vec3>,
    /// The solved system's inverse rows for the mesh points: per point,
    /// the weight of each centre (n) and the polynomial part folded in.
    weights: Vec<Vec<f32>>,
}

/// Position key for welding cage vertices (1e-5 studs).
fn key(p: Vec3) -> (i64, i64, i64) {
    (
        (p.x as f64 * 1e5).round() as i64,
        (p.y as f64 * 1e5).round() as i64,
        (p.z as f64 * 1e5).round() as i64,
    )
}

/// Cage points welded by position, the last duplicate kept: (position,
/// vertex index) per distinct point.
pub fn weld(cage: &[Vec3]) -> Vec<(Vec3, usize)> {
    let mut at: HashMap<(i64, i64, i64), usize> = HashMap::new();
    let mut out: Vec<(Vec3, usize)> = Vec::new();
    for (i, p) in cage.iter().enumerate() {
        match at.get(&key(*p)) {
            Some(&j) => out[j].1 = i,
            None => {
                at.insert(key(*p), out.len());
                out.push((*p, i));
            }
        }
    }
    out
}

fn nearest(points: &[(Vec3, usize)], p: Vec3, k: usize, best: &mut Vec<(f32, usize)>) {
    best.clear();
    for (i, (q, _)) in points.iter().enumerate() {
        let d = q.distance_squared(p);
        if best.len() < k || d < best[best.len() - 1].0 {
            let at = best.partition_point(|b| b.0 <= d);
            best.insert(at, (d, i));
            best.truncate(k);
        }
    }
}

/// Solves A x = b in place by LU with partial pivoting (f64, dense).
fn solve(a: &mut [f64], n: usize, rhs: &mut [f64], m: usize) -> bool {
    for c in 0..n {
        let mut piv = c;
        let mut best = a[c * n + c].abs();
        for r in c + 1..n {
            let v = a[r * n + c].abs();
            if v > best {
                best = v;
                piv = r;
            }
        }
        if best < 1e-12 {
            return false;
        }
        if piv != c {
            for k in 0..n {
                a.swap(c * n + k, piv * n + k);
            }
            for k in 0..m {
                rhs.swap(c * m + k, piv * m + k);
            }
        }
        let d = a[c * n + c];
        for r in c + 1..n {
            let f = a[r * n + c] / d;
            if f == 0.0 {
                continue;
            }
            a[r * n + c] = 0.0;
            for k in c + 1..n {
                a[r * n + k] -= f * a[c * n + k];
            }
            for k in 0..m {
                rhs[r * m + k] -= f * rhs[c * m + k];
            }
        }
    }
    for c in (0..n).rev() {
        let d = a[c * n + c];
        for k in 0..m {
            let mut s = rhs[c * m + k];
            for j in c + 1..n {
                s -= a[c * n + j] * rhs[j * m + k];
            }
            rhs[c * m + k] = s / d;
        }
    }
    true
}

impl Rbf {
    /// Binds `points` (the mesh) to the reference `cage` (vertex positions
    /// in the same space). None when the cage is empty or degenerate.
    pub fn new(cage: &[Vec3], points: &[Vec3]) -> Option<Rbf> {
        let welded = weld(cage);
        if welded.len() < 4 {
            return None;
        }
        // Centres: the cage points nearest the mesh.
        let mut used = vec![false; welded.len()];
        let mut best = Vec::with_capacity(NEAREST + 1);
        for p in points {
            nearest(&welded, *p, NEAREST, &mut best);
            for (_, i) in &best {
                used[*i] = true;
            }
        }
        let sel: Vec<usize> = (0..welded.len()).filter(|i| used[*i]).collect();
        let n = sel.len();
        let centres: Vec<Vec3> = sel.iter().map(|i| welded[*i].0).collect();
        let dim = n + 4;
        // The system is symmetric; the weight of centre j at a point x is
        // [φ(|x - c|), 1, x] · A⁻¹ e_j. Solve A Z = [φ(X); P(X)]ᵀ for all
        // mesh points at once (Z: dim × points), then w_j(x) = Z[j][x].
        let mut a = vec![0f64; dim * dim];
        for i in 0..n {
            for j in 0..n {
                a[i * dim + j] = centres[i].distance(centres[j]) as f64;
            }
            let c = centres[i];
            for (k, v) in [1.0, c.x as f64, c.y as f64, c.z as f64].into_iter().enumerate() {
                a[i * dim + n + k] = v;
                a[(n + k) * dim + i] = v;
            }
        }
        let m = points.len();
        let mut rhs = vec![0f64; dim * m];
        for (x, p) in points.iter().enumerate() {
            for j in 0..n {
                rhs[j * m + x] = p.distance(centres[j]) as f64;
            }
            for (k, v) in [1.0, p.x as f64, p.y as f64, p.z as f64].into_iter().enumerate() {
                rhs[(n + k) * m + x] = v;
            }
        }
        if !solve(&mut a, dim, &mut rhs, m) {
            return None;
        }
        let weights = (0..m).map(|x| (0..n).map(|j| rhs[j * m + x] as f32).collect()).collect();
        Some(Rbf {
            centre_vertex: sel.iter().map(|i| welded[*i].1).collect(),
            centres,
            weights,
        })
    }

    /// The mesh points moved by the cage moving from its reference to
    /// `cage` (same vertex order as the reference).
    pub fn deform(&self, points: &[Vec3], cage: &[Vec3]) -> Vec<Vec3> {
        let d: Vec<Vec3> = self
            .centre_vertex
            .iter()
            .zip(&self.centres)
            .map(|(v, c)| cage.get(*v).map_or(Vec3::ZERO, |q| *q - *c))
            .collect();
        points
            .iter()
            .zip(&self.weights)
            .map(|(p, w)| *p + w.iter().zip(&d).fold(Vec3::ZERO, |s, (w, d)| s + *d * *w))
            .collect()
    }

    pub fn centres(&self) -> usize {
        self.centres.len()
    }
}

/// Moves `points` by the radial basis field that takes each `rest` centre
/// to its `goal` (same kernel and centre choice as `Rbf`; centres at the
/// same position keep the last goal). One solve for the three
/// displacement components, then one sum per point.
pub fn fit_points(rest: &[Vec3], goal: &[Vec3], points: &[Vec3]) -> Option<Vec<Vec3>> {
    fit_points_near(rest, goal, points, points)
}

/// [`fit_points`] with the centres chosen near `mesh` (the deformed mesh's
/// vertices) while `points` may be anywhere (WrapDeformer's
/// GetDeformedCFrameAsync evaluates the part mesh's field at one point).
pub fn fit_points_near(rest: &[Vec3], goal: &[Vec3], mesh: &[Vec3], points: &[Vec3]) -> Option<Vec<Vec3>> {
    let welded = weld(rest);
    if welded.len() < 4 || goal.len() < rest.len() {
        return None;
    }
    let mut used = vec![false; welded.len()];
    let mut best = Vec::with_capacity(NEAREST + 1);
    for p in mesh {
        nearest(&welded, *p, NEAREST, &mut best);
        for (_, i) in &best {
            used[*i] = true;
        }
    }
    let sel: Vec<usize> = (0..welded.len()).filter(|i| used[*i]).collect();
    let n = sel.len();
    let c: Vec<Vec3> = sel.iter().map(|i| welded[*i].0).collect();
    let d: Vec<Vec3> = sel.iter().map(|i| goal[welded[*i].1] - welded[*i].0).collect();
    let dim = n + 4;
    let mut a = vec![0f64; dim * dim];
    let mut rhs = vec![0f64; dim * 3];
    for i in 0..n {
        for j in 0..n {
            a[i * dim + j] = c[i].distance(c[j]) as f64;
        }
        for (k, v) in [1.0, c[i].x as f64, c[i].y as f64, c[i].z as f64].into_iter().enumerate() {
            a[i * dim + n + k] = v;
            a[(n + k) * dim + i] = v;
        }
        rhs[i * 3] = d[i].x as f64;
        rhs[i * 3 + 1] = d[i].y as f64;
        rhs[i * 3 + 2] = d[i].z as f64;
    }
    if !solve(&mut a, dim, &mut rhs, 3) {
        return None;
    }
    let co = |k: usize| glam::DVec3::new(rhs[k * 3], rhs[k * 3 + 1], rhs[k * 3 + 2]);
    Some(
        points
            .iter()
            .map(|p| {
                let mut s = co(n) + co(n + 1) * p.x as f64 + co(n + 2) * p.y as f64 + co(n + 3) * p.z as f64;
                for j in 0..n {
                    s += co(j) * p.distance(c[j]) as f64;
                }
                *p + s.as_vec3()
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube() -> Vec<Vec3> {
        let mut v = Vec::new();
        for x in [-1.0, 0.0, 1.0] {
            for y in [-1.0, 0.0, 1.0] {
                for z in [-1.0, 0.0, 1.0] {
                    if x != 0.0 || y != 0.0 || z != 0.0 {
                        v.push(Vec3::new(x, y, z));
                    }
                }
            }
        }
        v
    }

    #[test]
    fn unmoved_cage_keeps_the_mesh() {
        let c = cube();
        let pts = vec![Vec3::new(0.2, 0.3, -0.1), Vec3::new(0.9, -0.5, 0.4)];
        let r = Rbf::new(&c, &pts).unwrap();
        for (a, b) in r.deform(&pts, &c).iter().zip(&pts) {
            assert!(a.distance(*b) < 1e-4);
        }
    }

    #[test]
    fn affine_moves_are_reproduced() {
        let c = cube();
        let pts = vec![Vec3::new(0.2, 0.3, -0.1), Vec3::new(0.9, -0.5, 0.4)];
        let r = Rbf::new(&c, &pts).unwrap();
        let m = glam::Mat3::from_rotation_z(0.6) * 1.3;
        let moved: Vec<Vec3> = c.iter().map(|p| m * *p + Vec3::X).collect();
        for (a, b) in r.deform(&pts, &moved).iter().zip(&pts) {
            assert!(a.distance(m * *b + Vec3::X) < 1e-4, "{a} vs {}", m * *b + Vec3::X);
        }
    }

    #[test]
    fn fit_points_matches_rbf() {
        let c = cube();
        let pts = vec![Vec3::new(0.2, 0.3, -0.1), Vec3::new(0.9, -0.5, 0.4)];
        let goal: Vec<Vec3> = c.iter().map(|p| *p + Vec3::new(p.y * 0.2, 0.0, p.x * p.x * 0.1)).collect();
        let a = Rbf::new(&c, &pts).unwrap().deform(&pts, &goal);
        let b = fit_points(&c, &goal, &pts).unwrap();
        for (x, y) in a.iter().zip(&b) {
            assert!(x.distance(*y) < 1e-4, "{x} {y}");
        }
    }

    #[test]
    fn duplicates_read_the_last_vertex() {
        let mut c = cube();
        c.push(c[0]);
        let w = weld(&c);
        assert_eq!(w.len(), 26);
        assert_eq!(w[0].1, 26);
    }
}
