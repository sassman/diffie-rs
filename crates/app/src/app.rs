//! The review window.
//!
//! One view owns all state. Repository work happens in background tasks and
//! comes back through [`ReviewApp::install`]; nothing here calls the backend
//! from the render path.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _, StyledExt as _, Theme,
    ThemeMode, TitleBar,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    diff::{
        Diff, DiffAnnotation, DiffEvent, DiffFile, DiffLinePosition, DiffLineRange, DiffMode,
        DiffSide, DiffState,
    },
    h_flex,
    input::{InputEvent, Textarea, TextareaState},
    popover::Popover,
    spinner::Spinner,
    status_bar::StatusBar,
    v_flex,
};
use gpui_kit::{
    Anchor, AnyElement, App, AppContext as _, ClipboardItem, Context, ElementId, Entity,
    Focusable as _, InteractiveElement as _, IntoElement, KeyBinding, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity,
    Window, div, prelude::FluentBuilder as _, px,
};
use review_core::review::{
    NewComment, Placement, ReactionKind, ReviewFile, ReviewSet, ReviewState, Side, as_prompt, place,
};
use review_core::time::short_date;
use review_core::{
    BlameLine, Commit, Comparison, FileState, FileStatus, FileSummary, NewSide, Oid, Resolved,
    ReviewBackend, Scope, ScopedPatch,
};

mod commands {
    gpui_kit::actions!(
        review,
        [
            NextChange,
            PreviousChange,
            NextFile,
            PreviousFile,
            ToggleMode,
            ToggleOtherRevisions,
            ToggleSidebar,
            ScopeNewForMe,
            ScopeWholeBranch,
            ScopeCustom,
            CollapseAll,
            ExpandAll,
            StartComment,
            CancelDraft,
            SubmitDraft,
            MarkReviewed,
            Refresh,
            Quit
        ]
    );
}
use commands::*;

/// Single-letter keys need the diff focused and no comment box open.
///
/// The comment box is rendered inside the Diff (it is an annotation), so the
/// `Diff` context alone is not enough: its bindings would also fire for every
/// letter typed into the box. The root adds `Reading` only while no draft is
/// open.
const DIFF: Option<&str> = Some("Reading > Diff");
const REVIEW: Option<&str> = Some("Review");

pub fn init(cx: &mut App) {
    let is_mac = cx.platform_info().is_mac();
    let (submit, refresh, quit) = if is_mac {
        ("cmd-enter", "cmd-r", "cmd-q")
    } else {
        ("ctrl-enter", "ctrl-r", "ctrl-q")
    };
    cx.bind_keys([
        KeyBinding::new("]", NextChange, DIFF),
        KeyBinding::new("[", PreviousChange, DIFF),
        KeyBinding::new("}", NextFile, DIFF),
        KeyBinding::new("{", PreviousFile, DIFF),
        KeyBinding::new("s", ToggleMode, DIFF),
        KeyBinding::new("o", ToggleOtherRevisions, DIFF),
        KeyBinding::new("t", ToggleSidebar, DIFF),
        KeyBinding::new("1", ScopeNewForMe, DIFF),
        KeyBinding::new("2", ScopeWholeBranch, DIFF),
        KeyBinding::new("3", ScopeCustom, DIFF),
        KeyBinding::new("z shift-m", CollapseAll, DIFF),
        KeyBinding::new("z shift-r", ExpandAll, DIFF),
        KeyBinding::new("c", StartComment, DIFF),
        KeyBinding::new("r", MarkReviewed, DIFF),
        KeyBinding::new("escape", CancelDraft, REVIEW),
        KeyBinding::new(submit, SubmitDraft, REVIEW),
        KeyBinding::new(refresh, Refresh, REVIEW),
        KeyBinding::new(quit, Quit, REVIEW),
    ]);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScopeKind {
    NewForMe,
    WholeBranch,
    Custom,
}

/// Where a comment sits in the current view.
#[derive(Clone)]
struct Placed {
    id: String,
    path: String,
    side: Side,
    line: usize,
    other_revision: bool,
}

struct Draft {
    range: DiffLineRange,
    input: Entity<TextareaState>,
    _submit: Subscription,
}

/// Everything one background load produces.
struct Loaded {
    resolved: Resolved,
    commits: Vec<Commit>,
    reviews: ReviewSet,
    review_path: PathBuf,
    patch: ScopedPatch,
    files: Vec<DiffFile>,
    blobs: HashMap<String, Option<Oid>>,
    placed: Vec<Placed>,
}

/// What annotation and header callbacks may read. They receive an `App`, not
/// this view, so they get a snapshot built during render.
struct Snapshot {
    threads: Vec<ThreadView>,
    files: HashMap<SharedString, FileView>,
    draft: Option<(Entity<TextareaState>, SharedString)>,
}

struct ThreadView {
    element: ElementId,
    id: String,
    author: SharedString,
    commit: SharedString,
    body: SharedString,
    selection: Option<SharedString>,
    other_revision: bool,
    mine: bool,
    resolved: bool,
    /// Kind, how many reviewers gave it, whether this reviewer did.
    reactions: Vec<(ReactionKind, usize, bool)>,
    left: Vec<ReactionKind>,
}

struct FileView {
    state: ReviewState,
    note: Option<SharedString>,
}

pub struct ReviewApp {
    backend: Arc<dyn ReviewBackend>,
    repository: SharedString,
    comparison: Comparison,
    scope: ScopeKind,
    picked: Vec<Oid>,
    resolved: Option<Resolved>,
    commits: Arc<Vec<Commit>>,
    files: Arc<Vec<FileSummary>>,
    new_side: Option<NewSide>,
    blobs: HashMap<String, Option<Oid>>,
    reviews: Option<ReviewSet>,
    review_path: PathBuf,
    placed: Vec<Placed>,
    diff: Entity<DiffState>,
    draft: Option<Draft>,
    current_file: Option<SharedString>,
    show_other_revisions: bool,
    sidebar_visible: bool,
    loading: bool,
    error: Option<SharedString>,
    revision: usize,
    _load: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl ReviewApp {
    pub fn new(
        backend: Arc<dyn ReviewBackend>,
        repository: SharedString,
        comparison: Comparison,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        window.set_window_title(&format!("{repository} — review"));
        Theme::change(ThemeMode::Dark, Some(window), cx);
        let diff = cx.new(|cx| DiffState::new([], cx).with_mode(DiffMode::Split));
        let subscriptions = vec![
            cx.observe(&diff, |_, _, cx| cx.notify()),
            cx.subscribe(&diff, |this, _, event: &DiffEvent, cx| {
                if let DiffEvent::SelectionChanged(Some(range)) = event {
                    this.current_file = Some(range.path().clone());
                    cx.notify();
                }
            }),
        ];
        diff.read(cx).focus_handle(cx).focus(window, cx);
        let mut this = Self {
            backend,
            repository,
            comparison,
            scope: ScopeKind::NewForMe,
            picked: Vec::new(),
            resolved: None,
            commits: Arc::default(),
            files: Arc::default(),
            new_side: None,
            blobs: HashMap::new(),
            reviews: None,
            review_path: PathBuf::new(),
            placed: Vec::new(),
            diff,
            draft: None,
            current_file: None,
            // Off by default: the diff shows only comments that belong to the scope.
            show_other_revisions: false,
            sidebar_visible: true,
            loading: false,
            error: None,
            revision: 0,
            _load: Task::ready(()),
            _subscriptions: subscriptions,
        };
        this.reload(cx);
        this
    }

    // ---- loading -------------------------------------------------------

    fn core_scope(&self) -> Scope {
        match self.scope {
            ScopeKind::NewForMe => Scope::NewForMe,
            ScopeKind::WholeBranch => Scope::WholeBranch,
            ScopeKind::Custom => Scope::Commits(self.picked.clone()),
        }
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.revision += 1;
        let revision = self.revision;
        self.loading = true;
        self.error = None;
        self.draft = None;
        let backend = self.backend.clone();
        let comparison = self.comparison.clone();
        let scope = self.core_scope();
        let mine = self.reviews.as_ref().map(|set| set.mine.clone());
        self._load = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { load(&*backend, &comparison, &scope, mine) })
                .await;
            let _ = this.update(cx, |this, cx| this.install(revision, result, cx));
        });
        cx.notify();
    }

    fn install(&mut self, revision: usize, result: Result<Loaded, String>, cx: &mut Context<Self>) {
        // A newer request is on its way; this answer is stale.
        if self.revision != revision {
            return;
        }
        self.loading = false;
        match result {
            Ok(loaded) => {
                let reviewed: Vec<String> = loaded
                    .patch
                    .files
                    .iter()
                    .filter(|file| {
                        let blob = loaded.blobs.get(&file.path).and_then(Option::as_ref);
                        loaded.reviews.mine.review_state(&file.path, blob) == ReviewState::Reviewed
                    })
                    .map(|file| file.path.clone())
                    .collect();
                self.diff.update(cx, |state, cx| {
                    state.set_files(loaded.files, cx);
                    for path in &reviewed {
                        state.set_file_collapsed(path, true, cx);
                    }
                });
                self.resolved = Some(loaded.resolved);
                self.commits = Arc::new(loaded.commits);
                self.files = Arc::new(loaded.patch.files);
                self.new_side = Some(loaded.patch.new_side);
                self.blobs = loaded.blobs;
                self.reviews = Some(loaded.reviews);
                self.review_path = loaded.review_path;
                self.placed = loaded.placed;
                self.current_file = None;
            }
            Err(error) => {
                self.diff.update(cx, |state, cx| state.set_files([], cx));
                self.files = Arc::default();
                self.placed.clear();
                self.error = Some(error.into());
            }
        }
        cx.notify();
    }

    /// Persist the reviewer's own file off the main thread.
    fn save_review(&mut self, cx: &mut Context<Self>) {
        let Some(mut file) = self.reviews.as_ref().map(|set| set.mine.clone()) else {
            return;
        };
        let path = self.review_path.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(
                    async move { file.save(&path).map_err(|error| error.to_string()) },
                )
                .await;
            if let Err(error) = result {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(error.into());
                    cx.notify();
                });
            }
        })
        .detach();
    }

    // ---- scope and commits ---------------------------------------------

    fn set_scope(&mut self, scope: ScopeKind, cx: &mut Context<Self>) {
        if self.scope == scope {
            return;
        }
        // An empty custom scope shows nothing; start it from every commit.
        if scope == ScopeKind::Custom && self.picked.is_empty() {
            self.picked = self
                .commits
                .iter()
                .map(|commit| commit.id.clone())
                .collect();
        }
        self.scope = scope;
        self.reload(cx);
    }

    fn toggle_commit(&mut self, id: &Oid, cx: &mut Context<Self>) {
        match self.picked.iter().position(|picked| picked == id) {
            Some(index) => {
                self.picked.remove(index);
            }
            None => self.picked.push(id.clone()),
        }
        self.scope = ScopeKind::Custom;
        self.reload(cx);
    }

    fn pick_many(&mut self, ids: Vec<Oid>, checked: bool, cx: &mut Context<Self>) {
        self.picked.retain(|picked| !ids.contains(picked));
        if checked {
            self.picked.extend(ids);
        }
        self.scope = ScopeKind::Custom;
        self.reload(cx);
    }

    fn swap_direction(&mut self, cx: &mut Context<Self>) {
        std::mem::swap(&mut self.comparison.source, &mut self.comparison.target);
        self.picked.clear();
        self.scope = ScopeKind::NewForMe;
        // The review file belongs to the other direction.
        self.reviews = None;
        self.reload(cx);
    }

    /// Index of the first commit that was already covered by the last
    /// completed review. Commits before it are new for this reviewer.
    fn reviewed_from(&self) -> Option<usize> {
        let completed = self.reviews.as_ref()?.mine.completed.as_ref()?;
        self.commits
            .iter()
            .position(|commit| commit.id == completed.head)
    }

    fn in_scope(&self) -> HashSet<Oid> {
        match self.scope {
            ScopeKind::Custom => self.picked.iter().cloned().collect(),
            _ => self
                .commits
                .iter()
                .map(|commit| commit.id.clone())
                .collect(),
        }
    }

    // ---- files -----------------------------------------------------------

    fn review_state(&self, path: &str) -> ReviewState {
        let blob = self.blobs.get(path).and_then(Option::as_ref);
        self.reviews
            .as_ref()
            .map_or(ReviewState::NotReviewed, |set| {
                set.mine.review_state(path, blob)
            })
    }

    fn set_reviewed(&mut self, path: &str, reviewed: bool, cx: &mut Context<Self>) {
        let (Some(set), Some(resolved)) = (self.reviews.as_mut(), self.resolved.as_ref()) else {
            return;
        };
        if reviewed {
            // A deleted file has no content to pin the tick to.
            let Some(blob) = self.blobs.get(path).cloned().flatten() else {
                return;
            };
            set.mine.mark_reviewed(path, blob, resolved.head.clone());
            set.mine.complete_if_all_reviewed(
                self.files.iter().map(|file| {
                    (
                        file.path.as_str(),
                        self.blobs.get(&file.path).and_then(Option::as_ref),
                    )
                }),
                &resolved.head,
            );
        } else {
            set.mine.clear_reviewed(path);
        }
        self.diff
            .update(cx, |state, cx| state.set_file_collapsed(path, reviewed, cx));
        self.save_review(cx);
        cx.notify();
    }

    fn reveal_file(&mut self, path: SharedString, cx: &mut Context<Self>) {
        self.diff.update(cx, |state, cx| {
            state.set_file_collapsed(&path, false, cx);
            state.scroll_to_file(&path, cx);
        });
        self.current_file = Some(path);
        cx.notify();
    }

    fn move_file(&mut self, direction: isize, cx: &mut Context<Self>) {
        let paths: Vec<SharedString> = self
            .diff
            .read(cx)
            .files()
            .iter()
            .map(|file| file.path().clone())
            .collect();
        if paths.is_empty() {
            return;
        }
        let current = self
            .current_file
            .as_ref()
            .and_then(|path| paths.iter().position(|candidate| candidate == path));
        let next = match current {
            Some(index) => index.saturating_add_signed(direction).min(paths.len() - 1),
            None => 0,
        };
        self.reveal_file(paths[next].clone(), cx);
    }

    fn set_all_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        self.diff.update(cx, |state, cx| {
            let paths: Vec<SharedString> = state
                .files()
                .iter()
                .map(|file| file.path().clone())
                .collect();
            for path in paths {
                state.set_file_collapsed(&path, collapsed, cx);
            }
        });
    }

    fn toggle_mode(&mut self, cx: &mut Context<Self>) {
        self.diff.update(cx, |state, cx| {
            let next = match state.mode() {
                DiffMode::Split => DiffMode::Unified,
                _ => DiffMode::Split,
            };
            state.set_mode(next, cx);
        });
    }

    // ---- comments --------------------------------------------------------

    fn start_comment(&mut self, range: DiffLineRange, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 10)
                .placeholder("Write a comment…")
        });
        // Focus once the box is in the element tree. Focusing right away works
        // from a click but not from the keyboard shortcut.
        cx.on_next_frame(window, |this, window, cx| {
            if let Some(draft) = &this.draft {
                draft.input.update(cx, |state, cx| state.focus(window, cx));
            }
        });
        // The box consumes Cmd/Ctrl+Enter itself and reports it as an event; a
        // key binding on the window never sees it.
        let submit = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter {
                secondary: true, ..
            } = event
            {
                this.submit_draft(window, cx);
            }
        });
        self.draft = Some(Draft {
            range,
            input,
            _submit: submit,
        });
        cx.notify();
    }

    fn focus_diff(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.diff.read(cx).focus_handle(cx).focus(window, cx);
    }

    fn cancel_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.draft = None;
        self.focus_diff(window, cx);
        cx.notify();
    }

    fn submit_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_diff(window, cx);
        self.submit_draft_inner(cx);
    }

    fn submit_draft_inner(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.draft.take() else {
            return;
        };
        // Cmd/Ctrl+Enter can leave a trailing newline in the box.
        let body = draft.input.read(cx).value().trim().to_string();
        if body.is_empty() {
            self.draft = Some(draft);
            return;
        }
        let (Some(new_side), Some(resolved)) = (self.new_side.clone(), self.resolved.clone())
        else {
            return;
        };
        let range = draft.range;
        let snippet = self.diff.update(cx, |state, cx| {
            state.set_selected_lines(Some(range.clone()), cx);
            let text = state.selected_text(cx);
            state.set_selected_lines(None, cx);
            text
        });
        let path = range.path().to_string();
        let side = if range.end_side() == DiffSide::Original {
            Side::Old
        } else {
            Side::New
        };
        let line = range.end();
        let lines = if range.side() == range.end_side() {
            range.end().abs_diff(range.start()) + 1
        } else {
            1
        };
        let in_scope = self.in_scope();
        // Used when blame cannot answer: removed lines and combined views.
        let fallback = self
            .commits
            .iter()
            .find(|commit| in_scope.contains(&commit.id))
            .map_or(resolved.head.clone(), |commit| commit.id.clone());
        let backend = self.backend.clone();
        let blame_path = path.clone();

        cx.spawn(async move |this, cx| {
            let origin = cx
                .background_spawn(async move {
                    match (&new_side, side) {
                        (NewSide::Commit(rev), Side::New) => backend
                            .line_origin(rev, &blame_path, line)
                            .map_err(|error| error.to_string()),
                        _ => Ok(None),
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let origin = match origin {
                    Ok(origin) => origin,
                    Err(error) => {
                        this.error = Some(error.into());
                        cx.notify();
                        return;
                    }
                };
                let (commit, anchored_line) =
                    origin.map_or((fallback, line), |origin| (origin.commit, origin.line));
                let other_revision = !in_scope.contains(&commit);
                let Some(set) = this.reviews.as_mut() else {
                    return;
                };
                let id = set.mine.add_comment(NewComment {
                    commit,
                    path: path.clone(),
                    side,
                    line: anchored_line,
                    lines,
                    selection: None,
                    snippet,
                    body,
                    suggestion: None,
                });
                this.placed.push(Placed {
                    id,
                    path,
                    side,
                    line,
                    other_revision,
                });
                this.save_review(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn toggle_reaction(&mut self, id: &str, kind: ReactionKind, cx: &mut Context<Self>) {
        if let Some(set) = self.reviews.as_mut() {
            set.mine.toggle_reaction(id, kind);
            self.save_review(cx);
            cx.notify();
        }
    }

    fn delete_comment(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(set) = self.reviews.as_mut()
            && set.mine.delete_comment(id)
        {
            self.placed.retain(|placed| placed.id != id);
            self.save_review(cx);
            cx.notify();
        }
    }

    fn copy_all_as_prompt(&self, cx: &mut Context<Self>) {
        if let Some(set) = &self.reviews {
            let open: Vec<_> = set
                .threads()
                .into_iter()
                .filter(|thread| !thread.resolved())
                .collect();
            cx.write_to_clipboard(ClipboardItem::new_string(as_prompt(&open)));
        }
    }

    // ---- rendering -------------------------------------------------------

    fn snapshot(&self) -> Snapshot {
        let mut threads = Vec::new();
        if let Some(set) = &self.reviews {
            for thread in set.threads() {
                let Some(placed) = self
                    .placed
                    .iter()
                    .find(|placed| placed.id == thread.comment.id)
                else {
                    continue;
                };
                if placed.other_revision && !self.show_other_revisions {
                    continue;
                }
                let reactions = ReactionKind::ALL
                    .into_iter()
                    .filter_map(|kind| {
                        let count = thread.count(kind);
                        let mine = set
                            .mine
                            .reactions
                            .iter()
                            .any(|r| r.comment == thread.comment.id && r.kind == kind);
                        (count > 0).then_some((kind, count, mine))
                    })
                    .collect();
                threads.push(ThreadView {
                    element: ElementId::from(SharedString::from(thread.comment.id.clone())),
                    id: thread.comment.id.clone(),
                    author: thread.author.to_string().into(),
                    commit: thread.comment.commit.short().to_string().into(),
                    body: thread.comment.body.clone().into(),
                    selection: thread.comment.selection.clone().map(Into::into),
                    other_revision: placed.other_revision,
                    mine: thread.mine,
                    resolved: thread.resolved(),
                    reactions,
                    left: set.mine.reactions_left(&thread.comment.id),
                });
            }
        }
        let files = self
            .files
            .iter()
            .map(|file| {
                let note = match &file.state {
                    FileState::CouldNotCombine { commit } => Some(
                        format!(
                            "stops before {}: a skipped commit touches the same lines",
                            commit.short()
                        )
                        .into(),
                    ),
                    _ if self.review_state(&file.path) == ReviewState::ChangedSince => {
                        Some("changed since your review".into())
                    }
                    _ => None,
                };
                (
                    SharedString::from(file.path.clone()),
                    FileView {
                        state: self.review_state(&file.path),
                        note,
                    },
                )
            })
            .collect();
        let draft = self.draft.as_ref().map(|draft| {
            let range = &draft.range;
            let label = if range.start() == range.end() {
                format!("New comment on line {}", range.end())
            } else {
                format!("New comment on lines {}–{}", range.start(), range.end())
            };
            (draft.input.clone(), label.into())
        });
        Snapshot {
            threads,
            files,
            draft,
        }
    }

    fn annotations(&self, snapshot: &Snapshot) -> Vec<DiffAnnotation> {
        let mut annotations: Vec<DiffAnnotation> = snapshot
            .threads
            .iter()
            .filter_map(|thread| {
                let placed = self.placed.iter().find(|placed| placed.id == thread.id)?;
                let side = match placed.side {
                    Side::Old => DiffSide::Original,
                    Side::New => DiffSide::Modified,
                };
                Some(DiffAnnotation::line(
                    thread.element.clone(),
                    DiffLinePosition::new(placed.path.clone(), side, placed.line),
                ))
            })
            .collect();
        if let Some(draft) = &self.draft {
            let range = &draft.range;
            annotations.push(DiffAnnotation::line(
                "draft",
                DiffLinePosition::new(range.path().clone(), range.end_side(), range.end()),
            ));
        }
        annotations
    }

    fn render_title(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mono = cx.theme().mono_font_family.clone();
        let muted = cx.theme().muted_foreground;
        let chip = |text: &str| {
            div()
                .px_1p5()
                .rounded_sm()
                .border_1()
                .border_color(cx.theme().border)
                .font_family(mono.clone())
                .text_xs()
                .child(text.to_string())
        };
        let split = self.diff.read(cx).mode() == DiffMode::Split;
        TitleBar::new().child(
            h_flex()
                .size_full()
                .px_1()
                .pr_3()
                .gap_2()
                .text_xs()
                .child(div().text_sm().font_medium().child(self.repository.clone()))
                .child(div().text_color(muted).child("changes in"))
                .child(chip(&self.comparison.source))
                .child(div().text_color(muted).child("going into"))
                .child(chip(&self.comparison.target))
                .child(
                    Button::new("swap")
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(Lucide::ArrowLeftRight))
                        .tooltip("Swap direction")
                        .on_click(cx.listener(|this, _, _, cx| this.swap_direction(cx))),
                )
                .child(div().text_color(muted).child("only what the branch adds"))
                .child(div().flex_1())
                .child(
                    Button::new("split")
                        .ghost()
                        .xsmall()
                        .label("Split")
                        .selected(split)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.diff
                                .update(cx, |state, cx| state.set_mode(DiffMode::Split, cx))
                        })),
                )
                .child(
                    Button::new("unified")
                        .ghost()
                        .xsmall()
                        .label("Unified")
                        .selected(!split)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.diff
                                .update(cx, |state, cx| state.set_mode(DiffMode::Unified, cx))
                        })),
                )
                .child(
                    Button::new("collapse-all")
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(Lucide::ChevronsDownUp))
                        .tooltip("Collapse all files")
                        .on_click(cx.listener(|this, _, _, cx| this.set_all_collapsed(true, cx))),
                )
                .child(
                    Button::new("expand-all")
                        .ghost()
                        .xsmall()
                        .icon(IconName::ChevronsUpDown)
                        .tooltip("Expand all files")
                        .on_click(cx.listener(|this, _, _, cx| this.set_all_collapsed(false, cx))),
                )
                .child(
                    Button::new("other-revisions")
                        .ghost()
                        .xsmall()
                        .label("Other revisions' comments")
                        .selected(self.show_other_revisions)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_other_revisions = !this.show_other_revisions;
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("copy-prompt")
                        .ghost()
                        .xsmall()
                        .label("Copy all as prompt")
                        .on_click(cx.listener(|this, _, _, cx| this.copy_all_as_prompt(cx))),
                )
                .child(
                    Button::new("theme")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Palette)
                        .tooltip("Switch theme")
                        .on_click(|_, window, cx| {
                            let next = if cx.theme().is_dark() {
                                ThemeMode::Light
                            } else {
                                ThemeMode::Dark
                            };
                            Theme::change(next, Some(window), cx);
                        }),
                ),
        )
    }

    fn render_commit_row(&self, commit: &Commit, head: bool, cx: &mut Context<Self>) -> AnyElement {
        let id = commit.id.clone();
        let checked = self.picked.contains(&commit.id) || self.scope != ScopeKind::Custom;
        let mut meta = format!("{} · {}", commit.id.short(), short_date(commit.time));
        if head {
            meta.push_str(" · HEAD");
        }
        h_flex()
            .w_full()
            .items_start()
            .gap_2()
            .px_1()
            .py_0p5()
            .child(
                Checkbox::new(SharedString::from(format!("pick-{}", commit.id)))
                    .xsmall()
                    .checked(checked)
                    .on_change(cx.listener(move |this, _, _, cx| {
                        // Leaving a non-custom scope starts from "all checked".
                        if this.scope != ScopeKind::Custom {
                            this.picked = this
                                .commits
                                .iter()
                                .map(|commit| commit.id.clone())
                                .collect();
                        }
                        this.toggle_commit(&id, cx);
                    })),
            )
            .child(
                v_flex()
                    .min_w_0()
                    .flex_1()
                    .child(
                        div()
                            .text_xs()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_color(cx.theme().muted_foreground)
                            .child(meta),
                    )
                    .child(div().text_xs().truncate().child(commit.subject.clone())),
            )
            .into_any_element()
    }

    fn render_scope_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let total = self.commits.len();
        let muted = cx.theme().muted_foreground;
        let label = match (self.scope, &self.resolved) {
            (ScopeKind::Custom, _) => {
                let positions: Vec<usize> = self
                    .commits
                    .iter()
                    .enumerate()
                    .filter(|(_, commit)| self.picked.contains(&commit.id))
                    .map(|(index, _)| index)
                    .collect();
                match (positions.first(), positions.last()) {
                    (Some(newest), Some(oldest)) => {
                        let skipped = oldest - newest + 1 - positions.len();
                        let mut label = format!(
                            "{} of {total} commits · {} … {}",
                            positions.len(),
                            self.commits[*oldest].id.short(),
                            self.commits[*newest].id.short()
                        );
                        if skipped > 0 {
                            label.push_str(&format!(" · {skipped} skipped"));
                        }
                        label
                    }
                    _ => format!("0 of {total} commits"),
                }
            }
            (_, Some(resolved)) => format!(
                "{total} commits · {} → {}",
                resolved.base.short(),
                resolved.head.short()
            ),
            _ => "No commits".to_string(),
        };

        let split_at = self.reviewed_from().unwrap_or(total);
        let group = |title: &'static str, range: std::ops::Range<usize>, cx: &mut Context<Self>| {
            let ids: Vec<Oid> = self.commits[range.clone()]
                .iter()
                .map(|commit| commit.id.clone())
                .collect();
            // Outside the custom scope every commit counts as checked.
            let checked = if self.scope == ScopeKind::Custom {
                ids.iter().filter(|id| self.picked.contains(id)).count()
            } else {
                ids.len()
            };
            let all = checked == ids.len() && !ids.is_empty();
            h_flex()
                .w_full()
                .gap_2()
                .px_1()
                .child(
                    Checkbox::new(title)
                        .xsmall()
                        .checked(all)
                        .on_change(cx.listener(move |this, checked: &bool, _, cx| {
                            this.pick_many(ids.clone(), *checked, cx)
                        })),
                )
                .child(div().flex_1().text_xs().font_medium().child(title))
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("{checked} of {}", range.len())),
                )
        };

        let mut list = v_flex().w_full().gap_0p5();
        if split_at > 0 {
            list = list.child(group("New since your review", 0..split_at, cx));
            for (index, commit) in self.commits[..split_at].iter().enumerate() {
                list = list.child(self.render_commit_row(commit, index == 0, cx));
            }
        }
        if split_at < total {
            list = list.child(group("Already reviewed", split_at..total, cx));
            for (index, commit) in self.commits[split_at..].iter().enumerate() {
                list = list.child(self.render_commit_row(commit, split_at + index == 0, cx));
            }
        }

        let scope_button =
            |id: &'static str, text: &'static str, kind: ScopeKind, cx: &mut Context<Self>| {
                Button::new(id)
                    .ghost()
                    .xsmall()
                    .label(text)
                    .selected(self.scope == kind)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_scope(kind, cx)))
            };
        let reviewed = self
            .files
            .iter()
            .filter(|file| self.review_state(&file.path) == ReviewState::Reviewed)
            .count();
        let explanation = match self.scope {
            ScopeKind::NewForMe => "Each file is compared with the version you last ticked",
            ScopeKind::WholeBranch => "Everything the branch adds",
            ScopeKind::Custom => "Combined diff of the checked commits",
        };

        h_flex()
            .flex_none()
            .w_full()
            .px_2()
            .py_1()
            .gap_2()
            .text_xs()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(div().text_color(muted).child("SCOPE"))
            .child(scope_button(
                "scope-new",
                "New for me",
                ScopeKind::NewForMe,
                cx,
            ))
            .child(scope_button(
                "scope-whole",
                "Whole branch",
                ScopeKind::WholeBranch,
                cx,
            ))
            .child(scope_button(
                "scope-custom",
                "Custom",
                ScopeKind::Custom,
                cx,
            ))
            .child(
                Popover::new("commit-picker")
                    .anchor(Anchor::TopLeft)
                    .trigger(
                        Button::new("commit-range")
                            .outline()
                            .xsmall()
                            .label(label)
                            .icon(IconName::ChevronDown),
                    )
                    .child(
                        div()
                            .id("commit-list")
                            .w(px(340.))
                            .max_h(px(440.))
                            .overflow_y_scroll()
                            .child(list),
                    ),
            )
            .child(div().text_color(muted).child(explanation))
            .child(div().flex_1())
            .child(format!("{reviewed} of {} files reviewed", self.files.len()))
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let mono = cx.theme().mono_font_family.clone();
        let mut list = v_flex().w_full();
        for file in self.files.iter() {
            let path = SharedString::from(file.path.clone());
            let (directory, name) = file.path.rsplit_once('/').unwrap_or(("", &file.path));
            let status = match file.status {
                FileStatus::Added => "A",
                FileStatus::Deleted => "D",
                FileStatus::Modified => "M",
                FileStatus::Renamed => "R",
            };
            let comments = self
                .placed
                .iter()
                .filter(|placed| placed.path == file.path)
                .count();
            let state = self.review_state(&file.path);
            let unchanged = file.state == FileState::UnchangedSinceReview;
            let selected = self.current_file.as_ref() == Some(&path);
            let target = path.clone();
            list = list.child(
                h_flex()
                    .id(path.clone())
                    .w_full()
                    .h(px(22.))
                    .px_2()
                    .gap_1p5()
                    .text_xs()
                    .rounded_sm()
                    .cursor_pointer()
                    .when(selected, |row| row.bg(cx.theme().accent))
                    .when(state == ReviewState::Reviewed || unchanged, |row| {
                        row.text_color(muted)
                    })
                    .hover(|row| row.bg(cx.theme().accent))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.reveal_file(target.clone(), cx)),
                    )
                    .child(
                        div()
                            .font_family(mono.clone())
                            .text_color(muted)
                            .child(status),
                    )
                    .child(div().flex_none().child(name.to_string()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(muted)
                            .child(directory.to_string()),
                    )
                    .when(comments > 0, |row| {
                        row.child(
                            div()
                                .px_1()
                                .rounded_full()
                                .bg(cx.theme().warning)
                                .text_color(cx.theme().warning_foreground)
                                .child(comments.to_string()),
                        )
                    })
                    .when(state == ReviewState::Reviewed, |row| {
                        row.child(Icon::new(IconName::Check).xsmall())
                    })
                    .when(unchanged, |row| row.child("no changes"))
                    .when(!unchanged && !file.binary, |row| {
                        row.child(
                            div()
                                .font_family(mono.clone())
                                .text_color(muted)
                                .child(format!("+{} −{}", file.additions, file.deletions)),
                        )
                    }),
            );
        }
        v_flex()
            .w(px(260.))
            .h_full()
            .flex_none()
            .border_r_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().sidebar)
            .child(
                h_flex()
                    .h(px(24.))
                    .px_2()
                    .text_xs()
                    .text_color(muted)
                    .child(format!("FILES · {}", self.files.len())),
            )
            .child(
                div()
                    .id("file-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(list),
            )
    }

    fn render_diff(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(error) = &self.error {
            return div()
                .size_full()
                .p_4()
                .text_sm()
                .text_color(cx.theme().danger)
                .child(error.clone())
                .into_any_element();
        }
        if self.loading && self.files.is_empty() {
            return h_flex()
                .size_full()
                .justify_center()
                .gap_2()
                .text_sm()
                .child(Spinner::new())
                .child("Reading the repository")
                .into_any_element();
        }
        if self.files.is_empty() {
            return h_flex()
                .size_full()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Nothing to review in this scope")
                .into_any_element();
        }

        let snapshot = Rc::new(self.snapshot());
        let annotations = self.annotations(&snapshot);
        let owner = cx.entity().downgrade();
        let for_annotations = (snapshot.clone(), owner.clone());
        let for_header = (snapshot.clone(), owner.clone());

        Diff::new(&self.diff)
            .size_full()
            // Without wrapping, a long line widens the whole list and pushes the
            // header's right-hand controls out of view.
            .soft_wrap(true)
            .annotations(annotations)
            .render_annotation(move |annotation, _, cx| {
                let (snapshot, owner) = &for_annotations;
                match snapshot
                    .threads
                    .iter()
                    .find(|thread| &thread.element == annotation.id())
                {
                    Some(thread) => render_thread(thread, owner.clone(), cx),
                    None => render_draft(snapshot, owner.clone(), cx),
                }
            })
            .on_add_annotation(move |range, window, cx| {
                let range = range.clone();
                let _ = owner.update(cx, |this, cx| this.start_comment(range, window, cx));
            })
            .render_header_suffix(move |file, _, cx| {
                let (snapshot, owner) = &for_header;
                let path = file.path().clone();
                let view = snapshot.files.get(&path);
                let reviewed = view.is_some_and(|view| view.state == ReviewState::Reviewed);
                let note = view.and_then(|view| view.note.clone());
                let owner = owner.clone();
                h_flex()
                    .gap_2()
                    .text_xs()
                    .when_some(note, |row, note| {
                        row.child(div().text_color(cx.theme().warning).child(note))
                    })
                    .child(
                        Checkbox::new(SharedString::from(format!("reviewed-{path}")))
                            .label("Reviewed")
                            .xsmall()
                            .checked(reviewed)
                            .on_change(move |checked, _, cx| {
                                let _ = owner
                                    .update(cx, |this, cx| this.set_reviewed(&path, *checked, cx));
                            }),
                    )
            })
            .into_any_element()
    }
}

fn render_thread(thread: &ThreadView, owner: WeakEntity<ReviewApp>, cx: &mut App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let id = thread.id.clone();

    let mut actions = h_flex().w_full().gap_1();
    for (kind, count, mine) in &thread.reactions {
        let (kind, owner, id) = (*kind, owner.clone(), id.clone());
        actions = actions.child(
            Button::new(SharedString::from(format!(
                "reaction-{id}-{}",
                kind.label()
            )))
            .ghost()
            .xsmall()
            .label(format!("{} {count}", kind.emoji()))
            .tooltip(kind.label())
            .selected(*mine)
            .on_click(move |_, _, cx| {
                let _ = owner.update(cx, |this, cx| this.toggle_reaction(&id, kind, cx));
            }),
        );
    }
    if !thread.left.is_empty() {
        let mut picker = v_flex().gap_0p5();
        for kind in thread.left.iter().copied() {
            let (owner, id) = (owner.clone(), id.clone());
            picker = picker.child(
                Button::new(SharedString::from(format!("react-{id}-{}", kind.label())))
                    .ghost()
                    .xsmall()
                    .label(format!("{} {}", kind.emoji(), kind.label()))
                    .on_click(move |_, _, cx| {
                        let _ = owner.update(cx, |this, cx| this.toggle_reaction(&id, kind, cx));
                    }),
            );
        }
        actions = actions.child(
            Popover::new(SharedString::from(format!("react-menu-{id}")))
                .anchor(Anchor::TopLeft)
                .trigger(
                    Button::new(SharedString::from(format!("react-open-{id}")))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Plus)
                        .tooltip("Add reaction"),
                )
                .child(picker),
        );
    }
    actions = actions.child(div().flex_1());
    if thread.mine {
        let (owner, id) = (owner.clone(), id.clone());
        actions = actions.child(
            Button::new(SharedString::from(format!("delete-{id}")))
                .ghost()
                .xsmall()
                .label("Delete")
                .on_click(move |_, _, cx| {
                    let _ = owner.update(cx, |this, cx| this.delete_comment(&id, cx));
                }),
        );
    }

    let mut header = h_flex()
        .gap_2()
        .text_xs()
        .text_color(muted)
        .child(div().font_medium().child(thread.author.clone()))
        .child(
            div()
                .font_family(cx.theme().mono_font_family.clone())
                .child(thread.commit.clone()),
        );
    if thread.other_revision {
        header = header.child("other revision · line unchanged since");
    }
    if thread.resolved {
        header = header.child("resolved");
    }
    if let Some(selection) = &thread.selection {
        header = header.child(format!("on “{selection}”"));
    }

    v_flex()
        .w_full()
        .gap_1()
        .px_2()
        .py_1()
        .when(thread.other_revision, |this| this.opacity(0.7))
        .child(header)
        // A resolved comment keeps its header and reactions, and folds its body.
        .when(!thread.resolved, |this| {
            this.child(div().text_sm().child(thread.body.clone()))
        })
        .child(actions)
        .into_any_element()
}

fn render_draft(snapshot: &Snapshot, owner: WeakEntity<ReviewApp>, cx: &mut App) -> AnyElement {
    let Some((input, label)) = &snapshot.draft else {
        return div().into_any_element();
    };
    let submit = owner.clone();
    v_flex()
        .w_full()
        .gap_1()
        .px_2()
        .py_1()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(label.clone()),
        )
        .child(Textarea::new(input))
        .child(
            h_flex()
                .gap_1()
                .child(
                    Button::new("draft-submit")
                        .primary()
                        .xsmall()
                        .label("Comment")
                        .on_click(move |_, window, cx| {
                            let _ = submit.update(cx, |this, cx| this.submit_draft(window, cx));
                        }),
                )
                .child(
                    Button::new("draft-cancel")
                        .ghost()
                        .xsmall()
                        .label("Cancel")
                        .on_click(move |_, window, cx| {
                            let _ = owner.update(cx, |this, cx| this.cancel_draft(window, cx));
                        }),
                ),
        )
        .into_any_element()
}

impl Render for ReviewApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hints = "] [ change · } { file · c comment · r reviewed · s split · o other revisions · 1 2 3 scope · zM zR fold · t files";
        v_flex()
            .id("review")
            .key_context(if self.draft.is_some() {
                "Review"
            } else {
                "Review Reading"
            })
            .size_full()
            .min_w_0()
            .min_h_0()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(|this, _: &NextChange, _, cx| {
                this.diff.update(cx, |state, cx| state.next_change(cx))
            }))
            .on_action(cx.listener(|this, _: &PreviousChange, _, cx| {
                this.diff.update(cx, |state, cx| state.previous_change(cx))
            }))
            .on_action(cx.listener(|this, _: &NextFile, _, cx| this.move_file(1, cx)))
            .on_action(cx.listener(|this, _: &PreviousFile, _, cx| this.move_file(-1, cx)))
            .on_action(cx.listener(|this, _: &ToggleMode, _, cx| this.toggle_mode(cx)))
            .on_action(cx.listener(|this, _: &ToggleOtherRevisions, _, cx| {
                this.show_other_revisions = !this.show_other_revisions;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.sidebar_visible = !this.sidebar_visible;
                cx.notify();
            }))
            .on_action(
                cx.listener(|this, _: &ScopeNewForMe, _, cx| {
                    this.set_scope(ScopeKind::NewForMe, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &ScopeWholeBranch, _, cx| {
                this.set_scope(ScopeKind::WholeBranch, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ScopeCustom, _, cx| this.set_scope(ScopeKind::Custom, cx)),
            )
            .on_action(cx.listener(|this, _: &CollapseAll, _, cx| this.set_all_collapsed(true, cx)))
            .on_action(cx.listener(|this, _: &ExpandAll, _, cx| this.set_all_collapsed(false, cx)))
            .on_action(cx.listener(|this, _: &StartComment, window, cx| {
                if let Some(range) = this.diff.read(cx).selected_lines() {
                    this.start_comment(range, window, cx);
                }
            }))
            .on_action(
                cx.listener(|this, _: &CancelDraft, window, cx| this.cancel_draft(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SubmitDraft, window, cx| this.submit_draft(window, cx)),
            )
            .on_action(cx.listener(|this, _: &MarkReviewed, _, cx| {
                if let Some(path) = this.current_file.clone() {
                    let reviewed = this.review_state(&path) == ReviewState::Reviewed;
                    this.set_reviewed(&path, !reviewed, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &Refresh, _, cx| this.reload(cx)))
            .on_action(|_: &Quit, _, cx| cx.quit())
            .child(self.render_title(cx))
            .child(self.render_scope_bar(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .items_start()
                    .when(self.sidebar_visible, |this| {
                        this.child(self.render_sidebar(cx))
                    })
                    .child(
                        div()
                            .flex_1()
                            .h_full()
                            .min_w_0()
                            .child(self.render_diff(cx)),
                    ),
            )
            .child(
                StatusBar::new()
                    .left(
                        Button::new("toggle-sidebar")
                            .ghost()
                            .xsmall()
                            .icon(if self.sidebar_visible {
                                IconName::PanelLeftClose
                            } else {
                                IconName::PanelLeftOpen
                            })
                            .tooltip("Show or hide the file list")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.sidebar_visible = !this.sidebar_visible;
                                cx.notify();
                            })),
                    )
                    .when(self.loading, |bar| {
                        bar.left(div().text_xs().child("Loading…"))
                    })
                    .right(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(hints),
                    ),
            )
    }
}

/// Runs on a background thread. Everything the window needs for one scope.
fn load(
    backend: &dyn ReviewBackend,
    comparison: &Comparison,
    scope: &Scope,
    mine: Option<ReviewFile>,
) -> Result<Loaded, String> {
    let text = |error: review_core::BackendError| error.to_string();
    let resolved = backend.resolve(comparison).map_err(text)?;
    let commits = backend.commits(&resolved).map_err(text)?;
    let author = backend.author_name().map_err(text)?;
    let review_dir = backend.review_dir().map_err(text)?;
    let review_path = ReviewFile::path_in(&review_dir, &comparison.source, &author);

    let mut mine = match mine {
        Some(file) => file,
        None if review_path.exists() => {
            ReviewFile::load(&review_path).map_err(|error| error.to_string())?
        }
        None => ReviewFile::new(author, &resolved),
    };
    mine.set_head(&resolved);
    let mut reviews = ReviewSet::new(mine);
    // Colleagues' reviews: any other review file dropped into the same folder.
    if let Some(entries) = review_path
        .parent()
        .and_then(|folder| std::fs::read_dir(folder).ok())
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if path != review_path
                && path
                    .extension()
                    .is_some_and(|extension| extension == "toml")
                && let Ok(file) = ReviewFile::load(&path)
            {
                reviews.add_other(file);
            }
        }
    }

    let patch = backend
        .patch(&resolved, scope, &reviews.mine.reviewed)
        .map_err(text)?;
    let files = DiffFile::parse(&patch.text).map_err(|error| {
        format!(
            "Could not read the diff (patch line {}): {}",
            error.line(),
            error.message()
        )
    })?;
    let mut blobs = HashMap::new();
    for file in &patch.files {
        let blob = backend.blob_id(&resolved.head, &file.path).map_err(text)?;
        blobs.insert(file.path.clone(), blob);
    }
    let in_scope: HashSet<Oid> = match scope {
        Scope::Commits(picked) => picked.iter().cloned().collect(),
        _ => commits.iter().map(|commit| commit.id.clone()).collect(),
    };
    let placed = place_all(backend, &reviews, &patch, &in_scope).map_err(text)?;

    Ok(Loaded {
        resolved,
        commits,
        reviews,
        review_path,
        patch,
        files,
        blobs,
        placed,
    })
}

fn place_all(
    backend: &dyn ReviewBackend,
    reviews: &ReviewSet,
    patch: &ScopedPatch,
    in_scope: &HashSet<Oid>,
) -> Result<Vec<Placed>, review_core::BackendError> {
    let shown: HashSet<&str> = patch
        .files
        .iter()
        .filter(|file| file.state != FileState::UnchangedSinceReview)
        .map(|file| file.path.as_str())
        .collect();
    let mut blames: HashMap<String, Vec<BlameLine>> = HashMap::new();
    let mut placed = Vec::new();
    for thread in reviews.threads() {
        let comment = thread.comment;
        if !shown.contains(comment.path.as_str()) {
            continue;
        }
        let hit = match (comment.side, &patch.new_side) {
            (Side::New, NewSide::Commit(rev)) => {
                if !blames.contains_key(&comment.path) {
                    blames.insert(comment.path.clone(), backend.blame(rev, &comment.path)?);
                }
                match place(comment, in_scope, &blames[&comment.path]) {
                    Placement::InScope { line } => Some((line, false)),
                    Placement::OtherRevision { line } => Some((line, true)),
                    Placement::NotInView => None,
                }
            }
            // No blame for removed lines or for a combined tree: show the
            // comment at its stored line while its commit is in scope.
            _ => in_scope
                .contains(&comment.commit)
                .then_some((comment.line, false)),
        };
        if let Some((line, other_revision)) = hit {
            placed.push(Placed {
                id: comment.id.clone(),
                path: comment.path.clone(),
                side: comment.side,
                line,
                other_revision,
            });
        }
    }
    Ok(placed)
}
