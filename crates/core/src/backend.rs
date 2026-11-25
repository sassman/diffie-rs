//! The contract between the UI and whatever reads the repository.
//!
//! Every method blocks. The UI must call them from a background task, never
//! from the render path, and must be able to show a loading state for each.

use crate::model::{
    BlameLine, ChangedRegion, Commit, Comparison, ConflictFile, ConflictLabels, LineOrigin, Oid,
    PredictedConflict, RepoState, ResolutionPush, Resolved, Scope, ScopedPatch,
};
use crate::review::ReviewInput;

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("git is not available: {0}")]
    GitMissing(std::io::Error),
    #[error("not a git repository: {0}")]
    NotARepository(String),
    #[error("unknown revision `{0}`")]
    UnknownRevision(String),
    #[error("`{target}` and `{source_rev}` share no history")]
    NoMergeBase { source_rev: String, target: String },
    #[error("git {command} failed: {stderr}")]
    Git { command: String, stderr: String },
    #[error("no merge or rebase is in progress")]
    NoOperation,
    #[error("`{0}` has no conflict to resolve")]
    NotConflicted(String),
    #[error("the merge still conflicts on {0}; resolve every path before pushing")]
    UnresolvedPaths(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, BackendError>;

pub trait ReviewBackend: Send + Sync {
    /// Resolve branch names to commits and find where the diff starts.
    fn resolve(&self, comparison: &Comparison) -> Result<Resolved>;

    /// The comparison's commits (`base..head`), newest first.
    fn commits(&self, resolved: &Resolved) -> Result<Vec<Commit>>;

    /// Every path the comparison changes (`base..head`), regardless of the
    /// scope shown: a review is complete only when all of these are ticked,
    /// so judging completion against the scope's files would be wrong.
    /// A renamed file is listed once, under its path at `head`, like
    /// [`FileSummary`](crate::model::FileSummary).
    fn changed_paths(&self, resolved: &Resolved) -> Result<Vec<String>>;

    /// The paths whose working-tree content differs from `head`: the files
    /// the uncommitted layer of an uncommitted comparison touches. Empty
    /// when the comparison reviews committed revisions.
    fn uncommitted_paths(&self, resolved: &Resolved) -> Result<Vec<String>>;

    /// The commits (`base..head`, newest first like [`Self::commits`]) that
    /// touched each path. Paths are the names each commit used, so a file
    /// renamed inside the range appears under its old name for the commits
    /// before the rename, next to the name [`Self::changed_paths`] reports.
    fn commits_touching(
        &self,
        resolved: &Resolved,
    ) -> Result<std::collections::HashMap<String, Vec<Oid>>>;

    /// Which of `candidates` are reachable from `rev`, in the order given.
    /// One call however long the list, so labelling a stack's commits by
    /// ancestry costs one call per member rather than one per commit.
    fn contains_commits(&self, rev: &str, candidates: &[Oid]) -> Result<Vec<Oid>>;

    /// The diff for one scope. `review` is only read for [`Scope::NewForMe`]:
    /// its ticks are what each file diffs against, and a ticked-unchanged
    /// file among its commented paths keeps its diff instead of collapsing,
    /// because the comments need rows to sit on and judging a fix is still
    /// work. With `ignore_whitespace` the diff hides changes in amount of
    /// whitespace; a file whose changes are all whitespace drops out of the
    /// patch entirely, matching `git diff --ignore-all-space`.
    fn patch(
        &self,
        resolved: &Resolved,
        scope: &Scope,
        review: ReviewInput<'_>,
        ignore_whitespace: bool,
    ) -> Result<ScopedPatch>;

    /// The commit that wrote `line` (1-based) of `path` as of `rev`.
    /// `None` when the line does not exist.
    fn line_origin(&self, rev: &Oid, path: &str, line: usize) -> Result<Option<LineOrigin>>;

    /// Blame for every line of `path` as of `rev`. Used to place comments
    /// from other revisions on lines that are still unchanged.
    fn blame(&self, rev: &Oid, path: &str) -> Result<Vec<BlameLine>>;

    /// The regions `git diff from to -- path` rewrites, for following a
    /// comment's line into a newer revision once blame no longer finds it.
    /// `None` when git cannot answer - a rebase may have dropped `from` -
    /// and the caller then leaves the comment unplaced rather than guessing.
    fn changed_regions(
        &self,
        from: &Oid,
        to: &Oid,
        path: &str,
    ) -> Result<Option<Vec<ChangedRegion>>>;

    /// The content id of `path` at `rev`, or `None` if it does not exist
    /// there. A "reviewed" tick stores this.
    fn blob_id(&self, rev: &Oid, path: &str) -> Result<Option<Oid>>;

    /// The content id of every file at `rev`, in one pass. A large commit
    /// changes thousands of files, and one [`Self::blob_id`] subprocess per
    /// file turns a fast load into a near-minute stall.
    fn blob_ids(&self, rev: &Oid) -> Result<std::collections::HashMap<String, Oid>>;

    /// The content id each of `paths` would get if committed from the
    /// working tree, in one pass. A path missing on disk is absent from the
    /// map, so a deletion reads the same as in [`Self::blob_ids`]. This is
    /// what an uncommitted review ticks against instead of a tree.
    fn worktree_blob_ids(&self, paths: &[String])
    -> Result<std::collections::HashMap<String, Oid>>;

    /// The content of each of `ids`, in one pass. An id git does not hold,
    /// or one that names no blob, is absent from the map. Symbol navigation
    /// reads a whole tree's sources through this, so one subprocess per
    /// file is not an option.
    fn blobs(&self, ids: &[Oid]) -> Result<std::collections::HashMap<Oid, Vec<u8>>>;

    /// The working tree's content of `path`, or `None` when it is not a
    /// file there. The uncommitted side of an uncommitted review reads
    /// sources through this, since they are in no object yet.
    fn worktree_file(&self, path: &str) -> Result<Option<Vec<u8>>>;

    /// Whether `revision` names a commit here. Rejects a typo before a
    /// window opens on an empty review, and tells an unfetched pull
    /// request from a fetched one.
    fn revision_exists(&self, revision: &str) -> Result<bool>;

    /// Whether the working tree holds anything an uncommitted review
    /// would show: changes against HEAD, staged or not, and untracked
    /// files. Decides the bare invocation's default comparison.
    fn has_uncommitted_changes(&self) -> Result<bool>;

    /// The repository's default branch as the clone recorded it: the
    /// branch the remotes' HEAD points at (`refs/remotes/<remote>/HEAD`),
    /// when the remotes that say agree on one name. `None` otherwise - a
    /// remote added by hand records no HEAD, and git is never asked over
    /// the network for one.
    fn default_branch(&self) -> Result<Option<String>>;

    /// The configured remotes, as `git remote` lists them. Names a remote
    /// in fetch hints without guessing.
    fn remotes(&self) -> Result<Vec<String>>;

    /// The configured remotes whose fetched refs carry `branch`. Stands
    /// `o/x` in for `x` when exactly one remote knows the branch, and
    /// names the candidates when several do.
    fn remotes_with_branch(&self, branch: &str) -> Result<Vec<String>>;

    /// Fetches `refspecs` from `remote`. The one sanctioned write to the
    /// repository besides the review files: objects and remote-tracking
    /// refs, on the reviewer's explicit action only - opening a pull
    /// request whose commits are not here yet. Never touches local
    /// branches, the index or the working tree.
    fn fetch(&self, remote: &str, refspecs: &[String]) -> Result<()>;

    /// Whether the repository sits in an interrupted merge or rebase.
    /// Read per worktree: a merge in one checkout leaves the others idle.
    fn repo_state(&self) -> Result<RepoState>;

    /// The checked-out branch, `None` on a detached HEAD. The UI needs it
    /// where a comparison's source names no revision (an uncommitted
    /// review) but the work still has a committed tip to reason from.
    fn current_branch(&self) -> Result<Option<String>>;

    /// Every unmerged path of the interrupted operation, sides already
    /// mapped to yours/upstream and labelled for display. Empty when
    /// [`Self::repo_state`] is idle.
    fn conflicts(&self) -> Result<Vec<ConflictFile>>;

    /// The paths a merge of `source` into `target` would collide on,
    /// each with git's auto-merged text, markers included. Purely
    /// predictive: merge objects go to a scratch directory and the
    /// repository, its index and its working tree stay untouched. Empty
    /// means the merge would be clean.
    ///
    /// The names feed git's marker labels, so each side is merged under
    /// its name - unless its pin says the name has moved on: a review
    /// pinned to a startup or pull-request revision must forecast that
    /// revision, not whatever a local branch of the same name points at
    /// today.
    fn predicted_conflicts(
        &self,
        source: &str,
        source_pin: Option<&str>,
        target: &str,
        target_pin: Option<&str>,
    ) -> Result<Vec<PredictedConflict>>;

    /// The labels a forecast's resolver panes carry, in the shape an
    /// interrupted operation's labels have: yours is the source side,
    /// upstream the target. The names are display material (a branch
    /// that only exists on the forge still labels its pane); the clocks
    /// come from the revisions, which must resolve here.
    fn forecast_labels(
        &self,
        source: &str,
        source_rev: &str,
        target: &str,
        target_rev: &str,
    ) -> Result<ConflictLabels>;

    /// Build the merge commit that resolves a pull request's predicted
    /// conflicts and push it to the pull request's head branch. The
    /// second sanctioned write family (owner decision, 2026-11-20,
    /// design document section 20): blobs, trees and one commit enter
    /// the object database - no local ref, index or working-tree change -
    /// and the remote branch moves only by this explicitly confirmed
    /// push, never forced. Returns the pushed commit.
    fn push_resolution(&self, push: &ResolutionPush) -> Result<Oid>;

    /// Write `contents` as the resolution of `path` and stage it. The one
    /// sanctioned repository write: only a path that is unmerged in the
    /// index may be written, anything else is [`BackendError::NotConflicted`].
    /// Full text crosses this boundary, never a patch.
    fn resolve_file(&self, path: &str, contents: &str) -> Result<()>;

    /// Put the conflict markers back for `path`, even after it was staged
    /// by [`Self::resolve_file`]. The regenerated markers carry generic
    /// side labels, so callers keep trusting [`ConflictFile`], not them.
    fn unresolve_file(&self, path: &str) -> Result<()>;

    /// Continue the interrupted merge or rebase. An error carries git's
    /// stderr verbatim; in an idle repository it is
    /// [`BackendError::NoOperation`].
    fn continue_op(&self) -> Result<()>;

    /// Abort the interrupted merge or rebase, discarding resolutions. The
    /// same errors as [`Self::continue_op`].
    fn abort_op(&self) -> Result<()>;

    /// The reviewer's name as git knows it (`user.name`).
    fn author_name(&self) -> Result<String>;

    /// The directory review files live in: `<git-common-dir>/review`.
    /// It is inside `.git`, so nothing here can be committed by accident.
    fn review_dir(&self) -> Result<std::path::PathBuf>;
}
