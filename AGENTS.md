# AGENTS.md

Rules for any agent working in this repository. Read `docs/HANDOFF.md` for the
current state and `docs/design-decisions.md` for product behaviour before
changing anything.

## What this project is

A native desktop app for reviewing git changes across several commits or a
whole branch, with comments on lines and selections. Rust, GPUI Kit, macOS,
Windows and Linux.

## Hard constraints

1. **No web technology.** No Tauri, webview, HTML, CSS or JavaScript in the
   app, not even for a prototype. `docs/mockup/main-screen.html` is a static
   design reference only.
2. **UI is GPUI through GPUI Kit.** Depend on `gpui-kit` alone, pinned to an
   exact version (`=x.y.z`). It re-exports GPUI and pins the matching GPUI
   snapshot. Never add `gpui` or a GPUI snapshot crate directly, and never
   loosen the pin: a Kit release can move to a GPUI snapshot with API changes.
3. **Use Kit components before writing your own.** In particular the diff view
   is `gpui_kit::component::diff::Diff`. Do not hand-roll diff rendering,
   virtualization, selection or annotations.
4. **`review-core` has no UI dependency.** It must never depend on `gpui-kit`
   or on `review-app`. The UI reaches the repository only through the
   `ReviewBackend` trait.
5. **Never write to the reviewed repository.** No changes to its objects,
   index, refs, config or working tree. The only files the app writes are
   review files under `<git-dir>/review/`. Scratch objects go to a temporary
   object directory (see `GitCli::combine`).
6. **Never block the UI thread.** Backend calls and file writes run in
   background tasks. Every load has a loading state and discards stale
   results (see the `revision` counter in `app.rs`).
7. **Cross-platform.** No platform-specific code without a fallback for the
   other two. Use Cmd on macOS and Ctrl elsewhere.

## The cheap backend stays cheap

`GitCli` shells out to `git` so the UI can be built against real repositories.
The owner does not want effort spent on the core until the UI and flow have
settled. Fix bugs in it and extend the trait when the UI needs something, but
do not optimise it, add caching layers, or start the `gix` and `imara-diff`
backend unless asked.

When calling git:

- Always pass `--histogram --indent-heuristic` for diffs. That algorithm is a
  product decision.
- Pass revisions so they cannot be read as options (`--end-of-options`, or a
  `--` separator before paths).
- Keep `LC_ALL=C`; error texts are matched.

## Product rules that are easy to break

- **Wording.** The comparison reads "`source` going into `target`" (the
  title bar: project, worktree if any, `/`, then the branches). Never show
  the term "merge-base" in the UI.
- **One scope rules the view.** New for me, Whole branch, or Custom. Per-file
  state never silently overrides the scope.
- **The commit picker is settled.** A dropdown from the range in the scope
  bar; two-row items (title, then author, age and hash); a leading checkbox
  per commit; non-contiguous selections allowed. Do not redesign it or bring
  back a rail, slider or sidebar commit list.
- **Comments attach to the commit that wrote the line**, never to a range or a
  branch tip. Every comment stores the commented lines as a snippet.
- **Reactions are a closed set** of four: Seen, Done, Won't fix, Follow-up.
  No free-form emoji. They sit behind one add-reaction button.
- **A reviewer writes only their own review file.** Reactions to someone
  else's comment go in the reactor's file, by comment id. Never edit, merge or
  rewrite a colleague's file.
- **A "Reviewed" tick stores the file's content id**, not a commit.
- **Density.** Controls are 22 to 26 px high, list rows 22 px, file headers
  28 px, text 12 px. Prefer `xsmall` Kit controls. Do not add padding to make
  things "breathe".
- **Icon-only buttons** for theme and for collapse all and expand all, each
  with a tooltip.
- **Single-letter shortcuts are bound to `Reading > Diff`**, never to a
  window-wide context. They must not fire while the reviewer types a comment.
  The comment box lives inside the Diff, so the `Diff` context alone is not
  enough (see `docs/HANDOFF.md`).

## Decisions and open questions

`docs/design-decisions.md` marks each item Decided, Assumed or Open.

- Do not reverse a Decided item. If it seems wrong, say why and ask.
- An Assumed item is a working default. Flag it when your work depends on it.
- Never settle an Open item silently. Ask, or implement the documented
  default and say that you did.
- When the owner settles something, update the document in the same change.

## Working method

- **UI first.** Changes are judged by how the app feels. Keep the core just
  good enough to feed the UI real data.
- **Small steps the owner can look at.** After a visible change, say what to
  look at and how to reach it.
- **Say what you verified and what you did not.** "It compiles" and "the
  tests pass" are not "it works on screen". If you could not run the window
  or could not test a platform, say so plainly. Never report a UI behaviour
  as working unless you saw it or a test exercised it.
- **Test the core.** Every backend behaviour gets a test against a throwaway
  repository (`crates/core/tests/git_cli.rs`). Review-file rules get unit
  tests.
- **Match the mockup, then improve.** When the app and
  `docs/mockup/main-screen.html` disagree, the mockup wins unless the design
  document says otherwise.

## Commands

```sh
cargo test -p review-core          # fast, no UI
cargo check -p review-app          # first run compiles GPUI: several minutes
cargo run -p review-app -- --target main /path/to/repository
cargo clippy --workspace --all-targets
cargo fmt --all
```

Linux build dependencies are listed in `README.md`.

## Before you finish a change

- `cargo fmt --all`, `cargo clippy --workspace --all-targets` and
  `cargo test -p review-core` are clean.
- `cargo check -p review-app` has no warnings.
- `docs/HANDOFF.md` reflects the new state if you changed what exists or what
  is next.
