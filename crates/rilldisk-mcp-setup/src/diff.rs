// SPDX-License-Identifier: AGPL-3.0-only
//! A small line diff for previews. Our edits touch a few adjacent lines, so the
//! common prefix and suffix are trimmed first and only the middle is compared.

/// Unified diff of `before` and `after` with two lines of context. Empty when equal.
pub fn unified(name: &str, before: &str, after: &str) -> String {
    if before == after {
        return String::new();
    }
    let a: Vec<&str> = before.lines().collect();
    let b: Vec<&str> = after.lines().collect();
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (mid_a, mid_b) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);
    // Longest common subsequence over the changed middle only.
    let (n, m) = (mid_a.len(), mid_b.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if mid_a[i] == mid_b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut ops: Vec<(char, &str)> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && mid_a[i] == mid_b[j] {
            ops.push((' ', mid_a[i]));
            i += 1;
            j += 1;
        } else if i < n && (j == m || lcs[i + 1][j] >= lcs[i][j + 1]) {
            // Removals before additions, as unified diffs show them.
            ops.push(('-', mid_a[i]));
            i += 1;
        } else {
            ops.push(('+', mid_b[j]));
            j += 1;
        }
    }
    const CONTEXT: usize = 2;
    let start = prefix.saturating_sub(CONTEXT);
    let end_a = (a.len() - suffix + CONTEXT).min(a.len());
    let end_b = (b.len() - suffix + CONTEXT).min(b.len());
    // An empty range starts at line 0 (`@@ -0,0 +1,9 @@` for a new file).
    let range = |len: usize| format!("{},{len}", if len == 0 { start } else { start + 1 });
    let mut out = format!("--- {name}\n+++ {name} (after)\n");
    out.push_str(&format!(
        "@@ -{} +{} @@\n",
        range(end_a - start),
        range(end_b - start)
    ));
    for line in &a[start..prefix] {
        out.push_str(&format!(" {line}\n"));
    }
    for (op, line) in ops {
        out.push_str(&format!("{op}{line}\n"));
    }
    for line in &a[a.len() - suffix..end_a] {
        out.push_str(&format!(" {line}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::unified;

    #[test]
    fn shows_only_the_changed_region() {
        let before = "a\nb\nc\nd\ne\nf\n";
        let after = "a\nb\nc\nX\nd\ne\nf\n";
        assert_eq!(
            unified("f", before, after),
            "--- f\n+++ f (after)\n@@ -2,4 +2,5 @@\n b\n c\n+X\n d\n e\n"
        );
        assert_eq!(unified("f", before, before), "");
        assert_eq!(
            unified("f", "", "{}\n"),
            "--- f\n+++ f (after)\n@@ -0,0 +1,1 @@\n+{}\n"
        );
        // A changed line reads as removal, then addition.
        assert_eq!(
            unified("f", "a\nold\nz\n", "a\nnew\nz\n"),
            "--- f\n+++ f (after)\n@@ -1,3 +1,3 @@\n a\n-old\n+new\n z\n"
        );
    }
}
