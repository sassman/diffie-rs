# Handoff

State of the project on 2026-11-16, written for whoever continues it in a
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
- **Symbol navigation** (`crates/symbols`, `app/symbols.rs`), added
  2026-11-18: Cmd/Ctrl-hover styles a resolvable name as a link,
  Cmd/Ctrl-click jumps to its definition. The engine and its rules are
  tested; **the gesture itself has not been seen on screen** (see the
  verification table). Design document section 19 holds the decisions.
  The Kit patch moved to the fork branch `diff-text-pointer` for it.

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
| CI and release workflows | Never run on GitHub. `actionlint` passes bar style notes in dist's generated shell; `dist plan` resolves the four targets; `just release-prepare` was dry-run and dist lifted the changelog section for the tag. A Windows build has never been attempted, so the Windows CI job and release target are unproven |
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
| Archive action on own comments: `take_comment` + `ArchiveFile::append` in the core, Archive button and background write in the app | Core rules covered by unit tests (move out of the live file, append dedupe, invisibility to discovery and `--reply`); the button and the write flow compiled, clippy-clean, not seen on screen - the window was not run |
| `r` ticking the file the focus mark is in after `]`/`[` (`follow_mark` pulls `current_file` from the Kit's mark; `n`/`p` and search jumps already set it), and ticking the first expanded, unticked file on a fresh load before any navigation | Compiled, clippy-clean; not seen on screen - the window was not run |
| Jump-back and forward (Cmd/Ctrl+[ and ]): the two stacks, the exact restore, the hint-bar entry and the Go > Back and Forward items | The Kit's `scroll_to_top_line`/`top_line` round trip is covered by a Kit integration test (`scroll_to_top_line_restores_what_top_line_read`); the app side compiled, clippy-clean, not seen on screen - the window was not run |
| Command palette (Cmd/Ctrl+P): the list, key hints, keyword search, focus returning on close | Verified on macOS by screenshot, driven by System Events keys: Cmd+P opens it with Reload (Cmd+R hint) and the split/unified switch (`s` hint); "spl" finds the switch by keyword; Enter switched the layout and a following `s` switched it back (focus was on the diff again); the same after Escape; Down, Down (wraps) and Up move the highlight, and a reopened palette starts on the first row again (read off the row backgrounds in screenshots). The two approvals and their greying were not seen - no `--pr` review was run |
| Other-revision cards keeping the full actions row, Edit and Delete disabled with explaining tooltips | Compiled, clippy-clean; not seen on screen - the window was not run |
| Resolved threads folding into gutter-lane markers: the 3 s grace and fade after a verdict, the marker's hover brightening and click toggle, the open-comments-only walk, the context menu's Comment line | Compiled, clippy-clean; not seen on screen - the window was not run. The timers, the animation and the marker lane's layout all need a real window to judge |
| Back, forward and reload icon buttons leading the scope bar | Compiled, clippy-clean; not seen on screen - the window was not run |
| Pull-request menu (title-bar icon in a `--pr` review): open in the browser, publish comments, approve with and without comments; the link icon on published cards; the status-bar line after a publish | Core covered by tests: what a publish sends and records (`forge.rs`, against a fake forge) and the exact gh calls, the file-level fallback and the pending-review cleanup (`github.rs`, against a stand-in gh script, Unix only). The GraphQL input and enum names were checked read-only against GitHub's live schema by introspection. The status query (`gh pr view --json state,reviewDecision,statusCheckRollup` with the app's jq) was run read-only against a real merged PR and its lines match what `fold_checks` parses. **Nothing was ever posted to a real pull request**, and the window was not run: the menu, the state icon and its red tint, the busy state, the link icon and whether a pending-review comment's URL still opens after the submit are all unseen |
| Count badges showing open threads only (file tree, file header and its tooltip breakdown, commit picker) once a thread is resolved | Compiled, clippy-clean; not seen on screen - the window was not run |
| User settings (`settings.rs`): split/unified and window bounds persisted to `~/.config/diffie/settings.toml`, restored at startup | Round trip, defaults on missing or broken files and future-key tolerance are unit-tested; the live flow (bounds observer, coalesced writes, startup placement) compiled, clippy-clean, not seen on screen - the window was not run |
| Comment cards capped at the viewport in unwrapped unified view (Kit fork commit `b97451f2`: the annotation wrapper takes the viewport's width and cancels the strip's offset, as the pinned file header does) | Verified on macOS on 2026-11-16 by screenshot: a demo repository with a 2378-character changed line and a hand-written review comment; the card ends at the viewport's edge with its actions row visible. The offset-cancel while scrolled sideways was not exercised (synthetic wheel events did not reach the scroll mask); split view unaffected (owner looked at it) |
| Orphan comments as snippet files: entry with note and comment badge in diff and panel, addition rows at stored lines, dashed card tagged "not in this comparison" with plain "lines n-m" header, scope bar "1 open comment" with the orphan outside "0 of 0 files reviewed", `o` hiding entry and card in both diff and panel while "1 not in this comparison" appears, `n` turning the toggle back on and landing on the card with the focus marker, scope untouched | Verified on macOS on 2026-11-17 by screenshot against a demo repository with a hand-written stale comment on a path the repository never had (uncommitted comparison, empty diff) |
| The same orphan flow's mouse paths: the count click (same `jump_comment` as `n`), the card's Archive click and the reload that removes the emptied snippet file, resolve folding an orphan card | Compiled, clippy-clean; not seen on screen - synthetic clicks never reached the handlers (CGEvent `postToPid` mouse events are ignored where its key events work), and real HID clicks were abandoned to keep hands off the owner's live session |
| First paint of a freshly opened occluded window lays the file header out at content width (the file name starves before the note truncates); the first real repaint fixes it. Pre-existing, surfaced by narrow snippet files; the name cluster now keeps its content basis (`flex_auto`) and the note truncates, so the name survives | Seen on screen on 2026-11-17 (the artifact and that it clears); the flex change itself judged only in the healthy layout |
| Symbol engine (`diffie-symbols`): Rust definitions, references and imports; role, qualifier and ranking rules; ties; positional membership of variants and fields; per-revision views, the working-tree overlay and references for shown files only | Unit tests on source snippets and tests against a throwaway repository, macOS, 2026-11-18. Probed once against this repository: both views of a load in about 135 ms |
| Core `blobs` (batch read, bytes kept exact) and `worktree_file` | Tests in `crates/core/tests/git_cli.rs`, macOS, 2026-11-18 |
| Kit fork additions (`on_text_hover` including the modifier-at-rest case, `on_text_click` and its claim, `has_line`, `DiffFile::excerpts`) | Kit tests with simulated pointer and modifier events, macOS, 2026-11-18. The pointing-hand cursor and the underline style compile and render in those tests; how they look was not seen |
| Symbol navigation in the window: Cmd-hover link styling and cursor, Cmd-click landing inside the diff with the focus mark, the origin card for a name from another crate, "Copy symbol" in the context menu, the tie dialog, a definition entry (excerpt rows, header note and x, panel row), Cmd+[ back from each, chained jumps inside an entry | Compiled, clippy-clean. The window was started once against this repository and loaded with the views built; **no gesture was exercised** - synthetic mouse events do not reach the handlers (see the orphan row above) |
| Moved-code folds surviving the earlier-comments toggle (`sync_diff_files` puts them back after `set_files`; before, the toggle dropped them whenever orphan entries existed) | Compiled; not seen on screen |

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
A `--pr` review is connected to its pull request: the title bar gains a
pull-request icon whose menu opens it in the browser, publishes the
reviewer's comments to it, and approves it (see "Pull requests and the
forge" below).
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

### CI and releases

`.github/workflows/build.yml` runs on pushes to `main` and on pull requests:
`just ci-check` on Linux, clippy and the tests on macOS and Windows.

A release is a tag the owner pushes. `just release-prepare <version>` bumps
the workspace version and rewrites `CHANGELOG.md` from the conventional
commits (git-cliff, `cliff.toml`); the owner commits that, tags
`v<version>` and pushes the tag. `.github/workflows/release.yml` (dist,
`dist-workspace.toml`) then builds macOS, Linux and Windows binaries,
creates the GitHub Release with that version's changelog section, and
commits the formula to `sassman/homebrew-tap`. Nothing goes to crates.io
(`publish = false`). `release.yml` is generated by `dist generate` and then
hand-edited (tag pattern, path filters, GitHub App token for the tap), so
`allow-dirty = ["ci"]` keeps dist from overwriting it. The tap push needs
the `BOT_APP_ID` and `BOT_PRIVATE_KEY` repository secrets.

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
  pr.rs                       --pr: the argument's parsing and the pull
                              request as a forge describes it
  forge.rs                    Forge trait; what a publish sends (`plan`),
                              sending it (`publish`), recording the ids
  github.rs                   GitHubCli: the Forge over the gh CLI
  time.rs                     RFC 3339 without a date-time crate
crates/core/tests/git_cli.rs  backend tests against a throwaway repository
crates/symbols/src/           symbol navigation's engine; depends on the
                              core, never on the UI
  lib.rs                      Definition, Reference, Import, FileSymbols; `parse`
  language.rs                 the language table: one entry per language
  rust.rs                     Rust: the tree walk and the ranking conventions
  index.rs                    SymbolIndex (per blob id) and RevisionView
  resolve.rs                  from a reference to its ranked definitions
  origin.rs                   the crate and full path of a name from outside
  tests.rs                    extraction and resolution on source snippets
crates/symbols/tests/index.rs views against a throwaway repository
crates/app/src/
  main.rs                     arguments, window
  app.rs                      ReviewApp: all state, loading, rendering
  app/pull.rs                 the connected pull request: its standing,
                              the title-bar menu, publishing (a child of
                              `app`, so it reaches ReviewApp's state)
  app/palette.rs              the command palette: the command list and
                              running the chosen one
  app/symbols.rs              symbol navigation in the UI: which revision
                              each side reads, the link, where a jump
                              lands, definition entries, the tie dialog
  settings.rs                 machine-wide settings (~/.config/diffie),
                              TOML; layout mode and window bounds
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
Archiving (`take_comment` + `ArchiveFile::append`) moves a finished thread
into `archive/<author>.toml` beside the live file - the `archive/` folder
keeps it out of colleague discovery, which reads every `*.toml` in the
review folder. Nothing reads the archive back; the app writes the archive
first and only then drops the comment from the live file, so a failed
write never loses the thread.

`pr.rs` is command-line plumbing, not part of `ReviewBackend`: it parses the
`--pr` value (a number, or a pull-request URL kept verbatim so gh keeps the
host and repository) and holds the `PullRequest` a forge answers with: state,
branch names, exact commits, its page and its forge id. `main.rs` turns that
into a comparison whose `source_rev`/`target_rev` pin the PR's commits while
`source`/`target` keep the branch names for the title and the review file;
`GitCli::resolve` resolves the pinned revision when one is set.

### Pull requests and the forge

`Forge` (`forge.rs`) is the code host behind one trait, beside
`ReviewBackend` and just as blocking: `pull_request`, `status`,
`submit_review`, `update_comment`, `react`. `main.rs` builds the one implementation,
`GitHubCli`, and hands the app a `Pull` (forge plus pull request) only for
a `--pr` start. Design document section 18 holds the product rules and
which of them are assumptions.

A publish is separate steps, so each is testable alone:

- `plan(reviews, head, verdict, with_comments, blame)` decides what goes:
  the reviewer's own open, unpublished comments, stale edits, and unsent
  reactions on comments the forge knows. A new-side comment is anchored
  where blame finds its whole span unbroken at head (`anchor_at_head`);
  anything else - a rewritten or split span, a file gone at head, every
  old-side comment - becomes a file-level comment quoting the snippet.
  `pending(reviews)` counts the same things without building anything;
  the menu calls it on every render.
- `publish_review` sends the review (comments and verdict) and
  `publish_followups` the edits and reactions, one call each; `publish`
  runs both. Each returns an `Outcome` that is partial on purpose: what
  landed is reported even when a later call failed, refused comments are
  counted, and the first error is kept.
- `ReviewFile::record(outcome)` writes the forge ids into the reviewer's
  own file (`Comment::published`, `Reaction::published`). It compares the
  sent bodies with the live file, so an edit made while the publish ran
  stays pending, and a comment deleted meanwhile is skipped.
  `Published::on_file` remembers a file-level posting so a later edit
  sends the quoted form again.

`GitHubCli` writes through `gh api graphql`: open a pending review, add
each thread (falling back to a file-level thread when the line is
refused, and leaving the comment out as `Posting::Refused` when the file
is refused too), submit once - one review, one notification, and every
thread answers with its own id and URL. A failed submit, or a comment
review whose every thread was refused, deletes the pending review, or it
would block the next attempt (GitHub allows one per user and pull
request). REST's one-shot review was rejected: one bad line fails the
batch and it returns no comment ids. Variables go as `-f` (raw) so a body
starting with `@` is never read as a file; only line numbers use `-F`.

`Forge::status` answers a `PullStatus`: state, review decision and the
checks folded into one `CheckState` with the latest run's page.
`GitHubCli` reads it with one `gh pr view --json
state,reviewDecision,statusCheckRollup`, one tab-separated line per check
(`fold_checks`, `run_page`). `ReviewApp::refresh_pull_status` asks in the
background from `reload` and after a publish; `_pull_status` holds the one
task, so a newer request drops the older.

In the app, `ReviewApp::publish` runs the plan and the review step on the
background executor, records and saves the ids (`record_published`, on
the UI thread through `save_review` - the one writer), and only then runs
the follow-ups: a quit or a hung gh call among the one-by-one edits and
reactions must not leave posted comments looking unpublished. `publish_status` is its own status-bar line because `notice`
is cleared by the next successful save, and every publish ends in one.

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
  on, both modes wrap instead of scrolling. Line annotation cards no
  longer ride that canvas: the fork pins them to the viewport's width and
  cancels the strip's offset (`CardPlacement` in the Kit's diff, the same
  treatment as the pinned file header), so a long line cannot stretch a
  comment card sideways.
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
  foreground timer while a load is in flight. For headless verification,
  synthetic events posted with `CGEvent.postToPid` (a mouse move and click)
  wake it even while another window covers it, and
  `screencapture -l <CGWindowID>` then captures the occluded window.

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
- The Kit does not re-export `tree_sitter`, so `diffie-symbols` depends on
  `tree-sitter` and `tree-sitter-rust` itself, at the versions the Kit
  uses for its own highlighting.
- The fork's text pointer API, added for symbol navigation:
  `Diff::on_text_hover` reports the character under the pointer
  (`DiffTextPosition`: line, byte offset, the line's text) with the held
  modifiers - on mouse move, and when a modifier changes at rest **while
  the diff has focus** (GPUI routes modifier changes along the focus
  path); `Diff::on_text_click` gets a click on a character first and may
  claim it; `Diff::text_cursor` sets the pointer over code;
  `DiffSpanHighlight::without_background` and `with_underline`;
  `DiffState::has_line`; and `DiffFile::excerpts`, parts of a file at
  their own line numbers as a single-column reference file.
- `DiffState::set_files` clears the folds, the focus mark and every
  expansion. Anything that swaps the file set mid-session goes through
  `ReviewApp::sync_diff_files`, which puts the folds back.

## What the UI does and does not do

Implemented in `app.rs` (compiled; see the verification table for what was
actually exercised):

- Symbol navigation (`app/symbols.rs`): after each load a background task
  builds two symbol views - the comparison's base for the left side, the
  shown revision (or the branch tip with the uncommitted files overlaid)
  for the right. With Cmd/Ctrl held, the name under the pointer becomes a
  link when it resolves (`name_at`: the reference in that side's view,
  trusted only where the row spells its name). A name that resolves to
  nothing but is imported from another crate gets a card at the pointer
  instead (`render_origin_hint`): full path and crate, read from the
  imports by `RevisionView::origin`, through `use super::*` too. The
  diff's context menu offers "Copy symbol" on such names and on
  resolvable ones (`pointed_symbol`, from the last text hover).
  Cmd/Ctrl-click jumps: one
  target lands on its row when the diff carries the line, with the
  change-navigation focus mark, and otherwise opens a definition entry;
  tied targets open a dialog over the Kit's command list. Entries are
  `DiffFile::excerpts` files after the comparison's files, with a header
  note, an x, no tick and a muted row under the file tree. Every jump
  goes onto the Cmd/Ctrl+[ history; the palette lists "Go back" and
  "Go forward".
- Title bar: repository, "changes in X going into Y" with chips, swap,
  split/unified as a segmented icon pair (columns and rows, named on
  hover), wrap and hide-whitespace as icon
  toggles (the pilcrow reloads with `--ignore-all-space`, so whitespace-only
  files leave the view), collapse/expand all as a joined icon pair,
  earlier comments (other revisions') as an icon toggle (dashed comment
  bubble, "Earlier comments - from other revisions" on hover; it also
  hides the changed-since cards, which are earlier comments too),
  copy all as prompt, a theme menu behind the palette button.
- Scope bar: icon buttons for back, forward and reload leading it
  (tooltips name the shortcuts; back and forward disable with empty
  stacks), then segmented scopes; the commit picker with search, group
  checkboxes, skipped markers, open-comment badges, Show-more rows, shift-click
  runs and Space on the hovered row; in an uncommitted review an
  "Uncommitted changes" row leads "New since your review" so the working
  tree can be excluded like a commit (files carrying its changes say
  "uncommitted" in the diff header, warning-coloured); the open-comments count in the
  comment accent ("comments handled" once every thread is resolved) -
  clickable: it walks to the next open comment like `n`;
  the files-reviewed count with a progress bar. `n`/`p` walk the open
  comments (resolved ones fold away and left the walk on 2026-11-16) in
  reading order, stop at the ends instead of circling, and
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
  counts, open-comment badges, reviewed ticks, "generated"; truncation that
  keeps
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
  chips and the smile-plus picker, and icon-only actions with tooltips -
  Edit (pencil), per-comment Copy as prompt (clipboard), Archive (box; own
  comments only: the thread moves into the archive file and out of every
  view, no restore - `archive_comment`), Delete (trash) - beside the
  labelled Resolve button. An other-revision card keeps the full row so
  the comment stays addressable; only Edit and Delete render disabled
  there, with the tooltip saying why;
  other-revision comments dashed and quiet. A comment whose code was
  rewritten since follows the rewrite (blame misses, `changed_regions` +
  `map_line` carry it to the replacement) and sits there dashed in the
  warning tint, tagged "code changed", header "was line n", the stored
  snippet quoted above the body, actions kept so the reviewer can react
  Done to approve the fix; file headers count these as "on changed code"
  and the status bar names open comments no row currently shows ("n not
  in this comparison"). An open thread whose file is in no comparison at
  all - outside the whole branch, or unplaceable even there because a
  rebase dropped its commit - gets a synthetic snippet file
  (`FileState::NotInComparison`, `orphan_patches` in the core): the
  stored snippet as addition rows at its stored lines (pure context
  folded away and hid the card), header note "not in this comparison -
  shown for its open comments", card tagged the same, full out-of-scope
  action row with archive as the disposal. The entries ride the
  earlier-comments toggle (`set_other_revisions` swaps the Diff's file
  set and restores the top line, since `set_files` resets the viewport),
  stay out of the reviewed progress and carry no tick; archiving an
  orphan's thread reloads so no empty snippet file lingers. In the file
  tree the entry drops the status letter - it is in no changeset to have
  one - for the earlier-comments toggle's dashed-bubble icon, a muted
  name and a tooltip carrying the header's note. Replies thread under the comment body (author plus
  markdown, left-bordered). Resolving changes nothing but the reaction -
  the thread stays the full record - but its card folds away: a 3 s
  grace (the last 500 ms a fade, `FOLD_GRACE`/`FOLD_FADE`), then only a
  checked comment bubble remains in the gutter's marker lane, faint
  until the pointer is on the line. Clicking the marker toggles the
  line's resolved cards; the state (`unfolded`/`fading` on `ReviewApp`)
  is session-only, so a restart folds everything again. The marker lane
  is set only while resolved threads exist.
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
  and Comment acting on the text selection (both disabled without one), and
  Comment line starting a fresh selection-less thread on the right-clicked
  line - also the way to comment a line whose resolved threads folded; the
  items use the payload snapshotted at menu-open, because picking an item
  clears the live selection first. The menu and `c` are the only
  selection-comment entry points; there is no floating button.
- Ticking Reviewed (or `r`) collapses the file and moves the current-file
  marker to the next one without scrolling; the collapse pulls it up. `r`
  acts on `current_file`, which every way of getting somewhere keeps in
  step: selection, the file stepper, comment jumps, search hits, and the
  change keys (`follow_mark` reads the file out of the Kit's focus mark,
  which `next_change` moves internally - before that, `r` after `]`/`[`
  ticked the file the reader had left). A fresh load starts with the
  first expanded, unticked file current - the view opens at the top, and
  the collapsed headers leading the list are not being read - before
  that, `r` did nothing until the reader had navigated somewhere.
- Pull request (only in a `--pr` review): a pull-request icon leads the
  title bar's controls as icon plus "#17" (it spins while a publish
  runs). The icon's shape follows the state (open, merged, closed), and a
  small dot beside the number carries the loudest news: red for failing
  checks or requested changes, amber for running checks, green for
  passing ones, none when there is nothing to report (`trigger_tone`; a
  merely required review never paints it). The tooltip says the standing
  in words. The menu is the Kit's `PopupMenu` (`dropdown_menu`), read top
  down the way the reviewer decides: a heading ("#17 · topic going into
  main"), three status rows that are always there - State, Review, Checks,
  each an icon and a value in its tone's colour (`status_rows`, `Tone`),
  "…" until the forge answers - then the pages ("Open pull request",
  "Open latest CI run", both with the external-link mark; the latter
  disabled without a run), then "Publish 2 comments, 1 edit" (naming what
  would go, disabled with nothing new), "Approve with ..." and "Approve
  without comments" and "Request changes with ..." (all only for an open
  pull request), set apart by separators because they are verdicts.
  Request changes is offered only while a new comment would be published
  with it (`Verdict::needs_comments`; greyed, its label says "needs a new
  comment"), and `publish_review` refuses it without one. A closing hint
  line says why the sending items are grey when the label cannot. Swapping the
  direction disables the sending items, and a running publish disables
  swap. A published comment's card gains a link icon that opens it on the
  pull request; edited since, its tooltip says a publish will update it.
  The status bar reports the result ("published 2 comments to #17",
  with "(1 on the file, no line took it)" when a comment moved off its
  line, or what landed and then the forge's error in the danger colour).
- Status bar: side-task failures (saving the review, the search's
  whole-branch fetch, blame for a comment) surface as a `notice` there and
  clear on the next success; the full-screen `error` page is reserved for
  load failures.
- Shortcuts: `]` `[` `}` `{` `s` `o` `t` `n` `p` `1` `2` `3` `zM` `zR` `c`
  `r`, Escape, Cmd/Ctrl+Enter, Cmd/Ctrl+R, Cmd/Ctrl+Q, Cmd/Ctrl+[ and ],
  Cmd/Ctrl+P, Space in the commit list.
- Command palette: Cmd/Ctrl+P (also View > Command palette) opens the
  Kit's `Command` in a Kit dialog. `palette_entries` in `app/palette.rs`
  is the one list - Reload, split or unified, and in a `--pr` review the
  two approvals, greyed by the same `can_approve` the title-bar menu
  uses. The confirmed row is mapped to a `PaletteCommand` and run by
  `run_palette_command`, not through the item's Action: the dialog sits
  outside the review view, so an Action dispatched from it would reach
  no handler. Rows with a shortcut draw the hint themselves (`Kbd`),
  looked up from the diff's focus handle: from the dialog's own focus no
  binding resolves. The dialog returns focus to where it was on close.
  Matching is the Kit's plain substring over label and keywords; no
  ranking, no recently-used order. The `CommandState` lives as long as
  the window, so `open_palette` clears the query and puts the highlight
  back on the first row. The Kit paints the highlighted row with the
  theme's `accent.background`: Gruvbox had it equal to the popover
  colour, which made the highlight (here and in every menu) invisible,
  so it is one Gruvbox step lighter now.
- Jump-back and forward: Cmd/Ctrl+[ and Cmd/Ctrl+] (also Go > Back and
  Forward, and a status-bar hint) walk the jump history. Every jump -
  change keys, `n`/`p`, the file stepper, tree clicks,
  `reveal_first_comment`, search hits - calls `remember_position` before
  scrolling, which pushes the viewport's top row (`top_line`) onto
  `back_stack` and clears `forward_stack` (forward dies with new
  navigation, as in a browser); plain scrolling pushes nothing, so the
  spot being read before an accidental jump is what comes back.
  `step_history` pops one stack and pushes the page being left onto the
  other, skips entries whose file left the comparison, restores with the
  Kit's `scroll_to_top_line` (the exact counterpart of `top_line`;
  centering `scroll_to_line` would land half a viewport off) and sets
  `current_file` so `r` acts on the restored page. The stacks survive
  reloads and are capped at 100.
- Navigation focus marker: `]`/`[` and `n`/`p` mark where they land (the
  Kit fork's focus marker - a primary edge bar, a translucent wash and a
  tinted gutter on the marked rows -
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
| `j` `k` line cursor, `v`, `g`, `(` `)`, `e`, `za` | Design question 8 for the cursor |
| Selections inside a line | Design question 7 |
| "Branch has moved since this review" warning | Header data is stored |
| Importing a colleague's file from the UI | Works by dropping the file in the folder |
| Persisted panel width | The split/unified choice and the window's bounds persist in `~/.config/diffie/settings.toml` (design document section 17; `settings.rs`); panel width still lives only in the session |
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

- **Symbol navigation reads syntax, not types.** `value.method()` and
  `value.field` resolve by name alone and often tie; that is the design
  (section 19), not a bug to fix with heuristics. Names inside macro
  arguments are read from raw tokens by their neighbours - good for
  `assert_eq!` and `format!`, blind to macros with their own grammar.
- **A definition entry's Diff path is a key, not a path.** It carries a
  prefix (`→ `) because the real path may already have an entry: a
  changed file whose patch does not reach the definition, or an orphan
  snippet. Use `Symbols::entry` to get from the key to the file; never
  hand the key to the backend or the review file. `set_reviewed` and
  `start_comment` refuse it.
- **The add-comment button still appears on definition rows.** The Kit
  shows it for every file once `on_add_annotation` is set; on an entry
  it only says, in the status bar, that definitions are for reading.
- **Publishing was never run against a real pull request.** Everything
  below the `Forge` trait is tested against a stand-in gh, and the schema
  names were introspected, but the first real publish is the first real
  test. Try it on a scratch pull request before trusting it on a
  colleague's. Things to watch: whether GitHub accepts a blamed head line
  that is not inside a diff hunk (it should refuse, and the comment should
  land on the file), whether a comment URL from the pending review still
  opens after the submit, and what gh prints when the user already has a
  pending review on that pull request, and whether GitHub takes a
  request for changes that has line threads but no review body (the app
  sends none). Two known gaps: gh calls have no
  timeout, and a submit whose answer is lost is reported as a failure
  although the review may have landed - the next publish then posts the
  comments a second time.
- **Any failed line thread is retried on the file**, a dropped connection
  included: gh's exit code does not tell a refused line from a network
  error. The status bar counts file-level landings so the move is seen.
- **A published comment is never taken back.** Delete, archive and
  un-react change only the local file.

- **The Kit currently comes from the owner's fork.** `[patch.crates-io]`
  in the workspace manifest points every Kit crate at `sassman/gpui-kit`;
  `Cargo.toml` names the current branch, each branch stacking on the last:
  `bulk-file-collapse` (`set_files_collapsed`, one rebuild for any number
  of files), `split-panel-scroll` (per-panel horizontal scrolling),
  `pinned-file-headers`, `folded-ranges` (`DiffFold` +
  `DiffState::set_folds`, the labeled click-to-reveal rows the moved-code
  feature renders), `scroll_to_top_line` (the exact counterpart of
  `top_line`, which the jump-back restore rides on), and
  `diff-text-pointer` (the text pointer API symbol navigation rides on,
  stacked on `sticky-file-headers`). Drop the patch when
  upstream ships them.
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
