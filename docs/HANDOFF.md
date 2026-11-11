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
- **Core** (`crates/core`): done for the scaffold's needs and tested.
- **UI** (`crates/app`): a first scaffold. It runs and is wired to the core,
  but it is well short of the mockup. See "What the UI does and does not do".

## Verification status

Be exact about this when you report further work.

| Claim | Status |
| --- | --- |
| `cargo test -p review-core`: 24 tests pass | Verified on Linux, git 2.43 |
| `cargo clippy --workspace --all-targets` is clean | Verified |
| `cargo check -p review-app` compiles with no warnings | Verified on Linux, Rust 1.97, gpui-kit 0.9.1 |
| The window opens and renders | Verified on Linux under Xvfb with a software Vulkan driver (lavapipe). Screenshots in `docs/screenshots/` |
| Driven with simulated clicks and keys (xdotool), on a 6-commit demo repository: commit picker opens; unchecking a commit gives a combined diff with "1 skipped"; line selection; `c` opens the comment box; typing; Cmd/Ctrl+Enter saves; the comment is anchored by blame and reappears after a restart; reaction picker and chips; "Reviewed" by checkbox and by `r`, and after a restart the ticked file leaves the "New for me" diff; `s`; Whole branch; collapse all; light theme | Verified once each, by screenshot and by reading the review file |
| Everything else interactive: swap, expand all, next and previous file or change, other-revisions toggle with real data, copy as prompt, delete, Escape, scope keys, sidebar toggle, colleagues' files | Not exercised |
| A real GPU, a real window manager, high-DPI | Not run |
| macOS and Windows | Not built, not run |
| Performance on large branches | Not measured |

## Build, run, test

```sh
cargo test -p review-core
cargo check -p review-app      # first run compiles GPUI: about 8 minutes on 2 cores
                               # (a full debug build: about 13)
cargo run -p review-app -- --target main /path/to/repository
```

Arguments: `--source REV` (default: the checked-out branch), `--target REV`
(default `main`), `--every-difference`, then the repository path.

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
- `blob_id`: a file's content id. "Reviewed" ticks store it.
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
- **New for me**: per file. Ticked and unchanged: listed, no diff. Ticked and
  changed: `git diff <ticked blob> <current blob>`, relabelled with the path.
  Otherwise the file's whole-branch diff.

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
- **Without `soft_wrap(true)` one long line widens the whole diff list** and
  pushes each file header's right-hand controls out of view.
- **The default `Assets` embeds only the Kit's own 101 icons.** Icons from
  the full catalogue silently render as nothing. The app registers
  `AllAssets` (about 1 MiB) for now; a small application asset source with
  just the needed icons is the tidier fix, and the place to add the owner's
  palette icon and a smile-plus icon.

Other Kit facts used here:

- `gpui_kit::application()`, `gpui_kit::init(cx)`, `gpui_kit::open_window(..)`.
- The component icon set (`component::IconName`) is a subset. The full Lucide
  catalogue is `gpui_kit::assets::IconName`, used through `Icon::new(..)`.
  There is no smile-plus icon; the add-reaction button uses a plus for now.
- `Diff`'s key context is `"Diff"`, and the input's is `"Input"`.
- Themes: `Theme::change(ThemeMode::Dark, Some(window), cx)`. The Kit also
  has a theme registry and ships theme files (`themes/` in its repository).
- Syntax highlighting uses the lighter `syntect` feature. Per-language
  `tree-sitter-*` features exist if higher fidelity is wanted.

## What the UI does and does not do

Implemented in `app.rs` (compiled; see the verification table for what was
actually exercised):

- Title bar: repository, "changes in X going into Y", swap, split and unified,
  collapse all, expand all, other revisions toggle, copy all as prompt, theme.
- Scope bar: the three scopes, the commit picker as a popover with two-row
  checkbox items, the two groups with group checkboxes, files-reviewed count.
- File panel: flat list with status, comment count, reviewed tick, counts.
  Clicking reveals and expands the file.
- Diff: GPUI Kit `Diff` with comments as annotations, a draft comment box, a
  "Reviewed" checkbox in each file header, notes for "changed since your
  review" and for files that could not be combined.
- Comments: created from the add-comment button or `c` on a selection,
  anchored by blame, saved to the review file, reactions with the picker,
  delete.
- Shortcuts: `]` `[` `}` `{` `s` `o` `t` `1` `2` `3` `zM` `zR` `c` `r`, Escape,
  Cmd/Ctrl+Enter, Cmd/Ctrl+R, Cmd/Ctrl+Q.

Not implemented, against the mockup and the design document:

| Missing | Notes |
| --- | --- |
| File tree | The panel is a flat list. The design wants a tree |
| Commit search, shift-click runs, skipped markers, comment counts in the picker | Picker is a plain list |
| Per-file commit stepper | Section 14 of the design document |
| `j` `k` line cursor, `v`, `g`, `(` `)`, `e`, `n` `N`, `za`, command palette | Design question 8 for the cursor |
| Selections inside a line | Design question 7 |
| Suggestions | The file format has the field; no UI |
| Edit comment, per-comment copy as prompt | Core supports edit |
| Auto-unified when narrow, remembered layout | Starts in split |
| Unchanged reviewed files as collapsed headers in the diff | Design question 9 |
| "Branch has moved since this review" warning | Header data is stored |
| Generated files collapsed by default | Not detected |
| Theme picker | Only a light and dark toggle |
| Exact mockup styling of comments | Other-revision comments are dimmed, not dashed |
| Importing a colleague's file from the UI | Works by dropping the file in the folder |
| Persisted window and UI settings | None |

## Suggested next steps

In this order, each one small enough to show the owner:

1. **Run it on a real machine and a real repository.** It has only run under
   a virtual display with software rendering, driven by simulated input, on a
   tiny demo repository. Operate every control by hand and fix what is wrong.
2. **Match the mockup's density and layout.** Compare side by side with
   `docs/mockup/main-screen.html`.
3. **Ask the owner the open questions** in section 16 of the design document,
   starting with 1, 2, 4 and 5. They change the data model.
4. **Commit picker to spec**: search, shift-click, skipped markers, counts.
5. **File tree** in place of the flat list.
6. **Keyboard model**: line cursor and the remaining keys.
7. **Per-file commit stepper.**
8. Selections inside a line, suggestions, comment editing.
9. Only when the UI has settled: the `gix` and `imara-diff` backend.

## Traps

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
