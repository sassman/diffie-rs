//! The cheap backend: shells out to `git`.
//!
//! It exists so the UI can be built on real repositories from day one. It is
//! meant to be replaced by a `gix` + `imara-diff` backend behind the same
//! [`ReviewBackend`] trait once the UI has settled; do not grow it beyond that.
//!
//! It never writes to the repository, with one sanctioned exception: a
//! fetch on the reviewer's explicit action (see [`ReviewBackend::fetch`]).
//! Combining non-contiguous commits needs scratch objects, and those go to
//! a temporary object directory that borrows the real one as an alternate.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::backend::{BackendError, Result, ReviewBackend};
use crate::model::{
    BlameLine, ChangedRegion, Commit, CompareMode, Comparison, ConflictFile, ConflictLabels,
    FileState, FileStatus, FileSummary, LineOrigin, NewSide, Oid, RepoState, Resolved, Scope,
    ScopedPatch,
};
use crate::review::{ReviewInput, ReviewedFile};

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

    /// Whether a working tree exists. A bare repository has no uncommitted
    /// state to review.
    pub fn has_working_tree(&self) -> Result<bool> {
        Ok(self.try_read(&["rev-parse", "--show-toplevel"])?.is_some())
    }

    /// The project's name and, when [`Self::open`] pointed into a linked
    /// worktree, that worktree's name. The project is named after the
    /// directory holding the common git dir, so every worktree of a
    /// repository reads the same.
    pub fn display_names(&self) -> Result<(String, Option<String>)> {
        let dirs = self.read(&[
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
        ])?;
        let mut lines = dirs.lines();
        let git_dir = PathBuf::from(lines.next().unwrap_or_default().trim());
        let common = PathBuf::from(lines.next().unwrap_or_default().trim());
        let basename = |path: &Path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        };
        let project = if basename(&common).as_deref() == Some(".git") {
            common.parent().and_then(basename)
        } else {
            // Bare: name `thrall.git` minus the conventional suffix.
            basename(&common).map(|name| name.trim_end_matches(".git").to_string())
        }
        .unwrap_or_else(|| "repository".to_string());
        let worktree = if git_dir == common {
            None
        } else {
            self.try_read(&["rev-parse", "--show-toplevel"])?
                .and_then(|top| basename(Path::new(top.trim())))
        };
        Ok((project, worktree))
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

    fn read(&self, args: &[impl AsRef<str>]) -> Result<String> {
        run(&mut self.command(), args, None)
    }

    /// `Ok(None)` when git exits non-zero, for lookups where "no" is an answer.
    fn try_read(&self, args: &[impl AsRef<str>]) -> Result<Option<String>> {
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
            .try_read(&[
                "rev-parse",
                "--verify",
                "--quiet",
                "--end-of-options",
                &spec,
            ])?
            .map(|output| output.trim().to_string())
            .unwrap_or_else(|| EMPTY_TREE.to_string()))
    }

    fn diff_args(ignore_whitespace: bool) -> Vec<String> {
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
        .chain(ignore_whitespace.then(|| "--ignore-all-space".to_string()))
        .chain([format!("-U{CONTEXT_LINES}")])
        .collect()
    }

    /// `git diff` of the working tree against `base`, untracked files
    /// included. A diff only lists untracked files once the index knows
    /// them, so this runs against a scratch index holding `base`'s tree
    /// plus intent-to-add entries; the repository's real index stays
    /// untouched.
    fn worktree_diff(
        &self,
        base: &str,
        name_only: bool,
        ignore_whitespace: bool,
    ) -> Result<String> {
        let mut args = Self::diff_args(ignore_whitespace);
        if name_only {
            args.push("--name-only".to_string());
            args.push("-z".to_string());
        }
        args.push("--end-of-options".to_string());
        args.push(base.to_string());
        args.push("--".to_string());
        self.worktree_run(base, &args)
    }

    /// The uncommitted layer as a replayable patch: the working tree against
    /// `base`, in the exact shape `combine` applies commit patches with.
    fn worktree_apply_patch(&self, base: &str) -> Result<String> {
        let args: Vec<String> = [
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--binary",
            "--full-index",
            "--no-renames",
            "--end-of-options",
            base,
            "--",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        self.worktree_run(base, &args)
    }

    /// Runs one git command against a scratch index holding `base`'s tree
    /// plus intent-to-add entries, so a diff lists untracked files too; the
    /// repository's real index stays untouched.
    fn worktree_run(&self, base: &str, args: &[String]) -> Result<String> {
        let scratch = self.scratch_git()?;
        scratch.read(self, &["read-tree", "--end-of-options", base], None)?;
        // `:/` keeps the sweep repository-wide even when the backend was
        // opened from a subdirectory.
        scratch.read(self, &["add", "--intent-to-add", "--", ":/"], None)?;
        scratch.read(self, args, None)
    }

    /// A scratch index and object directory for commands that must stage
    /// entries or write objects. See [`ScratchGit`].
    fn scratch_git(&self) -> Result<ScratchGit> {
        let scratch = Scratch::new()?;
        let objects = scratch.path.join("objects");
        std::fs::create_dir_all(&objects)?;
        let index = scratch.path.join("index");
        let real_objects =
            self.absolute(self.read(&["rev-parse", "--git-path", "objects"])?.trim());
        Ok(ScratchGit {
            index,
            objects,
            real_objects,
            _scratch: scratch,
        })
    }

    fn diff(
        &self,
        from: &str,
        to: &str,
        paths: &[&str],
        ignore_whitespace: bool,
    ) -> Result<String> {
        let mut args = Self::diff_args(ignore_whitespace);
        args.push("--end-of-options".to_string());
        args.push(from.to_string());
        args.push(to.to_string());
        args.push("--".to_string());
        args.extend(paths.iter().map(|path| path.to_string()));
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

    fn new_for_me(
        &self,
        resolved: &Resolved,
        review: ReviewInput<'_>,
        ignore_whitespace: bool,
    ) -> Result<ScopedPatch> {
        let base = resolved.base.as_str();
        let head = resolved.head.as_str();
        let uncommitted = resolved.comparison.uncommitted;
        let whole = if uncommitted {
            self.worktree_diff(base, false, ignore_whitespace)?
        } else {
            self.diff(base, head, &[], ignore_whitespace)?
        };
        let tree = if uncommitted {
            let paths: Vec<String> = split_files(&whole)
                .into_iter()
                .map(|(summary, _)| summary.path)
                .collect();
            self.worktree_blob_ids(&paths)?
        } else {
            self.blob_ids(&resolved.head)?
        };
        let mut text = String::new();
        let mut files = Vec::new();

        for (summary, section) in split_files(&whole) {
            let tick = review
                .reviewed
                .iter()
                .find(|tick| tick.path == summary.path);
            let current = tree.get(summary.path.as_str()).cloned();
            match (tick, current) {
                // Same content as the tick; a tick without a blob matches a
                // file that is still deleted. Open comments overrule the
                // tick: judging a fix is still work, so the file keeps its
                // branch diff and the comments keep rows to sit on.
                (Some(tick), current) if tick.blob == current => {
                    if review.commented.contains(&summary.path) {
                        files.push(FileSummary {
                            state: FileState::KeptForComments,
                            ..summary
                        });
                        text.push_str(section);
                    } else {
                        files.push(FileSummary {
                            state: FileState::UnchangedSinceReview,
                            ..summary
                        });
                    }
                }
                // A worktree hash names no stored object, so an uncommitted
                // review cannot diff since the tick and shows the full file.
                (
                    Some(ReviewedFile {
                        blob: Some(old), ..
                    }),
                    Some(current),
                ) if !uncommitted && self.object_exists(old)? => {
                    let since =
                        self.diff(old.as_str(), current.as_str(), &[], ignore_whitespace)?;
                    let since = relabel_blob_diff(&since, &summary.path);
                    match split_files(&since).into_iter().next() {
                        Some((counted, _)) => {
                            files.push(FileSummary {
                                path: summary.path.clone(),
                                status: FileStatus::Modified,
                                ..counted
                            });
                            text.push_str(&since);
                        }
                        // Hiding whitespace can empty the since-tick diff;
                        // publishing the branch summary instead would fake a
                        // changed file, so collapse like a matching tick.
                        None if review.commented.contains(&summary.path) => {
                            files.push(FileSummary {
                                state: FileState::KeptForComments,
                                ..summary
                            });
                            text.push_str(section);
                        }
                        None => {
                            files.push(FileSummary {
                                state: FileState::UnchangedSinceReview,
                                ..summary
                            });
                        }
                    }
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
            new_side: new_side_of(resolved),
        })
    }

    fn object_exists(&self, id: &Oid) -> Result<bool> {
        Ok(self
            .try_read(&["cat-file", "-e", "--end-of-options", id.as_str()])?
            .is_some())
    }

    /// Replay only `picked` (oldest first) onto the parent of the first one,
    /// file by file, and diff the result. A file that cannot be replayed
    /// cleanly stops at the commit that failed and is reported as such.
    /// With `worktree_on`, the working tree's uncommitted changes (its diff
    /// against that revision) are replayed as one more layer on top.
    fn combine(
        &self,
        picked: &[Oid],
        worktree_on: Option<Oid>,
        ignore_whitespace: bool,
    ) -> Result<ScopedPatch> {
        let scratch = self.scratch_git()?;

        let base = self.parent_of(&picked[0])?;
        scratch.read(self, &["read-tree", "--end-of-options", &base], None)?;

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
                "--end-of-options",
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
                    "--end-of-options",
                    &parent,
                    commit.as_str(),
                    "--",
                    path,
                ])?;
                // A failed three-way apply can leave conflict entries behind.
                let saved = std::fs::read(&scratch.index)?;
                let applied = scratch.read(
                    self,
                    &["apply", "--cached", "--3way", "--whitespace=nowarn", "-"],
                    Some(patch.as_bytes()),
                );
                match applied {
                    Ok(_) => {}
                    Err(BackendError::Git { .. }) => {
                        std::fs::write(&scratch.index, saved)?;
                        failed.insert(path.to_string(), commit.clone());
                    }
                    Err(other) => return Err(other),
                }
            }
        }

        let mut uncommitted_failed: Vec<String> = Vec::new();
        if let Some(head) = worktree_on {
            let layer = self.worktree_apply_patch(head.as_str())?;
            for (summary, section) in split_files(&layer) {
                // A file a commit layer already gave up on keeps that state;
                // its patch stops there either way.
                if failed.contains_key(&summary.path) {
                    continue;
                }
                let saved = std::fs::read(&scratch.index)?;
                let applied = scratch.read(
                    self,
                    &["apply", "--cached", "--3way", "--whitespace=nowarn", "-"],
                    Some(section.as_bytes()),
                );
                match applied {
                    Ok(_) => {}
                    Err(BackendError::Git { .. }) => {
                        std::fs::write(&scratch.index, saved)?;
                        uncommitted_failed.push(summary.path);
                    }
                    Err(other) => return Err(other),
                }
            }
        }

        let tree = scratch.read(self, &["write-tree"], None)?;
        let mut args = Self::diff_args(ignore_whitespace);
        args.push("--end-of-options".to_string());
        args.push(base);
        args.push(tree.trim().to_string());
        let text = scratch.read(self, &args, None)?;

        let mut files: Vec<FileSummary> = split_files(&text)
            .into_iter()
            .map(|(summary, _)| summary)
            .collect();
        let mut mark = |path: String, state: FileState| match files
            .iter_mut()
            .find(|file| file.path == path)
        {
            Some(file) => file.state = state,
            None => files.push(FileSummary {
                path,
                status: FileStatus::Modified,
                additions: 0,
                deletions: 0,
                state,
                binary: false,
            }),
        };
        for (path, commit) in failed {
            mark(path, FileState::CouldNotCombine { commit });
        }
        for path in uncommitted_failed {
            mark(path, FileState::UncommittedNotCombined);
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

    /// The absolute path of `name` inside the git dir. `--git-path` answers
    /// per worktree, so MERGE_HEAD and the rebase state directories are the
    /// ones of the checkout the backend was opened in.
    fn git_path(&self, name: &str) -> Result<PathBuf> {
        Ok(self.absolute(self.read(&["rev-parse", "--git-path", name])?.trim()))
    }

    /// The working tree's top directory; an error on a bare repository.
    fn worktree_root(&self) -> Result<PathBuf> {
        self.try_read(&["rev-parse", "--show-toplevel"])?
            .map(|output| PathBuf::from(output.trim()))
            .ok_or_else(|| BackendError::NotARepository(self.repository.display().to_string()))
    }

    /// `(path, [stage 1, stage 2, stage 3])` for every unmerged index entry,
    /// in git's path order, optionally narrowed to one path.
    fn unmerged_entries(&self, path: Option<&str>) -> Result<Vec<(String, [Option<Oid>; 3])>> {
        let literal = path.map(literal);
        let mut args = vec!["ls-files", "-u", "-z", "--"];
        args.extend(literal.as_deref());
        let output = self.read(&args)?;
        let mut entries: Vec<(String, [Option<Oid>; 3])> = Vec::new();
        // `<mode> <oid> <stage>\t<path>` per entry; -z keeps paths raw, and
        // the entries of one path are adjacent.
        for record in output.split('\0').filter(|record| !record.is_empty()) {
            let Some((meta, path)) = record.split_once('\t') else {
                continue;
            };
            let mut fields = meta.split(' ');
            let (Some(_mode), Some(oid), Some(stage)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let Some(stage) = stage
                .parse::<usize>()
                .ok()
                .filter(|stage| (1..=3).contains(stage))
            else {
                continue;
            };
            if entries.last().map(|(existing, _)| existing.as_str()) != Some(path) {
                entries.push((path.to_string(), [None, None, None]));
            }
            if let Some((_, stages)) = entries.last_mut() {
                stages[stage - 1] = Some(Oid::new(oid));
            }
        }
        Ok(entries)
    }

    /// The pane labels for the interrupted operation. Together with the
    /// stage swap in [`ReviewBackend::conflicts`] this is the one place that
    /// maps git's ours/theirs to yours/upstream; the git terms leave neither.
    fn conflict_labels(&self, rebase: bool) -> Result<ConflictLabels> {
        if rebase {
            Ok(ConflictLabels {
                yours: self.replayed_title()?,
                upstream: self.onto_name()?,
            })
        } else {
            Ok(ConflictLabels {
                yours: self.current_branch()?.unwrap_or_else(|| "HEAD".to_string()),
                upstream: self.merged_name()?,
            })
        }
    }

    /// The name of what a merge is bringing in: the quoted ref of the
    /// prepared MERGE_MSG ("Merge branch 'x' ..."), or the merged commit's
    /// short id when the message names none.
    fn merged_name(&self) -> Result<String> {
        if let Ok(message) = std::fs::read_to_string(self.git_path("MERGE_MSG")?)
            && let Some(first) = message.lines().next()
            && let Some((_, rest)) = first.split_once('\'')
            && let Some((name, _)) = rest.split_once('\'')
            && !name.is_empty()
        {
            return Ok(name.to_string());
        }
        let head = std::fs::read_to_string(self.git_path("MERGE_HEAD")?).unwrap_or_default();
        Ok(Oid::new(head.trim()).short().to_string())
    }

    /// The name of the upstream a rebase replays onto: the name the user
    /// typed, recovered from the reflog's rebase start entry; failing that,
    /// a ref pointing at the recorded onto commit - refs/heads sort before
    /// refs/remotes, so a local branch wins - or that commit's short id.
    fn onto_name(&self) -> Result<String> {
        let onto = ["rebase-merge", "rebase-apply"]
            .iter()
            .find_map(|dir| {
                let state = self.git_path(dir).ok()?;
                std::fs::read_to_string(state.join("onto")).ok()
            })
            .map(|text| text.trim().to_string())
            .unwrap_or_default();
        if onto.is_empty() {
            return Ok("upstream".to_string());
        }
        // Several refs can sit on the onto commit; only the start entry
        // `git rebase <upstream>` wrote knows which one the user meant.
        if let Some(reflog) = self.try_read(&[
            "log",
            "-g",
            "--format=%gs",
            "--end-of-options",
            "HEAD",
            "--",
        ])? && let Some(name) = reflog
            .lines()
            .find_map(|subject| subject.strip_prefix("rebase (start): checkout "))
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            return Ok(name.to_string());
        }
        let points_at = format!("--points-at={onto}");
        Ok(self
            .try_read(&[
                "for-each-ref",
                "--count=1",
                "--format=%(refname:short)",
                &points_at,
                "refs/heads",
                "refs/remotes",
            ])?
            .map(|output| output.trim().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| Oid::new(onto).short().to_string()))
    }

    /// The replayed commit's title: rebase-merge/message, or the subject of
    /// REBASE_HEAD where the apply backend keeps no message file.
    fn replayed_title(&self) -> Result<String> {
        if let Ok(state) = self.git_path("rebase-merge")
            && let Ok(message) = std::fs::read_to_string(state.join("message"))
            && let Some(first) = message.lines().next().map(str::trim)
            && !first.is_empty()
        {
            return Ok(first.to_string());
        }
        Ok(self
            .try_read(&[
                "log",
                "-1",
                "--format=%s",
                "--end-of-options",
                "REBASE_HEAD",
                "--",
            ])?
            .map(|output| output.trim().to_string())
            .filter(|subject| !subject.is_empty())
            .unwrap_or_else(|| "replayed commit".to_string()))
    }

    /// `git merge <action>` or `git rebase <action>`, editors stubbed out so
    /// git can never block waiting for a message or a todo list.
    fn operate(&self, action: &str) -> Result<()> {
        let subcommand = match self.repo_state()? {
            RepoState::Merge => "merge",
            RepoState::Rebase { .. } => "rebase",
            RepoState::Idle => return Err(BackendError::NoOperation),
        };
        let mut command = self.command();
        command
            .env("GIT_EDITOR", "true")
            .env("GIT_SEQUENCE_EDITOR", "true");
        run(&mut command, &[subcommand, action], None)?;
        Ok(())
    }

    fn blame_with(
        &self,
        rev: &Oid,
        path: &str,
        range: Option<usize>,
    ) -> Result<Option<Vec<BlameLine>>> {
        // `git blame` reads an option-shaped revision as an option even
        // behind `--end-of-options` (which also breaks its `--` path
        // separator), so such a value is refused here: not a commit.
        if rev.as_str().starts_with('-') {
            return Ok(None);
        }
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
    fn revision_exists(&self, revision: &str) -> Result<bool> {
        let spec = format!("{revision}^{{commit}}");
        Ok(self
            .try_read(&[
                "rev-parse",
                "--verify",
                "--quiet",
                "--end-of-options",
                &spec,
            ])?
            .is_some())
    }

    fn has_uncommitted_changes(&self) -> Result<bool> {
        // Like the uncommitted layer: untracked counts, ignored does not.
        Ok(!self
            .read(&["status", "--porcelain", "-z", "--no-renames"])?
            .is_empty())
    }

    fn default_branch(&self) -> Result<Option<String>> {
        let mut names: Vec<String> = Vec::new();
        for remote in self.remotes()? {
            // The constant `refs/remotes/` prefix keeps the ref from being
            // read as an option.
            let reference = format!("refs/remotes/{remote}/HEAD");
            let Some(target) = self.try_read(&["symbolic-ref", "--quiet", &reference])? else {
                continue;
            };
            let prefix = format!("refs/remotes/{remote}/");
            if let Some(name) = target.trim().strip_prefix(&prefix)
                && !names.iter().any(|known| known == name)
            {
                names.push(name.to_string());
            }
        }
        Ok(match names.as_slice() {
            [one] => Some(one.clone()),
            // No remote says, or they disagree: no guess.
            _ => None,
        })
    }

    fn remotes(&self) -> Result<Vec<String>> {
        Ok(self
            .read(&["remote"])?
            .lines()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect())
    }

    fn remotes_with_branch(&self, branch: &str) -> Result<Vec<String>> {
        let mut found = Vec::new();
        for remote in self.remotes()? {
            // The constant `refs/remotes/` prefix keeps the ref from being
            // read as an option; `--verify` demands this exact ref.
            let reference = format!("refs/remotes/{remote}/{branch}");
            if self
                .try_read(&["show-ref", "--verify", "--quiet", &reference])?
                .is_some()
            {
                found.push(remote.to_string());
            }
        }
        Ok(found)
    }

    fn fetch(&self, remote: &str, refspecs: &[String]) -> Result<()> {
        let mut args = vec!["fetch", "--quiet", "--end-of-options", remote];
        args.extend(refspecs.iter().map(String::as_str));
        self.read(&args).map(drop)
    }

    fn resolve(&self, comparison: &Comparison) -> Result<Resolved> {
        // An uncommitted review diffs the working tree, so `head` is only
        // the anchor for merge-base and review bookkeeping.
        let head = if comparison.uncommitted {
            self.commit_id("HEAD")?
        } else {
            self.commit_id(
                comparison
                    .source_rev
                    .as_deref()
                    .unwrap_or(&comparison.source),
            )?
        };
        let target_head = self.commit_id(
            comparison
                .target_rev
                .as_deref()
                .unwrap_or(&comparison.target),
        )?;
        let base = match comparison.mode {
            CompareMode::EveryDifference => target_head.clone(),
            CompareMode::BranchAdds => self
                .try_read(&[
                    "merge-base",
                    "--end-of-options",
                    target_head.as_str(),
                    head.as_str(),
                ])?
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
            .as_chunks::<4>()
            .0
            .iter()
            .map(|record| Commit {
                id: Oid::new(record[0].trim()),
                author: record[1].to_string(),
                time: record[2].parse().unwrap_or_default(),
                subject: record[3].to_string(),
            })
            .collect())
    }

    fn changed_paths(&self, resolved: &Resolved) -> Result<Vec<String>> {
        if resolved.comparison.uncommitted {
            // The file list stays complete: hiding whitespace is a view
            // concern and must not unlist a file from scopes or blob maps.
            let output = self.worktree_diff(resolved.base.as_str(), true, false)?;
            return Ok(output
                .split('\0')
                .filter(|path| !path.is_empty())
                .map(String::from)
                .collect());
        }
        // `--find-renames` lists a renamed file once, under its name at
        // `head`, the same name `split_files` summarizes it under.
        let output = self.read(&[
            "diff",
            "--name-only",
            "--find-renames",
            "-z",
            "--end-of-options",
            resolved.base.as_str(),
            resolved.head.as_str(),
            "--",
        ])?;
        Ok(output
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(String::from)
            .collect())
    }

    fn uncommitted_paths(&self, resolved: &Resolved) -> Result<Vec<String>> {
        if !resolved.comparison.uncommitted {
            return Ok(Vec::new());
        }
        let output = self.worktree_diff(resolved.head.as_str(), true, false)?;
        Ok(output
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(String::from)
            .collect())
    }

    fn commits_touching(&self, resolved: &Resolved) -> Result<HashMap<String, Vec<Oid>>> {
        let range = format!("{}..{}", resolved.base, resolved.head);
        // One record per commit: `\x01<hash>\0`, then the touched paths
        // NUL-separated, the first with a leading newline. The `\x01` marks
        // the hash, so a path is never mistaken for one.
        let output = self.read(&[
            "log",
            "--no-show-signature",
            "--name-only",
            "--find-renames",
            "-z",
            "--format=%x01%H",
            &range,
            "--",
        ])?;
        let mut touched: HashMap<String, Vec<Oid>> = HashMap::new();
        for record in output.split('\x01').filter(|record| !record.is_empty()) {
            let mut fields = record.split('\0');
            let Some(hash) = fields.next() else { continue };
            let commit = Oid::new(hash.trim());
            for path in fields {
                let path = path.strip_prefix('\n').unwrap_or(path);
                if path.is_empty() {
                    continue;
                }
                touched
                    .entry(path.to_string())
                    .or_default()
                    .push(commit.clone());
            }
        }
        Ok(touched)
    }

    fn patch(
        &self,
        resolved: &Resolved,
        scope: &Scope,
        review: ReviewInput<'_>,
        ignore_whitespace: bool,
    ) -> Result<ScopedPatch> {
        match scope {
            Scope::NewForMe => self.new_for_me(resolved, review, ignore_whitespace),
            Scope::WholeBranch => {
                let text = if resolved.comparison.uncommitted {
                    self.worktree_diff(resolved.base.as_str(), false, ignore_whitespace)?
                } else {
                    self.diff(
                        resolved.base.as_str(),
                        resolved.head.as_str(),
                        &[],
                        ignore_whitespace,
                    )?
                };
                Ok(patch_from(text, new_side_of(resolved)))
            }
            Scope::Commits {
                picked,
                uncommitted,
            } => {
                let worktree = *uncommitted && resolved.comparison.uncommitted;
                let (ordered, contiguous) = self.in_history_order(resolved, picked)?;
                let (Some(oldest), Some(newest)) = (ordered.first(), ordered.last()) else {
                    if worktree {
                        // Only the uncommitted layer: the working tree
                        // against `head`.
                        let text =
                            self.worktree_diff(resolved.head.as_str(), false, ignore_whitespace)?;
                        return Ok(patch_from(text, NewSide::Synthetic));
                    }
                    return Ok(patch_from(
                        String::new(),
                        NewSide::Commit(resolved.head.clone()),
                    ));
                };
                if contiguous && worktree && *newest == resolved.head {
                    // A run ending at `head` plus the uncommitted layer is
                    // simply the working tree against the run's parent.
                    let text =
                        self.worktree_diff(&self.parent_of(oldest)?, false, ignore_whitespace)?;
                    Ok(patch_from(text, NewSide::Synthetic))
                } else if contiguous && !worktree {
                    let text = self.diff(
                        &self.parent_of(oldest)?,
                        newest.as_str(),
                        &[],
                        ignore_whitespace,
                    )?;
                    Ok(patch_from(text, NewSide::Commit(newest.clone())))
                } else {
                    self.combine(
                        &ordered,
                        worktree.then(|| resolved.head.clone()),
                        ignore_whitespace,
                    )
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

    fn changed_regions(
        &self,
        from: &Oid,
        to: &Oid,
        path: &str,
    ) -> Result<Option<Vec<ChangedRegion>>> {
        // -U0, unlike the view's generous context, because only the rewritten
        // regions matter for mapping a line; context would blur the borders.
        match self.try_read(&[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--histogram",
            "--indent-heuristic",
            "-U0",
            "--end-of-options",
            from.as_str(),
            to.as_str(),
            "--",
            path,
        ])? {
            Some(output) => Ok(Some(parse_regions(&output))),
            None => Ok(None),
        }
    }

    fn blob_id(&self, rev: &Oid, path: &str) -> Result<Option<Oid>> {
        let spec = format!("{rev}:{path}");
        Ok(self
            .try_read(&[
                "rev-parse",
                "--verify",
                "--quiet",
                "--end-of-options",
                &spec,
            ])?
            .map(|output| Oid::new(output.trim())))
    }

    fn blob_ids(&self, rev: &Oid) -> Result<HashMap<String, Oid>> {
        let output = self.read(&[
            "ls-tree",
            "-r",
            "-z",
            "--full-tree",
            "--end-of-options",
            rev.as_str(),
        ])?;
        let mut blobs = HashMap::new();
        // `<mode> <type> <oid>\t<path>`; -z keeps paths raw.
        for entry in output.split('\0') {
            let Some((meta, path)) = entry.split_once('\t') else {
                continue;
            };
            let mut fields = meta.split(' ');
            if let (Some(_), Some("blob"), Some(oid)) =
                (fields.next(), fields.next(), fields.next())
            {
                blobs.insert(path.to_string(), Oid::new(oid));
            }
        }
        Ok(blobs)
    }

    fn worktree_blob_ids(&self, paths: &[String]) -> Result<HashMap<String, Oid>> {
        let top = self.worktree_root()?;
        // One `hash-object` call for every path. Absolute paths keep the
        // call independent of where the backend was opened. `--stdin-paths`
        // is newline-separated, so a path containing one cannot be hashed
        // and reads as absent, like a deletion.
        let present: Vec<&String> = paths
            .iter()
            .filter(|path| !path.contains('\n') && top.join(path).is_file())
            .collect();
        if present.is_empty() {
            return Ok(HashMap::new());
        }
        let list: String = present
            .iter()
            .map(|path| format!("{}\n", top.join(path).display()))
            .collect();
        let args = ["hash-object", "--stdin-paths"];
        let output = run(&mut self.command(), &args, Some(list.as_bytes()))?;
        Ok(present
            .into_iter()
            .zip(output.lines())
            .map(|(path, id)| (path.clone(), Oid::new(id.trim())))
            .collect())
    }

    fn blobs(&self, ids: &[Oid]) -> Result<HashMap<Oid, Vec<u8>>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let list: String = ids.iter().map(|id| format!("{id}\n")).collect();
        let output = run_bytes(
            &mut self.command(),
            &["cat-file", "--batch"],
            Some(list.as_bytes()),
        )?;
        // `<oid> <type> <size>\n<content>\n` per object, `<name> missing\n`
        // for one git does not hold. Sizes count bytes, hence the raw output.
        let mut blobs = HashMap::new();
        let mut rest = output.as_slice();
        while let Some(end) = rest.iter().position(|byte| *byte == b'\n') {
            let header = String::from_utf8_lossy(&rest[..end]).into_owned();
            rest = &rest[end + 1..];
            let mut fields = header.split(' ');
            let (Some(id), Some(kind), Some(size)) = (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let Some(content) = size.parse().ok().and_then(|size: usize| rest.get(..size)) else {
                break;
            };
            if kind == "blob" {
                blobs.insert(Oid::new(id), content.to_vec());
            }
            rest = rest.get(content.len() + 1..).unwrap_or_default();
        }
        Ok(blobs)
    }

    fn worktree_file(&self, path: &str) -> Result<Option<Vec<u8>>> {
        let file = self.worktree_root()?.join(path);
        if !file.is_file() {
            return Ok(None);
        }
        Ok(Some(std::fs::read(file)?))
    }

    fn repo_state(&self) -> Result<RepoState> {
        // The two rebase backends keep separate state directories; both are
        // checked before MERGE_HEAD, which a rebase never leaves behind.
        for (dir, current, last) in [
            ("rebase-merge", "msgnum", "end"),
            ("rebase-apply", "next", "last"),
        ] {
            let state = self.git_path(dir)?;
            if state.is_dir() {
                let number = |file: &str| {
                    std::fs::read_to_string(state.join(file))
                        .ok()
                        .and_then(|text| text.trim().parse().ok())
                        .unwrap_or(0)
                };
                return Ok(RepoState::Rebase {
                    done: number(current),
                    total: number(last),
                });
            }
        }
        if self.git_path("MERGE_HEAD")?.is_file() {
            return Ok(RepoState::Merge);
        }
        Ok(RepoState::Idle)
    }

    fn conflicts(&self) -> Result<Vec<ConflictFile>> {
        let rebase = match self.repo_state()? {
            RepoState::Idle => return Ok(Vec::new()),
            RepoState::Merge => false,
            RepoState::Rebase { .. } => true,
        };
        let labels = self.conflict_labels(rebase)?;
        let entries = self.unmerged_entries(None)?;
        let ids: Vec<Oid> = entries
            .iter()
            .flat_map(|(_, stages)| stages.iter().flatten().cloned())
            .collect();
        let blobs = self.blobs(&ids)?;
        let bytes = |id: &Option<Oid>| id.as_ref().and_then(|id| blobs.get(id));
        let text =
            |id: &Option<Oid>| bytes(id).map(|bytes| String::from_utf8_lossy(bytes).into_owned());
        entries
            .into_iter()
            .map(|(path, [base, ours, theirs])| {
                let working_bytes = self.worktree_file(&path)?.unwrap_or_default();
                // Checked on the raw bytes: the lossy texts read the same
                // whether the original was valid or already corrupted.
                let not_text = |bytes: &[u8]| std::str::from_utf8(bytes).is_err();
                let binary = not_text(&working_bytes)
                    || [&base, &ours, &theirs]
                        .into_iter()
                        .flat_map(&bytes)
                        .any(|blob| not_text(blob));
                let working = String::from_utf8_lossy(&working_bytes).into_owned();
                // In a merge git's stage 2 is the user's own branch; a
                // rebase replays the user's commits onto the upstream, so
                // the stages swap sides. This mapping exists only here.
                let (yours, upstream) = if rebase {
                    (text(&theirs), text(&ours))
                } else {
                    (text(&ours), text(&theirs))
                };
                Ok(ConflictFile {
                    path,
                    base: text(&base),
                    yours,
                    upstream,
                    working,
                    binary,
                    labels: labels.clone(),
                })
            })
            .collect()
    }

    fn resolve_file(&self, path: &str, contents: &str) -> Result<()> {
        // The sanctioned write is narrow: nothing but an unmerged path may
        // be touched, however the caller got here.
        if self.unmerged_entries(Some(path))?.is_empty() {
            return Err(BackendError::NotConflicted(path.to_string()));
        }
        let file = self.worktree_root()?.join(path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(file, contents)?;
        // `--end-of-options` would turn the following `--` into a literal
        // pathspec here; the separator alone keeps the path a path.
        self.read(&["add", "--", &literal(path)])?;
        Ok(())
    }

    fn unresolve_file(&self, path: &str) -> Result<()> {
        // `-m` rebuilds the markers from the resolve-undo data, even after
        // the path was staged. Older gits reject `--end-of-options` here;
        // the `--` separator alone still keeps the path a path.
        let path = literal(path);
        match self.read(&["checkout", "-m", "--end-of-options", "--", &path]) {
            Err(BackendError::Git { stderr, .. }) if stderr.contains("end-of-options") => {
                self.read(&["checkout", "-m", "--", &path]).map(|_| ())
            }
            other => other.map(|_| ()),
        }
    }

    fn continue_op(&self) -> Result<()> {
        self.operate("--continue")
    }

    fn abort_op(&self) -> Result<()> {
        self.operate("--abort")
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

/// `path` as a pathspec matching only itself. Bare pathspecs glob, so a
/// path like `a[b].txt` would match `ab.txt` and miss its own file.
fn literal(path: &str) -> String {
    format!(":(literal){path}")
}

fn run(command: &mut Command, args: &[impl AsRef<str>], stdin: Option<&[u8]>) -> Result<String> {
    run_bytes(command, args, stdin).map(|output| String::from_utf8_lossy(&output).into_owned())
}

/// [`run`] without the text conversion, for output whose byte counts matter.
fn run_bytes(
    command: &mut Command,
    args: &[impl AsRef<str>],
    stdin: Option<&[u8]>,
) -> Result<Vec<u8>> {
    command
        .args(args.iter().map(|argument| argument.as_ref()))
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
        let Some(mut pipe) = child.stdin.take() else {
            // Unreachable with the piped stdin above; refuse, never panic.
            let _ = child.kill();
            return Err(std::io::Error::other("git's stdin pipe was not opened").into());
        };
        let input = input.to_vec();
        std::thread::spawn(move || {
            let _ = pipe.write_all(&input);
        });
    }
    let output = child.wait_with_output()?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(BackendError::Git {
            command: args
                .first()
                .map(|argument| argument.as_ref().to_string())
                .unwrap_or_default(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

/// Uncommitted lines exist in no commit, so blame has nothing to find.
fn new_side_of(resolved: &Resolved) -> NewSide {
    if resolved.comparison.uncommitted {
        NewSide::Synthetic
    } else {
        NewSide::Commit(resolved.head.clone())
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

/// The `@@ -a,b +c,d @@` headers of a `-U0` diff, one region each.
fn parse_regions(patch: &str) -> Vec<ChangedRegion> {
    patch
        .lines()
        .filter_map(crate::patch::hunk_header)
        .collect()
}

/// A scratch git environment: a private index and object directory, with
/// the repository's objects readable as an alternate. Routing every command
/// that stages entries or writes objects through here is what keeps the
/// reviewed repository untouched, so the trio lives in one place.
struct ScratchGit {
    index: PathBuf,
    objects: PathBuf,
    real_objects: PathBuf,
    /// Held for its drop: the directory the index and objects live in.
    _scratch: Scratch,
}

impl ScratchGit {
    fn read(
        &self,
        backend: &GitCli,
        args: &[impl AsRef<str>],
        stdin: Option<&[u8]>,
    ) -> Result<String> {
        let mut command = backend.command();
        command
            .env("GIT_INDEX_FILE", &self.index)
            .env("GIT_OBJECT_DIRECTORY", &self.objects)
            .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", &self.real_objects);
        run(&mut command, args, stdin)
    }
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

    #[test]
    fn parses_hunk_headers_into_regions() {
        let patch = "diff --git a/x b/x\n--- a/x\n+++ b/x\n\
                     @@ -3,0 +4,2 @@ fn context() {\n+a\n+b\n\
                     @@ -10,2 +12,3 @@\n-c\n-d\n+e\n+f\n+g\n";
        assert_eq!(
            parse_regions(patch),
            vec![
                ChangedRegion {
                    old_start: 3,
                    old_lines: 0,
                    new_start: 4,
                    new_lines: 2,
                },
                ChangedRegion {
                    old_start: 10,
                    old_lines: 2,
                    new_start: 12,
                    new_lines: 3,
                },
            ]
        );
        // The single-line short form `@@ -5 +5 @@` means one line each.
        assert_eq!(
            parse_regions("@@ -5 +5 @@\n"),
            vec![ChangedRegion {
                old_start: 5,
                old_lines: 1,
                new_start: 5,
                new_lines: 1,
            }]
        );
    }
}
