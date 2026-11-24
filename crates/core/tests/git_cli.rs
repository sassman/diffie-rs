//! Runs the cheap backend against a real, throwaway repository.

use std::path::Path;
use std::process::Command;

use diffie_core::pr::{
    FetchMode, Ownership, PrArgument, PrState, PrSummary, PullRequest, Stack, StackSummary,
    open_stack, ownership,
};
use diffie_core::review::{ReviewInput, ReviewedFile};
use diffie_core::{
    BackendError, Commit, CompareMode, Comparison, FileState, FileStatus, GitCli, NewSide, Oid,
    RepoState, ResolutionPush, ReviewBackend, Scope,
};

fn git(directory: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Ada")
        .env("GIT_AUTHOR_EMAIL", "ada@example.invalid")
        .env("GIT_COMMITTER_NAME", "Ada")
        .env("GIT_COMMITTER_EMAIL", "ada@example.invalid")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Like [`git`], but committing at a fixed instant so a test can assert
/// the clocks the conflict labels carry.
fn git_at(directory: &Path, date: &str, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Ada")
        .env("GIT_AUTHOR_EMAIL", "ada@example.invalid")
        .env("GIT_COMMITTER_NAME", "Ada")
        .env("GIT_COMMITTER_EMAIL", "ada@example.invalid")
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn lines(changed: &[(usize, &str)]) -> String {
    (1..=10)
        .map(|number| {
            changed
                .iter()
                .find(|(line, _)| *line == number)
                .map(|(_, text)| format!("{text}\n"))
                .unwrap_or_else(|| format!("line {number}\n"))
        })
        .collect()
}

struct Fixture {
    directory: tempfile::TempDir,
    /// Oldest first: c1 edits line 2, c2 adds b.txt, c3 edits line 9,
    /// c4 edits line 2 again, c5 edits line 9 again.
    commits: Vec<Oid>,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        git(root, &["init", "-q", "-b", "main"]);
        git(root, &["config", "user.name", "Ada"]);
        git(root, &["config", "user.email", "ada@example.invalid"]);
        // The fixtures write and assert \n; Windows' default autocrlf
        // would hand CRLF back after checkouts.
        git(root, &["config", "core.autocrlf", "false"]);
        std::fs::write(root.join("a.txt"), lines(&[])).unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-q", "-m", "base"]);
        git(root, &["checkout", "-q", "-b", "feat"]);

        let mut commits = Vec::new();
        let mut commit = |message: &str| {
            git(root, &["add", "."]);
            git(root, &["commit", "-q", "-m", message]);
            commits.push(Oid::new(git(root, &["rev-parse", "HEAD"])));
        };
        std::fs::write(root.join("a.txt"), lines(&[(2, "two")])).unwrap();
        commit("c1 edit line 2");
        std::fs::write(root.join("b.txt"), "hello\n").unwrap();
        commit("c2 add b");
        std::fs::write(root.join("a.txt"), lines(&[(2, "two"), (9, "nine")])).unwrap();
        commit("c3 edit line 9");
        std::fs::write(root.join("a.txt"), lines(&[(2, "TWO"), (9, "nine")])).unwrap();
        commit("c4 edit line 2 again");
        std::fs::write(root.join("a.txt"), lines(&[(2, "TWO"), (9, "NINE")])).unwrap();
        commit("c5 edit line 9 again");
        Self { directory, commits }
    }

    fn backend(&self) -> GitCli {
        GitCli::open(self.directory.path()).unwrap()
    }

    fn comparison() -> Comparison {
        Comparison {
            source: "feat".into(),
            target: "main".into(),
            mode: CompareMode::BranchAdds,
            uncommitted: false,
            ..Default::default()
        }
    }

    fn loose_objects(&self) -> usize {
        fn count(directory: &Path) -> usize {
            std::fs::read_dir(directory)
                .map(|entries| {
                    entries
                        .flatten()
                        .map(|entry| {
                            let path = entry.path();
                            if path.is_dir() { count(&path) } else { 1 }
                        })
                        .sum()
                })
                .unwrap_or(0)
        }
        count(&self.directory.path().join(".git/objects"))
    }
}

fn paths(files: &[diffie_core::FileSummary]) -> Vec<&str> {
    let mut paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
    paths.sort();
    paths
}

#[test]
fn resolves_and_lists_commits_newest_first() {
    let fixture = Fixture::new();
    let backend = fixture.backend();
    let resolved = backend.resolve(&Fixture::comparison()).unwrap();
    assert_eq!(resolved.head, fixture.commits[4]);
    assert_eq!(resolved.base, resolved.target_head);

    let commits = backend.commits(&resolved).unwrap();
    let subjects: Vec<&str> = commits
        .iter()
        .map(|commit| commit.subject.as_str())
        .collect();
    assert_eq!(
        subjects,
        [
            "c5 edit line 9 again",
            "c4 edit line 2 again",
            "c3 edit line 9",
            "c2 add b",
            "c1 edit line 2"
        ]
    );
    assert_eq!(commits[0].author, "Ada");
    assert!(commits[0].time > 0);
}

#[test]
fn reports_unknown_revisions_and_non_repositories() {
    let fixture = Fixture::new();
    let mut comparison = Fixture::comparison();
    comparison.source = "--output=/tmp/x".into();
    assert!(matches!(
        fixture.backend().resolve(&comparison),
        Err(BackendError::UnknownRevision(_))
    ));
    let empty = tempfile::tempdir().unwrap();
    assert!(matches!(
        GitCli::open(empty.path()),
        Err(BackendError::NotARepository(_))
    ));
}

#[test]
fn finds_revisions_locally_and_on_remotes() {
    let fixture = Fixture::new();
    let root = fixture.directory.path();
    let backend = fixture.backend();
    assert!(backend.revision_exists("feat").unwrap());
    assert!(backend.revision_exists("HEAD~1").unwrap());
    assert!(!backend.revision_exists("nope").unwrap());

    // Where a remote points is irrelevant; only its fetched refs matter.
    assert!(backend.remotes().unwrap().is_empty());
    git(root, &["remote", "add", "o", "."]);
    git(root, &["remote", "add", "fork", "."]);
    assert_eq!(backend.remotes().unwrap(), ["fork", "o"]);
    git(root, &["update-ref", "refs/remotes/o/topic", "HEAD"]);
    git(root, &["update-ref", "refs/remotes/o/solo", "HEAD"]);
    git(root, &["update-ref", "refs/remotes/fork/topic", "HEAD"]);
    assert_eq!(backend.remotes_with_branch("solo").unwrap(), ["o"]);
    // `git remote` lists alphabetically; the order carries into the answer.
    assert_eq!(backend.remotes_with_branch("topic").unwrap(), ["fork", "o"]);
    assert!(backend.remotes_with_branch("nope").unwrap().is_empty());
}

#[test]
fn pinned_revisions_outrank_the_display_names() {
    // A --pr review resolves the PR's exact commits; source and target are
    // then only names for the title and the review file, and need not exist.
    let fixture = Fixture::new();
    let backend = fixture.backend();
    let comparison = Comparison {
        source: "feat/typed-groups".into(),
        target: "refactor/base".into(),
        source_rev: Some(fixture.commits[4].to_string()),
        target_rev: Some("main".into()),
        mode: CompareMode::BranchAdds,
        uncommitted: false,
    };
    let resolved = backend.resolve(&comparison).unwrap();
    assert_eq!(resolved.head, fixture.commits[4]);
    assert_eq!(resolved.base, resolved.target_head);
    assert_eq!(backend.commits(&resolved).unwrap().len(), 5);
}

#[test]
fn whole_branch_covers_every_file() {
    let fixture = Fixture::new();
    let backend = fixture.backend();
    let resolved = backend.resolve(&Fixture::comparison()).unwrap();
    let patch = backend
        .patch(&resolved, &Scope::WholeBranch, ReviewInput::EMPTY, false)
        .unwrap();
    assert_eq!(paths(&patch.files), ["a.txt", "b.txt"]);
    assert_eq!(patch.new_side, NewSide::Commit(resolved.head.clone()));
    let a = patch
        .files
        .iter()
        .find(|file| file.path == "a.txt")
        .unwrap();
    assert_eq!((a.additions, a.deletions), (2, 2));
    let b = patch
        .files
        .iter()
        .find(|file| file.path == "b.txt")
        .unwrap();
    assert_eq!(b.status, FileStatus::Added);
    // Generous context: the whole ten-line file is in the patch.
    assert!(patch.text.contains(" line 5\n"));
}

#[test]
fn ignore_whitespace_hides_whitespace_only_changes() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    git(root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("w.txt"), "hello\nworld\n").unwrap();
    std::fs::write(root.join("real.txt"), "old\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "base"]);
    git(root, &["checkout", "-q", "-b", "feat"]);
    std::fs::write(root.join("w.txt"), "  hello\nworld   \n").unwrap();
    std::fs::write(root.join("real.txt"), "new\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "reindent w, edit real"]);

    let backend = GitCli::open(root).unwrap();
    let resolved = backend.resolve(&Fixture::comparison()).unwrap();
    let plain = backend
        .patch(&resolved, &Scope::WholeBranch, ReviewInput::EMPTY, false)
        .unwrap();
    assert_eq!(paths(&plain.files), ["real.txt", "w.txt"]);
    let tidy = backend
        .patch(&resolved, &Scope::WholeBranch, ReviewInput::EMPTY, true)
        .unwrap();
    assert_eq!(paths(&tidy.files), ["real.txt"]);
    // The file list is a scope concern and stays complete either way.
    assert_eq!(
        backend.changed_paths(&resolved).unwrap(),
        ["real.txt", "w.txt"]
    );
}

#[test]
fn changed_paths_cover_the_whole_branch_at_head_names() {
    let fixture = Fixture::new();
    // The rename happens inside the range, so only the head-side name
    // may show up.
    git(fixture.directory.path(), &["mv", "b.txt", "renamed.txt"]);
    git(fixture.directory.path(), &["commit", "-qm", "c6 rename b"]);
    let backend = fixture.backend();
    let resolved = backend.resolve(&Fixture::comparison()).unwrap();
    let mut paths = backend.changed_paths(&resolved).unwrap();
    paths.sort();
    assert_eq!(paths, ["a.txt", "renamed.txt"]);
}

#[test]
fn commits_touching_maps_each_file_newest_first() {
    let fixture = Fixture::new();
    let backend = fixture.backend();
    let resolved = backend.resolve(&Fixture::comparison()).unwrap();
    let touched = backend.commits_touching(&resolved).unwrap();
    assert_eq!(touched.len(), 2);
    assert_eq!(touched["b.txt"], [fixture.commits[1].clone()]);
    let a_edits: Vec<Oid> = [4, 3, 2, 0]
        .map(|index| fixture.commits[index].clone())
        .into();
    assert_eq!(touched["a.txt"], a_edits);
}

#[test]
fn a_contiguous_selection_is_a_plain_range() {
    let fixture = Fixture::new();
    let backend = fixture.backend();
    let resolved = backend.resolve(&Fixture::comparison()).unwrap();
    let picked = vec![fixture.commits[2].clone(), fixture.commits[1].clone()];
    let patch = backend
        .patch(
            &resolved,
            &Scope::Commits {
                picked,
                uncommitted: false,
            },
            ReviewInput::EMPTY,
            false,
        )
        .unwrap();
    assert_eq!(paths(&patch.files), ["a.txt", "b.txt"]);
    assert_eq!(patch.new_side, NewSide::Commit(fixture.commits[2].clone()));
    assert!(patch.text.contains("+nine"));
    assert!(!patch.text.contains("+two"), "c1 is outside the range");
}

#[test]
fn skipped_commits_are_left_out_without_writing_to_the_repository() {
    let fixture = Fixture::new();
    let backend = fixture.backend();
    let resolved = backend.resolve(&Fixture::comparison()).unwrap();
    let before = fixture.loose_objects();

    // c1 and c3, skipping c2 (which adds b.txt).
    let picked = vec![fixture.commits[0].clone(), fixture.commits[2].clone()];
    let patch = backend
        .patch(
            &resolved,
            &Scope::Commits {
                picked,
                uncommitted: false,
            },
            ReviewInput::EMPTY,
            false,
        )
        .unwrap();
    assert_eq!(patch.new_side, NewSide::Synthetic);
    assert_eq!(paths(&patch.files), ["a.txt"]);
    assert!(patch.text.contains("+two") && patch.text.contains("+nine"));
    assert!(
        patch
            .files
            .iter()
            .all(|file| file.state == FileState::Changed)
    );

    assert_eq!(
        fixture.loose_objects(),
        before,
        "the repository gained objects"
    );
    assert!(git(fixture.directory.path(), &["status", "--porcelain"]).is_empty());
}

#[test]
fn a_file_that_cannot_be_combined_is_reported() {
    let fixture = Fixture::new();
    let backend = fixture.backend();
    let resolved = backend.resolve(&Fixture::comparison()).unwrap();
    // c2 and c5, skipping c3: c5 rewrites the line c3 introduced.
    let picked = vec![fixture.commits[1].clone(), fixture.commits[4].clone()];
    let patch = backend
        .patch(
            &resolved,
            &Scope::Commits {
                picked,
                uncommitted: false,
            },
            ReviewInput::EMPTY,
            false,
        )
        .unwrap();
    let a = patch
        .files
        .iter()
        .find(|file| file.path == "a.txt")
        .unwrap();
    assert_eq!(
        a.state,
        FileState::CouldNotCombine {
            commit: fixture.commits[4].clone()
        }
    );
    let b = patch
        .files
        .iter()
        .find(|file| file.path == "b.txt")
        .unwrap();
    assert_eq!(b.state, FileState::Changed);
    assert!(
        !patch.text.contains("NINE"),
        "the failed commit is left out"
    );
}

#[test]
fn blame_finds_the_commit_that_wrote_a_line() {
    let fixture = Fixture::new();
    let backend = fixture.backend();
    let head = fixture.commits[4].clone();
    let origin = backend.line_origin(&head, "a.txt", 9).unwrap().unwrap();
    assert_eq!(origin.commit, fixture.commits[4]);
    assert_eq!(origin.line, 9);
    assert_eq!(backend.line_origin(&head, "a.txt", 99).unwrap(), None);
    assert_eq!(backend.line_origin(&head, "missing.txt", 1).unwrap(), None);

    let blame = backend.blame(&head, "a.txt").unwrap();
    assert_eq!(blame.len(), 10);
    assert_eq!(blame[1].origin.commit, fixture.commits[3]);
}

#[test]
fn changed_regions_follow_a_commented_line_into_a_newer_revision() {
    let fixture = Fixture::new();
    let backend = fixture.backend();
    // A comment written on c1's line 2 ("two"): c4 rewrote that line, so a
    // blame at head no longer finds it. The regions of c1..head carry it to
    // the replacement instead.
    let regions = backend
        .changed_regions(&fixture.commits[0], &fixture.commits[4], "a.txt")
        .unwrap()
        .unwrap();
    use diffie_core::review::{LineFate, map_line};
    assert_eq!(map_line(2, &regions), LineFate::Rewritten { line: 2 });
    assert_eq!(map_line(9, &regions), LineFate::Rewritten { line: 9 });
    assert_eq!(map_line(5, &regions), LineFate::Untouched { line: 5 });

    // A revision git does not have - a rebase dropped the comment's commit -
    // is "cannot answer", never an error or a guess.
    let gone = Oid::new("b".repeat(40));
    assert_eq!(
        backend
            .changed_regions(&gone, &fixture.commits[4], "a.txt")
            .unwrap(),
        None
    );
}

#[test]
fn new_for_me_compares_each_file_with_its_tick() {
    let fixture = Fixture::new();
    let backend = fixture.backend();
    let resolved = backend.resolve(&Fixture::comparison()).unwrap();
    let tick = |path: &str, commit: &Oid| ReviewedFile {
        path: path.into(),
        blob: backend.blob_id(commit, path).unwrap(),
        commit: commit.clone(),
        at: "2026-11-11T00:00:00Z".into(),
    };

    // Nothing ticked: same as the whole branch.
    let patch = backend
        .patch(&resolved, &Scope::NewForMe, ReviewInput::EMPTY, false)
        .unwrap();
    assert_eq!(paths(&patch.files), ["a.txt", "b.txt"]);

    // a.txt was reviewed at c1, b.txt at head.
    let reviewed = [
        tick("a.txt", &fixture.commits[0]),
        tick("b.txt", &resolved.head),
    ];
    let patch = backend
        .patch(
            &resolved,
            &Scope::NewForMe,
            ReviewInput {
                reviewed: &reviewed,
                commented: &[],
            },
            false,
        )
        .unwrap();
    let a = patch
        .files
        .iter()
        .find(|file| file.path == "a.txt")
        .unwrap();
    assert_eq!(a.state, FileState::Changed);
    assert!(patch.text.contains("diff --git a/a.txt b/a.txt"));
    assert!(
        patch.text.contains("-two\n+TWO"),
        "only what changed since c1"
    );
    assert!(patch.text.contains("+NINE"));
    assert!(
        !patch.text.contains("-line 2"),
        "c1's own change was already reviewed"
    );
    let b = patch
        .files
        .iter()
        .find(|file| file.path == "b.txt")
        .unwrap();
    assert_eq!(b.state, FileState::UnchangedSinceReview);
    assert!(!patch.text.contains("b.txt"));
}

#[test]
fn new_for_me_keeps_a_reviewed_deletion_collapsed() {
    let fixture = Fixture::new();
    let root = fixture.directory.path();
    std::fs::remove_file(root.join("a.txt")).unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "c6 delete a"]);
    let backend = fixture.backend();
    let resolved = backend.resolve(&Fixture::comparison()).unwrap();

    // Unticked, the deletion shows in full.
    let patch = backend
        .patch(&resolved, &Scope::NewForMe, ReviewInput::EMPTY, false)
        .unwrap();
    let a = patch
        .files
        .iter()
        .find(|file| file.path == "a.txt")
        .unwrap();
    assert_eq!(a.status, FileStatus::Deleted);
    assert!(patch.text.contains("diff --git a/a.txt b/a.txt"));

    // Ticked at "deleted", it collapses like any other reviewed file.
    let reviewed = [ReviewedFile {
        path: "a.txt".into(),
        blob: None,
        commit: resolved.head.clone(),
        at: "2026-11-11T00:00:00Z".into(),
    }];
    let patch = backend
        .patch(
            &resolved,
            &Scope::NewForMe,
            ReviewInput {
                reviewed: &reviewed,
                commented: &[],
            },
            false,
        )
        .unwrap();
    let a = patch
        .files
        .iter()
        .find(|file| file.path == "a.txt")
        .unwrap();
    assert_eq!(a.state, FileState::UnchangedSinceReview);
    assert!(!patch.text.contains("diff --git a/a.txt b/a.txt"));
}

#[test]
fn names_the_project_and_its_worktrees() {
    let fixture = Fixture::new();
    let root = fixture.directory.path();
    let project = root.file_name().unwrap().to_string_lossy().into_owned();
    let backend = fixture.backend();
    assert_eq!(backend.display_names().unwrap(), (project.clone(), None));

    let elsewhere = tempfile::tempdir().unwrap();
    let side = elsewhere.path().join("side");
    git(root, &["worktree", "add", side.to_str().unwrap(), "main"]);
    let linked = GitCli::open(&side).unwrap();
    assert_eq!(
        linked.display_names().unwrap(),
        (project, Some("side".into()))
    );
}

#[test]
fn review_files_live_inside_the_git_directory() {
    let fixture = Fixture::new();
    let backend = fixture.backend();
    let directory = backend.review_dir().unwrap();
    assert!(directory.ends_with(".git/review"));
    assert_eq!(backend.author_name().unwrap(), "Ada");
    assert_eq!(backend.blob_id(&fixture.commits[0], "b.txt").unwrap(), None);
}

#[test]
fn lists_every_blob_in_one_pass() {
    let fixture = Fixture::new();
    let backend = fixture.backend();
    let head = fixture.commits[4].clone();
    let blobs = backend.blob_ids(&head).unwrap();
    for path in ["a.txt", "b.txt"] {
        assert_eq!(
            blobs.get(path),
            backend.blob_id(&head, path).unwrap().as_ref()
        );
    }
    assert_eq!(blobs.len(), 2);
}

#[test]
fn reviews_the_uncommitted_working_tree() {
    let fixture = Fixture::new();
    let root = fixture.directory.path();
    // Uncommitted state on `feat`: a.txt edited, b.txt deleted, c.txt new
    // and untracked, d.txt ignored.
    std::fs::write(
        root.join("a.txt"),
        lines(&[(2, "TWO"), (5, "FIVE"), (9, "NINE")]),
    )
    .unwrap();
    std::fs::remove_file(root.join("b.txt")).unwrap();
    std::fs::write(root.join("c.txt"), "fresh\n").unwrap();
    std::fs::write(root.join("d.txt"), "noise\n").unwrap();
    std::fs::write(root.join(".gitignore"), ".gitignore\nd.txt\n").unwrap();

    let backend = fixture.backend();
    let comparison = Comparison {
        source: "uncommitted".into(),
        target: "feat".into(),
        mode: CompareMode::BranchAdds,
        uncommitted: true,
        ..Default::default()
    };
    let resolved = backend.resolve(&comparison).unwrap();
    assert_eq!(resolved.base, resolved.head);
    assert!(backend.commits(&resolved).unwrap().is_empty());

    let mut changed = backend.changed_paths(&resolved).unwrap();
    changed.sort();
    assert_eq!(changed, ["a.txt", "b.txt", "c.txt"]);

    let patch = backend
        .patch(&resolved, &Scope::WholeBranch, ReviewInput::EMPTY, false)
        .unwrap();
    assert_eq!(patch.new_side, NewSide::Synthetic);
    let status = |path: &str| {
        patch
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap()
            .status
    };
    assert_eq!(status("a.txt"), FileStatus::Modified);
    assert_eq!(status("b.txt"), FileStatus::Deleted);
    assert_eq!(status("c.txt"), FileStatus::Added);
    assert!(patch.text.contains("+fresh"));

    // The repository itself is untouched: still clean apart from our edits.
    let dirty = git(root, &["status", "--porcelain"]);
    assert!(!dirty.contains("A "), "nothing staged: {dirty}");
}

#[test]
fn the_uncommitted_layer_is_pickable() {
    let fixture = Fixture::new();
    let root = fixture.directory.path();
    // One uncommitted edit on top of the branch: line 5.
    std::fs::write(
        root.join("a.txt"),
        lines(&[(2, "TWO"), (5, "FIVE"), (9, "NINE")]),
    )
    .unwrap();

    let backend = fixture.backend();
    let comparison = Comparison {
        source: "uncommitted".into(),
        target: "main".into(),
        mode: CompareMode::BranchAdds,
        uncommitted: true,
        ..Default::default()
    };
    let resolved = backend.resolve(&comparison).unwrap();
    assert_eq!(backend.uncommitted_paths(&resolved).unwrap(), ["a.txt"]);

    // Every commit plus the layer: the whole worktree diff.
    let all = fixture.commits.clone();
    let patch = backend
        .patch(
            &resolved,
            &Scope::Commits {
                picked: all.clone(),
                uncommitted: true,
            },
            ReviewInput::EMPTY,
            false,
        )
        .unwrap();
    assert_eq!(patch.new_side, NewSide::Synthetic);
    assert!(patch.text.contains("+FIVE") && patch.text.contains("+NINE"));

    // Layer unchecked: the uncommitted edit stays out, the commits stay in.
    let patch = backend
        .patch(
            &resolved,
            &Scope::Commits {
                picked: all,
                uncommitted: false,
            },
            ReviewInput::EMPTY,
            false,
        )
        .unwrap();
    assert!(!patch.text.contains("FIVE") && patch.text.contains("+NINE"));

    // The layer alone: only the uncommitted edit.
    let patch = backend
        .patch(
            &resolved,
            &Scope::Commits {
                picked: Vec::new(),
                uncommitted: true,
            },
            ReviewInput::EMPTY,
            false,
        )
        .unwrap();
    assert_eq!(paths(&patch.files), ["a.txt"]);
    assert!(patch.text.contains("+FIVE") && !patch.text.contains("+NINE"));

    // A committed comparison has no uncommitted layer.
    let committed = backend.resolve(&Fixture::comparison()).unwrap();
    assert!(backend.uncommitted_paths(&committed).unwrap().is_empty());
}

#[test]
fn the_uncommitted_layer_combines_with_skipped_commits() {
    let fixture = Fixture::new();
    let root = fixture.directory.path();
    std::fs::write(
        root.join("a.txt"),
        lines(&[(2, "TWO"), (5, "FIVE"), (9, "NINE")]),
    )
    .unwrap();

    let backend = fixture.backend();
    let comparison = Comparison {
        source: "uncommitted".into(),
        target: "main".into(),
        mode: CompareMode::BranchAdds,
        uncommitted: true,
        ..Default::default()
    };
    let resolved = backend.resolve(&comparison).unwrap();

    // Skip c5 (it rewrote line 9) but keep the uncommitted line-5 edit.
    let patch = backend
        .patch(
            &resolved,
            &Scope::Commits {
                picked: fixture.commits[..4].to_vec(),
                uncommitted: true,
            },
            ReviewInput::EMPTY,
            false,
        )
        .unwrap();
    assert_eq!(patch.new_side, NewSide::Synthetic);
    assert!(patch.text.contains("+FIVE"), "the layer is applied");
    assert!(!patch.text.contains("NINE"), "c5 stays out");
    assert!(
        patch
            .files
            .iter()
            .all(|file| file.state == FileState::Changed)
    );
}

#[test]
fn an_uncombinable_uncommitted_layer_is_reported() {
    let fixture = Fixture::new();
    let root = fixture.directory.path();
    // The uncommitted edit rewrites the line c5 wrote; skipping c5 then
    // leaves the layer nothing to sit on.
    std::fs::write(root.join("a.txt"), lines(&[(2, "TWO"), (9, "NINE edited")])).unwrap();

    let backend = fixture.backend();
    let comparison = Comparison {
        source: "uncommitted".into(),
        target: "main".into(),
        mode: CompareMode::BranchAdds,
        uncommitted: true,
        ..Default::default()
    };
    let resolved = backend.resolve(&comparison).unwrap();

    let patch = backend
        .patch(
            &resolved,
            &Scope::Commits {
                picked: fixture.commits[..4].to_vec(),
                uncommitted: true,
            },
            ReviewInput::EMPTY,
            false,
        )
        .unwrap();
    let a = patch
        .files
        .iter()
        .find(|file| file.path == "a.txt")
        .unwrap();
    assert_eq!(a.state, FileState::UncommittedNotCombined);
    assert!(!patch.text.contains("NINE"), "the layer is left out");
    // Unstaged-modified only: the failed apply wrote nothing to the repository.
    assert_eq!(
        git(root, &["status", "--porcelain", "--", "a.txt"]),
        "M a.txt"
    );
}

#[test]
fn an_uncommitted_tick_holds_the_content_on_disk() {
    let fixture = Fixture::new();
    let root = fixture.directory.path();
    std::fs::write(root.join("c.txt"), "fresh\n").unwrap();
    std::fs::remove_file(root.join("b.txt")).unwrap();

    let backend = fixture.backend();
    let comparison = Comparison {
        source: "uncommitted".into(),
        target: "feat".into(),
        mode: CompareMode::BranchAdds,
        uncommitted: true,
        ..Default::default()
    };
    let resolved = backend.resolve(&comparison).unwrap();

    let ids = backend
        .worktree_blob_ids(&["b.txt".into(), "c.txt".into()])
        .unwrap();
    // The deleted file hashes to nothing; the new one to its disk content.
    assert!(!ids.contains_key("b.txt"));
    let tick = |path: &str| ReviewedFile {
        path: path.into(),
        blob: ids.get(path).cloned(),
        commit: resolved.head.clone(),
        at: "2026-11-11T00:00:00Z".into(),
    };

    let patch = backend
        .patch(
            &resolved,
            &Scope::NewForMe,
            ReviewInput {
                reviewed: &[tick("b.txt"), tick("c.txt")],
                commented: &[],
            },
            false,
        )
        .unwrap();
    for path in ["b.txt", "c.txt"] {
        let file = patch.files.iter().find(|file| file.path == path).unwrap();
        assert_eq!(file.state, FileState::UnchangedSinceReview, "{path}");
    }
    assert!(!patch.text.contains("+fresh"));

    // Edited after the tick: the full file comes back.
    std::fs::write(root.join("c.txt"), "fresher\n").unwrap();
    let patch = backend
        .patch(
            &resolved,
            &Scope::NewForMe,
            ReviewInput {
                reviewed: &[tick("c.txt")],
                commented: &[],
            },
            false,
        )
        .unwrap();
    let c = patch
        .files
        .iter()
        .find(|file| file.path == "c.txt")
        .unwrap();
    assert_eq!(c.state, FileState::Changed);
    assert!(patch.text.contains("+fresher"));
}

#[test]
fn an_option_shaped_revision_fails_as_a_revision_not_as_an_option() {
    let fixture = Fixture::new();
    let backend = fixture.backend();
    // A commit id read from a colleague's review file reaches git as a
    // revision; a value shaped like an option must be refused as a bad
    // revision, never acted on.
    let outfile = fixture.directory.path().join("hijacked");
    let evil = Oid::new(format!("--output={}", outfile.display()));
    assert_eq!(
        backend
            .changed_regions(&evil, &fixture.commits[4], "a.txt")
            .unwrap(),
        None
    );
    assert!(!outfile.exists(), "git obeyed the value as an option");
    // Blame answers "cannot say" instead of handing the value to git, which
    // would obey it as an option even behind --end-of-options.
    assert_eq!(backend.line_origin(&evil, "a.txt", 1).unwrap(), None);
    assert!(backend.blame(&evil, "a.txt").unwrap().is_empty());
    assert!(!outfile.exists(), "git obeyed the value as an option");
}

#[test]
fn a_whitespace_only_change_since_the_tick_stays_collapsed() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    git(root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("w.txt"), "hello\nworld\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "base"]);
    git(root, &["checkout", "-q", "-b", "feat"]);
    std::fs::write(root.join("w.txt"), "hello!\nworld\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "real edit"]);
    let ticked_at = Oid::new(git(root, &["rev-parse", "HEAD"]));
    std::fs::write(root.join("w.txt"), "  hello!\nworld\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "reindent"]);

    let backend = GitCli::open(root).unwrap();
    let resolved = backend.resolve(&Fixture::comparison()).unwrap();
    let reviewed = [ReviewedFile {
        path: "w.txt".into(),
        blob: backend.blob_id(&ticked_at, "w.txt").unwrap(),
        commit: ticked_at,
        at: "2026-11-11T00:00:00Z".into(),
    }];
    let patch = backend
        .patch(
            &resolved,
            &Scope::NewForMe,
            ReviewInput {
                reviewed: &reviewed,
                commented: &[],
            },
            true,
        )
        .unwrap();
    // Hiding whitespace empties the since-tick diff; the file must collapse
    // like an unchanged one, not resurface with the branch's diff counts.
    let w = patch
        .files
        .iter()
        .find(|file| file.path == "w.txt")
        .unwrap();
    assert_eq!(w.state, FileState::UnchangedSinceReview);
    assert!(!patch.text.contains("w.txt"));
}

#[test]
fn every_difference_diffs_from_the_target_tip() {
    let fixture = Fixture::new();
    let root = fixture.directory.path();
    // Newer work on the target after the branch point.
    git(root, &["checkout", "-q", "main"]);
    std::fs::write(root.join("main-only.txt"), "mainline\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "m1 mainline file"]);
    git(root, &["checkout", "-q", "feat"]);

    let backend = fixture.backend();
    let mut comparison = Fixture::comparison();
    comparison.mode = CompareMode::EveryDifference;
    let resolved = backend.resolve(&comparison).unwrap();
    assert_eq!(resolved.base, resolved.target_head);
    let patch = backend
        .patch(&resolved, &Scope::WholeBranch, ReviewInput::EMPTY, false)
        .unwrap();
    // Both tips differ in main's new file too: it reads as a deletion.
    let main_only = patch
        .files
        .iter()
        .find(|file| file.path == "main-only.txt")
        .unwrap();
    assert_eq!(main_only.status, FileStatus::Deleted);
    assert!(patch.text.contains("-mainline"));

    // The default mode ignores the target's newer work.
    let adds = backend.resolve(&Fixture::comparison()).unwrap();
    assert_ne!(adds.base, adds.target_head);
    let patch = backend
        .patch(&adds, &Scope::WholeBranch, ReviewInput::EMPTY, false)
        .unwrap();
    assert!(!patch.text.contains("main-only"));
}

#[test]
fn unrelated_histories_report_no_merge_base() {
    let fixture = Fixture::new();
    let root = fixture.directory.path();
    // A second root commit: no history shared with feat.
    git(root, &["checkout", "-q", "--orphan", "isolated"]);
    git(root, &["commit", "-q", "-m", "other root"]);
    git(root, &["checkout", "-q", "feat"]);

    let backend = fixture.backend();
    let comparison = Comparison {
        source: "feat".into(),
        target: "isolated".into(),
        mode: CompareMode::BranchAdds,
        uncommitted: false,
        ..Default::default()
    };
    match backend.resolve(&comparison) {
        Err(BackendError::NoMergeBase { source_rev, target }) => {
            assert_eq!((source_rev.as_str(), target.as_str()), ("feat", "isolated"));
        }
        other => panic!("expected NoMergeBase, got {other:?}"),
    }
}

#[test]
fn commits_touching_names_a_renamed_file_as_each_commit_did() {
    let fixture = Fixture::new();
    let root = fixture.directory.path();
    git(root, &["mv", "b.txt", "renamed.txt"]);
    git(root, &["commit", "-qm", "c6 rename b"]);
    let rename = Oid::new(git(root, &["rev-parse", "HEAD"]));

    let backend = fixture.backend();
    let resolved = backend.resolve(&Fixture::comparison()).unwrap();
    let touched = backend.commits_touching(&resolved).unwrap();
    // The commit before the rename used the old name; the rename commit the
    // new one, next to the name changed_paths reports.
    assert_eq!(touched["b.txt"], [fixture.commits[1].clone()]);
    assert_eq!(touched["renamed.txt"], [rename]);
}

#[test]
fn open_comments_keep_a_ticked_file_in_new_for_me() {
    let fixture = Fixture::new();
    let backend = fixture.backend();
    let resolved = backend.resolve(&Fixture::comparison()).unwrap();
    let tick = ReviewedFile {
        path: "a.txt".into(),
        blob: backend.blob_id(&resolved.head, "a.txt").unwrap(),
        commit: resolved.head.clone(),
        at: "2026-11-11T00:00:00Z".into(),
    };

    // Ticked at head and quiet: the file collapses out of New for me, and
    // its comments would have no rows to sit on.
    let patch = backend
        .patch(
            &resolved,
            &Scope::NewForMe,
            ReviewInput {
                reviewed: std::slice::from_ref(&tick),
                commented: &[],
            },
            false,
        )
        .unwrap();
    let a = patch
        .files
        .iter()
        .find(|file| file.path == "a.txt")
        .unwrap();
    assert_eq!(a.state, FileState::UnchangedSinceReview);
    assert!(!patch.text.contains("a.txt"));

    // The same tick with an open comment: the tick does not end the work,
    // so the file keeps its branch diff for the comment to sit on.
    let patch = backend
        .patch(
            &resolved,
            &Scope::NewForMe,
            ReviewInput {
                reviewed: &[tick],
                commented: &["a.txt".into()],
            },
            false,
        )
        .unwrap();
    let a = patch
        .files
        .iter()
        .find(|file| file.path == "a.txt")
        .unwrap();
    assert_eq!(a.state, FileState::KeptForComments);
    assert!(patch.text.contains("+TWO"));
}

#[test]
fn one_revision_reads_like_git_diff() {
    // `diffie HEAD~1` must show the working tree against that revision -
    // the commits since it and the uncommitted layer together, exactly
    // what `git diff HEAD~1` prints.
    let fixture = Fixture::new();
    let root = fixture.directory.path();
    // Uncommitted edit on line 5 on top of c5.
    std::fs::write(
        root.join("a.txt"),
        lines(&[(2, "TWO"), (5, "FIVE"), (9, "NINE")]),
    )
    .unwrap();

    let backend = fixture.backend();
    let comparison = Comparison {
        source: "working tree".into(),
        target: "HEAD~1".into(),
        mode: CompareMode::BranchAdds,
        uncommitted: true,
        ..Default::default()
    };
    let resolved = backend.resolve(&comparison).unwrap();
    // HEAD~1 is an ancestor, so the merge-base rule changes nothing.
    assert_eq!(resolved.base, resolved.target_head);
    assert_eq!(resolved.base, fixture.commits[3]);

    // The commit between the revision and HEAD stays pickable.
    let commits = backend.commits(&resolved).unwrap();
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0].subject, "c5 edit line 9 again");

    let patch = backend
        .patch(&resolved, &Scope::NewForMe, ReviewInput::EMPTY, false)
        .unwrap();
    assert_eq!(paths(&patch.files), ["a.txt"]);
    assert!(
        patch.text.contains("+FIVE") && patch.text.contains("+NINE"),
        "the committed run and the uncommitted layer both show"
    );
}

#[test]
fn blobs_reads_many_objects_in_one_pass() {
    let fixture = Fixture::new();
    let backend = fixture.backend();
    let tree = backend.blob_ids(&fixture.commits[4]).unwrap();
    let a = tree.get("a.txt").unwrap().clone();
    let b = tree.get("b.txt").unwrap().clone();
    // An id git does not hold is left out, never an error.
    let absent = Oid::new("0123456789012345678901234567890123456789");

    let blobs = backend
        .blobs(&[a.clone(), absent.clone(), b.clone()])
        .unwrap();

    assert_eq!(blobs.len(), 2);
    assert_eq!(
        blobs.get(&a).map(Vec::as_slice),
        Some(lines(&[(2, "TWO"), (9, "NINE")]).as_bytes())
    );
    assert_eq!(blobs.get(&b).map(Vec::as_slice), Some(&b"hello\n"[..]));
    assert!(backend.blobs(&[]).unwrap().is_empty());
}

#[test]
fn blobs_keeps_bytes_that_are_not_text() {
    let fixture = Fixture::new();
    let root = fixture.directory.path();
    // Invalid UTF-8 ahead of a second object: a lossy read would shift
    // every later object's byte offsets.
    let raw: &[u8] = &[0xff, 0xfe, b'\n', 0x00, 0xc3];
    std::fs::write(root.join("raw.bin"), raw).unwrap();
    git(root, &["add", "raw.bin"]);
    git(root, &["commit", "-q", "-m", "raw"]);
    let backend = fixture.backend();
    let head = Oid::new(git(root, &["rev-parse", "HEAD"]));
    let tree = backend.blob_ids(&head).unwrap();
    let raw_id = tree.get("raw.bin").unwrap().clone();
    let b = tree.get("b.txt").unwrap().clone();

    let blobs = backend.blobs(&[raw_id.clone(), b.clone()]).unwrap();

    assert_eq!(blobs.get(&raw_id).map(Vec::as_slice), Some(raw));
    assert_eq!(blobs.get(&b).map(Vec::as_slice), Some(&b"hello\n"[..]));
}

#[test]
fn worktree_file_reads_what_is_on_disk() {
    let fixture = Fixture::new();
    let root = fixture.directory.path();
    std::fs::write(root.join("a.txt"), "not committed\n").unwrap();
    let backend = fixture.backend();

    assert_eq!(
        backend.worktree_file("a.txt").unwrap().as_deref(),
        Some(&b"not committed\n"[..])
    );
    assert_eq!(backend.worktree_file("missing.txt").unwrap(), None);
}

#[test]
fn default_branch_is_what_the_remotes_head_records() {
    // A clone records the remote's default branch; a repository without
    // remotes has nobody to say.
    let (_directory, local, _head, _base) = clone_behind_upstream();
    let backend = GitCli::open(&local).unwrap();
    assert_eq!(backend.default_branch().unwrap().as_deref(), Some("main"));

    let fixture = Fixture::new();
    assert_eq!(fixture.backend().default_branch().unwrap(), None);
}

#[test]
fn has_uncommitted_changes_counts_edits_and_untracked_files() {
    let fixture = Fixture::new();
    let root = fixture.directory.path();
    let backend = fixture.backend();
    assert!(!backend.has_uncommitted_changes().unwrap());

    std::fs::write(root.join("a.txt"), "edited\n").unwrap();
    assert!(backend.has_uncommitted_changes().unwrap());
    git(root, &["checkout", "--", "a.txt"]);
    assert!(!backend.has_uncommitted_changes().unwrap());

    std::fs::write(root.join("fresh.txt"), "untracked\n").unwrap();
    assert!(backend.has_uncommitted_changes().unwrap());
}

/// Upstream with a pull request's shape: `topic` going into `main`, the
/// head also reachable as `refs/pull/7/head` like on GitHub, and a clone
/// made before the pull request's commits existed.
fn clone_behind_upstream() -> (tempfile::TempDir, std::path::PathBuf, Oid, Oid) {
    let directory = tempfile::tempdir().unwrap();
    let upstream = directory.path().join("upstream");
    std::fs::create_dir(&upstream).unwrap();
    git(&upstream, &["init", "-q", "-b", "main"]);
    std::fs::write(upstream.join("a.txt"), "base\n").unwrap();
    git(&upstream, &["add", "."]);
    git(&upstream, &["commit", "-q", "-m", "base"]);
    let local = directory.path().join("local");
    git(
        directory.path(),
        &["clone", "-q", "upstream", local.to_str().unwrap()],
    );
    // The pull request lands upstream only after the clone.
    git(&upstream, &["checkout", "-q", "-b", "topic"]);
    std::fs::write(upstream.join("a.txt"), "changed\n").unwrap();
    git(&upstream, &["add", "."]);
    git(&upstream, &["commit", "-q", "-m", "pr commit"]);
    let head = Oid::new(git(&upstream, &["rev-parse", "HEAD"]));
    let base = Oid::new(git(&upstream, &["rev-parse", "main"]));
    git(
        &upstream,
        &["update-ref", "refs/pull/7/head", head.as_str()],
    );
    git(&upstream, &["checkout", "-q", "main"]);
    (directory, local, head, base)
}

#[test]
fn fetch_brings_missing_commits_and_leaves_the_checkout_alone() {
    let (_directory, local, head, _base) = clone_behind_upstream();
    let backend = GitCli::open(&local).unwrap();
    assert!(!backend.revision_exists(head.as_str()).unwrap());
    let before = git(&local, &["rev-parse", "HEAD"]);

    backend
        .fetch(
            "origin",
            &["main".to_string(), "refs/pull/7/head".to_string()],
        )
        .unwrap();

    assert!(backend.revision_exists(head.as_str()).unwrap());
    // Only objects and remote-tracking refs: no new local branch, HEAD and
    // the working tree as they were.
    assert_eq!(git(&local, &["rev-parse", "HEAD"]), before);
    assert_eq!(
        git(&local, &["branch", "--format=%(refname:short)"]),
        "main"
    );
    assert_eq!(git(&local, &["status", "--porcelain"]), "");
}

struct PullForge(diffie_core::pr::PullRequest);

impl diffie_core::forge::Forge for PullForge {
    fn name(&self) -> &'static str {
        "Fake"
    }

    fn pull_request(
        &self,
        _: &diffie_core::pr::PrArgument,
    ) -> Result<diffie_core::pr::PullRequest, diffie_core::forge::ForgeError> {
        Ok(self.0.clone())
    }

    fn pull_requests(
        &self,
        _: diffie_core::pr::PrList,
    ) -> Result<Vec<diffie_core::pr::PrSummary>, diffie_core::forge::ForgeError> {
        unreachable!("opening never lists")
    }

    fn status(
        &self,
        _: &diffie_core::pr::PullRequest,
    ) -> Result<diffie_core::pr::PullStatus, diffie_core::forge::ForgeError> {
        unreachable!("opening never asks for status")
    }

    fn submit_review(
        &self,
        _: &diffie_core::pr::PullRequest,
        _: &diffie_core::forge::ReviewDraft,
    ) -> Result<Vec<diffie_core::forge::Posting>, diffie_core::forge::ForgeError> {
        unreachable!("opening never publishes")
    }

    fn update_comment(
        &self,
        _: &diffie_core::pr::PullRequest,
        _: &str,
        _: &str,
    ) -> Result<(), diffie_core::forge::ForgeError> {
        unreachable!("opening never edits")
    }

    fn react(
        &self,
        _: &diffie_core::pr::PullRequest,
        _: &str,
        _: diffie_core::review::ReactionKind,
    ) -> Result<(), diffie_core::forge::ForgeError> {
        unreachable!("opening never reacts")
    }

    fn stack(&self, _: &PrArgument) -> Result<Option<Stack>, diffie_core::forge::ForgeError> {
        unreachable!("opening one pull request never reads a stack")
    }

    fn stack_numbered(&self, _: u64) -> Result<Option<Stack>, diffie_core::forge::ForgeError> {
        unreachable!("opening one pull request never reads a stack")
    }

    fn stacks(&self) -> Result<Vec<StackSummary>, diffie_core::forge::ForgeError> {
        unreachable!("opening never lists")
    }
}

fn pull_request(head: &Oid, base: &Oid) -> diffie_core::pr::PullRequest {
    diffie_core::pr::PullRequest {
        number: 7,
        id: "PR_7".into(),
        url: "https://github.com/o/r/pull/7".into(),
        state: diffie_core::pr::PrState::Open,
        source_branch: "topic".into(),
        source_commit: head.to_string(),
        target_branch: "main".into(),
        target_commit: base.to_string(),
        cross_repository: false,
        head_owner: "o".into(),
    }
}

#[test]
fn opening_a_pull_request_fetches_missing_commits_on_demand() {
    let (_directory, local, head, base) = clone_behind_upstream();
    let backend = GitCli::open(&local).unwrap();
    let forge = PullForge(pull_request(&head, &base));
    let argument = diffie_core::pr::PrArgument {
        number: 7,
        url: None,
    };

    let (pull, comparison) = diffie_core::pr::open(
        &backend,
        &forge,
        &argument,
        CompareMode::BranchAdds,
        diffie_core::pr::FetchMode::OnDemand,
    )
    .unwrap();

    assert_eq!(pull.number, 7);
    assert_eq!(comparison.source_rev.as_deref(), Some(head.as_str()));
    assert_eq!(comparison.target_rev.as_deref(), Some("origin/main"));
    assert!(backend.revision_exists(head.as_str()).unwrap());
}

#[test]
fn opening_without_fetching_names_the_fetch_to_run() {
    let (_directory, local, head, base) = clone_behind_upstream();
    let backend = GitCli::open(&local).unwrap();
    let forge = PullForge(pull_request(&head, &base));
    let argument = diffie_core::pr::PrArgument {
        number: 7,
        url: None,
    };

    let error = diffie_core::pr::open(
        &backend,
        &forge,
        &argument,
        CompareMode::BranchAdds,
        diffie_core::pr::FetchMode::Never,
    )
    .unwrap_err();

    assert_eq!(
        error,
        "the pull request's head commit is not here; run \
         `git fetch origin topic` and retry - diffie never fetches."
    );
    assert!(!backend.revision_exists(head.as_str()).unwrap());
}

/// One member of a stack fixture. Unlike the ownership helper's, its
/// recorded base commit matters: that is what a merged member pins the
/// comparison's target to.
fn stack_member(number: u64, head: &Oid, source: &str, target: &str, base: &Oid) -> PullRequest {
    PullRequest {
        number,
        id: format!("PR_{number}"),
        url: format!("https://github.com/o/r/pull/{number}"),
        state: PrState::Open,
        source_branch: source.into(),
        source_commit: head.to_string(),
        target_branch: target.into(),
        target_commit: base.to_string(),
        cross_repository: false,
        head_owner: "o".into(),
    }
}

/// A forge that knows one stack. `stack` answers it for any member's
/// number, `Ok(None)` for `unstacked`, and refuses every other number the
/// way GitHub refuses one that names no pull request - the stack's own
/// included, which only `stack_numbered` answers for.
///
/// `stacks` reports nothing: every member sits past the list's page, the
/// case a lookup by stack number must survive.
#[derive(Default)]
struct StackForge {
    /// Latest first, as the forge reads a stack's entries in.
    members: Vec<PullRequest>,
    number: Option<u64>,
    unstacked: Option<u64>,
    calls: std::sync::Mutex<Vec<String>>,
}

impl StackForge {
    fn new(members: Vec<PullRequest>) -> Self {
        Self {
            members,
            number: Some(42),
            ..Default::default()
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl diffie_core::forge::Forge for StackForge {
    fn name(&self) -> &'static str {
        "Fake"
    }

    fn pull_request(&self, _: &PrArgument) -> Result<PullRequest, diffie_core::forge::ForgeError> {
        unreachable!("a stack never opens one pull request")
    }

    fn pull_requests(
        &self,
        _: diffie_core::pr::PrList,
    ) -> Result<Vec<PrSummary>, diffie_core::forge::ForgeError> {
        unreachable!("a stack is listed through stacks()")
    }

    fn status(
        &self,
        _: &PullRequest,
    ) -> Result<diffie_core::pr::PullStatus, diffie_core::forge::ForgeError> {
        unreachable!("opening never asks for status")
    }

    fn submit_review(
        &self,
        _: &PullRequest,
        _: &diffie_core::forge::ReviewDraft,
    ) -> Result<Vec<diffie_core::forge::Posting>, diffie_core::forge::ForgeError> {
        unreachable!("opening never publishes")
    }

    fn update_comment(
        &self,
        _: &PullRequest,
        _: &str,
        _: &str,
    ) -> Result<(), diffie_core::forge::ForgeError> {
        unreachable!("opening never edits")
    }

    fn react(
        &self,
        _: &PullRequest,
        _: &str,
        _: diffie_core::review::ReactionKind,
    ) -> Result<(), diffie_core::forge::ForgeError> {
        unreachable!("opening never reacts")
    }

    fn stack(
        &self,
        argument: &PrArgument,
    ) -> Result<Option<Stack>, diffie_core::forge::ForgeError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("stack {}", argument.number));
        if self.unstacked == Some(argument.number) {
            return Ok(None);
        }
        if !self
            .members
            .iter()
            .any(|member| member.number == argument.number)
        {
            return Err(diffie_core::forge::ForgeError::Refused(format!(
                "Could not resolve to a PullRequest with the number of {}.",
                argument.number
            )));
        }
        Ok(Some(Stack {
            number: self.number,
            pulls: self.members.clone(),
        }))
    }

    fn stack_numbered(&self, number: u64) -> Result<Option<Stack>, diffie_core::forge::ForgeError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("stack_numbered {number}"));
        if self.number != Some(number) {
            return Ok(None);
        }
        Ok(Some(Stack {
            number: self.number,
            pulls: self.members.clone(),
        }))
    }

    fn stacks(&self) -> Result<Vec<StackSummary>, diffie_core::forge::ForgeError> {
        self.calls.lock().unwrap().push("stacks".to_string());
        Ok(Vec::new())
    }
}

/// A stack of three members over one throwaway repository: `feat/c` going
/// into `feat/b` into `feat/a` into `main`. `main` has moved on since the
/// stack was branched and `o/main` carries that newer tip, so a test can
/// tell the remote-tracking ref apart from a member's recorded base.
struct StackFixture {
    directory: tempfile::TempDir,
    /// Latest first, like the members.
    heads: Vec<Oid>,
    /// The commit `feat/a` branched from and records as its base.
    base: Oid,
}

impl StackFixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        git(root, &["init", "-q", "-b", "main"]);
        let commit = |name: &str| {
            std::fs::write(root.join(name), format!("{name}\n")).unwrap();
            git(root, &["add", "."]);
            git(root, &["commit", "-q", "-m", name]);
            Oid::new(git(root, &["rev-parse", "HEAD"]))
        };
        let base = commit("base");
        let mut heads = Vec::new();
        for (branch, file) in [("feat/a", "a"), ("feat/b", "b"), ("feat/c", "c")] {
            git(root, &["checkout", "-q", "-b", branch]);
            heads.push(commit(file));
        }
        heads.reverse();
        git(root, &["checkout", "-q", "main"]);
        commit("moved on");
        git(root, &["remote", "add", "o", "."]);
        git(root, &["update-ref", "refs/remotes/o/main", "main"]);
        Self {
            directory,
            heads,
            base,
        }
    }

    fn backend(&self) -> GitCli {
        GitCli::open(self.directory.path()).unwrap()
    }

    /// Latest first, as the forge reads a stack's entries in.
    fn members(&self) -> Vec<PullRequest> {
        vec![
            stack_member(3, &self.heads[0], "feat/c", "feat/b", &self.heads[1]),
            stack_member(2, &self.heads[1], "feat/b", "feat/a", &self.heads[2]),
            stack_member(1, &self.heads[2], "feat/a", "main", &self.base),
        ]
    }
}

fn numbered(number: u64) -> PrArgument {
    PrArgument { number, url: None }
}

#[test]
fn a_stack_reviews_the_latest_member_into_the_first_members_base() {
    let fixture = StackFixture::new();
    let forge = StackForge::new(fixture.members());
    let (stack, comparison) = open_stack(
        &fixture.backend(),
        &forge,
        &numbered(2),
        CompareMode::BranchAdds,
        FetchMode::Never,
    )
    .unwrap();

    assert_eq!(stack.number, Some(42));
    assert_eq!(stack.pulls.first().unwrap().number, 3);
    assert_eq!(comparison.source, "feat/c");
    assert_eq!(
        comparison.source_rev.as_deref(),
        Some(fixture.heads[0].as_str())
    );
    // The first member is open, so its base follows the remote-tracking
    // ref, which has moved past the commit the stack branched from.
    assert_eq!(comparison.target, "main");
    assert_eq!(comparison.target_rev.as_deref(), Some("o/main"));
    // A member's number is a pull request, so the list is never asked.
    assert_eq!(forge.calls(), ["stack 2"]);
}

#[test]
fn a_merged_first_member_pins_the_base_commit_it_recorded() {
    let fixture = StackFixture::new();
    let mut members = fixture.members();
    members[2].state = PrState::Merged;
    let forge = StackForge::new(members);
    let (_, comparison) = open_stack(
        &fixture.backend(),
        &forge,
        &numbered(3),
        CompareMode::BranchAdds,
        FetchMode::Never,
    )
    .unwrap();

    // Not `o/main`, which already carries what the member merged.
    assert_eq!(comparison.target, "main");
    assert_eq!(
        comparison.target_rev.as_deref(),
        Some(fixture.base.as_str())
    );
}

#[test]
fn a_stack_number_resolves_with_its_members_past_the_list() {
    let fixture = StackFixture::new();
    let forge = StackForge::new(fixture.members());
    let (stack, comparison) = open_stack(
        &fixture.backend(),
        &forge,
        &numbered(42),
        CompareMode::BranchAdds,
        FetchMode::Never,
    )
    .unwrap();

    assert_eq!(stack.number, Some(42));
    assert_eq!(comparison.source, "feat/c");
    // 42 is the stack's own number and so no pull request. It is looked
    // up directly: the list reports nothing here, as it would for a stack
    // whose members all sit past its page, and that must not lose it.
    assert_eq!(forge.calls(), ["stack 42", "stack_numbered 42"]);
}

#[test]
fn a_pull_request_in_no_stack_names_the_pr_flag() {
    let fixture = StackFixture::new();
    let forge = StackForge {
        unstacked: Some(9),
        ..StackForge::new(fixture.members())
    };
    let error = open_stack(
        &fixture.backend(),
        &forge,
        &numbered(9),
        CompareMode::BranchAdds,
        FetchMode::Never,
    )
    .unwrap_err();

    assert!(error.contains("not in a stack"), "{error}");
    assert!(error.contains("--pr 9"), "{error}");
    // A pull request's answer is final: no stack carries its number.
    assert_eq!(forge.calls(), ["stack 9"]);
}

#[test]
fn a_number_that_names_neither_names_the_pr_flag() {
    let fixture = StackFixture::new();
    let forge = StackForge::new(fixture.members());
    let error = open_stack(
        &fixture.backend(),
        &forge,
        &numbered(77),
        CompareMode::BranchAdds,
        FetchMode::Never,
    )
    .unwrap_err();

    assert!(
        error.contains("neither a pull request nor a stack"),
        "{error}"
    );
    assert!(error.contains("`--pr`"), "{error}");
    assert_eq!(forge.calls(), ["stack 77", "stack_numbered 77"]);
}

/// Upstream with a three-member stack, each head also reachable as
/// `refs/pull/<n>/head` like on GitHub, and a clone made before any of
/// the stack's commits existed.
fn clone_behind_stack() -> (tempfile::TempDir, std::path::PathBuf, Vec<PullRequest>) {
    let directory = tempfile::tempdir().unwrap();
    let upstream = directory.path().join("upstream");
    std::fs::create_dir(&upstream).unwrap();
    git(&upstream, &["init", "-q", "-b", "main"]);
    std::fs::write(upstream.join("a.txt"), "base\n").unwrap();
    git(&upstream, &["add", "."]);
    git(&upstream, &["commit", "-q", "-m", "base"]);
    let base = Oid::new(git(&upstream, &["rev-parse", "HEAD"]));
    let local = directory.path().join("local");
    git(
        directory.path(),
        &["clone", "-q", "upstream", local.to_str().unwrap()],
    );
    // The stack lands upstream only after the clone.
    let mut members = Vec::new();
    let mut target = "main".to_string();
    for (number, branch) in [(1, "feat/a"), (2, "feat/b"), (3, "feat/c")] {
        git(&upstream, &["checkout", "-q", "-b", branch, &target]);
        std::fs::write(upstream.join(format!("{number}.txt")), "x\n").unwrap();
        git(&upstream, &["add", "."]);
        git(&upstream, &["commit", "-q", "-m", branch]);
        let head = Oid::new(git(&upstream, &["rev-parse", "HEAD"]));
        git(
            &upstream,
            &[
                "update-ref",
                &format!("refs/pull/{number}/head"),
                head.as_str(),
            ],
        );
        members.push(stack_member(number, &head, branch, &target, &base));
        target = branch.to_string();
    }
    git(&upstream, &["checkout", "-q", "main"]);
    members.reverse();
    (directory, local, members)
}

#[test]
fn opening_a_stack_fetches_every_member_in_one_call() {
    let (_directory, local, members) = clone_behind_stack();
    let backend = GitCli::open(&local).unwrap();
    let forge = StackForge::new(members.clone());
    for member in &members {
        assert!(!backend.revision_exists(&member.source_commit).unwrap());
    }

    let (_, comparison) = open_stack(
        &backend,
        &forge,
        &numbered(3),
        CompareMode::BranchAdds,
        FetchMode::OnDemand,
    )
    .unwrap();

    assert_eq!(comparison.target_rev.as_deref(), Some("origin/main"));
    // `git fetch` rewrites FETCH_HEAD, so every member standing in it at
    // once is what proves the fetch was batched into one call.
    let fetched = std::fs::read_to_string(local.join(".git/FETCH_HEAD")).unwrap();
    assert_eq!(fetched.lines().count(), members.len() + 1, "{fetched}");
    for member in &members {
        assert!(
            backend.revision_exists(&member.source_commit).unwrap(),
            "#{} was not fetched",
            member.number
        );
        assert!(fetched.contains(member.source_commit.as_str()), "{fetched}");
    }
    // Only objects and remote-tracking refs.
    assert_eq!(
        git(&local, &["branch", "--format=%(refname:short)"]),
        "main"
    );
    assert_eq!(git(&local, &["status", "--porcelain"]), "");
}

/// Like [`git`], but for a merge or rebase that is expected to stop on a
/// conflict: git exits non-zero, and that interrupted state is the fixture.
fn git_conflicting(directory: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Ada")
        .env("GIT_AUTHOR_EMAIL", "ada@example.invalid")
        .env("GIT_COMMITTER_NAME", "Ada")
        .env("GIT_COMMITTER_EMAIL", "ada@example.invalid")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git runs");
    assert!(
        !output.status.success(),
        "git {args:?} should stop on a conflict"
    );
}

/// `feat` and `main` both rewrite the one line of `t.txt`, so merging or
/// rebasing one into the other always conflicts there; `calm.txt` only
/// changes on `feat` and merges cleanly.
struct ConflictFixture {
    directory: tempfile::TempDir,
}

impl ConflictFixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        git(root, &["init", "-q", "-b", "main"]);
        git(root, &["config", "user.name", "Ada"]);
        // The identity lives in the repo config, not just the test
        // helper's env: continue commits through the backend, which
        // inherits no env, and Windows' git refuses to guess an email.
        git(root, &["config", "user.email", "ada@example.invalid"]);
        git(root, &["config", "core.autocrlf", "false"]);
        // diff3 markers carry the base section the resolver needs. Set on
        // the throwaway repository; the app never touches a repo's config.
        git(root, &["config", "merge.conflictStyle", "diff3"]);
        // Commits detach a background auto-maintenance that briefly holds
        // objects/maintenance.lock: a race against the no-write guard's
        // object snapshots, and noise in a throwaway repository.
        git(root, &["config", "maintenance.auto", "false"]);
        git(root, &["config", "gc.auto", "0"]);
        // Fixed commit instants so the label tests can assert the tips'
        // clocks: feat's tip is four days before main's, feat's first
        // commit five days before.
        std::fs::write(root.join("t.txt"), "base line\n").unwrap();
        std::fs::write(root.join("calm.txt"), "calm\n").unwrap();
        git(root, &["add", "."]);
        git_at(
            root,
            "2026-11-01T10:00:00+01:00",
            &["commit", "-q", "-m", "base"],
        );
        git(root, &["checkout", "-q", "-b", "feat"]);
        std::fs::write(root.join("t.txt"), "feat line\n").unwrap();
        git_at(
            root,
            "2026-11-03T10:00:00+01:00",
            &["commit", "-q", "-am", "feat rewrites t"],
        );
        std::fs::write(root.join("calm.txt"), "calm feat\n").unwrap();
        git_at(
            root,
            "2026-11-04T10:00:00+01:00",
            &["commit", "-q", "-am", "feat edits calm"],
        );
        git(root, &["checkout", "-q", "main"]);
        std::fs::write(root.join("t.txt"), "main line\n").unwrap();
        git_at(
            root,
            "2026-11-08T10:00:00+01:00",
            &["commit", "-q", "-am", "main rewrites t"],
        );
        Self { directory }
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    fn backend(&self) -> GitCli {
        GitCli::open(self.root()).unwrap()
    }

    /// Interrupt a merge of `feat` into `main`, conflicted on `t.txt`.
    fn start_merge(&self) {
        git_conflicting(self.root(), &["merge", "feat"]);
    }

    /// Interrupt a rebase of `feat` onto `main`: it stops at the first of
    /// feat's two commits.
    fn start_rebase(&self) {
        git(self.root(), &["checkout", "-q", "feat"]);
        git_conflicting(self.root(), &["rebase", "main"]);
    }
}

#[test]
fn repo_state_tells_a_merge_from_idle_per_worktree() {
    let fixture = ConflictFixture::new();
    assert_eq!(fixture.backend().repo_state().unwrap(), RepoState::Idle);

    // A linked worktree on another branch stays idle while the main
    // checkout merges: the state is read from the worktree's own git dir.
    let aside = fixture.root().join("aside");
    git(
        fixture.root(),
        &["worktree", "add", "-q", aside.to_str().unwrap(), "feat"],
    );
    fixture.start_merge();
    assert_eq!(fixture.backend().repo_state().unwrap(), RepoState::Merge);
    assert_eq!(
        GitCli::open(&aside).unwrap().repo_state().unwrap(),
        RepoState::Idle
    );
}

#[test]
fn repo_state_counts_rebase_progress() {
    let fixture = ConflictFixture::new();
    fixture.start_rebase();
    assert_eq!(
        fixture.backend().repo_state().unwrap(),
        RepoState::Rebase { done: 1, total: 2 }
    );
}

#[test]
fn predicted_conflicts_see_the_collision_before_any_merge_starts() {
    let fixture = ConflictFixture::new();
    let objects_before = file_set(&fixture.root().join(".git/objects"));

    let predicted = fixture
        .backend()
        .predicted_conflicts("feat", None, "main", None)
        .unwrap();

    assert_eq!(predicted.len(), 1);
    assert_eq!(predicted[0].path, "t.txt");
    let merged = predicted[0].merged.as_deref().unwrap();
    assert!(merged.contains("<<<<<<< feat"), "{merged}");
    assert!(merged.contains("feat line"), "{merged}");
    assert!(merged.contains("main line"), "{merged}");
    assert!(merged.contains(">>>>>>> main"), "{merged}");
    // Prediction never starts the operation or writes merge objects into
    // the repository: everything lands in the scratch directory.
    assert_eq!(fixture.backend().repo_state().unwrap(), RepoState::Idle);
    assert_eq!(
        file_set(&fixture.root().join(".git/objects")),
        objects_before
    );
}

#[test]
fn predicted_conflicts_stay_empty_for_branches_that_merge_cleanly() {
    let fixture = ConflictFixture::new();
    // Branched at the fixture's base commit so it shares no rewrite with
    // feat: the two touch disjoint paths and the merge is clean.
    git(fixture.root(), &["checkout", "-q", "-b", "docs", "feat~2"]);
    std::fs::write(fixture.root().join("d.txt"), "docs\n").unwrap();
    git(fixture.root(), &["add", "d.txt"]);
    git(fixture.root(), &["commit", "-q", "-m", "docs add d"]);
    git(fixture.root(), &["checkout", "-q", "main"]);

    let predicted = fixture
        .backend()
        .predicted_conflicts("docs", None, "feat", None)
        .unwrap();
    assert_eq!(predicted, Vec::new());
}

#[test]
fn predicted_conflicts_follow_the_pin_once_a_name_has_moved() {
    let fixture = ConflictFixture::new();
    let pinned = git(fixture.root(), &["rev-parse", "main"]);

    // While the name and the pin agree, the name labels the markers.
    let predicted = fixture
        .backend()
        .predicted_conflicts("feat", None, "main", Some(&pinned))
        .unwrap();
    let merged = predicted[0].merged.as_deref().unwrap();
    assert!(merged.contains(">>>>>>> main"), "{merged}");

    // Once main moves on, the review stays pinned: the forecast merges
    // the pinned tip's content and the markers carry the pin.
    git(fixture.root(), &["checkout", "-q", "main"]);
    std::fs::write(fixture.root().join("t.txt"), "main line moved on\n").unwrap();
    git(
        fixture.root(),
        &["commit", "-q", "-am", "main rewrites t again"],
    );
    let predicted = fixture
        .backend()
        .predicted_conflicts("feat", None, "main", Some(&pinned))
        .unwrap();
    let merged = predicted[0].merged.as_deref().unwrap();
    assert!(merged.contains("main line\n"), "{merged}");
    assert!(!merged.contains("main line moved on"), "{merged}");
    assert!(merged.contains(&format!(">>>>>>> {pinned}")), "{merged}");
}

#[test]
fn predicted_conflicts_keep_a_path_git_would_quote() {
    let fixture = ConflictFixture::new();
    // Added differently on both sides: a two-way conflict on a name that
    // a C-quoting listing would mangle under LC_ALL=C.
    git(fixture.root(), &["checkout", "-q", "feat"]);
    std::fs::write(fixture.root().join("grüße.txt"), "feat greets\n").unwrap();
    git(fixture.root(), &["add", "grüße.txt"]);
    git(
        fixture.root(),
        &["commit", "-q", "-m", "feat adds greetings"],
    );
    git(fixture.root(), &["checkout", "-q", "main"]);
    std::fs::write(fixture.root().join("grüße.txt"), "main greets\n").unwrap();
    git(fixture.root(), &["add", "grüße.txt"]);
    git(
        fixture.root(),
        &["commit", "-q", "-m", "main adds greetings"],
    );

    let predicted = fixture
        .backend()
        .predicted_conflicts("feat", None, "main", None)
        .unwrap();
    let greeting = predicted
        .iter()
        .find(|conflict| conflict.path == "grüße.txt")
        .expect("the quoted-name path is listed verbatim");
    let merged = greeting.merged.as_deref().unwrap();
    assert!(merged.contains("feat greets"), "{merged}");
    assert!(merged.contains("main greets"), "{merged}");
}

/// Every file under `directory`, relative, sorted: an assertion on the
/// set names the file that appeared or vanished, where a count only says
/// that one did.
fn file_set(directory: &Path) -> Vec<String> {
    fn walk(root: &Path, directory: &Path, into: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, into);
            } else if let Ok(relative) = path.strip_prefix(root) {
                into.push(relative.display().to_string());
            }
        }
    }
    let mut files = Vec::new();
    walk(directory, directory, &mut files);
    files.sort();
    files
}

/// A bare repository beside the fixture, wired up as remote `o` with the
/// fixture's branches pushed, so a resolution push has somewhere real to
/// land without the network.
fn bare_remote(fixture: &ConflictFixture) -> tempfile::TempDir {
    let remote = tempfile::tempdir().unwrap();
    let url = remote.path().to_str().unwrap().to_string();
    git(fixture.root(), &["init", "-q", "--bare", &url]);
    git(fixture.root(), &["remote", "add", "o", &url]);
    git(fixture.root(), &["push", "-q", "o", "main", "feat"]);
    remote
}

#[test]
fn push_resolution_builds_the_merge_and_moves_only_the_remote_branch() {
    let fixture = ConflictFixture::new();
    let remote = bare_remote(&fixture);
    let feat = git(fixture.root(), &["rev-parse", "feat"]);
    let main = git(fixture.root(), &["rev-parse", "main"]);

    let pushed = fixture
        .backend()
        .push_resolution(&ResolutionPush {
            remote: "o".into(),
            branch: "feat".into(),
            source: feat.clone(),
            target: main.clone(),
            message: "Merge branch 'main' into feat".into(),
            resolutions: vec![("t.txt".into(), "merged line\n".into())],
        })
        .unwrap();

    // The remote branch is the pushed merge commit, parented on the two
    // tips, carrying the resolution and the clean side of the merge.
    assert_eq!(git(remote.path(), &["rev-parse", "feat"]), pushed.as_str());
    assert_eq!(
        git(
            fixture.root(),
            &["log", "-1", "--format=%P", pushed.as_str()]
        ),
        format!("{feat} {main}")
    );
    assert_eq!(
        git(
            fixture.root(),
            &["show", &format!("{}:t.txt", pushed.as_str())]
        ),
        "merged line"
    );
    assert_eq!(
        git(
            fixture.root(),
            &["show", &format!("{}:calm.txt", pushed.as_str())]
        ),
        "calm feat"
    );
    // Nothing local moved: branch, working tree, index, operation state.
    assert_eq!(git(fixture.root(), &["rev-parse", "feat"]), feat);
    assert_eq!(git(fixture.root(), &["status", "--porcelain"]), "");
    assert_eq!(fixture.backend().repo_state().unwrap(), RepoState::Idle);
}

#[test]
fn push_resolution_refuses_an_uncovered_conflict_and_touches_nothing() {
    let fixture = ConflictFixture::new();
    let remote = bare_remote(&fixture);
    let feat = git(fixture.root(), &["rev-parse", "feat"]);
    let main = git(fixture.root(), &["rev-parse", "main"]);

    let error = fixture
        .backend()
        .push_resolution(&ResolutionPush {
            remote: "o".into(),
            branch: "feat".into(),
            source: feat.clone(),
            target: main,
            message: "Merge branch 'main' into feat".into(),
            resolutions: Vec::new(),
        })
        .unwrap_err();

    assert!(error.to_string().contains("t.txt"), "{error}");
    assert_eq!(git(remote.path(), &["rev-parse", "feat"]), feat);
}

#[test]
fn push_resolution_refuses_a_resolution_the_merge_did_not_conflict_on() {
    let fixture = ConflictFixture::new();
    let remote = bare_remote(&fixture);
    let feat = git(fixture.root(), &["rev-parse", "feat"]);
    let main = git(fixture.root(), &["rev-parse", "main"]);

    // calm.txt merges cleanly; a stale resolution for it must refuse, not
    // bury the cleanly merged content.
    let error = fixture
        .backend()
        .push_resolution(&ResolutionPush {
            remote: "o".into(),
            branch: "feat".into(),
            source: feat.clone(),
            target: main,
            message: "Merge branch 'main' into feat".into(),
            resolutions: vec![
                ("t.txt".into(), "merged line\n".into()),
                ("calm.txt".into(), "stale\n".into()),
            ],
        })
        .unwrap_err();

    assert!(error.to_string().contains("calm.txt"), "{error}");
    assert_eq!(git(remote.path(), &["rev-parse", "feat"]), feat);
}

#[test]
fn push_resolution_rejects_a_head_branch_that_moved_on() {
    let fixture = ConflictFixture::new();
    let remote = bare_remote(&fixture);
    let pinned = git(fixture.root(), &["rev-parse", "feat"]);
    let main = git(fixture.root(), &["rev-parse", "main"]);
    // The branch moves on after the review pinned its head: the push must
    // come back rejected instead of overwriting the newer work.
    git(fixture.root(), &["checkout", "-q", "feat"]);
    std::fs::write(fixture.root().join("calm.txt"), "calm moved on\n").unwrap();
    git(fixture.root(), &["commit", "-q", "-am", "feat moves on"]);
    git(fixture.root(), &["push", "-q", "o", "feat"]);
    let moved = git(fixture.root(), &["rev-parse", "feat"]);

    let result = fixture.backend().push_resolution(&ResolutionPush {
        remote: "o".into(),
        branch: "feat".into(),
        source: pinned,
        target: main,
        message: "Merge branch 'main' into feat".into(),
        resolutions: vec![("t.txt".into(), "merged line\n".into())],
    });

    assert!(result.is_err());
    assert_eq!(git(remote.path(), &["rev-parse", "feat"]), moved);
}

#[test]
fn forecast_labels_name_the_sides_with_their_clocks() {
    let fixture = ConflictFixture::new();
    let labels = fixture
        .backend()
        .forecast_labels("feat", "feat", "main", "main")
        .unwrap();
    assert_eq!(labels.yours, "feat");
    assert_eq!(labels.upstream, "main");
    assert_eq!(labels.yours_date, "2026-11-04 10:00");
    assert!(labels.yours_time < labels.upstream_time);
}

#[test]
fn conflicts_name_the_sides_of_a_merge() {
    let fixture = ConflictFixture::new();
    fixture.start_merge();
    let conflicts = fixture.backend().conflicts().unwrap();
    assert_eq!(conflicts.len(), 1);
    let file = &conflicts[0];
    assert_eq!(file.path, "t.txt");
    // Merging feat into main: git's ours is main, the user's own branch.
    assert_eq!(file.base.as_deref(), Some("base line\n"));
    assert_eq!(file.yours.as_deref(), Some("main line\n"));
    assert_eq!(file.upstream.as_deref(), Some("feat line\n"));
    assert_eq!(file.labels.yours, "main");
    assert_eq!(file.labels.upstream, "feat");
    // The yours tip anchors the recency comparison: its absolute date on
    // the committer's clock, and feat's tip four days behind it.
    assert_eq!(file.labels.yours_date, "2026-11-08 10:00");
    assert_eq!(
        file.labels.yours_time - file.labels.upstream_time,
        4 * 86_400
    );
    // diff3 working text: both sides, the base section, all markers.
    assert!(file.working.contains("<<<<<<<"));
    assert!(file.working.contains("|||||||"));
    assert!(file.working.contains(">>>>>>>"));
    assert!(file.working.contains("main line"));
    assert!(file.working.contains("feat line"));
    assert!(file.working.contains("base line"));
}

#[test]
fn conflicts_swap_the_sides_of_a_rebase() {
    let fixture = ConflictFixture::new();
    fixture.start_rebase();
    let conflicts = fixture.backend().conflicts().unwrap();
    assert_eq!(conflicts.len(), 1);
    let file = &conflicts[0];
    assert_eq!(file.path, "t.txt");
    // Rebasing feat onto main: git's ours is main - the upstream being
    // rebased onto - and theirs is the user's replayed commit.
    assert_eq!(file.base.as_deref(), Some("base line\n"));
    assert_eq!(file.yours.as_deref(), Some("feat line\n"));
    assert_eq!(file.upstream.as_deref(), Some("main line\n"));
    assert_eq!(file.labels.yours, "feat rewrites t");
    assert_eq!(file.labels.upstream, "main");
    // The replayed commit anchors the comparison; the onto tip is five
    // days ahead of it.
    assert_eq!(file.labels.yours_date, "2026-11-03 10:00");
    assert_eq!(
        file.labels.upstream_time - file.labels.yours_time,
        5 * 86_400
    );
    assert!(file.working.contains("<<<<<<<"));
}

#[test]
fn conflicts_flag_a_side_that_is_not_text() {
    let fixture = ConflictFixture::new();
    let root = fixture.root();
    // Both branches add raw.bin with clashing bytes; feat's side is not
    // UTF-8, so its lossy text must never be written back.
    git(root, &["checkout", "-q", "feat"]);
    std::fs::write(root.join("raw.bin"), [0xff, 0xfe, 0x00]).unwrap();
    git(root, &["add", "raw.bin"]);
    git(root, &["commit", "-q", "-m", "feat adds raw"]);
    git(root, &["checkout", "-q", "main"]);
    std::fs::write(root.join("raw.bin"), "text\n").unwrap();
    git(root, &["add", "raw.bin"]);
    git(root, &["commit", "-q", "-m", "main adds raw"]);
    fixture.start_merge();

    let conflicts = fixture.backend().conflicts().unwrap();
    assert_eq!(conflicts.len(), 2);
    assert_eq!(conflicts[0].path, "raw.bin");
    assert!(conflicts[0].binary);
    assert_eq!(conflicts[1].path, "t.txt");
    assert!(!conflicts[1].binary);
}

#[test]
fn resolve_file_takes_glob_characters_in_a_path_literally() {
    let fixture = ConflictFixture::new();
    let root = fixture.root();
    // As a glob pathspec `a[b].txt` matches ab.txt and never itself, so
    // both files conflict and only literal matching stages the right one.
    for (branch, text) in [("feat", "feat\n"), ("main", "main\n")] {
        git(root, &["checkout", "-q", branch]);
        std::fs::write(root.join("a[b].txt"), text).unwrap();
        std::fs::write(root.join("ab.txt"), text).unwrap();
        git(root, &["add", "a[b].txt", "ab.txt"]);
        git(root, &["commit", "-q", "-m", "adds bracket files"]);
    }
    fixture.start_merge();
    let backend = fixture.backend();
    backend.resolve_file("a[b].txt", "settled\n").unwrap();

    assert_eq!(
        std::fs::read_to_string(root.join("a[b].txt")).unwrap(),
        "settled\n"
    );
    assert_eq!(
        git(root, &["ls-files", "-u", "--", ":(literal)a[b].txt"]),
        ""
    );
    assert_ne!(git(root, &["ls-files", "-u", "--", ":(literal)ab.txt"]), "");

    backend.unresolve_file("a[b].txt").unwrap();
    assert_ne!(
        git(root, &["ls-files", "-u", "--", ":(literal)a[b].txt"]),
        ""
    );
}

#[test]
fn conflicts_label_the_rebase_upstream_the_user_named() {
    let fixture = ConflictFixture::new();
    let root = fixture.root();
    // `release` and `main` share the onto commit; only the reflog's
    // rebase start entry knows which one the user typed.
    git(root, &["branch", "release", "main"]);
    git(root, &["checkout", "-q", "feat"]);
    git_conflicting(root, &["rebase", "release"]);

    let conflicts = fixture.backend().conflicts().unwrap();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].labels.upstream, "release");
}

#[test]
fn resolve_file_writes_and_stages_the_resolution() {
    let fixture = ConflictFixture::new();
    fixture.start_merge();
    let backend = fixture.backend();
    backend.resolve_file("t.txt", "settled\n").unwrap();

    assert_eq!(
        std::fs::read_to_string(fixture.root().join("t.txt")).unwrap(),
        "settled\n"
    );
    assert_eq!(git(fixture.root(), &["ls-files", "-u", "--", "t.txt"]), "");
    assert!(backend.conflicts().unwrap().is_empty());
    // Staging the last file does not continue anything by itself.
    assert_eq!(backend.repo_state().unwrap(), RepoState::Merge);

    // A path without a conflict is outside the narrow write exception.
    assert!(matches!(
        backend.resolve_file("calm.txt", "no\n"),
        Err(BackendError::NotConflicted(path)) if path == "calm.txt"
    ));
}

#[test]
fn unresolve_file_regenerates_markers_after_staging_in_a_merge() {
    let fixture = ConflictFixture::new();
    fixture.start_merge();
    let backend = fixture.backend();
    backend.resolve_file("t.txt", "settled\n").unwrap();
    backend.unresolve_file("t.txt").unwrap();

    assert_ne!(git(fixture.root(), &["ls-files", "-u", "--", "t.txt"]), "");
    let conflicts = backend.conflicts().unwrap();
    assert_eq!(conflicts.len(), 1);
    let file = &conflicts[0];
    // The markers are back; their labels are generic, so only the stage
    // texts and the stored labels are trusted, never the marker lines.
    assert!(file.working.contains("<<<<<<<"));
    assert_eq!(file.yours.as_deref(), Some("main line\n"));
    assert_eq!(file.upstream.as_deref(), Some("feat line\n"));
}

#[test]
fn unresolve_file_regenerates_markers_after_staging_in_a_rebase() {
    let fixture = ConflictFixture::new();
    fixture.start_rebase();
    let backend = fixture.backend();
    backend.resolve_file("t.txt", "settled\n").unwrap();
    backend.unresolve_file("t.txt").unwrap();

    assert_ne!(git(fixture.root(), &["ls-files", "-u", "--", "t.txt"]), "");
    let conflicts = backend.conflicts().unwrap();
    assert_eq!(conflicts.len(), 1);
    let file = &conflicts[0];
    assert!(file.working.contains("<<<<<<<"));
    // The swap still holds after regeneration.
    assert_eq!(file.yours.as_deref(), Some("feat line\n"));
    assert_eq!(file.upstream.as_deref(), Some("main line\n"));
}

#[test]
fn continue_finishes_a_merge_and_abort_discards_one() {
    let fixture = ConflictFixture::new();
    fixture.start_merge();
    let backend = fixture.backend();
    backend.resolve_file("t.txt", "settled\n").unwrap();
    // GIT_EDITOR=true keeps `merge --continue` from waiting on an editor.
    backend.continue_op().unwrap();
    assert_eq!(backend.repo_state().unwrap(), RepoState::Idle);
    // A real merge commit: two parents, carrying the resolution.
    git(fixture.root(), &["rev-parse", "--verify", "HEAD^2"]);
    assert_eq!(
        std::fs::read_to_string(fixture.root().join("t.txt")).unwrap(),
        "settled\n"
    );

    let fixture = ConflictFixture::new();
    fixture.start_merge();
    let backend = fixture.backend();
    backend.abort_op().unwrap();
    assert_eq!(backend.repo_state().unwrap(), RepoState::Idle);
    assert_eq!(
        std::fs::read_to_string(fixture.root().join("t.txt")).unwrap(),
        "main line\n"
    );
}

#[test]
fn continue_finishes_a_rebase_and_abort_discards_one() {
    let fixture = ConflictFixture::new();
    fixture.start_rebase();
    let backend = fixture.backend();
    backend.resolve_file("t.txt", "settled\n").unwrap();
    // The second commit replays cleanly, so one continue finishes it all.
    backend.continue_op().unwrap();
    assert_eq!(backend.repo_state().unwrap(), RepoState::Idle);
    assert_eq!(
        std::fs::read_to_string(fixture.root().join("t.txt")).unwrap(),
        "settled\n"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.root().join("calm.txt")).unwrap(),
        "calm feat\n"
    );

    let fixture = ConflictFixture::new();
    fixture.start_rebase();
    let backend = fixture.backend();
    backend.abort_op().unwrap();
    assert_eq!(backend.repo_state().unwrap(), RepoState::Idle);
    assert_eq!(
        std::fs::read_to_string(fixture.root().join("t.txt")).unwrap(),
        "feat line\n"
    );
}

#[test]
fn continue_and_abort_refuse_an_idle_repository() {
    let fixture = ConflictFixture::new();
    let backend = fixture.backend();
    assert!(matches!(
        backend.continue_op(),
        Err(BackendError::NoOperation)
    ));
    assert!(matches!(backend.abort_op(), Err(BackendError::NoOperation)));
}

#[test]
fn continue_reports_the_next_conflicted_commit_as_progress() {
    // Two feat commits, each conflicting with main: git exits non-zero
    // when the continue stops at the second one, but that stop is
    // progress, not a refusal.
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["config", "user.name", "Ada"]);
    git(root, &["config", "user.email", "ada@example.invalid"]);
    git(root, &["config", "core.autocrlf", "false"]);
    std::fs::write(root.join("t.txt"), "base t\n").unwrap();
    std::fs::write(root.join("c.txt"), "base c\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "base"]);
    git(root, &["checkout", "-q", "-b", "feat"]);
    std::fs::write(root.join("t.txt"), "feat t\n").unwrap();
    git(root, &["commit", "-q", "-am", "feat t"]);
    std::fs::write(root.join("c.txt"), "feat c\n").unwrap();
    git(root, &["commit", "-q", "-am", "feat c"]);
    git(root, &["checkout", "-q", "main"]);
    std::fs::write(root.join("t.txt"), "main t\n").unwrap();
    std::fs::write(root.join("c.txt"), "main c\n").unwrap();
    git(root, &["commit", "-q", "-am", "main both"]);
    git(root, &["checkout", "-q", "feat"]);
    git_conflicting(root, &["rebase", "main"]);

    let backend = GitCli::open(root).unwrap();
    assert_eq!(
        backend.repo_state().unwrap(),
        RepoState::Rebase { done: 1, total: 2 }
    );
    backend.resolve_file("t.txt", "resolved t\n").unwrap();
    backend.continue_op().unwrap();
    assert_eq!(
        backend.repo_state().unwrap(),
        RepoState::Rebase { done: 2, total: 2 }
    );
    let conflicts = backend.conflicts().unwrap();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].path, "c.txt");
    // With the conflict still unresolved the state cannot move: that
    // continue is a genuine refusal and keeps git's words.
    assert!(matches!(
        backend.continue_op(),
        Err(BackendError::Git { .. })
    ));
    backend.resolve_file("c.txt", "resolved c\n").unwrap();
    backend.continue_op().unwrap();
    assert_eq!(backend.repo_state().unwrap(), RepoState::Idle);
}

/// A stack of pull requests over a throwaway repository. Every commit gets
/// an explicit instant: `commits` is committer-date ordered, and the
/// orderings below are what ownership has to survive.
struct StackRepo {
    directory: tempfile::TempDir,
}

impl StackRepo {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        git(root, &["init", "-q", "-b", "main"]);
        git(root, &["config", "user.name", "Ada"]);
        git(root, &["config", "user.email", "ada@example.invalid"]);
        git(root, &["config", "core.autocrlf", "false"]);
        let repo = Self { directory };
        repo.commit("09:00", "base");
        repo
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    /// One commit adding `name`, at `time` of a fixed day.
    fn commit(&self, time: &str, name: &str) -> Oid {
        std::fs::write(self.root().join(name), format!("{name}\n")).unwrap();
        let date = format!("2026-01-01T{time}:00Z");
        git_at(self.root(), &date, &["add", "."]);
        git_at(self.root(), &date, &["commit", "-q", "-m", name]);
        Oid::new(git(self.root(), &["rev-parse", "HEAD"]))
    }

    fn branch(&self, name: &str) {
        git(self.root(), &["checkout", "-q", "-b", name]);
    }

    fn checkout(&self, name: &str) {
        git(self.root(), &["checkout", "-q", name]);
    }

    /// Merges `name` into the checked-out branch at `time`.
    fn merge(&self, time: &str, name: &str) -> Oid {
        let date = format!("2026-01-01T{time}:00Z");
        let message = format!("merge {name}");
        git_at(
            self.root(),
            &date,
            &["merge", "-q", "--no-ff", name, "-m", &message],
        );
        Oid::new(git(self.root(), &["rev-parse", "HEAD"]))
    }

    fn backend(&self) -> GitCli {
        GitCli::open(self.root()).unwrap()
    }

    /// The comparison a stack review runs on: the latest member's head
    /// against the first member's base.
    fn commits(&self, source: &str, target: &str) -> Vec<Commit> {
        let backend = self.backend();
        let resolved = backend
            .resolve(&Comparison {
                source: source.into(),
                target: target.into(),
                mode: CompareMode::BranchAdds,
                uncommitted: false,
                ..Default::default()
            })
            .unwrap();
        backend.commits(&resolved).unwrap()
    }
}

/// One stack member. `target_commit` stays empty: ownership reads the head
/// and the branch names, never the base commit.
fn member(number: u64, branch: &str, head: &Oid, base: &str) -> PullRequest {
    PullRequest {
        number,
        id: format!("PR_{number}"),
        url: format!("https://github.com/o/r/pull/{number}"),
        state: PrState::Open,
        source_branch: branch.into(),
        source_commit: head.to_string(),
        target_branch: base.into(),
        target_commit: String::new(),
        cross_repository: false,
        head_owner: "o".into(),
    }
}

/// Latest first, as the forge's entries are read in.
fn stack(pulls: Vec<PullRequest>) -> Stack {
    Stack {
        number: Some(99),
        pulls,
    }
}

fn group(ownership: &Ownership, number: u64) -> &[Oid] {
    let group = ownership
        .groups
        .iter()
        .find(|group| group.number == number)
        .unwrap_or_else(|| panic!("#{number} has no group"));
    &group.commits
}

/// Every commit is owned by exactly one member or explicitly unclaimed.
fn assert_total(ownership: &Ownership, commits: &[Commit]) {
    let grouped: usize = ownership
        .groups
        .iter()
        .map(|group| group.commits.len())
        .sum();
    assert_eq!(grouped + ownership.unclaimed.len(), commits.len());
    for commit in commits {
        let owner = ownership.owner(&commit.id).map(|group| group.number);
        let unclaimed = ownership.unclaimed.contains(&commit.id);
        assert!(
            owner.is_some() != unclaimed,
            "{} is owned by {owner:?} and unclaimed {unclaimed}",
            commit.id
        );
    }
}

#[test]
fn a_rebased_stack_gives_each_member_its_own_commits() {
    let repo = StackRepo::new();
    repo.branch("feat/a");
    let a1 = repo.commit("10:00", "a1");
    let a2 = repo.commit("11:00", "a2");
    repo.branch("feat/b");
    let b1 = repo.commit("12:00", "b1");
    repo.branch("feat/c");
    let c1 = repo.commit("13:00", "c1");

    let commits = repo.commits("feat/c", "main");
    let stack = stack(vec![
        member(3, "feat/c", &c1, "feat/b"),
        member(2, "feat/b", &b1, "feat/a"),
        member(1, "feat/a", &a2, "main"),
    ]);
    let owned = ownership(&repo.backend(), &stack, &commits).unwrap();

    assert_eq!(group(&owned, 3), [c1]);
    assert_eq!(group(&owned, 2), [b1]);
    assert_eq!(group(&owned, 1), [a2, a1]);
    assert!(owned.unclaimed.is_empty());
    assert!(owned.stale.is_empty());
    assert!(owned.off_chain.is_empty());
    assert_total(&owned, &commits);
}

#[test]
fn a_member_kept_current_by_a_merge_keeps_the_commits_above_it() {
    let repo = StackRepo::new();
    repo.branch("feat/a");
    let a1 = repo.commit("10:00", "a1");
    repo.branch("feat/b");
    let b1 = repo.commit("11:00", "b1");
    // The review fix lands on the member below after the member above
    // committed, and the member above takes it by merging.
    repo.checkout("feat/a");
    let a2 = repo.commit("12:00", "a2");
    repo.checkout("feat/b");
    let merge = repo.merge("13:00", "feat/a");

    let commits = repo.commits("feat/b", "main");
    // The trap an index range falls into: the lower member's head sits
    // above the upper member's own commit in the list, so a range taken
    // from it would hand `b1` to #1.
    assert_eq!(
        commits.iter().map(|commit| &commit.id).collect::<Vec<_>>(),
        vec![&merge, &a2, &b1, &a1]
    );

    let stack = stack(vec![
        member(2, "feat/b", &merge, "feat/a"),
        member(1, "feat/a", &a2, "main"),
    ]);
    let owned = ownership(&repo.backend(), &stack, &commits).unwrap();

    assert_eq!(group(&owned, 2), [merge, b1]);
    assert_eq!(group(&owned, 1), [a2, a1]);
    assert!(owned.stale.is_empty());
    assert_total(&owned, &commits);
}

#[test]
fn a_member_head_outside_the_comparison_is_reported() {
    let repo = StackRepo::new();
    repo.branch("feat/a");
    let a1 = repo.commit("10:00", "a1");
    repo.branch("feat/b");
    let b1 = repo.commit("11:00", "b1");
    // The member below takes a review fix and the one above has not
    // rebased onto it, so #1's head is nowhere in the comparison.
    repo.checkout("feat/a");
    let a2 = repo.commit("12:00", "a2");

    let commits = repo.commits("feat/b", "main");
    let stack = stack(vec![
        member(2, "feat/b", &b1, "feat/a"),
        member(1, "feat/a", &a2, "main"),
    ]);
    let owned = ownership(&repo.backend(), &stack, &commits).unwrap();

    assert_eq!(owned.stale, vec![1]);
    // Reported, not skipped: the commits it does carry are still its own.
    assert_eq!(group(&owned, 1), [a1]);
    assert_eq!(group(&owned, 2), [b1]);
    assert_total(&owned, &commits);
}

#[test]
fn commits_no_member_claims_can_sit_anywhere_in_the_list() {
    let repo = StackRepo::new();
    repo.branch("feat/a");
    let a1 = repo.commit("10:00", "a1");
    repo.branch("feat/b");
    let b1 = repo.commit("12:00", "b1");
    // Work that belongs to no pull request, merged into the latest
    // branch after the stack was resolved on `b1`.
    repo.checkout("feat/a");
    repo.branch("side");
    let u1 = repo.commit("11:00", "u1");
    repo.checkout("feat/b");
    let merge = repo.merge("13:00", "side");

    let commits = repo.commits("feat/b", "main");
    let stack = stack(vec![
        member(2, "feat/b", &b1, "feat/a"),
        member(1, "feat/a", &a1, "main"),
    ]);
    let owned = ownership(&repo.backend(), &stack, &commits).unwrap();

    assert_eq!(group(&owned, 2), [b1]);
    assert_eq!(group(&owned, 1), [a1]);
    // `u1` sits between two owned commits, not at the end of the list.
    assert_eq!(commits[2].id, u1);
    assert_eq!(owned.unclaimed, vec![merge, u1]);
    assert_total(&owned, &commits);
}

#[test]
fn a_stack_with_no_members_leaves_every_commit_unclaimed() {
    let repo = StackRepo::new();
    repo.branch("feat/a");
    let a1 = repo.commit("10:00", "a1");
    let a2 = repo.commit("11:00", "a2");

    let commits = repo.commits("feat/a", "main");
    let owned = ownership(&repo.backend(), &stack(Vec::new()), &commits).unwrap();

    assert!(owned.groups.is_empty());
    assert_eq!(owned.unclaimed, vec![a2, a1]);
    assert_total(&owned, &commits);
}
