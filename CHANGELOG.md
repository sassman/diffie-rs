# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] - 2026-11-17


### Bug Fixes

- Resolved threads still counted in tree, header and picker badges

  The cards fold away on a verdict, but the orange numbers kept ticking,
  nagging about finished work. Every count badge now shows open threads
  only, matching the scope bar's open-comments count.

- Stop long diff lines stretching comment cards in unified view

  The unwrapped unified strip is as wide as its longest line, and the
  annotation wrapper filled it. The Kit fork now pins line annotation
  cards to the viewport's width and offset, the same treatment the pinned
  file header gets; this bump picks that revision up. Verified on screen
  on a demo repository; split view is untouched.


### Documentation

- Settle how open comments outside every comparison surface and die

  A thread whose file is in no comparison counted as open but had no card,
  so no resolve and no archive - the count-click dead-ended after flipping
  the scope. Settled: synthetic snippet files behind the earlier-comments
  toggle, archive as the disposal.

- Update the review skill to offer --pr and cost fewer tokens

  - a branch with a pull request now gets both ways offered, --pr
    recommended: its window publishes comments to the PR and approves it
  - the skill loads at roughly half the size; what `diffie --prompt`
    shows anyway is no longer spelled out
  - a failed launch keeps its message, so the `git fetch` a PR commit
    needs is not lost to /dev/null
  - the upstream example says origin/main, not the author's own remote name
  - the README says how to install binary and skill, and how to link the
    skill from a checkout so it follows `git pull`


### Features

- Surface open comments no comparison can show (#5) by @sassman in [#5](https://github.com/sassman/diffie-rs/pull/5)

  - "n open comments" counted threads whose file is in no comparison,
  while the click dead-ended and the only disposal was hand-editing the
  review TOML
  - such threads now render as synthetic snippet files (stored snippet as
  addition rows), behind the earlier-comments toggle, with the full
  out-of-scope action row - archive is the way out
  - the count click and `n` land on them by clearing the toggle; the
  label, reviewed progress and file tree treat the entries as what they
  are: not part of the changeset
  - design decision recorded in docs/design-decisions.md;
  toggle/jump/label flows verified on screen, the archive click path only
  compiled

  ---------

- Add a command palette so actions are reachable without the mouse (#4) by @sassman in [#4](https://github.com/sassman/diffie-rs/pull/4)

  ## Why

  Actions like approving a pull request sit behind menus. A palette on
  Cmd/Ctrl+P makes them reachable from the keyboard and gives new commands
  one place to join.

  ## What changed

  - Cmd/Ctrl+P (also View > Command palette) opens the Kit's `Command` in
  a dialog.
  - Commands: Reload, split or unified, and in a `--pr` review Approve
  with / without comments, greyed by the same `can_approve` check the
  title-bar menu now uses.
  - Rows show their shortcut; keywords widen the search ("lgtm", "split").
  - Focus returns to where it was on close; a reopened palette starts on
  the first row.
  - Gruvbox `accent.background` is one step lighter: it equalled the
  popover colour, hiding the highlighted row.
  - `docs/design-decisions.md` records Cmd/Ctrl+P (was Cmd/Ctrl+K) and the
  palette rules; `docs/HANDOFF.md` is updated.

  ## Verified

  - On macOS by screenshot: open, key hints, keyword search, arrow keys,
  Enter, focus return after Enter and Escape.
  - Not seen: the approval rows and an approval going out (no `--pr`
  review was run); Linux and Windows.
  - `just ci-check` is clean.


### Miscellaneous Tasks

- Keep local superpowers notes out of the repo

## [0.1.0] - 2026-11-17


### Bug Fixes

- Let r tick the first open file before any navigation

  On a fresh load current_file was empty, so r on the opening screen did
  nothing until the reader jumped somewhere. The first expanded, unticked
  file - the page the view opens on - is now current; the collapsed
  headers that can lead the list are not being read and never soak up
  the first r.

- Let r tick the file the change keys' mark landed in

  The change keys move the focus mark inside the Kit, but r acts on
  current_file, which only selection, file stepping, comment jumps and
  search hits updated - after ]/[ it ticked the file the reader had left,
  or nothing.

- Keep the warning triangle out of the files tree

  The changed-since triangle made tree rows noisy, and the file header
  already carries the "changed since your review" note. Tree rows stay
  quiet; on-demand detail lives in the header.

- Align picker rows, catch Quit globally and use bar change marks

  Four small fixes gathered from review sessions: the picker checkbox
  rode 2px high, group headings only toggled on their checkbox, Cmd+Q
  died when focus sat in an overlay, and the diff's +/- signs give way
  to the Kit's bar indicator.

- Stop racing background work from losing review edits

  A reaction, tick or comment landing while a reload was in flight was
  wiped from memory and then deleted on disk by the next save. The
  reviewer's own file is now generation-stamped, a reload that lost the
  race keeps the live version, and saves go through a single writer so
  disk order equals mutation order. A stale blame can no longer label
  the wrong draft, side-task failures show a status-bar notice instead
  of replacing a working diff, and search scans left the UI thread.

  The file header rework rides in the same file: the touched-by note
  sits behind an info icon, and a removed file starts collapsed, names
  itself in the danger color and dates its deletion on the D badge.
  Duplicated toggle, side-mapping and theme-palette logic collapsed
  into single helpers along the way.

- Keep revisions out of git's option parsing

  A commit id read from a colleague's review file reaches git verbatim,
  so an option-shaped value could act as an option instead of failing as
  a revision. Every call now passes --end-of-options before revisions -
  except blame, which acts on option-shaped revisions even behind that
  guard (seen on git 2.54), so it refuses dash-led revisions outright.

  Also in this file: a ticked file whose only change since the tick is
  whitespace no longer resurfaces as changed with branch-level counts
  under ignore-whitespace, and the scratch-object environment that keeps
  the reviewed repository untouched now lives in one place instead of
  two. Regression tests cover both fixes plus the previously untested
  every-difference mode, missing merge base, and rename tracking.

- --prompt refuses to guess between several reviews

  The latest-written pick could hand out another review's comments.
  Without a name only a sole review prints; several are listed by name
  and refused.

- Drop the sticky floating comment button for the context menu

  The + pinned at the drag release stuck around through clicks and
  keypresses. The context menu's Comment entry and `c` take over, acting
  on the same payload snapshot.

- Search jumps landed under the pinned header bar

- Reveal the fold hiding a search or comment jump target

- Moved detection pairs whole runs, a partial match is an edit

- Search hits drowned in the diff line fills

- A stray r keypress must not clear the reviewed tick

- Stop marking uncommitted-review comments as other revision

  An uncommitted review has no in-scope commits, so every fresh comment
  fell to the branch-tip fallback and dimmed to "other revision" at
  once - and a reload dropped it from view entirely. The working tree
  is exactly the revision under review there, so its comments are
  current: both the add path and place_all now say so.

- Quit when the last window closes

  macOS keeps a windowless process alive, but a review tool with no way
  to reopen a window is just a stuck process.

- Drop the current-file bar over the diff

  Each file's own header already names it; a second line repeating the
  selected path on top of the content carried no information.

- Sort the diff into the file tree's order

  The panel following the patch's path-sorted order kept the two columns
  in step but read wrong: a tree puts folders on top and the root's files
  at the bottom. Now both lists are resorted into that classic tree order
  at load, so the tree look wins and the diff walks with it.

- Keep the folder tree, ordered like the diff

  The flat list took the owner's metaphor literally. The tree look stays;
  it is built in one pass over the patch order, opening a folder row when
  the chain changes, instead of sorting folders ahead of files.

- List files in the diff's order

  The folder tree sorted the panel one way and the patch another, so the
  top file in the panel was not the top file in the diff. The panel is now
  a flat list in the patch's own order, the folder muted before the name.

- Stop the comment button from pinning on empty selections

  A double click reports a selection without real text, which pinned the
  floating + at arbitrary spots. Now only a selection worth quoting pins
  it, and line comments start from the diff's own + beside the hovered
  line number (the Kit button, without its permanent marker lane).

- A second press on the active layout segment kept flipping

  The Kit's single toggle group clears itself when the checked segment
  is clicked again; Split/Unified is an either-or choice, so the
  clearing click now keeps the current mode.

- Keep title bar buttons from minimizing the window

  Two quick clicks on a control bubbled to the title bar as a double
  click, which runs the macOS title bar action. The control clusters now
  swallow their clicks; the empty strip keeps the native double click.


### Documentation

- Small rewording

- The app crate and binary are named diffie

  The rename had landed while the docs, justfile and the naming decision
  still said review-app; `just run` was broken by it.


### Features

- Add pull request actions behind a forge trait (#2) by @sassman in [#2](https://github.com/sassman/diffie-rs/pull/2)

  - A `--pr` review is connected to its pull request; a title-bar icon
  opens it, publishes comments, and approves with or without them.
  - The code host sits behind a `Forge` trait, so nothing outside
  `github.rs` names GitHub.
  - Published comments keep their forge id and URL in the reviewer's own
  file: never posted twice, and each card links to its comment.
  - One review per publish, so one notification; a refused line lands on
  the file, a refused path stays out without blocking the rest.
  - Shows the pull request's state, review decision and CI, with "Open
  latest CI run".
  - Reactions map to GitHub's nearest emoji.
  - Not yet run against a real pull request or on screen; tested against a
  fake forge and a stand-in gh.

  ---------

- Update other-revision cards to keep their actions

  Being pinned to another revision is no reason a comment cannot be
  addressed: react, resolve, reply, copy and archive all work there now.
  Only edit and delete render disabled, with the tooltip saying why -
  the code the comment was written against is not on screen. Before
  this the whole actions row was hidden.

- Fold resolved threads behind a quiet line marker

  A verdict should take the thread out of the way: the card stays a
  three-second grace so the click is seen landing, fades, and folds into
  a checked comment bubble in the gutter lane - faint until the pointer
  is on its line, a click away from showing the cards again for the
  session. Nothing is persisted, so a restart folds every resolved
  thread. The n/p walk now skips resolved comments for the same reason:
  it is a tool for working through what is open, and done is done. The
  context menu gains Comment line, so a line whose threads folded can
  start a fresh one without a selection.

- Update the comment actions to icons with tooltips

  Five labels read like a sentence. Resolve keeps its word as the row's
  one verdict; pencil, clipboard, archive box and trash carry the rest,
  each named by its tooltip.

- Add jump back and forward to undo an accidental navigation

  Cmd/Ctrl+[ returns to the page the last jump left - its top row
  restored exactly by the Kit's new scroll_to_top_line - and Cmd/Ctrl+]
  retraces the steps; a fresh jump clears the forward trail, as in a
  browser. Plain scrolling pushes nothing, so the spot being read before
  an accidental n or ] is what comes back. Carries the Kit pin with
  scroll_to_top_line.

- Add an Archive action so finished threads stop making noise

  Archiving moves the whole thread - the comment plus the owner's own
  reactions and replies - into archive/<author>.toml beside the live
  review file, which nothing reads back: final, no restore. The archive/
  folder keeps the record out of colleague discovery, and the archive
  write lands before the live file forgets the comment, so a failed
  write never loses the thread.

- Update jump landings to center the mark and spotlight the comment

  The Kit pin moves to 1cccf3c4: change keys and scroll_to_line center
  the reached group in the viewport instead of tucking it under the top
  edge, and n/p ride on scroll_to_line, so comment jumps center the same
  way for free. The landed comment was still easy to lose among its
  neighbours, so while the focus mark stays on the jumped-to line its
  card carries a 2px primary border; a change-key press replaces the
  mark and the card falls back in line.

- Update the file header to hide comment breakdown on hover

- Update reload to pin the base and take in new commits

  Cmd+R re-resolved the typed target, so `diffie HEAD~1` drifted with every
  new commit and a branch target swallowed exactly the commits a reload
  should add. The target is now pinned to its startup commit while the head
  side keeps moving, and an all-picked custom scope re-picks the fresh
  commit list; hand-picked subsets stay untouched.

- Add the application menu bar

  GPUI installs no default bar, so an app without set_menus has an
  empty one. Letter-bound actions sit behind unbound menu-only wrappers
  handled by the root: a bare-letter NSMenuItem key equivalent would
  swallow the key while the reviewer types a comment. Edit rides the
  native selectors via OsAction, About is the standard AppKit panel
  with the icon passed explicitly because an unbundled binary shows a
  generic folder otherwise. HANDOFF covers this and the icon work.

- Show the brand mark as window and Dock icon

  GPUI reads WindowOptions::icon on X11 alone and never touches the
  macOS app icon, so the Dock gets a runtime setApplicationIconImage:
  call - the dev binary has no bundle to carry an .icns. The image
  crate is pinned to the version gpui already carries so the RgbaImage
  types unify; the objc2 crates ride in at the versions gpui-pre-macos
  locked. Wayland waits for a .desktop entry, Windows for an embedded
  .ico, both packaging-time.

- Add the eagle brand mark

  The reviewer's gaze on the README, and a 1024 px master from which
  packaging-time icons (.icns, .ico) get generated rather than stored.

- Add --pr to review a GitHub pull request by number or URL

  gh names the branches, but the review pins the PR's exact commit ids:
  branch names go stale locally and vanish on merge, while headRefOid and
  baseRefOid reproduce what GitHub's own Files-changed view shows, merged
  or open. The merge commit's first parent was rejected as the merged
  target: a base rebuilt under the PR drags foreign commits into the
  picker. Diffie never fetches; a missing object names the fetch to run.

- Comments follow rewritten code and the review talks back

  A second review round used to lose its first: blame missed rewritten
  lines, so addressed comments vanished while still counting open, and
  an answer to a question had nowhere to live. A comment whose code
  changed now follows the rewrite to the replacement, tagged "code
  changed" with its stored snippet - never "addressed", judging stays
  human. A ticked file with open comments stays visible in New for me,
  and the open-comments count and n/p walk every comment, clearing
  whatever hides a stop and stopping at the ends.

  Comments thread: replies live in the replier's own file like
  reactions; diffie --prompt prints each comment's id and diffie
  --reply answers headless as "Claude", optionally naming the agent
  conversation behind a robot badge that hands back `claude --resume`.
  In the app a thread invites the next word with a Respond box and an
  explicit Resolve button - the Done reaction with a door handle on it -
  and resolving changes nothing but the reaction. Quoted snippets follow
  the wrap toggle and scroll on their own past the Diff's wheel mask.

- Drop the --source/--target flags, revisions are the one spelling

  A superseded second form for the same comparison. The prompt mode
  follows: the review to print is a positional name, not --source.

- Read the review back with diffie --prompt, not a shipped script

  The skill needs diffie installed anyway, and its bundled python reader
  duplicated the app's prompt rendering - a drift waiting to happen. The
  rendering stays in the core (`review::prompt`), headless behind
  `--prompt`/`--source`/`--all`, and gains author names when reviewers
  differ and a [resolved] mark for included closed threads.

- Add the diffie-review agent skill to the repository

  The user-local skill becomes a vendored copy anyone can install
  (npx skills add, or plain cp). Its launch table now reads revisions in
  the merge direction and mentions the single-remote fallback, matching
  the CLI change.

- Highlight the matched string inside a search-hit line

- Read positional revisions in the merge direction

  `diffie x..main` reviews x going into main; reading like git diff kept
  surprising. A revision unknown locally resolves through the remotes
  when exactly one has it, otherwise the CLI fails before a window opens.

- Add a context menu with Copy and Comment on the selection

- Sticky file header while the file's content scrolls

  The implementation lives in the Kit fork's sticky-file-headers branch,
  stacked on folded-ranges; the app only moves the pin there.

- Add uncommitted changes as a pickable commit-picker layer

- Add changeset search with an all-commits extension

  Finding every place the changeset touches a name meant eyeballing the
  whole diff. `/` opens a search bar seeded from the text selection;
  smartcase matches over the scoped patch tint their lines and
  enter/shift-enter walk them. "All commits" extends the search over the
  whole branch in the background, and stepping onto a hit the scoped
  view does not hold switches the scope to Whole branch and lands on it
  after the reload - an explicit jump may change the scope, per-file
  state never does.

- Add an open-comments counter and n/N jumps

  "Am I done?" was answered by rescrolling the whole review. The scope
  bar now counts open comments in the comment accent and flips to
  "comments handled" when every thread carries a resolving reaction;
  n and N walk the open comments in reading order, revealing collapsed
  files on landing. The walk skips comments the other-revisions toggle
  hides, because a jump must land on something visible.

- Fold verbatim-moved blocks behind labeled rows

  A moved block reads as a removal plus an addition, so reviewers study
  code that only changed address. Both ends now collapse to one row -
  "moved to b.rs:3-8" where the block left, "moved from a.rs:3-8" where
  it landed - and a click reveals the lines when the move itself needs a
  look. Rendering rides the fork's new DiffFold rows, so the Kit patch
  moves to the folded-ranges branch (stacked on the earlier fork work).

- Detect blocks a patch moves verbatim

  A moved block reads as a removal plus an addition, so reviewers study
  code that only changed address. The detector pairs runs of removed
  lines with identical runs of added lines across the whole patch,
  exact-match only (an indentation change is an edit worth seeing) and
  above git's --color-moved bar of 20 alphanumeric characters, so lone
  braces cannot pair up. The UI will fold such blocks behind a label.

- Add a hide-whitespace toggle to the title bar

  Re-indents and formatter runs bury the lines a reviewer actually has
  to read. The pilcrow button reloads the diff with --ignore-all-space;
  a file whose changes are all whitespace drops out of the view and the
  reviewed count until the toggle goes off. The file list for scopes and
  blob maps stays complete, so ticks and scopes are unaffected.

- Update the prompt header to the compiler-style path:line form

  "path (line 13, as of commit b0fc4ba)" said in prose what
  "path:13 at b0fc4ba" says in the location syntax every tool and
  agent already parses.

- Replace the other-revisions button with an outdated comments switch

  The labelled button spelled its state out ("...: on" / "...: off") and
  read as a mode choice. Showing comments from other revisions is an
  overlay on the view, so the Kit Switch carries the state in the knob
  and the label shrinks to "Outdated comments".

- Fold folders in the file panel

  A folder row now carries an open or closed folder icon and a click
  collapses its subtree in the panel; the first collapsed ancestor hides
  everything beneath it. Panel only - the diff keeps showing every file.

- Split panels scroll their long lines separately

  Split wrapped as an interim because the Kit scrolled both columns as
  one strip. The fork branch split-panel-scroll makes each panel half
  the viewport with its own offset, so the wrap toggle now only wraps
  and split scrolls per panel; gutters stay put while the code shifts.

- Read the comparison like git diff

  - Positional revisions borrow git's muscle memory: none is the
    uncommitted review, one diffs the working tree against it, two
    (or A..B) review the second going into the first.
  - -C <path> picks the project, so a bare word is always a revision
    and a folder named like a branch cannot collide; a path-looking
    directory argument errors with a -C hint.
  - The caption says "everything the working tree adds" when the target
    is not the checked-out branch, where "uncommitted" would understate
    the diff.

- Add uncommitted review as the plain-open default

  - Opening a folder with neither --source nor --target now reviews the
    working tree's uncommitted changes going into the checked-out branch,
    the review one most often wants before committing.
  - Untracked files count as uncommitted: the diff runs against a scratch
    index with intent-to-add entries, so the real index is never touched.
  - Reviewed ticks hash the file on disk instead of a tree, so ticking and
    New for me behave as everywhere else; deletions tick blobless as usual.
  - Uncommitted lines exist in no commit, so the patch's new side is
    Synthetic and comments cannot name the writing commit there.
  - The swap button hides in this mode; the working tree cannot be a
    target.

- Add commenting, reviewed ticks, themes and fast large loads

  - Comments attach to the commit that wrote the line; a selection stays
    visibly marked after posting and bodies render Markdown, so a thread
    still reads like the code it discusses.
  - A reviewed tick stores an optional content id, so a deleted file can
    be ticked off like any other.
  - `r` collapses the file and moves the marker without scrolling, to
    avoid jumping around while working down the list.
  - One `ls-tree` pass replaces a subprocess per file and the file panel
    is virtualized, so a several-thousand-file commit loads in seconds
    instead of minutes.
  - Unified diffs scroll right by default with a wrap toggle; split wraps
    until gpui-kit grows per-panel horizontal scrolling.
  - Title bar names project, worktree and branches; commit picker rows
    lead with the title; themes and bundled fonts ship with the app.


### Miscellaneous Tasks

- Add a changelog for first release v0.1.0

- Update the Kit pin so change navigation stops at the ends

  The fork's change keys no longer wrap past the terminal group, matching
  the comment walk the app already clamps.

- Update the Kit pin for the louder change-navigation mark

  The owner found the mark too subtle live. The fork widens the edge bar
  to 3px, raises the row wash to 0.14 primary and tints the marked rows'
  gutter cells so the landing reads at a glance.

- Let navigation mark where it lands and guide within the page

  The change and comment keys scrolled with nothing pointing at the
  target - under the pinned header for changes - and after paging away
  they yanked back to wherever the walk had stopped, which read as
  dizzying. The reached stop now carries the Kit fork's focus marker and
  is the anchor while visible; scrolled off the page, the walk re-anchors
  at the viewport. Kit pin moves to f9e19fbc for the marker and the
  anchored stepping.

- Move the Kit pin to the rebased fork branches

  Upstream merged the bulk-collapse PR (longbridge/gpui-kit#3935), so the
  fork stack rebased onto upstream main; the lock follows the rewritten
  folded-ranges head. The pin stays =0.9.1 - upstream has not released
  since - and the patch now carries three commits instead of four.

- Let the diff follow folder folds in the file panel

  Shutting a folder only tidied the tree while the main view kept every
  file expanded, so the two walked out of step. A shut folder now
  collapses its files in the diff in one rebuild, and reopening frees
  only the files not still under another shut folder, so nested folds
  survive the round trip.

- Add the just ci-check gate

  One command now runs the whole sweep: fmt, clippy -D warnings, cargo
  deny, cargo shear, doc build and all tests including doc tests. The
  deny policy allows permissive licenses only, flags unmaintained crates
  only when they are direct dependencies (GPUI's snapshot tree is not
  ours to fix), and sanctions the one git source, the owner's Kit fork.
  Shear's first catch, the unused anyhow dependency, is dropped.

- Pull the pinned file headers into the build


### Performance

- Collapse and expand all files in one rebuild

  Collapse all looped set_file_collapsed, and every call rebuilt the
  whole row model: minutes on a several-thousand-file review. The load's
  reviewed/generated collapse had the same loop. Both now use the fork's
  DiffState::set_files_collapsed (one rebuild), wired via patch.crates-io
  on the owner's gpui-kit fork until upstream takes the PR.


### Refactor

- Let thiserror carry the error boilerplate

  Two error enums hand-rolled Display and Error while the crate already
  depends on thiserror; the derive keeps the exact wording with far less
  to drift. Alongside: unused helpers removed, the oid newtype's field
  made private so the hex invariant is enforceable and short() cannot
  panic, and reply matching no longer clones whole review files.

- Walk unified diffs in one place

  Search and moved-code detection each carried their own copy of the
  patch-walking state machine, down to the comments; a drifted copy was
  only a matter of time, and both shared a latent panic on a malformed
  hunk header. One crate-private walker now emits per-line events and
  both consume it.

- Route diagnostics through tracing

  Ad-hoc eprintln probes and a /tmp file writer for the selection flow
  served one debugging session each and leaked into every run. The app
  now installs a tracing-subscriber fmt layer filtered by RUST_LOG
  (default diffie=warn), the selection flow logs at trace level, and the
  load pipeline at debug. CLI usage and error texts stay plain prints:
  they are the interface, not diagnostics.

- Finish the rename to diffie and diffie-core

  The owner renamed the crates; this carries the imports, docs and the
  naming decision along.


### Styling

- Split and unified as icons in the segmented control

- The outdated comments switch becomes an earlier comments toggle

  "Outdated" judges relevance, while the comments are only pinned to
  another revision. An icon toggle like wrap and hide-whitespace, the
  dashed bubble the diff already draws them with, named in the tooltip.

- Slightly larger file tree text

- More air between indent guides and file names

- Add indent guides to the file tree

  Nested rows floated in space once a tree grew a few levels deep; a
  faint vertical line per ancestor level, as Zed draws them, lets the
  eye walk a folder's contents without re-reading indentation. Absolute
  segments per 22px row keep the virtualized list layout untouched and
  stack into continuous lines.

- Make changed-since-your-review impossible to miss

  The note sat in the same muted grey as ordinary metadata and the tree
  row showed nothing at all - a vanished tick is invisible unless the
  reviewer remembers it was there. The note now renders in the warning
  amber with medium weight, and the tree row carries a small amber
  triangle, mirroring how reviewed files carry their check.

- Clippy

- Square the floating comment button

  The circle looked foreign next to the Kit's square-ish add-annotation
  look the owner liked; back to theme radius at control height.


### Testing

- Pin that one revision reads like git diff

  A report said `diffie HEAD~1` leaves out the commits since the revision
  or the uncommitted layer. The backend provably includes both; this pins
  it so the single-revision promise in the usage text cannot rot silently.


### Ci

- Add checks for main and PRs, and a tag-driven release (#3) by @sassman in [#3](https://github.com/sassman/diffie-rs/pull/3)

  ## Why

  Nothing checks `main` or a PR today, and a release is manual. This makes
  the gate automatic and turns a pushed `v*` tag into the whole release,
  as on amoxide.

  ## What changed

  - **CI** (`build.yml`): pushes to `main`, PRs and merge groups. Linux
  runs `just ci-check`; macOS and Windows run clippy and the tests.
  Docs-only changes are skipped.
  - **Release** (`release.yml`, `dist-workspace.toml`): cargo-dist 0.32
  builds macOS (arm, Intel), Linux x86_64 and Windows x86_64, creates the
  GitHub Release and commits the formula to `sassman/homebrew-tap`.
  - **Changelog**: git-cliff (`cliff.toml`), not release-plz - the tag is
  set by hand and nothing goes to a registry. `just release-prepare
  <version>` bumps the version and rewrites `CHANGELOG.md`.
  - **No crates.io publish.** The README's new Install section offers
  `brew install sassman/tap/diffie` and `cargo install --locked --git`.
  - **README header**: centred title, tagline and badges.

  ## Before the first tag

  - [ ] Add the `BOT_APP_ID` and `BOT_PRIVATE_KEY` secrets (the tap push
  needs them)
  - [ ] Windows was never built; this PR's run is the first evidence
  - [ ] The repo is private: brew, `cargo install --git` and the release
  badge only work for people with access

  ## Verified

  - Locally: `actionlint` (only shellcheck notes in dist's generated
  shell), `dist plan`, `cargo deny check`, `cargo shear`, `cargo fmt
  --check`, and a dry run of `release-prepare`.
  - Not verified: the full `just ci-check` in this branch, and any run on
  GitHub before this PR.

  ---------

