//! The review file: one TOML file per reviewer per reviewed branch.
//!
//! Rules that keep sharing conflict-free (see docs/design-decisions.md):
//! - A reviewer only ever writes their own file.
//! - A reaction to someone else's comment lives in the reactor's file and
//!   points at the comment's id.
//! - Several files are overlaid read-only by [`ReviewSet`].

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::{BlameLine, ChangedRegion, Oid, Resolved};
use crate::time::now_rfc3339;

pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum ReviewFileError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("{path} is format version {found}; this build reads version {FORMAT_VERSION}")]
    Version { path: PathBuf, found: u32 },
    #[error("could not write the review file: {0}")]
    Serialize(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewFile {
    pub version: u32,
    pub review: Header,
    /// The last time every changed file was ticked. "New since your review"
    /// means the commits after `completed.head`. Absent until the first
    /// complete pass: ticking only some files sets no marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed: Option<Completed>,
    #[serde(default, rename = "reviewed", skip_serializing_if = "Vec::is_empty")]
    pub reviewed: Vec<ReviewedFile>,
    #[serde(default, rename = "comment", skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<Comment>,
    #[serde(default, rename = "reaction", skip_serializing_if = "Vec::is_empty")]
    pub reactions: Vec<Reaction>,
    #[serde(default, rename = "reply", skip_serializing_if = "Vec::is_empty")]
    pub replies: Vec<Reply>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    /// `user.name` of the reviewer. Shown on every comment.
    pub author: String,
    pub source: String,
    pub target: String,
    /// Tip of `source` when the file was last written. If the branch no longer
    /// contains it, the UI warns that the branch has moved since this review.
    pub head: Oid,
    pub base: Oid,
    pub updated: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Completed {
    /// Tip of `source` when the last file was ticked.
    pub head: Oid,
    pub at: String,
}

/// A "Reviewed" tick. It holds the file's content id, so it clears itself
/// when the file changes again and survives rebases that leave the file alone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewedFile {
    pub path: String,
    /// Absent for a deleted file: the deletion itself is the reviewed
    /// content, so a re-added file reads as changed since.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<Oid>,
    /// Tip of `source` at the time of the tick. Informational.
    pub commit: Oid,
    pub at: String,
}

/// What of the reviewer's own review building a patch may read: the ticks
/// New for me diffs against, and the paths whose open comments keep a
/// ticked file visible. Other scopes read neither - `EMPTY` says exactly
/// that at a call site, where a pair of bare `&[]` said nothing.
#[derive(Clone, Copy)]
pub struct ReviewInput<'a> {
    pub reviewed: &'a [ReviewedFile],
    pub commented: &'a [String],
}

impl ReviewInput<'static> {
    pub const EMPTY: Self = Self {
        reviewed: &[],
        commented: &[],
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Old,
    New,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    /// Random, stable, unique across reviewers.
    pub id: String,
    /// The commit that wrote the commented line. Never a range or branch tip.
    pub commit: Oid,
    pub path: String,
    pub side: Side,
    /// Last commented line, numbered in `commit`'s version of the file.
    pub line: usize,
    /// How many lines the comment covers, ending at `line`. At least 1.
    #[serde(default = "one")]
    pub lines: usize,
    /// The exact text selected inside the line, for a sub-line comment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<String>,
    /// The commented lines, verbatim. Keeps the comment readable after a
    /// rebase removes `commit`.
    pub snippet: String,
    pub body: String,
    /// Replacement text for the commented lines, if this is a suggestion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
    pub created: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited: Option<String>,
}

fn one() -> usize {
    1
}

/// The agreed, closed set of reactions. Do not add free-form emoji.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReactionKind {
    Seen,
    Done,
    WontFix,
    FollowUp,
}

impl ReactionKind {
    pub const ALL: [ReactionKind; 4] = [Self::Seen, Self::Done, Self::WontFix, Self::FollowUp];

    pub fn emoji(self) -> &'static str {
        match self {
            Self::Seen => "👀",
            Self::Done => "✅",
            Self::WontFix => "🚫",
            Self::FollowUp => "📌",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Seen => "Seen",
            Self::Done => "Done",
            Self::WontFix => "Won't fix",
            Self::FollowUp => "Follow-up",
        }
    }

    /// Done and Won't fix close a comment; Seen and Follow-up keep it open.
    pub fn resolves(self) -> bool {
        matches!(self, Self::Done | Self::WontFix)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reaction {
    /// Id of the comment, which may live in another reviewer's file.
    pub comment: String,
    pub kind: ReactionKind,
    pub at: String,
}

/// A threaded answer under a comment. Like a reaction it lives in the
/// replier's own file and points at the comment's id, so an agent or a
/// colleague answers without touching the author's file. Replies are
/// conversation, never verdict: only a reaction resolves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    /// Id of the comment, which may live in another reviewer's file.
    pub comment: String,
    pub body: String,
    pub at: String,
    /// The agent conversation the answer came from, when one did: the UI
    /// offers it as `claude --resume <id>` so the reviewer can dig into
    /// the reasoning behind the answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewState {
    NotReviewed,
    Reviewed,
    /// Ticked earlier, but the file has changed since.
    ChangedSince,
}

/// Everything needed to create a comment.
#[derive(Clone, Debug)]
pub struct NewComment {
    pub commit: Oid,
    pub path: String,
    pub side: Side,
    pub line: usize,
    pub lines: usize,
    pub selection: Option<String>,
    pub snippet: String,
    pub body: String,
    pub suggestion: Option<String>,
}

impl ReviewFile {
    pub fn new(author: impl Into<String>, resolved: &Resolved) -> Self {
        Self {
            version: FORMAT_VERSION,
            review: Header {
                author: author.into(),
                source: resolved.comparison.source.clone(),
                target: resolved.comparison.target.clone(),
                head: resolved.head.clone(),
                base: resolved.base.clone(),
                updated: now_rfc3339(),
            },
            completed: None,
            reviewed: Vec::new(),
            comments: Vec::new(),
            reactions: Vec::new(),
            replies: Vec::new(),
        }
    }

    pub fn load(path: &Path) -> Result<Self, ReviewFileError> {
        let text = std::fs::read_to_string(path)?;
        let file: Self = toml::from_str(&text).map_err(|error| ReviewFileError::Parse {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
        if file.version != FORMAT_VERSION {
            return Err(ReviewFileError::Version {
                path: path.to_path_buf(),
                found: file.version,
            });
        }
        Ok(file)
    }

    /// Write atomically: a crash never leaves a half-written review.
    pub fn save(&mut self, path: &Path) -> Result<(), ReviewFileError> {
        self.review.updated = now_rfc3339();
        let text = toml::to_string_pretty(self)
            .map_err(|error| ReviewFileError::Serialize(error.to_string()))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("toml.tmp");
        std::fs::write(&temporary, text)?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    }

    /// `<review_dir>/<source>/<author>.toml`, with both names made file-safe.
    pub fn path_in(review_dir: &Path, source: &str, author: &str) -> PathBuf {
        review_dir
            .join(slug(source))
            .join(format!("{}.toml", slug(author)))
    }

    /// Record a new head after the branch moved or the app refreshed.
    pub fn set_head(&mut self, resolved: &Resolved) {
        self.review.head = resolved.head.clone();
        self.review.base = resolved.base.clone();
    }

    pub fn review_state(&self, path: &str, current_blob: Option<&Oid>) -> ReviewState {
        match self.reviewed.iter().find(|tick| tick.path == path) {
            None => ReviewState::NotReviewed,
            Some(tick) if tick.blob.as_ref() == current_blob => ReviewState::Reviewed,
            Some(_) => ReviewState::ChangedSince,
        }
    }

    /// Tick a file as reviewed at its current content, replacing any older
    /// tick. `None` ticks a deleted file.
    pub fn mark_reviewed(&mut self, path: &str, blob: Option<Oid>, head: Oid) {
        self.reviewed.retain(|tick| tick.path != path);
        self.reviewed.push(ReviewedFile {
            path: path.to_string(),
            blob,
            commit: head,
            at: now_rfc3339(),
        });
    }

    pub fn clear_reviewed(&mut self, path: &str) {
        self.reviewed.retain(|tick| tick.path != path);
    }

    /// Call after a tick with the current content id of every changed file.
    /// Records a completed review when all of them are ticked at that content,
    /// and returns whether it did.
    pub fn complete_if_all_reviewed<'a>(
        &mut self,
        files: impl IntoIterator<Item = (&'a str, Option<&'a Oid>)>,
        head: &Oid,
    ) -> bool {
        let mut any = false;
        for (path, blob) in files {
            any = true;
            if self.review_state(path, blob) != ReviewState::Reviewed {
                return false;
            }
        }
        if any {
            self.completed = Some(Completed {
                head: head.clone(),
                at: now_rfc3339(),
            });
        }
        any
    }

    /// Drop a `completed` marker the branch has outgrown: new commits can
    /// change more files, or change a ticked file again. Takes the same
    /// file list as [`Self::complete_if_all_reviewed`] and returns whether
    /// the marker was cleared.
    pub fn verify_completed<'a>(
        &mut self,
        files: impl IntoIterator<Item = (&'a str, Option<&'a Oid>)>,
    ) -> bool {
        if self.completed.is_none() {
            return false;
        }
        let mut any = false;
        let all_reviewed = files.into_iter().all(|(path, blob)| {
            any = true;
            self.review_state(path, blob) == ReviewState::Reviewed
        });
        if all_reviewed && any {
            return false;
        }
        self.completed = None;
        true
    }

    pub fn add_comment(&mut self, new: NewComment) -> String {
        let id = new_id();
        self.comments.push(Comment {
            id: id.clone(),
            commit: new.commit,
            path: new.path,
            side: new.side,
            line: new.line,
            lines: new.lines.max(1),
            selection: new.selection,
            snippet: new.snippet,
            body: new.body,
            suggestion: new.suggestion,
            created: now_rfc3339(),
            edited: None,
        });
        id
    }

    /// Returns false when the comment is not in this file (it is someone
    /// else's, and must not be edited from here).
    pub fn edit_comment(&mut self, id: &str, body: String) -> bool {
        match self.comments.iter_mut().find(|comment| comment.id == id) {
            Some(comment) => {
                comment.body = body;
                comment.edited = Some(now_rfc3339());
                true
            }
            None => false,
        }
    }

    /// Deletes one of the reviewer's own comments and their reactions to it.
    pub fn delete_comment(&mut self, id: &str) -> bool {
        let before = self.comments.len();
        self.comments.retain(|comment| comment.id != id);
        self.reactions.retain(|reaction| reaction.comment != id);
        self.comments.len() != before
    }

    /// Add the reaction, or remove it if this reviewer already gave it.
    /// Returns true when the reaction is now present.
    pub fn toggle_reaction(&mut self, comment_id: &str, kind: ReactionKind) -> bool {
        let existing = self
            .reactions
            .iter()
            .position(|reaction| reaction.comment == comment_id && reaction.kind == kind);
        match existing {
            Some(index) => {
                self.reactions.remove(index);
                false
            }
            None => {
                self.reactions.push(Reaction {
                    comment: comment_id.to_string(),
                    kind,
                    at: now_rfc3339(),
                });
                true
            }
        }
    }

    /// Thread an answer under a comment, which may live in another
    /// reviewer's file.
    pub fn add_reply(
        &mut self,
        comment_id: &str,
        body: impl Into<String>,
        session: Option<String>,
    ) {
        self.replies.push(Reply {
            comment: comment_id.to_string(),
            body: body.into(),
            at: now_rfc3339(),
            session,
        });
    }

    /// Reactions this reviewer has not given to the comment yet: what the
    /// picker offers.
    pub fn reactions_left(&self, comment_id: &str) -> Vec<ReactionKind> {
        ReactionKind::ALL
            .into_iter()
            .filter(|kind| {
                !self
                    .reactions
                    .iter()
                    .any(|reaction| reaction.comment == comment_id && reaction.kind == *kind)
            })
            .collect()
    }
}

/// A comment together with who wrote it, every reaction and every reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thread<'a> {
    pub author: &'a str,
    pub comment: &'a Comment,
    /// `(reactor, kind)`, across all loaded files.
    pub reactions: Vec<(&'a str, ReactionKind)>,
    /// `(replier, reply)`, across all loaded files, oldest first.
    pub replies: Vec<(&'a str, &'a Reply)>,
    /// True when the comment is in the reviewer's own file and may be edited.
    pub mine: bool,
}

impl Thread<'_> {
    pub fn resolved(&self) -> bool {
        self.reactions.iter().any(|(_, kind)| kind.resolves())
    }

    /// Someone other than the comment's author replied. The question has an
    /// answer to read, which is not the same as being resolved.
    pub fn answered(&self) -> bool {
        self.replies
            .iter()
            .any(|(replier, _)| *replier != self.author)
    }

    pub fn count(&self, kind: ReactionKind) -> usize {
        self.reactions.iter().filter(|(_, k)| *k == kind).count()
    }
}

/// The reviewer's own file plus read-only files from colleagues.
#[derive(Clone, Debug)]
pub struct ReviewSet {
    pub mine: ReviewFile,
    pub others: Vec<ReviewFile>,
}

impl ReviewSet {
    pub fn new(mine: ReviewFile) -> Self {
        Self {
            mine,
            others: Vec::new(),
        }
    }

    /// Overlay a colleague's file. A file by the same author replaces the
    /// earlier one; the reviewer's own name is refused.
    pub fn add_other(&mut self, file: ReviewFile) -> bool {
        if file.review.author == self.mine.review.author {
            return false;
        }
        self.others
            .retain(|other| other.review.author != file.review.author);
        self.others.push(file);
        true
    }

    fn files(&self) -> impl Iterator<Item = (&ReviewFile, bool)> {
        std::iter::once((&self.mine, true)).chain(self.others.iter().map(|file| (file, false)))
    }

    pub fn threads(&self) -> Vec<Thread<'_>> {
        let mut threads = Vec::new();
        for (file, mine) in self.files() {
            for comment in &file.comments {
                let reactions = self
                    .files()
                    .flat_map(|(reactor, _)| {
                        reactor
                            .reactions
                            .iter()
                            .filter(|reaction| reaction.comment == comment.id)
                            .map(|reaction| (reactor.review.author.as_str(), reaction.kind))
                    })
                    .collect();
                let mut replies: Vec<(&str, &Reply)> = self
                    .files()
                    .flat_map(|(replier, _)| {
                        replier
                            .replies
                            .iter()
                            .filter(|reply| reply.comment == comment.id)
                            .map(|reply| (replier.review.author.as_str(), reply))
                    })
                    .collect();
                // RFC 3339 sorts as text, so the thread reads oldest first.
                replies.sort_by_key(|(_, reply)| reply.at.as_str());
                threads.push(Thread {
                    author: &file.review.author,
                    comment,
                    reactions,
                    replies,
                    mine,
                });
            }
        }
        threads
    }

    pub fn threads_for<'a>(&'a self, path: &str) -> Vec<Thread<'a>> {
        self.threads()
            .into_iter()
            .filter(|thread| thread.comment.path == path)
            .collect()
    }
}

/// Where a comment shows up in the current view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// Its commit is part of the scope. `line` is on the view's new side.
    InScope { line: usize },
    /// Its commit is outside the scope, but the line is still unchanged.
    /// Shown dimmed, and only while "other revisions' comments" is on.
    OtherRevision { line: usize },
    /// Blame no longer finds the line: the code it described was rewritten.
    /// [`map_line`] then follows it to the replacement, so the comment can
    /// sit beside the fix marked "code changed" instead of vanishing.
    NotInView,
}

/// Where a line of the old revision ends up after the changed regions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineFate {
    /// No region rewrote it; `line` is its shifted position in the new
    /// revision. Rare after a blame miss (whitespace-rewritten attribution),
    /// but still the best anchor there is.
    Untouched { line: usize },
    /// A region rewrote it; `line` is the first line of the replacement, or
    /// the line before the gap when the region only deleted.
    Rewritten { line: usize },
}

impl LineFate {
    pub fn line(self) -> usize {
        match self {
            LineFate::Untouched { line } | LineFate::Rewritten { line } => line,
        }
    }
}

/// Follow `line` of the old revision through `regions` (ascending, as git
/// emits them) to its place in the new revision.
pub fn map_line(line: usize, regions: &[ChangedRegion]) -> LineFate {
    let mut mapped = line as isize;
    for region in regions {
        let rewritten = region.old_lines > 0
            && line >= region.old_start
            && line < region.old_start + region.old_lines;
        if rewritten {
            return LineFate::Rewritten {
                line: region.new_start.max(1),
            };
        }
        // A pure insertion sits after `old_start`, so it shifts only lines
        // strictly beyond it; a real region shifts everything past its end.
        let past = if region.old_lines == 0 {
            line > region.old_start
        } else {
            line >= region.old_start + region.old_lines
        };
        if past {
            mapped += region.new_lines as isize - region.old_lines as isize;
        }
    }
    LineFate::Untouched {
        line: mapped.max(1) as usize,
    }
}

/// Place a new-side comment using a blame of the view's new side.
///
/// `in_scope` holds the commits the current scope covers. Old-side comments
/// are not handled here: they belong to the commit that removed the line and
/// are shown at their stored line only while that commit is in scope.
pub fn place(comment: &Comment, in_scope: &HashSet<Oid>, blame: &[BlameLine]) -> Placement {
    debug_assert_eq!(comment.side, Side::New);
    let row = blame
        .iter()
        .find(|row| row.origin.commit == comment.commit && row.origin.line == comment.line);
    match row {
        Some(row) if in_scope.contains(&comment.commit) => Placement::InScope {
            line: row.final_line,
        },
        Some(row) => Placement::OtherRevision {
            line: row.final_line,
        },
        None => Placement::NotInView,
    }
}

/// All comments rendered as one prompt for an AI agent ("Copy all as prompt").
pub fn as_prompt(threads: &[Thread<'_>]) -> String {
    let mut out = String::from("Review comments to address:\n");
    for (index, thread) in threads.iter().enumerate() {
        let comment = thread.comment;
        let first = comment.line + 1 - comment.lines.min(comment.line);
        let lines = if comment.lines > 1 {
            format!("{first}-{}", comment.line)
        } else {
            comment.line.to_string()
        };
        // `path:line at hash` over prose: the owner wants the header to read
        // like a compiler location, and agents parse that form natively.
        // The short id is the address `diffie --reply` answers to.
        out.push_str(&format!(
            "\n{}. {}:{lines} at {} #{}",
            index + 1,
            comment.path,
            comment.commit.short(),
            &comment.id[..comment.id.len().min(8)]
        ));
        // The author only when reviewers differ; [resolved] only reaches
        // callers that include closed threads (the app copies open ones).
        if threads.iter().any(|other| other.author != thread.author) {
            out.push_str(&format!(" ({})", thread.author));
        }
        if thread.resolved() {
            out.push_str(" [resolved]");
        }
        out.push('\n');
        out.push_str("```\n");
        out.push_str(comment.snippet.trim_end_matches('\n'));
        out.push_str("\n```\n");
        if let Some(selection) = &comment.selection {
            out.push_str(&format!("About `{selection}`: "));
        }
        out.push_str(comment.body.trim());
        out.push('\n');
        if let Some(suggestion) = &comment.suggestion {
            out.push_str("Suggested replacement:\n```\n");
            out.push_str(suggestion.trim_end_matches('\n'));
            out.push_str("\n```\n");
        }
        for (replier, reply) in &thread.replies {
            out.push_str(&format!("Reply ({replier}): {}\n", reply.body.trim()));
        }
    }
    out
}

/// Why the headless prompt (`diffie --prompt`) produced nothing.
#[derive(Debug)]
pub enum PromptError {
    /// No review folder with files under the review dir.
    NoReviews(PathBuf),
    /// The asked-for source has no review; the sources that do are listed.
    UnknownSource {
        asked: String,
        available: Vec<String>,
    },
    /// No source asked for while several exist. Never guessed: the wrong
    /// pick would hand out another review's comments.
    SeveralReviews {
        available: Vec<String>,
    },
    File(PathBuf, ReviewFileError),
}

impl std::fmt::Display for PromptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoReviews(dir) => write!(f, "No reviews under {}", dir.display()),
            Self::UnknownSource { asked, available } => {
                write!(
                    f,
                    "No review for '{asked}'. Reviews: {}",
                    available.join(", ")
                )
            }
            Self::SeveralReviews { available } => {
                write!(
                    f,
                    "Several reviews here: {}. Name the one to print.",
                    available.join(", ")
                )
            }
            Self::File(path, error) => write!(f, "{}: {error}", path.display()),
        }
    }
}

impl std::error::Error for PromptError {}

/// Render one review source like "Copy all as prompt", without the UI.
///
/// `source` picks the review folder, slugged exactly like the app names it.
/// Without it a sole review is printed; several are refused by name, never
/// guessed at. Returns the chosen source's folder name with the rendered
/// text.
pub fn prompt(
    review_dir: &Path,
    source: Option<&str>,
    include_resolved: bool,
) -> Result<(String, String), PromptError> {
    let folders: Vec<PathBuf> = std::fs::read_dir(review_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    if folders.is_empty() {
        return Err(PromptError::NoReviews(review_dir.to_path_buf()));
    }
    let names = || {
        let mut names: Vec<String> = folders
            .iter()
            .filter_map(|folder| folder.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    let folder = match (source, folders.as_slice()) {
        (Some(asked), _) => {
            let want = slug(asked);
            folders
                .iter()
                .find(|folder| folder.file_name().is_some_and(|name| name == want.as_str()))
                .ok_or_else(|| PromptError::UnknownSource {
                    asked: asked.to_string(),
                    available: names(),
                })?
                .clone()
        }
        (None, [only]) => only.clone(),
        (None, _) => return Err(PromptError::SeveralReviews { available: names() }),
    };
    let mut files: Vec<PathBuf> = std::fs::read_dir(&folder)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    // Sorted for a stable comment order; which file plays "mine" does not
    // matter here, no file is written.
    files.sort();
    let mut files = files.into_iter();
    let Some(first) = files.next() else {
        return Err(PromptError::NoReviews(folder));
    };
    let mut set = ReviewSet::new(
        ReviewFile::load(&first).map_err(|error| PromptError::File(first.clone(), error))?,
    );
    for path in files {
        let file =
            ReviewFile::load(&path).map_err(|error| PromptError::File(path.clone(), error))?;
        set.add_other(file);
    }
    let threads: Vec<Thread<'_>> = set
        .threads()
        .into_iter()
        .filter(|thread| include_resolved || !thread.resolved())
        .collect();
    let name = folder
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let text = if threads.is_empty() {
        "No open review comments.\n".to_string()
    } else {
        as_prompt(&threads)
    };
    Ok((name, text))
}

fn slug(name: &str) -> String {
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches(|c| c == '-' || c == '.').to_string();
    if slug.is_empty() {
        "unnamed".to_string()
    } else {
        slug
    }
}

/// Why `diffie --reply` could not thread an answer.
#[derive(Debug)]
pub enum ReplyError {
    /// No review folder with files under the review dir.
    NoReviews(PathBuf),
    /// No comment id starts with the prefix.
    UnknownComment {
        prefix: String,
    },
    /// Several comments match the prefix. Never guessed: the answer would
    /// thread under the wrong question.
    AmbiguousComment {
        prefix: String,
        matches: Vec<String>,
    },
    File(PathBuf, ReviewFileError),
}

impl std::fmt::Display for ReplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoReviews(dir) => write!(f, "No reviews under {}", dir.display()),
            Self::UnknownComment { prefix } => {
                write!(f, "No comment starts with #{prefix}.")
            }
            Self::AmbiguousComment { prefix, matches } => {
                write!(
                    f,
                    "#{prefix} matches several comments: {}. Give more of the id.",
                    matches.join(", ")
                )
            }
            Self::File(path, error) => write!(f, "{}: {error}", path.display()),
        }
    }
}

impl std::error::Error for ReplyError {}

/// Where [`reply`] threaded the answer: enough to confirm the address.
#[derive(Debug, PartialEq, Eq)]
pub struct RepliedTo {
    pub review: String,
    pub path: String,
    pub line: usize,
}

/// Thread `body` under the comment whose id starts with `id_prefix`, as
/// `author`: the reply goes into the author's own file beside the comment's,
/// never into the file the comment lives in. An id addresses one comment
/// across every review here, so unlike [`prompt`] no review needs naming -
/// but an ambiguous prefix is refused, never guessed.
pub fn reply(
    review_dir: &Path,
    id_prefix: &str,
    body: &str,
    author: &str,
    session: Option<&str>,
) -> Result<RepliedTo, ReplyError> {
    let folders: Vec<PathBuf> = std::fs::read_dir(review_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    if folders.is_empty() {
        return Err(ReplyError::NoReviews(review_dir.to_path_buf()));
    }
    // (folder, the file holding the comment, its index): the host header
    // seeds the replier's file when this review has none from them yet.
    let mut matches: Vec<(&PathBuf, ReviewFile, usize)> = Vec::new();
    for folder in &folders {
        let mut files: Vec<PathBuf> = std::fs::read_dir(folder)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
            .collect();
        files.sort();
        for path in files {
            let file =
                ReviewFile::load(&path).map_err(|error| ReplyError::File(path.clone(), error))?;
            for index in 0..file.comments.len() {
                if file.comments[index].id.starts_with(id_prefix) {
                    matches.push((folder, file.clone(), index));
                }
            }
        }
    }
    match matches.as_slice() {
        [] => Err(ReplyError::UnknownComment {
            prefix: id_prefix.to_string(),
        }),
        [(folder, host, index)] => {
            let comment = &host.comments[*index];
            let target = folder.join(format!("{}.toml", slug(author)));
            let mut mine = match ReviewFile::load(&target) {
                Ok(file) => file,
                Err(ReviewFileError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    ReviewFile {
                        review: Header {
                            author: author.to_string(),
                            ..host.review.clone()
                        },
                        completed: None,
                        reviewed: Vec::new(),
                        comments: Vec::new(),
                        reactions: Vec::new(),
                        replies: Vec::new(),
                        version: FORMAT_VERSION,
                    }
                }
                Err(error) => return Err(ReplyError::File(target, error)),
            };
            mine.add_reply(&comment.id, body, session.map(String::from));
            mine.save(&target)
                .map_err(|error| ReplyError::File(target, error))?;
            Ok(RepliedTo {
                review: folder
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                path: comment.path.clone(),
                line: comment.line,
            })
        }
        several => Err(ReplyError::AmbiguousComment {
            prefix: id_prefix.to_string(),
            matches: several
                .iter()
                .map(|(_, host, index)| {
                    let comment = &host.comments[*index];
                    format!(
                        "#{} {}:{}",
                        &comment.id[..comment.id.len().min(8)],
                        comment.path,
                        comment.line
                    )
                })
                .collect(),
        }),
    }
}

/// 128 random bits as hex. `RandomState` is seeded per instance by the OS,
/// which is enough for ids that only have to be unique among reviewers.
fn new_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let part = || {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default(),
        );
        hasher.finish()
    };
    format!("{:016x}{:016x}", part(), part())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CompareMode, Comparison, LineOrigin};

    fn resolved() -> Resolved {
        Resolved {
            comparison: Comparison {
                source: "feat/index-cache".into(),
                target: "main".into(),
                mode: CompareMode::BranchAdds,
                uncommitted: false,
                ..Default::default()
            },
            head: Oid::new("h".repeat(40)),
            target_head: Oid::new("t".repeat(40)),
            base: Oid::new("b".repeat(40)),
        }
    }

    fn comment(commit: &str, line: usize) -> NewComment {
        NewComment {
            commit: Oid::new(commit),
            path: "src/index/cache.rs".into(),
            side: Side::New,
            line,
            lines: 1,
            selection: Some("meta.modified().ok()?".into()),
            snippet: "if entry.mtime != meta.modified().ok()? {".into(),
            body: "This swallows permission errors.".into(),
            suggestion: None,
        }
    }

    #[test]
    fn round_trips_through_toml() {
        let mut file = ReviewFile::new("Ada", &resolved());
        let id = file.add_comment(comment("a3f9c1e", 45));
        file.toggle_reaction(&id, ReactionKind::Seen);
        file.mark_reviewed(
            "src/index/stats.rs",
            Some(Oid::new("blob1")),
            Oid::new("head1"),
        );
        let blob = Oid::new("blob1");
        assert!(
            file.complete_if_all_reviewed(
                [("src/index/stats.rs", Some(&blob))],
                &Oid::new("head1")
            )
        );

        let directory = tempfile::tempdir().unwrap();
        let path = ReviewFile::path_in(directory.path(), "feat/index-cache", "Ada L.");
        assert!(path.ends_with("feat-index-cache/Ada-L.toml"));
        file.save(&path).unwrap();
        let loaded = ReviewFile::load(&path).unwrap();
        assert_eq!(loaded, file);
    }

    #[test]
    fn rejects_other_format_versions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("review.toml");
        let mut file = ReviewFile::new("Ada", &resolved());
        file.version = 99;
        std::fs::write(&path, toml::to_string_pretty(&file).unwrap()).unwrap();
        assert!(matches!(
            ReviewFile::load(&path),
            Err(ReviewFileError::Version { found: 99, .. })
        ));
    }

    #[test]
    fn tick_clears_itself_when_the_file_changes() {
        let mut file = ReviewFile::new("Ada", &resolved());
        let blob = Oid::new("blob1");
        assert_eq!(
            file.review_state("a.rs", Some(&blob)),
            ReviewState::NotReviewed
        );
        file.mark_reviewed("a.rs", Some(blob.clone()), Oid::new("head1"));
        assert_eq!(
            file.review_state("a.rs", Some(&blob)),
            ReviewState::Reviewed
        );
        let changed = Oid::new("blob2");
        assert_eq!(
            file.review_state("a.rs", Some(&changed)),
            ReviewState::ChangedSince
        );
        assert_eq!(file.review_state("a.rs", None), ReviewState::ChangedSince);
    }

    #[test]
    fn a_review_is_complete_only_when_every_file_is_ticked() {
        let mut file = ReviewFile::new("Ada", &resolved());
        let (a, b) = (Oid::new("blob-a"), Oid::new("blob-b"));
        let head = Oid::new("head1");
        file.mark_reviewed("a.rs", Some(a.clone()), head.clone());
        assert!(!file.complete_if_all_reviewed([("a.rs", Some(&a)), ("b.rs", Some(&b))], &head));
        assert_eq!(file.completed, None);
        file.mark_reviewed("b.rs", Some(b.clone()), head.clone());
        assert!(file.complete_if_all_reviewed([("a.rs", Some(&a)), ("b.rs", Some(&b))], &head));
        assert_eq!(file.completed.as_ref().map(|done| &done.head), Some(&head));
        assert!(
            !file.complete_if_all_reviewed([], &head),
            "no files is not a review"
        );
    }

    #[test]
    fn ticking_a_narrow_scope_does_not_complete_the_branch() {
        // The regression behind `changed_paths`: the branch changes a.rs and
        // b.rs, the scope showed only a.rs. Completion must be judged
        // against the branch's list, and then not trigger.
        let mut file = ReviewFile::new("Ada", &resolved());
        let (a, b) = (Oid::new("blob-a"), Oid::new("blob-b"));
        let head = Oid::new("head1");
        file.mark_reviewed("a.rs", Some(a.clone()), head.clone());
        assert!(!file.complete_if_all_reviewed([("a.rs", Some(&a)), ("b.rs", Some(&b))], &head));
        assert_eq!(file.completed, None);
    }

    #[test]
    fn a_deleted_file_ticks_at_no_content() {
        let mut file = ReviewFile::new("Ada", &resolved());
        let head = Oid::new("head1");
        file.mark_reviewed("gone.rs", None, head.clone());
        assert_eq!(file.review_state("gone.rs", None), ReviewState::Reviewed);
        let back = Oid::new("blob-back");
        assert_eq!(
            file.review_state("gone.rs", Some(&back)),
            ReviewState::ChangedSince,
            "a re-added file needs a fresh look"
        );
        assert!(file.complete_if_all_reviewed([("gone.rs", None)], &head));
    }

    #[test]
    fn a_stale_completed_marker_is_cleared() {
        let mut file = ReviewFile::new("Ada", &resolved());
        let (a, b) = (Oid::new("blob-a"), Oid::new("blob-b"));
        let head = Oid::new("head1");
        file.mark_reviewed("a.rs", Some(a.clone()), head.clone());
        assert!(file.complete_if_all_reviewed([("a.rs", Some(&a))], &head));

        assert!(
            !file.verify_completed([("a.rs", Some(&a))]),
            "still complete, nothing to clear"
        );
        assert!(file.completed.is_some());

        // The branch gained b.rs since the marker was set.
        assert!(file.verify_completed([("a.rs", Some(&a)), ("b.rs", Some(&b))]));
        assert_eq!(file.completed, None);
        assert!(
            !file.verify_completed([("a.rs", Some(&a)), ("b.rs", Some(&b))]),
            "already cleared"
        );
    }

    #[test]
    fn reactions_toggle_and_the_picker_shrinks() {
        let mut file = ReviewFile::new("Ada", &resolved());
        let id = file.add_comment(comment("a3f9c1e", 45));
        assert_eq!(file.reactions_left(&id).len(), 4);
        assert!(file.toggle_reaction(&id, ReactionKind::Seen));
        assert_eq!(
            file.reactions_left(&id),
            vec![
                ReactionKind::Done,
                ReactionKind::WontFix,
                ReactionKind::FollowUp
            ]
        );
        assert!(!file.toggle_reaction(&id, ReactionKind::Seen));
        assert_eq!(file.reactions_left(&id).len(), 4);
    }

    #[test]
    fn a_colleagues_reaction_resolves_my_comment_without_touching_my_file() {
        let mut mine = ReviewFile::new("Ada", &resolved());
        let id = mine.add_comment(comment("a3f9c1e", 45));
        let mut theirs = ReviewFile::new("Grace", &resolved());
        theirs.toggle_reaction(&id, ReactionKind::Done);

        let untouched = mine.clone();
        let mut set = ReviewSet::new(mine);
        assert!(set.add_other(theirs));
        let threads = set.threads();
        assert_eq!(threads.len(), 1);
        assert!(threads[0].mine);
        assert!(threads[0].resolved());
        assert_eq!(threads[0].reactions, vec![("Grace", ReactionKind::Done)]);
        assert_eq!(set.mine, untouched);
    }

    #[test]
    fn refuses_a_second_file_under_my_own_name() {
        let mut set = ReviewSet::new(ReviewFile::new("Ada", &resolved()));
        assert!(!set.add_other(ReviewFile::new("Ada", &resolved())));
    }

    #[test]
    fn only_the_owner_can_edit_or_delete() {
        let mut mine = ReviewFile::new("Ada", &resolved());
        let id = mine.add_comment(comment("a3f9c1e", 45));
        let mut theirs = ReviewFile::new("Grace", &resolved());
        assert!(!theirs.edit_comment(&id, "hijacked".into()));
        assert!(!theirs.delete_comment(&id));
        assert!(mine.edit_comment(&id, "clearer".into()));
        assert!(mine.comments[0].edited.is_some());
        assert!(mine.delete_comment(&id));
    }

    #[test]
    fn places_comments_by_blame() {
        let mut file = ReviewFile::new("Ada", &resolved());
        file.add_comment(comment("a3f9c1e", 45));
        let comment = &file.comments[0];
        // Three lines were inserted above since: the line is now 48.
        let blame = vec![BlameLine {
            final_line: 48,
            origin: LineOrigin {
                commit: Oid::new("a3f9c1e"),
                line: 45,
            },
        }];
        let in_scope: HashSet<Oid> = [Oid::new("a3f9c1e")].into();
        assert_eq!(
            place(comment, &in_scope, &blame),
            Placement::InScope { line: 48 }
        );
        assert_eq!(
            place(comment, &HashSet::new(), &blame),
            Placement::OtherRevision { line: 48 }
        );
        assert_eq!(place(comment, &in_scope, &[]), Placement::NotInView);
    }

    #[test]
    fn maps_lines_through_changed_regions() {
        let region = |old_start, old_lines, new_start, new_lines| ChangedRegion {
            old_start,
            old_lines,
            new_start,
            new_lines,
        };
        // Lines 10-11 became lines 12-14 after an insertion of 2 above.
        let regions = [region(3, 0, 4, 2), region(10, 2, 12, 3)];
        assert_eq!(map_line(2, &regions), LineFate::Untouched { line: 2 });
        // The insertion comes after line 3, so line 3 itself does not move.
        assert_eq!(map_line(3, &regions), LineFate::Untouched { line: 3 });
        assert_eq!(map_line(4, &regions), LineFate::Untouched { line: 6 });
        assert_eq!(map_line(10, &regions), LineFate::Rewritten { line: 12 });
        assert_eq!(map_line(11, &regions), LineFate::Rewritten { line: 12 });
        // Past the rewrite: shifted by both regions (+2, then +1).
        assert_eq!(map_line(12, &regions), LineFate::Untouched { line: 15 });
        // A pure deletion of lines 5-6 anchors at the line before the gap,
        // and a deletion at the top of the file clamps to line 1.
        assert_eq!(
            map_line(5, &[region(5, 2, 4, 0)]),
            LineFate::Rewritten { line: 4 }
        );
        assert_eq!(
            map_line(1, &[region(1, 2, 0, 0)]),
            LineFate::Rewritten { line: 1 }
        );
    }

    #[test]
    fn prompt_carries_snippet_selection_and_body() {
        let mut file = ReviewFile::new("Ada", &resolved());
        file.add_comment(comment("a3f9c1e0000", 45));
        let set = ReviewSet::new(file);
        let prompt = as_prompt(&set.threads());
        assert!(prompt.contains("src/index/cache.rs:45 at a3f9c1e"));
        assert!(prompt.contains("if entry.mtime != meta.modified().ok()? {"));
        assert!(prompt.contains("About `meta.modified().ok()?`: This swallows permission errors."));
    }

    #[test]
    fn prompt_merges_reviewers_and_never_guesses_a_review() {
        let dir = tempfile::tempdir().unwrap();
        let review_dir = dir.path();
        let mut old = ReviewFile::new("Ada", &resolved());
        old.add_comment(comment("c0ffee00000", 3));
        old.save(&ReviewFile::path_in(review_dir, "old-branch", "Ada"))
            .unwrap();
        let mut mine = ReviewFile::new("Ada", &resolved());
        let done_id = mine.add_comment(comment("a3f9c1e0000", 45));
        mine.add_comment(comment("b4e8d2f0000", 9));
        mine.save(&ReviewFile::path_in(review_dir, "feature", "Ada"))
            .unwrap();
        let mut theirs = ReviewFile::new("Grace", &resolved());
        theirs.toggle_reaction(&done_id, ReactionKind::Done);
        theirs
            .save(&ReviewFile::path_in(review_dir, "feature", "Grace"))
            .unwrap();

        // Two reviews and no name: refused by name, not resolved by a guess.
        let error = prompt(review_dir, None, false).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Several reviews here: feature, old-branch. Name the one to print."
        );

        let (source, text) = prompt(review_dir, Some("feature"), false).unwrap();
        assert_eq!(source, "feature");
        assert!(text.contains("at b4e8d2f"));
        // Grace's Done closes Ada's first comment across files.
        assert!(!text.contains("at a3f9c1e"));

        let (_, text) = prompt(review_dir, Some("feature"), true).unwrap();
        // The random id sits between hash and tag: `at a3f9c1e #<id> [resolved]`.
        assert!(text.contains("at a3f9c1e #"));
        assert!(text.contains(" [resolved]"));

        let (source, _) = prompt(review_dir, Some("old-branch"), false).unwrap();
        assert_eq!(source, "old-branch");

        let error = prompt(review_dir, Some("gone"), false).unwrap_err();
        assert_eq!(
            error.to_string(),
            "No review for 'gone'. Reviews: feature, old-branch"
        );
    }

    #[test]
    fn reply_threads_an_answer_into_the_repliers_own_file() {
        let dir = tempfile::tempdir().unwrap();
        let review_dir = dir.path();
        let mut mine = ReviewFile::new("Ada", &resolved());
        let id = mine.add_comment(comment("a3f9c1e0000", 45));
        mine.save(&ReviewFile::path_in(review_dir, "feature", "Ada"))
            .unwrap();

        // A unique prefix addresses the comment without naming the review.
        let to = reply(
            review_dir,
            &id[..8],
            "it counts, not a time",
            "Claude",
            Some("89c2fcec"),
        )
        .unwrap();
        assert_eq!(to.review, "feature");
        assert_eq!((to.path.as_str(), to.line), ("src/index/cache.rs", 45));
        // The answer landed in Claude's file; Ada's was not touched.
        let claudes =
            ReviewFile::load(&ReviewFile::path_in(review_dir, "feature", "Claude")).unwrap();
        assert_eq!(claudes.review.author, "Claude");
        assert_eq!(claudes.replies.len(), 1);
        assert_eq!(claudes.replies[0].session.as_deref(), Some("89c2fcec"));
        let adas = ReviewFile::load(&ReviewFile::path_in(review_dir, "feature", "Ada")).unwrap();
        assert!(adas.replies.is_empty());

        // The thread knows the question has an answer, the prompt prints it,
        // and an answer is conversation, not a verdict: still open.
        let mut set = ReviewSet::new(adas);
        set.add_other(claudes);
        let threads = set.threads();
        assert!(threads[0].answered());
        assert!(!threads[0].resolved());
        let text = as_prompt(&threads);
        assert!(text.contains(&format!("#{}", &id[..8])));
        assert!(text.contains("Reply (Claude): it counts, not a time"));

        // A second reply from the same author appends; order is by time.
        reply(review_dir, &id[..8], "see 698c1af", "Claude", None).unwrap();
        let claudes =
            ReviewFile::load(&ReviewFile::path_in(review_dir, "feature", "Claude")).unwrap();
        assert_eq!(claudes.replies.len(), 2);

        // No match and an ambiguous prefix are refused, never guessed.
        let error = reply(review_dir, "0000", "x", "Claude", None).unwrap_err();
        assert_eq!(error.to_string(), "No comment starts with #0000.");
        let mut more =
            ReviewFile::load(&ReviewFile::path_in(review_dir, "feature", "Ada")).unwrap();
        more.add_comment(comment("b4e8d2f0000", 9));
        more.save(&ReviewFile::path_in(review_dir, "feature", "Ada"))
            .unwrap();
        let error = reply(review_dir, "", "x", "Claude", None).unwrap_err();
        assert!(error.to_string().contains("matches several comments"));
        assert!(error.to_string().contains("Give more of the id."));
    }

    #[test]
    fn prompt_names_authors_only_when_reviewers_differ() {
        let dir = tempfile::tempdir().unwrap();
        let mut mine = ReviewFile::new("Ada", &resolved());
        mine.add_comment(comment("a3f9c1e0000", 45));
        mine.save(&ReviewFile::path_in(dir.path(), "feature", "Ada"))
            .unwrap();
        let (_, text) = prompt(dir.path(), None, false).unwrap();
        assert!(!text.contains("(Ada)"));

        let mut theirs = ReviewFile::new("Grace", &resolved());
        theirs.add_comment(comment("b4e8d2f0000", 9));
        theirs
            .save(&ReviewFile::path_in(dir.path(), "feature", "Grace"))
            .unwrap();
        let (_, text) = prompt(dir.path(), None, false).unwrap();
        assert!(text.contains("(Ada)"));
        assert!(text.contains("(Grace)"));
    }

    #[test]
    fn prompt_without_reviews_says_where_it_looked() {
        let dir = tempfile::tempdir().unwrap();
        let error = prompt(&dir.path().join("review"), None, false).unwrap_err();
        assert!(error.to_string().starts_with("No reviews under "));
    }

    #[test]
    fn ids_are_unique() {
        let ids: HashSet<String> = (0..1000).map(|_| new_id()).collect();
        assert_eq!(ids.len(), 1000);
    }
}
