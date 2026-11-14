//! Finds code blocks that a patch moves verbatim from one place to another.
//!
//! A moved block is a whole run of removed lines that reappears, character
//! for character, as a whole run of added lines elsewhere in the same
//! patch - another file or another spot in the same file. Reviewers can
//! skip such blocks: nothing in them changed, only their address. Runs
//! that merely share lines stay ordinary removals and additions: a partial
//! match means the block was edited in flight, and the leftover lines on
//! either side are part of that edit, not a separate move.

use std::collections::HashMap;

use crate::patch::{PatchEvent, walk};

/// A run of lines removed in one place and added verbatim in another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovedBlock {
    /// The file the block left, named as the old side names it.
    pub from_path: String,
    /// 1-based first removed line, counted on the old side of `from_path`.
    pub from_start: usize,
    /// The file the block landed in, named as the new side names it.
    pub to_path: String,
    /// 1-based first added line, counted on the new side of `to_path`.
    pub to_start: usize,
    /// Length of the block in lines.
    pub lines: usize,
}

impl MovedBlock {
    /// Last removed line on the old side.
    pub fn from_end(&self) -> usize {
        self.from_start + self.lines - 1
    }

    /// Last added line on the new side.
    pub fn to_end(&self) -> usize {
        self.to_start + self.lines - 1
    }
}

/// One run of consecutive `-` or `+` lines, with the text of each line.
struct Run {
    path: String,
    start: usize,
    texts: Vec<String>,
}

/// Scans a unified diff for blocks moved verbatim.
///
/// A pair forms only between a whole removal run and a whole addition run
/// with identical content. Line-level matching inside runs was rejected:
/// it flagged fragments of hunks as moved even though the surrounding
/// edit meant the reviewer had to read them anyway. Matches are exact:
/// whitespace differences break them, because an indentation change is an
/// edit the reviewer may care about. A match shorter than 20 alphanumeric
/// characters is ignored, the same bar `git diff --color-moved` uses to
/// keep lone braces and blank lines from pairing up across the patch.
pub fn moved_blocks(patch: &str) -> Vec<MovedBlock> {
    let (removed, added) = runs(patch);

    // Every addition run, addressable by its full content.
    let mut index: HashMap<&[String], Vec<usize>> = HashMap::new();
    for (run_index, run) in added.iter().enumerate() {
        index
            .entry(run.texts.as_slice())
            .or_default()
            .push(run_index);
    }

    let mut blocks = Vec::new();
    for removal in &removed {
        if !significant(&removal.texts) {
            continue;
        }
        let Some(candidates) = index.get_mut(removal.texts.as_slice()) else {
            continue;
        };
        if candidates.is_empty() {
            continue;
        }
        // Identical runs pair one to one, earliest addition first.
        let run = candidates.remove(0);
        blocks.push(MovedBlock {
            from_path: removal.path.clone(),
            from_start: removal.start,
            to_path: added[run].path.clone(),
            to_start: added[run].start,
            lines: removal.texts.len(),
        });
    }
    blocks
}

/// The bar a block must clear to count as moved rather than coincidence.
fn significant(lines: &[String]) -> bool {
    let alphanumeric: usize = lines
        .iter()
        .map(|line| line.chars().filter(|c| c.is_alphanumeric()).count())
        .sum();
    alphanumeric >= 20
}

/// Splits a unified diff into runs of removed and added lines, each run
/// carrying the file it belongs to and its 1-based first line number on
/// its own side.
fn runs(patch: &str) -> (Vec<Run>, Vec<Run>) {
    let mut removed = Vec::new();
    let mut added = Vec::new();
    let mut removing: Option<Run> = None;
    let mut adding: Option<Run> = None;

    walk(patch, |event| match event {
        PatchEvent::Removed { path, line, text } => {
            added.extend(adding.take());
            removing
                .get_or_insert_with(|| Run {
                    path: path.to_string(),
                    start: line,
                    texts: Vec::new(),
                })
                .texts
                .push(text.to_string());
        }
        PatchEvent::Added { path, line, text } => {
            removed.extend(removing.take());
            adding
                .get_or_insert_with(|| Run {
                    path: path.to_string(),
                    start: line,
                    texts: Vec::new(),
                })
                .texts
                .push(text.to_string());
        }
        // A context line or a header interrupts both sides: whatever run was
        // open is complete.
        PatchEvent::Context { .. } | PatchEvent::Break => {
            removed.extend(removing.take());
            added.extend(adding.take());
        }
    });
    removed.extend(removing.take());
    added.extend(adding.take());
    (removed, added)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch(sections: &[(&str, &str)]) -> String {
        sections
            .iter()
            .map(|(path, hunks)| {
                format!("diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n{hunks}")
            })
            .collect()
    }

    const BLOCK: &str = "fn moved_one() {\n    let total = alpha + beta;\n    total * gamma\n}";

    fn minus(text: &str) -> String {
        text.lines().map(|line| format!("-{line}\n")).collect()
    }

    fn plus(text: &str) -> String {
        text.lines().map(|line| format!("+{line}\n")).collect()
    }

    #[test]
    fn a_block_moved_across_files_is_found() {
        let text = patch(&[
            ("a.rs", &format!("@@ -10,4 +9,0 @@\n{}", minus(BLOCK))),
            ("b.rs", &format!("@@ -0,0 +5,4 @@\n{}", plus(BLOCK))),
        ]);
        assert_eq!(
            moved_blocks(&text),
            [MovedBlock {
                from_path: "a.rs".into(),
                from_start: 10,
                to_path: "b.rs".into(),
                to_start: 5,
                lines: 4,
            }]
        );
    }

    #[test]
    fn a_block_moved_inside_one_file_is_found() {
        let text = patch(&[(
            "a.rs",
            &format!(
                "@@ -10,5 +10,1 @@\n context\n{}@@ -40,1 +37,5 @@\n context\n{}",
                minus(BLOCK),
                plus(BLOCK)
            ),
        )]);
        let blocks = moved_blocks(&text);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].from_start, 11);
        assert_eq!(blocks[0].to_start, 38);
        assert_eq!(blocks[0].from_end(), 14);
        assert_eq!(blocks[0].to_end(), 41);
    }

    #[test]
    fn an_edited_block_is_not_moved() {
        let edited = BLOCK.replace("alpha", "delta");
        let text = patch(&[
            ("a.rs", &format!("@@ -10,4 +9,0 @@\n{}", minus(BLOCK))),
            ("b.rs", &format!("@@ -0,0 +5,4 @@\n{}", plus(&edited))),
        ]);
        // The runs share most lines, but only whole-run matches pair.
        assert_eq!(moved_blocks(&text), []);
    }

    #[test]
    fn tiny_coincidences_stay_quiet() {
        let text = patch(&[
            ("a.rs", "@@ -3,1 +2,0 @@\n-}\n"),
            ("b.rs", "@@ -0,0 +9,1 @@\n+}\n"),
        ]);
        assert_eq!(moved_blocks(&text), []);
    }

    #[test]
    fn an_indentation_change_breaks_the_match() {
        let indented: String = BLOCK
            .lines()
            .map(|line| format!("    {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let text = patch(&[
            ("a.rs", &format!("@@ -10,4 +9,0 @@\n{}", minus(BLOCK))),
            ("b.rs", &format!("@@ -0,0 +5,4 @@\n{}", plus(&indented))),
        ]);
        assert_eq!(moved_blocks(&text), []);
    }

    #[test]
    fn a_fragment_of_a_larger_removal_is_not_moved() {
        let enlarged = format!("{BLOCK}\nextra_tail_line(epsilon);");
        let text = patch(&[
            ("a.rs", &format!("@@ -10,5 +9,0 @@\n{}", minus(&enlarged))),
            ("b.rs", &format!("@@ -0,0 +5,4 @@\n{}", plus(BLOCK))),
        ]);
        // The extra removed line means the block was edited on its way,
        // so the whole run stays an ordinary removal.
        assert_eq!(moved_blocks(&text), []);
    }

    #[test]
    fn each_added_line_pairs_with_one_removal_only() {
        let text = patch(&[
            ("a.rs", &format!("@@ -10,4 +9,0 @@\n{}", minus(BLOCK))),
            ("c.rs", &format!("@@ -20,4 +19,0 @@\n{}", minus(BLOCK))),
            ("b.rs", &format!("@@ -0,0 +5,4 @@\n{}", plus(BLOCK))),
        ]);
        // Two identical removals, one addition: only one pair forms.
        assert_eq!(moved_blocks(&text).len(), 1);
    }
}
