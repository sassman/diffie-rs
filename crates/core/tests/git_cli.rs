//! Runs the cheap backend against a real, throwaway repository.

use std::path::Path;
use std::process::Command;

use diffie_core::review::{ReviewInput, ReviewedFile};
use diffie_core::{
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
