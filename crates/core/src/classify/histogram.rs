//! Line diff for the similarity step, hand-rolled on hash sequences.
//!
//! Lines unique on both sides anchor the match - the histogram idea: a line
//! occurring once per side can only pair with itself - and the spans between
//! anchors recurse. A small anchorless span falls back to a plain LCS; a
//! large one matches nothing, which errs toward meaningful, the safe
//! direction. Hand-rolled rather than a crate because the workspace reserves
//! diff dependencies for the real backend.

/// Matched index pairs between two hash sequences, ascending on both sides.
pub(crate) fn matches(old: &[u64], new: &[u64]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    span(old, 0, new, 0, &mut out);
    out
}

/// One span to diff: trims the common lead and tail, hands the rest to
/// [`middle`]. Appends in ascending order so callers never sort.
fn span(old: &[u64], old_base: usize, new: &[u64], new_base: usize, out: &mut Vec<(usize, usize)>) {
    let limit = old.len().min(new.len());
    let mut lead = 0;
    while lead < limit && old[lead] == new[lead] {
        out.push((old_base + lead, new_base + lead));
        lead += 1;
    }
    let mut tail = 0;
    while lead + tail < limit && old[old.len() - 1 - tail] == new[new.len() - 1 - tail] {
        tail += 1;
    }
    middle(
        &old[lead..old.len() - tail],
        old_base + lead,
        &new[lead..new.len() - tail],
        new_base + lead,
        out,
    );
    for offset in 0..tail {
        out.push((
            old_base + old.len() - tail + offset,
            new_base + new.len() - tail + offset,
        ));
    }
}

/// An anchorless span this large would make the LCS table noticeable; its
/// lines stay unmatched instead, and unmatched only ever means meaningful.
const LCS_LIMIT: usize = 10_000;

fn middle(
    old: &[u64],
    old_base: usize,
    new: &[u64],
    new_base: usize,
    out: &mut Vec<(usize, usize)>,
) {
    if old.is_empty() || new.is_empty() {
        return;
    }
    let anchors = anchors(old, new);
    if anchors.is_empty() {
        if old.len() * new.len() <= LCS_LIMIT {
            lcs(old, old_base, new, new_base, out);
        }
        return;
    }
    let (mut from_old, mut from_new) = (0, 0);
    for &(old_index, new_index) in &anchors {
        span(
            &old[from_old..old_index],
            old_base + from_old,
            &new[from_new..new_index],
            new_base + from_new,
            out,
        );
        out.push((old_base + old_index, new_base + new_index));
        (from_old, from_new) = (old_index + 1, new_index + 1);
    }
    span(
        &old[from_old..],
        old_base + from_old,
        &new[from_new..],
        new_base + from_new,
        out,
    );
}

/// The anchor set: hashes unique on both sides, thinned to the longest
/// subsequence that keeps both sides ascending - a crossing anchor pair
/// cannot both be kept without reordering lines.
fn anchors(old: &[u64], new: &[u64]) -> Vec<(usize, usize)> {
    use std::collections::HashMap;
    let census = |side: &[u64]| {
        let mut seen: HashMap<u64, (usize, usize)> = HashMap::new();
        for (index, &hash) in side.iter().enumerate() {
            seen.entry(hash).or_insert((0, index)).0 += 1;
        }
        seen
    };
    let old_seen = census(old);
    let new_seen = census(new);
    let unique: Vec<(usize, usize)> = old
        .iter()
        .enumerate()
        .filter_map(|(index, hash)| {
            let &(old_count, _) = old_seen.get(hash)?;
            let &(new_count, new_index) = new_seen.get(hash)?;
            (old_count == 1 && new_count == 1).then_some((index, new_index))
        })
        .collect();
    longest_increasing(&unique)
}

/// Longest subsequence with strictly increasing new-side indices; the
/// old-side indices already ascend. Patience piles with back-pointers.
fn longest_increasing(pairs: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut tails: Vec<usize> = Vec::new();
    let mut prev = vec![usize::MAX; pairs.len()];
    for (index, &(_, new_index)) in pairs.iter().enumerate() {
        let pos = tails.partition_point(|&tail| pairs[tail].1 < new_index);
        if pos > 0 {
            prev[index] = tails[pos - 1];
        }
        if pos == tails.len() {
            tails.push(index);
        } else {
            tails[pos] = index;
        }
    }
    let Some(&last) = tails.last() else {
        return Vec::new();
    };
    let mut chain = Vec::new();
    let mut current = last;
    loop {
        chain.push(pairs[current]);
        if prev[current] == usize::MAX {
            break;
        }
        current = prev[current];
    }
    chain.reverse();
    chain
}

/// Plain LCS over a span with no unique line to anchor on.
fn lcs(old: &[u64], old_base: usize, new: &[u64], new_base: usize, out: &mut Vec<(usize, usize)>) {
    let mut table = vec![vec![0usize; new.len() + 1]; old.len() + 1];
    for old_index in (0..old.len()).rev() {
        for new_index in (0..new.len()).rev() {
            table[old_index][new_index] = if old[old_index] == new[new_index] {
                table[old_index + 1][new_index + 1] + 1
            } else {
                table[old_index + 1][new_index].max(table[old_index][new_index + 1])
            };
        }
    }
    let (mut old_index, mut new_index) = (0, 0);
    while old_index < old.len() && new_index < new.len() {
        if old[old_index] == new[new_index] {
            out.push((old_base + old_index, new_base + new_index));
            old_index += 1;
            new_index += 1;
        } else if table[old_index + 1][new_index] >= table[old_index][new_index + 1] {
            old_index += 1;
        } else {
            new_index += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_sequences_match_fully() {
        let side = [1, 2, 3, 4];
        let matched = matches(&side, &side);
        assert_eq!(matched, [(0, 0), (1, 1), (2, 2), (3, 3)]);
    }

    #[test]
    fn an_edit_between_anchors_leaves_only_that_line_unmatched() {
        let old = [10, 20, 30, 40, 50];
        let new = [10, 20, 99, 40, 50];
        let matched = matches(&old, &new);
        assert_eq!(matched, [(0, 0), (1, 1), (3, 3), (4, 4)]);
    }

    #[test]
    fn a_shifted_block_keeps_ascending_matches() {
        let old = [1, 2, 3, 4, 5];
        let new = [4, 5, 1, 2, 3];
        let matched = matches(&old, &new);
        // Either block may win; the matches must stay ascending on both
        // sides and cover one whole block.
        assert!(matched.len() >= 2);
        assert!(
            matched
                .windows(2)
                .all(|w| w[0].0 < w[1].0 && w[0].1 < w[1].1)
        );
    }

    #[test]
    fn repeated_lines_fall_back_to_lcs() {
        let old = [7, 7, 7, 8];
        let new = [7, 7, 9, 7, 8];
        let matched = matches(&old, &new);
        assert!(
            matched
                .windows(2)
                .all(|w| w[0].0 < w[1].0 && w[0].1 < w[1].1)
        );
        assert_eq!(matched.len(), 4);
    }
}
