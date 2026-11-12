//! Finds code blocks that a patch moves verbatim from one place to another.
//!
//! A moved block is a run of removed lines that reappears, character for
//! character, as a run of added lines elsewhere in the same patch - another
//! file or another spot in the same file. Reviewers can skip such blocks:
//! nothing in them changed, only their address. Blocks with edits in flight
//! are not detected here; they stay ordinary removals and additions.

use std::collections::HashMap;

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
/// Matches are exact: whitespace differences break them, because an
/// indentation change is an edit the reviewer may care about. A match
/// shorter than 20 alphanumeric characters is ignored, the same bar
/// `git diff --color-moved` uses to keep lone braces and blank lines
/// from pairing up across the patch.
pub fn moved_blocks(patch: &str) -> Vec<MovedBlock> {
    let (removed, added) = runs(patch);

    // Every added line, addressable by content.
    let mut index: HashMap<&str, Vec<(usize, usize)>> = HashMap::new();
    for (run_index, run) in added.iter().enumerate() {
        for (offset, text) in run.texts.iter().enumerate() {
            index.entry(text).or_default().push((run_index, offset));
        }
    }
    let mut consumed: Vec<Vec<bool>> = added
        .iter()
        .map(|run| vec![false; run.texts.len()])
        .collect();

    let mut blocks = Vec::new();
    for removal in &removed {
        let mut at = 0;
        while at < removal.texts.len() {
            let Some(candidates) = index.get(removal.texts[at].as_str()) else {
                at += 1;
                continue;
            };
            // The longest verbatim continuation wins among all the places
            // this line was added.
            let best = candidates
                .iter()
                .filter(|(run, offset)| !consumed[*run][*offset])
                .map(|&(run, offset)| {
                    let length = removal.texts[at..]
                        .iter()
                        .zip(&added[run].texts[offset..])
                        .zip(&consumed[run][offset..])
                        .take_while(|((mine, theirs), used)| mine == theirs && !**used)
                        .count();
                    (length, run, offset)
                })
                .max();
            match best {
                Some((length, run, offset)) if significant(&removal.texts[at..at + length]) => {
                    for used in &mut consumed[run][offset..offset + length] {
                        *used = true;
                    }
                    blocks.push(MovedBlock {
                        from_path: removal.path.clone(),
                        from_start: removal.start + at,
                        to_path: added[run].path.clone(),
                        to_start: added[run].start + offset,
                        lines: length,
                    });
                    at += length;
                }
                _ => at += 1,
            }
        }
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
    let mut old_path = String::new();
    let mut new_path = String::new();
    let mut old_line = 0usize;
    let mut new_line = 0usize;
    let mut in_hunk = false;
    let mut removing: Option<Run> = None;
    let mut adding: Option<Run> = None;

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
            removed.extend(removing.take());
            added.extend(adding.take());
            in_hunk = false;
        } else if let Some(rest) = line.strip_prefix("@@ ") {
            removed.extend(removing.take());
            added.extend(adding.take());
            let mut sides = rest.split(' ');
            old_line = sides.next().map(start).unwrap_or(0);
            new_line = sides.next().map(start).unwrap_or(0);
            in_hunk = true;
        } else if in_hunk {
            match line.as_bytes().first() {
                Some(b'-') => {
                    added.extend(adding.take());
                    removing
                        .get_or_insert_with(|| Run {
                            path: old_path.clone(),
                            start: old_line,
                            texts: Vec::new(),
                        })
                        .texts
                        .push(line[1..].to_string());
                    old_line += 1;
                }
                Some(b'+') => {
                    removed.extend(removing.take());
                    adding
                        .get_or_insert_with(|| Run {
                            path: new_path.clone(),
                            start: new_line,
                            texts: Vec::new(),
                        })
                        .texts
                        .push(line[1..].to_string());
                    new_line += 1;
                }
                Some(b' ') => {
                    removed.extend(removing.take());
                    added.extend(adding.take());
                    old_line += 1;
                    new_line += 1;
                }
                // `\ No newline at end of file` moves neither side; any
                // other prefix ends the hunk (a new file header follows).
                Some(b'\\') => {}
                _ => {
                    removed.extend(removing.take());
                    added.extend(adding.take());
                    in_hunk = false;
                }
            }
        } else if let Some(rest) = line.strip_prefix("--- ") {
            old_path = rest.strip_prefix("a/").unwrap_or(rest).to_string();
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            new_path = rest.strip_prefix("b/").unwrap_or(rest).to_string();
        }
    }
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
        // The matching run ("fn moved_one() {" plus the tail) survives, but
        // each fragment alone stays under the significance bar.
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
