//! `--pr`: a GitHub pull request resolved to its branches through the
//! GitHub CLI.
//!
//! gh does the talking because it already carries the user's authentication
//! and knows GitHub Enterprise hosts. Its embedded jq (`--jq`) hands back
//! plain lines, so the crate stays free of HTTP and JSON dependencies.

use std::path::Path;
use std::process::{Command, Stdio};

/// The `--pr` value, parsed but not yet resolved.
#[derive(Debug, PartialEq, Eq)]
pub struct PrArgument {
    pub number: u64,
    /// The URL as typed, when one was given. gh gets it verbatim, which pins
    /// host and repository (GitHub Enterprise included); a bare number leaves
    /// gh to find the repository from the working directory's remotes.
    pub url: Option<String>,
}

/// What gh reports about the pull request.
#[derive(Debug)]
pub struct PullRequest {
    pub number: u64,
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

#[derive(Debug, thiserror::Error)]
pub enum PrError {
    #[error("GitHub CLI (gh) is required for --pr, and it was not found on PATH: {0}")]
    GhMissing(std::io::Error),
    #[error("gh pr view failed: {0}")]
    GhFailed(String),
    #[error("unexpected output from gh pr view: {0:?}")]
    Output(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
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

/// Asks gh for the pull request's state, branches and exact commits.
/// `repository` becomes the process working directory, so a bare number
/// resolves against this checkout's remotes.
pub fn resolve(repository: &Path, argument: &PrArgument) -> Result<PullRequest, PrError> {
    let selector = argument
        .url
        .clone()
        .unwrap_or_else(|| argument.number.to_string());
    let output = Command::new("gh")
        .args(["pr", "view"])
        .arg(&selector)
        .args([
            "--json",
            "state,headRefName,headRefOid,baseRefName,baseRefOid,isCrossRepository,headRepositoryOwner",
            // One value per line; `// ""` keeps the line count stable when a
            // fork's owner cannot be read.
            "--jq",
            ".state, .headRefName, .headRefOid, .baseRefName, .baseRefOid, .isCrossRepository, .headRepositoryOwner.login // \"\"",
        ])
        .current_dir(repository)
        // gh's errors reach the user verbatim; keep them in English.
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => PrError::GhMissing(error),
            _ => PrError::Io(error),
        })?;
    if !output.status.success() {
        return Err(PrError::GhFailed(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let unexpected = || PrError::Output(stdout.trim().to_string());
    let mut lines = stdout.lines();
    let (
        Some(state),
        Some(source_branch),
        Some(source_commit),
        Some(target_branch),
        Some(target_commit),
        Some(cross),
    ) = (
        lines.next(),
        lines.next(),
        lines.next(),
        lines.next(),
        lines.next(),
        lines.next(),
    )
    else {
        return Err(unexpected());
    };
    let state = match state {
        "OPEN" => PrState::Open,
        "MERGED" => PrState::Merged,
        "CLOSED" => PrState::Closed,
        _ => return Err(unexpected()),
    };
    let cross_repository = match cross {
        "true" => true,
        "false" => false,
        _ => return Err(unexpected()),
    };
    if source_commit.is_empty() || target_commit.is_empty() {
        return Err(unexpected());
    }
    Ok(PullRequest {
        number: argument.number,
        state,
        source_branch: source_branch.to_string(),
        source_commit: source_commit.to_string(),
        target_branch: target_branch.to_string(),
        target_commit: target_commit.to_string(),
        cross_repository,
        head_owner: lines.next().unwrap_or_default().to_string(),
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
