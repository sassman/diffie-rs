//! Plain data shared between the backend, the review file and the UI.
//!
//! Nothing in here knows about GPUI or about how git is reached.

use serde::{Deserialize, Serialize};

/// A full git object id in hex. Never abbreviate when storing.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Oid(pub String);

impl Oid {
    pub fn new(hex: impl Into<String>) -> Self {
        Self(hex.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The seven character form shown in the UI. Display only.
    pub fn short(&self) -> &str {
        &self.0[..self.0.len().min(7)]
    }
}

impl std::fmt::Display for Oid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What the reviewer compares, phrased the way the UI phrases it:
/// "changes in `source` going into `target`".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comparison {
    /// The branch (or any revision) whose changes are reviewed.
    pub source: String,
    /// The branch the changes are going into.
    pub target: String,
    pub mode: CompareMode,
    /// Review the working tree's uncommitted changes instead of resolving
    /// `source`, which then only names the review in the UI and on disk.
    pub uncommitted: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CompareMode {
    /// Only what `source` adds: diff from the merge-base to `source`.
    /// Newer work on `target` is ignored. The default.
    #[default]
    BranchAdds,
    /// Every difference between the two tips. (Open design question whether
    /// the UI exposes this; the backend supports it either way.)
    EveryDifference,
}

/// A [`Comparison`] resolved against the repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub comparison: Comparison,
    /// Tip of `source`.
    pub head: Oid,
    /// Tip of `target`.
    pub target_head: Oid,
    /// Where the diff starts: the merge-base, or `target_head` for
    /// [`CompareMode::EveryDifference`].
    pub base: Oid,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub id: Oid,
    pub subject: String,
    pub author: String,
    /// Author date, unix seconds.
    pub time: i64,
}

/// Which part of the comparison is shown. One scope rules the whole view;
/// per-file state never overrides it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Each file compared with the version the reviewer last ticked as
    /// reviewed. Unticked files show their full diff; ticked files with no
    /// changes are listed as [`FileState::UnchangedSinceReview`].
    NewForMe,
    /// `base..head`, the big picture.
    WholeBranch,
    /// Any subset of the comparison's commits. May skip commits in between.
    Commits(Vec<Oid>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Deleted,
    Modified,
    Renamed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileState {
    /// Has a diff in [`ScopedPatch::text`].
    Changed,
    /// Ticked as reviewed and identical since. No diff in the patch.
    UnchangedSinceReview,
    /// A skipped commit touched the same lines, so the checked commits could
    /// not be combined for this file. `commit` is the first one that failed.
    /// The file's diff in the patch stops before that commit.
    CouldNotCombine { commit: Oid },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSummary {
    /// Path on the new side (old side for deletions). Identifies the file.
    pub path: String,
    pub status: FileStatus,
    pub additions: usize,
    pub deletions: usize,
    pub state: FileState,
    pub binary: bool,
}

/// What line numbers on the new side of a patch refer to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewSide {
    /// A real commit: lines can be blamed.
    Commit(Oid),
    /// Lines that exist in no commit: a tree built by combining
    /// non-contiguous commits, or the working tree's uncommitted state.
    /// Blame is not available (see docs/design-decisions.md, open
    /// questions).
    Synthetic,
}

/// A diff for one [`Scope`], as git-format unified patch text.
///
/// Patch text is the hand-over format on purpose: GPUI Kit's `Diff` component
/// parses exactly this, and both the cheap backend and a future `gix` +
/// `imara-diff` backend can produce it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopedPatch {
    pub text: String,
    pub files: Vec<FileSummary>,
    pub new_side: NewSide,
}

/// The commit that wrote a line, and the line's number in that commit's
/// version of the file. This pair is what a comment is anchored to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineOrigin {
    pub commit: Oid,
    pub line: usize,
}

/// One row of a whole-file blame: line `final_line` of the blamed revision
/// comes from `origin`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlameLine {
    pub final_line: usize,
    pub origin: LineOrigin,
}
