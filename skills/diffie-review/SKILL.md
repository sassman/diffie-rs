---
name: diffie-review
description: Drive a code review through the diffie app (native git diff review GUI) and read the review back. Use when asked to review uncommitted changes, a worktree, or a branch before push or merge ("review my changes", "open a review", "review before push/main"), and when asked to read, apply, or address diffie review comments ("address the review", "what did I comment", like a pasted "Copy all as prompt").
---

# diffie review

`diffie` (assume it is on PATH) opens a native review window for a git
comparison: file tree, split/unified diff, line and selection comments,
reviewed ticks. The review itself happens in the app, by the human. This
skill covers two jobs: **launching the right comparison** and **reading the
finished review back** so the comments can be acted on in the session.

## Launching the right comparison

Revisions read in the merge direction, source first; the window shows
"`source` going into `target`". Pick by what the user wants to review:

| Ask | Command |
| --- | --- |
| uncommitted changes (worktree vs checked-out branch) | `diffie` |
| everything the working tree adds over a branch | `diffie main` |
| current branch before pushing into main | `diffie HEAD..main` |
| before pushing, against the remote state | `diffie HEAD..o/main` (remotes are named `o`) |
| one branch into another | `diffie feature..main` (`...` works the same; both resolve the merge base) |
| a single commit | `diffie HEAD..HEAD~1` |
| another project | add `-C /path/to/repo` (never pass the folder as a positional) |

A revision the repository does not have fails before the window opens; a
branch that exactly one remote has is substituted automatically (`o/x`
stands in for `x`, and the review names it), several candidates are listed
and refused.

Launch it detached so the session is not blocked - it is a GUI app:

```bash
diffie -C /path/to/repo HEAD..main >/dev/null 2>&1 &
```

Then stop: tell the user the window is up and that you will read the review
back when they say they are done. Do not poll, do not screenshot, do not try
to drive the app.

## Reading the review back

The app writes review files (TOML, one per reviewer) under
`$(git rev-parse --git-common-dir)/review/<source>/`. Do not parse them -
diffie itself renders them exactly like the app's "Copy all as prompt"
button, without opening a window:

```bash
diffie --prompt -C /path/to/repo
```

- No `--source` needed normally: it picks the most recently touched review
  and names it on stderr. Pass `--source feature`-style names only if the
  user reviewed several comparisons (`--source uncommitted`).
- Only open comments are printed. A Done or Won't fix reaction from any
  reviewer closes a comment; `--all` includes closed ones marked `[resolved]`.
- Output format, same as a pasted prompt:

  ```
  Review comments to address:

  1. src/index/cache.rs:45 at a3f9c1e
  ```(fenced snippet of the commented lines)```
  About `selection`: the comment body
  Suggested replacement:
  ```(fenced replacement)```
  ```

Treat that output as the user's own review feedback: work through the
numbered items like a pasted "Copy all as prompt" message - fix what each
one asks at the given `path:line`, using the snippet to find the spot even
if lines have shifted, and apply `Suggested replacement` blocks verbatim
unless they conflict with something newer. Report per item what was done.

## Rules

- **Never write to the review files or the reviewed repository's git dir.**
  The app owns `<git-common-dir>/review/`; reviewers own their files. Read
  only.
- The comment's hash is the commit that wrote the line (7 chars), not a
  branch tip - do not try to check it out to "see the comment".
- `diffie <rev>` compares the **working tree** (uncommitted edits included)
  against `<rev>`; use the `A..B` form when only committed work should count.
- If `diffie` is not on PATH: `cargo install --path crates/app` from the
  diffie repo, or ask the user.
