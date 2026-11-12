//! The review window.
//!
//! One view owns all state. Repository work happens in background tasks and
//! comes back through [`ReviewApp::install`]; nothing here calls the backend
//! from the render path.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use diffie_core::review::{
    NewComment, Placement, ReactionKind, ReviewFile, ReviewSet, ReviewState, Side, as_prompt, place,
};
use diffie_core::time::ago;
use diffie_core::{
    BlameLine, Commit, Comparison, FileState, FileStatus, FileSummary, NewSide, Oid, Resolved,
    ReviewBackend, Scope, ScopedPatch,
};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _,
    StyledExt as _, Theme, ThemeMode, ThemeRegistry, TitleBar,
    button::{
        Button, ButtonCustomVariant, ButtonVariants as _, Toggle, ToggleGroup, ToggleVariants as _,
    },
    checkbox::Checkbox,
    diff::{
        Diff, DiffAnnotation, DiffDecoration, DiffEvent, DiffFile, DiffFold, DiffHunkSeparator,
        DiffLinePosition, DiffLineRange, DiffMode, DiffSide, DiffState,
    },
    h_flex,
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
    list::ListItem,
    popover::Popover,
    progress::Progress,
    resizable::{ResizableState, h_resizable, resizable_panel},
    scroll::ScrollableElement as _,
    spinner::Spinner,
    switch::Switch,
    text::TextView,
    v_flex,
};
use gpui_kit::{
    Anchor, AnyElement, App, AppContext as _, ClipboardItem, Context, ElementId, Entity,
    FocusHandle, Focusable as _, Hsla, InteractiveElement as _, IntoElement, KeyBinding,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, WeakEntity, Window, div, prelude::FluentBuilder as _, px, rgb,
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
            PickerToggle,
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

/// The application's themes, embedded so every platform ships them. The
/// owner's default dark theme is Gruvbox; Midnight is the mockup's palette.
const THEMES: [&str; 3] = [
    include_str!("../themes/light.json"),
    include_str!("../themes/gruvbox-dark.json"),
    include_str!("../themes/midnight.json"),
];
const THEME_CHOICES: [&str; 3] = ["Light", "Gruvbox Dark", "Midnight"];
const DEFAULT_DARK: &str = "Gruvbox Dark";

fn apply_theme(name: &str, cx: &mut App) {
    let Some(config) = ThemeRegistry::global(cx).themes().get(name).cloned() else {
        return;
    };
    Theme::update(cx, |theme| theme.apply_config(&config));
}

pub fn init(cx: &mut App) {
    {
        let registry = ThemeRegistry::global_mut(cx);
        for theme in THEMES {
            registry
                .load_themes_from_str(theme)
                .expect("the embedded themes must parse");
        }
    }
    let registry = ThemeRegistry::global(cx);
    let dark = registry.themes().get(DEFAULT_DARK).cloned();
    let light = registry.themes().get("Light").cloned();
    Theme::update(cx, |theme| {
        if let Some(dark) = dark {
            theme.dark_theme = dark;
        }
        if let Some(light) = light {
            theme.light_theme = light;
        }
    });

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
        KeyBinding::new("space", PickerToggle, Some("CommitList")),
        KeyBinding::new("escape", CancelDraft, REVIEW),
        KeyBinding::new(submit, SubmitDraft, REVIEW),
        KeyBinding::new(refresh, Refresh, REVIEW),
        KeyBinding::new(quit, Quit, REVIEW),
    ]);
}

/// Temporary tracing for the text-selection comment flow; remove once it is
/// settled. Appends to /tmp/diffie-selection-debug.log so a session other
/// than the one that launched the app can read it.
fn selection_debug(message: impl AsRef<str>) {
    use std::io::Write as _;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/diffie-selection-debug.log")
    {
        let _ = writeln!(file, "{}", message.as_ref());
    }
}

/// The orange that marks everything comment-related. It rides on the
/// `chart.4` token because the Kit has no dedicated comment token; every
/// embedded theme sets `chart.4` to its comment orange.
fn comment_accent(cx: &App) -> Hsla {
    cx.theme().chart_4
}

/// Added and removed line backgrounds for the diff.
///
/// The Diff's own tint is `success`/`danger` at a fixed 12% over the
/// background, which can only produce washed-out pastels. The mockup wants
/// dark saturated fills, so the app paints its own row decorations and keeps
/// the tint off. The values are per theme; unknown themes fall back to the
/// wash the tint would have given.
fn diff_backgrounds(cx: &App) -> (Hsla, Hsla) {
    let theme = cx.theme();
    match theme.theme_name().as_ref() {
        "Midnight" => (rgb(0x10291c).into(), rgb(0x33161a).into()),
        "Gruvbox Dark" => (rgb(0x3d4220).into(), rgb(0x472322).into()),
        "Light" | "Default Light" => (rgb(0xe6ffec).into(), rgb(0xffebe9).into()),
        _ => {
            let mix = |over: Hsla| {
                let mut color = over;
                color.a = 0.12;
                color
            };
            (mix(theme.success), mix(theme.danger))
        }
    }
}

/// A fine count pill in the comment orange, for tree rows and picker items.
fn comment_badge(count: usize, cx: &App) -> gpui_kit::Div {
    h_flex()
        .flex_none()
        .h(px(13.))
        .min_w(px(13.))
        .px(px(4.))
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(comment_accent(cx))
        .text_color(cx.theme().background)
        .text_size(px(9.5))
        .line_height(px(13.))
        .font_medium()
        .child(count.to_string())
}

/// Lockfiles and similar machine-written files start collapsed and are
/// listed quietly.
fn is_generated(path: &str) -> bool {
    let name = path.rsplit_once('/').map_or(path, |(_, name)| name);
    matches!(
        name,
        "Cargo.lock"
            | "package-lock.json"
            | "yarn.lock"
            | "pnpm-lock.yaml"
            | "go.sum"
            | "Gemfile.lock"
            | "composer.lock"
            | "flake.lock"
            | "poetry.lock"
            | "uv.lock"
    ) || name.ends_with(".min.js")
        || name.ends_with(".min.css")
}

/// One run of added or removed lines, parsed from the patch so the diff rows
/// can be painted with the theme's own backgrounds.
#[derive(Clone)]
struct ChangedRun {
    path: SharedString,
    side: DiffSide,
    start: usize,
    end: usize,
    added: bool,
}

/// Classic tree order: inside every folder its subfolders come first, files
/// after, each alphabetical. The diff and the file panel are both sorted
/// with this, so reading one is reading the other.
fn tree_order(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let mut a_parts = a.split('/').peekable();
    let mut b_parts = b.split('/').peekable();
    loop {
        match (a_parts.next(), b_parts.next()) {
            (Some(x), Some(y)) => {
                let x_is_file = a_parts.peek().is_none();
                let y_is_file = b_parts.peek().is_none();
                match (x_is_file, y_is_file) {
                    (false, true) => return Ordering::Less,
                    (true, false) => return Ordering::Greater,
                    _ => match x.cmp(y) {
                        Ordering::Equal if x_is_file => return Ordering::Equal,
                        Ordering::Equal => {}
                        unequal => return unequal,
                    },
                }
            }
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
        }
    }
}

#[cfg(test)]
mod tree_order_tests {
    #[test]
    fn folders_sort_above_files_on_every_level() {
        let mut paths = vec![
            "justfile",
            "crates/core/src/lib.rs",
            "README.md",
            "crates/app/src/app.rs",
            "docs/HANDOFF.md",
            "crates/zz.rs",
        ];
        paths.sort_by(|a, b| super::tree_order(a, b));
        assert_eq!(
            paths,
            vec![
                "crates/app/src/app.rs",
                "crates/core/src/lib.rs",
                "crates/zz.rs",
                "docs/HANDOFF.md",
                "README.md",
                "justfile",
            ]
        );
    }
}

/// Collects the added and removed line runs of a git-format patch, counting
/// lines the way the Diff numbers them (per side, starting at the hunk
/// headers).
fn changed_runs(patch: &str) -> Vec<ChangedRun> {
    let mut runs: Vec<ChangedRun> = Vec::new();
    let mut path: Option<SharedString> = None;
    let mut old_line = 0usize;
    let mut new_line = 0usize;
    let mut in_hunk = false;
    let push = |runs: &mut Vec<ChangedRun>, path: &Option<SharedString>, side, line, added| {
        let Some(path) = path else { return };
        if let Some(last) = runs.last_mut()
            && last.added == added
            && last.side == side
            && &last.path == path
            && last.end + 1 == line
        {
            last.end = line;
            return;
        }
        runs.push(ChangedRun {
            path: path.clone(),
            side,
            start: line,
            end: line,
            added,
        });
    };
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            let name = rest.strip_prefix("b/").unwrap_or(rest);
            path = (name != "/dev/null").then(|| SharedString::from(name.to_string()));
            in_hunk = false;
        } else if let Some(header) = line.strip_prefix("@@ ") {
            // "@@ -old,count +new,count @@"
            let mut parts = header.split(' ');
            let old = parts.next().and_then(|part| {
                part.strip_prefix('-')?
                    .split(',')
                    .next()?
                    .parse::<usize>()
                    .ok()
            });
            let new = parts.next().and_then(|part| {
                part.strip_prefix('+')?
                    .split(',')
                    .next()?
                    .parse::<usize>()
                    .ok()
            });
            if let (Some(old), Some(new)) = (old, new) {
                old_line = old;
                new_line = new;
                in_hunk = true;
            }
        } else if in_hunk {
            match line.as_bytes().first() {
                Some(b'+') => {
                    push(&mut runs, &path, DiffSide::Modified, new_line, true);
                    new_line += 1;
                }
                Some(b'-') => {
                    push(&mut runs, &path, DiffSide::Original, old_line, false);
                    old_line += 1;
                }
                Some(b' ') => {
                    old_line += 1;
                    new_line += 1;
                }
                // "\ No newline at end of file" and anything else leave the
                // counters alone.
                _ => {}
            }
        }
    }
    runs
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
    /// How many lines the comment covers, ending at `line`.
    lines: usize,
    /// Whether it quotes a dragged text selection; those lines keep an
    /// accent tint so the comment visibly belongs to the span, not the line.
    selection: bool,
    other_revision: bool,
}

struct Draft {
    range: DiffLineRange,
    input: Entity<TextareaState>,
    /// The dragged text this comment is about, when it came from a text
    /// selection rather than whole lines.
    selection: Option<String>,
    /// The commit the comment will attach to, looked up by blame as soon as
    /// the box opens so the reviewer sees it before saving.
    commit: Option<SharedString>,
    /// The comment being edited, when the box replaces an existing body.
    editing: Option<String>,
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
    runs: Vec<ChangedRun>,
    blobs: HashMap<String, Option<Oid>>,
    /// Every file the whole branch changes, with its blob at head. Completion
    /// is judged against this list, never against the scope on screen.
    branch: Vec<(String, Option<Oid>)>,
    touched: HashMap<String, Vec<Oid>>,
    /// A stale `completed` marker was cleared and the file needs saving.
    completed_cleared: bool,
    placed: Vec<Placed>,
    /// Blocks the patch moves verbatim; the diff folds them behind labels.
    moved: Vec<diffie_core::moved::MovedBlock>,
}

/// What annotation and header callbacks may read. They receive an `App`, not
/// this view, so they get a snapshot built during render.
struct Snapshot {
    threads: Vec<ThreadView>,
    files: HashMap<SharedString, FileView>,
    draft: Option<DraftView>,
}

struct DraftView {
    input: Entity<TextareaState>,
    lines: SharedString,
    /// The dragged text, quoted in the box so the draft reads like the card
    /// it will become, not like a line comment.
    selection: Option<SharedString>,
    commit: Option<SharedString>,
    editing: bool,
}

struct ThreadView {
    element: ElementId,
    id: String,
    author: SharedString,
    /// Authors only matter once another reviewer's file is loaded.
    show_author: bool,
    commit: SharedString,
    line_label: SharedString,
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
    additions: usize,
    deletions: usize,
    binary: bool,
    generated: bool,
    /// Checked commits that touched this file, and how many are checked.
    touched: usize,
    in_scope: usize,
    comments: usize,
    other_revision: usize,
}

pub struct ReviewApp {
    backend: Arc<dyn ReviewBackend>,
    repository: SharedString,
    /// The linked worktree the review runs in, when it is not the primary
    /// checkout; shown beside the project name.
    worktree: Option<SharedString>,
    comparison: Comparison,
    scope: ScopeKind,
    picked: Vec<Oid>,
    resolved: Option<Resolved>,
    commits: Arc<Vec<Commit>>,
    files: Arc<Vec<FileSummary>>,
    branch: Vec<(String, Option<Oid>)>,
    touched: HashMap<String, Vec<Oid>>,
    new_side: Option<NewSide>,
    blobs: HashMap<String, Option<Oid>>,
    reviews: Option<ReviewSet>,
    review_path: PathBuf,
    placed: Vec<Placed>,
    diff: Entity<DiffState>,
    /// The reviewer's own split-or-unified choice. The view falls back to
    /// Unified while the window is too narrow for a split, without losing it.
    manual_mode: DiffMode,
    /// Soft-wrap long lines instead of scrolling the diff horizontally.
    wrap: bool,
    /// Hide changes in amount of whitespace. The diff is rebuilt with
    /// `--ignore-all-space`, so whitespace-only files drop out of the view.
    ignore_whitespace: bool,
    /// Folders the reviewer closed in the file panel, by full path.
    collapsed_folders: HashSet<String>,
    runs: Vec<ChangedRun>,
    picker_query: Entity<InputState>,
    picker_focus: FocusHandle,
    layout: Entity<ResizableState>,
    /// Shift-click anchors on the commit toggled last.
    last_picked: Option<Oid>,
    hovered_commit: Option<Oid>,
    /// The diff line under the pointer; carries the add-comment marker.
    hovered_line: Option<DiffLinePosition>,
    /// Whether the primary button is held, tracked in the capture phase so
    /// text-drag tracking survives handlers that stop propagation.
    mouse_down: bool,
    /// The line under the pointer when the primary button went down; a text
    /// drag that ends later selected from here.
    text_anchor: Option<DiffLinePosition>,
    /// The lines a finished text drag covered, first to last. The Kit does
    /// not expose the text selection's location, so the app keeps it from
    /// hover positions at mouse down and up.
    text_lines: Option<(DiffLinePosition, DiffLinePosition)>,
    /// Where the pointer released a selection; the floating comment button
    /// is pinned there.
    text_button_at: Option<gpui_kit::Point<gpui_kit::Pixels>>,
    /// What the floating button will comment on, captured when it is pinned.
    /// Pressing the button must not consult the live selection: the press
    /// itself clears it before a click could complete.
    pending_comment: Option<(DiffLineRange, Option<String>)>,
    show_all_new: bool,
    show_all_reviewed: bool,
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
        worktree: Option<SharedString>,
        comparison: Comparison,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        window.set_window_title(&match &worktree {
            Some(name) => format!("{repository} ({name}) — review"),
            None => format!("{repository} — review"),
        });
        Theme::change(ThemeMode::Dark, Some(window), cx);
        let diff = cx.new(|cx| DiffState::new([], cx).with_mode(DiffMode::Split));
        let picker_query =
            cx.new(|cx| InputState::new(window, cx).placeholder("Message, hash or author"));
        let subscriptions = vec![
            cx.observe(&diff, |_, _, cx| cx.notify()),
            cx.subscribe(&diff, |this, _, event: &DiffEvent, cx| {
                if let DiffEvent::SelectionChanged(Some(range)) = event {
                    this.current_file = Some(range.path().clone());
                    cx.notify();
                }
            }),
            cx.subscribe(&picker_query, |_, _, event: &InputEvent, cx| {
                if let InputEvent::Change = event {
                    cx.notify();
                }
            }),
        ];
        diff.read(cx).focus_handle(cx).focus(window, cx);
        let mut this = Self {
            backend,
            repository,
            worktree,
            comparison,
            scope: ScopeKind::NewForMe,
            picked: Vec::new(),
            resolved: None,
            commits: Arc::default(),
            files: Arc::default(),
            branch: Vec::new(),
            touched: HashMap::new(),
            new_side: None,
            blobs: HashMap::new(),
            reviews: None,
            review_path: PathBuf::new(),
            placed: Vec::new(),
            diff,
            manual_mode: DiffMode::Split,
            wrap: false,
            ignore_whitespace: false,
            collapsed_folders: HashSet::new(),
            runs: Vec::new(),
            picker_query,
            picker_focus: cx.focus_handle(),
            layout: cx.new(|_| ResizableState::default()),
            last_picked: None,
            hovered_commit: None,
            hovered_line: None,
            mouse_down: false,
            text_anchor: None,
            text_lines: None,
            text_button_at: None,
            pending_comment: None,
            show_all_new: false,
            show_all_reviewed: false,
            draft: None,
            current_file: None,
            // On by default: the owner wants other revisions' comments in
            // view unless they are toggled away.
            show_other_revisions: true,
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
        let ignore_whitespace = self.ignore_whitespace;
        self._load = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    load(&*backend, &comparison, &scope, mine, ignore_whitespace)
                })
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
                        file.state == FileState::UnchangedSinceReview
                            || loaded.reviews.mine.review_state(&file.path, blob)
                                == ReviewState::Reviewed
                    })
                    .map(|file| file.path.clone())
                    .collect();
                let generated: Vec<String> = loaded
                    .patch
                    .files
                    .iter()
                    .filter(|file| is_generated(&file.path))
                    .map(|file| file.path.clone())
                    .collect();
                self.diff.update(cx, |state, cx| {
                    state.set_files(loaded.files, cx);
                    // One rebuild; collapsing one by one froze a review
                    // with thousands of ticked or generated files.
                    state.set_files_collapsed(
                        reviewed
                            .iter()
                            .chain(&generated)
                            .map(|path| SharedString::from(path.clone())),
                        true,
                        cx,
                    );
                    // Verbatim-moved blocks fold behind a label on both ends;
                    // a click reveals them when the address change matters.
                    state.set_folds(
                        loaded.moved.iter().flat_map(|block| {
                            let span = |start: usize, end: usize| {
                                if start == end {
                                    format!("{start}")
                                } else {
                                    format!("{start}-{end}")
                                }
                            };
                            [
                                DiffFold::new(
                                    block.from_path.clone(),
                                    DiffSide::Original,
                                    block.from_start,
                                    block.lines,
                                    format!(
                                        "moved to {}:{}",
                                        block.to_path,
                                        span(block.to_start, block.to_end())
                                    ),
                                ),
                                DiffFold::new(
                                    block.to_path.clone(),
                                    DiffSide::Modified,
                                    block.to_start,
                                    block.lines,
                                    format!(
                                        "moved from {}:{}",
                                        block.from_path,
                                        span(block.from_start, block.from_end())
                                    ),
                                ),
                            ]
                        }),
                        cx,
                    );
                });
                self.resolved = Some(loaded.resolved);
                self.commits = Arc::new(loaded.commits);
                self.runs = loaded.runs;
                self.files = Arc::new(loaded.patch.files);
                self.branch = loaded.branch;
                self.touched = loaded.touched;
                self.new_side = Some(loaded.patch.new_side);
                self.blobs = loaded.blobs;
                self.reviews = Some(loaded.reviews);
                self.review_path = loaded.review_path;
                self.placed = loaded.placed;
                self.current_file = None;
                if loaded.completed_cleared {
                    self.save_review(cx);
                }
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

    /// A click on a picker row. Shift checks the run from the last toggled
    /// commit; a plain click toggles one.
    fn picker_click(&mut self, id: &Oid, shift: bool, cx: &mut Context<Self>) {
        // Leaving a non-custom scope starts from "all checked".
        if self.scope != ScopeKind::Custom {
            self.picked = self
                .commits
                .iter()
                .map(|commit| commit.id.clone())
                .collect();
        }
        let position =
            |id: &Oid| -> Option<usize> { self.commits.iter().position(|commit| &commit.id == id) };
        if shift
            && let Some(last) = self.last_picked.clone()
            && let (Some(from), Some(to)) = (position(&last), position(id))
        {
            let run: Vec<Oid> = self.commits[from.min(to)..=from.max(to)]
                .iter()
                .map(|commit| commit.id.clone())
                .collect();
            self.last_picked = Some(id.clone());
            self.pick_many(run, true, cx);
            return;
        }
        self.last_picked = Some(id.clone());
        self.toggle_commit(id, cx);
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
            // A deleted file ticks with no blob: the deletion is the content.
            let blob = self.blobs.get(path).cloned().flatten();
            set.mine.mark_reviewed(path, blob, resolved.head.clone());
            // Completion means the whole branch is reviewed, not the scope on
            // screen: a narrow scope must never set the "last review" marker.
            set.mine.complete_if_all_reviewed(
                self.branch
                    .iter()
                    .map(|(path, blob)| (path.as_str(), blob.as_ref())),
                &resolved.head,
            );
        } else {
            set.mine.clear_reviewed(path);
        }
        self.diff
            .update(cx, |state, cx| state.set_file_collapsed(path, reviewed, cx));
        self.save_review(cx);
        // Ticking hands the current-file marker to the next file, so `r r r`
        // reviews a run of files. Only the marker moves: the view must not
        // scroll (the owner found the jumping annoying); collapsing the
        // ticked file pulls the next one up on its own.
        if reviewed {
            let paths: Vec<SharedString> = self
                .diff
                .read(cx)
                .files()
                .iter()
                .map(|file| file.path().clone())
                .collect();
            self.current_file = match paths.iter().position(|candidate| candidate == path) {
                Some(index) if index + 1 < paths.len() => Some(paths[index + 1].clone()),
                _ => Some(path.to_string().into()),
            };
        }
        cx.notify();
    }

    /// Scrolls to the file's first comment, expanding the file on the way.
    fn reveal_first_comment(&mut self, path: &SharedString, cx: &mut Context<Self>) {
        let Some(placed) = self
            .placed
            .iter()
            .filter(|placed| placed.path == path.as_ref())
            .min_by_key(|placed| placed.line)
        else {
            return;
        };
        let side = match placed.side {
            Side::Old => DiffSide::Original,
            Side::New => DiffSide::Modified,
        };
        let position = DiffLinePosition::new(placed.path.clone(), side, placed.line);
        self.diff
            .update(cx, |state, cx| state.scroll_to_line(position, cx));
        self.current_file = Some(path.clone());
        cx.notify();
    }

    fn reveal_file(&mut self, path: SharedString, cx: &mut Context<Self>) {
        self.diff.update(cx, |state, cx| {
            let was = state.is_file_collapsed(&path);
            state.set_file_collapsed(&path, false, cx);
            selection_debug(format!(
                "reveal {path} was_collapsed={was} now_collapsed={}",
                state.is_file_collapsed(&path)
            ));
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
        // One rebuild for the whole change; a per-file loop rebuilt the
        // rows once per file and froze large reviews for minutes.
        self.diff
            .update(cx, |state, cx| state.set_all_files_collapsed(collapsed, cx));
    }

    fn set_manual_mode(&mut self, mode: DiffMode, cx: &mut Context<Self>) {
        self.manual_mode = mode;
        self.diff.update(cx, |state, cx| state.set_mode(mode, cx));
    }

    fn toggle_mode(&mut self, cx: &mut Context<Self>) {
        let next = match self.manual_mode {
            DiffMode::Split => DiffMode::Unified,
            _ => DiffMode::Split,
        };
        self.set_manual_mode(next, cx);
    }

    /// Falls back to Unified while the window is too narrow for two readable
    /// columns, and returns to the reviewer's own choice when it widens.
    fn fit_mode_to_width(&mut self, window: &Window, cx: &mut Context<Self>) {
        let sidebar = if self.sidebar_visible { 240. } else { 0. };
        let narrow = window.viewport_size().width < px(620. + sidebar);
        let effective = if narrow {
            DiffMode::Unified
        } else {
            self.manual_mode
        };
        if self.diff.read(cx).mode() != effective {
            self.diff
                .update(cx, |state, cx| state.set_mode(effective, cx));
        }
    }

    // ---- comments --------------------------------------------------------

    /// The line range of the current text drag, when there is one. A drag
    /// that crosses files or sides keeps only its end line.
    fn text_selection_range(&self) -> Option<DiffLineRange> {
        let (start, end) = self.text_lines.clone()?;
        if start.path() == end.path() && start.side() == end.side() {
            Some(DiffLineRange::new(
                end.path().clone(),
                end.side(),
                start.line(),
                end.line(),
            ))
        } else {
            Some(DiffLineRange::new(
                end.path().clone(),
                end.side(),
                end.line(),
                end.line(),
            ))
        }
    }

    fn start_comment(
        &mut self,
        range: DiffLineRange,
        selection: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
            range: range.clone(),
            input,
            selection,
            commit: None,
            editing: None,
            _submit: submit,
        });
        self.resolve_draft_commit(range, cx);
        cx.notify();
    }

    /// Blames the drafted lines so the box can say which commit the comment
    /// will attach to before it is saved.
    fn resolve_draft_commit(&mut self, range: DiffLineRange, cx: &mut Context<Self>) {
        let (Some(new_side), Some(resolved)) = (self.new_side.clone(), self.resolved.clone())
        else {
            return;
        };
        let in_scope = self.in_scope();
        let fallback = self
            .commits
            .iter()
            .find(|commit| in_scope.contains(&commit.id))
            .map_or(resolved.head.clone(), |commit| commit.id.clone());
        let backend = self.backend.clone();
        let path = range.path().to_string();
        let line = range.end();
        let on_new_side = range.end_side() == DiffSide::Modified;
        cx.spawn(async move |this, cx| {
            let origin = cx
                .background_spawn(async move {
                    match (&new_side, on_new_side) {
                        (NewSide::Commit(rev), true) => {
                            backend.line_origin(rev, &path, line).unwrap_or(None)
                        }
                        _ => None,
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let commit = origin.map_or(fallback, |origin| origin.commit);
                if let Some(draft) = this.draft.as_mut()
                    && draft.editing.is_none()
                {
                    draft.commit = Some(commit.short().to_string().into());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Opens the comment box over an existing comment with its body loaded.
    fn start_edit(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(set) = self.reviews.as_ref() else {
            return;
        };
        let Some(comment) = set
            .threads()
            .into_iter()
            .find(|thread| thread.comment.id == id)
            .map(|thread| thread.comment.clone())
        else {
            return;
        };
        let Some(placed) = self.placed.iter().find(|placed| placed.id == id) else {
            return;
        };
        let side = match placed.side {
            Side::Old => DiffSide::Original,
            Side::New => DiffSide::Modified,
        };
        let range = DiffLineRange::new(placed.path.clone(), side, placed.line, placed.line);
        self.start_comment(range, None, window, cx);
        if let Some(draft) = self.draft.as_mut() {
            draft.editing = Some(id.to_string());
            draft.commit = Some(comment.commit.short().to_string().into());
            let body = comment.body.trim_end().to_string();
            draft
                .input
                .update(cx, |state, cx| state.set_value(body, window, cx));
        }
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
        if let Some(id) = &draft.editing {
            if let Some(set) = self.reviews.as_mut() {
                set.mine.edit_comment(id, body);
                self.save_review(cx);
            }
            cx.notify();
            return;
        }
        let (Some(new_side), Some(resolved)) = (self.new_side.clone(), self.resolved.clone())
        else {
            return;
        };
        let range = draft.range;
        let dragged = draft.selection;
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
                // An uncommitted review has no in-scope commits at all, yet
                // the working tree is exactly the revision under review: a
                // fresh comment there is current, not from another revision.
                let other_revision = !this.comparison.uncommitted && !in_scope.contains(&commit);
                let Some(set) = this.reviews.as_mut() else {
                    return;
                };
                let from_selection = dragged.is_some();
                let id = set.mine.add_comment(NewComment {
                    commit,
                    path: path.clone(),
                    side,
                    line: anchored_line,
                    lines,
                    selection: dragged,
                    snippet,
                    body,
                    suggestion: None,
                });
                this.placed.push(Placed {
                    id,
                    path,
                    side,
                    line,
                    lines,
                    selection: from_selection,
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

    fn copy_comment_as_prompt(&self, id: &str, cx: &mut Context<Self>) {
        if let Some(set) = &self.reviews
            && let Some(thread) = set
                .threads()
                .into_iter()
                .find(|thread| thread.comment.id == id)
        {
            cx.write_to_clipboard(ClipboardItem::new_string(as_prompt(&[thread])));
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
            let show_author = set.threads().iter().any(|thread| !thread.mine);
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
                let line_label = if thread.comment.lines > 1 {
                    format!(
                        "lines {}–{}",
                        placed.line.saturating_sub(thread.comment.lines - 1),
                        placed.line
                    )
                } else {
                    format!("line {}", placed.line)
                };
                threads.push(ThreadView {
                    element: ElementId::from(SharedString::from(thread.comment.id.clone())),
                    id: thread.comment.id.clone(),
                    author: thread.author.to_string().into(),
                    show_author,
                    commit: thread.comment.commit.short().to_string().into(),
                    line_label: line_label.into(),
                    body: thread.comment.body.trim_end().to_string().into(),
                    selection: thread.comment.selection.clone().map(Into::into),
                    other_revision: placed.other_revision,
                    mine: thread.mine,
                    resolved: thread.resolved(),
                    reactions,
                    left: set.mine.reactions_left(&thread.comment.id),
                });
            }
        }
        let in_scope_set = self.in_scope();
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
                    _ if file.state == FileState::UnchangedSinceReview => {
                        Some("no changes since your review".into())
                    }
                    _ if is_generated(&file.path) => {
                        Some("generated file · collapsed by default".into())
                    }
                    _ if self.review_state(&file.path) == ReviewState::ChangedSince => {
                        Some("changed since your review".into())
                    }
                    _ => None,
                };
                let touched = self.touched.get(&file.path).map_or(0, |commits| {
                    commits
                        .iter()
                        .filter(|id| in_scope_set.contains(id))
                        .count()
                });
                let comments = self
                    .placed
                    .iter()
                    .filter(|placed| placed.path == file.path)
                    .count();
                let other_revision = self
                    .placed
                    .iter()
                    .filter(|placed| placed.path == file.path && placed.other_revision)
                    .count();
                (
                    SharedString::from(file.path.clone()),
                    FileView {
                        state: self.review_state(&file.path),
                        note,
                        additions: file.additions,
                        deletions: file.deletions,
                        binary: file.binary,
                        generated: is_generated(&file.path),
                        touched,
                        in_scope: in_scope_set.len(),
                        comments,
                        other_revision,
                    },
                )
            })
            .collect();
        let draft = self.draft.as_ref().map(|draft| {
            let range = &draft.range;
            let lines = if range.start() == range.end() {
                format!("line {}", range.end())
            } else {
                format!(
                    "lines {}–{}",
                    range.start().min(range.end()),
                    range.start().max(range.end())
                )
            };
            DraftView {
                input: draft.input.clone(),
                lines: lines.into(),
                selection: draft.selection.clone().map(Into::into),
                commit: draft.commit.clone(),
                editing: draft.editing.is_some(),
            }
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
        // Worktree and branches read as icon plus name, not chips: the owner
        // wants the cluster quiet and free of repeated words.
        let marked = |icon: Lucide, text: String| {
            h_flex()
                .gap_1()
                .text_color(muted)
                .font_family(mono.clone())
                .text_size(px(11.5))
                .child(Icon::new(icon).xsmall())
                .child(text)
        };
        let split = self.manual_mode == DiffMode::Split;
        let revisions_on = self.show_other_revisions;
        let mut theme_menu = v_flex().gap_0p5();
        for name in THEME_CHOICES {
            let current = cx.theme().theme_name().as_ref() == name;
            theme_menu = theme_menu.child(
                Button::new(SharedString::from(format!("theme-{name}")))
                    .ghost()
                    .xsmall()
                    .label(name)
                    .selected(current)
                    .on_click(move |_, _, cx| apply_theme(name, cx)),
            );
        }
        TitleBar::new().child(
            h_flex()
                .size_full()
                .px(px(10.))
                .gap(px(6.))
                .child(
                    div()
                        .text_size(px(13.))
                        .font_semibold()
                        .child(self.repository.clone()),
                )
                .when_some(self.worktree.clone(), |bar, worktree| {
                    bar.child(marked(Lucide::FolderGit2, worktree.to_string()))
                })
                .child(div().text_color(muted).child("/"))
                .child(marked(
                    if self.comparison.uncommitted {
                        Lucide::Pencil
                    } else {
                        Lucide::GitBranch
                    },
                    self.comparison.source.clone(),
                ))
                .child(div().text_color(muted).child("going into"))
                .child(marked(Lucide::GitBranch, self.comparison.target.clone()))
                // Swapping would make the working tree the target, which is
                // nothing git can resolve.
                .when(!self.comparison.uncommitted, |bar| {
                    bar.child(
                        Button::new("swap")
                            .outline()
                            .xsmall()
                            .w(px(24.))
                            .h(px(22.))
                            .icon(Icon::new(Lucide::ArrowLeftRight))
                            .tooltip("Swap direction")
                            .on_click(cx.listener(|this, _, _, cx| {
                                // A second quick click must not reach the
                                // title bar, whose double click minimizes
                                // the window.
                                cx.stop_propagation();
                                this.swap_direction(cx);
                            })),
                    )
                })
                .child(
                    div()
                        .text_color(muted)
                        .truncate()
                        .child(if !self.comparison.uncommitted {
                            "only what the branch adds"
                        } else if self.comparison.source == "uncommitted" {
                            "only what is not yet committed"
                        } else {
                            // Against another revision the diff also carries
                            // commits, so "uncommitted" would understate it.
                            "everything the working tree adds"
                        }),
                )
                .child(div().flex_1())
                .child(
                    h_flex()
                        .id("title-controls")
                        .gap(px(6.))
                        // The buttons' clicks end here: a pair of quick
                        // clicks would otherwise reach the title bar as a
                        // double click and minimize the window (the macOS
                        // title bar default). The empty strip and the labels
                        // stay out, so the native double click still works
                        // there.
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(
                            ToggleGroup::new("layout")
                                .outline()
                                .segmented()
                                .single()
                                .small()
                                .child(Toggle::new("split").label("Split").checked(split))
                                .child(Toggle::new("unified").label("Unified").checked(!split))
                                .on_click(cx.listener(|this, checks: &Vec<bool>, _, cx| {
                                    // Clicking the checked segment clears the
                                    // whole group; an either-or control keeps
                                    // the current choice instead.
                                    let mode = match checks.iter().position(|on| *on) {
                                        Some(0) => DiffMode::Split,
                                        Some(_) => DiffMode::Unified,
                                        None => return,
                                    };
                                    this.set_manual_mode(mode, cx);
                                })),
                        )
                        .child(
                            Button::new("wrap")
                                .outline()
                                .xsmall()
                                .w(px(28.))
                                .h(px(24.))
                                .icon(Icon::new(Lucide::TextWrap))
                                .selected(self.wrap)
                                .tooltip("Wrap long lines")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.wrap = !this.wrap;
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("ignore-whitespace")
                                .outline()
                                .xsmall()
                                .w(px(28.))
                                .h(px(24.))
                                .icon(Icon::new(Lucide::Pilcrow))
                                .selected(self.ignore_whitespace)
                                .tooltip("Hide whitespace-only changes")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.ignore_whitespace = !this.ignore_whitespace;
                                    // The patch itself changes, not just the
                                    // rendering: a reload is required.
                                    this.reload(cx);
                                })),
                        )
                        .child(
                            h_flex()
                                .border_1()
                                .border_color(cx.theme().border)
                                .rounded(px(5.))
                                .overflow_hidden()
                                .child(
                                    Button::new("collapse-all")
                                        .ghost()
                                        .xsmall()
                                        .w(px(28.))
                                        .h(px(24.))
                                        .rounded_none()
                                        .border_r_1()
                                        .border_color(cx.theme().border)
                                        .icon(Icon::new(Lucide::ChevronsDownUp))
                                        .tooltip("Collapse all files")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.set_all_collapsed(true, cx)
                                        })),
                                )
                                .child(
                                    Button::new("expand-all")
                                        .ghost()
                                        .xsmall()
                                        .w(px(28.))
                                        .h(px(24.))
                                        .rounded_none()
                                        .icon(IconName::ChevronsUpDown)
                                        .tooltip("Expand all files")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.set_all_collapsed(false, cx)
                                        })),
                                ),
                        )
                        .child(
                            // A switch, not a labelled button: showing comments
                            // from other revisions is an overlay on the view,
                            // not a mode, and the knob carries the state so the
                            // label no longer has to spell out "on" or "off".
                            Switch::new("other-revisions")
                                .xsmall()
                                .checked(revisions_on)
                                .label("Outdated comments")
                                .tooltip("Show comments from other revisions")
                                .on_change(cx.listener(|this, checked: &bool, _, cx| {
                                    this.show_other_revisions = *checked;
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("copy-prompt")
                                .outline()
                                .xsmall()
                                .h(px(26.))
                                .label("Copy all as prompt")
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.copy_all_as_prompt(cx)),
                                ),
                        )
                        .child(
                            Popover::new("theme-menu")
                                .trigger(
                                    Button::new("theme")
                                        .outline()
                                        .xsmall()
                                        .w(px(28.))
                                        .h(px(26.))
                                        .icon(IconName::Palette)
                                        .tooltip("Switch theme"),
                                )
                                .child(theme_menu),
                        ),
                ),
        )
    }

    /// One picker item: checkbox, then the title over author, age and hash.
    fn render_commit_row(
        &self,
        commit: &Commit,
        head: bool,
        skipped: bool,
        comments: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = commit.id.clone();
        let hover_id = commit.id.clone();
        let checked = self.picked.contains(&commit.id) || self.scope != ScopeKind::Custom;
        let muted = cx.theme().muted_foreground;
        let accent = comment_accent(cx);
        let mono = cx.theme().mono_font_family.clone();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs() as i64);
        let mut meta = h_flex()
            .gap_1()
            .text_size(px(11.))
            .text_color(if checked {
                cx.theme().blue_light
            } else {
                muted
            })
            .child(Icon::new(Lucide::CircleUserRound).xsmall())
            .child(div().truncate().child(commit.author.clone()))
            .child(format!("· {} ·", ago(commit.time, now)))
            .child(div().font_family(mono).child(commit.id.short().to_string()));
        if head {
            meta = meta.child("· HEAD");
        }
        if skipped {
            meta = meta.child(div().text_color(accent).child("skipped"));
        }
        h_flex()
            .id(SharedString::from(format!("pick-{}", commit.id)))
            .w_full()
            .items_start()
            .gap_2()
            .px_1()
            .py(px(3.))
            .rounded(px(4.))
            .cursor_pointer()
            .when(checked, |row| row.bg(cx.theme().accent))
            .when(skipped, |row| {
                row.border_1()
                    .border_dashed()
                    .border_color(accent.opacity(0.45))
            })
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered {
                    this.hovered_commit = Some(hover_id.clone());
                    cx.notify();
                }
            }))
            .on_click(
                cx.listener(move |this, event: &gpui_kit::ClickEvent, window, cx| {
                    window.focus(&this.picker_focus, cx);
                    this.picker_click(&id, event.modifiers().shift, cx);
                }),
            )
            .child(
                Checkbox::new(SharedString::from(format!("pick-box-{}", commit.id)))
                    .xsmall()
                    .checked(checked),
            )
            .child(
                v_flex()
                    .min_w_0()
                    .flex_1()
                    .child(
                        div()
                            .truncate()
                            .when(!checked, |title| title.text_color(muted))
                            .child(commit.subject.clone()),
                    )
                    .child(meta),
            )
            .when(comments > 0, |row| {
                row.child(comment_badge(comments, cx).mt(px(2.)))
            })
            .into_any_element()
    }

    fn render_scope_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let total = self.commits.len();
        let muted = cx.theme().muted_foreground;
        let accent = comment_accent(cx);
        let mono = cx.theme().mono_font_family.clone();
        let blue_light = cx.theme().blue_light;

        // The range button's parts: count, hash range, skipped note.
        let positions: Vec<usize> = self
            .commits
            .iter()
            .enumerate()
            .filter(|(_, commit)| {
                self.scope != ScopeKind::Custom || self.picked.contains(&commit.id)
            })
            .map(|(index, _)| index)
            .collect();
        let count_text = format!("{} of {total} commits", positions.len());
        let range_text = match (positions.first(), positions.last()) {
            (Some(newest), Some(oldest)) => Some(format!(
                "{} … {}",
                self.commits[*oldest].id.short(),
                self.commits[*newest].id.short()
            )),
            _ => None,
        };
        let skipped_count = match (positions.first(), positions.last()) {
            (Some(newest), Some(oldest)) => oldest - newest + 1 - positions.len(),
            _ => 0,
        };

        // Which unchecked commits sit between checked ones.
        let skipped_set: HashSet<Oid> = if self.scope == ScopeKind::Custom {
            match (positions.first(), positions.last()) {
                (Some(first), Some(last)) => self.commits[*first..=*last]
                    .iter()
                    .filter(|commit| !self.picked.contains(&commit.id))
                    .map(|commit| commit.id.clone())
                    .collect(),
                _ => HashSet::new(),
            }
        } else {
            HashSet::new()
        };
        let comment_counts: HashMap<Oid, usize> =
            self.reviews.as_ref().map_or_else(HashMap::new, |set| {
                let mut counts = HashMap::new();
                for thread in set.threads() {
                    *counts.entry(thread.comment.commit.clone()).or_insert(0) += 1;
                }
                counts
            });

        let query = self.picker_query.read(cx).value().trim().to_lowercase();
        let matches = |commit: &Commit| {
            query.is_empty()
                || commit.subject.to_lowercase().contains(&query)
                || commit.author.to_lowercase().contains(&query)
                || commit.id.short().to_lowercase().starts_with(&query)
        };

        let split_at = self.reviewed_from().unwrap_or(total);
        let group = |title: &'static str,
                     range: std::ops::Range<usize>,
                     heading_color: Hsla,
                     bordered: bool,
                     cx: &mut Context<Self>| {
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
                .h(px(24.))
                .gap_2()
                .px_1()
                .when(bordered, |row| {
                    row.mt(px(2.)).border_t_1().border_color(cx.theme().border)
                })
                .child(
                    Checkbox::new(title)
                        .xsmall()
                        .checked(all)
                        .on_change(cx.listener(move |this, checked: &bool, _, cx| {
                            this.pick_many(ids.clone(), *checked, cx)
                        })),
                )
                .child(
                    div()
                        .flex_1()
                        .font_semibold()
                        .text_color(heading_color)
                        .child(title),
                )
                .child(
                    div()
                        .font_normal()
                        .text_color(muted)
                        .child(format!("{checked} of {}", range.len())),
                )
        };

        let mut list = v_flex().w_full().gap_0p5();
        let section = |list: gpui_kit::Div,
                       title: &'static str,
                       range: std::ops::Range<usize>,
                       heading_color: Hsla,
                       bordered: bool,
                       expanded: bool,
                       cx: &mut Context<Self>| {
            let mut list = list;
            if range.is_empty() {
                return list;
            }
            list = list.child(group(title, range.clone(), heading_color, bordered, cx));
            let rows: Vec<&Commit> = self.commits[range.clone()]
                .iter()
                .filter(|commit| matches(commit))
                .collect();
            let shown = if expanded || rows.len() <= 8 {
                rows.len()
            } else {
                6
            };
            for commit in &rows[..shown] {
                let head = self
                    .commits
                    .first()
                    .is_some_and(|first| first.id == commit.id);
                let skipped = skipped_set.contains(&commit.id);
                let comments = comment_counts.get(&commit.id).copied().unwrap_or(0);
                list = list.child(self.render_commit_row(commit, head, skipped, comments, cx));
            }
            if shown < rows.len() {
                let remaining = rows.len() - shown;
                let toggle = title;
                list = list.child(
                    Button::new(SharedString::from(format!("more-{title}")))
                        .ghost()
                        .xsmall()
                        .label(format!("Show {remaining} more"))
                        .text_color(muted)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if toggle == "New since your review" {
                                this.show_all_new = true;
                            } else {
                                this.show_all_reviewed = true;
                            }
                            cx.notify();
                        })),
                );
            }
            list
        };
        list = section(
            list,
            "New since your review",
            0..split_at,
            blue_light,
            false,
            self.show_all_new,
            cx,
        );
        list = section(
            list,
            "Already reviewed",
            split_at..total,
            muted,
            split_at > 0,
            self.show_all_reviewed,
            cx,
        );

        let reviewed = self
            .files
            .iter()
            .filter(|file| self.review_state(&file.path) == ReviewState::Reviewed)
            .count();
        let progress = if self.files.is_empty() {
            0.
        } else {
            reviewed as f32 * 100. / self.files.len() as f32
        };
        let explanation = match self.scope {
            ScopeKind::NewForMe => "Each file is compared with the version you last ticked",
            ScopeKind::WholeBranch => "Everything the branch adds",
            ScopeKind::Custom => "Combined diff of the checked commits",
        };

        let mut range_button =
            h_flex()
                .gap(px(6.))
                .child(count_text)
                .when_some(range_text, |row, text| {
                    row.child(
                        div()
                            .font_family(mono)
                            .text_size(px(11.5))
                            .text_color(blue_light)
                            .child(text),
                    )
                });
        if skipped_count > 0 {
            range_button = range_button.child(
                div()
                    .text_color(accent)
                    .child(format!("{skipped_count} skipped")),
            );
        }
        range_button = range_button.child(Icon::new(IconName::ChevronDown).xsmall());

        let picker = v_flex()
            .w(px(340.))
            .gap_0p5()
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(muted)
                    .px_0p5()
                    .child("Find commit"),
            )
            .child(Input::new(&self.picker_query).small().cleanable(true))
            .child(
                div()
                    .id("commit-list")
                    .key_context("CommitList")
                    .track_focus(&self.picker_focus)
                    .max_h(px(400.))
                    .mt_1()
                    .overflow_y_scrollbar()
                    .child(list),
            )
            .child(
                div()
                    .pt_1()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_size(px(11.))
                    .text_color(muted)
                    .child(
                        "Space toggles the hovered commit · shift-click checks a run · \
                         a group heading checks the whole group",
                    ),
            );

        h_flex()
            .flex_none()
            .w_full()
            .px(px(10.))
            .py(px(4.))
            .gap_2()
            .bg(cx.theme().sidebar)
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .text_size(px(10.5))
                    .font_semibold()
                    .text_color(muted)
                    .child("SCOPE"),
            )
            .child(
                ToggleGroup::new("scope")
                    .outline()
                    .segmented()
                    .single()
                    .small()
                    .child(
                        Toggle::new("scope-new")
                            .label("New for me")
                            .checked(self.scope == ScopeKind::NewForMe),
                    )
                    .child(
                        Toggle::new("scope-whole")
                            .label("Whole branch")
                            .checked(self.scope == ScopeKind::WholeBranch),
                    )
                    .child(
                        Toggle::new("scope-custom")
                            .label("Custom")
                            .checked(self.scope == ScopeKind::Custom),
                    )
                    .on_click(cx.listener(|this, checks: &Vec<bool>, _, cx| {
                        let kind = match checks.iter().position(|checked| *checked) {
                            Some(0) => ScopeKind::NewForMe,
                            Some(1) => ScopeKind::WholeBranch,
                            Some(2) => ScopeKind::Custom,
                            _ => return,
                        };
                        this.set_scope(kind, cx);
                    })),
            )
            .child(
                Popover::new("commit-picker")
                    .anchor(Anchor::TopLeft)
                    .trigger(
                        ListItem::new("commit-range")
                            .h(px(26.))
                            .px_2()
                            .border_1()
                            .border_color(cx.theme().border)
                            .rounded(px(5.))
                            .child(range_button),
                    )
                    .child(picker),
            )
            .child(div().text_color(muted).child(explanation))
            .child(div().flex_1())
            .child(format!("{reviewed} of {} files reviewed", self.files.len()))
            .child(
                Progress::new("files-reviewed")
                    .value(progress)
                    .xsmall()
                    .w(px(90.)),
            )
    }

    fn render_file_row(&self, index: usize, depth: usize, cx: &mut Context<Self>) -> AnyElement {
        let file = &self.files[index];
        let muted = cx.theme().muted_foreground;
        let mono = cx.theme().mono_font_family.clone();
        let path = SharedString::from(file.path.clone());
        let name = file
            .path
            .rsplit_once('/')
            .map_or(file.path.as_str(), |(_, name)| name);
        let (status, status_color) = match file.status {
            FileStatus::Added => ("A", cx.theme().success),
            FileStatus::Deleted => ("D", cx.theme().danger),
            FileStatus::Modified => ("M", cx.theme().warning),
            FileStatus::Renamed => ("R", cx.theme().blue_light),
        };
        let comments = self
            .placed
            .iter()
            .filter(|placed| placed.path == file.path)
            .count();
        let state = self.review_state(&file.path);
        let unchanged = file.state == FileState::UnchangedSinceReview;
        let quiet = state == ReviewState::Reviewed || unchanged || is_generated(&file.path);
        let selected = self.current_file.as_ref() == Some(&path);
        let target = path.clone();
        let counts = |count: usize, sign: char, color: Hsla| {
            (count > 0).then(|| {
                div()
                    .flex_none()
                    .font_family(mono.clone())
                    .text_size(px(10.5))
                    .text_color(color)
                    .child(format!("{sign}{count}"))
            })
        };
        h_flex()
            .id(path.clone())
            .w_full()
            .h(px(22.))
            .pl(px(4. + depth as f32 * 10.))
            .pr(px(4.))
            .gap(px(6.))
            .rounded(px(4.))
            .cursor_pointer()
            .when(selected, |row| row.bg(cx.theme().accent))
            .when(quiet, |row| row.text_color(muted))
            .hover(|row| row.bg(cx.theme().list_hover))
            .on_click(cx.listener(move |this, _, _, cx| this.reveal_file(target.clone(), cx)))
            .child(
                div()
                    .flex_none()
                    .font_family(mono.clone())
                    .text_size(px(10.5))
                    .font_medium()
                    .text_color(status_color)
                    .child(status),
            )
            .child(div().flex_1().min_w_0().truncate().child(name.to_string()))
            .when(comments > 0, |row| row.child(comment_badge(comments, cx)))
            .when(state == ReviewState::Reviewed, |row| {
                row.child(
                    Icon::new(IconName::Check)
                        .xsmall()
                        .text_color(cx.theme().blue_light),
                )
            })
            .when(state == ReviewState::ChangedSince, |row| {
                // The lost tick alone is invisible; the reviewer needs to
                // see which files moved under them since the last pass.
                row.child(
                    Icon::new(Lucide::TriangleAlert)
                        .xsmall()
                        .text_color(cx.theme().warning),
                )
            })
            .when(is_generated(&file.path), |row| {
                row.child(div().flex_none().text_size(px(10.5)).child("generated"))
            })
            .when(
                !unchanged && !file.binary && !is_generated(&file.path),
                |row| {
                    row.children(counts(file.additions, '+', cx.theme().success))
                        .children(counts(file.deletions, '−', cx.theme().danger))
                },
            )
            .into_any_element()
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        // Folder rows over indented files, in the diff's order — which the
        // load sorts into the classic tree (see `tree_order`), so panel and
        // main content walk in step. One linear pass opens a folder row
        // whenever the folder chain changes; an order that revisited a
        // folder would repeat its header rather than reorder a single one.
        enum TreeRow {
            Folder {
                label: SharedString,
                path: SharedString,
                depth: usize,
                collapsed: bool,
            },
            File {
                index: usize,
                depth: usize,
            },
        }
        let mut rows = Vec::new();
        let mut previous: Vec<String> = Vec::new();
        for (index, file) in self.files.iter().enumerate() {
            let mut folders: Vec<String> = file.path.split('/').map(String::from).collect();
            folders.pop();
            let common = previous
                .iter()
                .zip(&folders)
                .take_while(|(kept, part)| kept == part)
                .count();
            // The first collapsed ancestor hides everything beneath it; its
            // own row stays, showing the closed folder.
            let shut = (0..folders.len()).find(|depth| {
                self.collapsed_folders
                    .contains(folders[..=*depth].join("/").as_str())
            });
            for (depth, part) in folders.iter().enumerate().skip(common) {
                if shut.is_some_and(|shut| depth > shut) {
                    break;
                }
                rows.push(TreeRow::Folder {
                    label: format!("{part} /").into(),
                    path: folders[..=depth].join("/").into(),
                    depth,
                    collapsed: shut == Some(depth),
                });
            }
            if shut.is_none() {
                rows.push(TreeRow::File {
                    index,
                    depth: folders.len(),
                });
            }
            previous = folders;
        }
        let rows = Rc::new(rows);

        let reviewed = self
            .files
            .iter()
            .filter(|file| self.review_state(&file.path) == ReviewState::Reviewed)
            .count();
        v_flex()
            .w_full()
            .h_full()
            .pt(px(4.))
            .pr(px(4.))
            .pb(px(8.))
            .pl(px(6.))
            .border_r_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().sidebar)
            .child(
                h_flex()
                    .h(px(22.))
                    .px(px(4.))
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(10.5))
                            .font_semibold()
                            .text_color(muted)
                            .child(format!(
                                "FILES · {} · {reviewed} REVIEWED",
                                self.files.len()
                            )),
                    )
                    .child(
                        Button::new("hide-files")
                            .ghost()
                            .xsmall()
                            .label("Hide")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.sidebar_visible = false;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                // Virtualized: with thousands of changed files, an element
                // per row each frame froze the window.
                gpui_kit::uniform_list(
                    "file-list",
                    rows.len(),
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        rows[range]
                            .iter()
                            .map(|row| match row {
                                TreeRow::Folder {
                                    label,
                                    path,
                                    depth,
                                    collapsed,
                                } => {
                                    let toggled = path.clone();
                                    h_flex()
                                        .id(SharedString::from(format!("folder-{path}")))
                                        .h(px(22.))
                                        .gap(px(4.))
                                        .pl(px(4. + *depth as f32 * 10.))
                                        .text_color(cx.theme().muted_foreground)
                                        .child(
                                            Icon::new(if *collapsed {
                                                Lucide::Folder
                                            } else {
                                                Lucide::FolderOpen
                                            })
                                            .xsmall(),
                                        )
                                        .child(label.clone())
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            let path = toggled.to_string();
                                            if !this.collapsed_folders.remove(&path) {
                                                this.collapsed_folders.insert(path);
                                            }
                                            cx.notify();
                                        }))
                                        .into_any_element()
                                }
                                TreeRow::File { index, depth } => {
                                    this.render_file_row(*index, *depth, cx)
                                }
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .flex_1()
                .min_h_0(),
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
        let for_hover = owner.clone();

        let (added, removed) = diff_backgrounds(cx);
        // The Kit decorates whole lines only, so a selection comment keeps an
        // accent tint on its lines; the exact character span cannot be marked
        // without an upstream change. Supplied last, the tint wins over the
        // added and removed fills on those lines.
        let span_tint = comment_accent(cx).opacity(0.16);
        let selection_spans = self
            .placed
            .iter()
            .filter(|placed| placed.selection)
            .map(|placed| {
                let side = match placed.side {
                    Side::Old => DiffSide::Original,
                    Side::New => DiffSide::Modified,
                };
                let start = placed.line.saturating_sub(placed.lines.saturating_sub(1));
                DiffDecoration::new(DiffLineRange::new(
                    SharedString::from(placed.path.clone()),
                    side,
                    start,
                    placed.line,
                ))
                .with_background(span_tint)
            });
        let draft_span = self
            .draft
            .as_ref()
            .filter(|draft| draft.selection.is_some())
            .map(|draft| DiffDecoration::new(draft.range.clone()).with_background(span_tint));
        let decorations: Vec<DiffDecoration> = self
            .runs
            .iter()
            .map(|run| {
                DiffDecoration::new(DiffLineRange::new(
                    run.path.clone(),
                    run.side,
                    run.start,
                    run.end,
                ))
                .with_background(if run.added { added } else { removed })
            })
            .chain(selection_spans)
            .chain(draft_span)
            .collect();

        Diff::new(&self.diff)
            .size_full()
            .soft_wrap(self.wrap)
            // The decorations below paint richer added and removed fills than
            // the component's own washed-out tint.
            .change_background(false)
            .decorations(decorations)
            .hunk_separator(DiffHunkSeparator::Simple)
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
            // Two ways into a comment: the diff's own + beside the hovered
            // line number for line comments, and the floating + pinned at
            // the pointer for real text selections (the root's capture
            // handlers). The marker lane stays off; it would cost a
            // permanent gap between numbers and code.
            .on_add_annotation(cx.listener(|this, range: &DiffLineRange, window, cx| {
                this.start_comment(range.clone(), None, window, cx);
            }))
            .on_line_hover(move |position, window, cx| {
                let position = position.cloned();
                let selecting = gpui_kit::base::TextSelection::has_selection(window, cx);
                let _ = for_hover.update(cx, |this, cx| {
                    let mut changed = false;
                    // Row leave events arrive after the next row's enter, so a
                    // None here is usually stale; the last line stays until a
                    // new one reports.
                    if position.is_some() && this.hovered_line != position {
                        this.hovered_line = position.clone();
                        changed = true;
                        selection_debug(format!(
                            "hover-any mouse_down={} selecting={} pos={:?}",
                            this.mouse_down,
                            selecting,
                            position.as_ref().map(|p| (p.path().to_string(), p.line()))
                        ));
                    }
                    // A text drag in progress keeps its end on the pointer.
                    if this.mouse_down
                        && selecting
                        && let (Some(start), Some(end)) = (this.text_anchor.clone(), position)
                    {
                        let lines = Some((start, end));
                        if this.text_lines != lines {
                            this.text_lines = lines;
                            changed = true;
                        }
                    }
                    if changed {
                        cx.notify();
                    }
                });
            })
            .render_header(move |file, _, cx| {
                let (snapshot, owner) = &for_header;
                let path = file.path().clone();
                let view = snapshot.files.get(&path);
                let reviewed = view.is_some_and(|view| view.state == ReviewState::Reviewed);
                let note = view.and_then(|view| view.note.clone());
                // A file that changed under an existing tick is the one note
                // worth interrupting for; the rest stay quiet metadata.
                let alert = view.is_some_and(|view| view.state == ReviewState::ChangedSince);
                let warning = cx.theme().warning;
                let muted = cx.theme().muted_foreground;
                let mono = cx.theme().mono_font_family.clone();
                let quiet = view.is_some_and(|view| view.generated) || reviewed;
                let owner = owner.clone();
                let counts = |count: usize, sign: char, color: Hsla| {
                    (count > 0).then(|| {
                        div()
                            .flex_none()
                            .font_family(mono.clone())
                            .text_size(px(11.5))
                            .text_color(color)
                            .child(format!("{sign}{count}"))
                    })
                };
                let touched = view.and_then(|view| {
                    (view.touched > 0 && view.in_scope > 1).then(|| {
                        if view.touched == view.in_scope {
                            format!("touched by all {} commits", view.in_scope)
                        } else {
                            format!("touched by {} of {} commits", view.touched, view.in_scope)
                        }
                    })
                });
                let comments = view.and_then(|view| {
                    (view.comments > 0).then(|| {
                        let mut text = format!(
                            "{} comment{}",
                            view.comments,
                            if view.comments == 1 { "" } else { "s" }
                        );
                        if view.other_revision > 0 {
                            text.push_str(&format!(
                                " · {} from another revision",
                                view.other_revision
                            ));
                        }
                        text
                    })
                });
                let path_for_copy = path.clone();
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(mono.clone())
                                    .text_size(px(12.))
                                    .font_medium()
                                    .when(quiet, |path| path.text_color(muted))
                                    .child(path.clone()),
                            )
                            .child(
                                Button::new(SharedString::from(format!("copy-path-{path}")))
                                    .ghost()
                                    .xsmall()
                                    .w(px(18.))
                                    .h(px(18.))
                                    .icon(Icon::new(Lucide::Copy))
                                    .tooltip("Copy the file path")
                                    .on_click(move |_, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            path_for_copy.to_string(),
                                        ));
                                    }),
                            ),
                    )
                    .children(view.and_then(|view| {
                        if view.binary {
                            None
                        } else {
                            counts(view.additions, '+', cx.theme().success)
                        }
                    }))
                    .children(view.and_then(|view| {
                        if view.binary {
                            None
                        } else {
                            counts(view.deletions, '−', cx.theme().danger)
                        }
                    }))
                    .when_some(touched, |row, touched| {
                        row.child(div().flex_none().text_color(muted).child(touched))
                    })
                    .when_some(comments, |row, comments| {
                        let path = path.clone();
                        let owner = owner.clone();
                        row.child(
                            div()
                                .id(SharedString::from(format!("comments-{path}")))
                                .flex_none()
                                .text_color(comment_accent(cx))
                                .cursor_pointer()
                                .on_click(move |_, _, cx| {
                                    let _ = owner.update(cx, |this, cx| {
                                        this.reveal_first_comment(&path, cx)
                                    });
                                })
                                .child(comments),
                        )
                    })
                    .when_some(note, |row, note| {
                        row.child(
                            div()
                                .flex_none()
                                .map(|element| {
                                    if alert {
                                        element.text_color(warning).font_medium()
                                    } else {
                                        element.text_color(muted)
                                    }
                                })
                                .child(note),
                        )
                    })
                    .child({
                        // A ticked file should read as done at a glance: box
                        // and label turn the theme's light blue. The Kit's
                        // labeled Checkbox hard-codes both colors, so this is
                        // the base checkbox plus its own label.
                        let blue = cx.theme().blue_light;
                        let box_owner = owner.clone();
                        let box_path = path.clone();
                        let label_path = path.clone();
                        h_flex()
                            .flex_none()
                            .gap(px(5.))
                            .child(
                                // The base checkbox is unstyled by design;
                                // box, border and check mark are drawn here.
                                gpui_kit::base::Checkbox::new(SharedString::from(format!(
                                    "reviewed-{path}"
                                )))
                                .checked(reviewed)
                                .w(px(13.))
                                .h(px(13.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(3.))
                                .border_1()
                                .border_color(muted)
                                .cursor_pointer()
                                .styles(|styles| {
                                    styles.checked(|state| state.bg(blue).border_color(blue))
                                })
                                .when(reviewed, |checkbox| {
                                    checkbox.child(
                                        Icon::new(IconName::Check)
                                            .xsmall()
                                            .text_color(cx.theme().background),
                                    )
                                })
                                .accessibility_label("Reviewed")
                                .on_change(
                                    move |state, _, _, cx| {
                                        let checked =
                                            state == gpui_kit::base::CheckboxState::Checked;
                                        let _ = box_owner.update(cx, |this, cx| {
                                            this.set_reviewed(&box_path, checked, cx)
                                        });
                                    },
                                ),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("reviewed-label-{path}")))
                                    .cursor_pointer()
                                    .map(|label| {
                                        if reviewed {
                                            label.text_color(blue).font_medium()
                                        } else {
                                            label.text_color(muted)
                                        }
                                    })
                                    .on_click(move |_, _, cx| {
                                        let _ = owner.update(cx, |this, cx| {
                                            this.set_reviewed(&label_path, !reviewed, cx)
                                        });
                                    })
                                    .child("Reviewed"),
                            )
                    })
                    .into_any_element()
            })
            .into_any_element()
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let mono = cx.theme().mono_font_family.clone();
        // Only shortcuts that exist; each new binding earns its hint here.
        let hints: &[(&str, &str)] = &[
            ("] [", "change"),
            ("} {", "file"),
            ("1 2 3", "scope"),
            ("r", "mark reviewed"),
            ("c", "comment"),
            ("s", "split / unified"),
            ("o", "outdated comments"),
            ("zM zR", "fold"),
            ("t", "files"),
        ];
        let mut bar = h_flex()
            .flex_none()
            .w_full()
            .px(px(10.))
            .py(px(3.))
            .gap(px(14.))
            .text_size(px(11.))
            .text_color(muted)
            .bg(cx.theme().status_bar)
            .border_t_1()
            .border_color(cx.theme().border);
        if !self.sidebar_visible {
            bar = bar.child(
                Button::new("show-files")
                    .ghost()
                    .xsmall()
                    .label("Files")
                    .tooltip("Show the file list")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar_visible = true;
                        cx.notify();
                    })),
            );
        }
        for (keys, label) in hints {
            bar = bar.child(
                h_flex()
                    .flex_none()
                    .gap_1()
                    .child(
                        div()
                            .font_family(mono.clone())
                            .text_color(cx.theme().foreground)
                            .child(*keys),
                    )
                    .child(*label),
            );
        }
        if self.loading {
            bar = bar.child("Loading…");
        }
        let position = self.current_file.as_ref().and_then(|current| {
            let files = self.diff.read(cx).files();
            let index = files.iter().position(|file| file.path() == current)?;
            Some(format!("File {} of {}", index + 1, files.len()))
        });
        bar.child(div().flex_1())
            .when_some(position, |bar, position| bar.child(position))
    }
}

fn render_thread(thread: &ThreadView, owner: WeakEntity<ReviewApp>, cx: &mut App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let accent = comment_accent(cx);
    let mono = cx.theme().mono_font_family.clone();
    let id = thread.id.clone();
    let quiet_border = muted.opacity(0.5);

    let mut actions = h_flex().w_full().h(px(22.)).gap_1();
    for (kind, count, mine) in &thread.reactions {
        let (kind, owner, id) = (*kind, owner.clone(), id.clone());
        let chip = Button::new(SharedString::from(format!(
            "reaction-{id}-{}",
            kind.label()
        )))
        .xsmall()
        .h(px(22.))
        .rounded(px(11.))
        .label(format!("{} {count}", kind.emoji()))
        .tooltip(if *mine {
            format!("{} · click to remove yours", kind.label())
        } else {
            kind.label().to_string()
        })
        .map(|chip| {
            if *mine {
                chip.custom(
                    ButtonCustomVariant::new(cx)
                        .color(cx.theme().accent)
                        .foreground(cx.theme().foreground)
                        .hover(cx.theme().accent.opacity(0.8))
                        .active(cx.theme().accent),
                )
                .border_1()
                .border_color(cx.theme().primary)
            } else {
                chip.outline()
            }
        })
        .on_click(move |_, _, cx| {
            let _ = owner.update(cx, |this, cx| this.toggle_reaction(&id, kind, cx));
        });
        actions = actions.child(chip);
    }
    if !thread.left.is_empty() {
        let mut picker = v_flex().gap_0p5();
        for kind in thread.left.iter().copied() {
            let (owner, id) = (owner.clone(), id.clone());
            picker = picker.child(
                Button::new(SharedString::from(format!("react-{id}-{}", kind.label())))
                    .ghost()
                    .xsmall()
                    .h(px(24.))
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
                        .outline()
                        .xsmall()
                        .w(px(26.))
                        .h(px(22.))
                        .rounded(px(11.))
                        .icon(Icon::new(Lucide::FaceSlightlySmilingPlus))
                        .tooltip("Add reaction"),
                )
                .child(picker),
        );
    }
    actions = actions.child(div().flex_1());
    let text_button = |id: SharedString, label: &'static str| {
        Button::new(id)
            .ghost()
            .xsmall()
            .h(px(22.))
            .label(label)
            .text_color(muted)
    };
    if thread.mine {
        let (owner, id) = (owner.clone(), id.clone());
        actions = actions.child(
            text_button(SharedString::from(format!("edit-{id}")), "Edit").on_click(
                move |_, window, cx| {
                    let _ = owner.update(cx, |this, cx| this.start_edit(&id, window, cx));
                },
            ),
        );
    }
    {
        let (owner, id) = (owner.clone(), id.clone());
        actions = actions.child(
            text_button(
                SharedString::from(format!("copy-prompt-{id}")),
                "Copy as prompt",
            )
            .on_click(move |_, _, cx| {
                let _ = owner.update(cx, |this, cx| this.copy_comment_as_prompt(&id, cx));
            }),
        );
    }
    if thread.mine {
        let (owner, id) = (owner.clone(), id.clone());
        actions = actions.child(
            text_button(SharedString::from(format!("delete-{id}")), "Delete").on_click(
                move |_, _, cx| {
                    let _ = owner.update(cx, |this, cx| this.delete_comment(&id, cx));
                },
            ),
        );
    }

    let header_color = if thread.other_revision { muted } else { accent };
    let mut header = h_flex()
        .flex_wrap()
        .gap(px(6.))
        .text_size(px(11.))
        .text_color(header_color);
    if thread.other_revision {
        header = header.child(
            div()
                .px(px(4.))
                .border_1()
                .border_color(quiet_border)
                .rounded(px(3.))
                .child("other revision"),
        );
    }
    if thread.show_author {
        header = header.child(div().font_medium().child(thread.author.clone()));
    }
    header = header.child(div().font_family(mono).child(thread.commit.clone()));
    header = header.child(if thread.other_revision {
        format!("{}, unchanged since", thread.line_label)
    } else {
        thread.line_label.to_string()
    });
    if thread.resolved {
        header = header.child("resolved");
    }
    if let Some(selection) = &thread.selection {
        header = header.child(format!("selection “{selection}”"));
    }

    let card = v_flex()
        .w_full()
        .gap_1()
        .px_2()
        .py(px(6.))
        .rounded(px(6.))
        .map(|card| {
            if thread.other_revision {
                card.border_1()
                    .border_dashed()
                    .border_color(quiet_border)
                    .text_color(muted)
            } else {
                card.border_1()
                    .border_color(accent.opacity(0.45))
                    .bg(accent.opacity(0.07))
            }
        })
        .child(header)
        // A resolved comment keeps its header and reactions, and folds its body.
        // Markdown, so fenced and backticked code in a comment reads as code.
        .when(!thread.resolved, |card| {
            card.child(TextView::markdown(
                SharedString::from(format!("comment-body-{}", thread.id)),
                thread.body.clone(),
            ))
        })
        .when(!thread.other_revision, |card| card.child(actions));

    div().w_full().px_2().py_1().child(card).into_any_element()
}

fn render_draft(snapshot: &Snapshot, owner: WeakEntity<ReviewApp>, cx: &mut App) -> AnyElement {
    let Some(draft) = &snapshot.draft else {
        return div().into_any_element();
    };
    let submit = owner.clone();
    let blue_light = cx.theme().blue_light;
    let mono = cx.theme().mono_font_family.clone();
    let submit_hint = if cx.platform_info().is_mac() {
        "⌘↵"
    } else {
        "ctrl ↵"
    };
    let mut label = h_flex()
        .flex_wrap()
        .gap_1()
        .text_size(px(11.))
        .text_color(blue_light);
    if draft.editing {
        label = label.child("Editing the comment attached to");
        if let Some(commit) = &draft.commit {
            label = label.child(div().font_family(mono.clone()).child(commit.clone()));
        }
    } else {
        label = match &draft.selection {
            Some(selection) => label
                .child("New comment on")
                .child(
                    div()
                        .font_family(mono.clone())
                        .text_color(comment_accent(cx))
                        .child(format!("“{selection}”")),
                )
                .child(format!("in {}", draft.lines)),
            None => label.child(format!("New comment on {}", draft.lines)),
        };
        match &draft.commit {
            Some(commit) => {
                label = label
                    .child("· attaches to")
                    .child(div().font_family(mono.clone()).child(commit.clone()));
            }
            None => label = label.child("· finding the commit…"),
        }
    }
    let card = v_flex()
        .w_full()
        .gap_1()
        .px_2()
        .py(px(6.))
        .rounded(px(6.))
        .border_1()
        .border_color(cx.theme().primary)
        .bg(cx.theme().primary.opacity(0.08))
        .child(label)
        .child(Textarea::new(&draft.input))
        .child(
            h_flex()
                .gap(px(6.))
                .child(
                    Button::new("draft-submit")
                        .primary()
                        .xsmall()
                        .h(px(24.))
                        .font_semibold()
                        .label(format!("Comment  {submit_hint}"))
                        .on_click(move |_, window, cx| {
                            let _ = submit.update(cx, |this, cx| this.submit_draft(window, cx));
                        }),
                )
                .child(
                    Button::new("draft-suggest")
                        .outline()
                        .xsmall()
                        .h(px(24.))
                        .label("Suggest change")
                        .disabled(true)
                        .tooltip("Suggestions are not built yet"),
                )
                .child(
                    Button::new("draft-cancel")
                        .ghost()
                        .xsmall()
                        .h(px(24.))
                        .text_color(cx.theme().muted_foreground)
                        .label("Cancel  esc")
                        .on_click(move |_, window, cx| {
                            let _ = owner.update(cx, |this, cx| this.cancel_draft(window, cx));
                        }),
                ),
        );
    div().w_full().px_2().py_1().child(card).into_any_element()
}

impl Render for ReviewApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.fit_mode_to_width(window, cx);
        // A click or Escape elsewhere ends the text selection; its button and
        // remembered lines go with it.
        if (self.text_lines.is_some() || self.text_button_at.is_some())
            && !gpui_kit::base::TextSelection::has_selection(window, cx)
            && !self.mouse_down
        {
            self.text_lines = None;
            self.text_button_at = None;
            self.pending_comment = None;
        }
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
            .text_size(px(12.))
            // The Kit does not say where a text drag happened, so the lines
            // under the pointer while the button is held stand in for it.
            // Both hooks run in the capture phase: the selection layer stops
            // the bubbled events, so bubble handlers never see a drag.
            .capture_any_mouse_down(cx.listener(|this, _, _, _| {
                this.mouse_down = true;
                this.text_anchor = this.hovered_line.clone();
                selection_debug(format!(
                    "down anchor={:?}",
                    this.text_anchor
                        .as_ref()
                        .map(|p| (p.path().to_string(), p.line()))
                ));
            }))
            .capture_any_mouse_up(cx.listener(
                |this, event: &gpui_kit::MouseUpEvent, window, cx| {
                    this.mouse_down = false;
                    selection_debug(format!(
                        "up has_sel={} anchor={:?} hovered={:?} gutter_sel={} text={:?}",
                        gpui_kit::base::TextSelection::has_selection(window, cx),
                        this.text_anchor
                            .as_ref()
                            .map(|p| (p.path().to_string(), p.line())),
                        this.hovered_line
                            .as_ref()
                            .map(|p| (p.path().to_string(), p.line())),
                        this.diff.read(cx).selected_lines().is_some(),
                        gpui_kit::base::TextSelection::selected_text(window, cx)
                            .chars()
                            .take(30)
                            .collect::<String>(),
                    ));
                    if gpui_kit::base::TextSelection::has_selection(window, cx) {
                        if let (Some(start), Some(end)) =
                            (this.text_anchor.clone(), this.hovered_line.clone())
                        {
                            this.text_lines = Some((start, end));
                        }
                        // Everything the comment needs is captured here:
                        // pressing the button clears the selection before a
                        // click could read it. Only a selection with real
                        // text pins the button: a double click reports a
                        // selection too, and following it put the button at
                        // arbitrary spots. Line comments start from the
                        // diff's own hover button instead.
                        let payload = this.text_selection_range().and_then(|range| {
                            let text = gpui_kit::base::TextSelection::selected_text(window, cx);
                            let text = text.trim().to_string();
                            (text.chars().count() > 1).then_some((range, Some(text)))
                        });
                        if let Some(payload) = payload {
                            this.pending_comment = Some(payload);
                            this.text_button_at = Some(event.position);
                            selection_debug(format!("button pinned at {:?}", event.position));
                        }
                    }
                    cx.notify();
                },
            ))
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
                    this.start_comment(range, None, window, cx);
                } else if let Some(range) = this.text_selection_range() {
                    let text = gpui_kit::base::TextSelection::selected_text(window, cx);
                    let text = text.trim();
                    let selection = (!text.is_empty()).then(|| text.to_string());
                    this.start_comment(range, selection, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &PickerToggle, _, cx| {
                if let Some(id) = this.hovered_commit.clone() {
                    this.picker_click(&id, false, cx);
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
            .child({
                let diff_column = v_flex().flex_1().h_full().min_w_0().child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .child(self.render_diff(cx)),
                );
                div().flex_1().min_h_0().w_full().map(|wrap| {
                    if self.sidebar_visible {
                        wrap.child(
                            h_resizable("main")
                                .with_state(&self.layout)
                                .child(
                                    resizable_panel()
                                        .size(px(240.))
                                        .size_range(px(160.)..px(420.))
                                        .child(self.render_sidebar(cx)),
                                )
                                .child(resizable_panel().child(diff_column)),
                        )
                    } else {
                        wrap.child(diff_column)
                    }
                })
            })
            .child(self.render_status_bar(cx))
            .when_some(self.text_button_at, |root, at| {
                // The floating comment button, just right of the pointer
                // where the drag released. It acts on mouse down with the
                // payload captured at pin time: the press itself clears the
                // selection, so neither a full click nor the live selection
                // can be relied on.
                root.child(
                    div()
                        .id("comment-selection")
                        .absolute()
                        .left(at.x + px(10.))
                        .top(at.y - px(11.))
                        .w(px(22.))
                        .h(px(22.))
                        .flex()
                        .items_center()
                        .justify_center()
                        // Square-ish like the Kit's own add-annotation button
                        // (owner); a circle looked foreign next to it.
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().primary)
                        .text_color(cx.theme().primary_foreground)
                        .cursor_pointer()
                        .child(Icon::new(IconName::Plus).xsmall())
                        .on_mouse_down(
                            gpui_kit::MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                let Some((range, selection)) = this.pending_comment.take() else {
                                    return;
                                };
                                this.text_button_at = None;
                                this.text_lines = None;
                                this.start_comment(range, selection, window, cx);
                            }),
                        ),
                )
            })
    }
}

/// Runs on a background thread. Everything the window needs for one scope.
fn load(
    backend: &dyn ReviewBackend,
    comparison: &Comparison,
    scope: &Scope,
    mine: Option<ReviewFile>,
    ignore_whitespace: bool,
) -> Result<Loaded, String> {
    let text = |error: diffie_core::BackendError| error.to_string();
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

    let mut patch = backend
        .patch(&resolved, scope, &reviews.mine.reviewed, ignore_whitespace)
        .map_err(text)?;
    let mut files = DiffFile::parse(&patch.text).map_err(|error| {
        format!(
            "Could not read the diff (patch line {}): {}",
            error.line(),
            error.message()
        )
    })?;
    // Ticked files without new changes still appear as collapsed headers in
    // the diff, so the list mirrors the file panel. A header-only entry is
    // enough; one that fails to parse is simply not shown.
    for file in &patch.files {
        if file.state == FileState::UnchangedSinceReview
            && let Ok(extra) = DiffFile::parse(&format!("diff --git a/{0} b/{0}\n", file.path))
        {
            files.extend(extra);
        }
    }
    // The owner reads both columns as a classic tree: folders on top, root
    // files at the bottom. git emits patches path-sorted instead, so both
    // lists are resorted the same way here; everything else keys by path.
    files.sort_by(|a, b| tree_order(a.path().as_str(), b.path().as_str()));
    patch.files.sort_by(|a, b| tree_order(&a.path, &b.path));
    let runs = changed_runs(&patch.text);
    let changed = backend.changed_paths(&resolved).map_err(text)?;
    // One tree listing instead of a `blob_id` subprocess per file: a large
    // commit changes thousands of files. An absent path is a deletion. An
    // uncommitted review ticks against what is on disk, not against `head`.
    let tree = if comparison.uncommitted {
        let mut paths = changed.clone();
        paths.extend(patch.files.iter().map(|file| file.path.clone()));
        backend.worktree_blob_ids(&paths).map_err(text)?
    } else {
        backend.blob_ids(&resolved.head).map_err(text)?
    };
    let mut blobs = HashMap::new();
    for file in &patch.files {
        blobs.insert(file.path.clone(), tree.get(&file.path).cloned());
    }
    let mut branch = Vec::new();
    for path in changed {
        let blob = tree.get(&path).cloned();
        branch.push((path, blob));
    }
    let completed_cleared = reviews.mine.verify_completed(
        branch
            .iter()
            .map(|(path, blob)| (path.as_str(), blob.as_ref())),
    );
    let touched = backend.commits_touching(&resolved).map_err(text)?;
    let in_scope: HashSet<Oid> = match scope {
        Scope::Commits(picked) => picked.iter().cloned().collect(),
        _ => commits.iter().map(|commit| commit.id.clone()).collect(),
    };
    let placed = place_all(
        backend,
        &reviews,
        &patch,
        &in_scope,
        resolved.comparison.uncommitted,
    )
    .map_err(text)?;
    let moved = diffie_core::moved::moved_blocks(&patch.text);

    Ok(Loaded {
        resolved,
        commits,
        reviews,
        review_path,
        patch,
        files,
        runs,
        blobs,
        branch,
        touched,
        completed_cleared,
        placed,
        moved,
    })
}

fn place_all(
    backend: &dyn ReviewBackend,
    reviews: &ReviewSet,
    patch: &ScopedPatch,
    in_scope: &HashSet<Oid>,
    uncommitted: bool,
) -> Result<Vec<Placed>, diffie_core::BackendError> {
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
            // comment at its stored line while its commit is in scope. An
            // uncommitted review has no in-scope commits, but its working
            // tree is the revision under review, so its comments stay
            // current instead of dropping out or dimming to other-revision.
            _ => {
                (uncommitted || in_scope.contains(&comment.commit)).then_some((comment.line, false))
            }
        };
        if let Some((line, other_revision)) = hit {
            placed.push(Placed {
                id: comment.id.clone(),
                path: comment.path.clone(),
                side: comment.side,
                line,
                lines: comment.lines,
                selection: comment.selection.is_some(),
                other_revision,
            });
        }
    }
    Ok(placed)
}
