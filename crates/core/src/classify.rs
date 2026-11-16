//! Separates mechanical changes from meaningful ones.
//!
//! This module holds the output types of the whole classification pipeline
//! and its step 0: the file rules that need no parser - Generated, Renamed
//! file, and whitespace-only Formatting. A file settled by
//! [`classify_file_rules`] skips every later step. The parser-based steps -
//! item lists, formatting and comment-only in place, moves, moved-and-edited
//! pairs and copies - live in the submodules and run behind [`classify`],
//! the one entry point that sees all changed files at once.
//!
//! Everything here is a pure function of texts, changed regions and
//! caller-resolved facts (rename similarity, `.gitattributes`, blob ids
//! for the parse cache). No git access, so it is unit testable without a
//! repository.

mod cache;
mod histogram;
mod items;
mod pipeline;

pub use cache::ParseCache;
pub use pipeline::{ClassifyInput, FileClassification, classify};

use crate::model::ChangedRegion;

/// Which side of the comparison a range of lines lives on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Old,
    New,
}

/// The class a changed line falls into. Every changed line gets exactly one.
///
/// A mechanical class is provable and foldable. `CommentOnly` is its own
/// bucket, never folded by default. The remaining classes are meaningful
/// but carry help (a counterpart, an inner diff).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Class {
    /// Gone from one place, present in another, identical token stream.
    Moved,
    /// Git reports the file renamed at 100% similarity.
    RenamedFile,
    /// Same place, same content up to whitespace and line breaks.
    Formatting,
    /// Not written by hand: matched list, attribute, or `@generated` marker.
    Generated,
    /// Only comment or doc-comment tokens differ.
    CommentOnly,
    /// Paired with a similar item elsewhere; the differing lines stay red
    /// and green.
    MovedAndEdited,
    /// Identical tokens, but the surroundings that give them meaning differ.
    MovedContextChanged,
    /// Identical item in a new place while the original remains.
    Copied,
}

impl Class {
    /// True for the classes that fold: proven equal or declared not worth
    /// reading. `CommentOnly` is deliberately out - it is filterable, never
    /// folded by default.
    pub fn is_mechanical(self) -> bool {
        matches!(
            self,
            Class::Moved | Class::RenamedFile | Class::Formatting | Class::Generated
        )
    }
}

/// The other end of a pairing: where a moved or renamed range came from or
/// went to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counterpart {
    pub path: String,
    pub first_line: usize,
    pub last_line: usize,
}

/// One classified run of lines on one side of one file.
///
/// `proof` is the short phrase shown to the reviewer; it states what was
/// checked, never a judgement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassifiedRange {
    pub path: String,
    pub side: Side,
    /// 1-based, inclusive.
    pub first_line: usize,
    /// 1-based, inclusive.
    pub last_line: usize,
    pub class: Class,
    pub counterpart: Option<Counterpart>,
    pub proof: String,
}

/// Per-file totals for the scope bar and the file panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileClassSummary {
    pub path: String,
    pub meaningful_added: usize,
    pub meaningful_removed: usize,
    /// Mechanical lines on both sides together.
    pub mechanical_lines: usize,
    /// Comment-only lines on both sides together.
    pub comment_only_lines: usize,
    /// Set when every changed line shares one mechanical class; such a file
    /// moves to the file panel's mechanical group under that one word.
    pub fully_mechanical: Option<Class>,
}

/// Glob patterns naming files that are not written by hand.
///
/// Matching is deliberately dependency-free: `*` matches within one path
/// segment, `**` spans segments, and a pattern without `/` matches the file
/// name at any depth (the gitignore convention).
#[derive(Clone, Debug)]
pub struct GeneratedRules {
    patterns: Vec<String>,
}

impl Default for GeneratedRules {
    fn default() -> Self {
        Self::new(
            [
                "Cargo.lock",
                "package-lock.json",
                "yarn.lock",
                "pnpm-lock.yaml",
                "go.sum",
                "Gemfile.lock",
                "composer.lock",
                "flake.lock",
                "poetry.lock",
                "uv.lock",
                "*.min.js",
                "*.min.css",
                "*.min.*",
                "**/generated/**",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
        )
    }
}

impl GeneratedRules {
    /// Per-repository patterns will feed this once settings persistence
    /// exists; until then callers take the default list.
    pub fn new(patterns: Vec<String>) -> Self {
        Self { patterns }
    }

    pub fn matches(&self, path: &str) -> bool {
        self.patterns.iter().any(|pattern| {
            if pattern.contains('/') {
                let pat: Vec<&str> = pattern.split('/').collect();
                let segs: Vec<&str> = path.split('/').collect();
                glob_segments(&pat, &segs)
            } else {
                let name = path.rsplit_once('/').map_or(path, |(_, name)| name);
                segment_matches(pattern, name)
            }
        })
    }
}

/// `**` may swallow zero or more whole segments, so every split point is a
/// candidate; paths are short enough that backtracking costs nothing.
fn glob_segments(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|skip| glob_segments(rest, &path[skip..])),
        Some((first, rest)) => match path.split_first() {
            Some((seg, segs)) => segment_matches(first, seg) && glob_segments(rest, segs),
            None => false,
        },
    }
}

/// Wildcard match of one segment, `*` matching any run of characters.
/// Two-pointer with backtracking over bytes; `*` is ASCII, so multi-byte
/// characters still compare correctly byte for byte.
fn segment_matches(pattern: &str, text: &str) -> bool {
    let (pat, txt) = (pattern.as_bytes(), text.as_bytes());
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < txt.len() {
        if p < pat.len() && pat[p] == b'*' {
            star = Some((p, t));
            p += 1;
        } else if p < pat.len() && pat[p] == txt[t] {
            p += 1;
            t += 1;
        } else if let Some((sp, st)) = star {
            // The literal run after the last `*` failed; let the `*` eat one
            // more byte and retry.
            p = sp + 1;
            t = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    pat[p..].iter().all(|&b| b == b'*')
}

/// What git reported about a rename, resolved by the caller from the patch
/// header. `similarity` is the `similarity index` percentage.
#[derive(Clone, Debug)]
pub struct RenameInfo<'a> {
    pub old_path: &'a str,
    pub similarity: Option<u8>,
}

/// Everything step 0 looks at for one file. All facts that need git - the
/// rename header, `.gitattributes` - arrive pre-resolved, keeping the
/// classifier pure.
#[derive(Debug)]
pub struct FileRuleInput<'a> {
    /// New-side path.
    pub path: &'a str,
    pub old_text: &'a str,
    pub new_text: &'a str,
    pub regions: &'a [ChangedRegion],
    pub rename: Option<RenameInfo<'a>>,
    pub generated: &'a GeneratedRules,
    /// From `.gitattributes`: `linguist-generated` set for this path.
    pub linguist_generated: bool,
}

/// Step 0: Generated, Renamed file, and whitespace-only Formatting.
///
/// `Some` settles the whole file - later, parser-based steps must skip it.
/// `None` says only that the file rules claim nothing; it is not a class.
pub fn classify_file_rules(
    input: &FileRuleInput,
) -> Option<(Vec<ClassifiedRange>, FileClassSummary)> {
    // Generated wins over the other rules: a renamed lockfile is still not
    // worth reading, and the label should say why.
    if input.generated.matches(input.path)
        || input.linguist_generated
        || generated_marker(input.old_text)
        || generated_marker(input.new_text)
    {
        let mut ranges = Vec::new();
        for (side, text) in [(Side::Old, input.old_text), (Side::New, input.new_text)] {
            let lines = text.lines().count();
            if lines > 0 {
                ranges.push(ClassifiedRange {
                    path: input.path.to_string(),
                    side,
                    first_line: 1,
                    last_line: lines,
                    class: Class::Generated,
                    counterpart: None,
                    proof: "generated file".to_string(),
                });
            }
        }
        return Some(settled(input, ranges, Class::Generated));
    }

    if let Some(rename) = &input.rename
        && rename.similarity == Some(100)
    {
        // An empty renamed file still deserves its row, hence the floor of 1.
        let last_line = input.new_text.lines().count().max(1);
        let ranges = vec![ClassifiedRange {
            path: input.path.to_string(),
            side: Side::New,
            first_line: 1,
            last_line,
            class: Class::RenamedFile,
            counterpart: Some(Counterpart {
                path: rename.old_path.to_string(),
                first_line: 1,
                last_line,
            }),
            proof: format!("renamed from `{}`, identical", rename.old_path),
        }];
        return Some(settled(input, ranges, Class::RenamedFile));
    }

    if !input.regions.is_empty()
        && input
            .regions
            .iter()
            .all(|region| whitespace_only(input.old_text, input.new_text, region))
    {
        let mut ranges = Vec::new();
        for region in input.regions {
            for (side, start, count) in [
                (Side::Old, region.old_start, region.old_lines),
                (Side::New, region.new_start, region.new_lines),
            ] {
                if count > 0 {
                    ranges.push(ClassifiedRange {
                        path: input.path.to_string(),
                        side,
                        first_line: start,
                        last_line: start + count - 1,
                        class: Class::Formatting,
                        counterpart: None,
                        proof: "whitespace only".to_string(),
                    });
                }
            }
        }
        return Some(settled(input, ranges, Class::Formatting));
    }

    None
}

/// A rule above settled the file, so the verdict is stated outright rather
/// than re-derived from the ranges - a 100% rename has no changed lines to
/// derive it from.
fn settled(
    input: &FileRuleInput,
    ranges: Vec<ClassifiedRange>,
    class: Class,
) -> (Vec<ClassifiedRange>, FileClassSummary) {
    let mut summary = summarize(input.path, input.regions, &ranges);
    summary.fully_mechanical = Some(class);
    (ranges, summary)
}

/// True when one changed region differs only in whitespace: the removed and
/// the added lines, each split on whitespace, yield the same word sequence.
/// Re-wrapped lines pass; joined or altered words fail. Language-blind, so
/// it also applies to files the later steps cannot parse.
pub fn whitespace_only(old_text: &str, new_text: &str, region: &ChangedRegion) -> bool {
    let words = |text: &str, start: usize, count: usize| {
        text.lines()
            .skip(start.saturating_sub(1))
            .take(count)
            .flat_map(str::split_whitespace)
            .map(String::from)
            .collect::<Vec<_>>()
    };
    words(old_text, region.old_start, region.old_lines)
        == words(new_text, region.new_start, region.new_lines)
}

/// Builds a file's totals from its changed regions and classified ranges.
/// A changed line no range covers is meaningful: when no rule claims a
/// line, it must be read.
pub fn summarize(
    path: &str,
    regions: &[ChangedRegion],
    ranges: &[ClassifiedRange],
) -> FileClassSummary {
    let class_of = |side: Side, line: usize| {
        ranges
            .iter()
            .find(|range| {
                range.side == side && (range.first_line..=range.last_line).contains(&line)
            })
            .map(|range| range.class)
    };

    let mut summary = FileClassSummary {
        path: path.to_string(),
        meaningful_added: 0,
        meaningful_removed: 0,
        mechanical_lines: 0,
        comment_only_lines: 0,
        fully_mechanical: None,
    };
    for region in regions {
        for (side, start, count) in [
            (Side::Old, region.old_start, region.old_lines),
            (Side::New, region.new_start, region.new_lines),
        ] {
            for line in start..start + count {
                match class_of(side, line) {
                    Some(class) if class.is_mechanical() => summary.mechanical_lines += 1,
                    Some(Class::CommentOnly) => summary.comment_only_lines += 1,
                    // Moved-and-edited and friends keep their lines
                    // meaningful; the help they add lives in the range.
                    _ if side == Side::Old => summary.meaningful_removed += 1,
                    _ => summary.meaningful_added += 1,
                }
            }
        }
    }

    // One word only: a file mixing mechanical classes keeps its counts but
    // claims no single verdict.
    let mut classes = ranges.iter().map(|range| range.class);
    let uniform = classes.next().filter(|&first| classes.all(|c| c == first));
    if summary.meaningful_added == 0
        && summary.meaningful_removed == 0
        && summary.comment_only_lines == 0
    {
        summary.fully_mechanical = uniform.filter(|class| class.is_mechanical());
    }
    summary
}

/// The `@generated` marker counts only near the top, where generators put
/// it; anywhere else it is likely prose about the convention itself.
fn generated_marker(text: &str) -> bool {
    text.lines().take(5).any(|line| line.contains("@generated"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(
        path: &'a str,
        old_text: &'a str,
        new_text: &'a str,
        regions: &'a [ChangedRegion],
        rename: Option<RenameInfo<'a>>,
        generated: &'a GeneratedRules,
    ) -> FileRuleInput<'a> {
        FileRuleInput {
            path,
            old_text,
            new_text,
            regions,
            rename,
            generated,
            linguist_generated: false,
        }
    }

    #[test]
    fn cargo_lock_is_generated() {
        let rules = GeneratedRules::default();
        let regions = [ChangedRegion {
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
        }];
        let input = input(
            "sub/dir/Cargo.lock",
            "version = 3\n",
            "version = 4\n",
            &regions,
            None,
            &rules,
        );
        let (ranges, summary) = classify_file_rules(&input).expect("settled");
        assert!(ranges.iter().all(|r| r.class == Class::Generated));
        assert!(ranges.iter().all(|r| r.proof == "generated file"));
        assert_eq!(summary.fully_mechanical, Some(Class::Generated));
        assert_eq!(summary.meaningful_added, 0);
        assert_eq!(summary.meaningful_removed, 0);
        assert_eq!(summary.mechanical_lines, 2);
    }

    #[test]
    fn generated_marker_in_first_five_lines_is_generated() {
        let rules = GeneratedRules::default();
        let regions = [ChangedRegion {
            old_start: 3,
            old_lines: 1,
            new_start: 3,
            new_lines: 1,
        }];
        let new_text = "// @generated by protoc\nfn a() {}\nfn b() { work(); }\n";
        let input = input(
            "src/proto.rs",
            "// @generated by protoc\nfn a() {}\nfn b() {}\n",
            new_text,
            &regions,
            None,
            &rules,
        );
        let (_, summary) = classify_file_rules(&input).expect("settled");
        assert_eq!(summary.fully_mechanical, Some(Class::Generated));
    }

    #[test]
    fn marker_below_the_fifth_line_claims_nothing() {
        let rules = GeneratedRules::default();
        let regions = [ChangedRegion {
            old_start: 6,
            old_lines: 1,
            new_start: 6,
            new_lines: 1,
        }];
        let old = "a\nb\nc\nd\ne\n// @generated is a marker convention\n";
        let new = "a\nb\nc\nd\ne\n// @generated markers are a convention\n";
        let input = input("notes.md", old, new, &regions, None, &rules);
        assert_eq!(classify_file_rules(&input), None);
    }

    #[test]
    fn generated_globs_cover_minified_and_generated_dirs() {
        let rules = GeneratedRules::default();
        assert!(rules.matches("assets/app.min.js"));
        assert!(rules.matches("assets/app.min.css"));
        assert!(rules.matches("assets/app.min.map"));
        assert!(rules.matches("src/generated/api.rs"));
        assert!(rules.matches("generated/api.rs"));
        assert!(!rules.matches("src/degenerated/api.rs"));
        assert!(!rules.matches("src/app.js"));
    }

    #[test]
    fn linguist_generated_flag_settles_the_file() {
        let rules = GeneratedRules::default();
        let regions = [ChangedRegion {
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
        }];
        let mut input = input("docs/api.html", "old\n", "new\n", &regions, None, &rules);
        input.linguist_generated = true;
        let (_, summary) = classify_file_rules(&input).expect("settled");
        assert_eq!(summary.fully_mechanical, Some(Class::Generated));
    }

    #[test]
    fn rename_at_100_is_renamed_file() {
        let rules = GeneratedRules::default();
        let text = "fn a() {}\nfn b() {}\n";
        let input = input(
            "src/new.rs",
            text,
            text,
            &[],
            Some(RenameInfo {
                old_path: "src/old.rs",
                similarity: Some(100),
            }),
            &rules,
        );
        let (ranges, summary) = classify_file_rules(&input).expect("settled");
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].class, Class::RenamedFile);
        assert_eq!(ranges[0].proof, "renamed from `src/old.rs`, identical");
        assert_eq!(
            ranges[0].counterpart,
            Some(Counterpart {
                path: "src/old.rs".to_string(),
                first_line: 1,
                last_line: 2,
            })
        );
        assert_eq!(summary.fully_mechanical, Some(Class::RenamedFile));
    }

    #[test]
    fn rename_with_an_edit_claims_nothing() {
        let rules = GeneratedRules::default();
        let regions = [ChangedRegion {
            old_start: 2,
            old_lines: 1,
            new_start: 2,
            new_lines: 1,
        }];
        let input = input(
            "src/new.rs",
            "fn a() {}\nfn b() { old(); }\n",
            "fn a() {}\nfn b() { new(); }\n",
            &regions,
            Some(RenameInfo {
                old_path: "src/old.rs",
                similarity: Some(97),
            }),
            &rules,
        );
        assert_eq!(classify_file_rules(&input), None);
    }

    #[test]
    fn whitespace_only_regions_settle_formatting_for_the_file() {
        let rules = GeneratedRules::default();
        let old = "fn a() { body(); }\nmid\nfn b(x: u8,\n     y: u8) {}\n";
        let new = "fn a() {\n    body();\n}\nmid\nfn b(x: u8, y: u8) {}\n";
        let regions = [
            ChangedRegion {
                old_start: 1,
                old_lines: 1,
                new_start: 1,
                new_lines: 3,
            },
            ChangedRegion {
                old_start: 3,
                old_lines: 2,
                new_start: 5,
                new_lines: 1,
            },
        ];
        let input = input("src/lib.rs", old, new, &regions, None, &rules);
        let (ranges, summary) = classify_file_rules(&input).expect("settled");
        assert!(ranges.iter().all(|r| r.class == Class::Formatting));
        assert!(ranges.iter().all(|r| r.proof == "whitespace only"));
        assert_eq!(summary.fully_mechanical, Some(Class::Formatting));
        assert_eq!(summary.meaningful_added, 0);
        assert_eq!(summary.meaningful_removed, 0);
    }

    #[test]
    fn rewrapped_readme_with_equal_words_is_formatting() {
        let rules = GeneratedRules::default();
        let old = "The reviewer reads the diff and marks each file when done.\n";
        let new = "The reviewer reads the diff\nand marks each file when done.\n";
        let regions = [ChangedRegion {
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 2,
        }];
        let input = input("README.md", old, new, &regions, None, &rules);
        let (_, summary) = classify_file_rules(&input).expect("settled");
        assert_eq!(summary.fully_mechanical, Some(Class::Formatting));
    }

    #[test]
    fn rewrapped_readme_with_one_word_changed_claims_nothing() {
        let rules = GeneratedRules::default();
        let old = "The reviewer reads the diff and marks each file when done.\n";
        let new = "The reviewer scans the diff\nand marks each file when done.\n";
        let regions = [ChangedRegion {
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 2,
        }];
        let input = input("README.md", old, new, &regions, None, &rules);
        assert_eq!(classify_file_rules(&input), None);
    }

    #[test]
    fn joined_words_are_not_formatting() {
        let region = ChangedRegion {
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
        };
        assert!(!whitespace_only("data base\n", "database\n", &region));
    }

    #[test]
    fn mixed_regions_leave_the_file_unsettled() {
        let rules = GeneratedRules::default();
        let old = "fn a() { body(); }\nfn b() { old(); }\n";
        let new = "fn a() {\n    body();\n}\nfn b() { new(); }\n";
        let regions = [
            ChangedRegion {
                old_start: 1,
                old_lines: 1,
                new_start: 1,
                new_lines: 3,
            },
            ChangedRegion {
                old_start: 2,
                old_lines: 1,
                new_start: 4,
                new_lines: 1,
            },
        ];
        let input = input("src/lib.rs", old, new, &regions, None, &rules);
        assert_eq!(classify_file_rules(&input), None);
    }

    #[test]
    fn summarize_counts_uncovered_lines_as_meaningful() {
        let regions = [ChangedRegion {
            old_start: 1,
            old_lines: 2,
            new_start: 1,
            new_lines: 3,
        }];
        let ranges = [ClassifiedRange {
            path: "src/lib.rs".to_string(),
            side: Side::New,
            first_line: 1,
            last_line: 1,
            class: Class::Formatting,
            counterpart: None,
            proof: "whitespace only".to_string(),
        }];
        let summary = summarize("src/lib.rs", &regions, &ranges);
        assert_eq!(summary.meaningful_removed, 2);
        assert_eq!(summary.meaningful_added, 2);
        assert_eq!(summary.mechanical_lines, 1);
        assert_eq!(summary.fully_mechanical, None);
    }
}
