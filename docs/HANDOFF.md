# Handoff

State of the project on 2026-11-11, written for whoever continues it in a
coding session. Read `AGENTS.md` first for the rules, and
`docs/design-decisions.md` for product behaviour.

## Where things stand

The product was designed in conversation, UI first, through a mockup that the
owner iterated on until the main screen was approved. This repository is the
first code: a working core and a first UI scaffold wired to it.

- **Design**: main screen settled. See `docs/design-decisions.md`, especially
  the open questions in section 16.
- **Mockup**: `docs/mockup/main-screen.html`. Open it in a browser. It is a
  static picture with sample data and it is the visual target.
- **Core** (`crates/core`): done for the scaffold's needs and tested. Besides
  the scaffold APIs it now has `changed_paths` and `commits_touching` on the
  trait, and `ReviewFile::verify_completed` for stale completion markers.
- **UI** (`crates/app`): restyled to the mockup on 2026-11-11: bundled Inter
  and JetBrains Mono, three themes (Gruvbox Dark is the default, Midnight is
  the mockup's palette, Light), segmented controls, file tree with a
  resizable panel, commit picker with search, skipped markers and badges,
  comment cards, the comment box with its attach-commit label, and the hint
  bar. See "What the UI does and does not do".

## Verification status

Be exact about this when you report further work.

| Claim | Status |
| --- | --- |
| `cargo test -p diffie-core`: 24 tests pass | Verified on Linux, git 2.43 |
| `cargo clippy --workspace --all-targets` is clean | Verified |
| `cargo check -p diffie` compiles with no warnings | Verified on Linux, Rust 1.97, gpui-kit 0.9.1 |
| The window opens and renders | Verified on Linux under Xvfb with a software Vulkan driver (lavapipe). Screenshots in `docs/screenshots/` |
| Driven with simulated clicks and keys (xdotool), on a 6-commit demo repository: commit picker opens; unchecking a commit gives a combined diff with "1 skipped"; line selection; `c` opens the comment box; typing; Cmd/Ctrl+Enter saves; the comment is anchored by blame and reappears after a restart; reaction picker and chips; "Reviewed" by checkbox and by `r`, and after a restart the ticked file leaves the "New for me" diff; `s`; Whole branch; collapse all; light theme | Verified once each, by screenshot and by reading the review file |
| Everything else interactive: swap, expand all, next and previous file or change, other-revisions toggle with real data, copy as prompt, delete, Escape, scope keys, sidebar toggle, colleagues' files | Not exercised |
| A real GPU, a real window manager, high-DPI | Not run |
| Windows | Not built, not run |
| Performance on large branches | Not measured |
| 2026-11-11, macOS, real repository (16-commit branch): themed window renders with bundled fonts; tree, scope bar, segmented controls, progress bar, hint bar; split view with decorated added and removed rows, signs, word emphasis; a comment card and the header controls (counts, touched-by, Reviewed) | Verified by screenshot |
| Same session: commit picker internals (search, Space, shift-click, Show more), theme menu, comment box and Escape focus return, reactions, light and Midnight themes, narrow-window fallback, resizable panel drag | Compiled, not seen on screen |

## Build, run, test

```sh
cargo test -p diffie-core
cargo check -p diffie      # first run compiles GPUI: about 8 minutes on 2 cores
                               # (a full debug build: about 13)
cargo run -p diffie -- -C /path/to/repository main
```

Arguments read in the merge direction, source first: `-C <path>` picks the
project, positional revisions pick the comparison (none: uncommitted into
the checked-out branch; one: the working tree into it; two or `A..B`: the
first into the second; `--every-difference` widens the comparison). A
revision that does not resolve fails before the window
opens; a branch that exactly one remote has is taken from there (`o/x`
stands in for `x`), several candidates are listed and refused.
`diffie --prompt [<review>]` opens no window: it prints the open review
comments the way "Copy all as prompt" does (no name works only for a sole
review, several are listed and refused; `--all` includes resolved ones,
marked).
With no revision at all, the review is the working tree's
uncommitted changes going into the checked-out branch
(`Comparison::uncommitted`): the diff runs against a scratch index seeded
with intent-to-add entries so untracked files show, ticks hash the file on
disk (`worktree_blob_ids`), and the patch's new side is `Synthetic`, so
comments cannot blame an exact commit there.

Linux packages are listed in `README.md`.

## Repository layout

```text
Cargo.toml                    workspace; pins gpui-kit exactly
AGENTS.md                     rules for agents
crates/core/src/
  model.rs                    Comparison, Scope, ScopedPatch, FileSummary, ...
  backend.rs                  ReviewBackend trait, BackendError
  git_cli.rs                  GitCli: the cheap backend (shells out to git)
  review.rs                   review file: comments, reactions, ticks, ReviewSet
  time.rs                     RFC 3339 without a date-time crate
crates/core/tests/git_cli.rs  backend tests against a throwaway repository
crates/app/src/
  main.rs                     arguments, window
  app.rs                      ReviewApp: all state, loading, rendering
docs/
  design-decisions.md         product behaviour, with Decided/Assumed/Open
  mockup/main-screen.html     visual reference
  screenshots/                the scaffold as it actually rendered
skills/diffie-review/         agent skill: launch a review, read it back
                              (canonical copy; installs to ~/.claude/skills)
```

## The core in one page

`ReviewBackend` (in `backend.rs`) is the whole contract:

- `resolve(Comparison) -> Resolved`: branch names to commits, plus where the
  diff starts.
- `commits(&Resolved)`: the comparison's commits, newest first.
- `patch(&Resolved, &Scope, &[ReviewedFile]) -> ScopedPatch`: git-format patch
  text for a scope, with a summary per file.
- `line_origin`, `blame`: which commit wrote a line. Comments are anchored
  with this.
- `blob_id`: a file's content id. "Reviewed" ticks store it; a deleted file
  ticks with no blob (the deletion is the reviewed content, and a re-added
  file reads as changed since).
- `blob_ids`: every file's content id at a revision in one `ls-tree -r`.
  Loads use it instead of per-file `blob_id`: a commit changing thousands of
  files costs one subprocess per file otherwise, which read as a stalled
  "Reading the repository". The file panel is a virtualized `uniform_list`
  for the same reason: an element per row each frame froze large reviews.
- `author_name`, `review_dir`.

How `GitCli` produces each scope:

- **Whole branch**: `git diff --histogram base head`.
- **Custom, contiguous**: `git diff` from the oldest commit's parent to the
  newest.
- **Custom, with skipped commits**: replays only the checked commits, file by
  file, with `git apply --cached --3way` into a temporary index and a
  temporary object directory that uses the real one as an alternate. Nothing
  is written to the repository (a test counts loose objects to prove it). A
  file that cannot be replayed stops before the failing commit and is reported
  as `FileState::CouldNotCombine`.
- **Custom, in an uncommitted review**: `Scope::Commits` carries an
  `uncommitted` flag; the working tree is one more pickable layer. All checked
  through head: one worktree diff. Otherwise the worktree-against-head patch
  is replayed on top of the combined tree; a file it cannot sit on is
  `FileState::UncommittedNotCombined`. `uncommitted_paths` on the trait lists
  the files the layer touches.
- **New for me**: per file. Ticked and unchanged: listed, no diff. Ticked and
  changed: `git diff <ticked blob> <current blob>`, relabelled with the path.
  Otherwise the file's whole-branch diff. A blobless tick on a still-deleted
  file counts as unchanged.

Patches carry 400 lines of context (`CONTEXT_LINES`), because the Diff view
can only expand context that the patch supplies.

`review.rs` holds the review file model and its rules: a reviewer writes only
their own file, reactions point at comment ids, `ReviewSet` overlays
colleagues' files read-only, `place()` positions a comment from a blame, and
`as_prompt()` renders comments for an AI agent. The TOML format is version 1.

## GPUI Kit: what to know

The single most useful fact: **GPUI Kit ships a `Diff` component that already
does most of what the mockup's diff area needs.** It was discovered after the
design was finished, so the design document does not mention it.

`gpui_kit::component::diff` provides:

- one virtualized list for all files, split and unified modes;
- inline word emphasis, syntax highlighting, foldable context with expansion;
- line and range selection, `selected_lines()`, `selected_text()`;
- **annotations**: application content under any line or under a file header
  (`annotations`, `render_annotation`), and `on_add_annotation`, which adds an
  add-comment button beside the hovered line and at the end of a selection;
- file header slots (`render_header_prefix`, `render_header_title_suffix`,
  `render_header_suffix`, `render_header`);
- collapse per file (`set_file_collapsed`, `is_file_collapsed`), and
  `scroll_to_file`, `scroll_to_line`, `next_change`, `previous_change`;
- decorations (line backgrounds) and gutter markers;
- it parses git patch text itself: `DiffFile::parse(patch)`.

What it does not give:

- no column positions for a selection inside a line (design question 7);
- no line cursor (design question 8);
- it does not compute diffs. That stays in the core;
- render callbacks receive an `App`, not the owning view. Pass them a snapshot
  and a `WeakEntity`, and never read or update the same `DiffState` inside
  them. `app.rs` shows the pattern (`Snapshot`).

Where to read more:

- Source and docs: <https://github.com/longbridge/gpui-kit>. The Diff page is
  `website/component/diff.md`.
- `examples/tig` in that repository is a complete git history browser built on
  `Diff`: background loading, stale-result guards, keyboard contexts. `app.rs`
  here follows its structure.
- `crates/story/src/stories/diff_story.rs` shows annotations with comments.
- The repository also carries agent skills under `skills/gpui-kit`.

Things that cost time and are easy to hit again:

- **Bindings on an ancestor context fire while typing in a descendant
  input.** The comment box is rendered inside the Diff, so letters bound in
  the `Diff` context fired on every keystroke typed into a comment. The root
  now adds a `Reading` context only while no draft is open, and letters are
  bound to `Reading > Diff`.
- **The input consumes Cmd/Ctrl+Enter and Escape itself.** A window-level
  binding never sees them. Subscribe to the input's `InputEvent::PressEnter
  { secondary: true, .. }` instead (see `start_comment`).
- **Focusing a new input works from a click but not from a key handler**
  unless it is deferred by a frame (`cx.on_next_frame`).
- **Without `soft_wrap(true)` one long line widens the unified diff list.**
  Unified scrolls the entire list as one canvas (wrap toggle beside Split
  and Unified, `wrap` on `ReviewApp`). Split is two half-viewport panels
  that scroll their code separately - the fork's
  `diff: Scroll each split panel on its own` carries that behavior (branch
  `split-panel-scroll`); gutters stay put, only the code shifts. With wrap
  on, both modes wrap instead of scrolling.
- **The default `Assets` embeds only the Kit's own 101 icons.** Icons from
  the full catalogue silently render as nothing. The app registers
  `AllAssets` (about 1 MiB) for now; a small application asset source with
  just the needed icons is the tidier fix. The palette icon is
  `assets::IconName::Palette` and smile-plus is `FaceSlightlySmilingPlus`
  (Lucide renamed it); both are used.
- **The Kit's add-annotation button covers the first characters of code**
  (its position is hard-coded). The app instead tracks `on_line_hover` and
  draws its own + in the gutter-marker lane; a sentinel marker with an
  impossible path keeps the lane reserved so hovering never shifts layout.
- **An unfocused window never finishes loading.** Launched behind another
  window, the app sits at "Reading the repository" indefinitely: every git
  call is milliseconds, every thread idles, and the loaded result is waiting
  for a wake-up that only arrives with the first event (focus, click, key).
  The first paint of the diff list needs one more event again. It looks like
  macOS App Nap / a lost wake in the GPUI dispatcher while occluded, so a
  focused app behaves perfectly. Diagnosed with `/usr/bin/sample`; not fixed
  here — worth raising against the Kit or working around with a periodic
  foreground timer while a load is in flight.

Other Kit facts used here:

- `gpui_kit::application()`, `gpui_kit::init(cx)`, `gpui_kit::open_window(..)`.
- The component icon set (`component::IconName`) is a subset. The full Lucide
  catalogue is `gpui_kit::assets::IconName`, used through `Icon::new(..)`.
- `Diff`'s key context is `"Diff"`, and the input's is `"Input"`.
- Themes: three files under `crates/app/themes/` are embedded and loaded in
  `app::init` — `gruvbox-dark.json` (the default dark), `midnight.json` (the
  mockup's palette) and `light.json` (Kit default colors plus the fonts).
  The palette button opens a menu; `apply_theme` runs
  `Theme::update(.. apply_config ..)`. The comment orange rides on the
  `chart.4` token (`comment_accent`), and the added and removed row fills
  are per-theme in `diff_backgrounds`, painted as decorations because the
  Diff's own tint is a fixed 12% wash.
- Inter and JetBrains Mono (OFL, `crates/app/assets/fonts/`) are embedded in
  `main.rs` via `add_fonts` before `gpui_kit::init`; the theme files name
  them. `mono_font.size` is 12.5 because the Diff's row height is a fixed
  1.6 × font size: 12.5 gives the mockup's 20px rows.
- Syntax highlighting uses the lighter `syntect` feature. Per-language
  `tree-sitter-*` features exist if higher fidelity is wanted.

## What the UI does and does not do

Implemented in `app.rs` (compiled; see the verification table for what was
actually exercised):

- Title bar: repository, "changes in X going into Y" with chips, swap,
  split/unified as a segmented icon pair (columns and rows, named on
  hover), wrap and hide-whitespace as icon
  toggles (the pilcrow reloads with `--ignore-all-space`, so whitespace-only
  files leave the view), collapse/expand all as a joined icon pair,
  earlier comments (other revisions') as an icon toggle (dashed comment
  bubble, "Earlier comments - from other revisions" on hover),
  copy all as prompt, a theme menu behind the palette button.
- Scope bar: segmented scopes; the commit picker with search, group
  checkboxes, skipped markers, comment badges, Show-more rows, shift-click
  runs and Space on the hovered row; in an uncommitted review an
  "Uncommitted changes" row leads "New since your review" so the working
  tree can be excluded like a commit (files carrying its changes say
  "uncommitted" in the diff header, warning-coloured); the open-comments count in the
  comment accent ("comments handled" once every thread is resolved);
  the files-reviewed count with a progress bar. `n`/`N` walk the open
  comments in reading order and reveal a collapsed file on landing.
- Search: `/` opens a bar under the scope bar, seeded from the text
  selection; smartcase matches over the scoped patch text
  (`diffie_core::search::search_patch`), enter/shift-enter walk them,
  matched lines carry an amber fill and the matched characters a louder
  saturated-amber emphasis with dark text (the Kit fork's
  `Diff::span_highlights`; hits carry their byte spans). Hits land on
  removed lines too, and
  a jump reveals the moved-code fold hiding its target, so removed-and-
  readded code is findable. "All commits" fetches the
  whole-branch patch in the background and counts both ("0 in view · 1
  across all commits"); stepping onto an out-of-view hit switches the
  scope to Whole branch and lands there after the reload.
- File panel: a tree with folder rows, coloured status letters, split +/−
  counts, comment badges, reviewed ticks, "generated"; truncation that keeps
  badges inside the panel; resizable by dragging its edge; Hide in its
  header, and a "Files" button in the status bar while hidden. Folder rows
  fold their subtree, and the diff follows: a shut folder collapses its
  files in the main view, reopening frees only files not under another
  shut folder.
- Diff: per-theme added and removed row fills (decorations), signs, a
  take-over header (path with a copy button, coloured counts, touched-by
  text, comment summary that jumps to the first comment,
  state notes, Reviewed), the current-file bar above, generated and
  unchanged-reviewed files as collapsed headers, `Simple` hunk bands,
  auto-unified when the window is too narrow with the manual choice kept.
- Comments: cards in the mockup's style with hash and line label, reaction
  chips and the smile-plus picker, Edit, per-comment Copy as prompt, Delete;
  other-revision comments dashed and quiet. The add-comment + sits in the
  gutter-marker lane. The comment box names the commit it will attach to
  before saving, and Comment/Suggest change (disabled)/Cancel carry their
  shortcut hints. Right-clicking in the diff opens a context menu with Copy
  and Comment acting on the text selection (both disabled without one); the
  items use the payload snapshotted at menu-open, because picking an item
  clears the live selection first. The menu and `c` are the only
  selection-comment entry points; there is no floating button.
- Ticking Reviewed (or `r`) collapses the file and moves the current-file
  marker to the next one without scrolling; the collapse pulls it up.
- Shortcuts: `]` `[` `}` `{` `s` `o` `t` `1` `2` `3` `zM` `zR` `c` `r`, Escape,
  Cmd/Ctrl+Enter, Cmd/Ctrl+R, Cmd/Ctrl+Q, Space in the commit list.

Not implemented or not possible, against the mockup and the design document:

| Missing | Notes |
| --- | --- |
| Sticky file headers | The Diff cannot pin a list row (the owner dropped the stand-in current-file bar as noise) |
| Per-file cards, per-status gutter cell colours, selected-line left edge, custom fold-row and separator styling, the mockup's exact split hairline | Hard-coded in the Kit's Diff |
| Per-file horizontal scrolling | Unified scrolls the whole list as one canvas (see the trap above). Scrolling is the default since 2026-11-11, with a wrap toggle in the title bar |
| Per-file commit stepper | Removed as YAGNI on 2026-11-11 (it only ever rendered disabled); design document section 14 holds the idea, `commits_touching` still feeds the touched-by text |
| Suggestions | "Suggest change" renders disabled; no flow behind it |
| `j` `k` line cursor, `v`, `g`, `(` `)`, `e`, `n` `N`, `za`, command palette | Design question 8 for the cursor |
| Selections inside a line | Design question 7 |
| "Branch has moved since this review" warning | Header data is stored |
| Importing a colleague's file from the UI | Works by dropping the file in the folder |
| Persisted window and UI settings | Layout choice and panel width live only in the session |
| "File N of M · change x of y" following the viewport | The Diff exposes no viewport position; the file part follows the current file, the change part is absent |

## Suggested next steps

In this order, each one small enough to show the owner:

1. **Exercise what the restyle has not shown on screen yet**: the commit
   picker internals, theme menu, comment box and Escape focus return,
   reactions, the Light and Midnight themes, the narrow-window fallback,
   panel resizing.
2. **Chase the slow load** on a real branch (see the trap list): likely the
   row building and highlighting of 400-context-line patches.
3. **Ask the owner the open questions** in section 16 of the design document,
   starting with 1, 2, 4 and 5. They change the data model.
4. **Keyboard model**: line cursor and the remaining keys.
5. Selections inside a line, suggestions.
6. Only when the UI has settled: the `gix` and `imara-diff` backend.

## Traps

- **The Kit currently comes from the owner's fork.** `[patch.crates-io]`
  in the workspace manifest points every Kit crate at `sassman/gpui-kit`;
  `Cargo.toml` names the current branch, each branch stacking on the last:
  `bulk-file-collapse` (`set_files_collapsed`, one rebuild for any number
  of files), `split-panel-scroll` (per-panel horizontal scrolling),
  `pinned-file-headers`, and `folded-ranges` (`DiffFold` +
  `DiffState::set_folds`, the labeled click-to-reveal rows the moved-code
  feature renders). Drop the patch when upstream ships them.
- **Moved-code folds.** `diffie_core::moved::moved_blocks(&patch.text)`
  pairs whole verbatim-moved runs - a complete removal run whose full
  content reappears as a complete addition run (exact match, at least 20
  alphanumeric characters); partial overlaps are edits, not moves.
  `install()` turns each block into two `DiffFold`s labeled
  "moved to/from path:span". A window fully covered by an always-on-top
  window stops painting (GPUI skips occluded windows), which mimics a
  load hang during screenshot verification - expose a sliver first.
- **gpui-kit is pinned with `=`.** Bumping it can change GPUI's API. Do it
  deliberately, alone, and re-check the whole UI.
- **Comment line numbers.** A stored comment holds the line number in the
  version of the file at the commit that wrote the line. The number on screen
  comes from blame at load time (`place_all`). Do not mix the two.
- **Combined views have no blame.** With skipped commits the new side is a
  synthetic tree (`NewSide::Synthetic`). Comments made there are anchored
  approximately, and existing comments are placed at their stored line number
  (design question 2). This is visibly wrong today: in
  `docs/screenshots/scaffold-commit-picker-skipped.png` the comment sits under
  line 28, one line above the line it was written on.
- **Fresh comments on context lines** are classified as "other revision" and
  are hidden while that toggle is off (design question 4). This will look like
  a lost comment.
- **`set_files` resets the Diff's scroll and selection.** Any reload jumps to
  the top. Changing scope reloads.
- **`HEAD` as a source.** `main.rs` swaps `HEAD` for the branch name so the
  review file is named after the branch. A detached HEAD stays `HEAD`.
- **Review file identity is the author's `user.name`.** Two reviewers with
  the same name collide.
