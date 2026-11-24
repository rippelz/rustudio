//! Islands: the awake bodies joined by contacts and joints (STEP §2.1, Studio:
//! a chain of constraints between simulated bodies; anchored / non-sim nodes bound
//! an island without joining it), each at the lowest level of its bodies, solved
//! on the ticks its level is due (STEP §2.2, Studio).
//!
//! Built every tick by union-find over the awake slots only (sleeping islands are
//! not touched, so the cost follows the awake set, not the world), into flat
//! buffers: `order` holds the slots grouped by island, `start[i]..start[i+1]` is
//! island i. No allocation once the buffers are warm. What joins an island and
//! how an island's bodies share a level (timestep arbiter, PROGRESS.md) are STEP
//! hooks.

use super::body::{self, BodySet, NONE};
use super::contact::ContactList;
use super::joints::JointList;

#[derive(Default)]
pub struct Islands {
    parent: Vec<u32>,
    root_island: Vec<u32>,
    /// Slots grouped by island.
    pub order: Vec<u32>,
    pub start: Vec<u32>,
    /// Contact manifolds / joints per island (indices into the lists).
    pub contacts: Vec<u32>,
    pub contact_start: Vec<u32>,
    pub joints: Vec<u32>,
    pub joint_start: Vec<u32>,
    /// The island's timestep level and whether it is solved this tick.
    pub level: Vec<u8>,
    pub due: Vec<bool>,
    counts: Vec<u32>,
}

fn find(p: &mut [u32], mut x: u32) -> u32 {
    while p[x as usize] != x {
        let up = p[p[x as usize] as usize];
        p[x as usize] = up;
        x = up;
    }
    x
}

fn union(p: &mut [u32], a: u32, b: u32) {
    let (ra, rb) = (find(p, a), find(p, b));
    if ra != rb {
        // Lower slot as root: deterministic.
        if ra < rb {
            p[rb as usize] = ra;
        } else {
            p[ra as usize] = rb;
        }
    }
}

impl Islands {
    pub fn len(&self) -> usize {
        self.start.len().saturating_sub(1)
    }

    pub fn bodies(&self, i: usize) -> &[u32] {
        &self.order[self.start[i] as usize..self.start[i + 1] as usize]
    }

    /// The island's contact manifolds (ContactList manifold indices).
    pub fn manifolds(&self, i: usize) -> &[u32] {
        &self.contacts[self.contact_start[i] as usize..self.contact_start[i + 1] as usize]
    }

    pub fn joint_ids(&self, i: usize) -> &[u32] {
        &self.joints[self.joint_start[i] as usize..self.joint_start[i + 1] as usize]
    }

    pub fn build(&mut self, bodies: &mut BodySet, contacts: &ContactList, joints: &JointList) {
        let n = bodies.len();
        self.parent.clear();
        self.parent.extend(0..n as u32);
        let solved = |b: &BodySet, s: u32| s != NONE && b.flags[s as usize] & (body::flag::AWAKE | body::flag::DYNAMIC) == (body::flag::AWAKE | body::flag::DYNAMIC);
        for i in 0..contacts.manifold_count() {
            let (a, b) = (contacts.a[i], contacts.b[i]);
            if solved(bodies, a) && solved(bodies, b) {
                union(&mut self.parent, a, b);
            }
        }
        for j in 0..joints.len() {
            let (a, b) = (joints.a[j], joints.b[j]);
            if solved(bodies, a) && solved(bodies, b) {
                union(&mut self.parent, a, b);
            }
        }
        // Number the roots, then counting sort the slots by island.
        self.root_island.clear();
        self.root_island.resize(n, NONE);
        self.counts.clear();
        for s in 0..n as u32 {
            if !solved(bodies, s) {
                bodies.island[s as usize] = NONE;
                continue;
            }
            let r = find(&mut self.parent, s);
            if self.root_island[r as usize] == NONE {
                self.root_island[r as usize] = self.counts.len() as u32;
                self.counts.push(0);
            }
            let isl = self.root_island[r as usize];
            bodies.island[s as usize] = isl;
            self.counts[isl as usize] += 1;
        }
        let k = self.counts.len();
        prefix(&mut self.start, &self.counts);
        self.order.clear();
        self.order.resize(self.start[k] as usize, 0);
        fill(&mut self.counts, &self.start);
        for s in 0..n as u32 {
            let isl = bodies.island[s as usize];
            if isl != NONE {
                let at = &mut self.counts[isl as usize];
                self.order[*at as usize] = s;
                *at += 1;
            }
        }
        // Manifolds and joints per island (one with an awake side and a fixed /
        // sleeping side belongs to the awake side's island).
        let side = |b: &BodySet, a: u32, c: u32| if solved(b, a) { b.island[a as usize] } else if solved(b, c) { b.island[c as usize] } else { NONE };
        self.counts.clear();
        self.counts.resize(k, 0);
        for i in 0..contacts.manifold_count() {
            let isl = side(bodies, contacts.a[i], contacts.b[i]);
            if isl != NONE {
                self.counts[isl as usize] += 1;
            }
        }
        prefix(&mut self.contact_start, &self.counts);
        self.contacts.clear();
        self.contacts.resize(self.contact_start[k] as usize, 0);
        fill(&mut self.counts, &self.contact_start);
        for i in 0..contacts.manifold_count() {
            let isl = side(bodies, contacts.a[i], contacts.b[i]);
            if isl != NONE {
                let at = &mut self.counts[isl as usize];
                self.contacts[*at as usize] = i as u32;
                *at += 1;
            }
        }
        self.counts.clear();
        self.counts.resize(k, 0);
        for j in 0..joints.len() {
            let isl = side(bodies, joints.a[j], joints.b[j]);
            if isl != NONE {
                self.counts[isl as usize] += 1;
            }
        }
        prefix(&mut self.joint_start, &self.counts);
        self.joints.clear();
        self.joints.resize(self.joint_start[k] as usize, 0);
        fill(&mut self.counts, &self.joint_start);
        for j in 0..joints.len() {
            let isl = side(bodies, joints.a[j], joints.b[j]);
            if isl != NONE {
                let at = &mut self.counts[isl as usize];
                self.joints[*at as usize] = j as u32;
                *at += 1;
            }
        }
        // Levels: "islands at their lowest level" (STEP §2.1: the node level byte
        // graph, not the list the body is in [C4]; contact connectors' edge level
        // conn = 4 never lowers it).
        // Whether it is due this tick: `set_due` (STEP §2.2).
        self.level.clear();
        self.due.clear();
        for i in 0..k {
            let lvl = self.bodies(i).iter().map(|&s| {
                let idx = bodies.handle[s as usize].into_raw_parts().0 as usize;
                bodies.persist[idx].node_level.max(1)
            }).min().unwrap_or(1);
            self.level.push(lvl);
            self.due.push(true);
        }
    }

    /// Studio: island I is solved this tick iff (p & (level(I) − 1)) == 0.
    pub fn set_due(&mut self, phase: u8) {
        for i in 0..self.level.len() {
            self.due[i] = phase & (self.level[i] - 1) == 0;
        }
    }

    /// Size histogram: islands of 1, 2–3, 4–7, 8–15, … bodies.
    pub fn histogram(&self) -> [u32; 12] {
        let mut h = [0u32; 12];
        for i in 0..self.len() {
            let n = self.start[i + 1] - self.start[i];
            let b = (32 - n.leading_zeros()).saturating_sub(1).min(11) as usize;
            h[b] += 1;
        }
        h
    }
}

fn prefix(start: &mut Vec<u32>, counts: &[u32]) {
    start.clear();
    start.push(0);
    let mut acc = 0;
    for &c in counts {
        acc += c;
        start.push(acc);
    }
}

fn fill(cursor: &mut Vec<u32>, start: &[u32]) {
    cursor.clear();
    cursor.extend_from_slice(&start[..start.len() - 1]);
}
