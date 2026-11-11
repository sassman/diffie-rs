//! Runs the cheap backend against a real, throwaway repository.

use std::path::Path;
use std::process::Command;

use review_core::review::ReviewedFile;
use review_core::{
    BackendError, CompareMode, Comparison, FileState, FileStatus, GitCli, NewSide, Oid,
    ReviewBackend, Scope,
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

fn paths(files: &[review_core::FileSummary]) -> Vec<&str> {
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
fn whole_branch_covers_every_file() {
    let fixture = Fixture::new();
    let backend = fixture.backend();
    let resolved = backend.resolve(&Fixture::comparison()).unwrap();
    let patch = backend.patch(&resolved, &Scope::WholeBranch, &[]).unwrap();
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
fn a_contiguous_selection_is_a_plain_range() {
    let fixture = Fixture::new();
    let backend = fixture.backend();
    let resolved = backend.resolve(&Fixture::comparison()).unwrap();
    let picked = vec![fixture.commits[2].clone(), fixture.commits[1].clone()];
    let patch = backend
        .patch(&resolved, &Scope::Commits(picked), &[])
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
        .patch(&resolved, &Scope::Commits(picked), &[])
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
        .patch(&resolved, &Scope::Commits(picked), &[])
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
fn new_for_me_compares_each_file_with_its_tick() {
    let fixture = Fixture::new();
    let backend = fixture.backend();
    let resolved = backend.resolve(&Fixture::comparison()).unwrap();
    let tick = |path: &str, commit: &Oid| ReviewedFile {
        path: path.into(),
        blob: backend.blob_id(commit, path).unwrap().unwrap(),
        commit: commit.clone(),
        at: "2026-11-11T00:00:00Z".into(),
    };

    // Nothing ticked: same as the whole branch.
    let patch = backend.patch(&resolved, &Scope::NewForMe, &[]).unwrap();
    assert_eq!(paths(&patch.files), ["a.txt", "b.txt"]);

    // a.txt was reviewed at c1, b.txt at head.
    let reviewed = [
        tick("a.txt", &fixture.commits[0]),
        tick("b.txt", &resolved.head),
    ];
    let patch = backend
        .patch(&resolved, &Scope::NewForMe, &reviewed)
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
fn review_files_live_inside_the_git_directory() {
    let fixture = Fixture::new();
    let backend = fixture.backend();
    let directory = backend.review_dir().unwrap();
    assert!(directory.ends_with(".git/review"));
    assert_eq!(backend.author_name().unwrap(), "Ada");
    assert_eq!(backend.blob_id(&fixture.commits[0], "b.txt").unwrap(), None);
}
