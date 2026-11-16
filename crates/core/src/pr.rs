//! `--pr`: the pull request a review is connected to, as a
//! [`Forge`](crate::forge::Forge) describes it, and the parsing of the
//! command-line value that names it.

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
