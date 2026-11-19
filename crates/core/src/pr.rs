//! `--pr`: the pull request a review is connected to, as a
//! [`Forge`](crate::forge::Forge) describes it, the parsing of the
//! command-line value that names it, and [`open`], which turns the named
//! pull request into the comparison a review runs on - for the command
//! line and for the in-app picker alike.

use crate::backend::ReviewBackend;
use crate::forge::Forge;
use crate::model::{CompareMode, Comparison};

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
        fetch_missing(backend, &pull)?;
    }
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
    if !exists(&pull.source_commit)? {
        return Err(match pull.state {
            PrState::Open if pull.cross_repository => format!(
                "the pull request's head commit is not here: it comes from {owner}'s \
                 fork. Add that fork as a remote, run `git fetch <fork> {branch}`, \
                 then retry{tail}.",
                owner = pull.head_owner,
                branch = pull.source_branch,
            ),
            PrState::Open => format!(
                "the pull request's head commit is not here; run \
                 `git fetch {remote_hint} {branch}` and retry{tail}.",
                branch = pull.source_branch,
            ),
            // Merged or closed, the head branch is usually deleted, but
            // GitHub keeps the commit reachable under refs/pull - also
            // after a squash or rebase merge, which puts different
            // commits on the base branch.
            PrState::Merged | PrState::Closed => format!(
                "the pull request's head commit is not here; run \
                 `git fetch {remote_hint} refs/pull/{number}/head` and retry{tail}.",
                number = pull.number,
            ),
        });
    }
    let target_rev = match pull.state {
        // The base as the PR was compared against it: the live base
        // already contains a merged PR, which would collapse the diff
        // to nothing. gh's baseRefOid, not the merge commit's first
        // parent - a base rebuilt under the PR moves that parent off
        // the PR's own comparison and drags foreign commits in.
        PrState::Merged => {
            if !exists(&pull.target_commit)? {
                return Err(format!(
                    "the pull request's base commit is not here; run \
                     `git fetch {remote_hint}` and retry{tail}."
                ));
            }
            pull.target_commit.clone()
        }
        // Open (or closed unmerged), the base moves on, so follow its
        // remote-tracking ref - never a same-named local branch, which
        // can lag.
        PrState::Open | PrState::Closed => {
            let branch = &pull.target_branch;
            remote_ref(backend, branch, || {
                format!(
                    "`{branch}` is not fetched here; run \
                     `git fetch {remote_hint} {branch}` and retry{tail}."
                )
            })?
        }
    };
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

/// The picker's fetch: one explicit `git fetch` when the pull request's
/// commits are not here yet. `refs/pull/<n>/head` reaches the head commit
/// uniformly - forks and merged pull requests included - and the base
/// branch rides along so its remote-tracking ref is fresh.
fn fetch_missing(backend: &dyn ReviewBackend, pull: &PullRequest) -> Result<(), String> {
    let head_missing = !backend
        .revision_exists(&pull.source_commit)
        .map_err(|error| error.to_string())?;
    let target_missing = match pull.state {
        PrState::Merged => !backend
            .revision_exists(&pull.target_commit)
            .map_err(|error| error.to_string())?,
        PrState::Open | PrState::Closed => backend
            .remotes_with_branch(&pull.target_branch)
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
                .remotes_with_branch(&pull.target_branch)
                .map_err(|error| error.to_string())?;
            match carrying.as_slice() {
                [only] => only.clone(),
                _ => {
                    return Err(format!(
                        "several remotes are configured ({}); fetch the pull request \
                         yourself, e.g. `git fetch {} refs/pull/{}/head`, and retry.",
                        remotes.join(", "),
                        remotes[0],
                        pull.number,
                    ));
                }
            }
        }
    };
    let refspecs = vec![
        pull.target_branch.clone(),
        format!("refs/pull/{}/head", pull.number),
    ];
    backend
        .fetch(&remote, &refspecs)
        .map_err(|error| error.to_string())
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
}
