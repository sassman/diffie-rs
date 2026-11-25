//! `--pr`: the pull request a review is connected to, as a
//! [`Forge`] describes it, the parsing of the command-line value that
//! names it, and [`open`], which turns the named pull request into the
//! comparison a review runs on - for the command line and for the in-app
//! picker alike.
//!
//! `--stack` reviews a whole stack of pull requests as one comparison.
//! [`Stack`] is the resolved stack, its [`StackMember`]s carrying both
//! halves, [`StackSummary`] the row a list shows,
//! [`parse_stack`] reads the stack's own number off the command line,
//! [`open_stack`] turns it into that one comparison, and [`ownership`]
//! says which member each commit belongs to.

use std::collections::{HashMap, HashSet};

use crate::backend::ReviewBackend;
use crate::forge::Forge;
use crate::model::{Commit, CompareMode, Comparison, Oid};

/// The `--pr` value, parsed but not yet resolved.
#[derive(Debug, PartialEq, Eq)]
pub struct PrArgument {
    pub number: u64,
    /// The URL as typed, when one was given. gh gets it verbatim, which pins
    /// host and repository (GitHub Enterprise included); a bare number leaves
    /// gh to find the repository from the working directory's remotes.
    pub url: Option<String>,
}

/// What the forge reports about the pull request.
#[derive(Clone, Debug)]
pub struct PullRequest {
    pub number: u64,
    /// The forge's own id for the pull request; opaque outside the forge.
    pub id: String,
    /// The pull request's page, for the browser.
    pub url: String,
    pub state: PrState,
    /// The branch the pull request merges (`headRefName`). Display only;
    /// the review resolves `source_commit`.
    pub source_branch: String,
    /// The exact tip of the pull request (`headRefOid`). Pinning it, rather
    /// than looking the branch up, keeps a stale same-named local or
    /// remote-tracking branch from silently showing other content.
    pub source_commit: String,
    /// The branch it merges into (`baseRefName`). Display only.
    pub target_branch: String,
    /// The base commit the PR is compared against (`baseRefOid`). For a
    /// merged PR this reproduces the PR page's own diff even when the base
    /// branch was rebuilt underneath; the merge commit's first parent was
    /// rejected for that reason - it reaches past the PR after such a
    /// rebuild.
    pub target_commit: String,
    /// The head branch lives in a fork, not in the base repository.
    pub cross_repository: bool,
    /// Owner login of the repository holding the head branch. Names the fork
    /// when its remote turns out not to be configured.
    pub head_owner: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrState {
    Open,
    /// Merged; the head branch is usually deleted on the remote, so only
    /// `refs/pull/<number>/head` still reaches the head commit there.
    Merged,
    /// Closed without merging.
    Closed,
}

/// Where the pull request stands right now. Unlike [`PullRequest`], which
/// pins what the review was opened on, this is asked again and again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullStatus {
    pub state: PrState,
    /// `None` while nobody has reviewed and no review is required.
    pub review: Option<ReviewDecision>,
    /// `None` when the head commit has no checks at all.
    pub checks: Option<Checks>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    /// The branch rules ask for a review that has not happened yet.
    Required,
}

/// The head commit's CI, folded into one verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checks {
    pub state: CheckState,
    /// The page of the run that started last, for the browser. `None`
    /// when no check names one.
    pub latest_run: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckState {
    Passing,
    /// At least one check is still queued or running, and none failed.
    Running,
    /// At least one check failed; it outranks the ones still running.
    Failing,
}

/// Which pull requests a list asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrList {
    Open,
    /// Merged and closed, behind the picker's "show more" row.
    Finished,
}

/// One row of a pull-request list: enough for picking, nothing more.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrSummary {
    pub number: u64,
    pub title: String,
    pub author: String,
    /// Last update as unix seconds, for the age column.
    pub updated: i64,
    pub state: PrState,
    pub draft: bool,
    /// The head commit's checks folded to one verdict; `None` without any.
    pub checks: Option<CheckState>,
    /// The branch it merges (`headRefName`), and the branch it merges into
    /// (`baseRefName`). A row shows neither; they are what tells a stack's
    /// members apart from each other and from the branch below.
    pub head: String,
    pub base: String,
}

/// A stack as a list shows it: enough for a row, not enough to review.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackSummary {
    /// The forge's number for the stack itself, where it has one.
    pub number: Option<u64>,
    /// The branch the first member merges into.
    pub base: String,
    /// Latest first, like [`Stack::members`].
    pub members: Vec<PrSummary>,
}

/// A resolved stack: every member carries its oids and its row.
#[derive(Clone, Debug)]
pub struct Stack {
    pub number: Option<u64>,
    /// Latest first: the member the comparison's source comes from leads,
    /// the one nearest the target branch is last.
    pub members: Vec<StackMember>,
}

/// One resolved member: the pull request the review is pinned to, and the
/// row a list draws for it. Both come out of the one stack read, because
/// the stack menu shows titles, authors, ages and check verdicts that no
/// [`PullRequest`] carries, and a field per member would mean a call per
/// member.
#[derive(Clone, Debug)]
pub struct StackMember {
    pub pull: PullRequest,
    pub summary: PrSummary,
}

/// Which member of a stack owns each commit of the comparison.
///
/// Total by construction: every commit handed to [`ownership`] sits in
/// exactly one group or in `unclaimed`. The picker seeds a Custom
/// selection from all of them, so a commit in no group would be selected,
/// feed the diff, and be reachable from no checkbox.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ownership {
    /// One group per member, latest first, so a group shares its index
    /// with [`Stack::members`].
    pub groups: Vec<MemberCommits>,
    /// The commits no member contains, in the commit list's order. They
    /// can sit anywhere in it, not only at its end.
    pub unclaimed: Vec<Oid>,
    /// Members whose head is not among the commits, latest first: a member
    /// below has moved on and this one has not rebased onto it, so the
    /// stack is out of date. Reported rather than skipped, because an
    /// empty group hides the fault.
    pub stale: Vec<u64>,
    /// Members whose base branch is not the head branch of the member
    /// below, latest first. The order is what decides ownership, so a list
    /// that is no chain makes the groups under it suspect. Ancestry cannot
    /// answer this: a member the one above has not rebased onto is off the
    /// ancestry line and still a proper link.
    pub off_chain: Vec<u64>,
    /// Which group holds a commit, so the picker and the publish routing
    /// ask the same question without walking the groups.
    by_commit: HashMap<Oid, usize>,
}

/// One member's commits, in the commit list's own order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberCommits {
    pub number: u64,
    /// The member's head branch, for the group header.
    pub branch: String,
    pub commits: Vec<Oid>,
}

impl Ownership {
    /// The member that owns `commit`, `None` when no member contains it.
    pub fn owner(&self, commit: &Oid) -> Option<&MemberCommits> {
        self.groups.get(*self.by_commit.get(commit)?)
    }
}

/// Whether [`open`] may fetch missing commits. The command line never
/// fetches - it prints the fetch to run instead - while the in-app picker
/// fetches on the reviewer's explicit pick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchMode {
    Never,
    OnDemand,
}

/// Resolves a named pull request into the pinned comparison a review runs
/// on. The error is the message to show, word for word: the command line
/// prints it, the window's status line carries it.
pub fn open(
    backend: &dyn ReviewBackend,
    forge: &dyn Forge,
    argument: &PrArgument,
    mode: CompareMode,
    fetch: FetchMode,
) -> Result<(PullRequest, Comparison), String> {
    let pull = forge
        .pull_request(argument)
        .map_err(|error| error.to_string())?;
    if let FetchMode::OnDemand = fetch {
        fetch_missing(backend, &[&pull], &pull)?;
    }
    let target_rev = endpoints(backend, &pull, &pull, fetch)?;
    let comparison = Comparison {
        source: pull.source_branch.clone(),
        target: pull.target_branch.clone(),
        source_rev: Some(pull.source_commit.clone()),
        target_rev: Some(target_rev),
        mode,
        uncommitted: false,
    };
    Ok((pull, comparison))
}

/// Resolves a named stack into the one comparison the whole stack is
/// reviewed as: the latest member's head going into the first member's
/// base. The error is the message to show, word for word.
pub fn open_stack(
    backend: &dyn ReviewBackend,
    forge: &dyn Forge,
    number: u64,
    mode: CompareMode,
    fetch: FetchMode,
) -> Result<(Stack, Comparison), String> {
    let stack = resolve_stack(forge, number)?;
    let (Some(latest), Some(first)) = (stack.members.first(), stack.members.last()) else {
        return Err("the stack has no pull requests to review.".to_string());
    };
    let (latest, first) = (&latest.pull, &first.pull);
    if let FetchMode::OnDemand = fetch {
        let heads: Vec<&PullRequest> = stack.members.iter().map(|member| &member.pull).collect();
        fetch_missing(backend, &heads, first)?;
    }
    let target_rev = endpoints(backend, latest, first, fetch)?;
    let comparison = Comparison {
        source: latest.source_branch.clone(),
        target: first.target_branch.clone(),
        source_rev: Some(latest.source_commit.clone()),
        target_rev: Some(target_rev),
        mode,
        uncommitted: false,
    };
    Ok((stack, comparison))
}

/// The stack of that number, looked up as a stack and nothing else
/// (owner, 2026-11-24): stacks and pull requests number themselves
/// separately, so `--stack` never reaches into `--pr`'s schema.
/// [`Forge::stack_numbered`] is the direct lookup - scanning
/// [`Forge::stacks`] would lose a stack sitting past that list's page.
fn resolve_stack(forge: &dyn Forge, number: u64) -> Result<Stack, String> {
    let refused = match forge.stack_numbered(number) {
        Ok(Some(stack)) => return Ok(stack),
        // A stack the forge knows but reports no members for. The empty
        // list is the answer; `open_stack` is where that is worded.
        Ok(None) => {
            return Ok(Stack {
                number: Some(number),
                members: Vec::new(),
            });
        }
        Err(error) => error.to_string(),
    };
    Err(not_a_stack(forge, number, &refused))
}

/// Why that number is no stack, and what to type instead. Only ever on
/// the way out, so the pull-request lookup it costs never slows a
/// resolve that worked: a reviewer who typed a pull request's number
/// gets handed the number that would have worked.
fn not_a_stack(forge: &dyn Forge, number: u64, refused: &str) -> String {
    match forge.stack(&PrArgument { number, url: None }) {
        Ok(Some(stack)) => match stack.number {
            Some(stack) => format!(
                "{number} is a pull request, not a stack; it belongs to stack {stack} - \
                 try `--stack {stack}`, or `--pr {number}` for that pull request alone."
            ),
            // The forge knows the stack but gives it no number, so
            // nothing can be typed for it.
            None => format!(
                "{number} is a pull request, not a stack; its stack carries no number \
                 here - use `--pr {number}` for that pull request alone."
            ),
        },
        Ok(None) => format!(
            "{number} is a pull request, not a stack, and it is not in one - \
             use `--pr {number}`."
        ),
        // Neither namespace knows it. The stack lookup's words, since
        // that is the question `--stack` asked - a refusal that is not a
        // plain "no such stack" must still reach the reviewer.
        Err(_) => format!(
            "{number} is neither a pull request nor a stack here ({refused}); \
             name a pull request with `--pr`."
        ),
    }
}

/// The revision the comparison's target end is pinned to. `head` is the
/// pull request the source comes from, `base` the one the target comes
/// from; a single pull request is both. Both ends are checked here, so a
/// stack resolves its ends by exactly the rules one pull request does.
fn endpoints(
    backend: &dyn ReviewBackend,
    head: &PullRequest,
    base: &PullRequest,
    fetch: FetchMode,
) -> Result<String, String> {
    let exists = |revision: &str| {
        backend
            .revision_exists(revision)
            .map_err(|error| error.to_string())
    };
    // For fetch hints only; with several remotes the user knows better
    // than a guess.
    let remote_hint = match backend.remotes().unwrap_or_default().as_slice() {
        [only] => only.clone(),
        _ => "<remote>".to_string(),
    };
    // True both before and after an on-demand fetch: the command line
    // never fetched, the picker already did and the hint is what to try
    // by hand.
    let tail = match fetch {
        FetchMode::Never => " - diffie never fetches",
        FetchMode::OnDemand => "",
    };
    // The PR's exact head commit, never a branch lookup: a stale
    // same-named branch must not silently show other content.
    if !exists(&head.source_commit)? {
        return Err(match head.state {
            PrState::Open if head.cross_repository => format!(
                "the pull request's head commit is not here: it comes from {owner}'s \
                 fork. Add that fork as a remote, run `git fetch <fork> {branch}`, \
                 then retry{tail}.",
                owner = head.head_owner,
                branch = head.source_branch,
            ),
            PrState::Open => format!(
                "the pull request's head commit is not here; run \
                 `git fetch {remote_hint} {branch}` and retry{tail}.",
                branch = head.source_branch,
            ),
            // Merged or closed, the head branch is usually deleted, but
            // GitHub keeps the commit reachable under refs/pull - also
            // after a squash or rebase merge, which puts different
            // commits on the base branch.
            PrState::Merged | PrState::Closed => format!(
                "the pull request's head commit is not here; run \
                 `git fetch {remote_hint} refs/pull/{number}/head` and retry{tail}.",
                number = head.number,
            ),
        });
    }
    match base.state {
        // The base as the PR was compared against it: the live base
        // already contains a merged PR, which would collapse the diff
        // to nothing. gh's baseRefOid, not the merge commit's first
        // parent - a base rebuilt under the PR moves that parent off
        // the PR's own comparison and drags foreign commits in.
        PrState::Merged => {
            if !exists(&base.target_commit)? {
                return Err(format!(
                    "the pull request's base commit is not here; run \
                     `git fetch {remote_hint}` and retry{tail}."
                ));
            }
            Ok(base.target_commit.clone())
        }
        // Open (or closed unmerged), the base moves on, so follow its
        // remote-tracking ref - never a same-named local branch, which
        // can lag.
        PrState::Open | PrState::Closed => {
            let branch = &base.target_branch;
            remote_ref(backend, branch, || {
                format!(
                    "`{branch}` is not fetched here; run \
                     `git fetch {remote_hint} {branch}` and retry{tail}."
                )
            })
        }
    }
}

/// A branch only the remotes know resolves when exactly one remote has it;
/// several are a question the user must answer, not a guess. `missing` words
/// the no-remote case, because the callers know different things. Public
/// so the command line's revision fallback resolves by the same rule and
/// in the same words.
pub fn remote_ref(
    backend: &dyn ReviewBackend,
    branch: &str,
    missing: impl FnOnce() -> String,
) -> Result<String, String> {
    let remotes = backend
        .remotes_with_branch(branch)
        .map_err(|error| error.to_string())?;
    match remotes.as_slice() {
        [remote] => Ok(format!("{remote}/{branch}")),
        [] => Err(missing()),
        _ => Err(format!(
            "`{branch}` is on more than one remote ({}); say which, e.g. `{}/{branch}`.",
            remotes.join(", "),
            remotes[0]
        )),
    }
}

/// The picker's fetch: one explicit `git fetch` when the commits under
/// review are not here yet. `refs/pull/<n>/head` reaches a head commit
/// uniformly - forks and merged pull requests included - and the base
/// branch of `base`, the pull request the comparison's target comes from,
/// rides along so its remote-tracking ref is fresh.
///
/// One call however many pull requests `pulls` holds: the sanctioned
/// exception to never writing to the repository is one fetch per explicit
/// action, not one per stack member.
fn fetch_missing(
    backend: &dyn ReviewBackend,
    pulls: &[&PullRequest],
    base: &PullRequest,
) -> Result<(), String> {
    let mut head_missing = false;
    for pull in pulls {
        head_missing |= !backend
            .revision_exists(&pull.source_commit)
            .map_err(|error| error.to_string())?;
    }
    let target_missing = match base.state {
        PrState::Merged => !backend
            .revision_exists(&base.target_commit)
            .map_err(|error| error.to_string())?,
        PrState::Open | PrState::Closed => backend
            .remotes_with_branch(&base.target_branch)
            .map_err(|error| error.to_string())?
            .is_empty(),
    };
    if !head_missing && !target_missing {
        return Ok(());
    }
    let remotes = backend.remotes().map_err(|error| error.to_string())?;
    let remote = match remotes.as_slice() {
        [] => {
            return Err(
                "no remote is configured; fetch the pull request's commits and retry.".to_string(),
            );
        }
        [only] => only.clone(),
        _ => {
            // Several remotes: only the one already carrying the base
            // branch is safe to assume; anything else is a guess.
            let carrying = backend
                .remotes_with_branch(&base.target_branch)
                .map_err(|error| error.to_string())?;
            match carrying.as_slice() {
                [only] => only.clone(),
                _ => {
                    return Err(format!(
                        "several remotes are configured ({}); fetch the pull request \
                         yourself, e.g. `git fetch {} refs/pull/{}/head`, and retry.",
                        remotes.join(", "),
                        remotes[0],
                        base.number,
                    ));
                }
            }
        }
    };
    let refspecs: Vec<String> = std::iter::once(base.target_branch.clone())
        .chain(
            pulls
                .iter()
                .map(|pull| format!("refs/pull/{}/head", pull.number)),
        )
        .collect();
    backend
        .fetch(&remote, &refspecs)
        .map_err(|error| error.to_string())
}

/// Which member of `stack` owns each of `commits`: the lowest member -
/// the one nearest the target branch - that contains a commit owns it.
/// Separate and tested because the picker groups by it and publishing
/// routes by it, and both must agree. The error is the message to show.
///
/// Position cannot decide this. The commit list is committer-date ordered
/// over the whole reachable set, merge commits included, so a member kept
/// current by merging the member below rather than rebasing onto it puts
/// that member's head in the middle of the list; an index range taken
/// from there hands the lower member the commits of the one above, which
/// would then publish its comments to the wrong pull request.
///
/// One ancestry call per member, so the cost follows the stack's size,
/// not the comparison's length.
pub fn ownership(
    backend: &dyn ReviewBackend,
    stack: &Stack,
    commits: &[Commit],
) -> Result<Ownership, String> {
    let ids: Vec<Oid> = commits.iter().map(|commit| commit.id.clone()).collect();
    let listed: HashSet<&str> = ids.iter().map(Oid::as_str).collect();
    let mut groups = Vec::with_capacity(stack.members.len());
    let mut claimed: HashSet<Oid> = HashSet::new();
    let mut stale = Vec::new();
    // Lowest first, so the member nearest the target branch claims a
    // commit before any member above it sees it.
    for pull in stack.members.iter().rev().map(|member| &member.pull) {
        let mut group = MemberCommits {
            number: pull.number,
            branch: pull.source_branch.clone(),
            commits: Vec::new(),
        };
        // A head outside the comparison is the routine "the member below
        // took a review fix and this one has not rebased" state. Its
        // earlier commits are still in the list, so it is still asked -
        // unless the head is not here at all and nothing can be asked.
        let reachable = if listed.contains(pull.source_commit.as_str()) {
            true
        } else {
            stale.push(pull.number);
            backend
                .revision_exists(&pull.source_commit)
                .map_err(|error| error.to_string())?
        };
        if reachable {
            let mine = backend
                .contains_commits(&pull.source_commit, &ids)
                .map_err(|error| error.to_string())?;
            for id in mine {
                if claimed.insert(id.clone()) {
                    group.commits.push(id);
                }
            }
        }
        groups.push(group);
    }
    groups.reverse();
    stale.reverse();
    Ok(Ownership {
        by_commit: groups
            .iter()
            .enumerate()
            .flat_map(|(index, group)| group.commits.iter().map(move |id| (id.clone(), index)))
            .collect(),
        unclaimed: ids
            .iter()
            .filter(|id| !claimed.contains(id))
            .cloned()
            .collect(),
        groups,
        stale,
        off_chain: off_chain(&stack.members),
    })
}

/// The members whose base branch does not name the member below them.
/// `members` is latest first, as [`Stack::members`] is.
fn off_chain(members: &[StackMember]) -> Vec<u64> {
    members
        .windows(2)
        .filter(|link| link[0].pull.target_branch != link[1].pull.source_branch)
        .map(|link| link[0].pull.number)
        .collect()
}

/// Parses the `--pr` value: a decimal number, or an http(s) URL shaped
/// `https://<host>/<owner>/<repo>/pull/<number>` with trailing path segments,
/// query or fragment ignored. Pure, so the command line refuses a typo
/// before anything runs; the error is the message to show.
pub fn parse(value: &str) -> Result<PrArgument, String> {
    let complaint = || {
        format!(
            "`{value}` is not a pull request: give a number, \
             or a URL like https://github.com/owner/repo/pull/17"
        )
    };
    let number = |segment: &str| -> Result<u64, String> {
        // `str::parse` alone would take a leading `+`.
        if segment.is_empty() || !segment.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(complaint());
        }
        segment.parse().map_err(|_| complaint())
    };
    if value
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_digit())
    {
        return Ok(PrArgument {
            number: number(value)?,
            url: None,
        });
    }
    let Some(rest) = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
    else {
        return Err(complaint());
    };
    // The query and fragment never carry the number; cut them before
    // splitting the path.
    let path = rest.split(['?', '#']).next().unwrap_or_default();
    let mut segments = path.split('/');
    let host = segments.next().unwrap_or_default();
    let owner = segments.next().unwrap_or_default();
    let repository = segments.next().unwrap_or_default();
    let pull = segments.next().unwrap_or_default();
    let digits = segments.next().unwrap_or_default();
    if host.is_empty() || owner.is_empty() || repository.is_empty() || pull != "pull" {
        return Err(complaint());
    }
    Ok(PrArgument {
        number: number(digits)?,
        url: Some(value.to_string()),
    })
}

/// Parses the `--stack` value: a stack's own number, nothing else.
/// Stacks and pull requests number themselves separately (owner,
/// 2026-11-24), so neither flag takes the other's names - a pull
/// request, by number or URL, belongs to `--pr`.
pub fn parse_stack(value: &str) -> Result<u64, String> {
    // Where `--pr` belongs is `--help`'s job, not every refusal's.
    let complaint = || format!("`{value}` is not a stack number.");
    // `str::parse` alone would take a leading `+`.
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(complaint());
    }
    value.parse().map_err(|_| complaint())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_bare_number() {
        let argument = parse("17").unwrap();
        assert_eq!(argument.number, 17);
        assert_eq!(argument.url, None);
    }

    #[test]
    fn parses_a_github_url_and_keeps_it() {
        let argument = parse("https://github.com/sassman/gpui-kit/pull/42").unwrap();
        assert_eq!(argument.number, 42);
        assert_eq!(
            argument.url.as_deref(),
            Some("https://github.com/sassman/gpui-kit/pull/42")
        );
    }

    #[test]
    fn parses_an_enterprise_host() {
        let argument = parse("https://github.example.corp/team/service/pull/7").unwrap();
        assert_eq!(argument.number, 7);
        assert!(argument.url.is_some());
    }

    #[test]
    fn ignores_trailing_path_query_and_fragment() {
        for url in [
            "https://github.com/o/r/pull/5/files",
            "https://github.com/o/r/pull/5/commits/abc123",
            "https://github.com/o/r/pull/5?diff=split",
            "https://github.com/o/r/pull/5#discussion_r1",
        ] {
            assert_eq!(parse(url).unwrap().number, 5, "{url}");
        }
    }

    #[test]
    fn refuses_a_host_less_value() {
        let message = parse("o/r/pull/5").unwrap_err();
        assert!(
            message.contains("github.com/owner/repo/pull/17"),
            "{message}"
        );
    }

    #[test]
    fn refuses_a_url_without_pull() {
        assert!(parse("https://github.com/o/r/issues/5").is_err());
        assert!(parse("https://github.com/o/r").is_err());
    }

    #[test]
    fn refuses_a_non_numeric_number_segment() {
        assert!(parse("https://github.com/o/r/pull/abc").is_err());
        assert!(parse("https://github.com/o/r/pull/+5").is_err());
        assert!(parse("https://github.com/o/r/pull/").is_err());
    }

    #[test]
    fn a_stack_takes_a_number_and_nothing_else() {
        assert_eq!(parse_stack("15082").unwrap(), 15082);
        // A pull request, by number or URL, is `--pr`'s to name.
        for value in [
            "https://github.com/o/r/pull/5",
            "#42",
            "+5",
            "",
            "feat/stack-model",
        ] {
            let message = parse_stack(value).unwrap_err();
            assert!(message.contains("not a stack number"), "{value}: {message}");
        }
    }

    /// Only the chain matters here, so the row is the barest one that
    /// still names the member.
    fn member(number: u64, head: &str, base: &str) -> StackMember {
        let pull = PullRequest {
            number,
            id: format!("PR_{number}"),
            url: format!("https://github.com/o/r/pull/{number}"),
            state: PrState::Open,
            source_branch: head.into(),
            source_commit: "h".repeat(40),
            target_branch: base.into(),
            target_commit: "b".repeat(40),
            cross_repository: false,
            head_owner: "o".into(),
        };
        let summary = PrSummary {
            number,
            title: format!("#{number}"),
            author: "ada".into(),
            updated: 0,
            state: pull.state,
            draft: false,
            checks: None,
            head: head.into(),
            base: base.into(),
        };
        StackMember { pull, summary }
    }

    #[test]
    fn a_chain_of_members_reports_no_broken_link() {
        let members = vec![
            member(3, "feat/c", "feat/b"),
            member(2, "feat/b", "feat/a"),
            member(1, "feat/a", "main"),
        ];
        assert_eq!(off_chain(&members), Vec::<u64>::new());
    }

    #[test]
    fn a_member_based_past_the_one_below_is_off_the_chain() {
        // #3 is based on main, so the list is no chain and the groups
        // under it cannot be trusted to be ordered base-first.
        let members = vec![
            member(3, "feat/c", "main"),
            member(2, "feat/b", "feat/a"),
            member(1, "feat/a", "main"),
        ];
        assert_eq!(off_chain(&members), vec![3]);
    }
}
