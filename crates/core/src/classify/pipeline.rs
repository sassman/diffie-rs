//! Steps 2 and 3 of the classification pipeline: formatting and
//! comment-only at item level, then pure moves across all changed files.
//!
//! [`classify`] is the one entry point. It runs the file rules first, parses
//! what they leave open, settles items in place, and pairs move candidates
//! in a single cross-file pass. Everything is a pure function of texts and
//! changed regions; blob ids only key the parse cache.

use std::collections::HashMap;

use super::cache::ParseCache;
use super::items::{Container, FileItems, Item, non_trivial_lines, parse_items};
use super::{
    Class, ClassifiedRange, Counterpart, FileClassSummary, FileRuleInput, GeneratedRules,
    RenameInfo, Side, classify_file_rules, summarize,
};
use crate::model::ChangedRegion;

/// One changed file, everything pre-resolved: texts, regions, the rename
/// header, and the blob ids that key the parse cache. `None` ids skip the
/// cache, never the parse.
#[derive(Debug)]
pub struct ClassifyInput<'a> {
    /// New-side path.
    pub path: &'a str,
    pub old_text: &'a str,
    pub new_text: &'a str,
    pub regions: &'a [ChangedRegion],
    pub rename: Option<RenameInfo<'a>>,
    pub linguist_generated: bool,
    pub old_blob: Option<&'a str>,
    pub new_blob: Option<&'a str>,
}

/// What the pipeline settled for one file, in input order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileClassification {
    pub ranges: Vec<ClassifiedRange>,
    pub summary: FileClassSummary,
}

/// Classifies every changed file: file rules, then item-level formatting
/// and comment-only, then one cross-file pass pairing pure moves. Takes all
/// files at once because a move's two ends rarely share a file.
pub fn classify(
    files: &[ClassifyInput<'_>],
    generated: &GeneratedRules,
    cache: Option<&ParseCache>,
) -> Vec<FileClassification> {
    let mut ranges: Vec<Vec<ClassifiedRange>> = vec![Vec::new(); files.len()];
    let mut settled: Vec<Option<FileClassSummary>> = (0..files.len()).map(|_| None).collect();
    let mut parsed: Vec<Option<(FileItems, FileItems)>> = Vec::with_capacity(files.len());

    for (index, file) in files.iter().enumerate() {
        let rule_input = FileRuleInput {
            path: file.path,
            old_text: file.old_text,
            new_text: file.new_text,
            regions: file.regions,
            rename: file.rename.clone(),
            generated,
            linguist_generated: file.linguist_generated,
        };
        if let Some((file_ranges, summary)) = classify_file_rules(&rule_input) {
            ranges[index] = file_ranges;
            settled[index] = Some(summary);
            parsed.push(None);
            continue;
        }
        if !file.path.ends_with(".rs") {
            parsed.push(None);
            continue;
        }
        let old = parse(cache, file.old_blob, file.old_text);
        let new = parse(cache, file.new_blob, file.new_text);
        // A parse error on either side: no item-level classification. The
        // dangerous error is the fold that should not have happened.
        parsed.push(old.zip(new));
    }

    // Step 2, in place: formatting and comment-only. `claimed` keeps those
    // items out of the move pass.
    let mut claimed: Vec<(Vec<usize>, Vec<usize>)> = vec![(Vec::new(), Vec::new()); files.len()];
    for (index, file) in files.iter().enumerate() {
        if settled[index].is_some() {
            continue;
        }
        let Some((old, new)) = &parsed[index] else {
            continue;
        };
        if old.token_hash == new.token_hash && file.old_text != file.new_text {
            ranges[index] = whole_file_formatting(file);
            settled[index] = Some(summarize(file.path, file.regions, &ranges[index]));
            continue;
        }
        let (file_ranges, old_claimed, new_claimed) = settle_in_place(file, old, new);
        ranges[index] = file_ranges;
        claimed[index] = (old_claimed, new_claimed);
    }

    // Step 3, the one cross-file pass: pure moves.
    for (file_index, range) in pair_moves(files, &parsed, &settled, &claimed) {
        ranges[file_index].push(range);
    }

    files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            let summary = settled[index]
                .take()
                .unwrap_or_else(|| summarize(file.path, file.regions, &ranges[index]));
            FileClassification {
                ranges: std::mem::take(&mut ranges[index]),
                summary,
            }
        })
        .collect()
}

fn parse(cache: Option<&ParseCache>, blob: Option<&str>, text: &str) -> Option<FileItems> {
    match (cache, blob) {
        (Some(cache), Some(blob)) => cache.items(blob, text),
        _ => parse_items(text),
    }
}

/// The whole file's tokens are equal, so every changed line is Formatting -
/// the token-level twin of the word-level rule in step 0.
fn whole_file_formatting(file: &ClassifyInput<'_>) -> Vec<ClassifiedRange> {
    [(Side::Old, file.old_text), (Side::New, file.new_text)]
        .into_iter()
        .filter(|(_, text)| !text.is_empty())
        .map(|(side, text)| ClassifiedRange {
            path: file.path.to_string(),
            side,
            first_line: 1,
            last_line: text.lines().count(),
            class: Class::Formatting,
            counterpart: None,
            proof: "whitespace only".to_string(),
        })
        .collect()
}

/// Step 2: same path, same module, container and name. Equal tokens with
/// different text prove Formatting; equal comment-stripped tokens prove
/// CommentOnly. Returns the claimed item indices per side.
fn settle_in_place(
    file: &ClassifyInput<'_>,
    old: &FileItems,
    new: &FileItems,
) -> (Vec<ClassifiedRange>, Vec<usize>, Vec<usize>) {
    type Key<'a> = (&'a [String], &'a Container, &'a str);
    fn key(item: &Item) -> Key<'_> {
        (
            item.module_path.as_slice(),
            &item.container,
            item.name.as_str(),
        )
    }
    let mut by_key: HashMap<Key<'_>, (Vec<usize>, Vec<usize>)> = HashMap::new();
    for (index, item) in old.items.iter().enumerate() {
        by_key.entry(key(item)).or_default().0.push(index);
    }
    for (index, item) in new.items.iter().enumerate() {
        by_key.entry(key(item)).or_default().1.push(index);
    }

    let mut ranges = Vec::new();
    let mut old_claimed = Vec::new();
    let mut new_claimed = Vec::new();
    // Outer items come first in document order, so a settled module or impl
    // swallows its members instead of tinting them twice.
    let mut pairs: Vec<(usize, usize)> = by_key
        .into_values()
        .flat_map(|(olds, news)| olds.into_iter().zip(news))
        .collect();
    pairs.sort_unstable();
    // "Same place" is relative, not absolute: an item may drift when lines
    // are added above it, but one that traded positions with a sibling has
    // moved and belongs to step 3. A pair is in place when its spans still
    // overlap or when no other pair inverts its order.
    let in_place = |pair: &(usize, usize), pairs: &[(usize, usize)]| {
        let at = |&(old_index, new_index): &(usize, usize)| {
            (
                old.items[old_index].first_line,
                new.items[new_index].first_line,
            )
        };
        let (old_at, new_at) = at(pair);
        let (old_last, new_last) = (old.items[pair.0].last_line, new.items[pair.1].last_line);
        old_at <= new_last && new_at <= old_last
            || pairs.iter().all(|other| {
                let (other_old, other_new) = at(other);
                (old_at < other_old) == (new_at < other_new)
            })
    };
    for (old_index, new_index) in &pairs {
        let (old_index, new_index) = (*old_index, *new_index);
        let old_item = &old.items[old_index];
        let new_item = &new.items[new_index];
        if old_item.kind != new_item.kind
            || old_item.text_hash == new_item.text_hash
            || !in_place(&(old_index, new_index), &pairs)
        {
            continue;
        }
        let covered = |claimed: &[usize], items: &[Item], index: usize| {
            claimed.iter().any(|&outer| {
                items[outer].first_line <= items[index].first_line
                    && items[outer].last_line >= items[index].last_line
            })
        };
        if covered(&old_claimed, &old.items, old_index) {
            continue;
        }
        let (class, proof) = if old_item.token_hash == new_item.token_hash {
            (Class::Formatting, "whitespace only")
        } else if old_item.stripped_hash == new_item.stripped_hash {
            (Class::CommentOnly, "comment text only")
        } else {
            continue;
        };
        old_claimed.push(old_index);
        new_claimed.push(new_index);
        for (side, item) in [(Side::Old, old_item), (Side::New, new_item)] {
            ranges.push(ClassifiedRange {
                path: file.path.to_string(),
                side,
                first_line: item.first_line,
                last_line: item.last_line,
                class,
                counterpart: None,
                proof: proof.to_string(),
            });
        }
    }
    (ranges, old_claimed, new_claimed)
}

/// One end of a move: a file and an item index on one side of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct End {
    file: usize,
    item: usize,
}

/// A candidate with its item record, so pairing never re-resolves indices.
type Cand<'a> = (End, &'a Item);

/// Step 3: candidates are items whose non-trivial lines are at least 90%
/// removed or added; they pair one to one by token hash, ties broken by
/// name and container, then nearest path, else not at all. Section 4 then
/// decides whether identical tokens still mean the same thing. Returns the
/// ranges with the index of the file each belongs to.
fn pair_moves(
    files: &[ClassifyInput<'_>],
    parsed: &[Option<(FileItems, FileItems)>],
    settled: &[Option<FileClassSummary>],
    claimed: &[(Vec<usize>, Vec<usize>)],
) -> Vec<(usize, ClassifiedRange)> {
    let mut by_hash: HashMap<u64, (Vec<Cand<'_>>, Vec<Cand<'_>>)> = HashMap::new();
    for (file_index, file) in files.iter().enumerate() {
        if settled[file_index].is_some() {
            continue;
        }
        let Some((old, new)) = &parsed[file_index] else {
            continue;
        };
        let sides = [
            (Side::Old, &old.items, file.old_text, &claimed[file_index].0),
            (Side::New, &new.items, file.new_text, &claimed[file_index].1),
        ];
        for (side, items, text, side_claimed) in sides {
            let changed = changed_lines(file.regions, side);
            for (item_index, it) in items.iter().enumerate() {
                if side_claimed.contains(&item_index) || !is_candidate(text, it, &changed) {
                    continue;
                }
                let end = End {
                    file: file_index,
                    item: item_index,
                };
                let entry = by_hash.entry(it.token_hash).or_default();
                match side {
                    Side::Old => entry.0.push((end, it)),
                    Side::New => entry.1.push((end, it)),
                }
            }
        }
    }

    let mut pairs: Vec<(Cand<'_>, Cand<'_>)> = Vec::new();
    let mut groups: Vec<_> = by_hash.into_values().collect();
    // HashMap order is arbitrary; sorted groups keep the output stable.
    groups.sort_unstable_by_key(|(olds, news)| {
        (
            olds.first().map(|(end, _)| *end),
            news.first().map(|(end, _)| *end),
        )
    });
    for (olds, news) in &groups {
        pairs.extend(pair_group(olds, news, files));
    }

    // An item inside a moved module or impl pairs on its own too; only the
    // outermost pair speaks, or the same lines would be classified twice.
    let span = |(end, it): Cand<'_>| (end.file, it.first_line, it.last_line);
    let inside = |(file, first, last): (usize, usize, usize),
                  (outer_file, outer_first, outer_last): (usize, usize, usize)| {
        file == outer_file && outer_first <= first && last <= outer_last
    };
    let kept: Vec<&(Cand<'_>, Cand<'_>)> = pairs
        .iter()
        .filter(|(old, new)| {
            !pairs.iter().any(|(outer_old, outer_new)| {
                (outer_old.0, outer_new.0) != (old.0, new.0)
                    && inside(span(*old), span(*outer_old))
                    && inside(span(*new), span(*outer_new))
            })
        })
        .collect();

    let mut ranges = Vec::new();
    for &&((old_end, old_item), (new_end, new_item)) in &kept {
        let old_path = files[old_end.file].path;
        let new_path = files[new_end.file].path;
        let (class, origin_proof, destination_proof) =
            judge(old_item, new_item, old_path, new_path);
        let counterpart = |it: &Item, path: &str| {
            Some(Counterpart {
                path: path.to_string(),
                first_line: it.first_line,
                last_line: it.last_line,
            })
        };
        ranges.push((
            old_end.file,
            ClassifiedRange {
                path: old_path.to_string(),
                side: Side::Old,
                first_line: old_item.first_line,
                last_line: old_item.last_line,
                class,
                counterpart: counterpart(new_item, new_path),
                proof: origin_proof,
            },
        ));
        ranges.push((
            new_end.file,
            ClassifiedRange {
                path: new_path.to_string(),
                side: Side::New,
                first_line: new_item.first_line,
                last_line: new_item.last_line,
                class,
                counterpart: counterpart(old_item, old_path),
                proof: destination_proof,
            },
        ));
    }
    ranges
}

/// Pairs one hash group. A lone candidate on each side pairs outright;
/// several sharing a hash must also agree on name and container, and among
/// those the nearest path wins - an ambiguity pairs nothing, because a
/// wrong fold is worse than none.
fn pair_group<'a>(
    olds: &[Cand<'a>],
    news: &[Cand<'a>],
    files: &[ClassifyInput<'_>],
) -> Vec<(Cand<'a>, Cand<'a>)> {
    if news.is_empty() || olds.is_empty() {
        return Vec::new();
    }
    if let ([old], [new]) = (olds, news) {
        return vec![(*old, *new)];
    }
    type Sides<'a> = (Vec<Cand<'a>>, Vec<Cand<'a>>);
    let mut by_key: HashMap<(&str, &Container), Sides<'a>> = HashMap::new();
    for &(end, it) in olds {
        by_key
            .entry((it.name.as_str(), &it.container))
            .or_default()
            .0
            .push((end, it));
    }
    for &(end, it) in news {
        by_key
            .entry((it.name.as_str(), &it.container))
            .or_default()
            .1
            .push((end, it));
    }
    let mut pairs = Vec::new();
    let mut groups: Vec<_> = by_key.into_values().collect();
    groups.sort_unstable_by_key(|(olds, news)| {
        (
            olds.first().map(|(end, _)| *end),
            news.first().map(|(end, _)| *end),
        )
    });
    for (key_olds, key_news) in groups {
        match (key_olds.as_slice(), key_news.as_slice()) {
            ([old], [new]) => pairs.push((*old, *new)),
            ([old], several) => {
                if let Some(new) = nearest(files, files[old.0.file].path, several) {
                    pairs.push((*old, new));
                }
            }
            (several, [new]) => {
                if let Some(old) = nearest(files, files[new.0.file].path, several) {
                    pairs.push((old, *new));
                }
            }
            // Several equal candidates on both sides: no pairing is
            // defensible, so none is made.
            _ => {}
        }
    }
    pairs
}

/// The candidate whose file path is nearest to `to`: the longest shared
/// leading path components, unique or nothing.
fn nearest<'a>(files: &[ClassifyInput<'_>], to: &str, candidates: &[Cand<'a>]) -> Option<Cand<'a>> {
    let closeness = |end: &End| {
        let path = files[end.file].path;
        if path == to {
            return usize::MAX;
        }
        path.split('/')
            .zip(to.split('/'))
            .take_while(|(a, b)| a == b)
            .count()
    };
    let best = candidates.iter().map(|(end, _)| closeness(end)).max()?;
    let mut nearest = candidates.iter().filter(|(end, _)| closeness(end) == best);
    let winner = nearest.next().copied();
    match nearest.next() {
        Some(_) => None,
        None => winner,
    }
}

/// Section 4: identical tokens are not enough when the surroundings that
/// give them meaning changed. Returns the class and the proof for each end.
fn judge(
    old_item: &Item,
    new_item: &Item,
    old_path: &str,
    new_path: &str,
) -> (Class, String, String) {
    let module_changed = old_path != new_path || old_item.module_path != new_item.module_path;
    let container_changed = old_item.container != new_item.container;
    let scope_changed = module_changed || container_changed;
    let origin_site = format!("`{new_path}:{}`", new_item.first_line);
    let destination_site = format!("`{old_path}:{}`", old_item.first_line);

    let context = |reason: String| {
        (
            Class::MovedContextChanged,
            format!(
                "moved from {} to {}: {reason}",
                place(old_item, old_path),
                place(new_item, new_path),
            ),
        )
    };
    let reason = if old_item.has_self_ty && scope_changed {
        Some(context("`Self` now means a different type".to_string()))
    } else if container_changed {
        Some(context("the container changed".to_string()))
    } else if old_item.cfgs != new_item.cfgs {
        Some(context("the enclosing `#[cfg]` differs".to_string()))
    } else if old_item.has_super_path && module_changed {
        Some(context("`super::` now resolves differently".to_string()))
    } else if old_item.has_restricted_vis && module_changed {
        Some(context(
            "a restricted visibility now names a different module".to_string(),
        ))
    } else {
        None
    };
    if let Some((class, proof)) = reason {
        // The reason names both ends, so it reads the same from either.
        return (class, proof.clone(), proof);
    }

    let identical_bytes = old_item.text_hash == new_item.text_hash;
    let mut suffix = String::new();
    if !identical_bytes {
        // Equal tokens with different text can only differ in whitespace -
        // worth seeing, just not first, so the label names it (settled).
        suffix.push_str(" · re-indented");
    }
    if module_changed {
        suffix.push_str(" · name resolution not checked");
    }
    let equality = if identical_bytes && !module_changed {
        "identical"
    } else {
        "identical tokens"
    };
    (
        Class::Moved,
        format!("moved to {origin_site}, {equality}{suffix}"),
        format!("moved from {destination_site}, {equality}{suffix}"),
    )
}

/// Where an item sits, for the context-changed proofs: its container when
/// it has one, its module otherwise.
fn place(item: &Item, path: &str) -> String {
    match &item.container {
        Container::Impl {
            self_ty,
            trait_: None,
        } => format!("`impl {self_ty}`"),
        Container::Impl {
            self_ty,
            trait_: Some(trait_),
        } => format!("`impl {trait_} for {self_ty}`"),
        Container::Trait { name } => format!("`trait {name}`"),
        Container::Module if item.module_path.is_empty() => {
            format!("the top level of `{path}`")
        }
        Container::Module => format!("module `{}`", item.module_path.join("::")),
    }
}

fn is_candidate(text: &str, item: &Item, changed: &[(usize, usize)]) -> bool {
    let lines = non_trivial_lines(text, item.first_line, item.last_line);
    let in_changed = lines
        .iter()
        .filter(|&&line| {
            changed
                .iter()
                .any(|&(first, last)| (first..=last).contains(&line))
        })
        .count();
    // At least 90% of the non-trivial lines are gone or new; an item
    // without any proves nothing and never pairs.
    !lines.is_empty() && 10 * in_changed >= 9 * lines.len()
}

/// The changed lines of one side as inclusive ranges.
fn changed_lines(regions: &[ChangedRegion], side: Side) -> Vec<(usize, usize)> {
    regions
        .iter()
        .filter_map(|region| {
            let (start, count) = match side {
                Side::Old => (region.old_start, region.old_lines),
                Side::New => (region.new_start, region.new_lines),
            };
            (count > 0).then(|| (start, start + count - 1))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal line LCS, test-only: the fixtures state old and new text
    /// and get realistic changed regions without hand-counting lines.
    fn diff_regions(old: &str, new: &str) -> Vec<ChangedRegion> {
        let a: Vec<&str> = old.lines().collect();
        let b: Vec<&str> = new.lines().collect();
        let mut lcs = vec![vec![0usize; b.len() + 1]; a.len() + 1];
        for i in (0..a.len()).rev() {
            for j in (0..b.len()).rev() {
                lcs[i][j] = if a[i] == b[j] {
                    lcs[i + 1][j + 1] + 1
                } else {
                    lcs[i + 1][j].max(lcs[i][j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        let mut regions = Vec::new();
        let mut open: Option<ChangedRegion> = None;
        while i < a.len() || j < b.len() {
            if i < a.len() && j < b.len() && a[i] == b[j] {
                regions.extend(open.take());
                i += 1;
                j += 1;
            } else {
                let region = open.get_or_insert(ChangedRegion {
                    old_start: i + 1,
                    old_lines: 0,
                    new_start: j + 1,
                    new_lines: 0,
                });
                let delete = j == b.len() || (i < a.len() && lcs[i + 1][j] >= lcs[i][j + 1]);
                if delete {
                    region.old_lines += 1;
                    i += 1;
                } else {
                    region.new_lines += 1;
                    j += 1;
                }
            }
        }
        regions.extend(open);
        regions
    }

    fn rust_file<'a>(
        path: &'a str,
        old: &'a str,
        new: &'a str,
        regions: &'a [ChangedRegion],
    ) -> ClassifyInput<'a> {
        ClassifyInput {
            path,
            old_text: old,
            new_text: new,
            regions,
            rename: None,
            linguist_generated: false,
            old_blob: None,
            new_blob: None,
        }
    }

    fn run(files: &[ClassifyInput<'_>]) -> Vec<FileClassification> {
        classify(files, &GeneratedRules::default(), None)
    }

    fn moved_ranges(result: &FileClassification, side: Side) -> Vec<&ClassifiedRange> {
        result
            .ranges
            .iter()
            .filter(|range| range.side == side)
            .collect()
    }

    #[test]
    fn cross_file_move_carries_the_resolution_caveat() {
        let a_old = "fn keep() {\n    stay();\n}\n\nfn travel(x: u8) -> u8 {\n    x + 1\n}\n";
        let a_new = "fn keep() {\n    stay();\n}\n";
        let b_old = "fn other() {\n    here();\n}\n";
        let b_new = "fn other() {\n    here();\n}\n\nfn travel(x: u8) -> u8 {\n    x + 1\n}\n";
        let a_regions = diff_regions(a_old, a_new);
        let b_regions = diff_regions(b_old, b_new);
        let files = [
            rust_file("src/a.rs", a_old, a_new, &a_regions),
            rust_file("src/b.rs", b_old, b_new, &b_regions),
        ];
        let results = run(&files);

        let origin = &moved_ranges(&results[0], Side::Old)[0];
        assert_eq!(origin.class, Class::Moved);
        assert_eq!(
            origin.proof,
            "moved to `src/b.rs:5`, identical tokens · name resolution not checked"
        );
        assert_eq!(origin.counterpart.as_ref().unwrap().path, "src/b.rs");
        let destination = &moved_ranges(&results[1], Side::New)[0];
        assert_eq!(
            destination.proof,
            "moved from `src/a.rs:5`, identical tokens · name resolution not checked"
        );
        assert_eq!((destination.first_line, destination.last_line), (5, 7));
    }

    #[test]
    fn swap_inside_one_file_moves_both_ends_without_caveat() {
        let old = "fn alpha() {\n    first();\n}\nfn beta() {\n    second();\n}\n";
        let new = "fn beta() {\n    second();\n}\nfn alpha() {\n    first();\n}\n";
        let regions = diff_regions(old, new);
        let files = [rust_file("src/a.rs", old, new, &regions)];
        let results = run(&files);

        let olds = moved_ranges(&results[0], Side::Old);
        let news = moved_ranges(&results[0], Side::New);
        assert_eq!((olds.len(), news.len()), (1, 1));
        for range in results[0].ranges.iter() {
            assert_eq!(range.class, Class::Moved);
            assert!(!range.proof.contains("name resolution"));
        }
        let summary = &results[0].summary;
        assert_eq!(summary.meaningful_added + summary.meaningful_removed, 0);
        assert_eq!(summary.fully_mechanical, Some(Class::Moved));
    }

    #[test]
    fn re_indented_move_names_the_re_indentation() {
        let old = "fn stay() {\n    here();\n}\nfn hop(x: u8) -> u8 {\n    x + 2\n}\n";
        let new = "fn hop(x: u8) -> u8 {\n        x + 2\n}\nfn stay() {\n    here();\n}\n";
        let regions = diff_regions(old, new);
        let files = [rust_file("src/a.rs", old, new, &regions)];
        let results = run(&files);

        let origin = &moved_ranges(&results[0], Side::Old)[0];
        assert_eq!(
            origin.proof,
            "moved to `src/a.rs:1`, identical tokens · re-indented"
        );
        assert_eq!(origin.class, Class::Moved);
    }

    #[test]
    fn byte_identical_move_reads_identical_without_the_tokens_wording() {
        let old = "fn stay() {\n    here();\n}\nfn hop(x: u8) -> u8 {\n    x + 2\n}\n";
        let new = "fn hop(x: u8) -> u8 {\n    x + 2\n}\nfn stay() {\n    here();\n}\n";
        let regions = diff_regions(old, new);
        let files = [rust_file("src/a.rs", old, new, &regions)];
        let results = run(&files);

        // Which of the two blocks the diff calls moved is the diff's
        // choice; the label matters: "identical", no tokens wording.
        let origin = &moved_ranges(&results[0], Side::Old)[0];
        assert_eq!(origin.proof, "moved to `src/a.rs:4`, identical");
        let destination = &moved_ranges(&results[0], Side::New)[0];
        assert_eq!(destination.proof, "moved from `src/a.rs:1`, identical");
    }

    #[test]
    fn free_fn_into_impl_changes_context() {
        let old = "struct Cache;\nimpl Cache {\n    fn get(&self) -> u8 {\n        1\n    }\n}\nfn evict(n: u8) -> u8 {\n    n + 1\n}\n";
        let new = "struct Cache;\nimpl Cache {\n    fn get(&self) -> u8 {\n        1\n    }\n    fn evict(n: u8) -> u8 {\n        n + 1\n    }\n}\n";
        let regions = diff_regions(old, new);
        let files = [rust_file("src/a.rs", old, new, &regions)];
        let results = run(&files);

        let origin = &moved_ranges(&results[0], Side::Old)[0];
        assert_eq!(origin.class, Class::MovedContextChanged);
        assert_eq!(
            origin.proof,
            "moved from the top level of `src/a.rs` to `impl Cache`: the container changed"
        );
        assert!(!origin.class.is_mechanical());
    }

    #[test]
    fn self_between_impls_names_self_as_the_reason() {
        let old = "struct Cache;\nstruct Index;\nimpl Cache {\n    fn fresh() -> Self {\n        Self::default()\n    }\n}\nimpl Index {\n    fn size(&self) -> u8 {\n        2\n    }\n}\n";
        let new = "struct Cache;\nstruct Index;\nimpl Cache {\n}\nimpl Index {\n    fn size(&self) -> u8 {\n        2\n    }\n    fn fresh() -> Self {\n        Self::default()\n    }\n}\n";
        let regions = [
            ChangedRegion {
                old_start: 4,
                old_lines: 3,
                new_start: 4,
                new_lines: 0,
            },
            ChangedRegion {
                old_start: 11,
                old_lines: 0,
                new_start: 9,
                new_lines: 3,
            },
        ];
        let files = [rust_file("src/a.rs", old, new, &regions)];
        let results = run(&files);

        let origin = &moved_ranges(&results[0], Side::Old)[0];
        assert_eq!(origin.class, Class::MovedContextChanged);
        assert_eq!(
            origin.proof,
            "moved from `impl Cache` to `impl Index`: `Self` now means a different type"
        );
        let destination = &moved_ranges(&results[0], Side::New)[0];
        assert_eq!(destination.proof, origin.proof);
    }

    #[test]
    fn attributes_and_doc_comment_travel_in_one_range() {
        let old = "/// Checks the edge.\n#[test]\nfn edge() {\n    assert!(works());\n}\nfn anchor() {\n    stay();\n    stay();\n    stay();\n    stay();\n}\n";
        let new = "fn anchor() {\n    stay();\n    stay();\n    stay();\n    stay();\n}\n/// Checks the edge.\n#[test]\nfn edge() {\n    assert!(works());\n}\n";
        let regions = diff_regions(old, new);
        let files = [rust_file("src/a.rs", old, new, &regions)];
        let results = run(&files);

        let origin = &moved_ranges(&results[0], Side::Old)[0];
        // One range from the doc comment through the closing brace.
        assert_eq!((origin.first_line, origin.last_line), (1, 5));
        let destination = &moved_ranges(&results[0], Side::New)[0];
        assert_eq!((destination.first_line, destination.last_line), (7, 11));
        assert_eq!(moved_ranges(&results[0], Side::Old).len(), 1);
    }

    #[test]
    fn identical_bodies_pair_only_where_the_impl_matches() {
        let body = "    fn default() -> u8 {\n        7\n    }\n";
        let old =
            format!("impl Alpha {{\n{body}}}\nimpl Beta {{\n{body}}}\nimpl Gamma {{\n{body}}}\n");
        let new =
            format!("impl Beta {{\n{body}}}\nimpl Gamma {{\n{body}}}\nimpl Delta {{\n{body}}}\n");
        // A full rewrite the way git reports one: a single region spanning
        // both files end to end.
        let regions = [ChangedRegion {
            old_start: 1,
            old_lines: 15,
            new_start: 1,
            new_lines: 15,
        }];
        let files = [rust_file("src/a.rs", &old, &new, &regions)];
        let results = run(&files);

        let covering = |side: Side, line: usize| {
            results[0]
                .ranges
                .iter()
                .any(|r| r.side == side && (r.first_line..=r.last_line).contains(&line))
        };
        // Beta and Gamma pair; Alpha's removal and Delta's addition stay
        // meaningful - a hash match alone is no pairing.
        assert!(covering(Side::Old, 6) && covering(Side::Old, 11));
        assert!(!covering(Side::Old, 2));
        assert!(covering(Side::New, 1) && covering(Side::New, 6));
        assert!(!covering(Side::New, 12));
        assert_eq!(results[0].summary.meaningful_removed, 5);
        assert_eq!(results[0].summary.meaningful_added, 5);
        for range in &results[0].ranges {
            assert_eq!(range.class, Class::Moved);
        }
    }

    #[test]
    fn whole_file_reformat_is_formatting() {
        let old = "fn a(x:u8)->u8{x+1}\nfn b(y:u8)->u8{y+2}\n";
        let new = "fn a(x: u8) -> u8 {\n    x + 1\n}\nfn b(y: u8) -> u8 {\n    y + 2\n}\n";
        let regions = diff_regions(old, new);
        let files = [rust_file("src/a.rs", old, new, &regions)];
        let results = run(&files);

        assert_eq!(results[0].summary.fully_mechanical, Some(Class::Formatting));
        assert!(
            results[0]
                .ranges
                .iter()
                .all(|r| r.proof == "whitespace only")
        );
    }

    #[test]
    fn reformat_with_one_token_changed_keeps_that_item_meaningful() {
        let old = "fn a(x:u8)->u8{x+1}\nfn b(y:u8)->u8{y+2}\n";
        let new = "fn a(x: u8) -> u8 {\n    x + 1\n}\nfn b(y: u8) -> u8 {\n    y + 3\n}\n";
        let regions = diff_regions(old, new);
        let files = [rust_file("src/a.rs", old, new, &regions)];
        let results = run(&files);

        let formatting: Vec<_> = results[0]
            .ranges
            .iter()
            .filter(|r| r.class == Class::Formatting)
            .collect();
        assert_eq!(formatting.len(), 2);
        assert!(formatting.iter().all(|r| match r.side {
            Side::Old => r.first_line == 1 && r.last_line == 1,
            Side::New => r.first_line == 1 && r.last_line == 3,
        }));
        assert!(results[0].summary.meaningful_added > 0);
        assert_eq!(results[0].summary.fully_mechanical, None);
    }

    #[test]
    fn syntax_error_on_one_side_yields_no_item_classes() {
        let old = "fn a() {\n    one();\n}\nfn b() {\n    two();\n}\n";
        let new = "fn b() {\n    two();\n}\nfn a( {\n    one();\n}\n";
        let regions = diff_regions(old, new);
        let files = [rust_file("src/a.rs", old, new, &regions)];
        let results = run(&files);

        assert!(results[0].ranges.is_empty());
        let summary = &results[0].summary;
        assert!(summary.meaningful_added + summary.meaningful_removed > 0);
        assert_eq!(summary.mechanical_lines, 0);
    }

    #[test]
    fn item_inside_macro_invocation_stays_meaningful() {
        let old = "wrap! {\n    fn tucked() {\n        inner();\n    }\n}\nfn stay() {\n    keep();\n    keep();\n    keep();\n    keep();\n}\n";
        let new = "fn stay() {\n    keep();\n    keep();\n    keep();\n    keep();\n}\nwrap! {\n    fn tucked() {\n        inner();\n    }\n}\n";
        let regions = diff_regions(old, new);
        let files = [rust_file("src/a.rs", old, new, &regions)];
        let results = run(&files);

        assert!(results[0].ranges.is_empty());
        assert!(results[0].summary.meaningful_removed > 0);
    }

    #[test]
    fn comment_edited_in_place_is_comment_only() {
        let old = "fn a() {\n    // first try\n    run();\n}\n";
        let new = "fn a() {\n    // second try\n    run();\n}\n";
        let regions = diff_regions(old, new);
        let files = [rust_file("src/a.rs", old, new, &regions)];
        let results = run(&files);

        assert_eq!(results[0].ranges.len(), 2);
        for range in &results[0].ranges {
            assert_eq!(range.class, Class::CommentOnly);
            assert_eq!(range.proof, "comment text only");
            assert!(!range.class.is_mechanical());
        }
        let summary = &results[0].summary;
        assert_eq!(summary.comment_only_lines, 2);
        assert_eq!(summary.meaningful_added + summary.meaningful_removed, 0);
        assert_eq!(summary.fully_mechanical, None);
    }

    #[test]
    fn doc_comment_edited_in_place_is_comment_only() {
        let old = "/// Old words.\nfn a() {\n    run();\n}\n";
        let new = "/// New words.\nfn a() {\n    run();\n}\n";
        let regions = diff_regions(old, new);
        let files = [rust_file("src/a.rs", old, new, &regions)];
        let results = run(&files);

        assert!(!results[0].ranges.is_empty());
        assert!(
            results[0]
                .ranges
                .iter()
                .all(|r| r.class == Class::CommentOnly)
        );
        assert_eq!(results[0].summary.comment_only_lines, 2);
    }

    #[test]
    fn file_rules_still_settle_before_any_parsing() {
        // A lockfile with a parse-hostile content never reaches the parser.
        let regions = [ChangedRegion {
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
        }];
        let files = [ClassifyInput {
            path: "Cargo.lock",
            old_text: "version = 3\n",
            new_text: "version = 4\n",
            regions: &regions,
            rename: None,
            linguist_generated: false,
            old_blob: None,
            new_blob: None,
        }];
        let results = run(&files);
        assert_eq!(results[0].summary.fully_mechanical, Some(Class::Generated));
    }
}
