//! Builds revision views against a real, throwaway repository.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use diffie_core::{GitCli, Oid};
use diffie_symbols::SymbolIndex;

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

const CALLER: &str = "fn run() { place(); }\n";

struct Fixture {
    directory: tempfile::TempDir,
    /// `place` lives in `src/old.rs`.
    first: Oid,
    /// `place` moved to `src/new.rs`, two lines further down.
    second: Oid,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::create_dir(root.join("src")).unwrap();
        let commit = |message: &str| {
            git(root, &["add", "-A"]);
            git(root, &["commit", "-q", "-m", message]);
            Oid::new(git(root, &["rev-parse", "HEAD"]))
        };
        std::fs::write(root.join("src/app.rs"), CALLER).unwrap();
        std::fs::write(root.join("src/old.rs"), "pub fn place() {}\n").unwrap();
        std::fs::write(root.join("notes.md"), "fn place() {}\n").unwrap();
        let first = commit("first");
        std::fs::remove_file(root.join("src/old.rs")).unwrap();
        std::fs::write(root.join("src/new.rs"), "\n\npub fn place() {}\n").unwrap();
        let second = commit("second");
        Self {
            directory,
            first,
            second,
        }
    }

    fn backend(&self) -> GitCli {
        GitCli::open(self.directory.path()).unwrap()
    }
}

fn shown(paths: &[&str]) -> HashSet<String> {
    paths.iter().map(|path| path.to_string()).collect()
}

/// Where the call in `src/app.rs` lands, as `path:line`.
fn call_targets(view: &diffie_symbols::RevisionView) -> Vec<String> {
    let reference = view
        .reference_at("src/app.rs", 1, CALLER.find("place").unwrap())
        .expect("the call is a reference");
    view.resolve("src/app.rs", reference)
        .into_iter()
        .map(|target| format!("{}:{}", target.path, target.definition.line))
        .collect()
}

#[test]
fn each_revision_resolves_against_its_own_tree() {
    let fixture = Fixture::new();
    let backend = fixture.backend();
    let index = SymbolIndex::new();
    let shown = shown(&["src/app.rs"]);

    let first = index.view(&backend, &fixture.first, &[], &shown).unwrap();
    let second = index.view(&backend, &fixture.second, &[], &shown).unwrap();

    // The caller's blob is the same in both; only the tree around it moved.
    assert_eq!(first.source("src/app.rs"), second.source("src/app.rs"));
    assert_eq!(call_targets(&first), ["src/old.rs:1"]);
    assert_eq!(call_targets(&second), ["src/new.rs:3"]);
    assert!(first.contains("src/old.rs") && !second.contains("src/old.rs"));
    // No language claims the markdown file.
    assert!(!first.contains("notes.md"));
}

#[test]
fn references_are_loaded_for_shown_files_only() {
    let fixture = Fixture::new();
    let backend = fixture.backend();
    let index = SymbolIndex::new();

    let bare = index
        .view(&backend, &fixture.second, &[], &HashSet::new())
        .unwrap();
    assert!(!bare.has_references("src/app.rs"));
    assert!(bare.reference_at("src/app.rs", 1, 12).is_none());

    // A later view that shows the file upgrades the cached entry.
    let full = index
        .view(&backend, &fixture.second, &[], &shown(&["src/app.rs"]))
        .unwrap();
    assert!(full.has_references("src/app.rs"));
    assert!(!full.has_references("src/new.rs"));
    assert_eq!(call_targets(&full), ["src/new.rs:3"]);
}

#[test]
fn the_working_tree_overlays_the_revision() {
    let fixture = Fixture::new();
    let root = fixture.directory.path();
    // Uncommitted: the definition moves down, a new file appears and the
    // old home of the call is deleted in favour of another caller.
    std::fs::write(root.join("src/new.rs"), "\n\n\n\npub fn place() {}\n").unwrap();
    std::fs::write(root.join("src/extra.rs"), "pub fn extra() {}\n").unwrap();
    let backend = fixture.backend();
    let index = SymbolIndex::new();
    let worktree = ["src/new.rs".to_string(), "src/extra.rs".to_string()];

    let view = index
        .view(
            &backend,
            &fixture.second,
            &worktree,
            &shown(&["src/app.rs"]),
        )
        .unwrap();

    assert_eq!(call_targets(&view), ["src/new.rs:5"]);
    assert!(view.contains("src/extra.rs"));
    assert_eq!(
        view.source("src/new.rs").map(|(_, worktree)| worktree),
        Some(true)
    );
    assert_eq!(
        view.source("src/app.rs").map(|(_, worktree)| worktree),
        Some(false)
    );

    // The committed tree is untouched by the overlay.
    let committed = index
        .view(&backend, &fixture.second, &[], &shown(&["src/app.rs"]))
        .unwrap();
    assert_eq!(call_targets(&committed), ["src/new.rs:3"]);
    assert!(!committed.contains("src/extra.rs"));
}

#[test]
fn a_file_deleted_in_the_working_tree_leaves_the_view() {
    let fixture = Fixture::new();
    let root = fixture.directory.path();
    std::fs::remove_file(root.join("src/new.rs")).unwrap();
    let backend = fixture.backend();
    let index = SymbolIndex::new();

    let view = index
        .view(
            &backend,
            &fixture.second,
            &["src/new.rs".to_string()],
            &shown(&["src/app.rs"]),
        )
        .unwrap();

    assert!(!view.contains("src/new.rs"));
    assert!(call_targets(&view).is_empty());
}
