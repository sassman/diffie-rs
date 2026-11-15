# Handoff

State of the project on 2026-11-14, written for whoever continues it in a
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
  A review cleanup on 2026-11-14 hardened the git invocations and pulled
  the scratch-git and unified-diff plumbing into one place each
  (`ScratchGit`, `patch.rs`); see "The core in one page".
- **UI** (`crates/app`): restyled to the mockup on 2026-11-11: bundled Inter
  and JetBrains Mono, three themes (Gruvbox Dark is the default, Midnight is
  the mockup's palette, Light), segmented controls, file tree with a
  resizable panel, commit picker with search, skipped markers and badges,
  comment cards, the comment box with its attach-commit label, and the hint
  bar. See "What the UI does and does not do". The same cleanup fixed a
  lost-update race around the review file, moved search scans off the UI
  thread and added a status-bar notice for side-task failures.

## Verification status

Be exact about this when you report further work.

| Claim | Status |
| --- | --- |
| `cargo test -p diffie-core` passes | Verified on macOS on 2026-11-14, after the cleanup; earlier on Linux, git 2.43 |
| `cargo clippy --workspace --all-targets` is clean with `-D warnings` | Verified on macOS on 2026-11-14 |
| `cargo check -p diffie` compiles with no warnings | Verified on Linux, Rust 1.97, gpui-kit 0.9.1; again on macOS on 2026-11-14 |
| The window opens and renders | Verified on Linux under Xvfb with a software Vulkan driver (lavapipe). Screenshots in `docs/screenshots/` |
| Driven with simulated clicks and keys (xdotool), on a 6-commit demo repository: commit picker opens; unchecking a commit gives a combined diff with "1 skipped"; line selection; `c` opens the comment box; typing; Cmd/Ctrl+Enter saves; the comment is anchored by blame and reappears after a restart; reaction picker and chips; "Reviewed" by checkbox and by `r`, and after a restart the ticked file leaves the "New for me" diff; `s`; Whole branch; collapse all; light theme | Verified once each, by screenshot and by reading the review file |
| Everything else interactive: swap, expand all, next and previous file or change, other-revisions toggle with real data, copy as prompt, delete, Escape, scope keys, sidebar toggle, colleagues' files | Not exercised |
| A real GPU, a real window manager, high-DPI | Not run |
| Windows | Not built, not run |
| `--pr` against a real pull request | Window not run. The argument parsing and the pinned-revision resolve are unit-tested; the gh call's output shape and the merged-PR commit range were confirmed read-only against a real merged PR (srp6-rs #41: `baseRefOid..headRefOid` lists exactly the PR's 64 commits, the merge commit's first parent does not) |
| Performance on large branches | Not measured |
| 2026-11-11, macOS, real repository (16-commit branch): themed window renders with bundled fonts; tree, scope bar, segmented controls, progress bar, hint bar; split view with decorated added and removed rows, signs, word emphasis; a comment card and the header controls (counts, touched-by, Reviewed) | Verified by screenshot |
| Same session: commit picker internals (search, Space, shift-click, Show more), theme menu, comment box and Escape focus return, reactions, light and Midnight themes, narrow-window fallback, resizable panel drag | Compiled, not seen on screen |
| X11 window icon (embedded brand PNG decoded into `WindowOptions::icon`) | Compiled on macOS; not seen - no X11 session on this machine |
| macOS Dock icon (runtime `setApplicationIconImage:` from the embedded 1024 px PNG) | Verified on macOS by screenshot: the mark sits in the Dock at launch |
| Application menu bar | Verified on macOS by screenshot: the bar reads diffie, Edit, View, Go, Window; the app menu shows About diffie, Services and Quit diffie with Cmd+Q; View and Go hold the declared items with no shortcut hints; About opens the standard panel with the brand mark and Version 0.1.0; "Collapse all files" from the menu collapsed every file (driven by System Events clicks) |
| Menu items while typing in a comment box | Not exercised interactively. No menu item carries a bare-letter key equivalent (the wrapper actions are unbound), so the NSMenu has no letter to match, but typing with the box open was not driven |
| Edit menu's native Undo/Cut/Copy/Paste on a focused input, Linux and Windows (bar stores but draws nothing) | Not exercised |
| 2026-11-14 cleanup on screen: the review-save ordering and reload grafting, the stale-blame guard on a draft's attach commit, the status-bar notice, the split scope-bar rendering | Compiled, clippy-clean, core tests pass; not seen on screen - the window was not run |
| Removed-file header: danger-coloured name, D badge tooltip with the deleting commit (newest in-scope commit touching the path; plain fallback for a worktree deletion) | Compiled, clippy-clean, core tests pass; not seen on screen - the window was not run |
| Reload semantics: pinned CLI target across Cmd/Ctrl+R, new commits extending an all-picked custom scope, View > Reload menu item | Compiled, clippy-clean; the one-revision CLI reading (`diffie HEAD~1` = `git diff HEAD~1`) is covered by a core test (`one_revision_reads_like_git_diff`); the reload flow itself not seen on screen - the window was not run |
| Navigation focus marker and viewport re-anchoring for `]`/`[`/`n`/`p` | Marking, stepping from a visible mark and viewport re-anchoring are covered by a Kit integration test (`change_keys_mark_the_reached_group_and_step_from_a_visible_mark`); the comment-jump mirror compiles and is clippy-clean; not seen on screen - the window was not run |

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
stands in for `x`, `remote_ref` in `main.rs`), several candidates are
listed and refused. A window that cannot open prints the error and exits
instead of panicking.
`diffie --pr <number|url>` reviews a GitHub pull request: gh (which must
be on PATH) describes it, and the review pins the PR's exact commits -
the head commit as source, and as target the base branch's
remote-tracking ref while the PR is open or the PR's recorded base
commit once merged - never a same-named local branch, so the review
matches what the PR holds. The title and the review file keep the branch
names. A commit that is not here is refused with the `git fetch` to run
(the head branch while open, `refs/pull/<n>/head` once merged or closed,
the fork named for a cross-repository head) - diffie never fetches.
`diffie --prompt [<review>]` opens no window: it prints the open review
comments the way "Copy all as prompt" does (no name works only for a sole
review, several are listed and refused; `--all` includes resolved ones,
marked), each headed by its short id. `diffie --reply <id> <answer>`
threads an answer under that comment, written into the agent's own
reviewer file ("Claude"), found by unique id prefix across all reviews -
ambiguous or unknown prefixes are refused, never guessed.
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
  patch.rs                    crate-private unified-diff walker; search,
                              moved and region parsing all ride on it
  search.rs                   smartcase search over patch text
  moved.rs                    pairs verbatim-moved runs for the diff folds
  review.rs                   review file: comments, reactions, ticks, ReviewSet
  pr.rs                       --pr: a pull request's state, branches and
                              exact commits via the gh CLI
  time.rs                     RFC 3339 without a date-time crate
crates/core/tests/git_cli.rs  backend tests against a throwaway repository
crates/app/src/
  main.rs                     arguments, window
  app.rs                      ReviewApp: all state, loading, rendering
assets/                       brand mark (logo.svg) and app icon (icon.svg)
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
  file counts as unchanged, and so does a ticked file whose only change is
  whitespace while hide-whitespace is on - it collapses instead of
  surfacing an empty Changed entry.

Two invariants are centralized in `git_cli.rs`. Revisions pass behind
`--end-of-options` everywhere, except `git blame`, which acts on
option-shaped revisions even behind it (probed on git 2.54) - `blame_with`
refuses any revision starting with `-` instead. And all scratch state (the
temporary index, object directory and alternates that keep the reviewed
repository untouched) lives in `ScratchGit`, shared by `worktree_run` and
`combine`.

Patches carry 400 lines of context (`CONTEXT_LINES`), because the Diff view
can only expand context that the patch supplies.

`review.rs` holds the review file model and its rules: a reviewer writes only
their own file, reactions point at comment ids, `ReviewSet` overlays
colleagues' files read-only, `place()` positions a comment from a blame, and
`as_prompt()` renders comments for an AI agent. The TOML format is version 1.

`pr.rs` is command-line plumbing, not part of `ReviewBackend`: it parses the
`--pr` value (a number, or a pull-request URL kept verbatim so gh keeps the
host and repository) and shells out to `gh pr view` for the PR's state,
branch names and exact commits. `main.rs` turns that into a comparison whose
`source_rev`/`target_rev` pin the PR's commits while `source`/`target` keep
the branch names for the title and the review file; `GitCli::resolve`
resolves the pinned revision when one is set.

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
  here - worth raising against the Kit or working around with a periodic
  foreground timer while a load is in flight.

Other Kit facts used here:

- `gpui_kit::application()`, `gpui_kit::init(cx)`, `gpui_kit::open_window(..)`.
- The component icon set (`component::IconName`) is a subset. The full Lucide
  catalogue is `gpui_kit::assets::IconName`, used through `Icon::new(..)`.
- `Diff`'s key context is `"Diff"`, and the input's is `"Input"`.
- Themes: three files under `crates/app/themes/` are embedded and loaded in
  `app::init` - `gruvbox-dark.json` (the default dark), `midnight.json` (the
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
  bubble, "Earlier comments - from other revisions" on hover; it also
  hides the changed-since cards, which are earlier comments too),
  copy all as prompt, a theme menu behind the palette button.
- Scope bar: segmented scopes; the commit picker with search, group
  checkboxes, skipped markers, comment badges, Show-more rows, shift-click
  runs and Space on the hovered row; in an uncommitted review an
  "Uncommitted changes" row leads "New since your review" so the working
  tree can be excluded like a commit (files carrying its changes say
  "uncommitted" in the diff header, warning-coloured); the open-comments count in the
  comment accent ("comments handled" once every thread is resolved) -
  clickable: it walks to the next open comment like `n`;
  the files-reviewed count with a progress bar. `n`/`p` walk the
  comments, resolved included, in reading order, stop at the ends instead of circling, and
  reveal a collapsed file on landing; when
  every stop is hidden they clear the blocker instead of dying silently -
  the earlier-comments toggle flips on, or the scope widens to Whole
  branch and lands there after the reload (the search out-of-view
  pattern). In New for me, a ticked-unchanged file with open comments
  keeps its branch diff (`FileState::KeptForComments`, note "no new
  changes - shown for its open comments") so the comments stay visible
  and judgeable; resolving them lets it collapse again.
- Reload: Cmd/Ctrl+R (also View > Reload) re-resolves the comparison and
  rebuilds the diff, so a review stays open while work continues. The
  CLI-given target is pinned to its startup commit in `main.rs` (a
  `HEAD~1` or branch target re-resolved on reload would move the base and
  swallow the commits the reload should add); the head side re-resolves,
  so commits made since appear. A custom scope with every commit picked
  re-picks the fresh list - new commits join the review, and an unticked
  uncommitted layer stays unticked; a hand-picked subset is left exactly
  as picked (`extend_picks` in `reload`/`load`).
- Search: `/` opens a bar under the scope bar, seeded from the text
  selection; smartcase matches over the scoped patch text
  (`diffie_core::search::search_patch`, scanned on the background executor
  with a stale-result guard, not per keystroke on the UI thread),
  enter/shift-enter walk them,
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
  behind an info icon's hover, comment summary that jumps to the first
  comment, state notes, Reviewed; a removed file leads with the tree's D
  and shows its name in the danger colour - the D's hover names the
  deleting commit's short hash, or says only "This file got removed" for
  a worktree deletion), the current-file bar above,
  generated, unchanged-reviewed and removed files as collapsed headers, `Simple` hunk bands,
  auto-unified when the window is too narrow with the manual choice kept.
- Comments: cards in the mockup's style with hash and line label, reaction
  chips and the smile-plus picker, Edit, per-comment Copy as prompt, Delete;
  other-revision comments dashed and quiet. A comment whose code was
  rewritten since follows the rewrite (blame misses, `changed_regions` +
  `map_line` carry it to the replacement) and sits there dashed in the
  warning tint, tagged "code changed", header "was line n", the stored
  snippet quoted above the body, actions kept so the reviewer can react
  Done to approve the fix; file headers count these as "on changed code"
  and the status bar names open comments nothing can show ("n not in this
  comparison"). Replies thread under the comment body (author plus
  markdown, left-bordered). Resolving changes nothing but the reaction:
  question, answers and respond box all stay, the thread is the record.
  A thread with answers carries a quiet "Respond…" stand-in that a click
  turns into a textarea (Cmd/Ctrl+Enter sends the reply into the
  reviewer's own file, Escape closes), and every open thread's actions
  row ends in a Resolve button that applies the Done reaction; an open
  comment answered by someone else wears an "answered" tag in the success
  tint unless "code changed" already covers it. The add-comment + sits in the
  gutter-marker lane. The comment box names the commit it will attach to
  before saving - the blame that names it is dropped when the draft or
  comparison changed underneath it (`resolve_draft_commit`) - and Comment/Suggest change (disabled)/Cancel carry their
  shortcut hints. Right-clicking in the diff opens a context menu with Copy
  and Comment acting on the text selection (both disabled without one); the
  items use the payload snapshotted at menu-open, because picking an item
  clears the live selection first. The menu and `c` are the only
  selection-comment entry points; there is no floating button.
- Ticking Reviewed (or `r`) collapses the file and moves the current-file
  marker to the next one without scrolling; the collapse pulls it up.
- Status bar: side-task failures (saving the review, the search's
  whole-branch fetch, blame for a comment) surface as a `notice` there and
  clear on the next success; the full-screen `error` page is reserved for
  load failures.
- Shortcuts: `]` `[` `}` `{` `s` `o` `t` `n` `p` `1` `2` `3` `zM` `zR` `c`
  `r`, Escape, Cmd/Ctrl+Enter, Cmd/Ctrl+R, Cmd/Ctrl+Q, Space in the commit
  list.
- Navigation focus marker: `]`/`[` and `n`/`p` mark where they land (the
  Kit fork's focus marker - a slim primary edge bar with a faint wash -
  via `DiffState::set_focused_ranges`; the change keys set it inside the
  Kit). A visible mark is the anchor the keys step from; once the reader
  scrolls it off the page the walk re-anchors at the viewport, so the
  keys guide within the page being looked at instead of yanking back.
  Landings center the mark in the viewport (the Kit's `center_block`: a
  group taller than the viewport starts a header's height below the top,
  and the list clamps at the document's ends); `n`/`p` ride on the Kit's
  `scroll_to_line`, so comment jumps center the same way. One marker
  serves both steppers - a comment jump moves it off the last change
  group and vice versa. While the mark is still the line a comment jump
  placed, that comment's card carries a prominent 2px primary border
  (`ThreadView::active`); a change-key press replaces the mark and the
  card falls back in line.
- Application menu bar (`install_menus` in `app.rs`, set at init so it sees
  the keymap): app menu with About diffie (the standard AppKit panel with
  name and version, since the dev binary has no bundle), Services and Quit;
  Edit with Undo through Select All as OS actions driving the native
  selectors on the focused input; View and Go mirroring the toggles,
  navigation and scopes; an empty "Window" menu macOS fills with the
  standard items. Only macOS draws the bar - Linux and Windows store the
  declaration and show nothing. Every single-letter shortcut sits behind a
  menu-only unbound action (`MenuCollapseAll`, ...) handled by the root:
  a bare-letter NSMenuItem key equivalent would swallow the key while the
  reviewer types a comment, so those items show no shortcut hint.
- Window icon: on X11 the brand mark (`assets/icon-256.png`, embedded and
  decoded at startup) goes into `WindowOptions::icon`, which only that
  backend reads (`_NET_WM_ICON`). Wayland takes its icon from a .desktop
  entry matched by the `diff-review` app id (no such entry ships yet). On
  macOS the dev binary sets the Dock icon at runtime
  (`setApplicationIconImage:` with the embedded `assets/icon-1024.png`),
  since GPUI never touches it and there is no bundle to carry an .icns.

Not implemented or not possible, against the mockup and the design document:

| Missing | Notes |
| --- | --- |
| Sticky file headers | The Diff cannot pin a list row (the owner dropped the stand-in current-file bar as noise) |
| Per-file cards, per-status gutter cell colours, selected-line left edge, custom fold-row and separator styling, the mockup's exact split hairline | Hard-coded in the Kit's Diff |
| Per-file horizontal scrolling | Unified scrolls the whole list as one canvas (see the trap above). Scrolling is the default since 2026-11-11, with a wrap toggle in the title bar |
| Per-file commit stepper | Removed as YAGNI on 2026-11-11 (it only ever rendered disabled); design document section 14 holds the idea, `commits_touching` still feeds the touched-by text |
| Suggestions | "Suggest change" renders disabled; no flow behind it |
| `j` `k` line cursor, `v`, `g`, `(` `)`, `e`, `za`, command palette | Design question 8 for the cursor |
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
   panel resizing - and the cleanup's unseen pieces: the status-bar notice
   and the save ordering under rapid edits.
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
- **The review file has one writer.** `save_review` is a dirty flag drained
  by a single writer task, so writes land in mutation order, and
  `review_generation` stamps the reviewer's own file: when a mutation raced
  a background reload, `install` grafts the live `mine` (and carries
  `placed` forward) instead of letting the stale reload clobber it. Do not
  add a second path that writes the file.
