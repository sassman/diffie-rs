//! The contract between the UI and whatever reads the repository.
//!
//! Every method blocks. The UI must call them from a background task, never
//! from the render path, and must be able to show a loading state for each.

use crate::model::{BlameLine, Commit, Comparison, LineOrigin, Oid, Resolved, Scope, ScopedPatch};
use crate::review::ReviewedFile;

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

    /// The commits (`base..head`, newest first like [`Self::commits`]) that
    /// touched each path. Paths are the names each commit used, so a file
    /// renamed inside the range appears under its old name for the commits
    /// before the rename, next to the name [`Self::changed_paths`] reports.
    fn commits_touching(
        &self,
        resolved: &Resolved,
    ) -> Result<std::collections::HashMap<String, Vec<Oid>>>;

    /// The diff for one scope. `reviewed` is only read for [`Scope::NewForMe`].
    fn patch(
        &self,
        resolved: &Resolved,
        scope: &Scope,
        reviewed: &[ReviewedFile],
    ) -> Result<ScopedPatch>;

    /// The commit that wrote `line` (1-based) of `path` as of `rev`.
    /// `None` when the line does not exist.
    fn line_origin(&self, rev: &Oid, path: &str, line: usize) -> Result<Option<LineOrigin>>;

    /// Blame for every line of `path` as of `rev`. Used to place comments
    /// from other revisions on lines that are still unchanged.
    fn blame(&self, rev: &Oid, path: &str) -> Result<Vec<BlameLine>>;

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

    /// The reviewer's name as git knows it (`user.name`).
    fn author_name(&self) -> Result<String>;

    /// The directory review files live in: `<git-common-dir>/review`.
    /// It is inside `.git`, so nothing here can be committed by accident.
    fn review_dir(&self) -> Result<std::path::PathBuf>;
}
