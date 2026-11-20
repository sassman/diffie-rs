# Roadmap

Two tracks: committed to deliver soon, and ideas to explore with no
commitment. Deferred is an archive, maybe revisited later.

## Committed to deliver

- Nothing right now.

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

## Deferred

- Nothing yet.
