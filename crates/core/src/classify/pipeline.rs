//! Steps 2 through 5 of the classification pipeline: formatting and
//! comment-only at item level, pure moves, moved-and-edited pairs, and
//! copies, across all changed files.
//!
//! [`classify`] is the one entry point. It runs the file rules first, parses
//! what they leave open, settles items in place, and pairs move candidates
//! in a single cross-file pass. Everything is a pure function of texts and
//! changed regions; blob ids only key the parse cache.

use std::collections::{HashMap, HashSet};

use super::cache::ParseCache;
use super::histogram;
use super::items::{Container, FileItems, Item, ItemKind, non_trivial_lines, parse_items};
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
    let (old_cands, new_cands) = collect_candidates(files, &parsed, &settled, &claimed);
    let (moved, old_used, new_used) = pair_moves(&old_cands, &new_cands, files);
    for (file_index, range) in moved {
        ranges[file_index].push(range);
    }

    // Step 4: candidates the move pass left behind still pair when their
    // bodies are similar enough; the differing lines stay red and green.
    fn unpaired<'a>(cands: &[Cand<'a>], used: &HashSet<End>) -> Vec<Cand<'a>> {
        cands
            .iter()
            .filter(|(end, _)| !used.contains(end))
            .copied()
            .collect()
    }
    let old_left = unpaired(&old_cands, &old_used);
    let new_left = unpaired(&new_cands, &new_used);
    let (edited, new_used_by_edit) = pair_edited(&old_left, &new_left, files);
    for (file_index, range) in edited {
        ranges[file_index].push(range);
    }

    // Step 5: what still pairs with nothing may be a copy of an item that
    // never went away.
    let new_left = unpaired(&new_left, &new_used_by_edit);
    let candidate_ends: HashSet<End> = new_cands.iter().map(|(end, _)| *end).collect();
    for (file_index, range) in find_copies(&new_left, &candidate_ends, files, &parsed) {
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct End {
    file: usize,
    item: usize,
}

/// A candidate with its item record, so pairing never re-resolves indices.
type Cand<'a> = (End, &'a Item);

/// The move candidates of every unsettled file: items whose non-trivial
/// lines are at least 90% removed (old side) or added (new side). Steps 3
/// to 5 all draw from this one pool.
fn collect_candidates<'a>(
    files: &[ClassifyInput<'_>],
    parsed: &'a [Option<(FileItems, FileItems)>],
    settled: &[Option<FileClassSummary>],
    claimed: &[(Vec<usize>, Vec<usize>)],
) -> (Vec<Cand<'a>>, Vec<Cand<'a>>) {
    let mut olds = Vec::new();
    let mut news = Vec::new();
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
                let cand = (
                    End {
                        file: file_index,
                        item: item_index,
                    },
                    it,
                );
                match side {
                    Side::Old => olds.push(cand),
                    Side::New => news.push(cand),
                }
            }
        }
    }
    (olds, news)
}

/// Step 3: candidates pair one to one by token hash, ties broken by name
/// and container, then nearest path, else not at all. Section 4 then
/// decides whether identical tokens still mean the same thing. Returns the
/// ranges with the index of the file each belongs to, and the ends that
/// found a partner - an end whose pair was swallowed by a moved container
/// counts as paired, so the later steps never re-pair it.
fn pair_moves<'a>(
    olds: &[Cand<'a>],
    news: &[Cand<'a>],
    files: &[ClassifyInput<'_>],
) -> (Vec<(usize, ClassifiedRange)>, HashSet<End>, HashSet<End>) {
    let mut by_hash: HashMap<u64, (Vec<Cand<'a>>, Vec<Cand<'a>>)> = HashMap::new();
    for &cand in olds {
        by_hash.entry(cand.1.token_hash).or_default().0.push(cand);
    }
    for &cand in news {
        by_hash.entry(cand.1.token_hash).or_default().1.push(cand);
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
    let old_used = pairs.iter().map(|((end, _), _)| *end).collect();
    let new_used = pairs.iter().map(|(_, (end, _))| *end).collect();
    (ranges, old_used, new_used)
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
    if let Some(proof) = context_changed(old_item, new_item, old_path, new_path) {
        // The reason names both ends, so it reads the same from either.
        return (Class::MovedContextChanged, proof.clone(), proof);
    }

    let module_changed = old_path != new_path || old_item.module_path != new_item.module_path;
    let origin_site = format!("`{new_path}:{}`", new_item.first_line);
    let destination_site = format!("`{old_path}:{}`", old_item.first_line);
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

/// The section 4 rules on their own: the full context-changed proof when
/// the surroundings that give identical tokens their meaning differ, `None`
/// when the move carries no such risk.
fn context_changed(
    old_item: &Item,
    new_item: &Item,
    old_path: &str,
    new_path: &str,
) -> Option<String> {
    let module_changed = old_path != new_path || old_item.module_path != new_item.module_path;
    let container_changed = old_item.container != new_item.container;
    let scope_changed = module_changed || container_changed;
    let context = |reason: &str| {
        format!(
            "moved from {} to {}: {reason}",
            place(old_item, old_path),
            place(new_item, new_path),
        )
    };
    if old_item.has_self_ty && scope_changed {
        Some(context("`Self` now means a different type"))
    } else if container_changed {
        Some(context("the container changed"))
    } else if old_item.cfgs != new_item.cfgs {
        Some(context("the enclosing `#[cfg]` differs"))
    } else if old_item.has_super_path && module_changed {
        Some(context("`super::` now resolves differently"))
    } else if old_item.has_restricted_vis && module_changed {
        Some(context(
            "a restricted visibility now names a different module",
        ))
    } else {
        None
    }
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

/// Line number and trimmed length of each non-blank line of an item,
/// parallel to its stored line hashes: the positions and weights the
/// similarity rule works with.
fn line_info(text: &str, item: &Item) -> Vec<(usize, usize)> {
    text.lines()
        .enumerate()
        .skip(item.first_line - 1)
        .take(item.last_line + 1 - item.first_line)
        .map(|(index, line)| (index + 1, line.trim()))
        .filter(|(_, line)| !line.is_empty())
        .map(|(line_no, line)| (line_no, line.chars().count()))
        .collect()
}

/// One step 4 pairing with everything its ranges need.
struct EditedPair<'a> {
    old: Cand<'a>,
    new: Cand<'a>,
    matched: Vec<(usize, usize)>,
    old_info: Vec<(usize, usize)>,
    new_info: Vec<(usize, usize)>,
}

/// Step 4: unpaired candidates of the same kind and name - module path
/// deliberately ignored, a move usually lands elsewhere - pair when both
/// have at least 5 non-trivial lines and the length-weighted similarity
/// `2 x matched / (old + new)` reaches 0.6. Lines the diff matched fold as
/// Moved; the rest stay red and green under an item-wide annotation.
/// Returns the ranges and the new-side ends it consumed.
fn pair_edited<'a>(
    olds: &[Cand<'a>],
    news: &[Cand<'a>],
    files: &[ClassifyInput<'_>],
) -> (Vec<(usize, ClassifiedRange)>, HashSet<End>) {
    let eligible = |text: &str, item: &Item| {
        non_trivial_lines(text, item.first_line, item.last_line).len() >= 5
    };
    type Group<'a> = (Vec<Cand<'a>>, Vec<Cand<'a>>);
    let mut groups: HashMap<(ItemKind, &str), Group<'a>> = HashMap::new();
    for &cand in olds {
        if eligible(files[cand.0.file].old_text, cand.1) {
            groups
                .entry((cand.1.kind, cand.1.name.as_str()))
                .or_default()
                .0
                .push(cand);
        }
    }
    for &cand in news {
        if eligible(files[cand.0.file].new_text, cand.1) {
            groups
                .entry((cand.1.kind, cand.1.name.as_str()))
                .or_default()
                .1
                .push(cand);
        }
    }

    let mut scored: Vec<(u64, (bool, bool), EditedPair<'a>)> = Vec::new();
    for (group_olds, group_news) in groups.into_values() {
        for &old in &group_olds {
            for &new in &group_news {
                // Equal tokens were step 3's to pair; an ambiguity there
                // stays unpaired rather than resurfacing as an edit.
                if old.1.token_hash == new.1.token_hash {
                    continue;
                }
                // Overlapping spans in one file are a rewrite in place, not
                // a move; plain red and green tell that story better.
                if old.0.file == new.0.file
                    && old.1.first_line <= new.1.last_line
                    && new.1.first_line <= old.1.last_line
                {
                    continue;
                }
                let old_info = line_info(files[old.0.file].old_text, old.1);
                let new_info = line_info(files[new.0.file].new_text, new.1);
                let matched = histogram::matches(&old.1.line_hashes, &new.1.line_hashes);
                let matched_weight: usize = matched.iter().map(|&(i, _)| old_info[i].1).sum();
                let total: usize = old_info.iter().chain(&new_info).map(|&(_, w)| w).sum();
                // 2m / (o + n) >= 0.6, kept in integers so the threshold
                // has no rounding edge.
                if total == 0 || 10 * matched_weight < 3 * total {
                    continue;
                }
                let permille = (2000 * matched_weight / total) as u64;
                let kin = (
                    old.1.container == new.1.container && old.1.module_path == new.1.module_path,
                    old.0.file == new.0.file,
                );
                scored.push((
                    permille,
                    kin,
                    EditedPair {
                        old,
                        new,
                        matched,
                        old_info,
                        new_info,
                    },
                ));
            }
        }
    }
    // Best similarity first; kinship breaks ties, then input order, so the
    // outcome never depends on hash-map iteration.
    scored.sort_by_key(|(permille, kin, pair)| {
        (
            std::cmp::Reverse(*permille),
            std::cmp::Reverse(*kin),
            pair.old.0,
            pair.new.0,
        )
    });

    let mut old_used: HashSet<End> = HashSet::new();
    let mut new_used: HashSet<End> = HashSet::new();
    let mut ranges = Vec::new();
    for (_, _, pair) in scored {
        if old_used.contains(&pair.old.0) || new_used.contains(&pair.new.0) {
            continue;
        }
        old_used.insert(pair.old.0);
        new_used.insert(pair.new.0);
        ranges.extend(edited_ranges(&pair, files));
    }
    (ranges, new_used)
}

/// The ranges of one moved-and-edited pair. Matched runs come first so the
/// summary resolves their lines before the item-wide range; that range is
/// the annotation anchor and keeps the edited lines meaningful. A changed
/// container (section 4) keeps every line meaningful: the runs and the
/// annotation switch to `MovedContextChanged`, so the app still shows the
/// note and the inner diff, but nothing folds.
fn edited_ranges(
    pair: &EditedPair<'_>,
    files: &[ClassifyInput<'_>],
) -> Vec<(usize, ClassifiedRange)> {
    let (old_end, old_item) = pair.old;
    let (new_end, new_item) = pair.new;
    let old_path = files[old_end.file].path;
    let new_path = files[new_end.file].path;
    let changed =
        (pair.old_info.len() - pair.matched.len()).max(pair.new_info.len() - pair.matched.len());
    let count = if changed == 1 {
        "1 line changed".to_string()
    } else {
        format!("{changed} lines changed")
    };

    let context = context_changed(old_item, new_item, old_path, new_path);
    let run_class = match context {
        Some(_) => Class::MovedContextChanged,
        None => Class::Moved,
    };
    let mut out = Vec::new();
    for ((old_first, old_last), (new_first, new_last)) in
        matched_runs(&pair.matched, &pair.old_info, &pair.new_info)
    {
        out.push((
            old_end.file,
            ClassifiedRange {
                path: old_path.to_string(),
                side: Side::Old,
                first_line: old_first,
                last_line: old_last,
                class: run_class,
                counterpart: Some(Counterpart {
                    path: new_path.to_string(),
                    first_line: new_first,
                    last_line: new_last,
                }),
                proof: format!("moved to `{new_path}:{new_first}`"),
            },
        ));
        out.push((
            new_end.file,
            ClassifiedRange {
                path: new_path.to_string(),
                side: Side::New,
                first_line: new_first,
                last_line: new_last,
                class: run_class,
                counterpart: Some(Counterpart {
                    path: old_path.to_string(),
                    first_line: old_first,
                    last_line: old_last,
                }),
                proof: format!("moved from `{old_path}:{old_first}`"),
            },
        ));
    }

    let (item_class, origin_proof, destination_proof) = match context {
        Some(reason) => {
            let proof = format!("{reason} · {count}");
            (Class::MovedContextChanged, proof.clone(), proof)
        }
        None => (
            Class::MovedAndEdited,
            format!("moved to `{new_path}:{}` · {count}", new_item.first_line),
            format!("moved from `{old_path}:{}` · {count}", old_item.first_line),
        ),
    };
    out.push((
        old_end.file,
        ClassifiedRange {
            path: old_path.to_string(),
            side: Side::Old,
            first_line: old_item.first_line,
            last_line: old_item.last_line,
            class: item_class,
            counterpart: Some(Counterpart {
                path: new_path.to_string(),
                first_line: new_item.first_line,
                last_line: new_item.last_line,
            }),
            proof: origin_proof,
        },
    ));
    out.push((
        new_end.file,
        ClassifiedRange {
            path: new_path.to_string(),
            side: Side::New,
            first_line: new_item.first_line,
            last_line: new_item.last_line,
            class: item_class,
            counterpart: Some(Counterpart {
                path: old_path.to_string(),
                first_line: old_item.first_line,
                last_line: old_item.last_line,
            }),
            proof: destination_proof,
        },
    ));
    out
}

/// Maximal runs of matches consecutive on both sides, as inclusive line
/// ranges per side - what tints or folds as one block. A blank line inside
/// a run is swallowed by its range; blanks prove nothing either way.
fn matched_runs(
    matched: &[(usize, usize)],
    old_info: &[(usize, usize)],
    new_info: &[(usize, usize)],
) -> Vec<((usize, usize), (usize, usize))> {
    let mut runs = Vec::new();
    let mut start = 0;
    while start < matched.len() {
        let mut end = start;
        while end + 1 < matched.len()
            && matched[end + 1].0 == matched[end].0 + 1
            && matched[end + 1].1 == matched[end].1 + 1
        {
            end += 1;
        }
        runs.push((
            (old_info[matched[start].0].0, old_info[matched[end].0].0),
            (new_info[matched[start].1].0, new_info[matched[end].1].0),
        ));
        start = end + 1;
    }
    runs
}

/// Step 5: a leftover new-side candidate whose tokens equal an item still
/// present elsewhere on the new side, where that hash also existed on the
/// old side, is a copy. The original stayed, so only the new side gets a
/// range, and it stays meaningful - duplication is worth reading.
fn find_copies(
    news: &[Cand<'_>],
    candidate_ends: &HashSet<End>,
    files: &[ClassifyInput<'_>],
    parsed: &[Option<(FileItems, FileItems)>],
) -> Vec<(usize, ClassifiedRange)> {
    let old_hashes: HashSet<u64> = parsed
        .iter()
        .flatten()
        .flat_map(|(old, _)| old.items.iter().map(|item| item.token_hash))
        .collect();
    let mut out = Vec::new();
    for &(end, item) in news {
        if !old_hashes.contains(&item.token_hash) {
            continue;
        }
        // The named original is the surviving twin: an item that is itself
        // freshly added makes a poor "copy of" target, so candidates rank
        // last; the candidate's own file ranks first.
        type Rank = (bool, bool, usize, usize);
        let mut best: Option<(Rank, usize, &Item)> = None;
        for (file_index, slot) in parsed.iter().enumerate() {
            let Some((_, new_items)) = slot else { continue };
            for (item_index, other) in new_items.items.iter().enumerate() {
                let other_end = End {
                    file: file_index,
                    item: item_index,
                };
                if other_end == end || other.token_hash != item.token_hash {
                    continue;
                }
                let rank = (
                    candidate_ends.contains(&other_end),
                    file_index != end.file,
                    file_index,
                    item_index,
                );
                if best.as_ref().is_none_or(|(held, _, _)| rank < *held) {
                    best = Some((rank, file_index, other));
                }
            }
        }
        let Some((_, original_file, original)) = best else {
            continue;
        };
        let original_path = files[original_file].path;
        out.push((
            end.file,
            ClassifiedRange {
                path: files[end.file].path.to_string(),
                side: Side::New,
                first_line: item.first_line,
                last_line: item.last_line,
                class: Class::Copied,
                counterpart: Some(Counterpart {
                    path: original_path.to_string(),
                    first_line: original.first_line,
                    last_line: original.last_line,
                }),
                proof: format!("copy of `{original_path}:{}`", original.first_line),
            },
        ));
    }
    out
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
        // meaningful - a hash match alone is no pairing. Delta's addition
        // is named a copy of a surviving twin, which does not fold.
        assert!(covering(Side::Old, 6) && covering(Side::Old, 11));
        assert!(!covering(Side::Old, 2));
        assert!(covering(Side::New, 1) && covering(Side::New, 6));
        let delta = results[0]
            .ranges
            .iter()
            .find(|r| r.side == Side::New && (r.first_line..=r.last_line).contains(&12))
            .expect("copy range");
        assert_eq!(delta.class, Class::Copied);
        assert_eq!(delta.proof, "copy of `src/a.rs:2`");
        assert_eq!(results[0].summary.meaningful_removed, 5);
        assert_eq!(results[0].summary.meaningful_added, 5);
        for range in &results[0].ranges {
            assert!(matches!(range.class, Class::Moved | Class::Copied));
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

    #[test]
    fn move_with_three_edited_lines_is_moved_and_edited() {
        let a_old = "fn keep() {\n    stay();\n}\nfn travel(x: u8) -> u8 {\n    let a = x + 1;\n    let b = a + 2;\n    let c = b + 3;\n    let d = c + 4;\n    let e = d + 5;\n    let f = e + 6;\n    let g = f + 7;\n    let h = g + 8;\n    h + 9\n}\n";
        let a_new = "fn keep() {\n    stay();\n}\n";
        let b_old = "fn other() {\n    here();\n}\n";
        let b_new = "fn other() {\n    here();\n}\nfn travel(x: u8) -> u8 {\n    let a = x + 1;\n    let b = a + 9;\n    let c = b + 3;\n    let d = c + 9;\n    let e = d + 5;\n    let f = e + 9;\n    let g = f + 7;\n    let h = g + 8;\n    h + 9\n}\n";
        let a_regions = diff_regions(a_old, a_new);
        let b_regions = diff_regions(b_old, b_new);
        let files = [
            rust_file("src/a.rs", a_old, a_new, &a_regions),
            rust_file("src/b.rs", b_old, b_new, &b_regions),
        ];
        let results = run(&files);

        let destination = results[1]
            .ranges
            .iter()
            .find(|r| r.class == Class::MovedAndEdited)
            .expect("annotation range");
        assert_eq!(destination.side, Side::New);
        assert_eq!(
            destination.proof,
            "moved from `src/a.rs:4` · 3 lines changed"
        );
        assert_eq!((destination.first_line, destination.last_line), (4, 14));
        assert_eq!(destination.counterpart.as_ref().unwrap().path, "src/a.rs");
        let origin = results[0]
            .ranges
            .iter()
            .find(|r| r.class == Class::MovedAndEdited)
            .expect("origin range");
        assert_eq!(origin.proof, "moved to `src/b.rs:4` · 3 lines changed");
        // Exactly the three edited lines stay red and green; the identical
        // lines fold as Moved on both ends.
        assert_eq!(results[0].summary.meaningful_removed, 3);
        assert_eq!(results[0].summary.meaningful_added, 0);
        assert_eq!(results[1].summary.meaningful_added, 3);
        assert_eq!(results[1].summary.meaningful_removed, 0);
        assert!(results[0].ranges.iter().any(|r| r.class == Class::Moved));
        assert!(results[1].ranges.iter().any(|r| r.class == Class::Moved));
    }

    #[test]
    fn move_with_an_edited_comment_is_moved_and_edited() {
        let a_old = "fn keep() {\n    stay();\n}\nfn note(x: u8) -> u8 {\n    // first wording\n    let a = x + 1;\n    let b = a + 2;\n    let c = b + 3;\n    c + 4\n}\n";
        let a_new = "fn keep() {\n    stay();\n}\n";
        let b_old = "fn other() {\n    here();\n}\n";
        let b_new = "fn other() {\n    here();\n}\nfn note(x: u8) -> u8 {\n    // second wording\n    let a = x + 1;\n    let b = a + 2;\n    let c = b + 3;\n    c + 4\n}\n";
        let a_regions = diff_regions(a_old, a_new);
        let b_regions = diff_regions(b_old, b_new);
        let files = [
            rust_file("src/a.rs", a_old, a_new, &a_regions),
            rust_file("src/b.rs", b_old, b_new, &b_regions),
        ];
        let results = run(&files);

        // The token hash includes comments, so this is no pure move; the
        // edited comment line is the one line left to read.
        let destination = results[1]
            .ranges
            .iter()
            .find(|r| r.class == Class::MovedAndEdited)
            .expect("annotation range");
        assert_eq!(
            destination.proof,
            "moved from `src/a.rs:4` · 1 line changed"
        );
        assert!(
            results[0]
                .ranges
                .iter()
                .all(|r| r.class != Class::Moved || r.proof.starts_with("moved to"))
        );
        assert_eq!(results[0].summary.meaningful_removed, 1);
        assert_eq!(results[1].summary.meaningful_added, 1);
    }

    #[test]
    fn rewritten_function_below_the_threshold_stays_plain() {
        let a_old = "fn keep() {\n    stay();\n}\nfn convert(x: u8) -> u8 {\n    let a = x + 1;\n    let b = a + 2;\n    let c = b + 3;\n    let d = c + 4;\n    d + 5\n}\n";
        let a_new = "fn keep() {\n    stay();\n}\n";
        let b_old = "fn other() {\n    here();\n}\n";
        let b_new = "fn other() {\n    here();\n}\nfn convert(x: u8) -> u8 {\n    let first = widen(x);\n    let second = clamp(first);\n    let third = scale(second);\n    let fourth = shift(third);\n    narrow(fourth)\n}\n";
        let a_regions = diff_regions(a_old, a_new);
        let b_regions = diff_regions(b_old, b_new);
        let files = [
            rust_file("src/a.rs", a_old, a_new, &a_regions),
            rust_file("src/b.rs", b_old, b_new, &b_regions),
        ];
        let results = run(&files);

        // Only the signature survives; the body is new, so nothing pairs
        // and the delete and the add are read in full.
        assert!(results.iter().all(|r| r.ranges.is_empty()));
        assert!(results[0].summary.meaningful_removed > 0);
        assert!(results[1].summary.meaningful_added > 0);
    }

    #[test]
    fn duplicated_function_with_the_original_kept_is_copied() {
        let old = "fn helper(x: u8) -> u8 {\n    x + 1\n}\nfn main_work() {\n    run();\n}\n";
        let new = "fn helper(x: u8) -> u8 {\n    x + 1\n}\nfn main_work() {\n    run();\n}\nfn helper(x: u8) -> u8 {\n    x + 1\n}\n";
        let regions = diff_regions(old, new);
        let files = [rust_file("src/a.rs", old, new, &regions)];
        let results = run(&files);

        let copied = results[0]
            .ranges
            .iter()
            .find(|r| r.class == Class::Copied)
            .expect("copy range");
        assert_eq!(copied.side, Side::New);
        assert_eq!((copied.first_line, copied.last_line), (7, 9));
        assert_eq!(copied.proof, "copy of `src/a.rs:1`");
        assert_eq!(
            copied.counterpart,
            Some(Counterpart {
                path: "src/a.rs".to_string(),
                first_line: 1,
                last_line: 3,
            })
        );
        // The duplication is worth reading: nothing folds.
        assert!(!Class::Copied.is_mechanical());
        assert_eq!(results[0].summary.mechanical_lines, 0);
        assert_eq!(results[0].summary.meaningful_added, 3);
    }

    #[test]
    fn similarity_at_the_threshold_pairs_and_one_more_edit_does_not() {
        // Line lengths are tuned so two edited lines sit exactly on the
        // 0.6 similarity line; the threshold is inclusive.
        let a_old = "fn keep() {\n    stay();\n}\nfn tune() {\n    let a = 100;\n    let b = 200;\n    let c = 300;\n    let d = 400;\n}\n";
        let a_new = "fn keep() {\n    stay();\n}\n";
        let b_old = "fn spare() {\n    rest();\n}\n";
        let b_at = "fn spare() {\n    rest();\n}\nfn tune() {\n    let a = 100;\n    let b = 900;\n    let c = 300;\n    let d = 900;\n}\n";
        let b_below = "fn spare() {\n    rest();\n}\nfn tune() {\n    let a = 100;\n    let b = 900;\n    let c = 900;\n    let d = 900;\n}\n";

        let a_regions = diff_regions(a_old, a_new);
        let b_regions = diff_regions(b_old, b_at);
        let files = [
            rust_file("src/a.rs", a_old, a_new, &a_regions),
            rust_file("src/b.rs", b_old, b_at, &b_regions),
        ];
        let results = run(&files);
        let destination = results[1]
            .ranges
            .iter()
            .find(|r| r.class == Class::MovedAndEdited)
            .expect("pairs at the threshold");
        assert_eq!(
            destination.proof,
            "moved from `src/a.rs:4` · 2 lines changed"
        );

        let b_regions = diff_regions(b_old, b_below);
        let files = [
            rust_file("src/a.rs", a_old, a_new, &a_regions),
            rust_file("src/b.rs", b_old, b_below, &b_regions),
        ];
        let results = run(&files);
        assert!(results.iter().all(|r| r.ranges.is_empty()));
    }

    #[test]
    fn edited_move_into_another_impl_keeps_every_line_meaningful() {
        let a_old = "struct Cache;\nimpl Cache {\n    fn fresh(n: u8) -> Self {\n        let a = n + 1;\n        let b = a + 2;\n        let c = b + 3;\n        Self::with(c)\n    }\n}\n";
        let a_new = "struct Cache;\nimpl Cache {\n}\n";
        let b_old = "struct Index;\nimpl Index {\n}\n";
        let b_new = "struct Index;\nimpl Index {\n    fn fresh(n: u8) -> Self {\n        let a = n + 1;\n        let b = a + 9;\n        let c = b + 3;\n        Self::with(c)\n    }\n}\n";
        let a_regions = diff_regions(a_old, a_new);
        let b_regions = diff_regions(b_old, b_new);
        let files = [
            rust_file("src/a.rs", a_old, a_new, &a_regions),
            rust_file("src/b.rs", b_old, b_new, &b_regions),
        ];
        let results = run(&files);

        // The container changed, so the whole item stays meaningful: the
        // note and the inner diff are shown, but no line folds.
        let origin = results[0]
            .ranges
            .iter()
            .find(|r| r.side == Side::Old && (r.first_line, r.last_line) == (3, 8))
            .expect("annotation range");
        assert_eq!(origin.class, Class::MovedContextChanged);
        assert_eq!(
            origin.proof,
            "moved from `impl Cache` to `impl Index`: `Self` now means a different type · 1 line changed"
        );
        assert!(
            results
                .iter()
                .flat_map(|r| r.ranges.iter())
                .all(|r| r.class == Class::MovedContextChanged)
        );
        assert!(results.iter().all(|r| r.summary.mechanical_lines == 0));
        // The inner diff survives as counterpart sub-ranges for the app.
        assert!(
            results[1]
                .ranges
                .iter()
                .any(|r| r.side == Side::New && r.first_line == 3 && r.last_line < 8)
        );
    }
}
