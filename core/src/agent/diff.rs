//! Line diffs for edit tool calls: enough for +/- counts and a short preview.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Add,
    Del,
}

#[derive(Clone, Debug)]
pub struct DiffLine {
    pub kind: LineKind,
    /// Line number in the new text (old text for deletions).
    pub number: Option<u32>,
    pub text: String,
}

/// Cap on the quadratic part; beyond it Myers' diff takes over.
const LCS_CELLS: usize = 1_000_000;
/// Beyond this many changed lines the middle shows as replaced wholesale.
const MAX_CHANGED: usize = 2000;

pub fn line_diff(old: &str, new: &str) -> Vec<DiffLine> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (am, bm) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);

    let mut out = Vec::with_capacity(a.len().max(b.len()) + 8);
    let (mut old_no, mut new_no) = (0u32, 0u32);
    let mut push = |kind: LineKind, text: &str, old_no: &mut u32, new_no: &mut u32| {
        let number = match kind {
            LineKind::Context => {
                *old_no += 1;
                *new_no += 1;
                *new_no
            }
            LineKind::Add => {
                *new_no += 1;
                *new_no
            }
            LineKind::Del => {
                *old_no += 1;
                *old_no
            }
        };
        out.push(DiffLine {
            kind,
            number: Some(number),
            text: text.to_string(),
        });
    };

    for line in &a[..prefix] {
        push(LineKind::Context, line, &mut old_no, &mut new_no);
    }
    if am.len().saturating_mul(bm.len()) <= LCS_CELLS {
        let (n, m) = (am.len(), bm.len());
        let w = m + 1;
        let mut dp = vec![0u32; (n + 1) * w];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                dp[i * w + j] = if am[i] == bm[j] {
                    dp[(i + 1) * w + j + 1] + 1
                } else {
                    dp[(i + 1) * w + j].max(dp[i * w + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n || j < m {
            if i < n && j < m && am[i] == bm[j] {
                push(LineKind::Context, am[i], &mut old_no, &mut new_no);
                i += 1;
                j += 1;
            } else if j < m && (i == n || dp[i * w + j + 1] >= dp[(i + 1) * w + j]) {
                push(LineKind::Add, bm[j], &mut old_no, &mut new_no);
                j += 1;
            } else {
                push(LineKind::Del, am[i], &mut old_no, &mut new_no);
                i += 1;
            }
        }
    } else if let Some(ops) = myers(am, bm, MAX_CHANGED) {
        let (mut i, mut j) = (0, 0);
        for op in ops {
            let line = match op {
                LineKind::Add => bm[j],
                _ => am[i],
            };
            push(op, line, &mut old_no, &mut new_no);
            i += usize::from(op != LineKind::Add);
            j += usize::from(op != LineKind::Del);
        }
    } else {
        for line in am {
            push(LineKind::Del, line, &mut old_no, &mut new_no);
        }
        for line in bm {
            push(LineKind::Add, line, &mut old_no, &mut new_no);
        }
    }
    for line in &a[a.len() - suffix..] {
        push(LineKind::Context, line, &mut old_no, &mut new_no);
    }
    out
}

/// Myers' O(ND) diff of `a` into `b`, as one kind per step; `None` when more
/// than `max_d` lines differ.
fn myers(a: &[&str], b: &[&str], max_d: usize) -> Option<Vec<LineKind>> {
    let (n, m) = (a.len() as i32, b.len() as i32);
    let max_d = max_d.min(a.len() + b.len()) as i32;
    let off = max_d + 1;
    let mut v = vec![0i32; 2 * max_d as usize + 3];
    // Before round d, the furthest x reached on diagonals -d..=d.
    let mut trace: Vec<Vec<i32>> = Vec::new();
    for d in 0..=max_d {
        trace.push(v[(off - d) as usize..=(off + d) as usize].to_vec());
        for k in (-d..=d).step_by(2) {
            let down = k == -d || (k != d && v[(off + k - 1) as usize] < v[(off + k + 1) as usize]);
            let mut x = if down {
                v[(off + k + 1) as usize]
            } else {
                v[(off + k - 1) as usize] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[(off + k) as usize] = x;
            if x >= n && y >= m {
                return Some(backtrack(&trace, n, m));
            }
        }
    }
    None
}

fn backtrack(trace: &[Vec<i32>], n: i32, m: i32) -> Vec<LineKind> {
    let (mut x, mut y) = (n, m);
    let mut ops = Vec::new();
    for d in (1..trace.len() as i32).rev() {
        let prev = &trace[d as usize];
        let at = |k: i32| prev[(k + d) as usize];
        let k = x - y;
        let down = k == -d || (k != d && at(k - 1) < at(k + 1));
        let prev_k = if down { k + 1 } else { k - 1 };
        let prev_x = at(prev_k);
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            ops.push(LineKind::Context);
            x -= 1;
            y -= 1;
        }
        ops.push(if down { LineKind::Add } else { LineKind::Del });
        (x, y) = (prev_x, prev_y);
    }
    while x > 0 && y > 0 {
        ops.push(LineKind::Context);
        x -= 1;
        y -= 1;
    }
    ops.reverse();
    ops
}
pub fn stats(lines: &[DiffLine]) -> (u32, u32) {
    lines.iter().fold((0, 0), |(a, d), l| match l.kind {
        LineKind::Add => (a + 1, d),
        LineKind::Del => (a, d + 1),
        LineKind::Context => (a, d),
    })
}

/// The changed lines with `context` lines around each run; `None` marks a gap.
pub fn hunks(lines: &[DiffLine], context: usize) -> Vec<Option<&DiffLine>> {
    let mut out = Vec::new();
    for range in hunk_ranges(lines, context) {
        if !out.is_empty() {
            out.push(None);
        }
        out.extend(lines[range].iter().map(Some));
    }
    out
}

/// Where each hunk of changed lines (with `context` lines around it) sits in `lines`.
pub fn hunk_ranges(lines: &[DiffLine], context: usize) -> Vec<std::ops::Range<usize>> {
    let mut keep = vec![false; lines.len()];
    for (i, l) in lines.iter().enumerate() {
        if l.kind != LineKind::Context {
            let lo = i.saturating_sub(context);
            let hi = (i + context + 1).min(lines.len());
            keep[lo..hi].iter_mut().for_each(|k| *k = true);
        }
    }
    let mut out: Vec<std::ops::Range<usize>> = Vec::new();
    for (i, kept) in keep.iter().enumerate() {
        if !kept {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.end == i => last.end = i + 1,
            _ => out.push(i..i + 1),
        }
    }
    out
}

/// A hunk's header as git writes it: `@@ -old,len +new,len @@`, followed by the
/// nearest line above it that starts at the margin, which usually names the
/// function or block the change lives in.
pub fn hunk_header(lines: &[DiffLine], range: std::ops::Range<usize>, new_text: &str) -> String {
    let (mut old_start, mut new_start) = (1u32, 1u32);
    for l in &lines[..range.start] {
        match l.kind {
            LineKind::Context => {
                old_start += 1;
                new_start += 1;
            }
            LineKind::Del => old_start += 1,
            LineKind::Add => new_start += 1,
        }
    }
    let (mut old_len, mut new_len) = (0u32, 0u32);
    for l in &lines[range.clone()] {
        match l.kind {
            LineKind::Context => {
                old_len += 1;
                new_len += 1;
            }
            LineKind::Del => old_len += 1,
            LineKind::Add => new_len += 1,
        }
    }
    let above: Vec<&str> = new_text
        .lines()
        .take(new_start.saturating_sub(1) as usize)
        .collect();
    let context = above
        .iter()
        .rev()
        .find(|l| {
            l.chars()
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
        })
        .map(|l| format!(" {}", l.trim_end().chars().take(80).collect::<String>()))
        .unwrap_or_default();
    // As git does: an empty side points at the line before it, and a single line has no count.
    let side = |start: u32, len: u32| match len {
        0 => format!("{},0", start.saturating_sub(1)),
        1 => start.to_string(),
        _ => format!("{start},{len}"),
    };
    format!(
        "@@ -{} +{} @@{context}",
        side(old_start, old_len),
        side(new_start, new_len)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts() {
        let d = line_diff("a\nb\nc\n", "a\nB\nc\nd\n");
        assert_eq!(stats(&d), (2, 1));
        let d = line_diff("", "hi\n");
        assert_eq!(stats(&d), (1, 0));
    }

    #[test]
    fn big_files_with_scattered_changes_count_only_those() {
        let old: String = (0..3000).map(|i| format!("line {i}\n")).collect();
        let new = old
            .replace("line 10\n", "ten\n")
            .replace("line 2990\n", "")
            .replace("line 1500\n", "line 1500\nadded\n");
        let d = line_diff(&old, &new);
        assert_eq!(stats(&d), (2, 2));
        let rebuilt: Vec<&str> = d
            .iter()
            .filter(|l| l.kind != LineKind::Del)
            .map(|l| l.text.as_str())
            .collect();
        assert_eq!(rebuilt, new.lines().collect::<Vec<_>>());
        let kept: Vec<&str> = d
            .iter()
            .filter(|l| l.kind != LineKind::Add)
            .map(|l| l.text.as_str())
            .collect();
        assert_eq!(kept, old.lines().collect::<Vec<_>>());
        assert_eq!(
            myers(&["a", "b"], &["a", "c"], 5).map(|ops| ops.len()),
            Some(3)
        );
        assert_eq!(myers(&["a", "b"], &["c", "d"], 3), None);
    }

    #[test]
    fn hunk_headers_read_like_gits() {
        let old: String = (1..=30).map(|i| format!("    line {i}\n")).collect();
        let new = old
            .replace("    line 5\n", "fn five() {\n    five\n}\n")
            .replace("    line 25\n", "");
        let lines = line_diff(&old, &new);
        let ranges = hunk_ranges(&lines, 2);
        assert_eq!(ranges.len(), 2);
        assert_eq!(hunks(&lines, 2).iter().filter(|l| l.is_none()).count(), 1);
        // Both sides counted, deletions and additions alike; the function above names the hunk.
        assert_eq!(
            hunk_header(&lines, ranges[0].clone(), &new),
            "@@ -3,5 +3,7 @@"
        );
        assert_eq!(
            hunk_header(&lines, ranges[1].clone(), &new),
            "@@ -23,5 +25,4 @@ fn five() {"
        );
        // A brand-new file starts from nothing.
        let lines = line_diff("", "a\nb\n");
        let ranges = hunk_ranges(&lines, 3);
        assert_eq!(
            hunk_header(&lines, ranges[0].clone(), "a\nb\n"),
            "@@ -0,0 +1,2 @@"
        );
    }
}
