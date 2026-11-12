//! Finds where a piece of text appears inside a patch.
//!
//! The reviewer selects a name in the diff and asks where else the changeset
//! touches it. The search walks the patch text, so it sees exactly what the
//! view shows: changed lines and the context around them, nothing more.

use crate::review::Side;

/// One line of the patch containing the needle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// The file, named as its own side of the diff names it.
    pub path: String,
    /// The side whose line number `line` is. Context lines report as
    /// [`Side::New`]: they exist on both sides and the new one is where
    /// the reviewer reads.
    pub side: Side,
    /// 1-based line number on `side`.
    pub line: usize,
}

/// Every patch line containing `needle`, in patch order.
///
/// Matching is smartcase, like the owner's editors: a needle without an
/// uppercase letter matches case-insensitively, one with matches exactly.
/// An empty needle matches nothing.
pub fn search_patch(patch: &str, needle: &str) -> Vec<SearchHit> {
    if needle.is_empty() {
        return Vec::new();
    }
    let fold = !needle.chars().any(|c| c.is_uppercase());
    let folded_needle = if fold {
        needle.to_lowercase()
    } else {
        needle.to_string()
    };
    let matches = |text: &str| {
        if fold {
            text.to_lowercase().contains(&folded_needle)
        } else {
            text.contains(&folded_needle)
        }
    };

    let mut hits = Vec::new();
    let mut old_path = String::new();
    let mut new_path = String::new();
    let mut old_line = 0usize;
    let mut new_line = 0usize;
    let mut in_hunk = false;
    // 1-based start of `-old[,len]` or `+new[,len]`; a missing length means 1.
    let start = |spec: &str| -> usize {
        spec[1..]
            .split(',')
            .next()
            .and_then(|number| number.parse().ok())
            .unwrap_or(0)
    };

    for line in patch.lines() {
        if line.starts_with("diff --git ") {
            // Only here may header lines follow: a removed line that itself
            // started with dashes renders as `--- text` inside a hunk and
            // must not be read as a header.
            in_hunk = false;
        } else if let Some(rest) = line.strip_prefix("@@ ") {
            let mut sides = rest.split(' ');
            old_line = sides.next().map(start).unwrap_or(0);
            new_line = sides.next().map(start).unwrap_or(0);
            in_hunk = true;
        } else if in_hunk {
            match line.as_bytes().first() {
                Some(b'-') => {
                    if matches(&line[1..]) {
                        hits.push(SearchHit {
                            path: old_path.clone(),
                            side: Side::Old,
                            line: old_line,
                        });
                    }
                    old_line += 1;
                }
                Some(b'+') => {
                    if matches(&line[1..]) {
                        hits.push(SearchHit {
                            path: new_path.clone(),
                            side: Side::New,
                            line: new_line,
                        });
                    }
                    new_line += 1;
                }
                Some(b' ') => {
                    if matches(&line[1..]) {
                        hits.push(SearchHit {
                            path: new_path.clone(),
                            side: Side::New,
                            line: new_line,
                        });
                    }
                    old_line += 1;
                    new_line += 1;
                }
                Some(b'\\') => {}
                _ => in_hunk = false,
            }
        } else if let Some(rest) = line.strip_prefix("--- ") {
            old_path = rest.strip_prefix("a/").unwrap_or(rest).to_string();
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            new_path = rest.strip_prefix("b/").unwrap_or(rest).to_string();
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATCH: &str = "\
diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -10,4 +10,4 @@
 fn frobnicate() {
-    let old = frobnicate_impl();
+    let new = frobnicate_impl();
 }
diff --git a/b.rs b/b.rs
--- a/b.rs
+++ b/b.rs
@@ -3,2 +3,3 @@
 fn other() {}
+fn calls_frobnicate() {}
";

    #[test]
    fn hits_land_on_every_side_with_their_own_line_numbers() {
        let hits = search_patch(PATCH, "frobnicate");
        assert_eq!(
            hits,
            [
                SearchHit {
                    path: "a.rs".into(),
                    side: Side::New,
                    line: 10,
                },
                SearchHit {
                    path: "a.rs".into(),
                    side: Side::Old,
                    line: 11,
                },
                SearchHit {
                    path: "a.rs".into(),
                    side: Side::New,
                    line: 11,
                },
                SearchHit {
                    path: "b.rs".into(),
                    side: Side::New,
                    line: 4,
                },
            ]
        );
    }

    #[test]
    fn smartcase_folds_only_lowercase_needles() {
        assert_eq!(search_patch(PATCH, "FROBNICATE"), []);
        assert_eq!(search_patch(PATCH, "Frobnicate"), []);
        assert_eq!(search_patch(PATCH, "frobnicate").len(), 4);
    }

    #[test]
    fn an_empty_needle_matches_nothing() {
        assert_eq!(search_patch(PATCH, ""), []);
    }

    #[test]
    fn a_deleted_file_reports_hits_on_its_old_path() {
        // The new side is /dev/null; hits must name the file the view shows.
        let patch = "\
diff --git a/gone.rs b/gone.rs
--- a/gone.rs
+++ /dev/null
@@ -1,2 +0,0 @@
-fn foobar() {}
-fn keep() {}
";
        assert_eq!(
            search_patch(patch, "fn foobar"),
            [SearchHit {
                path: "gone.rs".into(),
                side: Side::Old,
                line: 1,
            }]
        );
    }
}
