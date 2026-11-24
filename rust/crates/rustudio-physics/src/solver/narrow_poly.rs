//! PolyPoly (GEOM §3; PolyPolyContact slot 18 Studio): block–wedge,
//! wedge–wedge and corner-wedge pairs, Studio's FaceVertex separating feature and
//! its manifold; and the ball–wedge / ball–corner-wedge closest feature (GEOM §5).
//! f32, Studio's order of operations where known. Mesh tables are
//! the exe's (.data Studio..Studio, .rdata Studio..Studio).
//!
//! Helpers whose bodies are not known are marked `GEOM Open 4` with the
//! reading taken.

use glam::Vec3;

use super::narrow::{FaceVertex, Frame, PairState, Shape, Temps, MESH_BLOCK, MESH_WEDGE};

/// .rdata Studio: ±FLT_MAX (the clamps of Studio).
const BIG: f32 = f32::MAX;

// ------------------------------------------------------------------ mesh tables

/// Faces: [edge count, edge ids…] (5 bytes; block Studio, wedge Studio,
/// corner Studio).
const FACES: [[[u8; 5]; 6]; 3] = [
    [[4, 0, 1, 2, 3], [4, 4, 5, 6, 0], [4, 6, 7, 8, 1], [4, 5, 9, 10, 7], [4, 11, 2, 8, 10], [4, 4, 3, 11, 9]],
    [[4, 0, 1, 2, 3], [4, 0, 4, 5, 6], [3, 3, 7, 4, 255], [3, 6, 8, 1, 255], [4, 5, 7, 2, 8], [0; 5]],
    [[3, 0, 1, 2, 255], [3, 2, 3, 4, 255], [3, 5, 3, 6, 255], [4, 0, 4, 5, 7], [3, 7, 6, 1, 255], [0; 5]],
];
/// Edges: [v0, v1, faceL, faceR] (block Studio, wedge Studio, corner Studio).
const EDGES: [[[u8; 4]; 12]; 3] = [
    [[1, 0, 0, 1], [0, 2, 0, 2], [2, 3, 0, 4], [3, 1, 0, 5], [1, 5, 1, 5], [5, 4, 1, 3], [4, 0, 1, 2], [4, 6, 2, 3], [6, 2, 2, 4], [5, 7, 3, 5], [7, 6, 3, 4], [7, 3, 4, 5]],
    [[0, 3, 0, 1], [3, 4, 0, 3], [4, 1, 0, 4], [1, 0, 0, 2], [0, 2, 1, 2], [2, 5, 1, 4], [5, 3, 1, 3], [1, 2, 2, 4], [5, 4, 3, 4], [0; 4], [0; 4], [0; 4]],
    [[0, 1, 0, 3], [1, 4, 0, 4], [4, 0, 0, 1], [4, 3, 1, 2], [3, 0, 1, 3], [2, 3, 2, 3], [4, 2, 2, 4], [2, 1, 3, 4], [0; 4], [0; 4], [0; 4], [0; 4]],
];
/// Vertices: [edge count, edge ids…] (block Studio, wedge Studio, corner Studio).
const VERTS: [[[u8; 5]; 8]; 3] = [
    [[3, 0, 1, 6, 255], [3, 0, 3, 4, 255], [3, 1, 2, 8, 255], [3, 2, 3, 11, 255], [3, 5, 6, 7, 255], [3, 4, 5, 9, 255], [3, 7, 8, 10, 255], [3, 9, 10, 11, 255]],
    [[3, 0, 3, 4, 255], [3, 2, 3, 7, 255], [3, 4, 5, 7, 255], [3, 0, 1, 6, 255], [3, 1, 2, 8, 255], [3, 5, 6, 8, 255], [0; 5], [0; 5]],
    [[3, 0, 2, 4, 255], [3, 0, 1, 7, 255], [3, 5, 6, 7, 255], [3, 3, 4, 5, 255], [4, 1, 2, 3, 6], [0; 5], [0; 5], [0; 5]],
];
/// Wedge / corner vertex tables (.rdata Studio), × size.
const WEDGE_V: [[f32; 3]; 6] = [[0.5, 0.5, 0.5], [0.5, -0.5, 0.5], [0.5, -0.5, -0.5], [-0.5, 0.5, 0.5], [-0.5, -0.5, 0.5], [-0.5, -0.5, -0.5]];
const CORNER_V: [[f32; 3]; 5] = [[0.5, -0.5, 0.5], [0.5, -0.5, -0.5], [-0.5, -0.5, -0.5], [-0.5, -0.5, 0.5], [0.5, 0.5, -0.5]];
/// Face normals: block (.rdata Studio), wedge (Studio, face 1 computed),
/// corner (Studio, faces 1, 2 computed); wedge plane offsets index Studio.
const BLOCK_N: [[f32; 3]; 6] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [-1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, -1.0]];
const WEDGE_N: [[f32; 3]; 5] = [[0.0, 0.0, 1.0], [1.0, 1.0, 1.0], [1.0, 0.0, 0.0], [-1.0, 0.0, 0.0], [0.0, -1.0, 0.0]];
const WEDGE_IDX: [usize; 5] = [2, 0, 0, 0, 1];
const CORNER_N: [[f32; 3]; 5] = [[1.0, 0.0, 0.0], [1.0, 1.0, 1.0], [1.0, 1.0, 1.0], [0.0, -1.0, 0.0], [0.0, 0.0, -1.0]];

/// A mesh reference {size, kind} (GEOM §3.1).
#[derive(Clone, Copy)]
struct Mesh {
    kind: u8,
    size: Vec3,
}

impl Mesh {
    fn of(s: &Shape) -> Mesh {
        Mesh { kind: s.mesh, size: s.size }
    }
    /// Studio: faces.
    fn n_faces(&self) -> usize {
        if self.kind == MESH_BLOCK { 6 } else { 5 }
    }
    /// Studio: vertices.
    fn n_verts(&self) -> usize {
        [8, 6, 5][self.kind as usize]
    }
    /// Studio: vertex i (block: code bits × size·0.5; others table × size).
    fn vertex(&self, i: usize) -> Vec3 {
        let s = self.size;
        match self.kind {
            MESH_BLOCK => super::narrow::vertex(i as u8, Vec3::new(s.x * 0.5, s.y * 0.5, s.z * 0.5)),
            MESH_WEDGE => {
                let t = WEDGE_V[i];
                Vec3::new(s.x * t[0], t[1] * s.y, t[2] * s.z)
            }
            _ => {
                let t = CORNER_V[i];
                Vec3::new(t[0] * s.x, t[1] * s.y, t[2] * s.z)
            }
        }
    }
    /// Studio: face plane (n, d), local.
    fn plane(&self, f: usize) -> (Vec3, f32) {
        let s = self.size;
        let sz = [s.x, s.y, s.z];
        match self.kind {
            MESH_BLOCK => (Vec3::from(BLOCK_N[f]), sz[f % 3] * 0.5),
            MESH_WEDGE => {
                if f != 1 {
                    return (Vec3::from(WEDGE_N[f]), sz[WEDGE_IDX[f]] * 0.5);
                }
                let m = -s.y;
                let l = 1.0 / ((s.z * s.z + 0.0) + m * m).sqrt();
                (Vec3::new(l * 0.0, l * s.z, m * l), 0.0)
            }
            _ => match f {
                1 => {
                    let l = 1.0 / ((s.z * s.z + 0.0) + s.y * s.y).sqrt();
                    (Vec3::new(l * 0.0, l * s.z, l * s.y), 0.0)
                }
                2 => {
                    let m = -s.y;
                    let l = 1.0 / ((s.x * s.x + m * m) + 0.0).sqrt();
                    (Vec3::new(l * m, s.x * l, l * 0.0), 0.0)
                }
                _ => (Vec3::from(CORNER_N[f]), sz[f >> 1] * 0.5),
            },
        }
    }
    fn face(&self, f: usize) -> &[u8; 5] {
        &FACES[self.kind as usize][f]
    }
    fn edge(&self, e: usize) -> &[u8; 4] {
        &EDGES[self.kind as usize][e]
    }
    /// Studio: edges (= vertices) of face f.
    fn face_len(&self, f: usize) -> usize {
        self.face(f)[0] as usize
    }
    /// Studio: the k-th vertex of face f.
    fn face_vertex(&self, f: usize, k: usize) -> usize {
        let e = self.edge(self.face(f)[1 + k] as usize);
        if f == e[2] as usize { e[0] as usize } else { e[1] as usize }
    }
    /// The face across face f's k-th edge (Studio's side planes).
    fn neighbour(&self, f: usize, k: usize) -> usize {
        let e = self.edge(self.face(f)[1 + k] as usize);
        if f == e[2] as usize { e[3] as usize } else { e[2] as usize }
    }
    /// Studio: faces around vertex v.
    fn vert_len(&self, v: usize) -> usize {
        VERTS[self.kind as usize][v][0] as usize
    }
    /// Studio: the k-th face around vertex v (v == E[0] → faceL, else faceR).
    fn vert_face(&self, v: usize, k: usize) -> usize {
        let e = self.edge(VERTS[self.kind as usize][v][1 + k] as usize);
        if v == e[0] as usize { e[2] as usize } else { e[3] as usize }
    }
    /// Studio: the edge joining v0 and v1 (GEOM Open 4: either orientation).
    fn edge_between(&self, v0: usize, v1: usize) -> usize {
        let n = [12, 9, 8][self.kind as usize];
        (0..n).find(|&e| {
            let x = self.edge(e);
            (x[0] as usize == v0 && x[1] as usize == v1) || (x[0] as usize == v1 && x[1] as usize == v0)
        }).unwrap_or(0)
    }
    /// Studio: edge e as a line, origin E[0], unit direction E[0] → E[1]
    /// (GEOM Open 4: body not known; the closest-point code assumes unit lines).
    fn edge_line(&self, e: usize) -> (Vec3, Vec3) {
        let x = self.edge(e);
        let (a, b) = (self.vertex(x[0] as usize), self.vertex(x[1] as usize));
        ((a), (b - a).normalize_or_zero())
    }
}

#[inline]
fn dot_xyz(n: Vec3, p: Vec3) -> f32 {
    (n.x * p.x + n.y * p.y) + n.z * p.z
}

#[inline]
fn clampf(x: f32) -> f32 {
    // `lo = -MAX; if lo <= x { lo = x }; hi = MAX; if lo <= hi { hi = lo }`.
    let a = if -BIG <= x { x } else { -BIG };
    if a <= BIG { a } else { BIG }
}

/// Studio(a, out, b): b in a's frame, inverse(a)·b (GEOM Open 4: order of the
/// fused product not known; Studio's).
#[inline]
fn rel(a: &Frame, b: &Frame) -> Frame {
    a.inverse().mul(b)
}

/// Studio(mesh, f, p): p projected on face f's plane lies inside every
/// neighbouring face's plane.
fn in_prism(m: &Mesh, f: usize, p: Vec3) -> bool {
    let (n, d) = m.plane(f);
    let t = -(((n.x * p.x + n.y * p.y) + n.z * p.z) - d);
    let q = Vec3::new(clampf(n.x * t + p.x), clampf(n.y * t + p.y), clampf(n.z * t + p.z));
    for k in 0..m.face_len(f) {
        let (g, gd) = m.plane(m.neighbour(f, k));
        if (q.y * g.y + q.x * g.x) + q.z * g.z > gd {
            return false;
        }
    }
    true
}

/// Studio: class of Q's vertex v (T = Q in P's frame) against P's face f:
/// 0 above inside, 1 above outside, 2 on/below inside, 3 below outside.
fn class(mp: &Mesh, f: usize, mq: &Mesh, v: usize, t: &Frame) -> u8 {
    let p = t.to_world(mq.vertex(v));
    let (n, d) = mp.plane(f);
    let inside = in_prism(mp, f, p);
    let q = Vec3::new(clampf(p.x), clampf(p.y), clampf(p.z));
    if d < (q.x * n.x + q.y * n.y) + q.z * n.z {
        if inside { 0 } else { 1 }
    } else if inside {
        2
    } else {
        3
    }
}

/// Studio(F, out, plane): a local plane in world (GEOM Open 4: body not
/// known; n' = R·n, d' = d + n'·t).
#[inline]
fn plane_world(f: &Frame, n: Vec3, d: f32) -> (Vec3, f32) {
    let nw = f.rot(n);
    (nw, d + dot_xyz(nw, f.pos()))
}

/// The world AABB of a part: its box's extents projected (GEOM Open 4).
fn aabb(s: &Shape) -> (Vec3, Vec3) {
    let e = s.size * 0.5;
    let r = &s.f.m;
    let x = Vec3::new(
        r[0].abs() * e.x + r[1].abs() * e.y + r[2].abs() * e.z,
        r[3].abs() * e.x + r[4].abs() * e.y + r[5].abs() * e.z,
        r[6].abs() * e.x + r[7].abs() * e.y + r[8].abs() * e.z,
    );
    (s.f.pos() - x, s.f.pos() + x)
}

/// The FaceVertex feature's two bodies: (P, Q) with P's role in the contact.
struct Pq<'a> {
    p: &'a Shape,
    q: &'a Shape,
    mp: Mesh,
    mq: Mesh,
    /// P is contact A.
    p_is_a: bool,
}

fn pq<'a>(a: &'a Shape, b: &'a Shape, p_is_a: bool) -> Pq<'a> {
    let (p, q) = if p_is_a { (a, b) } else { (b, a) };
    Pq { p, q, mp: Mesh::of(p), mq: Mesh::of(q), p_is_a }
}

/// FaceVertex.Separation Studio (GEOM §3.2): updates face_p, face_q, alt_q.
fn separation(a: &Shape, b: &Shape, fv: &mut FaceVertex) -> f32 {
    let x = pq(a, b, fv.p_is_a);
    // 1: Q's vertices in P's frame.
    let t = rel(&x.p.f, &x.q.f);
    let nv = x.mq.n_verts();
    let mut v = [Vec3::ZERO; 8];
    for (i, vi) in v.iter_mut().enumerate().take(nv) {
        *vi = t.to_world(x.mq.vertex(i));
    }
    let s_of = |f: usize| {
        let (n, d) = x.mp.plane(f);
        let mut best = BIG;
        let mut vi = usize::MAX;
        for (i, p) in v.iter().enumerate().take(nv) {
            let s = ((n.x * p.x + n.y * p.y) + n.z * p.z) - d;
            if s < best {
                best = s;
                vi = i;
            }
        }
        (best, vi)
    };
    // 2–3.
    let (mut best, mut vi) = s_of(fv.face_p as usize);
    let mut face_q = -1i32;
    if best <= 0.0 {
        // 4: a face of P with a larger separation.
        for f in 0..x.mp.n_faces() {
            if f as i32 == fv.face_p {
                continue;
            }
            let (s, i) = s_of(f);
            if best < s {
                fv.face_p = f as i32;
                best = s;
                vi = i;
                if 0.0 < s {
                    fv.face_q = -1;
                    return best;
                }
            }
        }
        // 5: Q's face around vertex vi most anti-parallel to P's face.
        fv.alt_q = -1;
        let t2 = rel(&x.q.f, &x.p.f);
        let (np, _) = x.mp.plane(fv.face_p as usize);
        let nq = t2.rot(np);
        let mut score = -2.0f32;
        let mut prev = -1i32;
        if vi != usize::MAX {
            for k in 0..x.mq.vert_len(vi) {
                let g = x.mq.vert_face(vi, k);
                let (gn, _) = x.mq.plane(g);
                let sc = ((-gn.x) * nq.x - nq.y * gn.y) - nq.z * gn.z;
                if score < sc {
                    // .rdata Studio = 0.4, Studio = 0.3.
                    if (sc - score).abs() < 0.4 && 0.3 < score {
                        fv.alt_q = prev;
                    }
                    score = sc;
                    prev = g as i32;
                }
            }
        }
        face_q = prev;
    }
    fv.face_q = face_q;
    best
}

/// ChooseFeature Studio (GEOM §3.2): the separating feature (contact) for
/// this tick; candidates are faces of A or of B only.
fn choose(a: &Shape, b: &Shape, st: &mut PairState) {
    let mut cur = st.fv.unwrap_or(FaceVertex { p_is_a: true, face_p: 0, face_q: -1, alt_q: -1 });
    let mut s = separation(a, b, &mut cur);
    st.fv = Some(cur);
    if s > 0.0 {
        return;
    }
    let mut best: Option<FaceVertex> = None;
    for p_is_a in [true, false] {
        if p_is_a == cur.p_is_a {
            continue;
        }
        let mut c = FaceVertex { p_is_a, face_p: 0, face_q: -1, alt_q: -1 };
        let sc = separation(a, b, &mut c);
        if 0.0 < sc {
            st.fv = Some(c);
            return;
        }
        if s < sc {
            best = Some(c);
            s = sc;
        }
    }
    if let Some(c) = best {
        st.fv = Some(c);
    }
}

/// PolyPoly slot 18 Studio.
pub fn poly_poly(a: &Shape, b: &Shape, st: &mut PairState, margin: f32, out: &mut Temps) -> bool {
    choose(a, b, st);
    let Some(mut fv) = st.fv else { return false };
    let mut ok = manifold(a, b, &fv, margin, out);
    if !ok && fv.alt_q != -1 {
        fv.face_q = fv.alt_q;
        st.fv = Some(fv);
        ok = manifold(a, b, &fv, margin, out);
    }
    if !ok {
        st.fv = None;
    }
    ok
}

/// FaceVertex.Manifold Studio (GEOM §3.3).
fn manifold(a: &Shape, b: &Shape, fv: &FaceVertex, margin: f32, out: &mut Temps) -> bool {
    if fv.face_q == -1 {
        return false;
    }
    let x = pq(a, b, fv.p_is_a);
    let (fp, fq) = (fv.face_p as usize, fv.face_q as usize);
    let mut any = false;
    let mut cls = [0u8; 8];
    // Studio: pass 0 (Q's face against P's), pass 1 (P's face against Q's).
    let t0 = rel(&x.p.f, &x.q.f);
    let mut all_in = false;
    for pass in 0..2 {
        let (fi, mi, si, fo, mo, so, i_is_a) =
            if pass == 0 { (fp, &x.mp, x.p, fq, &x.mq, x.q, x.p_is_a) } else { (fq, &x.mq, x.q, fp, &x.mp, x.p, !x.p_is_a) };
        let t = if pass == 0 { t0 } else { rel(&si.f, &so.f) };
        let mut inn = true;
        for k in 0..mo.face_len(fo) {
            let v = mo.face_vertex(fo, k);
            let c = class(mi, fi, mo, v, &t);
            if pass == 0 && k < 8 {
                cls[k] = c;
            }
            if c != 2 {
                inn = false;
                continue;
            }
            // Vertex point Studio kind 4 (A = the face body) → Studio.
            let (n, d) = mi.plane(fi);
            let (nw, dw) = plane_world(&si.f, n, d);
            let p = so.f.to_world(mo.vertex(v));
            let sep = ((p.y * nw.y + p.x * nw.x) + p.z * nw.z) - dw;
            let tp = out.take(i_is_a, 4);
            tp.id_a = fi as i32;
            tp.id_b = v as i32;
            tp.p = p;
            tp.n = nw;
            tp.sep = sep;
            // Row point (GEOM §3.3 [L3], fitted): the reading (A = the face body, p = the
            // vertex on B, ctor arg 0 → slot 10 Studio: P = p + n·sep/2) puts it |sep|/2
            // inside A; Studio's wedge_drop first impact (tick 49) matches the midpoint
            // p − n·sep/2 (v to 3e-3, ω to 0.07, against 1.4 / 2.4 off with the literal one).
            // RUSTUDIO_SOLVER_K4_ROW=literal keeps the reading.
            if !k4_row_literal() {
                tp.p = p - nw * (sep * 0.5);
                tp.row_at_p = true;
            }
            if sep < -margin {
                any = true;
            } else {
                tp.n = Vec3::ZERO;
            }
        }
        if inn {
            all_in = true;
            break;
        }
    }
    if all_in {
        return any;
    }
    // Edge points: Q's face edges by their pass-0 classes.
    let nq = x.mq.face_len(fq);
    for k in 0..nq {
        let (v0, v1) = (x.mq.face_vertex(fq, k), x.mq.face_vertex(fq, (k + 1) % nq));
        let (c0, c1) = (cls[k], cls[(k + 1) % nq]);
        let a0 = t0.to_world(x.mq.vertex(v0));
        let a1 = t0.to_world(x.mq.vertex(v1));
        match (c0, c1) {
            // Studio: one crossing.
            (0, 3) | (3, 0) | (1, 2) | (2, 1) => {
                if let Some(s) = crossings(&x.mp, fp, a0, a1, 1)[0] {
                    any |= edge_point(&x, fp, s, v0, v1, margin, out);
                }
            }
            // Studio: up to two.
            (1, 3) | (3, 1) | (3, 3) => {
                let c = crossings(&x.mp, fp, a0, a1, 2);
                for s in c.into_iter().flatten() {
                    any |= edge_point(&x, fp, s, v0, v1, margin, out);
                }
            }
            // Studio(v0, v1) / (v1, v0): Studio.
            (2, 3) | (3, 2) => {
                let (u0, u1, b0, b1) = if c0 == 2 { (v0, v1, a0, a1) } else { (v1, v0, a1, a0) };
                if let Some(s) = crossings(&x.mp, fp, b0, b1, 1)[0] {
                    any |= edge_point(&x, fp, s, u0, u1, margin, out);
                }
            }
            _ => {}
        }
    }
    any
}

fn k4_row_literal() -> bool {
    static L: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *L.get_or_init(|| std::env::var("RUSTUDIO_SOLVER_K4_ROW").is_ok_and(|v| v == "literal"))
}

/// Studio: the sides k of P's face f the segment
/// a–b (P's frame) crosses, up to `max`. Per side: the side plane
/// separates a and b, and the crossing point lies inside the face's
/// other sides. GEOM Open 4: Studio
/// not known; the side plane is taken as the neighbouring face's plane (as
/// Studio uses), and Studio (2ac0 / 2b80) as the same test.
fn crossings(m: &Mesh, f: usize, a: Vec3, b: Vec3, max: usize) -> [Option<usize>; 2] {
    let mut out = [None, None];
    let mut n_out = 0;
    let nf = m.face_len(f);
    let (ca, cb) = (Vec3::new(clampf(a.x), clampf(a.y), clampf(a.z)), Vec3::new(clampf(b.x), clampf(b.y), clampf(b.z)));
    for k in 0..nf {
        let (n, d) = m.plane(m.neighbour(f, k));
        let sa = (ca.x * n.x + ca.y * n.y) + ca.z * n.z;
        let sb = (cb.x * n.x + cb.y * n.y) + cb.z * n.z;
        if (d <= sa) == (d <= sb) {
            continue;
        }
        let den = dot_xyz(n, b - a);
        if den == 0.0 {
            continue;
        }
        let t = (d - dot_xyz(n, a)) / den;
        let p = a + (b - a) * t;
        let inside = (0..nf).all(|j| {
            if j == k {
                return true;
            }
            let (g, gd) = m.plane(m.neighbour(f, j));
            dot_xyz(g, p) <= gd
        });
        if inside {
            out[n_out] = Some(k);
            n_out += 1;
            if n_out == max {
                break;
            }
        }
    }
    out
}

/// The edge point (Studio, kind 3; A = P, B = Q): P's face edge k against Q's
/// edge (v0, v1): closest points of the two lines (eps 1e-6), n = normalize(pA − pB)
/// (eps 1e-20), sep = −|pA − pB|; kept only if n·(P's face normal) > 0 and pA lies in
/// both parts' world AABBs; active iff sep < −margin.
fn edge_point(x: &Pq, fp: usize, k: usize, v0: usize, v1: usize, margin: f32, out: &mut Temps) -> bool {
    let eq = x.mq.edge_between(v0, v1);
    let ep = x.mp.face(fp)[1 + k] as usize;
    let tp = out.take(x.p_is_a, 3);
    tp.id_a = k as i32;
    tp.id_b = eq as i32;
    // Studio: the lines in the world.
    let (oa, da) = x.mp.edge_line(ep);
    let (ob, db) = x.mq.edge_line(eq);
    let (pa0, da) = (x.p.f.to_world(oa), x.p.f.rot(da));
    let (pb0, db) = (x.q.f.to_world(ob), x.q.f.rot(db));
    let c = (db.x * da.x + da.y * db.y) + da.z * db.z;
    let r = pb0 - pa0;
    let ra = (da.y * r.y + da.x * r.x) + da.z * r.z;
    let rb = -((db.y * r.y + db.x * r.x) + db.z * r.z);
    let den = 1.0 - c * c;
    tp.n = Vec3::ZERO;
    if !(1e-6 < den) {
        return false;
    }
    let inv = 1.0 / den;
    let ta = (rb * c + ra) * inv;
    let pa = Vec3::new(da.x * ta + pa0.x, da.y * ta + pa0.y, da.z * ta + pa0.z);
    let tb = (ra * c + rb) * inv;
    let pb = Vec3::new(db.x * tb + pb0.x, db.y * tb + pb0.y, db.z * tb + pb0.z);
    let mut n = pa - pb;
    let l2 = n.x * n.x + n.y * n.y + n.z * n.z;
    let l = l2.sqrt();
    // Studio with eps 1e-20 (.rdata Studio): returns 0 when not normalised.
    let len = if l <= 1e-20 { 0.0 } else {
        let k = 1.0 / l;
        n = Vec3::new(n.x * k, n.y * k, n.z * k);
        l
    };
    let sep = -len;
    let (fnl, _) = x.mp.plane(fp);
    let fw = x.p.f.rot(fnl);
    if !(0.0 < (n.y * fw.y + n.x * fw.x) + n.z * fw.z) {
        return false;
    }
    for s in [x.p, x.q] {
        let (lo, hi) = aabb(s);
        if !(lo.x <= pa.x && lo.y <= pa.y && lo.z <= pa.z && pa.x <= hi.x && pa.y <= hi.y && pa.z <= hi.z) {
            return false;
        }
    }
    tp.p = pa;
    tp.n = n;
    tp.sep = sep;
    if sep < -margin {
        true
    } else {
        tp.n = Vec3::ZERO;
        false
    }
}

// ------------------------------------------------------------------ ball–poly

/// Studio (wedge) / Studio (corner wedge) stand-in (GEOM Open 4: not
/// known): closest point q of the poly to c (local), g from the surface toward
/// c (the outward normal of the nearest face when c is inside), f = signed squared
/// distance.
pub fn closest_poly(c: Vec3, b: &Shape) -> (Vec3, Vec3, f32) {
    let m = Mesh::of(b);
    let nf = m.n_faces();
    // Inside: the face with the largest (≤ 0) plane distance.
    let mut smax = -BIG;
    let mut fmax = 0;
    for f in 0..nf {
        let (n, d) = m.plane(f);
        let s = dot_xyz(n, c) - d;
        if s > smax {
            smax = s;
            fmax = f;
        }
    }
    if smax <= 0.0 {
        let (n, _) = m.plane(fmax);
        return (c - n * smax, n, -(smax * smax));
    }
    // Outside: faces (projection inside the polygon), then edges / vertices.
    let mut best = (c, Vec3::Y, BIG);
    for f in 0..nf {
        let (n, d) = m.plane(f);
        let s = dot_xyz(n, c) - d;
        if s < 0.0 {
            continue;
        }
        let q = c - n * s;
        let k = m.face_len(f);
        let inside = (0..k).all(|i| {
            let (p0, p1) = (m.vertex(m.face_vertex(f, i)), m.vertex(m.face_vertex(f, (i + 1) % k)));
            (p1 - p0).cross(q - p0).dot(n) >= 0.0
        });
        if inside && s * s < best.2 {
            best = (q, c - q, s * s);
        }
    }
    let ne = [12, 9, 8][m.kind as usize];
    for e in 0..ne {
        let x = m.edge(e);
        let (p0, p1) = (m.vertex(x[0] as usize), m.vertex(x[1] as usize));
        let dd = p1 - p0;
        let t = ((c - p0).dot(dd) / dd.length_squared()).clamp(0.0, 1.0);
        let q = p0 + dd * t;
        let g = c - q;
        if g.length_squared() < best.2 {
            best = (q, g, g.length_squared());
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topology_closes() {
        for kind in 0..3u8 {
            let m = Mesh { kind, size: Vec3::new(2.0, 3.0, 4.0) };
            for f in 0..m.n_faces() {
                let (n, d) = m.plane(f);
                for k in 0..m.face_len(f) {
                    let v = m.vertex(m.face_vertex(f, k));
                    assert!((dot_xyz(n, v) - d).abs() < 1e-5, "kind {kind} face {f} vertex {k}");
                }
            }
            for v in 0..m.n_verts() {
                for k in 0..m.vert_len(v) {
                    let f = m.vert_face(v, k);
                    let (n, d) = m.plane(f);
                    assert!((dot_xyz(n, m.vertex(v)) - d).abs() < 1e-5, "kind {kind} vertex {v} face {f}");
                }
            }
        }
    }
}
