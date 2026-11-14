//! Finds where a piece of text appears inside a patch.
//!
//! The reviewer selects a name in the diff and asks where else the changeset
//! touches it. The search walks the patch text, so it sees exactly what the
//! view shows: changed lines and the context around them, nothing more.

use std::ops::Range;

use crate::patch::{PatchEvent, walk};
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
    /// Byte ranges of every needle occurrence within the line's content,
    /// so the view can mark the matched characters, not just the line.
    pub spans: Vec<Range<usize>>,
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
    let mut hits = Vec::new();
    walk(patch, |event| {
        let (path, side, line, text) = match event {
            PatchEvent::Removed { path, line, text } => (path, Side::Old, line, text),
            PatchEvent::Added { path, line, text } | PatchEvent::Context { path, line, text } => {
                (path, Side::New, line, text)
            }
            PatchEvent::Break => return,
        };
        let spans = match_ranges(text, &folded_needle, fold);
        if !spans.is_empty() {
            hits.push(SearchHit {
                path: path.to_string(),
                side,
                line,
                spans,
            });
        }
    });
    hits
}

/// Byte ranges of every `needle` occurrence in `text`, non-overlapping.
///
/// With `fold`, matching runs over the lowercased text, but folding can
/// change a character's byte length, so each folded byte remembers the
/// original character it came from and matches map back through that table.
fn match_ranges(text: &str, needle: &str, fold: bool) -> Vec<Range<usize>> {
    if !fold {
        return text
            .match_indices(needle)
            .map(|(at, found)| at..at + found.len())
            .collect();
    }
    let mut folded = String::with_capacity(text.len());
    let mut origins = Vec::with_capacity(text.len() + 1);
    for (at, c) in text.char_indices() {
        for low in c.to_lowercase() {
            folded.push(low);
            origins.resize(folded.len(), at);
        }
    }
    origins.push(text.len());
    folded
        .match_indices(needle)
        .map(|(at, found)| {
            let start = origins[at];
            let mut end = origins[at + found.len()];
            if end <= start {
                // The match ends inside a character that folded to several;
                // keep the whole original character.
                end = origins[at + found.len()..]
                    .iter()
                    .copied()
                    .find(|&origin| origin > start)
                    .unwrap_or(text.len());
            }
            start..end
        })
        .collect()
}

#[cfg(test)]
// An expected hit with one match really holds one range; clippy suspects
// every single-range vec of meaning `vec![value; count]`.
#[allow(clippy::single_range_in_vec_init)]
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
                    spans: vec![3..13],
                },
                SearchHit {
                    path: "a.rs".into(),
                    side: Side::Old,
                    line: 11,
                    spans: vec![14..24],
                },
                SearchHit {
                    path: "a.rs".into(),
                    side: Side::New,
                    line: 11,
                    spans: vec![14..24],
                },
                SearchHit {
                    path: "b.rs".into(),
                    side: Side::New,
                    line: 4,
                    spans: vec![9..19],
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
                spans: vec![0..9],
            }]
        );
    }

    #[test]
    fn folded_matches_report_spans_in_original_bytes() {
        // The Kelvin sign is three bytes and lowercases to a one-byte k, so
        // folded offsets drift from the original line's bytes.
        let patch = "\
diff --git a/u.rs b/u.rs
--- a/u.rs
+++ b/u.rs
@@ -1,1 +1,2 @@
 set \u{212A}max = 1;
+set kmax = 2;
";
        let hits = search_patch(patch, "kmax");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].spans, [4..10]);
        assert_eq!(hits[1].spans, [4..8]);
    }
}
