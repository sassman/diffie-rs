# Design decisions

This records what was settled while designing the diff review app, and what
was not. It is the source of truth for product behaviour. The visual reference
is `docs/mockup/main-screen.html`.

Every item carries a status:

- **Decided**: the owner said so explicitly.
- **Assumed**: a default proposed during design that the owner did not object
  to, or was not asked about. Treat as a working default and confirm before
  building much on top of it.
- **Open**: unresolved. Do not pick silently; see section 16.

"App" below means this project. It has no final name yet (Open).

## 1. What this is

A native desktop app for reviewing git changes: several commits, a whole
branch, or a hand-picked subset, with comments and suggestions on any line or
selection. It exists because existing diff viewers are weak at multi-commit
review and at annotating.

Baseline (Decided): GitHub's pull request "Files changed" view plus the ideas
of difit, but not in a browser and more ergonomic.

Kept from GitHub: file tree on the left, all files in one scrolling view, split
and unified layouts, expandable context, inline comment threads, suggestion
blocks, a per-file checkbox.

Kept from difit: launch from the terminal with a source and an optional target,
generated files start collapsed, comments can be copied out as a prompt for an
AI agent.

Where it must be better: choosing commits (section 5), keyboard-first use
(section 13), comments on a selection inside a line (section 9), no waiting.

## 2. Constraints

| Constraint | Status |
| --- | --- |
| Rust | Decided |
| macOS, Windows and Linux | Decided |
| No web technology. Native UI on GPUI | Decided |
| GPUI through GPUI Kit (`gpui-kit`, by Longbridge), not raw upstream GPUI | Decided |
| Theme support | Decided |
| UX quality is the priority | Decided |
| Tight, Zed-like density: controls mindful of the space they take | Decided |

A Tauri and Vue prototype was considered to iterate faster and rejected
(Decided). Do not reintroduce web UI, not even for prototyping.

GPUI Kit is the successor umbrella of `gpui-component`, from the same
maintainers. It pins its own GPUI snapshot (`gpui-pre`), so the app lists one
dependency and pins it exactly.

## 3. Architecture

Decided:

- Two crates. `diffie-core` has no UI dependency: repository access, diff
  production, the review file. `diffie` is the GPUI Kit UI.
- The UI talks to the repository only through the `ReviewBackend` trait.
- The first backend is deliberately cheap: it shells out to `git`. The owner
  does not want effort spent there until the UI and flow have settled. A real
  backend (`gix` for repository access, `imara-diff` for diffing) replaces it
  later behind the same trait.
- Diff algorithm: git's `histogram` with the indent heuristic, for hunks that
  stay block-consistent.
- UI first. The UI was designed and iterated before the core.

Assumed:

- The hand-over format from core to UI is git-format unified patch text. GPUI
  Kit's `Diff` component parses exactly that, and any backend can produce it.
- Backend calls block and always run on a background task.
- The app never writes to the reviewed repository's objects, index or working
  tree. Its only writes are review files under `.git/review/`.

## 4. Naming the comparison

Decided:

- The UI says "changes in `source` going into `target`", read in the direction
  of the merge. The owner's mental model is a branch and the target it will be
  merged or squashed into.
- A swap button reverses the direction (for example when sitting on `main` and
  comparing with `next`).
- The term "merge-base" confuses the owner. Do not show it in the UI.
- Opened with neither `--source` nor `--target`, the app reviews the
  uncommitted changes (untracked files included) going into the checked-out
  branch, shown as "uncommitted going into `branch`" (owner, 2026-11-11).
  The swap button hides there; the working tree cannot be a target.
- The command line reads like `git diff` (owner, 2026-11-11): positional
  revisions, `-C <path>` for the project, no positional path. One revision
  reviews the working tree going into it ("working tree going into `rev`"
  unless the revision is the checked-out branch, which is the uncommitted
  review); two revisions or `A..B` review the second going into the first.
  Two and three dots resolve the same way; the merge-base rule already
  governs the comparison.

Assumed:

- The default comparison is "only what the branch adds": from the common
  ancestor to the source tip, ignoring newer work on the target. The UI states
  this in those words.

Open: whether a second mode, "every difference between the two tips", is
exposed. The backend supports it (`CompareMode::EveryDifference`); no control
is designed.

## 5. Scope: what is being diffed

Decided:

- Both the big picture and smaller subsets matter. An explicit selector is
  crucial, and so is "what is new since my last review".
- One scope rules the whole view. Per-file state never overrides it silently.
- Three scopes:
  - **New for me**: each file is compared with the version the reviewer last
    ticked as reviewed. Unticked files show their full diff.
  - **Whole branch**: everything the branch adds.
  - **Custom**: the commits checked in the commit picker.
- The commit picker is a dropdown opened by clicking the range in the scope
  bar. The owner called this design "perfect"; do not redesign it.
  - Every commit is an item with two rows: the title, then author, age and
    hash (owner, 2026-11-11; previously hash and date over the title).
  - Every item leads with a checkbox. The range is composed of individual
    commits.
  - Commits in the middle can be left unchecked, so a review can follow a
    topic that is not one continuous range.
- "Commits since last review" must be available as a range definition.
- Only the reviewer's own reviews define "last review". Colleagues' files do
  not.

Assumed:

- The picker groups commits under "New since your review" and "Already
  reviewed". Each heading has a checkbox for the whole group.
- The picker has a search field (message, hash, author) and marks skipped
  commits. Commits that carry comments show a count.
- Shift-click checks a run of commits; Space toggles one.
- A non-contiguous selection is shown as one combined diff: only the checked
  commits are replayed onto the starting point. When a skipped commit touched
  the same lines, the affected file cannot be combined; that file stops before
  the failing commit and says so. (The alternative, stacking each commit's
  patch one after another, is Open in section 16.)

Rejected along the way, with the reason, so they are not proposed again:

- A horizontal rail of commit cards: unusable at 40 to 60 commits.
- A slider with one tick per commit: scales, but ticks are anonymous and the
  owner found it "not nice".
- A commit list in the left sidebar: duplicated the dropdown and crowded the
  file tree.

## 6. Reviewed ticks and "last review"

Decided:

- The checkbox at the top right of each file's diff marks the file as
  reviewed.
- When every changed file is ticked, the review is done, and that point is the
  "last review" marker. Ticking only some files sets no marker.

Assumed:

- A tick stores the file's content id (blob), not a commit. It clears itself
  when the file changes again and survives rebases that leave the file alone.
- The app remembers the ticked version, so "New for me" can show only what
  changed since.
- Ticking a file collapses it.
- In "New for me", a ticked file with no changes stays visible, collapsed, as
  "no changes since your review".
- The label is "Reviewed", not "Viewed".

## 7. Files

Decided:

- One continuous scroll through all files.
- File boundaries can be expanded and collapsed.
- Collapse all and expand all exist, as icon-only buttons.
- A file tree on the left, hidable, for quick jumps. Jumping scrolls to the
  file and expands it if collapsed. Tree and diff follow one order, the
  classic tree sort: folders on top, the project root's files at the bottom.
  The diff is resorted into it at load, so the file on top of the tree is
  the file on top of the diff (owner, 2026-11-11; first the tree followed
  git's path-sorted patch order, but the tree sort reads better and the
  main content can follow it just as well).
- The file panel runs the full height of the diff view.
- The file panel is condensed, not spacious.

Assumed: generated files such as lockfiles start collapsed; the tree shows a
status letter, comment count, added and removed lines, and a tick for reviewed
files.

Decided: a hide-whitespace toggle (the pilcrow icon in the title bar)
reloads the diff with `--ignore-all-space`; a file whose changes are all
whitespace leaves the view and the reviewed count while the toggle is on
(owner, 2026-11-12; a GitHub-style placeholder row for such files was not
built, git's own `-w` behaviour was kept).

Decided: a block the patch moves verbatim folds behind one labeled row at
both ends ("moved to b.rs:3-8" where it left, "moved from a.rs:3-8" where
it landed); a click reveals the lines, another folds them back (owner,
2026-11-12). Detection is exact-match only - an indentation change is an
edit worth seeing - and needs at least 20 alphanumeric characters, the
`git --color-moved` bar that keeps lone braces from pairing up. Moves
with edits in flight stay ordinary removals and additions for now; the
SemanticDiff-style "show only the in-flight edits" view is future work.

## 8. Layout

Decided: each split panel is half the window and scrolls its long lines on
its own (owner, 2026-11-12; the Kit strip that scrolled both as one canvas
was rejected, carried in the owner's fork until upstream takes it).

Decided: split view by default, switching to unified automatically when the
window is too narrow; one key toggles it and the choice is remembered.

Decided: `/` opens a search bar under the scope bar, seeded from the text
selection when there is one (owner, 2026-11-12). It searches the scoped
patch - what the view shows - matching smartcase; enter and shift-enter
walk the matches and tint their lines. An "All commits" checkbox extends
the search over the whole branch; stepping onto a hit the scoped patch
does not hold switches the scope to Whole branch and lands on it after
the reload. Per-file state never changes the scope; this explicit jump
does.

## 9. Comments

Decided:

- Comments and suggestions can be made on any line or selection, including a
  selection inside a line.
- The scope bar counts open comments ("3 open comments" in the comment
  accent, "comments handled" once every thread carries a resolving
  reaction), and `n`/`N` walk the open comments in reading order,
  revealing a collapsed file on landing (owner, 2026-11-12). The counter
  counts every open comment; the walk skips ones the other-revisions
  toggle currently hides, because a jump must land on something visible.
- Two entry points (owner, 2026-11-11): a + beside the hovered line number
  for line comments, and a + pinned at the pointer when a text selection
  with real content ends. A click or double click without selected text
  shows no button.
- A comment is written and read inline, directly under the line.
- A comment belongs to the commit that wrote the line (found by blame), not to
  the range that happened to be on screen. It then shows in every scope that
  contains that commit.
- Comments do not have to survive history rewrites, as long as going back to
  the revision still shows them.
- Comments from commits outside the current scope are shown by default
  (settled 2026-11-11; they started out hidden), with a toggle that hides
  them. They appear wherever their line is still unchanged.
- Comments can be marked done.

Assumed:

- The comment box names the commit it will attach to before saving.
- In split view a comment sits under the side it belongs to.
- Other-revision comments are visually quieter (dashed, dimmed).
- Each comment stores the commented lines as a snippet, so it stays readable
  after a rebase removes its commit.
- A comment on a removed line belongs to the commit that removed it.
- Actions per comment: edit, copy as prompt, delete. Only the author can edit
  or delete.

## 10. Reactions

Decided:

- A small, fixed set of emoji reactions, each tied to an agreed action. No
  free-form emoji.
- The four actions: acknowledged, done, won't fix, follow-up.
- Reactions are hidden behind one button, as on GitHub: an emoji icon with a
  plus opens a picker.
- Several reactions are given by pressing that button again; the picker lists
  only what is left, until nothing is left.

Assumed:

- The emoji for each action: 👀 Seen (acknowledged), ✅ Done, 🚫 Won't fix,
  📌 Follow-up.
- Done and Won't fix resolve a comment and fold it. Seen and Follow-up keep it
  open.
- Reactions take the place of replies in the first version (Open, section 16).

## 11. The review file

Decided:

- Local files, no server and no accounts.
- One review file per reviewer, holding all comments, commits and paths. This
  replaced the earlier idea of one sidecar per source file.
- Format: TOML. (KDL was acceptable to the owner; TOML was chosen for its
  mature serde support.)
- A review is shared by sending the file. Whoever has the file and the branch
  sees the comments inline.

Assumed:

- Several colleagues' files can be shown consolidated in one view. The owner
  raised this as a "maybe"; it is intended but not confirmed.

- Location: `.git/review/<branch>/<author>.toml`, inside the git directory so
  it cannot be committed by accident. A colleague's file dropped into the same
  folder is picked up.
- A reviewer only ever writes their own file. A reaction to someone else's
  comment is stored in the reactor's file and points at the comment's id. Files
  therefore never conflict.
- The header records the source, target and source tip. If the branch no
  longer contains that tip, the app warns that the branch has moved.
- Sending by hand is the first sharing route. Committing the file into the
  branch was rejected (pollutes history). A dedicated git ref pushed alongside
  is a possible later route.

The format is defined by `diffie-core/src/review.rs` and versioned
(`version = 1`).

## 12. Comments for AI agents

Decided: comments serve both as the reviewer's own notes and as input for an
AI agent. There is a "copy all as prompt" action and a per-comment one. The
prompt includes the code snippet.

## 13. Keyboard

Decided: Vim-style single keys, with a command palette. The mouse works for
everything.

Assumed key map (the mockup's hint bar):

| Key | Action |
| --- | --- |
| `j` `k` | line down, up |
| `]` `[` | next, previous change |
| `}` `{` | next, previous file |
| `1` `2` `3` | scope: New for me, Whole branch, Custom |
| `g` | open the commit picker |
| `(` `)` | step through the commits that touched this file |
| `r` | mark the file reviewed |
| `v` | extend a selection |
| `c` | comment |
| `e` | react |
| `n` `N` | next, previous comment |
| `o` | toggle outdated comments (other revisions') |
| `/` | search the changeset |
| `s` | split or unified |
| `za` `zM` `zR` | fold file, collapse all, expand all |
| `t` | file tree |
| Cmd/Ctrl+K | command palette |

Single-letter keys must never fire while the reviewer types in a comment box.

## 14. Per-file commit stepper

Assumed: each file header says how many of the scope's commits touched the
file and offers previous and next arrows to walk through only those commits,
with "all together" as the resting state. This came from combining the commit
drawer with per-file history.

Deferred (owner, 2026-11-11): the stepper control was removed from the
header as YAGNI. The touched-by count stays. Revisit when a per-file,
per-commit patch exists in the backend.

## 15. Themes and density

Decided:

- Theme support. The theme button is icon-only and uses a palette icon the
  owner supplied (in the mockup).
- Tight sizing. The owner approved the mockup's sizes: buttons 22 to 26 px
  high, tree rows 22 px, file headers 28 px, 12 px text.

Assumed: additions and deletions carry a `+` or `−` sign so they do not rely
on colour alone. Only a dark theme has been drawn.

## 16. Open questions

Ask the owner; do not decide these silently.

1. **Skipped commits: combined or stacked?** The scaffold builds a combined
   diff and stops a file at the first commit that cannot be replayed. Stacking
   each commit's patch always works but shows a twice-changed line twice.
2. **Anchoring comments in a combined view.** The combined tree exists nowhere
   in history, so blame cannot say which commit wrote a line. During design
   this was wrongly described as "works unchanged". The scaffold falls back to
   the newest checked commit and the displayed line number. A proper fix:
   build the replay as a chain of scratch commits and blame that.
3. **"Every difference" mode.** Expose it or not (section 4).
4. **Comments on unchanged context lines.** Blame attributes such a line to a
   commit older than the branch, so a fresh comment is immediately classified
   as "other revision". Probably the reviewer's own new comments should always
   show.
5. **Replies.** Are reactions enough for the first version, or are threaded
   answers needed?
6. **Who may resolve.** Anyone, or only the comment's author?
7. **Selections inside a line.** GPUI Kit's `Diff` reports selected line
   ranges, not columns. The review file can store the selected text; how the
   app captures and highlights it is undesigned.
8. **Line cursor.** `j` and `k` need a cursor; `Diff` has none. It can perhaps
   be emulated with a one-line selection.
9. **Unchanged reviewed files in the diff list.** They are listed in the file
   panel only; the mockup also shows them as collapsed headers in the diff.
10. **Theme picker.** The scaffold only toggles light and dark. GPUI Kit ships
    a theme registry.
11. **The app's name.** The crates and the binary are `diffie` and
    `diffie-core` (owner, 2026-11-11). The repository name `diff-review`
    remains a placeholder.
12. **Scale.** The commit picker is not virtualized (the file panel is).
    Fine for dozens, not for thousands.
