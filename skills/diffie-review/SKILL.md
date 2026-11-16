---
name: diffie-review
description: Use when asked to review uncommitted changes, a worktree, a branch or a GitHub pull request before push or merge ("review my changes", "open a review", "review PR 12", "review before push/main"), or to read, address or answer diffie review comments ("address the review", "what did I comment", a pasted "Copy all as prompt").
---

# diffie review

`diffie` is a native review GUI for a git comparison. The human reviews in
the window; you launch the right comparison, then read the comments back
and act on them.

## Launch

Source first: the window shows "`source` going into `target`".

| Review | Command |
| --- | --- |
| uncommitted changes | `diffie` |
| working tree, uncommitted included, into a branch | `diffie main` |
| committed work only, before pushing | `diffie HEAD..main`, or the upstream: `diffie HEAD..origin/main` (use the repo's remote name) |
| a single commit | `diffie HEAD..HEAD~1` |
| a GitHub pull request | `diffie --pr 42` (or its URL; needs `gh`) |
| another repository | add `-C <path>`, never a positional |

**The branch has a PR** (`gh pr view --json number,url` succeeds): offer
both, recommend `--pr` - its window can publish the comments to the PR and
approve it. It shows only the PR's pushed commits; local or unpushed work
needs a row above. Asked for a PR outright: go straight to `--pr`.

It is a GUI, so launch detached and keep the errors:

```bash
diffie --pr 42 >/tmp/diffie.log 2>&1 &
```

A bad revision, or a PR commit that is not fetched, exits at once: read the
log (it names the `git fetch` to run). Otherwise say the window is up and
stop until the user is done. No polling, screenshots or driving the app.

## Read back

```bash
diffie --prompt        # open comments, as "Copy all as prompt"
```

With several reviews it lists them and exits: name one
(`diffie --prompt feature`). `--all` adds resolved comments.

Treat the output as the user's own feedback. Per numbered item: fix it at
`path:line`, locating the spot by the snippet if lines shifted; apply
`Suggested replacement` verbatim unless newer work conflicts; report what
was done. The hash is the commit that wrote the line - not something to
check out.

## Answer

Each header ends in a short id. After addressing an item, or to answer a
question, thread one sentence saying what was done and where (name the
commit):

```bash
diffie --reply '#89fe6a0c' "renamed in 698c1af" --session "<your session id>"
```

- `--session` lets the reviewer resume your conversation from the answer;
  `$CLAUDE_SESSION_ID` is the default.
- `Reply (Claude): ...` lines in `--prompt` output are already given - do
  not repeat them.

## Rules

- Never resolve or mark anything done: that is the human's reaction.
- Never edit review files (`<git-common-dir>/review/`); `--reply` is the
  only write.
- `diffie` missing: `cargo install --path crates/app` in the diffie repo,
  or ask.
