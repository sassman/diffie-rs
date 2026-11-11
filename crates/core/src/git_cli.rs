//! The cheap backend: shells out to `git`.
//!
//! It exists so the UI can be built on real repositories from day one. It is
//! meant to be replaced by a `gix` + `imara-diff` backend behind the same
//! [`ReviewBackend`] trait once the UI has settled; do not grow it beyond that.
//!
//! It never writes to the repository. Combining non-contiguous commits needs
//! scratch objects, and those go to a temporary object directory that borrows
//! the real one as an alternate.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::backend::{BackendError, Result, ReviewBackend};
use crate::model::{
    BlameLine, Commit, CompareMode, Comparison, FileState, FileStatus, FileSummary, LineOrigin,
    NewSide, Oid, Resolved, Scope, ScopedPatch,
};
use crate::review::ReviewedFile;

/// Unchanged lines git supplies around each change. The Diff view folds them
/// and can only expand what the patch contains, so this is generous.
const CONTEXT_LINES: usize = 400;

/// Git's well-known empty tree, the "parent" of a root commit.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

pub struct GitCli {
    repository: PathBuf,
}

impl GitCli {
    /// `path` may point anywhere inside a working tree, or at a bare repository.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let backend = Self {
            repository: path.into(),
        };
        backend
            .read(&["rev-parse", "--git-dir"])
            .map_err(|error| match error {
                BackendError::Git { .. } => {
                    BackendError::NotARepository(backend.repository.display().to_string())
                }
                other => other,
            })?;
        Ok(backend)
    }

    /// The checked-out branch, or `None` on a detached HEAD. Lets the app name
    /// the review after the branch instead of after the literal `HEAD`.
    pub fn current_branch(&self) -> Result<Option<String>> {
        Ok(self
            .try_read(&["symbolic-ref", "--quiet", "--short", "HEAD"])?
            .map(|output| output.trim().to_string())
            .filter(|name| !name.is_empty()))
    }

    fn command(&self) -> Command {
        let mut command = Command::new("git");
        command
            .arg("--no-pager")
            .arg("-C")
            .arg(&self.repository)
            .args(["-c", "core.quotepath=off", "-c", "color.ui=never"])
            // Never take locks or refresh the index of the user's repository.
            .env("GIT_OPTIONAL_LOCKS", "0")
            // Error texts are matched below; keep them in English.
            .env("LC_ALL", "C");
        command
    }

    fn read(&self, args: &[&str]) -> Result<String> {
        run(self.command().args(args), args, None)
    }

    /// `Ok(None)` when git exits non-zero, for lookups where "no" is an answer.
    fn try_read(&self, args: &[&str]) -> Result<Option<String>> {
        match self.read(args) {
            Ok(output) => Ok(Some(output)),
            Err(BackendError::Git { .. }) => Ok(None),
            Err(other) => Err(other),
        }
    }

    fn commit_id(&self, revision: &str) -> Result<Oid> {
        let spec = format!("{revision}^{{commit}}");
        // `--end-of-options` stops a branch name from being read as a flag.
        self.try_read(&[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &spec,
        ])?
        .map(|output| Oid::new(output.trim()))
        .ok_or_else(|| BackendError::UnknownRevision(revision.to_string()))
    }

    /// The first parent, or the empty tree for a root commit.
    fn parent_of(&self, commit: &Oid) -> Result<String> {
        let spec = format!("{commit}^");
        Ok(self
            .try_read(&["rev-parse", "--verify", "--quiet", &spec])?
            .map(|output| output.trim().to_string())
            .unwrap_or_else(|| EMPTY_TREE.to_string()))
    }

    fn diff_args() -> Vec<String> {
        [
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            // The algorithm this project chose: stable, block-consistent hunks.
            "--histogram",
            "--indent-heuristic",
            "--find-renames",
            "--src-prefix=a/",
            "--dst-prefix=b/",
        ]
        .into_iter()
        .map(String::from)
        .chain([format!("-U{CONTEXT_LINES}")])
        .collect()
    }

    fn diff(&self, from: &str, to: &str, paths: &[&str]) -> Result<String> {
        let mut args = Self::diff_args();
        args.push(from.to_string());
        args.push(to.to_string());
        args.push("--".to_string());
        args.extend(paths.iter().map(|path| path.to_string()));
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        self.read(&args)
    }

    /// Oldest first, in the order of the comparison's history.
    fn in_history_order(&self, resolved: &Resolved, picked: &[Oid]) -> Result<(Vec<Oid>, bool)> {
        let history = self.commits(resolved)?;
        let positions: Vec<usize> = history
            .iter()
            .enumerate()
            .filter(|(_, commit)| picked.contains(&commit.id))
            .map(|(index, _)| index)
            .collect();
        let contiguous = positions.windows(2).all(|pair| pair[1] == pair[0] + 1);
        let ordered = positions
            .iter()
            .rev()
            .map(|index| history[*index].id.clone())
            .collect();
        Ok((ordered, contiguous))
    }

    fn new_for_me(&self, resolved: &Resolved, reviewed: &[ReviewedFile]) -> Result<ScopedPatch> {
        let base = resolved.base.as_str();
        let head = resolved.head.as_str();
        let whole = self.diff(base, head, &[])?;
        let mut text = String::new();
        let mut files = Vec::new();

        for (summary, section) in split_files(&whole) {
            let tick = reviewed.iter().find(|tick| tick.path == summary.path);
            let current = self.blob_id(&resolved.head, &summary.path)?;
            match (tick, current) {
                (Some(tick), Some(current)) if tick.blob == current => {
                    files.push(FileSummary {
                        state: FileState::UnchangedSinceReview,
                        ..summary
                    });
                }
                (Some(tick), Some(current)) if self.object_exists(&tick.blob)? => {
                    let since = self.diff(tick.blob.as_str(), current.as_str(), &[])?;
                    let since = relabel_blob_diff(&since, &summary.path);
                    let counted = split_files(&since)
                        .into_iter()
                        .next()
                        .map(|(counted, _)| counted)
                        .unwrap_or_else(|| summary.clone());
                    files.push(FileSummary {
                        path: summary.path.clone(),
                        status: FileStatus::Modified,
                        ..counted
                    });
                    text.push_str(&since);
                }
                // Never ticked, deleted since, or the ticked content is gone
                // (rebased away and collected): show the full diff.
                _ => {
                    files.push(summary);
                    text.push_str(section);
                }
            }
        }
        Ok(ScopedPatch {
            text,
            files,
            new_side: NewSide::Commit(resolved.head.clone()),
        })
    }

    fn object_exists(&self, id: &Oid) -> Result<bool> {
        Ok(self.try_read(&["cat-file", "-e", id.as_str()])?.is_some())
    }

    /// Replay only `picked` (oldest first) onto the parent of the first one,
    /// file by file, and diff the result. A file that cannot be replayed
    /// cleanly stops at the commit that failed and is reported as such.
    fn combine(&self, picked: &[Oid]) -> Result<ScopedPatch> {
        let scratch = Scratch::new()?;
        let objects = scratch.path.join("objects");
        std::fs::create_dir_all(&objects)?;
        let index = scratch.path.join("index");
        let real_objects =
            self.absolute(self.read(&["rev-parse", "--git-path", "objects"])?.trim());

        let with_scratch = |command: &mut Command| {
            command
                .env("GIT_INDEX_FILE", &index)
                .env("GIT_OBJECT_DIRECTORY", &objects)
                .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", &real_objects);
        };
        let scratch_read = |args: &[&str], stdin: Option<&[u8]>| -> Result<String> {
            let mut command = self.command();
            with_scratch(&mut command);
            run(command.args(args), args, stdin)
        };

        let base = self.parent_of(&picked[0])?;
        scratch_read(&["read-tree", &base], None)?;

        let mut failed: HashMap<String, Oid> = HashMap::new();
        for commit in picked {
            let parent = self.parent_of(commit)?;
            let names = self.read(&[
                "diff-tree",
                "--no-commit-id",
                "--name-only",
                "--no-renames",
                "-r",
                "-z",
                &parent,
                commit.as_str(),
            ])?;
            for path in names.split('\0').filter(|path| !path.is_empty()) {
                if failed.contains_key(path) {
                    continue;
                }
                let patch = self.read(&[
                    "diff",
                    "--no-color",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--binary",
                    "--full-index",
                    "--no-renames",
                    &parent,
                    commit.as_str(),
                    "--",
                    path,
                ])?;
                // A failed three-way apply can leave conflict entries behind.
                let saved = std::fs::read(&index)?;
                let applied = scratch_read(
                    &["apply", "--cached", "--3way", "--whitespace=nowarn", "-"],
                    Some(patch.as_bytes()),
                );
                match applied {
                    Ok(_) => {}
                    Err(BackendError::Git { .. }) => {
                        std::fs::write(&index, saved)?;
                        failed.insert(path.to_string(), commit.clone());
                    }
                    Err(other) => return Err(other),
                }
            }
        }

        let tree = scratch_read(&["write-tree"], None)?;
        let mut args = Self::diff_args();
        args.push(base);
        args.push(tree.trim().to_string());
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let text = scratch_read(&args, None)?;

        let mut files: Vec<FileSummary> = split_files(&text)
            .into_iter()
            .map(|(summary, _)| summary)
            .collect();
        for (path, commit) in failed {
            let state = FileState::CouldNotCombine { commit };
            match files.iter_mut().find(|file| file.path == path) {
                Some(file) => file.state = state,
                None => files.push(FileSummary {
                    path,
                    status: FileStatus::Modified,
                    additions: 0,
                    deletions: 0,
                    state,
                    binary: false,
                }),
            }
        }
        Ok(ScopedPatch {
            text,
            files,
            new_side: NewSide::Synthetic,
        })
    }

    fn absolute(&self, path: &str) -> PathBuf {
        let path = Path::new(path);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.repository.join(path)
        }
    }

    fn blame_with(
        &self,
        rev: &Oid,
        path: &str,
        range: Option<usize>,
    ) -> Result<Option<Vec<BlameLine>>> {
        let range = range.map(|line| format!("-L{line},{line}"));
        let mut args = vec!["blame", "--porcelain"];
        if let Some(range) = &range {
            args.push(range);
        }
        args.extend([rev.as_str(), "--", path]);
        match self.read(&args) {
            Ok(output) => Ok(Some(parse_blame(&output))),
            Err(BackendError::Git { stderr, .. })
                if stderr.contains("has only") || stderr.contains("no such path") =>
            {
                Ok(None)
            }
            Err(other) => Err(other),
        }
    }
}

impl ReviewBackend for GitCli {
    fn resolve(&self, comparison: &Comparison) -> Result<Resolved> {
        let head = self.commit_id(&comparison.source)?;
        let target_head = self.commit_id(&comparison.target)?;
        let base = match comparison.mode {
            CompareMode::EveryDifference => target_head.clone(),
            CompareMode::BranchAdds => self
                .try_read(&["merge-base", target_head.as_str(), head.as_str()])?
                .map(|output| Oid::new(output.trim()))
                .ok_or_else(|| BackendError::NoMergeBase {
                    source_rev: comparison.source.clone(),
                    target: comparison.target.clone(),
                })?,
        };
        Ok(Resolved {
            comparison: comparison.clone(),
            head,
            target_head,
            base,
        })
    }

    fn commits(&self, resolved: &Resolved) -> Result<Vec<Commit>> {
        let range = format!("{}..{}", resolved.base, resolved.head);
        let output = self.read(&[
            "log",
            "--no-show-signature",
            "--encoding=UTF-8",
            "-z",
            "--format=%H%x00%an%x00%at%x00%s",
            &range,
            "--",
        ])?;
        let fields: Vec<&str> = output.split('\0').collect();
        Ok(fields
            .chunks_exact(4)
            .map(|record| Commit {
                id: Oid::new(record[0].trim()),
                author: record[1].to_string(),
                time: record[2].parse().unwrap_or_default(),
                subject: record[3].to_string(),
            })
            .collect())
    }

    fn patch(
        &self,
        resolved: &Resolved,
        scope: &Scope,
        reviewed: &[ReviewedFile],
    ) -> Result<ScopedPatch> {
        match scope {
            Scope::NewForMe => self.new_for_me(resolved, reviewed),
            Scope::WholeBranch => {
                let text = self.diff(resolved.base.as_str(), resolved.head.as_str(), &[])?;
                Ok(patch_from(text, NewSide::Commit(resolved.head.clone())))
            }
            Scope::Commits(picked) => {
                let (ordered, contiguous) = self.in_history_order(resolved, picked)?;
                let (Some(oldest), Some(newest)) = (ordered.first(), ordered.last()) else {
                    return Ok(patch_from(
                        String::new(),
                        NewSide::Commit(resolved.head.clone()),
                    ));
                };
                if contiguous {
                    let text = self.diff(&self.parent_of(oldest)?, newest.as_str(), &[])?;
                    Ok(patch_from(text, NewSide::Commit(newest.clone())))
                } else {
                    self.combine(&ordered)
                }
            }
        }
    }

    fn line_origin(&self, rev: &Oid, path: &str, line: usize) -> Result<Option<LineOrigin>> {
        Ok(self
            .blame_with(rev, path, Some(line))?
            .and_then(|rows| rows.into_iter().next())
            .map(|row| row.origin))
    }

    fn blame(&self, rev: &Oid, path: &str) -> Result<Vec<BlameLine>> {
        Ok(self.blame_with(rev, path, None)?.unwrap_or_default())
    }

    fn blob_id(&self, rev: &Oid, path: &str) -> Result<Option<Oid>> {
        let spec = format!("{rev}:{path}");
        Ok(self
            .try_read(&["rev-parse", "--verify", "--quiet", &spec])?
            .map(|output| Oid::new(output.trim())))
    }

    fn author_name(&self) -> Result<String> {
        let name = self
            .try_read(&["config", "user.name"])?
            .map(|output| output.trim().to_string())
            .filter(|name| !name.is_empty());
        Ok(name
            .or_else(|| std::env::var("USER").ok())
            .or_else(|| std::env::var("USERNAME").ok())
            .unwrap_or_else(|| "reviewer".to_string()))
    }

    fn review_dir(&self) -> Result<PathBuf> {
        let common = self.read(&["rev-parse", "--git-common-dir"])?;
        Ok(self.absolute(common.trim()).join("review"))
    }
}

fn run(command: &mut Command, args: &[&str], stdin: Option<&[u8]>) -> Result<String> {
    command
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(BackendError::GitMissing)?;
    if let Some(input) = stdin {
        // Feed stdin from a thread: a large patch would otherwise deadlock
        // against git filling its stdout pipe.
        let mut pipe = child.stdin.take().expect("stdin was piped");
        let input = input.to_vec();
        std::thread::spawn(move || {
            let _ = pipe.write_all(&input);
        });
    }
    let output = child.wait_with_output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(BackendError::Git {
            command: args.first().copied().unwrap_or_default().to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

fn patch_from(text: String, new_side: NewSide) -> ScopedPatch {
    let files = split_files(&text)
        .into_iter()
        .map(|(summary, _)| summary)
        .collect();
    ScopedPatch {
        text,
        files,
        new_side,
    }
}

/// Split a git patch into per-file sections with a summary each.
fn split_files(patch: &str) -> Vec<(FileSummary, &str)> {
    let mut starts: Vec<usize> = Vec::new();
    let mut offset = 0;
    for line in patch.split_inclusive('\n') {
        if line.starts_with("diff --git ") {
            starts.push(offset);
        }
        offset += line.len();
    }
    starts
        .iter()
        .enumerate()
        .map(|(index, start)| {
            let end = starts.get(index + 1).copied().unwrap_or(patch.len());
            let section = &patch[*start..end];
            (summarize(section), section)
        })
        .collect()
}

fn summarize(section: &str) -> FileSummary {
    let mut old_path = None;
    let mut new_path = None;
    let mut renamed_to = None;
    let mut status = FileStatus::Modified;
    let mut binary = false;
    let mut additions = 0;
    let mut deletions = 0;
    let mut in_hunk = false;
    let mut first_line = "";

    for (number, line) in section.lines().enumerate() {
        if number == 0 {
            first_line = line;
        }
        if line.starts_with("@@") {
            in_hunk = true;
            continue;
        }
        if in_hunk {
            match line.as_bytes().first() {
                Some(b'+') => additions += 1,
                Some(b'-') => deletions += 1,
                _ => {}
            }
            continue;
        }
        if let Some(path) = line.strip_prefix("--- a/") {
            old_path = Some(path.to_string());
        } else if let Some(path) = line.strip_prefix("+++ b/") {
            new_path = Some(path.to_string());
        } else if let Some(path) = line.strip_prefix("rename to ") {
            renamed_to = Some(path.to_string());
            status = FileStatus::Renamed;
        } else if line.starts_with("new file mode") {
            status = FileStatus::Added;
        } else if line.starts_with("deleted file mode") {
            status = FileStatus::Deleted;
        } else if line.starts_with("Binary files ") || line == "GIT binary patch" {
            binary = true;
        }
    }

    // Header-only sections (binary, mode change) have no ---/+++ lines; fall
    // back to the `b/` half of `diff --git a/x b/x`.
    let from_header = || {
        let rest = first_line
            .strip_prefix("diff --git a/")
            .unwrap_or(first_line);
        match rest.len() {
            // `x b/x`: both halves are equal, so the path is half of the rest.
            length if length >= 3 => rest[(length + 3) / 2..].to_string(),
            _ => rest.to_string(),
        }
    };
    let path = renamed_to
        .or(new_path)
        .or(old_path)
        .unwrap_or_else(from_header);

    FileSummary {
        path,
        status,
        additions,
        deletions,
        state: FileState::Changed,
        binary,
    }
}

/// `git diff <blob> <blob>` labels both sides with object ids. Put the real
/// path back so the section reads like any other file's diff.
fn relabel_blob_diff(patch: &str, path: &str) -> String {
    let mut out = String::with_capacity(patch.len());
    let mut in_hunk = false;
    for line in patch.split_inclusive('\n') {
        if in_hunk || line.starts_with("@@") {
            in_hunk = true;
            out.push_str(line);
        } else if line.starts_with("diff --git ") {
            out.push_str(&format!("diff --git a/{path} b/{path}\n"));
        } else if line.starts_with("--- ") {
            out.push_str(&format!("--- a/{path}\n"));
        } else if line.starts_with("+++ ") {
            out.push_str(&format!("+++ b/{path}\n"));
        } else if line.starts_with("Binary files ") {
            out.push_str(&format!("Binary files a/{path} and b/{path} differ\n"));
        } else {
            out.push_str(line);
        }
    }
    out
}

fn parse_blame(porcelain: &str) -> Vec<BlameLine> {
    let mut rows = Vec::new();
    for line in porcelain.lines() {
        if line.starts_with('\t') {
            continue;
        }
        let mut parts = line.split(' ');
        let (Some(id), Some(original), Some(last)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let is_object_id = id.len() >= 40 && id.bytes().all(|byte| byte.is_ascii_hexdigit());
        if !is_object_id {
            continue;
        }
        if let (Ok(original), Ok(last)) = (original.parse(), last.parse()) {
            rows.push(BlameLine {
                final_line: last,
                origin: LineOrigin {
                    commit: Oid::new(id),
                    line: original,
                },
            });
        }
    }
    rows
}

/// A temporary directory removed on drop.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new() -> std::io::Result<Self> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "review-core-{}-{nanos}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_sections() {
        let patch = "\
diff --git a/src/a.rs b/src/a.rs
index 1111111..2222222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,3 +1,3 @@
 fn main() {
-    run();
+    run_app();
 }
diff --git a/new.txt b/new.txt
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+--- not a header
+second
diff --git a/logo.png b/logo.png
index 4444444..5555555 100644
Binary files a/logo.png and b/logo.png differ
";
        let files = split_files(patch);
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].0.path, "src/a.rs");
        assert_eq!((files[0].0.additions, files[0].0.deletions), (1, 1));
        assert_eq!(files[1].0.status, FileStatus::Added);
        assert_eq!(files[1].0.additions, 2);
        assert_eq!(files[2].0.path, "logo.png");
        assert!(files[2].0.binary);
        assert!(files[1].1.starts_with("diff --git a/new.txt"));
    }

    #[test]
    fn relabels_blob_diffs() {
        let patch = "diff --git a/1111111 b/2222222\nindex 1111111..2222222 100644\n--- a/1111111\n+++ b/2222222\n@@ -1 +1 @@\n--- kept\n+new\n";
        let relabeled = relabel_blob_diff(patch, "src/a.rs");
        assert!(relabeled.starts_with("diff --git a/src/a.rs b/src/a.rs\nindex"));
        assert!(relabeled.contains("--- a/src/a.rs\n+++ b/src/a.rs\n@@"));
        assert!(relabeled.ends_with("--- kept\n+new\n"));
    }

    #[test]
    fn parses_porcelain_blame() {
        let id = "a".repeat(40);
        let porcelain =
            format!("{id} 45 48 1\nauthor Ada\nsummary x\n\tcode\n{id} 46 49\n\t{id} 1 2\n");
        let rows = parse_blame(&porcelain);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].final_line, 48);
        assert_eq!(rows[0].origin.line, 45);
        assert_eq!(rows[1].final_line, 49);
    }
}
