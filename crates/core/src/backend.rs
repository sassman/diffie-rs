//! The contract between the UI and whatever reads the repository.
//!
//! Every method blocks. The UI must call them from a background task, never
//! from the render path, and must be able to show a loading state for each.

use crate::model::{
    BlameLine, ChangedRegion, Commit, Comparison, LineOrigin, Oid, Resolved, Scope, ScopedPatch,
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

    /// The reviewer's name as git knows it (`user.name`).
    fn author_name(&self) -> Result<String>;

    /// The directory review files live in: `<git-common-dir>/review`.
    /// It is inside `.git`, so nothing here can be committed by accident.
    fn review_dir(&self) -> Result<std::path::PathBuf>;
}
