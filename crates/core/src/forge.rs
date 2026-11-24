//! The code host a pull request lives on, behind one trait.
//!
//! [`Forge`] is everything the review needs from the host: describe the
//! pull request, take a review with its comments, update a comment, react.
//! [`plan`] decides what a publish sends, [`publish`] sends it and reports
//! what landed, and [`ReviewFile::record`] writes the forge's ids back - so
//! a second publish never posts a comment twice. Nothing here knows which
//! forge it talks to; [`crate::github`] is the one implementation.

use crate::model::{BlameLine, Oid};
use crate::pr::{PrArgument, PrList, PrSummary, PullRequest, PullStatus, Stack, StackSummary};
use crate::review::{
    Comment, Published, Reaction, ReactionKind, ReviewFile, ReviewSet, Side, Thread,
};
use crate::time::now_rfc3339;

#[derive(Debug, thiserror::Error)]
pub enum ForgeError {
    #[error("{tool} is required for a pull request, and it was not found on PATH: {error}")]
    ToolMissing {
        tool: &'static str,
        error: std::io::Error,
    },
    /// The forge refused; its own words, verbatim.
    #[error("{0}")]
    Refused(String),
    #[error("unexpected answer from the forge: {0:?}")]
    Output(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub trait Forge: Send + Sync {
    /// The forge's name as tooltips and messages say it.
    fn name(&self) -> &'static str;

    /// The pull request's state, branches and exact commits.
    fn pull_request(&self, argument: &PrArgument) -> Result<PullRequest, ForgeError>;

    /// The repository's pull requests, newest activity first: enough per
    /// row for the picker to show and choose, never the full description.
    fn pull_requests(&self, which: PrList) -> Result<Vec<PrSummary>, ForgeError>;

    /// The stack the named pull request belongs to, every member resolved
    /// to its oids, latest first. `Ok(None)` is a pull request in no
    /// stack. A number that names no pull request at all is an `Err` in
    /// the forge's own words - [`crate::pr::open_stack`] then reads the
    /// number as a stack's own, which is the other thing it can be.
    fn stack(&self, argument: &PrArgument) -> Result<Option<Stack>, ForgeError>;

    /// The stack of that number, where the number is the stack's own and
    /// not a member's. A direct lookup, never a scan of
    /// [`Self::stacks`]: a stack whose members all sit past that list's
    /// page must still be reachable by its number.
    fn stack_numbered(&self, number: u64) -> Result<Option<Stack>, ForgeError>;

    /// The stacks among the repository's pull requests, the newest
    /// activity first. Grouped from the same single page
    /// [`Self::pull_requests`] lists, so a stack whose members all sit
    /// past that page is not reported.
    fn stacks(&self) -> Result<Vec<StackSummary>, ForgeError>;

    /// Where the pull request stands now: merged or not, the review
    /// decision, and the head commit's checks when it has any.
    fn status(&self, pull: &PullRequest) -> Result<PullStatus, ForgeError>;

    /// Posts one review holding the comments and the verdict. `Err` means
    /// nothing reached the pull request. `Ok` holds one entry per comment,
    /// in order: a line the forge will not take (it is not part of the
    /// pull request's diff) is posted on the file instead, with
    /// [`DraftComment::file_body`], and a comment it will not take at all
    /// is left out of the review and answered [`Posting::Refused`], so one
    /// bad path never blocks the rest.
    fn submit_review(
        &self,
        pull: &PullRequest,
        review: &ReviewDraft,
    ) -> Result<Vec<Posting>, ForgeError>;

    /// Replaces the body of a comment published earlier.
    fn update_comment(&self, pull: &PullRequest, id: &str, body: &str) -> Result<(), ForgeError>;

    /// Reacts to a published comment with the forge's nearest emoji.
    fn react(&self, pull: &PullRequest, id: &str, kind: ReactionKind) -> Result<(), ForgeError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Comments only; says nothing about merging.
    Comment,
    Approve,
    /// Blocks the merge until answered. Only ever sent with comments: a
    /// refusal that does not say what to change helps nobody.
    RequestChanges,
}

impl Verdict {
    /// The review must carry at least one comment to be sent at all.
    pub fn needs_comments(self) -> bool {
        match self {
            Verdict::Comment | Verdict::RequestChanges => true,
            Verdict::Approve => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewDraft {
    /// The commit the review judges; line anchors count in its files.
    pub commit: Oid,
    pub verdict: Verdict,
    pub comments: Vec<DraftComment>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftComment {
    pub path: String,
    /// `None` when no line of the reviewed commit can be named for the
    /// comment; it then goes on the file.
    pub anchor: Option<LineAnchor>,
    pub body: String,
    /// The body for a file-level comment: it names the lines and quotes
    /// the code itself, since no diff row beside it does.
    pub file_body: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineAnchor {
    pub side: Side,
    /// Last commented line.
    pub line: usize,
    /// How many lines the comment covers, ending at `line`. At least 1.
    pub lines: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostedComment {
    /// The forge's own id for the comment; opaque outside the forge.
    pub id: String,
    pub url: String,
    /// It sits on the file, not on a line, and so carries
    /// [`DraftComment::file_body`]; a later edit must send that form again.
    pub on_file: bool,
}

/// How one comment of a submitted review fared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Posting {
    Posted(PostedComment),
    /// Not part of the review; the forge's own words.
    Refused(String),
}

/// What one publish sends. Built on a snapshot of the review, so the
/// bodies ride along: [`ReviewFile::record`] compares them with the live
/// file to notice an edit made while the publish ran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub review: ReviewDraft,
    /// The comment ids behind `review.comments`, in the same order, each
    /// with the body as written.
    comments: Vec<(String, String)>,
    edits: Vec<Edit>,
    reactions: Vec<PlannedReaction>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Edit {
    comment: String,
    forge_id: String,
    /// As written, for [`ReviewFile::record`]'s comparison.
    body: String,
    /// As sent: the line or the file form, whichever the forge holds.
    rendered: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PlannedReaction {
    comment: String,
    forge_id: String,
    kind: ReactionKind,
}

impl Plan {
    /// Nothing to send: no comment, no edit, no reaction, no approval.
    pub fn is_empty(&self) -> bool {
        self.review.verdict == Verdict::Comment && self.pending() == Pending::default()
    }

    pub fn pending(&self) -> Pending {
        Pending {
            comments: self.comments.len(),
            edits: self.edits.len(),
            reactions: self.reactions.len(),
        }
    }
}

/// How much a publish would send; the menu shows it before the click.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pending {
    pub comments: usize,
    pub edits: usize,
    pub reactions: usize,
}

impl Pending {
    pub fn total(self) -> usize {
        self.comments + self.edits + self.reactions
    }
}

/// What a comment of the reviewer's own still owes the forge.
enum Owed<'a> {
    Post,
    Edit(&'a Published),
}

/// A thread already resolved here was settled without the pull request
/// and would only be noise there, so only open ones are posted.
fn owed<'a>(thread: &Thread<'a>) -> Option<Owed<'a>> {
    if !thread.mine {
        return None;
    }
    match &thread.comment.published {
        Some(published) if published.stale => Some(Owed::Edit(published)),
        Some(_) => None,
        None if thread.resolved() => None,
        None => Some(Owed::Post),
    }
}

/// The reviewer's own unsent reactions on comments the forge knows - a
/// colleague's included, since the colleague's file carries its forge id.
fn owed_reactions<'a>(
    reviews: &'a ReviewSet,
    threads: &'a [Thread<'a>],
) -> impl Iterator<Item = (&'a Reaction, &'a Published)> {
    reviews
        .mine
        .reactions
        .iter()
        .filter(|reaction| reaction.published.is_none())
        .filter_map(|reaction| {
            let published = threads
                .iter()
                .find(|thread| thread.comment.id == reaction.comment)?
                .comment
                .published
                .as_ref()?;
            Some((reaction, published))
        })
}

/// How much a publish would send. The menu asks on every render, so this
/// counts without building a single body or asking for a blame.
pub fn pending(reviews: &ReviewSet) -> Pending {
    let threads = reviews.threads();
    let mut pending = Pending::default();
    for thread in &threads {
        match owed(thread) {
            Some(Owed::Post) => pending.comments += 1,
            Some(Owed::Edit(_)) => pending.edits += 1,
            None => {}
        }
    }
    pending.reactions = owed_reactions(reviews, &threads).count();
    pending
}

/// Decides what a publish sends: the comments, edits and reactions
/// [`pending`] counts.
///
/// `with_comments` off sends the verdict alone. `blame` answers for a path
/// at `head`; `None` (the file is gone there) leaves the comment on the
/// file.
pub fn plan(
    reviews: &ReviewSet,
    head: &Oid,
    verdict: Verdict,
    with_comments: bool,
    mut blame: impl FnMut(&str) -> Option<Vec<BlameLine>>,
) -> Plan {
    let mut plan = Plan {
        review: ReviewDraft {
            commit: head.clone(),
            verdict,
            comments: Vec::new(),
        },
        comments: Vec::new(),
        edits: Vec::new(),
        reactions: Vec::new(),
    };
    if !with_comments {
        return plan;
    }
    let mut blames: std::collections::HashMap<String, Option<Vec<BlameLine>>> =
        std::collections::HashMap::new();
    let threads = reviews.threads();
    for thread in &threads {
        let comment = thread.comment;
        match owed(thread) {
            None => {}
            Some(Owed::Edit(published)) => plan.edits.push(Edit {
                comment: comment.id.clone(),
                forge_id: published.id.clone(),
                body: comment.body.clone(),
                rendered: if published.on_file {
                    file_body(comment)
                } else {
                    body(comment)
                },
            }),
            Some(Owed::Post) => {
                let anchor = match comment.side {
                    // An old-side line counts in whatever the view's old
                    // side was when the comment was made - the base, or an
                    // intermediate commit under a narrower scope - and the
                    // comment does not say which. A wrong line is worse
                    // than none, so it goes on the file.
                    Side::Old => None,
                    Side::New => blames
                        .entry(comment.path.clone())
                        .or_insert_with(|| blame(&comment.path))
                        .as_deref()
                        .and_then(|rows| anchor_at_head(comment, rows)),
                };
                plan.review.comments.push(DraftComment {
                    path: comment.path.clone(),
                    anchor,
                    body: body(comment),
                    file_body: file_body(comment),
                });
                plan.comments
                    .push((comment.id.clone(), comment.body.clone()));
            }
        }
    }
    plan.reactions = owed_reactions(reviews, &threads)
        .map(|(reaction, published)| PlannedReaction {
            comment: reaction.comment.clone(),
            forge_id: published.id.clone(),
            kind: reaction.kind,
        })
        .collect();
    plan
}

/// Where the commented lines sit at head, when all of them survive there
/// as one unbroken run. Lines inserted into the span since, or a part of
/// it rewritten, would stretch the anchor over code the comment never saw.
fn anchor_at_head(comment: &Comment, blame: &[BlameLine]) -> Option<LineAnchor> {
    let lines = comment.lines.clamp(1, comment.line.max(1));
    let final_line = |origin_line: usize| {
        blame
            .iter()
            .find(|row| row.origin.commit == comment.commit && row.origin.line == origin_line)
            .map(|row| row.final_line)
    };
    let last = final_line(comment.line)?;
    for back in 1..lines {
        if final_line(comment.line - back)? + back != last {
            return None;
        }
    }
    Some(LineAnchor {
        side: Side::New,
        line: last,
        lines,
    })
}

/// The comment as the forge shows it beside its line: a dragged selection
/// leads as a quote, because the forge only marks whole lines.
fn body(comment: &Comment) -> String {
    match &comment.selection {
        Some(selection) => format!("> {}\n\n{}", code_span(selection), comment.body),
        None => comment.body.clone(),
    }
}

/// The comment for the file's own thread: no diff row sits beside it, so
/// it names where it was written and quotes the code.
fn file_body(comment: &Comment) -> String {
    let first = comment.line + 1 - comment.lines.min(comment.line);
    let removed = match comment.side {
        Side::Old => "removed ",
        Side::New => "",
    };
    let lines = if comment.lines > 1 {
        format!("{removed}lines {first}-{}", comment.line)
    } else {
        format!("{removed}line {}", comment.line)
    };
    let fence = "`".repeat(longest_backtick_run(&comment.snippet).max(2) + 1);
    format!(
        "On {lines} as of `{}`:\n\n{fence}\n{}\n{fence}\n\n{}",
        comment.commit.short(),
        comment.snippet.trim_end_matches('\n'),
        body(comment)
    )
}

/// Inline code that survives backticks in `text`: the delimiter is one
/// backtick longer than the longest run inside.
fn code_span(text: &str) -> String {
    let text = text.replace('\n', " ");
    let ticks = "`".repeat(longest_backtick_run(&text) + 1);
    // A leading or trailing backtick would merge with the delimiter.
    let pad = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{ticks}{pad}{text}{pad}{ticks}")
}

fn longest_backtick_run(text: &str) -> usize {
    text.split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or_default()
}

/// What a publish achieved. Partial on purpose: what landed must be
/// recorded even when a later call failed - or the next publish would post
/// it again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// `(comment id, body as sent, the forge's comment)`.
    published: Vec<(String, String, PostedComment)>,
    /// `(comment id, body as sent)`.
    edited: Vec<(String, String)>,
    reacted: Vec<(String, ReactionKind)>,
    /// Comments the forge would not take; they stay unpublished.
    pub refused: usize,
    /// The verdict the forge took, when the review carried one beyond its
    /// comments: an approval or a request for changes.
    pub verdict: Option<Verdict>,
    /// The first failure, in the forge's words.
    pub error: Option<String>,
}

impl Outcome {
    pub fn done(&self) -> Pending {
        Pending {
            comments: self.published.len(),
            edits: self.edited.len(),
            reactions: self.reacted.len(),
        }
    }

    /// How many of the published comments landed on their file because no
    /// line would take them.
    pub fn on_file(&self) -> usize {
        self.published
            .iter()
            .filter(|(_, _, posted)| posted.on_file)
            .count()
    }

    /// Whether the edits and reactions may follow. Not behind a failed
    /// review: one click never half-publishes around it.
    pub fn review_landed(&self) -> bool {
        self.error.is_none() || !self.published.is_empty() || self.verdict.is_some()
    }

    /// Folds a later step's outcome into this one; the first error stays.
    pub fn merge(&mut self, later: Outcome) {
        self.published.extend(later.published);
        self.edited.extend(later.edited);
        self.reacted.extend(later.reacted);
        self.refused += later.refused;
        self.verdict = self.verdict.or(later.verdict);
        if self.error.is_none() {
            self.error = later.error;
        }
    }
}

/// Sends all of `plan`. Blocks on the forge; call it from a background
/// task. A caller that can save in between runs the two steps itself, so
/// the review's ids are on disk before the slower one-by-one calls start.
pub fn publish(forge: &dyn Forge, pull: &PullRequest, plan: &Plan) -> Outcome {
    let mut outcome = publish_review(forge, pull, plan);
    if outcome.review_landed() {
        outcome.merge(publish_followups(forge, pull, plan));
    }
    outcome
}

/// First step: the review with its comments and verdict, in one call.
pub fn publish_review(forge: &dyn Forge, pull: &PullRequest, plan: &Plan) -> Outcome {
    let mut outcome = Outcome::default();
    let verdict = plan.review.verdict;
    if verdict.needs_comments() && plan.review.comments.is_empty() {
        // The menu never offers this; a caller that asks anyway hears why
        // instead of seeing a silent nothing.
        if verdict == Verdict::RequestChanges {
            outcome.error =
                Some("requesting changes needs at least one new comment to publish".into());
        }
        return outcome;
    }
    let landed = (verdict != Verdict::Comment).then_some(verdict);
    match forge.submit_review(pull, &plan.review) {
        Ok(postings) if postings.len() == plan.comments.len() => {
            outcome.verdict = landed;
            for ((id, body), posting) in plan.comments.iter().zip(postings) {
                match posting {
                    Posting::Posted(posted) => {
                        outcome.published.push((id.clone(), body.clone(), posted));
                    }
                    Posting::Refused(reason) => {
                        outcome.refused += 1;
                        outcome.error.get_or_insert(reason);
                    }
                }
            }
        }
        Ok(postings) => {
            // Which comment got which id is unknowable now; say so
            // instead of linking cards to the wrong threads.
            outcome.error = Some(format!(
                "the review was posted, but {} answered {} comments for {}",
                forge.name(),
                postings.len(),
                plan.comments.len()
            ));
            outcome.verdict = landed;
        }
        Err(error) => outcome.error = Some(error.to_string()),
    }
    outcome
}

/// Second step: edits and reactions, one call each. Every one is tried;
/// a failed one stays owed and the first failure is reported.
pub fn publish_followups(forge: &dyn Forge, pull: &PullRequest, plan: &Plan) -> Outcome {
    let mut outcome = Outcome::default();
    for edit in &plan.edits {
        match forge.update_comment(pull, &edit.forge_id, &edit.rendered) {
            Ok(()) => outcome
                .edited
                .push((edit.comment.clone(), edit.body.clone())),
            Err(error) => {
                outcome.error.get_or_insert(error.to_string());
            }
        }
    }
    for reaction in &plan.reactions {
        match forge.react(pull, &reaction.forge_id, reaction.kind) {
            Ok(()) => outcome
                .reacted
                .push((reaction.comment.clone(), reaction.kind)),
            Err(error) => {
                outcome.error.get_or_insert(error.to_string());
            }
        }
    }
    outcome
}

impl ReviewFile {
    /// Writes a publish's results into the reviewer's own file. The file
    /// may have moved on while the publish ran: a comment deleted since is
    /// skipped, and one edited since is marked stale so the next publish
    /// carries the edit. Returns whether anything changed.
    pub fn record(&mut self, outcome: &Outcome) -> bool {
        let at = now_rfc3339();
        let mut changed = false;
        for (id, sent, posted) in &outcome.published {
            if let Some(comment) = self.comments.iter_mut().find(|comment| &comment.id == id) {
                comment.published = Some(Published {
                    id: posted.id.clone(),
                    url: posted.url.clone(),
                    at: at.clone(),
                    stale: &comment.body != sent,
                    on_file: posted.on_file,
                });
                changed = true;
            }
        }
        for (id, sent) in &outcome.edited {
            if let Some(published) = self
                .comments
                .iter_mut()
                .find(|comment| &comment.id == id && &comment.body == sent)
                .and_then(|comment| comment.published.as_mut())
            {
                published.stale = false;
                published.at = at.clone();
                changed = true;
            }
        }
        for (id, kind) in &outcome.reacted {
            if let Some(reaction) = self
                .reactions
                .iter_mut()
                .find(|reaction| &reaction.comment == id && reaction.kind == *kind)
            {
                reaction.published = Some(at.clone());
                changed = true;
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::model::{CompareMode, Comparison, LineOrigin, Resolved};
    use crate::pr::PrState;
    use crate::review::NewComment;

    const HEAD: &str = "hhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhh";

    fn reviews() -> ReviewSet {
        ReviewSet::new(ReviewFile::new(
            "Ada",
            &Resolved {
                comparison: Comparison {
                    source: "topic".into(),
                    target: "main".into(),
                    mode: CompareMode::BranchAdds,
                    uncommitted: false,
                    ..Default::default()
                },
                head: Oid::new(HEAD),
                target_head: Oid::new("t".repeat(40)),
                base: Oid::new("b".repeat(40)),
            },
        ))
    }

    fn comment(path: &str, line: usize, body: &str) -> NewComment {
        NewComment {
            commit: Oid::new("a3f9c1e0"),
            path: path.into(),
            side: Side::New,
            line,
            lines: 1,
            selection: None,
            snippet: "let x = 1;".into(),
            body: body.into(),
            suggestion: None,
        }
    }

    fn pull() -> PullRequest {
        PullRequest {
            number: 17,
            id: "PR_1".into(),
            url: "https://github.com/o/r/pull/17".into(),
            state: PrState::Open,
            source_branch: "topic".into(),
            source_commit: HEAD.into(),
            target_branch: "main".into(),
            target_commit: "b".repeat(40),
            cross_repository: false,
            head_owner: "o".into(),
        }
    }

    /// The row a list shows for a stack member. The double knows no
    /// titles, authors or checks; what a stack needs is the branches.
    fn summary(pull: &PullRequest) -> PrSummary {
        PrSummary {
            number: pull.number,
            title: format!("#{}", pull.number),
            author: "Ada".into(),
            updated: 0,
            state: pull.state,
            draft: false,
            checks: None,
            head: pull.source_branch.clone(),
            base: pull.target_branch.clone(),
        }
    }

    /// Blame that moved every line of the commit down by `shift`.
    fn shifted(shift: usize) -> impl FnMut(&str) -> Option<Vec<BlameLine>> {
        move |_| {
            Some(
                (1..=50)
                    .map(|line| BlameLine {
                        final_line: line + shift,
                        origin: LineOrigin {
                            commit: Oid::new("a3f9c1e0"),
                            line,
                        },
                    })
                    .collect(),
            )
        }
    }

    fn plan_all(reviews: &ReviewSet) -> Plan {
        plan(reviews, &Oid::new(HEAD), Verdict::Comment, true, shifted(0))
    }

    #[derive(Default)]
    struct FakeForge {
        calls: Mutex<Vec<String>>,
        fail_review: bool,
        fail_updates: bool,
        /// Answer one comment fewer than asked.
        short_answer: bool,
        /// Post unanchored comments' siblings, refuse these paths.
        refuse_path: Option<&'static str>,
        /// The stack `pull()` belongs to; `None` is a pull request in none.
        stack: Option<Stack>,
    }

    impl FakeForge {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Forge for FakeForge {
        fn name(&self) -> &'static str {
            "Fake"
        }

        fn pull_request(&self, _: &PrArgument) -> Result<PullRequest, ForgeError> {
            Ok(pull())
        }

        fn pull_requests(&self, _: PrList) -> Result<Vec<PrSummary>, ForgeError> {
            Ok(Vec::new())
        }

        fn stack(&self, _: &PrArgument) -> Result<Option<Stack>, ForgeError> {
            Ok(self.stack.clone())
        }

        fn stack_numbered(&self, _: u64) -> Result<Option<Stack>, ForgeError> {
            Ok(self.stack.clone())
        }

        fn stacks(&self) -> Result<Vec<StackSummary>, ForgeError> {
            Ok(self
                .stack
                .iter()
                .map(|stack| StackSummary {
                    number: stack.number,
                    base: stack
                        .pulls
                        .last()
                        .map(|first| first.target_branch.clone())
                        .unwrap_or_default(),
                    members: stack.pulls.iter().map(summary).collect(),
                })
                .collect())
        }

        fn status(&self, _: &PullRequest) -> Result<PullStatus, ForgeError> {
            Ok(PullStatus {
                state: PrState::Open,
                review: None,
                checks: None,
            })
        }

        fn submit_review(
            &self,
            _: &PullRequest,
            review: &ReviewDraft,
        ) -> Result<Vec<Posting>, ForgeError> {
            self.calls.lock().unwrap().push(format!(
                "review {:?} x{}",
                review.verdict,
                review.comments.len()
            ));
            if self.fail_review {
                return Err(ForgeError::Refused("no".into()));
            }
            Ok(review
                .comments
                .iter()
                .enumerate()
                .skip(usize::from(self.short_answer))
                .map(|(index, comment)| {
                    if self.refuse_path == Some(comment.path.as_str()) {
                        return Posting::Refused("path is not in the diff".into());
                    }
                    Posting::Posted(PostedComment {
                        id: format!("C{index}"),
                        url: format!("https://forge/c/{index}"),
                        on_file: comment.anchor.is_none(),
                    })
                })
                .collect())
        }

        fn update_comment(&self, _: &PullRequest, id: &str, body: &str) -> Result<(), ForgeError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("update {id} {body}"));
            if self.fail_updates {
                return Err(ForgeError::Refused("locked".into()));
            }
            Ok(())
        }

        fn react(&self, _: &PullRequest, id: &str, kind: ReactionKind) -> Result<(), ForgeError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("react {id} {}", kind.label()));
            Ok(())
        }
    }

    #[test]
    fn plans_open_own_comments_at_their_blamed_head_line() {
        let mut set = reviews();
        set.mine.add_comment(comment("src/a.rs", 10, "why?"));
        let plan = plan(&set, &Oid::new(HEAD), Verdict::Comment, true, shifted(3));
        assert_eq!(plan.review.commit, Oid::new(HEAD));
        assert_eq!(
            plan.review.comments[0].anchor,
            Some(LineAnchor {
                side: Side::New,
                line: 13,
                lines: 1
            })
        );
        assert_eq!(plan.review.comments[0].body, "why?");
        assert_eq!(plan.pending().comments, 1);
    }

    #[test]
    fn a_line_blame_lost_goes_on_the_file_with_its_code_quoted() {
        let mut set = reviews();
        set.mine.add_comment(NewComment {
            lines: 2,
            snippet: "a\n```\nb\n".into(),
            ..comment("src/a.rs", 99, "rewritten since")
        });
        let draft = &plan_all(&set).review.comments[0];
        assert_eq!(draft.anchor, None);
        assert_eq!(
            draft.file_body,
            "On lines 98-99 as of `a3f9c1e`:\n\n````\na\n```\nb\n````\n\nrewritten since"
        );
    }

    #[test]
    fn a_file_gone_at_head_goes_on_the_file() {
        let mut set = reviews();
        set.mine.add_comment(comment("gone.rs", 1, "x"));
        let plan = plan(&set, &Oid::new(HEAD), Verdict::Comment, true, |_| None);
        assert_eq!(plan.review.comments[0].anchor, None);
    }

    #[test]
    fn old_side_comments_go_on_the_file() {
        let mut set = reviews();
        set.mine.add_comment(NewComment {
            side: Side::Old,
            ..comment("src/a.rs", 7, "why removed?")
        });
        // No blame can answer for a removed line; it must not be asked.
        let plan = plan(&set, &Oid::new(HEAD), Verdict::Comment, true, |_| {
            panic!("blamed an old-side comment")
        });
        let draft = &plan.review.comments[0];
        assert_eq!(draft.anchor, None);
        assert!(
            draft.file_body.starts_with("On removed line 7 as of"),
            "{}",
            draft.file_body
        );
    }

    #[test]
    fn a_span_broken_up_at_head_goes_on_the_file() {
        let mut set = reviews();
        set.mine.add_comment(NewComment {
            lines: 3,
            ..comment("src/a.rs", 10, "this block")
        });
        // Whole at head, merely shifted: the span keeps its lines.
        let whole = plan(&set, &Oid::new(HEAD), Verdict::Comment, true, shifted(4));
        assert_eq!(
            whole.review.comments[0].anchor,
            Some(LineAnchor {
                side: Side::New,
                line: 14,
                lines: 3
            })
        );
        // A line inserted inside the span pushes its end away from its start.
        let split = plan(&set, &Oid::new(HEAD), Verdict::Comment, true, |path| {
            let mut rows = shifted(0)(path)?;
            for row in rows.iter_mut().filter(|row| row.origin.line >= 10) {
                row.final_line += 1;
            }
            Some(rows)
        });
        assert_eq!(split.review.comments[0].anchor, None);
    }

    #[test]
    fn an_edit_of_a_file_level_comment_keeps_its_code_quote() {
        let mut set = reviews();
        let id = set.mine.add_comment(comment("gone.rs", 4, "one"));
        let forge = FakeForge::default();
        let first = plan(&set, &Oid::new(HEAD), Verdict::Comment, true, |_| None);
        set.mine.record(&publish(&forge, &pull(), &first));
        assert!(set.mine.comments[0].published.as_ref().unwrap().on_file);

        set.mine.edit_comment(&id, "two".into());
        publish(&forge, &pull(), &plan_all(&set));
        assert_eq!(
            forge.calls()[1],
            "update C0 On line 4 as of `a3f9c1e`:\n\n```\nlet x = 1;\n```\n\ntwo"
        );
    }

    #[test]
    fn a_refused_comment_stays_owed_and_the_rest_is_recorded() {
        let mut set = reviews();
        set.mine.add_comment(comment("src/a.rs", 1, "fine"));
        set.mine
            .add_comment(comment("vanished.rs", 1, "nowhere to go"));
        let forge = FakeForge {
            refuse_path: Some("vanished.rs"),
            ..Default::default()
        };
        let outcome = publish(&forge, &pull(), &plan_all(&set));
        assert_eq!(outcome.refused, 1);
        assert_eq!(outcome.error.as_deref(), Some("path is not in the diff"));
        set.mine.record(&outcome);
        assert!(set.mine.comments[0].published.is_some());
        assert!(set.mine.comments[1].published.is_none());
        assert_eq!(pending(&set).comments, 1);
    }

    #[test]
    fn pending_counts_what_plan_would_send() {
        let mut set = reviews();
        let published = set.mine.add_comment(comment("src/a.rs", 1, "one"));
        set.mine
            .record(&publish(&FakeForge::default(), &pull(), &plan_all(&set)));
        set.mine.edit_comment(&published, "edited".into());
        set.mine.toggle_reaction(&published, ReactionKind::Seen);
        set.mine.add_comment(comment("src/a.rs", 2, "two"));
        let resolved = set.mine.add_comment(comment("src/a.rs", 3, "three"));
        set.mine.toggle_reaction(&resolved, ReactionKind::Done);
        assert_eq!(pending(&set), plan_all(&set).pending());
        assert_eq!(pending(&set).total(), 3);
    }

    #[test]
    fn the_review_step_alone_reports_ids_before_the_followups_run() {
        let mut set = reviews();
        let published = set.mine.add_comment(comment("src/a.rs", 1, "one"));
        set.mine
            .record(&publish(&FakeForge::default(), &pull(), &plan_all(&set)));
        set.mine.edit_comment(&published, "edited".into());
        set.mine.add_comment(comment("src/a.rs", 2, "two"));

        let forge = FakeForge::default();
        let plan = plan_all(&set);
        let review = publish_review(&forge, &pull(), &plan);
        assert_eq!(forge.calls(), vec!["review Comment x1"]);
        assert!(review.review_landed());
        assert!(set.mine.record(&review));
        assert!(set.mine.comments[1].published.is_some());

        let followups = publish_followups(&forge, &pull(), &plan);
        assert_eq!(followups.done().edits, 1);
    }

    #[test]
    fn a_selection_leads_the_body_as_a_quote() {
        let mut set = reviews();
        set.mine.add_comment(NewComment {
            selection: Some("a `b` c".into()),
            ..comment("src/a.rs", 1, "rename")
        });
        assert_eq!(
            plan_all(&set).review.comments[0].body,
            "> ``a `b` c``\n\nrename"
        );
    }

    #[test]
    fn skips_resolved_published_and_colleagues_comments() {
        let mut set = reviews();
        let resolved = set.mine.add_comment(comment("src/a.rs", 1, "settled"));
        set.mine.toggle_reaction(&resolved, ReactionKind::Done);
        let open = set.mine.add_comment(comment("src/a.rs", 2, "open"));
        let mut theirs = reviews().mine;
        theirs.review.author = "Grace".into();
        theirs.add_comment(comment("src/a.rs", 3, "hers"));
        set.add_other(theirs);

        let first = plan_all(&set);
        assert_eq!(first.comments, vec![(open, "open".to_string())]);

        set.mine
            .record(&publish(&FakeForge::default(), &pull(), &first));
        assert!(plan_all(&set).is_empty());
    }

    #[test]
    fn publish_records_ids_and_a_second_publish_posts_nothing() {
        let mut set = reviews();
        let id = set.mine.add_comment(comment("src/a.rs", 1, "one"));
        let forge = FakeForge::default();
        let outcome = publish(&forge, &pull(), &plan_all(&set));
        assert_eq!(outcome.error, None);
        assert_eq!(outcome.verdict, None);
        assert!(set.mine.record(&outcome));
        let published = set.mine.comments[0].published.clone().unwrap();
        assert_eq!(
            (published.id.as_str(), published.url.as_str()),
            ("C0", "https://forge/c/0")
        );
        assert!(!published.stale);

        let again = plan_all(&set);
        assert!(again.is_empty());
        publish(&forge, &pull(), &again);
        assert_eq!(forge.calls(), vec!["review Comment x1"]);
        let _ = id;
    }

    #[test]
    fn an_edit_after_publishing_updates_the_forge_copy_once() {
        let mut set = reviews();
        let id = set.mine.add_comment(comment("src/a.rs", 1, "one"));
        let forge = FakeForge::default();
        set.mine.record(&publish(&forge, &pull(), &plan_all(&set)));

        // Saving the box unchanged is not an edit.
        set.mine.edit_comment(&id, "one".into());
        assert!(plan_all(&set).is_empty());

        set.mine.edit_comment(&id, "two".into());
        let plan = plan_all(&set);
        assert_eq!(
            plan.pending(),
            Pending {
                comments: 0,
                edits: 1,
                reactions: 0
            }
        );
        set.mine.record(&publish(&forge, &pull(), &plan));
        assert_eq!(forge.calls()[1], "update C0 two");
        assert!(plan_all(&set).is_empty());
    }

    #[test]
    fn an_edit_made_while_publishing_stays_pending() {
        let mut set = reviews();
        let id = set.mine.add_comment(comment("src/a.rs", 1, "one"));
        let outcome = publish(&FakeForge::default(), &pull(), &plan_all(&set));
        // Not yet published when edited, so `edit_comment` cannot flag it.
        set.mine.edit_comment(&id, "one, reworded".into());
        set.mine.record(&outcome);
        assert!(set.mine.comments[0].published.as_ref().unwrap().stale);
        assert_eq!(plan_all(&set).pending().edits, 1);
    }

    #[test]
    fn a_comment_deleted_while_publishing_is_skipped() {
        let mut set = reviews();
        let id = set.mine.add_comment(comment("src/a.rs", 1, "one"));
        let outcome = publish(&FakeForge::default(), &pull(), &plan_all(&set));
        set.mine.delete_comment(&id);
        assert!(!set.mine.record(&outcome));
    }

    #[test]
    fn reactions_follow_once_their_comment_is_on_the_forge() {
        let mut set = reviews();
        let id = set.mine.add_comment(comment("src/a.rs", 1, "one"));
        set.mine.toggle_reaction(&id, ReactionKind::Seen);
        let forge = FakeForge::default();

        // The comment has no forge id yet, so the reaction has no target.
        let first = plan_all(&set);
        assert_eq!(first.pending().reactions, 0);
        set.mine.record(&publish(&forge, &pull(), &first));

        let second = plan_all(&set);
        assert_eq!(second.pending().reactions, 1);
        set.mine.record(&publish(&forge, &pull(), &second));
        assert_eq!(forge.calls()[1], "react C0 Seen");
        assert!(set.mine.reactions[0].published.is_some());
        assert!(plan_all(&set).is_empty());
    }

    #[test]
    fn reacts_to_a_colleagues_published_comment() {
        let mut set = reviews();
        let mut theirs = reviews().mine;
        theirs.review.author = "Grace".into();
        let id = theirs.add_comment(comment("src/a.rs", 3, "hers"));
        theirs.comments[0].published = Some(Published {
            id: "C9".into(),
            url: "https://forge/c/9".into(),
            at: now_rfc3339(),
            stale: false,
            on_file: false,
        });
        set.add_other(theirs);
        set.mine.toggle_reaction(&id, ReactionKind::WontFix);

        let forge = FakeForge::default();
        publish(&forge, &pull(), &plan_all(&set));
        assert_eq!(forge.calls(), vec!["react C9 Won't fix"]);
    }

    #[test]
    fn approving_without_comments_sends_the_verdict_alone() {
        let mut set = reviews();
        let id = set.mine.add_comment(comment("src/a.rs", 1, "one"));
        set.mine.toggle_reaction(&id, ReactionKind::Seen);
        let plan = plan(&set, &Oid::new(HEAD), Verdict::Approve, false, |_| {
            panic!("blamed for a bare approval")
        });
        assert!(!plan.is_empty());
        let forge = FakeForge::default();
        let outcome = publish(&forge, &pull(), &plan);
        assert_eq!(outcome.verdict, Some(Verdict::Approve));
        assert_eq!(forge.calls(), vec!["review Approve x0"]);
        assert!(!set.mine.record(&outcome));
    }

    #[test]
    fn requesting_changes_carries_its_comments() {
        let mut set = reviews();
        set.mine
            .add_comment(comment("src/a.rs", 1, "this must change"));
        let plan = plan(
            &set,
            &Oid::new(HEAD),
            Verdict::RequestChanges,
            true,
            shifted(0),
        );
        let forge = FakeForge::default();
        let outcome = publish(&forge, &pull(), &plan);
        assert_eq!(outcome.verdict, Some(Verdict::RequestChanges));
        assert_eq!(outcome.error, None);
        assert_eq!(forge.calls(), vec!["review RequestChanges x1"]);
        assert!(set.mine.record(&outcome));
    }

    #[test]
    fn requesting_changes_without_a_new_comment_is_refused_unsent() {
        let mut set = reviews();
        // An edit and a reaction are owed, but no comment is new.
        let id = set.mine.add_comment(comment("src/a.rs", 1, "one"));
        set.mine
            .record(&publish(&FakeForge::default(), &pull(), &plan_all(&set)));
        set.mine.edit_comment(&id, "edited".into());
        set.mine.toggle_reaction(&id, ReactionKind::Seen);

        let plan = plan(
            &set,
            &Oid::new(HEAD),
            Verdict::RequestChanges,
            true,
            shifted(0),
        );
        let forge = FakeForge::default();
        let outcome = publish(&forge, &pull(), &plan);
        assert_eq!(outcome.verdict, None);
        assert!(outcome.error.as_deref().unwrap().contains("new comment"));
        // Nothing went out, the owed edit and reaction included: they are
        // not what the reviewer clicked for.
        assert!(forge.calls().is_empty());
    }

    #[test]
    fn approving_with_comments_posts_them_in_the_approval() {
        let mut set = reviews();
        set.mine.add_comment(comment("src/a.rs", 1, "nit"));
        let plan = plan(&set, &Oid::new(HEAD), Verdict::Approve, true, shifted(0));
        let forge = FakeForge::default();
        let outcome = publish(&forge, &pull(), &plan);
        assert_eq!(outcome.verdict, Some(Verdict::Approve));
        assert_eq!(forge.calls(), vec!["review Approve x1"]);
        assert_eq!(outcome.done().comments, 1);
    }

    #[test]
    fn a_refused_review_sends_nothing_else_and_records_nothing() {
        let mut set = reviews();
        let published = set.mine.add_comment(comment("src/a.rs", 1, "one"));
        set.mine
            .record(&publish(&FakeForge::default(), &pull(), &plan_all(&set)));
        set.mine.edit_comment(&published, "edited".into());
        set.mine.add_comment(comment("src/a.rs", 2, "two"));

        let forge = FakeForge {
            fail_review: true,
            ..Default::default()
        };
        let outcome = publish(&forge, &pull(), &plan_all(&set));
        assert_eq!(outcome.error.as_deref(), Some("no"));
        assert_eq!(forge.calls(), vec!["review Comment x1"]);
        assert!(!set.mine.record(&outcome));
    }

    #[test]
    fn a_failed_edit_keeps_what_the_review_posted() {
        let mut set = reviews();
        let first = set.mine.add_comment(comment("src/a.rs", 1, "one"));
        set.mine
            .record(&publish(&FakeForge::default(), &pull(), &plan_all(&set)));
        set.mine.edit_comment(&first, "edited".into());
        set.mine.add_comment(comment("src/a.rs", 2, "two"));

        let forge = FakeForge {
            fail_updates: true,
            ..Default::default()
        };
        let outcome = publish(&forge, &pull(), &plan_all(&set));
        assert_eq!(outcome.error.as_deref(), Some("locked"));
        set.mine.record(&outcome);
        assert!(set.mine.comments[1].published.is_some());
        // The edit failed, so it is still owed.
        assert_eq!(plan_all(&set).pending().edits, 1);
    }

    #[test]
    fn a_miscounted_answer_links_nothing() {
        let mut set = reviews();
        set.mine.add_comment(comment("src/a.rs", 1, "one"));
        let forge = FakeForge {
            short_answer: true,
            ..Default::default()
        };
        let outcome = publish(&forge, &pull(), &plan_all(&set));
        assert!(outcome.error.is_some());
        assert!(!set.mine.record(&outcome));
    }

    #[test]
    fn a_span_never_starts_above_the_first_line() {
        let mut set = reviews();
        set.mine.add_comment(NewComment {
            lines: 5,
            ..comment("src/a.rs", 2, "top")
        });
        assert_eq!(plan_all(&set).review.comments[0].anchor.unwrap().lines, 2);
    }

    #[test]
    fn published_state_round_trips_through_toml() {
        let mut set = reviews();
        let id = set.mine.add_comment(comment("src/a.rs", 1, "one"));
        set.mine
            .record(&publish(&FakeForge::default(), &pull(), &plan_all(&set)));
        set.mine.toggle_reaction(&id, ReactionKind::Done);
        set.mine
            .record(&publish(&FakeForge::default(), &pull(), &plan_all(&set)));
        let text = toml::to_string_pretty(&set.mine).unwrap();
        assert_eq!(toml::from_str::<ReviewFile>(&text).unwrap(), set.mine);
        assert!(text.contains("[comment.published]"), "{text}");
    }
}
