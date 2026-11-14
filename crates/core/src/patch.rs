//! One walker for unified-diff text.
//!
//! Search, moved-block detection and region parsing all read the same patch
//! format. A single state machine keeps the header rules (a removed line
//! that itself starts with dashes renders as `--- text` inside a hunk and
//! must not be read as a header) from drifting apart between copies.

use crate::model::ChangedRegion;

/// One step of a patch walk. Content lines carry their text without the
/// `-`/`+`/space prefix and a 1-based line number on their own side.
pub(crate) enum PatchEvent<'a> {
    /// A file or hunk header: no run of content lines continues across it.
    Break,
    /// A removed line, numbered on the old side of `path`.
    Removed {
        path: &'a str,
        line: usize,
        text: &'a str,
    },
    /// An added line, numbered on the new side of `path`.
    Added {
        path: &'a str,
        line: usize,
        text: &'a str,
    },
    /// An unchanged line. It exists on both sides; `path` and `line` name
    /// the new one, where the reviewer reads.
    Context {
        path: &'a str,
        line: usize,
        text: &'a str,
    },
}

/// Walks `patch` and hands every event to `emit`, in patch order.
pub(crate) fn walk(patch: &str, mut emit: impl FnMut(PatchEvent<'_>)) {
    let mut old_path = "";
    let mut new_path = "";
    let mut old_line = 0usize;
    let mut new_line = 0usize;
    let mut in_hunk = false;

    for line in patch.lines() {
        if line.starts_with("diff --git ") {
            // Only after this line may `---`/`+++` headers follow; anywhere
            // else they are content (see the module doc).
            in_hunk = false;
            emit(PatchEvent::Break);
        } else if let Some(region) = hunk_header(line) {
            old_line = region.old_start;
            new_line = region.new_start;
            in_hunk = true;
            emit(PatchEvent::Break);
        } else if in_hunk {
            match line.as_bytes().first() {
                Some(b'-') => {
                    emit(PatchEvent::Removed {
                        path: old_path,
                        line: old_line,
                        text: &line[1..],
                    });
                    old_line += 1;
                }
                Some(b'+') => {
                    emit(PatchEvent::Added {
                        path: new_path,
                        line: new_line,
                        text: &line[1..],
                    });
                    new_line += 1;
                }
                Some(b' ') => {
                    emit(PatchEvent::Context {
                        path: new_path,
                        line: new_line,
                        text: &line[1..],
                    });
                    old_line += 1;
                    new_line += 1;
                }
                // `\ No newline at end of file` belongs to neither side; any
                // other prefix means the hunk is over (a header follows).
                Some(b'\\') => {}
                _ => {
                    in_hunk = false;
                    emit(PatchEvent::Break);
                }
            }
        } else if let Some(rest) = line.strip_prefix("--- ") {
            old_path = rest.strip_prefix("a/").unwrap_or(rest);
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            new_path = rest.strip_prefix("b/").unwrap_or(rest);
        }
    }
}

/// Parses `@@ -old[,n] +new[,m] @@ ...`. A missing length means one line.
pub(crate) fn hunk_header(line: &str) -> Option<ChangedRegion> {
    let span = |text: &str| -> Option<(usize, usize)> {
        match text.split_once(',') {
            Some((start, lines)) => Some((start.parse().ok()?, lines.parse().ok()?)),
            None => Some((text.parse().ok()?, 1)),
        }
    };
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, _) = rest.split_once(" @@")?;
    let (old_start, old_lines) = span(old)?;
    let (new_start, new_lines) = span(new)?;
    Some(ChangedRegion {
        old_start,
        old_lines,
        new_start,
        new_lines,
    })
}
