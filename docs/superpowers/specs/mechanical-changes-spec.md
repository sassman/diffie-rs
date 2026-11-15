# Spec: separating mechanical from meaningful changes

Status: reviewed with the owner on 2026-11-14; the items below marked
**Decided** were settled in that review. Statuses follow
`docs/design-decisions.md`: **Decided**, **Assumed**, **Open**. Everything
unmarked is Assumed.

Home: `docs/superpowers/specs/`, the convention the systematic-edits design
uses. Crate names: `diffie-core` (`crates/core`) and `diffie` (`crates/app`).

One decision here supersedes a Decided item in `docs/design-decisions.md`
(moved-code folds, owner, 2026-11-12, exact-match only): moves now pair by
token hash, so a re-indented move folds. Rewrite that item when phase 2
lands; until then the shipped behaviour stays exact-match.

## 1. Purpose

A reviewer's attention is the scarce resource. A branch that moves a module,
reformats two files and changes forty lines of behaviour shows up as thousands
of red and green lines. This feature marks the part that needs no reading and
gets it out of the way, so the forty lines stand out.

## 2. Principles

1. **Provable, not probable.** A change is mechanical only when the app can
   state the proof in one short phrase ("identical tokens", "whitespace
   only"). No scores, no model, no judgement.
2. **Wrong in one direction only.** Calling a meaningful change mechanical is
   the dangerous error. When any rule is unsure, the change is meaningful.
3. **Fold, never remove.** Mechanical changes stay in the diff, one key away.
4. **Say what was not checked.** Syntax can prove that code is identical. It
   cannot prove that it means the same in a new place. Where that gap exists,
   the label says so.
5. **Never tick for the reviewer.** Mechanical changes are not marked reviewed
   automatically.

## 3. Classes

Every changed line gets exactly one class.

### Mechanical

| Class | Definition | Proof shown to the reviewer |
| --- | --- | --- |
| **Moved** | An item (function, method, struct, enum, trait, impl, const, static, type alias, macro definition) is gone from one place and present in another with an identical token stream. Includes reordering inside one file. The item's attributes and doc comments belong to it. | "moved to `path:line`, identical tokens" |
| **Renamed file** | Git reports the file as renamed with 100% similarity. | "renamed from `old/path`, identical" |
| **Formatting** | Same place, identical token stream, different whitespace or line breaks. For non-Rust files: the removed and the added lines of a region, each split on whitespace, yield the same word sequence (so re-wrapped lines pass and joined words fail). | "whitespace only" |
| **Generated** | The file is not written by hand: a path matched by the generated-files list, a `linguist-generated` attribute in `.gitattributes`, or `@generated` in the first five lines. | "generated file" |

**Decided:** Moved pairs by token hash, so a re-indented move is Moved, not
an edit - but the indentation change is worth seeing, just not first, so
the label names it: "moved to `path:line`, identical tokens · re-indented".
A byte-identical move reads "moved to `path:line`, identical". This
supersedes the exact-match rule in `docs/design-decisions.md` (see the note
at the top).

**Decided:** Formatting is strict token equality. No rustfmt allow-list:
trailing commas and reordered `use` lists change tokens and stay meaningful.

Notes on feasibility:

- Renamed file: `GitCli` already passes `--find-renames`, but `summarize`
  drops the `similarity index` line; it must be parsed. The New-for-me
  since-tick diff is blob against blob and loses rename information, so the
  class applies only where git reports the rename.
- Generated: the hard-coded list in the app's `is_generated` moves to core
  and becomes the default list (section 7).

Generated differs in kind: it is not proven equal, it is declared not worth
reading. It is in the same bucket but always carries its own label.

**Decided (owner, 2026-11-15):** generated files are classified and listed -
the Guided "only generated" root, the tint, the counts in the summary and
progress all stay - but they never fold in the diff view. Folding was tried
on `Cargo.lock` and read badly; a generated file's diff renders unfolded
like any normal file, and the file-level collapse (generated files start
collapsed) remains the only thing standing between the reviewer and its
content.

### Comment-only

**Decided:** a class of its own, not folded into Meaningful.

Same place, same item: the token streams are equal once comment and
doc-comment tokens are dropped, and differ otherwise. Proof: "comment text
only". Tinted and counted in its own bucket, filterable on demand, **never
folded by default**. Rust files only; elsewhere comment edits stay
meaningful.

### Meaningful, with help

| Class | Definition | What the app adds |
| --- | --- | --- |
| **Moved and edited** | An item is paired with an item of the same name elsewhere and their bodies are similar (section 5, step 4). Lines identical to the origin are classed *Moved*; the rest stay meaningful. | "moved from `path:line` · 3 lines changed"; only the 3 lines keep red and green |
| **Moved, context changed** | Tokens are identical but the surroundings that give them meaning differ (section 4). | Shown as moved, not folded, with the reason |
| **Copied** | An identical item appears in a new place and the original is still there. | "copy of `path:line`" |

**Decided:** the token hash includes comments. A move that also edits a
comment is Moved and edited, so the edited comment lines stay red and green
and nothing a reviewer should see disappears into a fold.

### Meaningful

Everything else, including: visibility and attribute changes, changes to
signatures, comment edits in non-Rust files, and any change in a file that
could not be parsed.

## 4. When an identical move is not mechanical

Identical tokens can resolve to different things after a move. Without
semantic analysis the app cannot check that, so it uses these rules.

A move is **Moved, context changed** (meaningful) when any of these holds:

- The item changes its kind of container: free function to method or back,
  one `impl` target type to another, inherent impl to trait impl or back.
- The enclosing `#[cfg(...)]` attributes differ between origin and
  destination.
- The item's tokens contain `Self`, `super::`, or a restricted visibility
  (`pub(super)`, `pub(in ...)`), and the module or impl changed.

**Decided:** a move to a different module that triggers none of the above
stays *Moved* and folds like any move, but its label reads "moved to
`path:line`, identical tokens · name resolution not checked", because `use`
declarations differ per module.

A move within the same module and container has no caveat.

## 5. Detection pipeline (`diffie-core`)

**Input.** `ReviewBackend::patch` returns `ScopedPatch { text, files,
new_side }`: patch text only, no file contents. The classifier needs each
changed file's full old and new content. Content retrieval is a backend
capability of its own, decoupled from diff processing:

- A new trait method fetches a blob's content by id (plus one for the
  working-tree side). The diff gains `--full-index` so the patch's `index`
  lines carry full blob ids to fetch by.
- For the combined custom scope (skipped commits) those blobs live in the
  scratch object directory, which today dies when `combine` returns. The
  backend keeps the scratch alive until the next load instead, so the same
  retrieval method reads combined blobs too (the scratch's alternates
  already cover the real objects). It stays temporary: dropped and
  recreated per load, never inside the repository.
- For every other scope, the method reads real objects or the working tree.

**Threading.** `load()` runs as one background task and everything in it
delays first paint, so classification runs as a *second* background task
with its own revision guard - the pattern the whole-branch search fetch
uses. It fetches contents through the retrieval method, classifies, and the
UI applies classes when they arrive. Show the diff first.

Definition used below - **non-trivial line**: trimmed text of three
characters or more. Filters blank lines and lone delimiters. (A guess; tune
with the other thresholds.)

**Step 0, file rules (any language, no parser).**
Generated, Renamed file, and whitespace-only Formatting. A file settled here
skips the remaining steps.

**Step 1, item lists (Rust only).**
Parse old and new content of each changed `.rs` file with rust-analyzer's
syntax crate (`ra_ap_syntax`, pinned to an exact version - the core's first
parser dependency; it does not touch `GitCli`, the cheap backend stays
cheap). For each item record:

- path, kind, name, container (inline module chain, impl target type, trait),
  enclosing `cfg` attributes
- line range including attributes and doc comments
- token hash: all tokens except whitespace; comments included
- comment-stripped token hash (for Comment-only)
- trimmed, non-blank line hashes (for step 4)

Items inside macro invocations are not seen and stay meaningful. A file with
a parse error on either side gets no item-level classification.

Cache each file's list by git blob id under `<git-dir>/review/.cache/`.
Dot-prefixed, because `review::prompt` and the colleague scan treat every
subdirectory of `review/` as a review; both must learn to skip
dot-directories in the same change. The same content is never parsed twice.
Eviction policy: Open.

**Step 2, formatting and comment-only.**
Same path, same container and name:

- equal token hash, different text: Formatting.
- token hash differs, comment-stripped hash equal: Comment-only.
- a whole file's token hash equal: the whole file is Formatting.

**Step 3, pure moves.**
Candidates are items whose non-trivial lines are at least 90% removed (old
side) or added (new side). Pair old and new candidates with equal token hash.
The pairing must be one to one: if several candidates share a hash, pair by
equal name and container (impl target type and trait), then by nearest path;
if still ambiguous, pair none. Apply section 4.

The existing `diffie_core::moved::moved_blocks` (exact-match, patch-text
based) is subsumed by this step and retires when phase 2 lands.

**Step 4, moved and edited.**
For unpaired candidates: pair old and new items with the same kind and name
(module path ignored) when the item has at least 5 non-trivial lines and
similarity is at least 0.6. Similarity is `2 x matched / (old + new)`, lines
weighted by length, matched lines taken from a histogram diff of the trimmed
lines. That diff also yields the per-line classes. Section 4 applies here
too: a changed container keeps the whole item meaningful, but the "moved
from" note and the inner diff are still shown.

**Step 5, copies.**
A new-side candidate whose hash equals an item that is still present on the
new side elsewhere and existed on the old side: Copied.

**Output.**

```text
ClassifiedRange { path, side, first_line, last_line, class,
                  counterpart: Option<(path, first_line, last_line)>,
                  proof: text }
FileClassSummary { path, meaningful_added, meaningful_removed,
                   mechanical_lines, comment_only_lines,
                   fully_mechanical: Option<class> }
```

The classifier is a pure function of (old text, new text, changed lines) per
file plus one cross-file pairing pass. It has no git access, so it is unit
testable without a repository.

**Budget.** Classification never delays the first paint (see Threading).
Target for a warm cache: under 100 ms for 300 changed files. Cold: under 2 s.
(Guesses; measure.)

## 6. Presentation

**Scope bar.** After the commit range: a toggle with three states: folded,
shown, only mechanical - no counts text in the bar; counts live on the
Guided roots and the progress line. **Decided:** folded is the default.
Key: `m` cycles (free today; the menu entry needs an unbound `Menu*`
wrapper action like the other single-letter bindings). The choice is
remembered - session-only until settings persistence exists (section 7).

**Guided tab (Decided, owner, 2026-11-15).** The file panel has two tight
tabs on the panel itself, "Files" and "Guided" - a tab strip, not a toggle
button; file-header height, `xsmall`. "Files" is today's tree, including
the mechanical bottom group. "Guided" shows one tree root per class
present in the diff - "only moved", "only renamed", "only comments",
"only formatting", "only generated" - each root a filter context: under
it, the same file tree reduced to the files carrying that class, the root
showing its file and line counts. A root with nothing under it is hidden.
Selecting a file there behaves exactly like selecting it in Files; the
root is only an entry point into the same tree, filtered. Pressing a
root makes it the active filter: the diff panel shows only that root's
files until the root is pressed again or the Files tab is chosen. The
active root is highlighted; its chevron is a separate target that folds
the subtree without touching the filter. The scope bar
carries no counts text - counts live on the roots and the progress line.
The review-guide content from systematic-edits can join this tab later;
the guided view no longer waits for that branch.

- A filter is not a scope. It narrows the presentation of the current
  scope's diff, shown by the active tab, never silently. Reviewed ticks
  stay per file, per content id, filter-independent.
- Fold, never remove: a commented range stays reachable from every root.
- `m` keeps cycling the diff toggle (folded, shown, only mechanical) in
  the scope bar.

**File panel.** Fully mechanical files move to a group at the bottom,
"Mechanical · 7", muted, each with a one-word class (moved, renamed,
formatting, generated). Mixed files show meaningful counts in colour and the
mechanical and comment-only line counts in grey.

**Diff, fully mechanical file.** Starts collapsed. The header carries the
proof and a jump, through the Kit's header slots
(`render_header_title_suffix`): "moved to `src/b.rs`, identical tokens".

**Diff, mixed file.**

- Mechanical lines lose red and green and get one neutral "moved" tint, on
  both the origin and the destination. The tint is a `DiffDecoration`
  background chained after the app's own fills (the app already paints
  change backgrounds itself, with `.change_background(false)`).
- A mechanical range of 4 lines or more folds into one row via the fork's
  `DiffFold` / `set_folds`, which folds changed rows: "48 lines moved to
  `src/b.rs:40`, identical tokens". Generated ranges are the exception and
  never fold (Decided, owner, 2026-11-15; see section 3). Known limits to
  design around: the
  placeholder renders its label only, a click reveals, there is no inline
  link or jump button without a Kit render hook; in split view a fold hides
  the whole paired row; folds must be re-applied after `set_files` (the app
  already orders it that way).
- Moved and edited: an annotation at the destination, "moved from
  `src/a.rs:120` · 3 lines changed". Kit annotations render beneath a line,
  so it sits under the first line of the range. Identical lines get the
  tint, changed lines keep red and green.
- Moved, context changed: tint, never folded, annotation with the reason,
  for example "moved from `impl Cache` to `impl Index`: `Self` now means a
  different type".
- Comment-only: tint and the label "comment text only". Never folded by
  default; the Guided "only comments" root narrows the tree to those files
  (a plain menu entry folds the ranges until phase 4).

**Navigation.** `]` and `[` skip folded ranges already: the Kit computes
`change_rows` after folding. A new key jumps between a moved range and its
counterpart, expanding and scrolling as needed (proposed: `%`, free today).

**Comments.** Allowed on mechanical lines. A range that carries a comment is
never folded.

**Reviewed ticks.** Unchanged: per file, by the reviewer. One explicit
action, "Mark fully mechanical files as reviewed" - a menu entry until the
command palette exists, then also there. The progress counter reads "3 of 19
files reviewed · 7 mechanical".

**Wording.** Every mechanical label states its proof. Never "safe", "no
risk" or "ignore".

## 7. Settings

No reviewer settings persistence exists yet, so phase 1 ships hard-coded
defaults and the configurable parts wait for it:

- Generated-files list: the app's current `is_generated` list moves to core
  and becomes the default, extended by `*.min.*` and `**/generated/**`.
  Per-repository glob patterns come once settings exist.
- Fold threshold (default 4 lines).
- The feature can be switched off as a whole: a menu toggle, session-only
  until settings exist.

## 8. Not in this spec

- Splitting a change into concerns.
- Renamed identifiers and other repeated edits (the systematic-edits
  feature, in flight on its own branch). Noted for later: the item records
  of step 1 are the natural hook - a second, rename-normalized token hash
  would pair items identical up to one consistently applied rename, proof
  "identical after renaming `old` to `new`". Riskier than plain token
  equality, because a rename can change what a token binds to, so it waits
  for that feature to define the rules.
- Extract-function and inline-function detection.
- Semantic analysis of any kind.
- Languages other than Rust, apart from the file rules in step 0.

## 9. Acceptance tests

Core tests, each on fixed old and new texts:

1. Function moved to another file, same module depth, unchanged: Moved, with
   the resolution caveat.
2. Two functions swapped inside one file: both Moved, no caveat, nothing
   else flagged.
3. Function moved and re-indented, tokens equal: Moved, label carries
   "re-indented". Byte-identical move: no such note.
4. Function moved from a free function into an `impl` block: Moved, context
   changed.
5. Method moved between impls of different types, body contains `Self`:
   Moved, context changed, reason names `Self`.
6. Function moved with its `#[test]` attribute and doc comment: one range
   covering all three.
7. Function moved and three lines edited: Moved and edited; exactly those
   three lines meaningful.
8. Function moved and one comment line edited: Moved and edited (the hash
   includes comments).
9. Function moved and rewritten (similarity below 0.6): plain delete and add.
10. Three identical `fn default()` bodies removed, three added: no pairing by
    hash alone; paired only where impl type and trait match. The added third
    body may additionally be classed Copied of a surviving twin.
11. File reformatted, tokens equal: Formatting for the whole file.
12. File reformatted and one token changed: only the items with equal tokens
    are Formatting; the changed item is meaningful.
13. Function duplicated, original kept: Copied.
14. File renamed at 100%: Renamed file. Renamed with one edit: the edit is
    meaningful and no mechanical class is claimed for the file.
15. `Cargo.lock` changed: Generated.
16. File with a syntax error on one side: no item-level classes.
17. Item defined inside a macro invocation and moved: meaningful.
18. Comment edited in place, code tokens equal: Comment-only. Doc-comment
    variant too.
19. Markdown file re-wrapped, word sequence equal: Formatting. One word
    changed: meaningful.

Backend tests: with skipped commits in a custom scope, combined blobs are
still retrievable after the patch call, the scratch directory is dropped on
the next load, and the repository gains no objects; the `similarity index`
of a rename is parsed.

UI checks, by hand: folded by default, `m` cycles, jump works in both
directions, a commented range does not fold, counts on the Guided roots
equal the sum over files.

## 10. Phases

Each phase is usable alone.

1. **File rules.** Generated (list moved to core, `.gitattributes` and
   `@generated` added), Renamed file (similarity parsed), whitespace-only.
   No parser. The scope-bar counter, the file-panel group and the toggle.
2. **Pure moves, formatting, comment-only.** The parser, the cache, steps 1
   to 3, the tint, folding, the jump key. Retires `moved_blocks`.
3. **Moved and edited, copies.** Steps 4 and 5 and the inner diff.
4. **Guided tab.** The Files/Guided tab strip and the class-filtered tree
   roots.

## 11. Settled in review (owner, 2026-11-14)

1. Default state: folded.
2. Comments are part of the token hash; a move that edits a comment is
   Moved and edited.
3. Formatting is strict token equality; no rustfmt allow-list.
4. Cross-module moves fold, with the name-resolution caveat in the label.
5. Comment-only and doc-only changes are a labelled class of their own,
   never folded by default, filterable.
6. Moves pair by token hash; a re-indented move folds, with the
   re-indentation named in its label - worth seeing, not worth seeing
   first. Supersedes the exact-match Decided item in
   `docs/design-decisions.md`; rewrite it when phase 2 lands.
7. The Kit question is answered in the code: the owner's fork folds changed
   rows (`DiffFold`, `set_folds`) and decorations override backgrounds. No
   fallback needed.
8. Class filters surface as a Guided tab in the file panel: one tree root
   per class, each filtering the same tree; files and guided are switched
   by a tight tab strip on the panel, not a toggle button (owner,
   2026-11-15).

## 12. Open questions

1. **Thresholds**: 90% changed lines, 0.6 similarity, 5 lines, fold at 4,
   3-character non-trivial line. All guesses; tune on the owner's
   repositories.
2. **Cache eviction**: `review/.cache/` grows per blob; when and how to
   prune. (The comment-only filter question is gone: it is the Guided
   "only comments" root. Until phase 4, a plain menu entry folds those
   ranges.)
