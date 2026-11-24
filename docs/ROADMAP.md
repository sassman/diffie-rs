# Roadmap

Two tracks: committed to deliver soon, and ideas to explore with no
commitment. Deferred is an archive, maybe revisited later.

## Committed to deliver

- Check for a new release and surface it in the UI so an upgrade is hard to
  miss: an "Install and restart" button that fetches and swaps the binary,
  then relaunches. Open: where the notice sits (title bar vs. a corner
  toast), the check cadence and the release source (GitHub releases likely),
  and whether install is a self-replace or hands off to the OS's package
  manager where the app came from one.

## Ideas to explore

- Create a PR from the app: a prefilled dialog (title, body, target branch,
  remote when there are several), drafted by AI, the preferences tweakable
  to one's own style. Open: a plain LLM call with own instrumentation, or
  wrapping an agent call - low token cost against high output quality.
  Typefully's "Condense" and "Make it punchier" buttons on the text fields
  might be an idea.
- Realtime agent interaction: writing a comment triggers the agent directly,
  and the app signals when it is answered or changes are there to reload.
  Spawning a fresh agent session raises which model and prompt it uses -
  likely configurable.
- Git basics through the palette: commit reviewed files that are
  uncommitted; push a branch, choosing the remote when there are several -
  the PR workflow needs the push anyway.
- Change the review's source, target or PR from within the app, UX like
  Zed's branch and worktree switcher. The PR flavour already shipped as the
  palette's PR picker (#7).
- PR context: show which comments are not yet published, per comment and in
  an overview beside the open-comment count.
- Image preview for png, svg and bmp files in the diff, instead of (or
  alongside) the binary-file placeholder. Open: side-by-side old/new, an
  overlay/toggle, or both; how this interacts with Kit's `Diff` component
  since it is not text.
- Video files in the diff get a play affordance (a player-symbol button)
  that hands the file to the OS's registered handler, rather than any
  in-app playback. Open: where that button sits in the file row/header.
- Commit signature status shown in the commit picker: signed tends
  green-ish, unsigned tends yellow/orange, a bad signature tends red.
  Open: the exact colors, clear of the comment-status colors already in
  use; where in the two-row item it sits; whether theme light/dark both
  keep the signal readable.
- A `--stack <no>` CLI arg: pull together all the PRs in a stack and show
  their commits grouped by PR in the commit picker, so a whole stack entry
  (all its commits) can be unselected or reviewed together. Likely needs a
  top-bar button next to the PR button for stack info, with navigation from
  there to the individual PRs. The palette gets an "Open Stack of PRs"
  command listing GitHub's stacked PRs with their status, in the same
  professional look as the open-PR list. Open: how a stack is detected
  (GitHub's native stacking, a naming convention, or both).

## Deferred

- Nothing yet.
