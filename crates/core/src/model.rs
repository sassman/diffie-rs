//! Plain data shared between the backend, the review file and the UI.
//!
//! Nothing in here knows about GPUI or about how git is reached.

use serde::{Deserialize, Serialize};

/// A full git object id in hex. Never abbreviate when storing. The field
/// stays private so every id enters through [`Oid::new`] or serde.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Oid(String);

impl Oid {
    pub fn new(hex: impl Into<String>) -> Self {
        Self(hex.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The seven character form shown in the UI. Display only.
    pub fn short(&self) -> &str {
        // `get` instead of slicing: a malformed id from a hand-edited
        // review file must not panic the UI.
        self.0.get(..7).unwrap_or(&self.0)
    }
}

impl std::fmt::Display for Oid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What the reviewer compares, phrased the way the UI phrases it:
/// "changes in `source` going into `target`".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Comparison {
    /// The branch (or any revision) whose changes are reviewed.
    pub source: String,
    /// The branch the changes are going into.
    pub target: String,
    /// Resolve this revision instead of `source`, which then only names the
    /// review in the UI and on disk. A pull-request review pins the PR's
    /// exact head commit here so a stale same-named branch can never show
    /// other content, while the title and the review file keep the branch
    /// name.
    pub source_rev: Option<String>,
    /// Resolve this revision instead of `target`; see `source_rev`.
    pub target_rev: Option<String>,
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
    /// With `uncommitted`, the working tree's uncommitted changes count as
    /// one more pickable layer on top; the flag only means something when
    /// [`Comparison::uncommitted`] is set.
    Commits { picked: Vec<Oid>, uncommitted: bool },
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
    /// Ticked and identical since, but carrying open comments: the diff
    /// stays in the patch so the comments have rows to sit on. An open
    /// comment is unfinished business, whatever the tick says.
    KeptForComments,
    /// A skipped commit touched the same lines, so the checked commits could
    /// not be combined for this file. `commit` is the first one that failed.
    /// The file's diff in the patch stops before that commit.
    CouldNotCombine { commit: Oid },
    /// The working tree's uncommitted changes could not be laid on top of
    /// the checked commits (a skipped commit touches the same lines). The
    /// file's diff in the patch stops at the last checked commit.
    UncommittedNotCombined,
    /// In no comparison at all: the entry is synthesized from its open
    /// comments' stored snippets ([`orphan_patches`](crate::review::orphan_patches))
    /// so the threads stay readable and disposable.
    NotInComparison,
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

/// One region a diff between two revisions of a file rewrites:
/// `old_start..old_start+old_lines` of the old revision became
/// `new_start..new_start+new_lines` of the new one. Zero-length sides follow
/// the unified-diff convention: a pure insertion has `old_lines == 0` with
/// `old_start` naming the line it comes after, a pure deletion the mirror.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChangedRegion {
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
}

/// Whether the repository sits in an interrupted merge or rebase. Polled on
/// load and reload only; while not [`RepoState::Idle`] the UI offers the
/// conflict view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepoState {
    Idle,
    Merge,
    /// `done` is the 1-based number of the commit the rebase stopped on,
    /// `total` how many it set out to replay: "commit `done` of `total`".
    Rebase {
        done: usize,
        total: usize,
    },
}

/// One unmerged path of an interrupted merge or rebase, with git's sides
/// already mapped to the review's semantic names: `yours` is always the
/// reviewer's own work, `upstream` what it collides with, whichever stage
/// git stored each in. The mapping lives in the backend, nowhere else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConflictFile {
    pub path: String,
    /// The common ancestor's text. Missing when both sides added the path.
    pub base: Option<String>,
    /// The reviewer's side. Missing when their side deleted the path.
    pub yours: Option<String>,
    /// The side being merged in or rebased onto. Missing when it deleted
    /// the path.
    pub upstream: Option<String>,
    /// The marker-laden working-tree text, empty when the file is gone.
    /// Markers carry generic side labels, so they are display material
    /// only; the texts and labels above are what resolution trusts.
    pub working: String,
    /// Some side's bytes are not valid UTF-8, so the texts above are lossy
    /// decodes: fine to show, never to write back as a resolution.
    pub binary: bool,
    pub labels: ConflictLabels,
}

/// What the conflict panes are suffixed with. The strings "ours" and
/// "theirs" never appear: a merge names the two branches, a rebase names
/// the upstream and the replayed commit's title.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConflictLabels {
    pub yours: String,
    pub upstream: String,
    /// When the reviewer's tip was committed, on the committer's own
    /// clock ("2026-11-17 14:02"). The yours pane anchors the comparison
    /// with this absolute date; the upstream pane is phrased relative to
    /// it from the two times below. Empty when the tip could not be read.
    pub yours_date: String,
    /// The two tips' commit times, unix seconds; 0 when unreadable.
    pub yours_time: i64,
    pub upstream_time: i64,
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
