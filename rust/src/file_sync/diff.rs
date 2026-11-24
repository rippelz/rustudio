//! Line diffs for File Sync's preview: what a file will do to a script's
//! Source. Common lines at both ends are trimmed, the middle is an LCS
//! (a whole-block replace when it is too big to compare line by line), and
//! unchanged runs are folded to `context` lines around each change.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    Same(String),
    Del(String),
    Add(String),
    /// This many unchanged lines folded.
    Fold(usize),
}

/// The middle is compared line by line up to this many cells (old × new).
const LCS_CELLS: usize = 4_000_000;

pub fn diff(old: &str, new: &str, context: usize) -> Vec<Row> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let pre = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suf = a[pre..].iter().rev().zip(b[pre..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (am, bm) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    let mut rows: Vec<Row> = a[..pre].iter().map(|l| Row::Same(l.to_string())).collect();
    if am.len().saturating_mul(bm.len()) <= LCS_CELLS {
        // LCS lengths from the end, then walk forward.
        let (n, m) = (am.len(), bm.len());
        let mut t = vec![0u32; (n + 1) * (m + 1)];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                t[i * (m + 1) + j] = if am[i] == bm[j] { t[(i + 1) * (m + 1) + j + 1] + 1 } else { t[(i + 1) * (m + 1) + j].max(t[i * (m + 1) + j + 1]) };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n || j < m {
            if i < n && j < m && am[i] == bm[j] {
                rows.push(Row::Same(am[i].to_string()));
                i += 1;
                j += 1;
            } else if i < n && (j == m || t[(i + 1) * (m + 1) + j] >= t[i * (m + 1) + j + 1]) {
                rows.push(Row::Del(am[i].to_string()));
                i += 1;
            } else {
                rows.push(Row::Add(bm[j].to_string()));
                j += 1;
            }
        }
    } else {
        rows.extend(am.iter().map(|l| Row::Del(l.to_string())));
        rows.extend(bm.iter().map(|l| Row::Add(l.to_string())));
    }
    rows.extend(a[a.len() - suf..].iter().map(|l| Row::Same(l.to_string())));
    fold(rows, context)
}

/// Folds unchanged runs longer than 2 × context (context kept each side).
fn fold(rows: Vec<Row>, context: usize) -> Vec<Row> {
    let changed: Vec<bool> = rows.iter().map(|r| !matches!(r, Row::Same(_))).collect();
    let near = |i: usize| {
        let lo = i.saturating_sub(context);
        let hi = (i + context + 1).min(rows.len());
        changed[lo..hi].iter().any(|c| *c)
    };
    let mut out = Vec::new();
    let mut folded = 0;
    for (i, r) in rows.iter().enumerate() {
        if changed[i] || near(i) {
            if folded > 0 {
                out.push(Row::Fold(folded));
                folded = 0;
            }
            out.push(r.clone());
        } else {
            folded += 1;
        }
    }
    if folded > 0 {
        out.push(Row::Fold(folded));
    }
    out
}

/// (lines added, lines removed).
pub fn stat(rows: &[Row]) -> (usize, usize) {
    rows.iter().fold((0, 0), |(a, d), r| match r {
        Row::Add(_) => (a + 1, d),
        Row::Del(_) => (a, d + 1),
        _ => (a, d),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changes_are_found_and_unchanged_runs_folded() {
        let old = (1..=20).map(|i| format!("l{i}")).collect::<Vec<_>>().join("\n");
        let new = old.replace("l10", "ten").replace("l3\n", "");
        let rows = diff(&old, &new, 1);
        assert_eq!(stat(&rows), (1, 2));
        assert_eq!(
            rows,
            [
                Row::Fold(1),
                Row::Same("l2".into()),
                Row::Del("l3".into()),
                Row::Same("l4".into()),
                Row::Fold(4),
                Row::Same("l9".into()),
                Row::Del("l10".into()),
                Row::Add("ten".into()),
                Row::Same("l11".into()),
                Row::Fold(9),
            ]
        );
        assert!(diff("a\nb", "a\nb", 3).iter().all(|r| matches!(r, Row::Fold(_))));
        assert_eq!(stat(&diff("", "x\ny", 3)), (2, 0));
    }
}
