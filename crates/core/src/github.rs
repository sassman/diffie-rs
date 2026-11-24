//! GitHub as a [`Forge`], through the GitHub CLI.
//!
//! gh does the talking because it already carries the user's authentication
//! and knows GitHub Enterprise hosts. Its embedded jq (`--jq`) hands back
//! plain lines, so the crate stays free of HTTP and JSON dependencies.
//!
//! Writes go through GraphQL: only there can a review be opened pending,
//! take its threads one by one - each answering with its own id, each free
//! to fall back to a file-level thread when its line is refused, or to
//! stay out when its path is - and then be submitted as a single review
//! with a single notification. REST's one-shot review rejects the whole
//! batch for one bad line and returns no comment ids.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::forge::{
    DraftComment, Forge, ForgeError, LineAnchor, PostedComment, Posting, ReviewDraft, Verdict,
};
use crate::pr::{
    CheckState, Checks, PrArgument, PrList, PrState, PrSummary, PullRequest, PullStatus,
    ReviewDecision, Stack, StackSummary,
};
use crate::review::{ReactionKind, Side};
use crate::time::parse_rfc3339;

pub struct GitHubCli {
    /// The checkout; gh finds the repository from its remotes.
    repository: PathBuf,
    program: OsString,
}

/// One GraphQL variable as gh takes it.
enum Variable<'a> {
    /// `-f`: always a string, never read as a file or a number.
    Text(&'a str, &'a str),
    /// `-F`: typed, for the integers a line anchor needs.
    Number(&'a str, usize),
}

const ADD_REVIEW: &str = "mutation($pull:ID!,$commit:GitObjectID!){\
    addPullRequestReview(input:{pullRequestId:$pull,commitOID:$commit}){pullRequestReview{id}}}";
const APPROVE: &str = "mutation($pull:ID!,$commit:GitObjectID!){\
    addPullRequestReview(input:{pullRequestId:$pull,commitOID:$commit,event:APPROVE})\
    {pullRequestReview{id}}}";
const ADD_LINE_THREAD: &str = "mutation($review:ID!,$path:String!,$body:String!,$line:Int!,\
    $side:DiffSide!,$startLine:Int,$startSide:DiffSide){\
    addPullRequestReviewThread(input:{pullRequestReviewId:$review,path:$path,body:$body,\
    line:$line,side:$side,startLine:$startLine,startSide:$startSide})\
    {thread{comments(first:1){nodes{id url}}}}}";
const ADD_FILE_THREAD: &str = "mutation($review:ID!,$path:String!,$body:String!){\
    addPullRequestReviewThread(input:{pullRequestReviewId:$review,path:$path,body:$body,\
    subjectType:FILE}){thread{comments(first:1){nodes{id url}}}}}";
const THREAD_COMMENT: &str =
    ".data.addPullRequestReviewThread.thread.comments.nodes[0] | .id, .url";
const SUBMIT_REVIEW: &str = "mutation($review:ID!,$event:PullRequestReviewEvent!){\
    submitPullRequestReview(input:{pullRequestReviewId:$review,event:$event})\
    {pullRequestReview{id}}}";
const DELETE_REVIEW: &str = "mutation($review:ID!){\
    deletePullRequestReview(input:{pullRequestReviewId:$review}){pullRequestReview{id}}}";
const UPDATE_COMMENT: &str = "mutation($comment:ID!,$body:String!){\
    updatePullRequestReviewComment(input:{pullRequestReviewCommentId:$comment,body:$body})\
    {pullRequestReviewComment{id}}}";
const ADD_REACTION: &str = "mutation($subject:ID!,$content:ReactionContent!){\
    addReaction(input:{subjectId:$subject,content:$content}){reaction{content}}}";
/// GitHub's native stack, read from any member: one query carries every
/// field a [`PullRequest`] needs, so resolving a stack costs no second
/// call per member. A number that names no pull request comes back as a
/// GraphQL error, which gh fails the call on, so the two answers stay
/// apart: an `Err` is "no such pull request", empty output "in no stack".
const STACK: &str = "query($owner:String!,$name:String!,$number:Int!,$after:String){\
    repository(owner:$owner,name:$name){pullRequest(number:$number){stack{number \
    entries(first:50,after:$after){pageInfo{hasNextPage endCursor}nodes{position \
    pullRequest{number id url state headRefName headRefOid baseRefName baseRefOid \
    isCrossRepository headRepositoryOwner{login}}}}}}}}";
/// Nothing at all for a pull request in no stack. Then the page's cursor
/// and the stack's number on one line, and one `@tsv` line per member.
const STACK_ROWS: &str = ".data.repository.pullRequest.stack as $stack | \
    if $stack == null then empty else \
    ([($stack.entries.pageInfo.hasNextPage|tostring), \
    ($stack.entries.pageInfo.endCursor // \"\"), ($stack.number|tostring)] | @tsv), \
    ($stack.entries.nodes[] | [(.position|tostring), (.pullRequest.number|tostring), \
    .pullRequest.id, .pullRequest.url, .pullRequest.state, .pullRequest.headRefName, \
    .pullRequest.headRefOid, .pullRequest.baseRefName, .pullRequest.baseRefOid, \
    (.pullRequest.isCrossRepository|tostring), \
    (.pullRequest.headRepositoryOwner.login // \"\")] | @tsv) end";
/// One page of pull requests, each with the stack it belongs to: the
/// stack list is that page grouped, not a walk of the archive.
const STACKS: &str = "query($owner:String!,$name:String!,$first:Int!){\
    repository(owner:$owner,name:$name){pullRequests(first:$first,states:OPEN,\
    orderBy:{field:UPDATED_AT,direction:DESC}){nodes{number title state isDraft \
    headRefName baseRefName updatedAt author{login} \
    commits(last:1){nodes{commit{statusCheckRollup{state}}}} \
    stackEntry{position}stack{number baseRefName}}}}}";
/// The stack's number, its base branch and the member's position, then
/// the columns [`parse_summary`] reads - the rollup as one `;;<state>`
/// triple, since GraphQL folds the checks for us.
const STACK_LIST: &str = ".data.repository.pullRequests.nodes[] | \
    select(.stack != null) | [(.stack.number|tostring), .stack.baseRefName, \
    (.stackEntry.position|tostring), (.number|tostring), .title, \
    (.author.login // \"\"), .updatedAt, (.isDraft|tostring), .state, \
    .headRefName, .baseRefName, \
    (.commits.nodes[0].commit.statusCheckRollup.state // \"\" | \
    if . == \"\" then \"\" else \";;\" + . end)] | @tsv";

impl GitHubCli {
    pub fn new(repository: &Path) -> Self {
        Self::with_program(repository, "gh")
    }

    /// Runs `program` in gh's place; the tests put a script there.
    pub fn with_program(repository: &Path, program: impl Into<OsString>) -> Self {
        Self {
            repository: repository.to_path_buf(),
            program: program.into(),
        }
    }

    fn run(&self, configure: impl FnOnce(&mut Command)) -> Result<String, ForgeError> {
        let mut command = Command::new(&self.program);
        configure(&mut command);
        let output = command
            .current_dir(&self.repository)
            // gh's errors reach the user verbatim; keep them in English.
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => ForgeError::ToolMissing {
                    tool: "the GitHub CLI (gh)",
                    error,
                },
                _ => ForgeError::Io(error),
            })?;
        if !output.status.success() {
            return Err(ForgeError::Refused(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// One GraphQL call against the pull request's host; `jq` picks the
    /// answer's lines. gh fails the call when the answer carries errors.
    fn graphql(
        &self,
        pull: &PullRequest,
        query: &str,
        variables: &[Variable<'_>],
        jq: &str,
    ) -> Result<String, ForgeError> {
        self.graphql_at(host_of(&pull.url), query, variables, jq)
    }

    /// [`Self::graphql`] where no pull request names the host yet: a stack
    /// is read before any of its members is resolved.
    fn graphql_at(
        &self,
        host: Option<&str>,
        query: &str,
        variables: &[Variable<'_>],
        jq: &str,
    ) -> Result<String, ForgeError> {
        self.run(|command| {
            command.args(["api", "graphql"]);
            // Without it gh asks github.com, also for an Enterprise pull
            // request named by URL.
            if let Some(host) = host {
                command.args(["--hostname", host]);
            }
            command.arg("-f").arg(format!("query={query}"));
            for variable in variables {
                match variable {
                    Variable::Text(name, value) => {
                        command.arg("-f").arg(format!("{name}={value}"));
                    }
                    Variable::Number(name, value) => {
                        command.arg("-F").arg(format!("{name}={value}"));
                    }
                }
            }
            command.args(["--jq", jq]);
        })
    }

    /// The repository a GraphQL query has to name. `pr view` and `pr list`
    /// find it themselves from a number, a URL or the checkout's remotes;
    /// GraphQL cannot, so a bare number costs one `repo view`.
    fn repository(&self, argument: &PrArgument) -> Result<Repo, ForgeError> {
        match argument.url.as_deref().and_then(repo_of) {
            Some(repo) => Ok(repo),
            None => self.local_repository(),
        }
    }

    fn local_repository(&self) -> Result<Repo, ForgeError> {
        let stdout = self.run(|command| {
            command.args(["repo", "view", "--json", "url", "--jq", ".url"]);
        })?;
        repo_of(stdout.trim()).ok_or_else(|| ForgeError::Output(stdout.trim().to_string()))
    }

    /// The stack holding pull request `number`, with the repository
    /// already resolved: [`Forge::stack_numbered`] learns that number
    /// through REST and must not pay for a second repository lookup.
    fn stack_of_pull(&self, repo: &Repo, number: u64) -> Result<Option<Stack>, ForgeError> {
        let mut stack = None;
        let mut members: Vec<(u64, PullRequest)> = Vec::new();
        let mut after = String::new();
        loop {
            let mut variables = vec![
                Variable::Text("owner", &repo.owner),
                Variable::Text("name", &repo.name),
                Variable::Number("number", number as usize),
            ];
            if !after.is_empty() {
                variables.push(Variable::Text("after", &after));
            }
            let stdout = self.graphql_at(Some(&repo.host), STACK, &variables, STACK_ROWS)?;
            let mut lines = stdout.lines().filter(|line| !line.is_empty());
            // No answer at all: the pull request carries no stack.
            let Some(header) = lines.next() else {
                return Ok(None);
            };
            let unexpected = || ForgeError::Output(header.to_string());
            let mut columns = header.split('\t');
            let mut column = || columns.next().ok_or_else(unexpected);
            let more = column()? == "true";
            let cursor = column()?.to_string();
            stack.get_or_insert(column()?.parse().map_err(|_| unexpected())?);
            for line in lines {
                members.push(parse_member(line)?);
            }
            // The cursor, not the stack's `size`: it answers directly
            // whether another page is waiting.
            if !more || cursor.is_empty() {
                break;
            }
            after = cursor;
        }
        // Latest first; `position` counts from the first member, the one
        // nearest the base branch.
        members.sort_by_key(|(position, _)| std::cmp::Reverse(*position));
        Ok(Some(Stack {
            number: stack,
            pulls: members.into_iter().map(|(_, pull)| pull).collect(),
        }))
    }

    fn add_thread(
        &self,
        pull: &PullRequest,
        review: &str,
        comment: &DraftComment,
        anchor: Option<LineAnchor>,
    ) -> Result<PostedComment, ForgeError> {
        let stdout = match anchor {
            Some(anchor) => {
                let side = diff_side(anchor.side);
                let mut variables = vec![
                    Variable::Text("review", review),
                    Variable::Text("path", &comment.path),
                    Variable::Text("body", &comment.body),
                    Variable::Number("line", anchor.line),
                    Variable::Text("side", side),
                ];
                if anchor.lines > 1 {
                    variables.push(Variable::Number(
                        "startLine",
                        anchor.line + 1 - anchor.lines,
                    ));
                    variables.push(Variable::Text("startSide", side));
                }
                self.graphql(pull, ADD_LINE_THREAD, &variables, THREAD_COMMENT)?
            }
            None => self.graphql(
                pull,
                ADD_FILE_THREAD,
                &[
                    Variable::Text("review", review),
                    Variable::Text("path", &comment.path),
                    Variable::Text("body", &comment.file_body),
                ],
                THREAD_COMMENT,
            )?,
        };
        let mut lines = stdout.lines();
        match (lines.next(), lines.next()) {
            // jq prints `null` for a thread GitHub declined without an error.
            (Some(id), Some(url)) if !id.is_empty() && id != "null" && url != "null" => {
                Ok(PostedComment {
                    id: id.to_string(),
                    url: url.to_string(),
                    on_file: anchor.is_none(),
                })
            }
            _ => Err(ForgeError::Output(stdout.trim().to_string())),
        }
    }

    /// One comment into the pending review: on its line, else on its file,
    /// else not at all.
    fn post(&self, pull: &PullRequest, review: &str, comment: &DraftComment) -> Posting {
        let on_line = comment
            .anchor
            .map(|anchor| self.add_thread(pull, review, comment, Some(anchor)));
        // A line outside the pull request's diff is refused; the comment
        // still belongs to the review, so it moves to the file. Any failure
        // takes this road - telling a refused line from a dropped
        // connection would mean matching gh's error texts - and the
        // result says where the comment landed, so nothing moves unseen.
        let posted = match on_line {
            Some(Ok(posted)) => Ok(posted),
            Some(Err(_)) | None => self.add_thread(pull, review, comment, None),
        };
        match posted {
            Ok(posted) => Posting::Posted(posted),
            // GitHub takes no thread on a path outside the pull request's
            // diff; the rest of the review must not wait for it.
            Err(error) => Posting::Refused(error.to_string()),
        }
    }
}

impl Forge for GitHubCli {
    fn name(&self) -> &'static str {
        "GitHub"
    }

    /// `repository` is gh's working directory, so a bare number resolves
    /// against this checkout's remotes.
    fn pull_request(&self, argument: &PrArgument) -> Result<PullRequest, ForgeError> {
        let selector = argument
            .url
            .clone()
            .unwrap_or_else(|| argument.number.to_string());
        let stdout = self.run(|command| {
            command.args(["pr", "view"]).arg(&selector).args([
                "--json",
                "id,url,state,headRefName,headRefOid,baseRefName,baseRefOid,isCrossRepository,headRepositoryOwner",
                // One value per line; `// ""` keeps the line count stable
                // when a fork's owner cannot be read.
                "--jq",
                ".id, .url, .state, .headRefName, .headRefOid, .baseRefName, .baseRefOid, .isCrossRepository, .headRepositoryOwner.login // \"\"",
            ]);
        })?;
        let unexpected = || ForgeError::Output(stdout.trim().to_string());
        let mut lines = stdout.lines();
        let mut next = || lines.next().ok_or_else(unexpected);
        let (id, url) = (next()?, next()?);
        let state = parse_state(next()?).ok_or_else(unexpected)?;
        let (source_branch, source_commit) = (next()?, next()?);
        let (target_branch, target_commit) = (next()?, next()?);
        let cross_repository = match next()? {
            "true" => true,
            "false" => false,
            _ => return Err(unexpected()),
        };
        if id.is_empty() || url.is_empty() || source_commit.is_empty() || target_commit.is_empty() {
            return Err(unexpected());
        }
        Ok(PullRequest {
            number: argument.number,
            id: id.to_string(),
            url: url.to_string(),
            state,
            source_branch: source_branch.to_string(),
            source_commit: source_commit.to_string(),
            target_branch: target_branch.to_string(),
            target_commit: target_commit.to_string(),
            cross_repository,
            head_owner: lines.next().unwrap_or_default().to_string(),
        })
    }

    /// One `gh pr list` call; the picker's page, not the archive. Like a
    /// bare `--pr` number, gh resolves the repository from this checkout's
    /// remotes.
    fn pull_requests(&self, which: PrList) -> Result<Vec<PrSummary>, ForgeError> {
        // gh's `--state closed` includes merged pull requests (search
        // semantics, not GraphQL's CLOSED), so finished is one call and
        // the `state` field tells merged and closed apart.
        let state = match which {
            PrList::Open => "open",
            PrList::Finished => "closed",
        };
        let stdout = self.run(|command| {
            command
                .args(["pr", "list", "--state", state])
                .arg("--limit")
                .arg(PR_PAGE.to_string())
                .args([
                    "--json",
                    "number,title,author,updatedAt,isDraft,state,\
                     headRefName,baseRefName,statusCheckRollup",
                    // One @tsv line per pull request; the rollup rides
                    // along as one column of `status;conclusion;state`
                    // triples so the line count stays one per row.
                    "--jq",
                    ".[] | [(.number|tostring), .title, .author.login // \"\", \
                     .updatedAt, (.isDraft|tostring), .state, \
                     .headRefName, .baseRefName, \
                     ([.statusCheckRollup // [] | .[] | \
                     [(.status // \"\"), (.conclusion // \"\"), (.state // \"\")] \
                     | join(\";\")] | join(\",\"))] | @tsv",
                ]);
        })?;
        let mut rows = Vec::new();
        for line in stdout.lines().filter(|line| !line.is_empty()) {
            rows.push(parse_summary(line)?);
        }
        // Newest activity first.
        rows.sort_by_key(|row| std::cmp::Reverse(row.updated));
        Ok(rows)
    }

    fn stack(&self, argument: &PrArgument) -> Result<Option<Stack>, ForgeError> {
        self.stack_of_pull(&self.repository(argument)?, argument.number)
    }

    /// REST, where GraphQL has no way in: a stack is reachable there only
    /// through a member, and `GET /repos/{owner}/{repo}/stacks/{number}`
    /// is the only lookup by the stack's own number. It answers with its
    /// members, but not with everything [`PullRequest`] needs - no
    /// head-repository owner, and a `state` plus `merged_at` instead of
    /// GitHub's three-way state - so only one member's number is taken
    /// from it and the GraphQL read does the rest.
    fn stack_numbered(&self, number: u64) -> Result<Option<Stack>, ForgeError> {
        let repo = self.local_repository()?;
        let stdout = self.run(|command| {
            command
                .args(["api", "--hostname", &repo.host])
                // `pull_requests` runs from the first member, the one
                // nearest the base branch; any of them resolves the stack.
                .arg(format!(
                    "repos/{}/{}/stacks/{number}",
                    repo.owner, repo.name
                ))
                .args(["--jq", ".pull_requests[0].number"]);
        })?;
        let member = stdout.trim();
        if member.is_empty() || member == "null" {
            return Ok(None);
        }
        let member = member
            .parse()
            .map_err(|_| ForgeError::Output(member.to_string()))?;
        self.stack_of_pull(&repo, member)
    }

    fn stacks(&self) -> Result<Vec<StackSummary>, ForgeError> {
        let repo = self.local_repository()?;
        let stdout = self.graphql_at(
            Some(&repo.host),
            STACKS,
            &[
                Variable::Text("owner", &repo.owner),
                Variable::Text("name", &repo.name),
                Variable::Number("first", PR_PAGE),
            ],
            STACK_LIST,
        )?;
        // Grouped in the order the page arrives, so the stack whose
        // members moved last leads, as the pull-request list does.
        let mut groups: Vec<Grouped> = Vec::new();
        for line in stdout.lines().filter(|line| !line.is_empty()) {
            let (number, base, position, summary) = parse_stack_row(line)?;
            match groups.iter_mut().find(|group| group.number == number) {
                Some(group) => group.members.push((position, summary)),
                None => groups.push(Grouped {
                    number,
                    base,
                    members: vec![(position, summary)],
                }),
            }
        }
        Ok(groups.into_iter().map(Grouped::into_summary).collect())
    }

    fn status(&self, pull: &PullRequest) -> Result<PullStatus, ForgeError> {
        let stdout = self.run(|command| {
            // The URL, not the number: it pins host and repository the way
            // the review was opened.
            command.args(["pr", "view"]).arg(&pull.url).args([
                "--json",
                "state,reviewDecision,statusCheckRollup",
                // State and decision on a line each, then one line per
                // check. A check run reports status and conclusion, a
                // commit status only a state; the absent ones stay empty.
                // `@tsv` escapes tabs and newlines inside a value, so the
                // column count holds.
                "--jq",
                ".state, (.reviewDecision // \"\"), (.statusCheckRollup // [] | .[] | \
                 [.status // \"\", .conclusion // \"\", .state // \"\", \
                 .detailsUrl // .targetUrl // \"\", .startedAt // \"\"] | @tsv)",
            ]);
        })?;
        let unexpected = || ForgeError::Output(stdout.trim().to_string());
        let mut lines = stdout.lines();
        let state = parse_state(lines.next().ok_or_else(unexpected)?).ok_or_else(unexpected)?;
        let review = match lines.next().ok_or_else(unexpected)? {
            "APPROVED" => Some(ReviewDecision::Approved),
            "CHANGES_REQUESTED" => Some(ReviewDecision::ChangesRequested),
            "REVIEW_REQUIRED" => Some(ReviewDecision::Required),
            "" => None,
            _ => return Err(unexpected()),
        };
        Ok(PullStatus {
            state,
            review,
            checks: fold_checks(lines),
        })
    }

    fn submit_review(
        &self,
        pull: &PullRequest,
        review: &ReviewDraft,
    ) -> Result<Vec<Posting>, ForgeError> {
        let opening = [
            Variable::Text("pull", &pull.id),
            Variable::Text("commit", review.commit.as_str()),
        ];
        let review_id = ".data.addPullRequestReview.pullRequestReview.id";
        if review.comments.is_empty() {
            return match review.verdict {
                Verdict::Approve => self
                    .graphql(pull, APPROVE, &opening, review_id)
                    .map(|_| Vec::new()),
                // GitHub refuses a comment review that says nothing, and a
                // request for changes without comments is never sent.
                Verdict::Comment | Verdict::RequestChanges => Ok(Vec::new()),
            };
        }
        let pending = self.graphql(pull, ADD_REVIEW, &opening, review_id)?;
        let pending = pending.trim();
        if pending.is_empty() || pending == "null" {
            return Err(ForgeError::Output(pending.to_string()));
        }
        let postings: Vec<Posting> = review
            .comments
            .iter()
            .map(|comment| self.post(pull, pending, comment))
            .collect();
        let submitted = match (review.verdict, first_refusal_if_all(&postings)) {
            // Every thread was refused, and a review that needs comments
            // has none left to carry: the refusal is the answer.
            (verdict, Some(refusal)) if verdict.needs_comments() => {
                Err(ForgeError::Refused(refusal.to_string()))
            }
            (verdict, _) => self
                .graphql(
                    pull,
                    SUBMIT_REVIEW,
                    &[
                        Variable::Text("review", pending),
                        Variable::Text(
                            "event",
                            match verdict {
                                Verdict::Comment => "COMMENT",
                                Verdict::Approve => "APPROVE",
                                Verdict::RequestChanges => "REQUEST_CHANGES",
                            },
                        ),
                    ],
                    ".data.submitPullRequestReview.pullRequestReview.id",
                )
                .map(drop),
        };
        if let Err(error) = submitted {
            // A pending review left behind would block the next attempt
            // (GitHub allows one per user and pull request) and swallow
            // its threads. Best effort - the first error is the one worth
            // reporting.
            let _ = self.graphql(
                pull,
                DELETE_REVIEW,
                &[Variable::Text("review", pending)],
                ".data.deletePullRequestReview.pullRequestReview.id",
            );
            return Err(error);
        }
        Ok(postings)
    }

    fn update_comment(&self, pull: &PullRequest, id: &str, body: &str) -> Result<(), ForgeError> {
        self.graphql(
            pull,
            UPDATE_COMMENT,
            &[Variable::Text("comment", id), Variable::Text("body", body)],
            ".data.updatePullRequestReviewComment.pullRequestReviewComment.id",
        )
        .map(drop)
    }

    fn react(&self, pull: &PullRequest, id: &str, kind: ReactionKind) -> Result<(), ForgeError> {
        self.graphql(
            pull,
            ADD_REACTION,
            &[
                Variable::Text("subject", id),
                Variable::Text("content", reaction_content(kind)),
            ],
            ".data.addReaction.reaction.content",
        )
        .map(drop)
    }
}

fn parse_state(state: &str) -> Option<PrState> {
    match state {
        "OPEN" => Some(PrState::Open),
        "MERGED" => Some(PrState::Merged),
        "CLOSED" => Some(PrState::Closed),
        _ => None,
    }
}

/// How many pull requests one list call brings: the picker is a page to
/// pick from, not the archive, and the stack list groups that same page.
const PR_PAGE: usize = 50;

/// A stack being gathered from the page of pull requests: its members
/// arrive scattered through it, each carrying its own position.
struct Grouped {
    number: u64,
    base: String,
    members: Vec<(u64, PrSummary)>,
}

impl Grouped {
    fn into_summary(mut self) -> StackSummary {
        self.members
            .sort_by_key(|(position, _)| std::cmp::Reverse(*position));
        StackSummary {
            number: Some(self.number),
            base: self.base,
            members: self
                .members
                .into_iter()
                .map(|(_, summary)| summary)
                .collect(),
        }
    }
}

/// The repository a GraphQL query names, and the host it runs against.
struct Repo {
    host: String,
    owner: String,
    name: String,
}

/// The repository a GitHub URL names: the first three path segments, so a
/// pull request's URL and a repository's own both answer.
fn repo_of(url: &str) -> Option<Repo> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let mut segments = rest.split('/');
    let mut segment = || segments.next().filter(|part| !part.is_empty());
    Some(Repo {
        host: segment()?.to_string(),
        owner: segment()?.to_string(),
        name: segment()?.to_string(),
    })
}

/// One stack entry's `@tsv` line: the member's position and the pull
/// request itself.
fn parse_member(line: &str) -> Result<(u64, PullRequest), ForgeError> {
    let unexpected = || ForgeError::Output(line.to_string());
    let mut columns = line.split('\t');
    let mut column = || columns.next().ok_or_else(unexpected);
    let position = column()?.parse().map_err(|_| unexpected())?;
    let number = column()?.parse().map_err(|_| unexpected())?;
    let (id, url) = (column()?.to_string(), column()?.to_string());
    let state = parse_state(column()?).ok_or_else(unexpected)?;
    let (source_branch, source_commit) = (untsv(column()?), column()?.to_string());
    let (target_branch, target_commit) = (untsv(column()?), column()?.to_string());
    let cross_repository = match column()? {
        "true" => true,
        "false" => false,
        _ => return Err(unexpected()),
    };
    if id.is_empty() || url.is_empty() || source_commit.is_empty() || target_commit.is_empty() {
        return Err(unexpected());
    }
    Ok((
        position,
        PullRequest {
            number,
            id,
            url,
            state,
            source_branch,
            source_commit,
            target_branch,
            target_commit,
            cross_repository,
            head_owner: untsv(column().unwrap_or_default()),
        },
    ))
}

/// One pull request of the stack list: its stack's number and base
/// branch, its own position, then the row [`parse_summary`] reads.
fn parse_stack_row(line: &str) -> Result<(u64, String, u64, PrSummary), ForgeError> {
    let unexpected = || ForgeError::Output(line.to_string());
    let mut columns = line.splitn(4, '\t');
    let mut column = || columns.next().ok_or_else(unexpected);
    let number = column()?.parse().map_err(|_| unexpected())?;
    let base = untsv(column()?);
    let position = column()?.parse().map_err(|_| unexpected())?;
    Ok((number, base, position, parse_summary(column()?)?))
}

/// One `@tsv` line of a `pr list` answer into a row.
fn parse_summary(line: &str) -> Result<PrSummary, ForgeError> {
    let unexpected = || ForgeError::Output(line.to_string());
    let mut columns = line.split('\t');
    let mut column = || columns.next().ok_or_else(unexpected);
    let number = column()?.parse().map_err(|_| unexpected())?;
    let (title, author) = (untsv(column()?), untsv(column()?));
    let updated = parse_rfc3339(column()?).ok_or_else(unexpected)?;
    let draft = match column()? {
        "true" => true,
        "false" => false,
        _ => return Err(unexpected()),
    };
    let state = parse_state(column()?).ok_or_else(unexpected)?;
    let (head, base) = (untsv(column()?), untsv(column()?));
    let checks = column()?
        .split(',')
        .filter(|check| !check.is_empty())
        .map(|check| {
            let mut words = check.split(';');
            let mut word = || words.next().unwrap_or_default();
            check_verdict((word(), word(), word()))
        })
        .fold(None, |folded, check| Some(fold_verdict(folded, check)));
    Ok(PrSummary {
        number,
        title,
        author,
        updated,
        state,
        draft,
        checks,
        head,
        base,
    })
}

/// Undoes `@tsv`'s escaping, so a title shows as typed.
fn untsv(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut escaped = false;
    for character in text.chars() {
        if escaped {
            out.push(match character {
                't' => '\t',
                'n' => '\n',
                'r' => '\r',
                other => other,
            });
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else {
            out.push(character);
        }
    }
    out
}

/// One check's verdict from gh's three state words: a check run reports
/// status and conclusion, a commit status only a state.
fn check_verdict(status_conclusion_state: (&str, &str, &str)) -> CheckState {
    match status_conclusion_state {
        (_, "SUCCESS" | "NEUTRAL" | "SKIPPED", _) | (_, _, "SUCCESS") => CheckState::Passing,
        (
            _,
            "FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE" | "STALE",
            _,
        )
        | (_, _, "FAILURE" | "ERROR") => CheckState::Failing,
        _ => CheckState::Running,
    }
}

/// The louder verdict wins: a failure outranks the checks still running,
/// and those outrank the green ones.
fn fold_verdict(folded: Option<CheckState>, check: CheckState) -> CheckState {
    match (folded, check) {
        (Some(CheckState::Failing), _) | (_, CheckState::Failing) => CheckState::Failing,
        (Some(CheckState::Running), _) | (_, CheckState::Running) => CheckState::Running,
        _ => CheckState::Passing,
    }
}

/// Folds the rollup's lines (status, conclusion, state, URL, start - tab
/// separated) into one verdict. A failure outranks a check still running:
/// the reviewer should hear about red before the rest finishes. Unknown
/// words count as running, never as passing - a new GitHub conclusion must
/// not paint the pull request green.
fn fold_checks<'a>(lines: impl Iterator<Item = &'a str>) -> Option<Checks> {
    let mut state = None;
    let mut latest: Option<(&str, &str)> = None;
    for line in lines.filter(|line| !line.is_empty()) {
        let mut columns = line.split('\t');
        let mut column = || columns.next().unwrap_or_default();
        let check = check_verdict((column(), column(), column()));
        let (url, started) = (column(), column());
        state = Some(fold_verdict(state, check));
        // RFC 3339 in UTC sorts as text. A check without a start time
        // (a commit status may have none) only wins while nothing else
        // has a page to offer.
        if !url.is_empty() && latest.is_none_or(|(_, newest)| started > newest) {
            latest = Some((url, started));
        }
    }
    Some(Checks {
        state: state?,
        latest_run: latest.map(|(url, _)| run_page(url).to_string()),
    })
}

/// An Actions check links to one job of its run; the run's own page shows
/// the whole pipeline. Other CI providers' links stay as they are.
fn run_page(url: &str) -> &str {
    match url.split_once("/job/") {
        Some((run, _)) if run.contains("/actions/runs/") => run,
        _ => url,
    }
}

/// The first refusal's words when no comment was posted at all.
fn first_refusal_if_all(postings: &[Posting]) -> Option<&str> {
    let mut first = None;
    for posting in postings {
        match posting {
            Posting::Posted(_) => return None,
            Posting::Refused(reason) => {
                first.get_or_insert(reason.as_str());
            }
        }
    }
    first
}

/// GitHub's reactions are a closed set of eight without a checkmark, a
/// stop sign or a pin, so each kind takes the nearest in meaning.
fn reaction_content(kind: ReactionKind) -> &'static str {
    match kind {
        ReactionKind::Seen => "EYES",
        ReactionKind::Done => "THUMBS_UP",
        ReactionKind::WontFix => "THUMBS_DOWN",
        ReactionKind::FollowUp => "ROCKET",
    }
}

fn diff_side(side: Side) -> &'static str {
    match side {
        Side::Old => "LEFT",
        Side::New => "RIGHT",
    }
}

/// The host of a pull request URL, for `--hostname`.
fn host_of(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    rest.split('/').next().filter(|host| !host.is_empty())
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;
    use crate::model::Oid;

    /// A stand-in gh: logs every call's arguments, one per line behind a
    /// `--` line, and answers by what the query mentions.
    struct FakeGh {
        directory: tempfile::TempDir,
        _serial: std::sync::MutexGuard<'static, ()>,
    }

    /// One fake at a time: a process forked by a parallel test inherits the
    /// script's still-open write handle, and running the script then fails
    /// as "text file busy".
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    impl FakeGh {
        /// `rules` are `case` arms tried before the stock answers.
        fn new(rules: &str) -> Self {
            let serial = SERIAL
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let directory = tempfile::tempdir().unwrap();
            let log = directory.path().join("log");
            let script = format!(
                r#"#!/bin/sh
{{ echo "--"; printf '%s\n' "$@"; }} >> '{log}'
case "$*" in
{rules}
  *"repo view"*) echo 'https://github.com/o/r' ;;
  *"pr view"*) printf 'PR_1\nhttps://github.com/o/r/pull/17\nOPEN\ntopic\nhhhh\nmain\nbbbb\nfalse\no\n' ;;
  *subjectType:FILE*) printf 'C_file\nhttps://github.com/o/r/pull/17#discussion_r2\n' ;;
  *addPullRequestReviewThread*) printf 'C_line\nhttps://github.com/o/r/pull/17#discussion_r1\n' ;;
  *addPullRequestReview*) echo REVIEW_1 ;;
  *) echo ok ;;
esac
"#,
                log = log.display()
            );
            let path = directory.path().join("gh");
            std::fs::write(&path, script).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            Self {
                directory,
                _serial: serial,
            }
        }

        fn forge(&self) -> GitHubCli {
            GitHubCli::with_program(self.directory.path(), self.directory.path().join("gh"))
        }

        /// Every call so far, its arguments joined by spaces.
        fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.directory.path().join("log"))
                .unwrap_or_default()
                .split("--\n")
                .filter(|call| !call.is_empty())
                .map(|call| call.trim_end().replace('\n', " "))
                .collect()
        }
    }

    fn pull(url: &str) -> PullRequest {
        PullRequest {
            number: 17,
            id: "PR_1".into(),
            url: url.into(),
            state: PrState::Open,
            source_branch: "topic".into(),
            source_commit: "hhhh".into(),
            target_branch: "main".into(),
            target_commit: "bbbb".into(),
            cross_repository: false,
            head_owner: "o".into(),
        }
    }

    fn github() -> PullRequest {
        pull("https://github.com/o/r/pull/17")
    }

    fn posted(posting: &Posting) -> &PostedComment {
        match posting {
            Posting::Posted(posted) => posted,
            Posting::Refused(reason) => panic!("refused: {reason}"),
        }
    }

    fn draft(verdict: Verdict, comments: Vec<DraftComment>) -> ReviewDraft {
        ReviewDraft {
            commit: Oid::new("hhhh"),
            verdict,
            comments,
        }
    }

    fn on_line(line: usize, lines: usize) -> DraftComment {
        DraftComment {
            path: "src/a.rs".into(),
            anchor: Some(LineAnchor {
                side: Side::New,
                line,
                lines,
            }),
            body: "@body".into(),
            file_body: "quoted @body".into(),
        }
    }

    fn numbered(number: u64) -> PrArgument {
        PrArgument { number, url: None }
    }

    #[test]
    fn describes_the_pull_request() {
        let gh = FakeGh::new("");
        let pull = gh.forge().pull_request(&numbered(17)).unwrap();
        assert_eq!(pull.id, "PR_1");
        assert_eq!(pull.url, "https://github.com/o/r/pull/17");
        assert_eq!(pull.state, PrState::Open);
        assert_eq!(
            (pull.source_branch.as_str(), pull.source_commit.as_str()),
            ("topic", "hhhh")
        );
        assert_eq!(
            (pull.target_branch.as_str(), pull.target_commit.as_str()),
            ("main", "bbbb")
        );
        assert!(!pull.cross_repository);
        assert_eq!(pull.head_owner, "o");
        assert!(gh.calls()[0].starts_with("pr view 17 --json id,url,state"));
    }

    fn rollup(lines: &[&str]) -> Option<Checks> {
        fold_checks(lines.iter().copied())
    }

    #[test]
    fn reads_state_decision_and_checks() {
        let gh = FakeGh::new(concat!(
            r#"  *statusCheckRollup*) printf 'OPEN\nAPPROVED\n"#,
            r#"COMPLETED\tSUCCESS\t\thttps://github.com/o/r/actions/runs/7/job/1\t2026-11-16T10:00:00Z\n"#,
            r#"IN_PROGRESS\t\t\thttps://github.com/o/r/actions/runs/9/job/4\t2026-11-16T11:00:00Z\n' ;;"#
        ));
        let status = gh.forge().status(&github()).unwrap();
        assert_eq!(
            status,
            PullStatus {
                state: PrState::Open,
                review: Some(ReviewDecision::Approved),
                checks: Some(Checks {
                    state: CheckState::Running,
                    latest_run: Some("https://github.com/o/r/actions/runs/9".into()),
                }),
            }
        );
        assert!(
            gh.calls()[0].starts_with("pr view https://github.com/o/r/pull/17 --json state,"),
            "{}",
            gh.calls()[0]
        );
    }

    #[test]
    fn a_merged_pull_request_without_review_or_checks() {
        let gh = FakeGh::new(r#"  *statusCheckRollup*) printf 'MERGED\n\n' ;;"#);
        assert_eq!(
            gh.forge().status(&github()).unwrap(),
            PullStatus {
                state: PrState::Merged,
                review: None,
                checks: None,
            }
        );
    }

    #[test]
    fn an_unknown_decision_is_unexpected_output() {
        let gh = FakeGh::new(r#"  *statusCheckRollup*) printf 'OPEN\nMAYBE\n' ;;"#);
        let error = gh.forge().status(&github()).unwrap_err();
        assert!(matches!(error, ForgeError::Output(_)), "{error}");
    }

    #[test]
    fn lists_open_pull_requests_newest_first() {
        let gh = FakeGh::new(concat!(
            r#"  *"pr list --state open"*) printf '"#,
            r#"17\tTab\\there\talice\t2026-11-16T10:00:00Z\tfalse\tOPEN\tfeat/a\tmain"#,
            r#"\tCOMPLETED;SUCCESS;,IN_PROGRESS;;\n"#,
            r#"21\tA draft\tbob\t2026-11-16T11:00:00Z\ttrue\tOPEN\tfeat/b\tfeat/a\t\n' ;;"#
        ));
        let rows = gh.forge().pull_requests(PrList::Open).unwrap();
        assert_eq!(
            rows,
            vec![
                PrSummary {
                    number: 21,
                    title: "A draft".into(),
                    author: "bob".into(),
                    updated: parse_rfc3339("2026-11-16T11:00:00Z").unwrap(),
                    state: PrState::Open,
                    draft: true,
                    checks: None,
                    head: "feat/b".into(),
                    base: "feat/a".into(),
                },
                PrSummary {
                    number: 17,
                    title: "Tab\there".into(),
                    author: "alice".into(),
                    updated: parse_rfc3339("2026-11-16T10:00:00Z").unwrap(),
                    state: PrState::Open,
                    draft: false,
                    checks: Some(CheckState::Running),
                    head: "feat/a".into(),
                    base: "main".into(),
                },
            ]
        );
        assert!(
            gh.calls()[0].starts_with(
                "pr list --state open --limit 50 --json \
                 number,title,author,updatedAt,isDraft,state,\
                 headRefName,baseRefName,statusCheckRollup"
            ),
            "{}",
            gh.calls()[0]
        );
    }

    #[test]
    fn a_finished_list_carries_merged_and_closed_in_one_call() {
        // gh's `--state closed` includes merged pull requests; the rows
        // tell them apart by the state field.
        let gh = FakeGh::new(concat!(
            r#"  *"pr list --state closed"*) printf '"#,
            r#"3\tLanded\tcarol\t2026-11-10T09:00:00Z\tfalse\tMERGED\tfeat/c\tmain"#,
            r#"\tCOMPLETED;FAILURE;\n"#,
            r#"5\tAbandoned\tdave\t2026-11-12T09:00:00Z\tfalse\tCLOSED\tfeat/d\tmain\t\n' ;;"#
        ));
        let rows = gh.forge().pull_requests(PrList::Finished).unwrap();
        assert_eq!(
            rows.iter()
                .map(|row| (row.number, row.state, row.checks))
                .collect::<Vec<_>>(),
            vec![
                (5, PrState::Closed, None),
                (3, PrState::Merged, Some(CheckState::Failing)),
            ]
        );
    }

    #[test]
    fn an_empty_list_is_no_error() {
        let gh = FakeGh::new(r#"  *"pr list"*) printf '' ;;"#);
        assert_eq!(gh.forge().pull_requests(PrList::Open).unwrap(), Vec::new());
    }

    #[test]
    fn a_malformed_list_line_is_unexpected_output() {
        let gh = FakeGh::new("");
        let error = gh.forge().pull_requests(PrList::Open).unwrap_err();
        assert!(matches!(error, ForgeError::Output(_)), "{error}");
    }

    /// One stack entry as the query's jq prints it, for the fake's
    /// `printf`: the escapes are the shell's to expand.
    fn entry(position: u64, number: u64, head: &str, base: &str) -> String {
        format!(
            "{position}\\t{number}\\tPR_{number}\\thttps://github.com/o/r/pull/{number}\\tOPEN\\t\
             {head}\\th{number}\\t{base}\\tb{number}\\tfalse\\to"
        )
    }

    fn numbers(pulls: &[PullRequest]) -> Vec<u64> {
        pulls.iter().map(|pull| pull.number).collect()
    }

    #[test]
    fn a_stack_is_read_from_any_member_latest_first() {
        let rows = format!(
            "false\\t\\t42\\n{}\\n{}\\n",
            entry(1, 1, "feat/a", "main"),
            entry(2, 2, "feat/b", "feat/a"),
        );
        let gh = FakeGh::new(&format!("  *entries*) printf '{rows}' ;;"));
        let stack = gh.forge().stack(&numbered(2)).unwrap().unwrap();
        assert_eq!(stack.number, Some(42));
        // Position 1 is the member nearest the base branch, and our model
        // leads with the latest one.
        assert_eq!(numbers(&stack.pulls), [2, 1]);
        assert_eq!(stack.pulls[0].source_branch, "feat/b");
        assert_eq!(stack.pulls[0].source_commit, "h2");
        assert_eq!(stack.pulls[1].target_branch, "main");
        assert_eq!(stack.pulls[1].target_commit, "b1");
        // A bare number names no repository, so gh is asked which one
        // this checkout speaks for before the query can name it.
        let calls = gh.calls();
        assert!(calls[0].starts_with("repo view"), "{calls:?}");
        assert!(calls[1].contains("--hostname github.com"), "{calls:?}");
        assert!(calls[1].contains("owner=o"), "{calls:?}");
    }

    #[test]
    fn a_stack_named_by_url_keeps_its_host_and_repository() {
        let rows = format!("false\\t\\t42\\n{}\\n", entry(1, 7, "feat/a", "main"));
        let gh = FakeGh::new(&format!("  *entries*) printf '{rows}' ;;"));
        let stack = gh
            .forge()
            .stack(&PrArgument {
                number: 7,
                url: Some("https://github.example.corp/team/service/pull/7".into()),
            })
            .unwrap()
            .unwrap();
        assert_eq!(numbers(&stack.pulls), [7]);
        // The URL answers for host, owner and repository, so nothing is
        // asked of the checkout.
        let calls = gh.calls();
        assert!(
            calls[0].contains("--hostname github.example.corp"),
            "{calls:?}"
        );
        assert!(calls[0].contains("owner=team"), "{calls:?}");
        assert!(calls[0].contains("name=service"), "{calls:?}");
    }

    #[test]
    fn a_stack_number_is_looked_up_through_rest() {
        let rows = format!("false\\t\\t42\\n{}\\n", entry(1, 1, "feat/a", "main"));
        let gh = FakeGh::new(&format!(
            "  *stacks/42*) echo 1 ;;\n  *entries*) printf '{rows}' ;;"
        ));
        let stack = gh.forge().stack_numbered(42).unwrap().unwrap();
        assert_eq!(numbers(&stack.pulls), [1]);
        // REST names a member, then the member read resolves the stack -
        // never a scan of the capped pull-request list.
        let calls = gh.calls();
        assert_eq!(calls.len(), 3, "{calls:?}");
        assert!(calls[0].starts_with("repo view"), "{calls:?}");
        assert_eq!(
            calls[1],
            "api --hostname github.com repos/o/r/stacks/42 --jq .pull_requests[0].number"
        );
        assert!(calls[2].contains("-f query=query("), "{calls:?}");
    }

    #[test]
    fn a_stack_without_members_is_no_stack() {
        let gh = FakeGh::new("  *stacks/42*) echo null ;;");
        assert!(gh.forge().stack_numbered(42).unwrap().is_none());
    }

    #[test]
    fn a_pull_request_in_no_stack_is_no_error() {
        // A null `stack` field leaves the jq filter with nothing to print.
        let gh = FakeGh::new("  *entries*) : ;;");
        assert!(gh.forge().stack(&numbered(2)).unwrap().is_none());
    }

    #[test]
    fn a_stack_longer_than_one_page_is_read_to_its_end() {
        let first = format!("true\\tCURSOR\\t42\\n{}\\n", entry(1, 1, "feat/a", "main"));
        let second = format!("false\\t\\t42\\n{}\\n", entry(2, 2, "feat/b", "feat/a"));
        let gh = FakeGh::new(&format!(
            "  *after=CURSOR*) printf '{second}' ;;\n  *entries*) printf '{first}' ;;"
        ));
        let stack = gh.forge().stack(&numbered(1)).unwrap().unwrap();
        assert_eq!(numbers(&stack.pulls), [2, 1]);
        // The repository lookup, then one call per page.
        let calls = gh.calls();
        assert_eq!(calls.len(), 3, "{calls:?}");
        assert!(calls[2].contains("after=CURSOR"), "{calls:?}");
    }

    #[test]
    fn a_member_the_viewer_cannot_read_is_unexpected_output() {
        let gh = FakeGh::new("  *entries*) printf 'false\\t\\t42\\nnull\\tnull\\n' ;;");
        let error = gh.forge().stack(&numbered(2)).unwrap_err();
        assert!(matches!(error, ForgeError::Output(_)), "{error}");
    }

    #[test]
    fn stacks_group_one_page_of_pull_requests() {
        let gh = FakeGh::new(concat!(
            r#"  *pullRequests*) printf '"#,
            r#"42\tmain\t2\t2\tlatest\tada\t2026-11-22T10:00:00Z\tfalse\tOPEN\tfeat/b\tfeat/a\t\n"#,
            r#"42\tmain\t1\t1\tfirst\tada\t2026-11-21T10:00:00Z\tfalse\tOPEN\tfeat/a\tmain\t;;SUCCESS\n"#,
            r#"7\ttrunk\t1\t5\tsolo\tgrace\t2026-11-20T10:00:00Z\ttrue\tOPEN\tfix\ttrunk\t;;FAILURE\n' ;;"#
        ));
        let stacks = gh.forge().stacks().unwrap();
        assert_eq!(stacks.len(), 2);
        assert_eq!(stacks[0].number, Some(42));
        assert_eq!(stacks[0].base, "main");
        assert_eq!(
            stacks[0]
                .members
                .iter()
                .map(|member| member.number)
                .collect::<Vec<_>>(),
            [2, 1]
        );
        assert_eq!(stacks[0].members[0].title, "latest");
        // GraphQL folds the checks, so a member answers one verdict or none.
        assert_eq!(stacks[0].members[0].checks, None);
        assert_eq!(stacks[0].members[1].checks, Some(CheckState::Passing));
        assert_eq!(stacks[1].number, Some(7));
        assert_eq!(stacks[1].members[0].checks, Some(CheckState::Failing));
        // The picker's page, grouped; never a walk past it.
        assert!(gh.calls()[1].contains("first=50"), "{:?}", gh.calls());
    }

    #[test]
    fn one_failure_outranks_running_and_passing_checks() {
        let checks = rollup(&[
            "COMPLETED\tSUCCESS\t\t\t",
            "QUEUED\t\t\t\t",
            "COMPLETED\tFAILURE\t\t\t",
        ])
        .unwrap();
        assert_eq!(checks.state, CheckState::Failing);
        assert_eq!(checks.latest_run, None);
    }

    #[test]
    fn commit_statuses_count_beside_check_runs() {
        assert_eq!(
            rollup(&["\t\tSUCCESS\thttps://ci.example/build/5\t"]),
            Some(Checks {
                state: CheckState::Passing,
                latest_run: Some("https://ci.example/build/5".into()),
            })
        );
        assert_eq!(
            rollup(&["COMPLETED\tSUCCESS\t\t\t", "\t\tPENDING\t\t"])
                .unwrap()
                .state,
            CheckState::Running
        );
        assert_eq!(
            rollup(&["\t\tERROR\t\t"]).unwrap().state,
            CheckState::Failing
        );
    }

    #[test]
    fn an_unknown_conclusion_never_reads_as_passing() {
        assert_eq!(
            rollup(&["COMPLETED\tSOMETHING_NEW\t\t\t"]).unwrap().state,
            CheckState::Running
        );
    }

    #[test]
    fn the_latest_started_check_names_the_run_to_open() {
        let checks = rollup(&[
            "COMPLETED\tSUCCESS\t\thttps://github.com/o/r/actions/runs/9/job/4\t2026-11-16T11:00:00Z",
            "COMPLETED\tSUCCESS\t\thttps://github.com/o/r/actions/runs/7/job/1\t2026-11-16T10:00:00Z",
            "\t\tSUCCESS\thttps://ci.example/build/5\t",
        ])
        .unwrap();
        assert_eq!(
            checks.latest_run.as_deref(),
            Some("https://github.com/o/r/actions/runs/9")
        );
    }

    #[test]
    fn no_checks_is_no_verdict() {
        assert_eq!(rollup(&[]), None);
    }

    #[test]
    fn a_url_reaches_gh_verbatim() {
        let gh = FakeGh::new("");
        let url = "https://github.example.corp/o/r/pull/17";
        gh.forge()
            .pull_request(&PrArgument {
                number: 17,
                url: Some(url.into()),
            })
            .unwrap();
        assert!(gh.calls()[0].starts_with(&format!("pr view {url} ")));
    }

    #[test]
    fn a_short_answer_is_unexpected_output() {
        let gh = FakeGh::new(r#"  *"pr view"*) printf 'PR_1\nhttps://x\nOPEN\n' ;;"#);
        let error = gh
            .forge()
            .pull_request(&PrArgument {
                number: 17,
                url: None,
            })
            .unwrap_err();
        assert!(matches!(error, ForgeError::Output(_)), "{error}");
    }

    #[test]
    fn gh_failing_reports_its_own_words() {
        let gh = FakeGh::new(r#"  *"pr view"*) echo "no pull requests found" >&2; exit 1 ;;"#);
        let error = gh
            .forge()
            .pull_request(&PrArgument {
                number: 17,
                url: None,
            })
            .unwrap_err();
        assert_eq!(error.to_string(), "no pull requests found");
    }

    #[test]
    fn a_missing_gh_is_named() {
        let directory = tempfile::tempdir().unwrap();
        let forge = GitHubCli::with_program(directory.path(), directory.path().join("absent"));
        let error = forge
            .pull_request(&PrArgument {
                number: 17,
                url: None,
            })
            .unwrap_err();
        assert!(matches!(error, ForgeError::ToolMissing { .. }), "{error}");
    }

    #[test]
    fn a_review_opens_pending_takes_its_threads_and_submits_once() {
        let gh = FakeGh::new("");
        let postings = gh
            .forge()
            .submit_review(
                &github(),
                &draft(Verdict::Comment, vec![on_line(12, 1), on_line(20, 3)]),
            )
            .unwrap();
        assert_eq!(postings.len(), 2);
        assert_eq!(
            posted(&postings[0]),
            &PostedComment {
                id: "C_line".into(),
                url: "https://github.com/o/r/pull/17#discussion_r1".into(),
                on_file: false,
            }
        );

        let calls = gh.calls();
        assert_eq!(calls.len(), 4, "{calls:#?}");
        assert!(calls[0].contains("commitOID:$commit}"), "{}", calls[0]);
        assert!(
            calls[0].contains("-f pull=PR_1 -f commit=hhhh"),
            "{}",
            calls[0]
        );
        // The body goes raw (`-f`): `-F` would read a leading `@` as a file.
        assert!(
            calls[1].contains(
                "-f review=REVIEW_1 -f path=src/a.rs -f body=@body -F line=12 -f side=RIGHT --jq"
            ),
            "{}",
            calls[1]
        );
        assert!(
            calls[2].contains("-F line=20 -f side=RIGHT -F startLine=18 -f startSide=RIGHT"),
            "{}",
            calls[2]
        );
        assert!(calls[3].contains("submitPullRequestReview"), "{}", calls[3]);
        assert!(
            calls[3].contains("-f review=REVIEW_1 -f event=COMMENT"),
            "{}",
            calls[3]
        );
    }

    #[test]
    fn a_refused_line_falls_back_to_the_file() {
        let gh = FakeGh::new(r#"  *line=12*) echo "line must be part of the diff" >&2; exit 1 ;;"#);
        let postings = gh
            .forge()
            .submit_review(&github(), &draft(Verdict::Comment, vec![on_line(12, 1)]))
            .unwrap();
        assert_eq!(posted(&postings[0]).id, "C_file");
        assert!(posted(&postings[0]).on_file);
        let calls = gh.calls();
        assert!(calls[2].contains("subjectType:FILE"), "{}", calls[2]);
        assert!(calls[2].contains("-f body=quoted @body"), "{}", calls[2]);
        assert!(calls[3].contains("submitPullRequestReview"), "{}", calls[3]);
    }

    #[test]
    fn an_unanchored_comment_goes_straight_to_the_file() {
        let gh = FakeGh::new("");
        let comment = DraftComment {
            anchor: None,
            ..on_line(1, 1)
        };
        let postings = gh
            .forge()
            .submit_review(&github(), &draft(Verdict::Comment, vec![comment]))
            .unwrap();
        assert_eq!(posted(&postings[0]).id, "C_file");
        assert_eq!(gh.calls().len(), 3);
    }

    #[test]
    fn a_review_whose_every_thread_is_refused_is_discarded() {
        let gh = FakeGh::new(r#"  *addPullRequestReviewThread*) echo "boom" >&2; exit 1 ;;"#);
        let error = gh
            .forge()
            .submit_review(
                &github(),
                &draft(Verdict::Comment, vec![on_line(12, 1), on_line(13, 1)]),
            )
            .unwrap_err();
        assert_eq!(error.to_string(), "boom");
        let calls = gh.calls();
        assert!(
            calls
                .iter()
                .all(|call| !call.contains("submitPullRequestReview")),
            "{calls:#?}"
        );
        let last = calls.last().unwrap();
        assert!(last.contains("deletePullRequestReview"), "{last}");
        assert!(last.contains("-f review=REVIEW_1"), "{last}");
    }

    #[test]
    fn a_refused_path_stays_out_and_the_rest_is_submitted() {
        let gh = FakeGh::new(r#"  *path=gone.rs*) echo "path not in diff" >&2; exit 1 ;;"#);
        let gone = DraftComment {
            path: "gone.rs".into(),
            ..on_line(3, 1)
        };
        let postings = gh
            .forge()
            .submit_review(
                &github(),
                &draft(Verdict::Comment, vec![gone, on_line(12, 1)]),
            )
            .unwrap();
        assert_eq!(postings[0], Posting::Refused("path not in diff".into()));
        assert_eq!(posted(&postings[1]).id, "C_line");
        assert!(
            gh.calls()
                .last()
                .unwrap()
                .contains("submitPullRequestReview")
        );
    }

    #[test]
    fn an_approval_stands_even_when_its_only_comment_is_refused() {
        let gh = FakeGh::new(r#"  *addPullRequestReviewThread*) echo "boom" >&2; exit 1 ;;"#);
        let postings = gh
            .forge()
            .submit_review(&github(), &draft(Verdict::Approve, vec![on_line(12, 1)]))
            .unwrap();
        assert_eq!(postings, vec![Posting::Refused("boom".into())]);
        assert!(gh.calls().last().unwrap().contains("-f event=APPROVE"));
    }

    #[test]
    fn a_null_thread_is_a_refusal_not_a_link() {
        let gh = FakeGh::new(r#"  *addPullRequestReviewThread*) printf 'null\nnull\n' ;;"#);
        let error = gh
            .forge()
            .submit_review(&github(), &draft(Verdict::Comment, vec![on_line(12, 1)]))
            .unwrap_err();
        assert!(error.to_string().contains("null"), "{error}");
    }

    #[test]
    fn a_failed_submit_discards_the_pending_review() {
        let gh = FakeGh::new(
            r#"  *submitPullRequestReview*) echo "cannot approve your own" >&2; exit 1 ;;"#,
        );
        let error = gh
            .forge()
            .submit_review(&github(), &draft(Verdict::Approve, vec![on_line(12, 1)]))
            .unwrap_err();
        assert_eq!(error.to_string(), "cannot approve your own");
        assert!(
            gh.calls()
                .last()
                .unwrap()
                .contains("deletePullRequestReview")
        );
    }

    #[test]
    fn approving_with_comments_submits_the_approval() {
        let gh = FakeGh::new("");
        gh.forge()
            .submit_review(&github(), &draft(Verdict::Approve, vec![on_line(12, 1)]))
            .unwrap();
        assert!(gh.calls()[2].contains("-f event=APPROVE"));
    }

    #[test]
    fn requesting_changes_submits_that_event() {
        let gh = FakeGh::new("");
        gh.forge()
            .submit_review(
                &github(),
                &draft(Verdict::RequestChanges, vec![on_line(12, 1)]),
            )
            .unwrap();
        assert!(gh.calls()[2].contains("-f event=REQUEST_CHANGES"));
    }

    #[test]
    fn a_request_for_changes_whose_comments_are_all_refused_is_discarded() {
        let gh = FakeGh::new(r#"  *addPullRequestReviewThread*) echo "boom" >&2; exit 1 ;;"#);
        let error = gh
            .forge()
            .submit_review(
                &github(),
                &draft(Verdict::RequestChanges, vec![on_line(12, 1)]),
            )
            .unwrap_err();
        assert_eq!(error.to_string(), "boom");
        let calls = gh.calls();
        assert!(
            calls
                .iter()
                .all(|call| !call.contains("submitPullRequestReview")),
            "{calls:#?}"
        );
        assert!(calls.last().unwrap().contains("deletePullRequestReview"));
    }

    #[test]
    fn approving_without_comments_is_one_call() {
        let gh = FakeGh::new("");
        let postings = gh
            .forge()
            .submit_review(&github(), &draft(Verdict::Approve, Vec::new()))
            .unwrap();
        assert!(postings.is_empty());
        let calls = gh.calls();
        assert_eq!(calls.len(), 1, "{calls:#?}");
        assert!(calls[0].contains("event:APPROVE"), "{}", calls[0]);
    }

    #[test]
    fn an_empty_comment_review_calls_nothing() {
        let gh = FakeGh::new("");
        gh.forge()
            .submit_review(&github(), &draft(Verdict::Comment, Vec::new()))
            .unwrap();
        assert!(gh.calls().is_empty());
    }

    #[test]
    fn every_call_names_the_pull_requests_host() {
        let gh = FakeGh::new("");
        let forge = gh.forge();
        let enterprise = pull("https://github.example.corp/o/r/pull/17");
        forge
            .submit_review(&enterprise, &draft(Verdict::Comment, vec![on_line(1, 1)]))
            .unwrap();
        forge.update_comment(&enterprise, "C_1", "new").unwrap();
        forge.react(&enterprise, "C_1", ReactionKind::Seen).unwrap();
        for call in gh.calls() {
            assert!(
                call.starts_with("api graphql --hostname github.example.corp -f query="),
                "{call}"
            );
        }
    }

    #[test]
    fn updates_a_comment() {
        let gh = FakeGh::new("");
        gh.forge()
            .update_comment(&github(), "C_1", "new body")
            .unwrap();
        let call = &gh.calls()[0];
        assert!(call.contains("updatePullRequestReviewComment"), "{call}");
        assert!(call.contains("-f comment=C_1 -f body=new body"), "{call}");
    }

    #[test]
    fn reactions_map_to_githubs_nearest_emoji() {
        let gh = FakeGh::new("");
        let forge = gh.forge();
        for kind in ReactionKind::ALL {
            forge.react(&github(), "C_1", kind).unwrap();
        }
        let sent: Vec<String> = gh
            .calls()
            .iter()
            .map(|call| {
                let content = call.split("-f content=").nth(1).unwrap();
                content.split(' ').next().unwrap().to_string()
            })
            .collect();
        assert_eq!(sent, ["EYES", "THUMBS_UP", "THUMBS_DOWN", "ROCKET"]);
        assert!(gh.calls()[0].contains("-f subject=C_1"));
    }
}
