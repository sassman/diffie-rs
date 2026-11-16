//! The review window.
//!
//! One view owns all state. Repository work happens in background tasks and
//! comes back through [`ReviewApp::install`]; nothing here calls the backend
//! from the render path.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use diffie_core::pr::PullStatus;
use diffie_core::review::{
    ArchiveFile, NewComment, Placement, ReactionKind, ReviewFile, ReviewInput, ReviewSet,
    ReviewState, Side, as_prompt, map_line, place,
};
use diffie_core::search::{SearchHit, search_patch};

use crate::settings::{LayoutMode, Settings, WindowSettings};
use diffie_core::time::ago;
use diffie_core::{
    BlameLine, ChangedRegion, Commit, Comparison, FileState, FileStatus, FileSummary, NewSide, Oid,
    Resolved, ReviewBackend, Scope, ScopedPatch,
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
        Diff, DiffAnnotation, DiffChangeIndicator, DiffDecoration, DiffEvent, DiffFile, DiffFold,
        DiffGutterMarker, DiffHunkSeparator, DiffLinePosition, DiffLineRange, DiffMode, DiffSide,
        DiffSpanHighlight, DiffState,
    },
    h_flex,
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
    list::ListItem,
    menu::{ContextMenuExt as _, PopupMenuItem},
    popover::Popover,
    progress::Progress,
    resizable::{ResizableState, h_resizable, resizable_panel},
    scroll::ScrollableElement as _,
    spinner::Spinner,
    text::TextView,
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::{
    Anchor, Animation, AnimationExt as _, AnyElement, App, AppContext as _, ClipboardItem, Context,
    ElementId, Entity, FocusHandle, Focusable as _, Hsla, InteractiveElement as _, IntoElement,
    KeyBinding, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, WeakEntity, Window, WindowBounds, deferred, div,
    prelude::FluentBuilder as _, px, rgb,
};

mod pull;
pub use pull::Pull;

mod commands {
    gpui_kit::actions!(
        review,
        [
            NextChange,
            PreviousChange,
            NextFile,
            PreviousFile,
            NextComment,
            PreviousComment,
            OpenSearch,
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
            JumpBack,
            JumpForward,
            Refresh,
            Quit
        ]
    );

    // Menu-only stand-ins for the shortcut actions above. A menu item shows
    // its action's keymap binding, and on macOS a single keystroke becomes
    // the NSMenuItem key equivalent - a bare letter there is matched by the
    // menu before the responder chain, so it would fire while the reviewer
    // types a comment. These actions stay unbound: their items show no
    // shortcut hint, and the root view routes them to the same methods.
    gpui_kit::actions!(
        menu,
        [
            MenuAbout,
            MenuSplitUnified,
            MenuWrap,
            MenuHideWhitespace,
            MenuCollapseAll,
            MenuExpandAll,
            MenuFileList,
            MenuEarlierComments,
            MenuNextChange,
            MenuPreviousChange,
            MenuNextFile,
            MenuPreviousFile,
            MenuNextComment,
            MenuPreviousComment,
            MenuJumpBack,
            MenuJumpForward,
            MenuScopeNewForMe,
            MenuScopeWholeBranch,
            MenuScopeCustom
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

/// Resolving folds the card away, but not under the pointer: the thread
/// stays for this grace, then fades over [`FOLD_FADE`] into its line
/// marker - three seconds from verdict to quiet.
const FOLD_GRACE: std::time::Duration = std::time::Duration::from_millis(2500);
const FOLD_FADE: std::time::Duration = std::time::Duration::from_millis(500);

/// The pause before a settings write, long enough to turn a window
/// drag's stream of bounds into one write.
const SETTINGS_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

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
    let (submit, refresh, quit, back, forward) = if is_mac {
        ("cmd-enter", "cmd-r", "cmd-q", "cmd-[", "cmd-]")
    } else {
        ("ctrl-enter", "ctrl-r", "ctrl-q", "ctrl-[", "ctrl-]")
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
        KeyBinding::new("n", NextComment, DIFF),
        KeyBinding::new("p", PreviousComment, DIFF),
        KeyBinding::new(back, JumpBack, DIFF),
        KeyBinding::new(forward, JumpForward, DIFF),
        KeyBinding::new("/", OpenSearch, DIFF),
        KeyBinding::new("space", PickerToggle, Some("CommitList")),
        KeyBinding::new("escape", CancelDraft, REVIEW),
        KeyBinding::new(submit, SubmitDraft, REVIEW),
        KeyBinding::new(refresh, Refresh, REVIEW),
        KeyBinding::new(quit, Quit, REVIEW),
    ]);

    // Quit must work whatever holds focus. The root view handles it too,
    // but that ride depends on the window's dispatch path; this global
    // fallback fires when no element on that path claimed the action.
    cx.on_action(|_: &Quit, cx| cx.quit());

    // After bind_keys: the menu reads the keymap once, at set time, to put
    // chords like Cmd+Q on the items. Only macOS draws the bar; Linux and
    // Windows just store the declaration, so the call needs no cfg.
    install_menus(cx);
}

fn install_menus(cx: &mut App) {
    use gpui_kit::component::input;
    use gpui_kit::{Menu, MenuItem, OsAction, SystemMenuType};

    cx.set_menus([
        // The app menu. macOS titles it with the process name, so the name
        // here never shows.
        Menu::new("diffie").items([
            MenuItem::action("About diffie", MenuAbout),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Quit diffie", Quit),
        ]),
        // The OS actions carry the native selectors, so the items act on the
        // focused text field; the Kit input's own actions back them.
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", input::Undo, OsAction::Undo),
            MenuItem::os_action("Redo", input::Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", input::Cut, OsAction::Cut),
            MenuItem::os_action("Copy", input::Copy, OsAction::Copy),
            MenuItem::os_action("Paste", input::Paste, OsAction::Paste),
            MenuItem::os_action("Select All", input::SelectAll, OsAction::SelectAll),
        ]),
        Menu::new("View").items([
            MenuItem::action("Reload", Refresh),
            MenuItem::separator(),
            MenuItem::action("Split | Unified", MenuSplitUnified),
            MenuItem::action("Wrap long lines", MenuWrap),
            MenuItem::action("Hide whitespace-only changes", MenuHideWhitespace),
            MenuItem::separator(),
            MenuItem::action("Collapse all files", MenuCollapseAll),
            MenuItem::action("Expand all files", MenuExpandAll),
            MenuItem::separator(),
            MenuItem::action("File list", MenuFileList),
            MenuItem::action("Earlier comments", MenuEarlierComments),
        ]),
        Menu::new("Go").items([
            MenuItem::action("Next change", MenuNextChange),
            MenuItem::action("Previous change", MenuPreviousChange),
            MenuItem::action("Next file", MenuNextFile),
            MenuItem::action("Previous file", MenuPreviousFile),
            MenuItem::action("Next comment", MenuNextComment),
            MenuItem::action("Previous comment", MenuPreviousComment),
            MenuItem::separator(),
            MenuItem::action("Back", MenuJumpBack),
            MenuItem::action("Forward", MenuJumpForward),
            MenuItem::separator(),
            MenuItem::action("New for me", MenuScopeNewForMe),
            MenuItem::action("Whole branch", MenuScopeWholeBranch),
            MenuItem::action("Custom", MenuScopeCustom),
        ]),
        // Exactly "Window": macOS recognises the name and fills the menu
        // with the standard window items.
        Menu::new("Window"),
    ]);
}

/// The standard About panel, with the name and version the dev binary has no
/// bundle to carry. Only macOS draws the menu bar today, so elsewhere the
/// action that reaches this is a no-op.
fn show_about() {
    #[cfg(target_os = "macos")]
    {
        use objc2::MainThreadMarker;
        use objc2::runtime::AnyObject;
        use objc2_app_kit::{
            NSAboutPanelOptionApplicationIcon, NSAboutPanelOptionApplicationName,
            NSAboutPanelOptionApplicationVersion, NSApplication,
        };
        use objc2_foundation::{NSDictionary, NSString};

        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        let name = NSString::from_str("diffie");
        let version = NSString::from_str(env!("CARGO_PKG_VERSION"));
        // The panel resolves its icon through the bundle, not through the
        // Dock image set at startup, so without this it draws a generic one.
        let icon = app.applicationIconImage();
        // Safety: the extern option keys exist whenever AppKit is linked,
        // and the values are the types the panel documents.
        unsafe {
            let mut keys = vec![
                NSAboutPanelOptionApplicationName,
                NSAboutPanelOptionApplicationVersion,
            ];
            let mut values: Vec<&AnyObject> = vec![&name, &version];
            if let Some(icon) = icon.as_deref() {
                keys.push(NSAboutPanelOptionApplicationIcon);
                values.push(icon);
            }
            let options = NSDictionary::from_slices(&keys, &values);
            app.orderFrontStandardAboutPanelWithOptions(&options);
        }
    }
}

/// The orange that marks everything comment-related. It rides on the
/// `chart.4` token because the Kit has no dedicated comment token; every
/// embedded theme sets `chart.4` to its comment orange.
fn comment_accent(cx: &App) -> Hsla {
    cx.theme().chart_4
}

/// The app's side enum mapped onto the Kit's. Both types are foreign, so
/// `From` cannot bridge them here.
fn diff_side(side: Side) -> DiffSide {
    match side {
        Side::Old => DiffSide::Original,
        Side::New => DiffSide::Modified,
    }
}

/// Per-theme fills the Kit has no tokens for, in one table so adding a theme
/// means extending one match.
struct ThemePalette {
    /// Added and removed row fills. The Diff's own tint is `success`/`danger`
    /// at a fixed 12% over the background, which can only produce washed-out
    /// pastels; the mockup wants dark saturated fills, so the app paints its
    /// own row decorations and keeps the tint off.
    added: Hsla,
    removed: Hsla,
    /// The fill behind a line a search hit sits on. Amber, the conventional
    /// search colour, so a hit stands apart from the green and red diff
    /// fills and the blue accent. A faint tint was rejected: it drowned in
    /// the added and removed fills.
    search_line: Hsla,
    /// The matched characters inside a hit line: a saturated amber step
    /// above `search_line`, so the eye lands on the match itself, with dark
    /// text because light-on-amber reads poorly in every theme.
    search_match: Hsla,
    search_match_foreground: Hsla,
}

/// Unknown themes fall back to token washes: the tint the Diff would have
/// given for rows, and warning-based search colors.
fn palette(cx: &App) -> ThemePalette {
    let theme = cx.theme();
    let (added, removed, search_line, search_match, search_match_foreground) =
        match theme.theme_name().as_ref() {
            "Midnight" => (0x10291c, 0x33161a, 0x4a3a10, 0xffc247, 0x241a02),
            "Gruvbox Dark" => (0x3d4220, 0x472322, 0x6b5618, 0xfabd2f, 0x282828),
            "Light" | "Default Light" => (0xe6ffec, 0xffebe9, 0xffd866, 0xf0940a, 0x231302),
            _ => {
                let wash = |over: Hsla| {
                    let mut color = over;
                    color.a = 0.12;
                    color
                };
                return ThemePalette {
                    added: wash(theme.success),
                    removed: wash(theme.danger),
                    search_line: theme.warning.opacity(0.35),
                    search_match: theme.warning,
                    search_match_foreground: theme.background,
                };
            }
        };
    ThemePalette {
        added: rgb(added).into(),
        removed: rgb(removed).into(),
        search_line: rgb(search_line).into(),
        search_match: rgb(search_match).into(),
        search_match_foreground: rgb(search_match_foreground).into(),
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

/// One faint vertical line per ancestor level, under that ancestor's icon
/// center. Absolute, so rows keep their flex layout; every row paints its
/// own segment and the stack reads as one continuous guide.
fn indent_guides(depth: usize, cx: &App) -> impl Iterator<Item = gpui_kit::Div> {
    let color = cx.theme().border;
    (0..depth).map(move |level| {
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(10. + level as f32 * 10.))
            .w(px(1.))
            .bg(color)
    })
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

/// One group of the commit picker: its heading row and the commits it lists.
struct PickerSection {
    title: &'static str,
    range: std::ops::Range<usize>,
    heading_color: Hsla,
    bordered: bool,
    expanded: bool,
    with_uncommitted: bool,
}

/// What every picker section shares: the typed filter, which commits read as
/// skipped, the comment count per commit, and one clock reading for the age
/// column.
struct PickerContext {
    query: String,
    skipped: HashSet<Oid>,
    comment_counts: HashMap<Oid, usize>,
    now: i64,
}

/// What a window reviews, as the command line settled it.
pub struct Subject {
    pub repository: SharedString,
    /// The linked worktree the review runs in, when it is not the primary
    /// checkout.
    pub worktree: Option<SharedString>,
    pub comparison: Comparison,
    pub pull: Option<Pull>,
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
    /// The commented code was rewritten since: the comment sits at the
    /// replacement, marked "code changed", so the reviewer can judge the fix.
    changed_since: bool,
}

impl Placed {
    /// Comments from earlier rounds - outside the picked commits, or on code
    /// rewritten since. Both hide behind the earlier-comments toggle.
    fn earlier(&self) -> bool {
        self.other_revision || self.changed_since
    }
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
    uncommitted_paths: HashSet<String>,
    /// The picks after an all-picked custom scope took in the commits the
    /// branch gained; `None` leaves the picker untouched.
    picked: Option<Vec<Oid>>,
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
    /// One per line holding resolved threads, folded or not: the marker
    /// in the gutter lane that toggles their cards.
    resolved_markers: Vec<ResolvedMarker>,
    /// The line under the pointer; its marker brightens.
    hovered_line: Option<DiffLinePosition>,
}

/// A line's resolved threads, behind one gutter-lane marker.
#[derive(Clone)]
struct ResolvedMarker {
    path: String,
    side: Side,
    line: usize,
    /// The comment ids the marker's click toggles.
    ids: Vec<String>,
    /// True while at least one of the cards is hidden: a click shows
    /// them; otherwise it folds them away again.
    folded: bool,
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
    /// On code rewritten since the comment; shown with the stored snippet
    /// so the reviewer can compare it with the fix and react.
    changed_since: bool,
    snippet: Option<SharedString>,
    /// The threaded answers, oldest first.
    replies: Vec<ReplyView>,
    /// Someone else answered: worth a tag when the code did not change,
    /// because a question can be settled by an answer alone.
    answered: bool,
    mine: bool,
    resolved: bool,
    /// The comment's page on the pull request once published, and whether
    /// an edit since then still waits for the next publish.
    published: Option<(SharedString, bool)>,
    /// Kind, how many reviewers gave it, whether this reviewer did.
    reactions: Vec<(ReactionKind, usize, bool)>,
    left: Vec<ReactionKind>,
    /// The open answer box, when it belongs to this thread.
    answer_input: Option<Entity<TextareaState>>,
    /// The diff's soft-wrap toggle; the quoted snippet follows it.
    wrap: bool,
    /// The comment the jump stepper is on; its card's border leaps out.
    active: bool,
    /// In the fold animation's last moment: the card fades out.
    fading: bool,
}

struct ReplyView {
    author: SharedString,
    body: SharedString,
    /// The agent conversation behind the answer, shown as a robot badge
    /// offering `claude --resume`.
    session: Option<SharedString>,
}

/// The reviewer's answer box, open under one thread at a time.
struct Answer {
    comment: String,
    input: Entity<TextareaState>,
    _submit: Subscription,
}

struct FileView {
    state: ReviewState,
    status: FileStatus,
    note: Option<SharedString>,
    additions: usize,
    deletions: usize,
    binary: bool,
    generated: bool,
    /// Checked commits that touched this file, and how many are checked.
    touched: usize,
    in_scope: usize,
    /// The diff on screen carries uncommitted changes of this file.
    uncommitted: bool,
    /// Short hash of the commit that deleted the file. `None` while the
    /// deletion only exists in the working tree.
    removed_at: Option<SharedString>,
    comments: usize,
    other_revision: usize,
    /// How many of the comments sit on code rewritten since they were made.
    changed_code_comments: usize,
}

pub struct ReviewApp {
    backend: Arc<dyn ReviewBackend>,
    repository: SharedString,
    /// The linked worktree the review runs in, when it is not the primary
    /// checkout; shown beside the project name.
    worktree: Option<SharedString>,
    comparison: Comparison,
    pull: Option<Pull>,
    /// A publish or approval is on its way to the forge; one at a time.
    publishing: bool,
    /// How the last publish went, and whether that was a failure. Its own
    /// line beside `notice`, which the next review save clears - and every
    /// publish ends in one.
    publish_status: Option<(SharedString, bool)>,
    /// Where the pull request stands, as last answered by the forge. Kept
    /// through a failed refresh: an older answer beats none.
    pull_status: Option<PullStatus>,
    /// Why the last refresh failed, in the forge's words.
    pull_status_error: Option<SharedString>,
    /// Replacing it drops the refresh in flight, so answers never land
    /// out of order.
    _pull_status: Task<()>,
    scope: ScopeKind,
    picked: Vec<Oid>,
    /// Whether the uncommitted layer of an uncommitted review is checked in
    /// the commit picker. Like `picked`, only the Custom scope reads it.
    picked_uncommitted: bool,
    /// Files whose working-tree content differs from `head`: the ones the
    /// uncommitted layer touches. Empty for a committed comparison.
    uncommitted_paths: HashSet<String>,
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
    /// The machine-wide settings file's state, kept current and written
    /// behind `persist_settings`'s coalescing delay.
    settings: Settings,
    settings_pending: bool,
    settings_running: bool,
    /// Soft-wrap long lines instead of scrolling the diff horizontally.
    wrap: bool,
    /// Hide changes in amount of whitespace. The diff is rebuilt with
    /// `--ignore-all-space`, so whitespace-only files drop out of the view.
    ignore_whitespace: bool,
    /// The comment `n`/`p` last jumped to, so the walk continues from
    /// there instead of restarting at the top.
    comment_cursor: Option<String>,
    /// Resolved threads whose cards are open anyway: the fold grace after
    /// a verdict, and marker clicks, put ids here. Session-only on
    /// purpose - a restart folds every resolved thread again.
    unfolded: HashSet<String>,
    /// Resolved threads in the fold animation's fade.
    fading: HashSet<String>,
    /// Pages left behind by jumps, each keyed by its top row, newest last.
    /// Cmd/Ctrl+[ pops back through them; plain scrolling never pushes.
    back_stack: Vec<DiffLinePosition>,
    /// Pages left by going back, so Cmd/Ctrl+] retraces them. A fresh
    /// jump clears it: forward dies with new navigation, as in a browser.
    forward_stack: Vec<DiffLinePosition>,
    search_open: bool,
    search_query: Entity<InputState>,
    /// Extend the search past the picked commits to the whole branch.
    search_all: bool,
    /// The scoped patch's text, kept so the search sees what the view shows.
    patch_text: Arc<String>,
    /// The whole-branch patch, fetched when the reviewer extends the search;
    /// keyed by the load revision that fetched it, so a reload drops it.
    whole_patch: Option<(usize, Arc<String>)>,
    search_hits: Vec<SearchHit>,
    search_all_hits: Vec<SearchHit>,
    search_cursor: Option<usize>,
    /// A search hit to land on after the reload a scope widening triggers.
    pending_jump: Option<(String, Side, usize)>,
    /// Land on the first open comment after the reload a comment jump
    /// triggered by widening the scope.
    pending_comment_jump: bool,
    _whole_patch: Task<()>,
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
    /// What the context menu's Comment will act on, captured when the drag
    /// releases. The menu must not consult the live selection: the
    /// right-click that opens it and the click that picks an item both
    /// clear the selection first.
    pending_comment: Option<(DiffLineRange, Option<String>)>,
    show_all_new: bool,
    show_all_reviewed: bool,
    draft: Option<Draft>,
    answer: Option<Answer>,
    current_file: Option<SharedString>,
    show_other_revisions: bool,
    sidebar_visible: bool,
    loading: bool,
    error: Option<SharedString>,
    /// A failed side task (a save, the whole-branch patch, a blame), said in
    /// the status bar without replacing a working diff. `error` stays
    /// reserved for a failed load, which leaves nothing to show.
    notice: Option<SharedString>,
    revision: usize,
    /// Bumped on every change to the reviewer's own file. A finishing load
    /// compares it with the value it started from, so a reload never rolls
    /// back what the reviewer did while it ran.
    review_generation: usize,
    /// One background write at a time: parallel detached writes to the same
    /// path can land out of order and leave stale content on disk. A save
    /// during a save only marks pending; the follow-up write clones the file
    /// at its newest.
    save_pending: bool,
    save_running: bool,
    /// Discards finished searches a newer query has outrun.
    search_generation: usize,
    _search: Task<()>,
    _load: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl ReviewApp {
    pub fn new(
        backend: Arc<dyn ReviewBackend>,
        subject: Subject,
        settings: Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let Subject {
            repository,
            worktree,
            comparison,
            pull,
        } = subject;
        window.set_window_title(&match &worktree {
            Some(name) => format!("{repository} ({name}) — review"),
            None => format!("{repository} — review"),
        });
        Theme::change(ThemeMode::Dark, Some(window), cx);
        let mode = match settings.mode {
            Some(LayoutMode::Unified) => DiffMode::Unified,
            _ => DiffMode::Split,
        };
        let diff = cx.new(|cx| DiffState::new([], cx).with_mode(mode));
        let picker_query =
            cx.new(|cx| InputState::new(window, cx).placeholder("Message, hash or author"));
        let search_query =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search the changeset"));
        let subscriptions = vec![
            // Fires on every move and resize tick; the settings writer
            // coalesces behind its delay. Maximized and fullscreen report
            // their restore bounds, which would overwrite the remembered
            // windowed ones, so only windowed bounds are taken.
            cx.observe_window_bounds(window, |this, window, cx| {
                let WindowBounds::Windowed(bounds) = window.window_bounds() else {
                    return;
                };
                let window = WindowSettings {
                    x: f32::from(bounds.origin.x),
                    y: f32::from(bounds.origin.y),
                    width: f32::from(bounds.size.width),
                    height: f32::from(bounds.size.height),
                };
                if this.settings.window != Some(window) {
                    this.settings.window = Some(window);
                    this.persist_settings(cx);
                }
            }),
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
            cx.subscribe(
                &search_query,
                |this, _, event: &InputEvent, cx| match event {
                    InputEvent::Change => this.run_search(cx),
                    InputEvent::PressEnter { shift, .. } => this.search_step(!shift, cx),
                    _ => {}
                },
            ),
        ];
        diff.read(cx).focus_handle(cx).focus(window, cx);
        let mut this = Self {
            backend,
            repository,
            worktree,
            comparison,
            pull,
            publishing: false,
            publish_status: None,
            pull_status: None,
            pull_status_error: None,
            _pull_status: Task::ready(()),
            scope: ScopeKind::NewForMe,
            picked: Vec::new(),
            picked_uncommitted: true,
            uncommitted_paths: HashSet::new(),
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
            manual_mode: mode,
            settings,
            settings_pending: false,
            settings_running: false,
            wrap: false,
            ignore_whitespace: false,
            comment_cursor: None,
            unfolded: HashSet::new(),
            fading: HashSet::new(),
            back_stack: Vec::new(),
            forward_stack: Vec::new(),
            search_open: false,
            search_query,
            search_all: false,
            patch_text: Arc::default(),
            whole_patch: None,
            search_hits: Vec::new(),
            search_all_hits: Vec::new(),
            search_cursor: None,
            pending_jump: None,
            pending_comment_jump: false,
            _whole_patch: Task::ready(()),
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
            pending_comment: None,
            show_all_new: false,
            show_all_reviewed: false,
            draft: None,
            answer: None,
            current_file: None,
            // On by default: the owner wants other revisions' comments in
            // view unless they are toggled away.
            show_other_revisions: true,
            sidebar_visible: true,
            loading: false,
            error: None,
            notice: None,
            revision: 0,
            review_generation: 0,
            save_pending: false,
            save_running: false,
            search_generation: 0,
            _search: Task::ready(()),
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
            ScopeKind::Custom => Scope::Commits {
                picked: self.picked.clone(),
                uncommitted: self.comparison.uncommitted && self.picked_uncommitted,
            },
        }
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.revision += 1;
        let revision = self.revision;
        self.loading = true;
        self.error = None;
        self.notice = None;
        self.draft = None;
        let backend = self.backend.clone();
        let comparison = self.comparison.clone();
        let scope = self.core_scope();
        // A reload can find commits HEAD gained since the last load. Picking
        // every commit means "everything", so the load re-picks the fresh
        // list and the newcomers join the scope; a hand-picked subset stays
        // exactly as picked.
        // Whoever reloads wants the present: the pull request may have been
        // merged, approved or turned red since the last look.
        self.refresh_pull_status(cx);
        let extend_picks = self.scope == ScopeKind::Custom
            && self
                .commits
                .iter()
                .all(|commit| self.picked.contains(&commit.id));
        let mine = self.reviews.as_ref().map(|set| set.mine.clone());
        let generation = self.review_generation;
        let ignore_whitespace = self.ignore_whitespace;
        tracing::debug!(revision, "reload requested");
        self._load = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let result = load(
                        &*backend,
                        &comparison,
                        &scope,
                        extend_picks,
                        mine,
                        ignore_whitespace,
                    );
                    tracing::debug!(ok = result.is_ok(), "load finished");
                    result
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.install(revision, generation, result, cx)
            });
        });
        cx.notify();
    }

    fn install(
        &mut self,
        revision: usize,
        generation: usize,
        result: Result<Loaded, String>,
        cx: &mut Context<Self>,
    ) {
        // A newer request is on its way; this answer is stale.
        if self.revision != revision {
            return;
        }
        self.loading = false;
        match result {
            Ok(mut loaded) => {
                self.patch_text = Arc::new(std::mem::take(&mut loaded.patch.text));
                // The whole-branch cache belongs to the load that made it.
                self.whole_patch = self
                    .whole_patch
                    .take()
                    .filter(|(cached, _)| *cached == revision);
                let reviewed: Vec<String> = loaded
                    .patch
                    .files
                    .iter()
                    .filter(|file| {
                        let blob = loaded.blobs.get(&file.path).and_then(Option::as_ref);
                        // A kept file is ticked too, but its open comments
                        // are the point of showing it: never collapse it.
                        file.state != FileState::KeptForComments
                            && (file.state == FileState::UnchangedSinceReview
                                || loaded.reviews.mine.review_state(&file.path, blob)
                                    == ReviewState::Reviewed)
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
                // A removed file's diff is all deletions; the header says so,
                // and expanding it is opt-in like a generated file.
                let removed: Vec<String> = loaded
                    .patch
                    .files
                    .iter()
                    .filter(|file| file.status == FileStatus::Deleted)
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
                            .chain(&removed)
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
                if let Some(picked) = loaded.picked {
                    self.picked = picked;
                }
                self.runs = loaded.runs;
                self.files = Arc::new(loaded.patch.files);
                self.branch = loaded.branch;
                self.touched = loaded.touched;
                self.uncommitted_paths = loaded.uncommitted_paths;
                self.new_side = Some(loaded.patch.new_side);
                self.blobs = loaded.blobs;
                // The reviewer kept working while the load ran: the live
                // review file is newer than the snapshot the load carried,
                // and installing that snapshot would wipe those edits from
                // memory - and from disk with the next save. Keep the live
                // file; placements for comments the load never saw ride
                // along from the old view and correct on the next reload.
                if self.review_generation != generation {
                    let known: HashSet<String> = loaded
                        .reviews
                        .threads()
                        .iter()
                        .map(|thread| thread.comment.id.clone())
                        .collect();
                    if let Some(live) = self.reviews.take() {
                        loaded.reviews.mine = live.mine;
                    }
                    loaded.placed.extend(
                        self.placed
                            .drain(..)
                            .filter(|placed| !known.contains(&placed.id)),
                    );
                }
                self.reviews = Some(loaded.reviews);
                self.review_path = loaded.review_path;
                self.placed = loaded.placed;
                // A fresh view reads from the top, so the first file still
                // open for review - expanded and unticked - is the one `r`
                // and the file stepper act on until the reader moves. The
                // collapsed headers leading the list (reviewed, generated,
                // removed) are not being read, and with no current file at
                // all, `r` on the opening screen did nothing.
                self.current_file = {
                    let diff = self.diff.read(cx);
                    diff.files()
                        .iter()
                        .map(|file| file.path().clone())
                        .find(|path| {
                            !diff.is_file_collapsed(path)
                                && self.review_state(path) != ReviewState::Reviewed
                        })
                };
                if loaded.completed_cleared {
                    self.save_review(cx);
                }
                if let Some((path, side, line)) = self.pending_jump.take() {
                    self.show_hit(&path, side, line, cx);
                }
                if std::mem::take(&mut self.pending_comment_jump) {
                    self.comment_cursor = None;
                    self.jump_comment(true, cx);
                }
                if self.search_open {
                    self.run_search(cx);
                }
            }
            Err(error) => {
                self.diff.update(cx, |state, cx| state.set_files([], cx));
                self.files = Arc::default();
                self.placed.clear();
                self.pending_jump = None;
                self.pending_comment_jump = false;
                self.error = Some(error.into());
            }
        }
        cx.notify();
    }

    /// Persist the reviewer's own file off the main thread.
    ///
    /// Every change to the file funnels through here, which makes this the
    /// place the generation ticks (see `review_generation`).
    fn save_review(&mut self, cx: &mut Context<Self>) {
        self.review_generation += 1;
        self.save_pending = true;
        self.write_review(cx);
    }

    fn write_review(&mut self, cx: &mut Context<Self>) {
        if self.save_running || !self.save_pending {
            return;
        }
        let Some(mut file) = self.reviews.as_ref().map(|set| set.mine.clone()) else {
            self.save_pending = false;
            return;
        };
        self.save_pending = false;
        self.save_running = true;
        let path = self.review_path.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(
                    async move { file.save(&path).map_err(|error| error.to_string()) },
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                this.save_running = false;
                match result {
                    // A failed save must not replace a working diff; the
                    // status bar carries the complaint instead.
                    Err(error) => {
                        this.notice = Some(format!("review not saved: {error}").into());
                    }
                    Ok(()) => this.notice = None,
                }
                this.write_review(cx);
                cx.notify();
            });
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

    /// Entering the custom scope from another starts from "all checked",
    /// uncommitted layer included; unchecking then narrows from there.
    fn enter_custom(&mut self) {
        if self.scope != ScopeKind::Custom {
            self.picked = self
                .commits
                .iter()
                .map(|commit| commit.id.clone())
                .collect();
            self.picked_uncommitted = true;
        }
        self.scope = ScopeKind::Custom;
    }

    fn toggle_commit(&mut self, id: &Oid, cx: &mut Context<Self>) {
        self.enter_custom();
        match self.picked.iter().position(|picked| picked == id) {
            Some(index) => {
                self.picked.remove(index);
            }
            None => self.picked.push(id.clone()),
        }
        self.reload(cx);
    }

    fn toggle_uncommitted(&mut self, cx: &mut Context<Self>) {
        self.enter_custom();
        self.picked_uncommitted = !self.picked_uncommitted;
        self.reload(cx);
    }

    fn pick_many(&mut self, ids: Vec<Oid>, checked: bool, cx: &mut Context<Self>) {
        self.enter_custom();
        self.picked.retain(|picked| !ids.contains(picked));
        if checked {
            self.picked.extend(ids);
        }
        self.reload(cx);
    }

    /// A click on a picker row. Shift checks the run from the last toggled
    /// commit; a plain click toggles one.
    fn picker_click(&mut self, id: &Oid, shift: bool, cx: &mut Context<Self>) {
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
        // A publish in flight records its ids into the review file it
        // started from; swapping would hand it the other direction's.
        if self.publishing {
            return;
        }
        std::mem::swap(&mut self.comparison.source, &mut self.comparison.target);
        // A pinned revision (--pr) belongs to its display name; keep the
        // pairs together.
        std::mem::swap(
            &mut self.comparison.source_rev,
            &mut self.comparison.target_rev,
        );
        self.picked.clear();
        self.picked_uncommitted = true;
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
        let side = diff_side(placed.side);
        let position = DiffLinePosition::new(placed.path.clone(), side, placed.line);
        self.remember_position(cx);
        self.diff
            .update(cx, |state, cx| state.scroll_to_line(position, cx));
        self.current_file = Some(path.clone());
        cx.notify();
    }

    /// Comments in reading order: files as the diff sorts them, then line,
    /// the old side before the new. Resolved threads count - the walk is
    /// for reading, and must not die with the last verdict. Comments hidden
    /// with the other revisions' toggle are skipped; a jump must land on
    /// something visible.
    fn comment_stops(&self) -> Vec<(String, String, Side, usize)> {
        let order: HashMap<&str, usize> = self
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| (file.path.as_str(), index))
            .collect();
        // The walk is for working through what is still open - done is
        // done (owner, 2026-11-16, reversing the resolved-included walk of
        // 2026-11-13: folded cards are reachable by their line markers).
        let resolved: HashSet<&str> = self
            .reviews
            .as_ref()
            .map(|set| {
                set.threads()
                    .iter()
                    .filter(|thread| thread.resolved())
                    .map(|thread| thread.comment.id.as_str())
                    .collect()
            })
            .unwrap_or_default();
        let mut stops: Vec<(String, String, Side, usize)> = self
            .placed
            .iter()
            .filter(|placed| self.show_other_revisions || !placed.earlier())
            .filter(|placed| !resolved.contains(placed.id.as_str()))
            .map(|placed| {
                (
                    placed.id.clone(),
                    placed.path.clone(),
                    placed.side,
                    placed.line,
                )
            })
            .collect();
        stops.sort_by_key(|(_, path, side, line)| {
            (
                order.get(path.as_str()).copied().unwrap_or(usize::MAX),
                *line,
                *side == Side::New,
            )
        });
        stops
    }

    fn jump_comment(&mut self, forward: bool, cx: &mut Context<Self>) {
        let mut stops = self.comment_stops();
        // A jump is an explicit ask to see a comment, so it clears what
        // hides them instead of dying silently: first the earlier-comments
        // toggle, then - like a search hit outside the view - the scope.
        if stops.is_empty() && !self.show_other_revisions {
            self.show_other_revisions = true;
            stops = self.comment_stops();
            if stops.is_empty() {
                self.show_other_revisions = false;
            }
        }
        if stops.is_empty() {
            // Only open threads justify hunting: a review where everything
            // is resolved has no stop a wider scope could surface.
            let any = self
                .reviews
                .as_ref()
                .is_some_and(|set| set.threads().iter().any(|thread| !thread.resolved()));
            if any && self.scope != ScopeKind::WholeBranch {
                self.pending_comment_jump = true;
                self.set_scope(ScopeKind::WholeBranch, cx);
            }
            // Still nothing at the whole branch: the comments' files are not
            // in this comparison at all; the status bar says so.
            return;
        }
        // A cursor the reader can still see steps to its neighbour; one
        // scrolled off the page is abandoned and the walk re-anchors at the
        // viewport, mirroring the change keys, so after paging away the key
        // guides within the page the reader is looking at.
        let at = self.comment_cursor.as_ref().and_then(|id| {
            let index = stops.iter().position(|(sid, ..)| sid == id)?;
            let (_, path, side, line) = &stops[index];
            let position =
                DiffLinePosition::new(SharedString::from(path.clone()), diff_side(*side), *line);
            self.diff
                .read(cx)
                .is_line_visible(&position)
                .then_some(index)
        });
        // The walk stops at the ends instead of circling: wrapping around
        // made it impossible to feel where the list ends.
        let next = match (at, forward) {
            (Some(index), true) if index + 1 >= stops.len() => return,
            (Some(index), true) => index + 1,
            (Some(0), false) => return,
            (Some(index), false) => index - 1,
            (None, forward) => {
                let order: HashMap<&str, usize> = self
                    .files
                    .iter()
                    .enumerate()
                    .map(|(index, file)| (file.path.as_str(), index))
                    .collect();
                let key = |path: &str, new_side: bool, line: usize| {
                    (
                        order.get(path).copied().unwrap_or(usize::MAX),
                        line,
                        new_side,
                    )
                };
                let top = self.diff.read(cx).top_line().map(|position| {
                    key(
                        position.path().as_ref(),
                        position.side() == DiffSide::Modified,
                        position.line(),
                    )
                });
                let stop_key = |(_, path, side, line): &(String, String, Side, usize)| {
                    key(path, *side == Side::New, *line)
                };
                match (top, forward) {
                    (None, true) => 0,
                    (None, false) => stops.len() - 1,
                    (Some(top), true) => {
                        match stops.iter().position(|stop| stop_key(stop) >= top) {
                            Some(index) => index,
                            None => return,
                        }
                    }
                    (Some(top), false) => {
                        match stops.iter().rposition(|stop| stop_key(stop) < top) {
                            Some(index) => index,
                            None => return,
                        }
                    }
                }
            }
        };
        let (id, path, side, line) = stops[next].clone();
        let side = diff_side(side);
        self.comment_cursor = Some(id);
        let path = SharedString::from(path);
        self.remember_position(cx);
        self.diff.update(cx, |state, cx| {
            state.set_file_collapsed(&path, false, cx);
            // The focus marker shows where the jump landed, as the change
            // keys do for their groups.
            state.set_focused_ranges([DiffLineRange::new(path.clone(), side, line, line)], cx);
            state.scroll_to_line(DiffLinePosition::new(path.clone(), side, line), cx);
        });
        self.current_file = Some(path);
        cx.notify();
    }

    /// Every jump calls this before it scrolls: the page being left goes
    /// onto the back stack, so an accidental `n` or `]` - or any jump off
    /// a spot reached by plain scrolling - can be taken back. The spot a
    /// jump lands on is remembered only when the next jump leaves it, so
    /// the stack holds pages the reader was at, not every stop passed.
    fn remember_position(&mut self, cx: &mut Context<Self>) {
        // A fresh jump orphans the forward trail, as in a browser: forward
        // only ever retraces back steps.
        self.forward_stack.clear();
        if let Some(top) = self.diff.read(cx).top_line() {
            Self::push_page(&mut self.back_stack, top);
        }
    }

    /// Newest page last, deduped against the top. Bounded so a day-long
    /// review cannot grow a stack without end; nobody steps back a
    /// hundred times.
    fn push_page(stack: &mut Vec<DiffLinePosition>, page: DiffLinePosition) {
        if stack.last() == Some(&page) {
            return;
        }
        stack.push(page);
        if stack.len() > 100 {
            stack.remove(0);
        }
    }

    /// Cmd/Ctrl+[: return to the page the last jump tore the reader away
    /// from, exactly as it stood (the Kit's `scroll_to_top_line` restores
    /// what `top_line` read). The page being left goes onto the forward
    /// stack, so Cmd/Ctrl+] retraces the steps.
    fn jump_back(&mut self, cx: &mut Context<Self>) {
        self.step_history(true, cx);
    }

    fn jump_forward(&mut self, cx: &mut Context<Self>) {
        self.step_history(false, cx);
    }

    /// Pops `back`'s stack and pushes the page being left onto the other,
    /// so the two keys walk one history. Entries whose file left the
    /// comparison are skipped, never guessed at; `current_file` follows
    /// so `r` acts on the page come back to.
    fn step_history(&mut self, back: bool, cx: &mut Context<Self>) {
        let top = self.diff.read(cx).top_line();
        let paths: Vec<SharedString> = self
            .diff
            .read(cx)
            .files()
            .iter()
            .map(|file| file.path().clone())
            .collect();
        let (from, to) = if back {
            (&mut self.back_stack, &mut self.forward_stack)
        } else {
            (&mut self.forward_stack, &mut self.back_stack)
        };
        while let Some(position) = from.pop() {
            if !paths.iter().any(|path| path == position.path()) {
                continue;
            }
            if let Some(top) = top.clone() {
                Self::push_page(to, top);
            }
            self.current_file = Some(position.path().clone());
            self.diff
                .update(cx, |state, cx| state.scroll_to_top_line(position, cx));
            cx.notify();
            return;
        }
    }

    /// `r` and the file stepper act on `current_file`, but a change key
    /// moves the focus mark inside the Kit; pull the file it landed in
    /// back out, or `r` ticks the file the reader left behind. A comment
    /// jump needs no follow-up: it sets `current_file` itself.
    fn follow_mark(&mut self, cx: &mut Context<Self>) {
        if let Some(range) = self.diff.read(cx).focused_ranges().first() {
            self.current_file = Some(range.path().clone());
            cx.notify();
        }
    }

    fn reveal_file(&mut self, path: SharedString, cx: &mut Context<Self>) {
        self.remember_position(cx);
        self.diff.update(cx, |state, cx| {
            let was = state.is_file_collapsed(&path);
            state.set_file_collapsed(&path, false, cx);
            tracing::trace!(%path, was_collapsed = was, "reveal file");
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
        let stored = match mode {
            DiffMode::Unified => LayoutMode::Unified,
            _ => LayoutMode::Split,
        };
        if self.settings.mode != Some(stored) {
            self.settings.mode = Some(stored);
            self.persist_settings(cx);
        }
    }

    /// Settings writes ride the review file's pattern - a dirty flag and
    /// one writer - plus a pause before each write, because a window drag
    /// reports every frame and the file deserves one write per gesture,
    /// not one per pixel. The state is read after the pause, so the write
    /// carries the newest of everything.
    fn persist_settings(&mut self, cx: &mut Context<Self>) {
        self.settings_pending = true;
        self.write_settings(cx);
    }

    fn write_settings(&mut self, cx: &mut Context<Self>) {
        if self.settings_running || !self.settings_pending {
            return;
        }
        self.settings_running = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SETTINGS_DELAY).await;
            let Ok(settings) = this.update(cx, |this, _| {
                this.settings_pending = false;
                this.settings.clone()
            }) else {
                return;
            };
            let result = cx.background_spawn(async move { settings.save() }).await;
            let _ = this.update(cx, |this, cx| {
                this.settings_running = false;
                if let Err(error) = result {
                    this.notice = Some(format!("settings not saved: {error}").into());
                    cx.notify();
                }
                this.write_settings(cx);
            });
        })
        .detach();
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

    // One body per toggle: the title-bar button, the shortcut and the menu
    // item must never drift apart.

    fn toggle_wrap(&mut self, cx: &mut Context<Self>) {
        self.wrap = !self.wrap;
        cx.notify();
    }

    fn toggle_whitespace(&mut self, cx: &mut Context<Self>) {
        self.ignore_whitespace = !self.ignore_whitespace;
        // The patch itself changes, not just the rendering: a reload is
        // required.
        self.reload(cx);
    }

    fn toggle_other_revisions(&mut self, cx: &mut Context<Self>) {
        self.show_other_revisions = !self.show_other_revisions;
        cx.notify();
    }

    fn set_sidebar(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.sidebar_visible = visible;
        cx.notify();
    }

    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.set_sidebar(!self.sidebar_visible, cx);
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
        let revision = self.revision;
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
                // A stale answer must not label the wrong box: a reload
                // changed what the lines mean, and a draft cancelled and
                // reopened elsewhere is a different question.
                if this.revision != revision {
                    return;
                }
                let commit = origin.map_or(fallback, |origin| origin.commit);
                if let Some(draft) = this.draft.as_mut()
                    && draft.editing.is_none()
                    && draft.range == range
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
        let side = diff_side(placed.side);
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

    /// Opens the answer box under a thread; any other open one closes, so
    /// Escape and the key context only ever juggle a single box.
    fn start_answer(&mut self, comment: &str, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 6)
                .placeholder("Respond…")
        });
        // Focus once the box is in the element tree, like the comment box.
        cx.on_next_frame(window, |this, window, cx| {
            if let Some(answer) = &this.answer {
                answer.input.update(cx, |state, cx| state.focus(window, cx));
            }
        });
        let submit = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter {
                secondary: true, ..
            } = event
            {
                this.submit_answer(window, cx);
            }
        });
        self.answer = Some(Answer {
            comment: comment.to_string(),
            input,
            _submit: submit,
        });
        cx.notify();
    }

    fn submit_answer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(answer) = self.answer.take() else {
            return;
        };
        let body = answer.input.read(cx).value().trim().to_string();
        if body.is_empty() {
            self.answer = Some(answer);
            return;
        }
        if let Some(set) = self.reviews.as_mut() {
            set.mine.add_reply(&answer.comment, body, None);
            self.save_review(cx);
        }
        self.focus_diff(window, cx);
        cx.notify();
    }

    fn focus_diff(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.diff.read(cx).focus_handle(cx).focus(window, cx);
    }

    fn cancel_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Escape peels one layer at a time: an answer box first, then a
        // draft, then the search.
        if self.answer.take().is_some() {
            self.focus_diff(window, cx);
            cx.notify();
            return;
        }
        if self.draft.is_none() && self.search_open {
            self.close_search(window, cx);
            return;
        }
        self.draft = None;
        self.focus_diff(window, cx);
        cx.notify();
    }

    fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = true;
        // A selection seeds the needle: select a name, press `/`, and the
        // search asks where else the changeset touches it.
        let selected = gpui_kit::base::TextSelection::selected_text(window, cx);
        let selected = selected.trim().to_string();
        if !selected.is_empty() && !selected.contains('\n') {
            self.search_query
                .update(cx, |input, cx| input.set_value(selected, window, cx));
        }
        self.search_query
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
        self.run_search(cx);
    }

    fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = false;
        self.search_cursor = None;
        self.focus_diff(window, cx);
        cx.notify();
    }

    fn run_search(&mut self, cx: &mut Context<Self>) {
        let needle = self.search_query.read(cx).value().trim().to_string();
        self.search_cursor = None;
        self.search_generation += 1;
        let generation = self.search_generation;
        if needle.is_empty() {
            // Nothing to scan; the bump above also outruns a scan in flight.
            self.search_hits = Vec::new();
            self.search_all_hits = Vec::new();
            cx.notify();
            return;
        }
        // The scan walks the whole patch text - tens of MB on a big branch -
        // so it leaves the UI thread; the generation discards outrun results.
        let patch = self.patch_text.clone();
        let whole = match &self.whole_patch {
            Some((revision, text)) if self.search_all && *revision == self.revision => {
                Some(text.clone())
            }
            _ => None,
        };
        self._search = cx.spawn(async move |this, cx| {
            let (hits, all_hits) = cx
                .background_spawn(async move {
                    let hits = search_patch(&patch, &needle);
                    let all_hits = whole
                        .map(|text| search_patch(&text, &needle))
                        .unwrap_or_default();
                    (hits, all_hits)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.search_generation != generation {
                    return;
                }
                this.search_hits = hits;
                this.search_all_hits = all_hits;
                cx.notify();
            });
        });
        cx.notify();
    }

    /// Fetch the whole-branch patch once per load so the extended search
    /// has something to walk; the scoped view stays untouched.
    fn ensure_whole_patch(&mut self, cx: &mut Context<Self>) {
        if self
            .whole_patch
            .as_ref()
            .is_some_and(|(revision, _)| *revision == self.revision)
        {
            return;
        }
        let backend = self.backend.clone();
        let comparison = self.comparison.clone();
        let revision = self.revision;
        let ignore_whitespace = self.ignore_whitespace;
        self._whole_patch = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let resolved = backend.resolve(&comparison)?;
                    backend.patch(
                        &resolved,
                        &Scope::WholeBranch,
                        ReviewInput::EMPTY,
                        ignore_whitespace,
                    )
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.revision != revision {
                    return;
                }
                match result {
                    Ok(patch) => {
                        this.whole_patch = Some((revision, Arc::new(patch.text)));
                        this.notice = None;
                        this.run_search(cx);
                    }
                    // The scoped view on screen is still good; only the
                    // extended search lost its data, so the status bar
                    // says it instead of replacing the diff.
                    Err(error) => {
                        this.notice =
                            Some(format!("search across all commits failed: {error}").into());
                        cx.notify();
                    }
                }
            });
        });
    }

    fn search_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let extended = self.search_all && self.scope != ScopeKind::WholeBranch;
        let hits = if extended {
            &self.search_all_hits
        } else {
            &self.search_hits
        };
        if hits.is_empty() {
            return;
        }
        let next = match (self.search_cursor, forward) {
            (Some(index), true) => (index + 1) % hits.len(),
            (Some(index), false) => (index + hits.len() - 1) % hits.len(),
            (None, true) => 0,
            (None, false) => hits.len() - 1,
        };
        self.search_cursor = Some(next);
        let hit = hits[next].clone();
        // In view means the scoped patch itself contains the hit; a ticked
        // file keeps a header-only entry in the file list, so a file-level
        // check would aim at lines the view does not hold.
        if self.search_hits.contains(&hit) {
            self.show_hit(&hit.path, hit.side, hit.line, cx);
        } else {
            // The hit sits outside the picked commits: widen to the whole
            // branch and land on it once the reload installs. An explicit
            // jump may change the scope; per-file state never does.
            self.pending_jump = Some((hit.path, hit.side, hit.line));
            self.set_scope(ScopeKind::WholeBranch, cx);
        }
        cx.notify();
    }

    fn show_hit(&mut self, path: &str, side: Side, line: usize, cx: &mut Context<Self>) {
        let side = diff_side(side);
        let path = SharedString::from(path.to_string());
        self.remember_position(cx);
        self.diff.update(cx, |state, cx| {
            state.set_file_collapsed(&path, false, cx);
            // A line inside a moved-code fold has no row until the fold is
            // revealed, so the scroll would silently go nowhere. Removed-
            // and-readded code is folded on both ends, and finding exactly
            // that is what a search is for.
            let covering: Vec<usize> = state
                .folds()
                .iter()
                .enumerate()
                .filter(|(_, fold)| {
                    fold.side() == side
                        && *fold.path() == path
                        && (fold.start()..fold.start() + fold.lines()).contains(&line)
                })
                .map(|(ix, _)| ix)
                .collect();
            for ix in covering {
                state.set_fold_revealed(ix, true, cx);
            }
            state.scroll_to_line(DiffLinePosition::new(path.clone(), side, line), cx);
        });
        self.current_file = Some(path);
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
        let revision = self.revision;

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
                    // A failed blame costs this comment; the diff on screen
                    // is still good, so the status bar says it instead of
                    // replacing the view.
                    Err(error) => {
                        this.notice = Some(format!("comment not saved: {error}").into());
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
                // The comment is kept even when a reload outran the blame -
                // dropping it would lose what the reviewer wrote - but the
                // placement below belongs to the view the draft was typed
                // in; on a stale revision the next reload places it instead.
                if this.revision == revision {
                    this.placed.push(Placed {
                        id,
                        path,
                        side,
                        line,
                        lines,
                        selection: from_selection,
                        other_revision,
                        changed_since: false,
                    });
                }
                this.save_review(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn toggle_reaction(&mut self, id: &str, kind: ReactionKind, cx: &mut Context<Self>) {
        if let Some(set) = self.reviews.as_mut() {
            let given = set.mine.toggle_reaction(id, kind);
            if kind.resolves() {
                if given && self.thread_resolved(id) {
                    self.grace_then_fold(id.to_string(), cx);
                } else if !self.thread_resolved(id) {
                    // Open again: the session's fold state has nothing to say.
                    self.unfolded.remove(id);
                    self.fading.remove(id);
                }
            }
            self.save_review(cx);
            cx.notify();
        }
    }

    fn thread_resolved(&self, id: &str) -> bool {
        self.reviews.as_ref().is_some_and(|set| {
            set.threads()
                .iter()
                .any(|thread| thread.comment.id == id && thread.resolved())
        })
    }

    /// A verdict folds the card into its line marker, but not under the
    /// pointer: the thread stays [`FOLD_GRACE`] so the click is seen
    /// landing, fades [`FOLD_FADE`], and is gone. Folding by hand or
    /// reopening during the grace calls the timer off.
    fn grace_then_fold(&mut self, id: String, cx: &mut Context<Self>) {
        self.unfolded.insert(id.clone());
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FOLD_GRACE).await;
            let fading = this
                .update(cx, |this, cx| {
                    if !this.unfolded.contains(&id) || !this.thread_resolved(&id) {
                        return false;
                    }
                    this.fading.insert(id.clone());
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !fading {
                return;
            }
            cx.background_executor().timer(FOLD_FADE).await;
            let _ = this.update(cx, |this, cx| {
                if this.fading.remove(&id) {
                    this.unfolded.remove(&id);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The marker's click: show the line's resolved cards, or fold them
    /// away again. Session state only; the review file knows nothing of it.
    fn toggle_folded(&mut self, ids: &[String], unfold: bool, cx: &mut Context<Self>) {
        for id in ids {
            self.fading.remove(id);
            if unfold {
                self.unfolded.insert(id.clone());
            } else {
                self.unfolded.remove(id);
            }
        }
        cx.notify();
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

    /// Move one of the reviewer's own threads into the archive file, out
    /// of every view for good. The archive write lands first and the live
    /// file forgets the comment only on its success, so a failed write
    /// never loses the thread - it stays on screen with a notice instead.
    fn archive_comment(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(set) = self.reviews.as_ref() else {
            return;
        };
        let Some(taken) = set.mine.clone().take_comment(id) else {
            return;
        };
        let path = ArchiveFile::path_for(&self.review_path);
        let id = id.to_string();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    ArchiveFile::append(&path, taken).map_err(|error| error.to_string())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Err(error) => {
                        this.notice = Some(format!("comment not archived: {error}").into());
                    }
                    Ok(()) => {
                        if let Some(set) = this.reviews.as_mut()
                            && set.mine.take_comment(&id).is_some()
                        {
                            this.placed.retain(|placed| placed.id != id);
                            this.save_review(cx);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
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

    fn snapshot(&self, cx: &App) -> Snapshot {
        let mut threads = Vec::new();
        let mut resolved_markers: Vec<ResolvedMarker> = Vec::new();
        if let Some(set) = &self.reviews {
            let set_threads = set.threads();
            let show_author = set_threads.iter().any(|thread| !thread.mine);
            for thread in set_threads {
                let Some(placed) = self
                    .placed
                    .iter()
                    .find(|placed| placed.id == thread.comment.id)
                else {
                    continue;
                };
                if placed.earlier() && !self.show_other_revisions {
                    continue;
                }
                // A resolved thread folds into its line's gutter marker;
                // the marker stays while the card is open too, as the way
                // to fold it again. Unfolding is session state only.
                if thread.resolved() {
                    let unfolded = self.unfolded.contains(&thread.comment.id);
                    let marker = resolved_markers.iter_mut().find(|marker| {
                        marker.path == placed.path
                            && marker.side == placed.side
                            && marker.line == placed.line
                    });
                    match marker {
                        Some(marker) => {
                            marker.ids.push(thread.comment.id.clone());
                            marker.folded |= !unfolded;
                        }
                        None => resolved_markers.push(ResolvedMarker {
                            path: placed.path.clone(),
                            side: placed.side,
                            line: placed.line,
                            ids: vec![thread.comment.id.clone()],
                            folded: !unfolded,
                        }),
                    }
                    if !unfolded {
                        continue;
                    }
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
                // A changed-since comment names its stored lines: its placed
                // line is the replacement's position, not where it was written.
                let line_label = if placed.changed_since {
                    let last = thread.comment.line;
                    if thread.comment.lines > 1 {
                        format!(
                            "was lines {}–{last}",
                            last + 1 - thread.comment.lines.min(last)
                        )
                    } else {
                        format!("was line {last}")
                    }
                } else if thread.comment.lines > 1 {
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
                    changed_since: placed.changed_since,
                    // The code the snippet quotes is gone from the view, so
                    // the card must carry it; elsewhere the diff row shows it.
                    snippet: placed
                        .changed_since
                        .then(|| thread.comment.snippet.trim_end().to_string().into()),
                    replies: thread
                        .replies
                        .iter()
                        .map(|(author, reply)| ReplyView {
                            author: SharedString::from((*author).to_string()),
                            body: SharedString::from(reply.body.trim_end().to_string()),
                            session: reply.session.clone().map(SharedString::from),
                        })
                        .collect(),
                    answered: thread.answered(),
                    mine: thread.mine,
                    resolved: thread.resolved(),
                    published: thread
                        .comment
                        .published
                        .as_ref()
                        .map(|published| (published.url.clone().into(), published.stale)),
                    reactions,
                    left: set.mine.reactions_left(&thread.comment.id),
                    answer_input: self
                        .answer
                        .as_ref()
                        .filter(|answer| answer.comment == thread.comment.id)
                        .map(|answer| answer.input.clone()),
                    wrap: self.wrap,
                    // Prominent only while the diff's focus mark is still the
                    // line the jump placed; a change-key press replaces the
                    // mark and the card falls back in line with the rest.
                    active: self.comment_cursor.as_deref() == Some(thread.comment.id.as_str()) && {
                        let mark = DiffLineRange::new(
                            SharedString::from(placed.path.clone()),
                            diff_side(placed.side),
                            placed.line,
                            placed.line,
                        );
                        self.diff.read(cx).focused_ranges() == std::slice::from_ref(&mark)
                    },
                    fading: self.fading.contains(&thread.comment.id),
                });
            }
        }
        let in_scope_set = self.in_scope();
        let shows_uncommitted = self.comparison.uncommitted
            && (self.scope != ScopeKind::Custom || self.picked_uncommitted);
        // One walk over the placements instead of one per file and count.
        #[derive(Default)]
        struct Tally {
            comments: usize,
            other_revision: usize,
            changed_since: usize,
        }
        let mut tallies: HashMap<&str, Tally> = HashMap::new();
        for placed in &self.placed {
            let tally = tallies.entry(placed.path.as_str()).or_default();
            tally.comments += 1;
            tally.other_revision += usize::from(placed.other_revision);
            tally.changed_since += usize::from(placed.changed_since);
        }
        let files = self
            .files
            .iter()
            .map(|file| {
                let state = self.review_state(&file.path);
                let generated = is_generated(&file.path);
                let note = match &file.state {
                    FileState::CouldNotCombine { commit } => Some(
                        format!(
                            "stops before {}: a skipped commit touches the same lines",
                            commit.short()
                        )
                        .into(),
                    ),
                    _ if file.state == FileState::KeptForComments => {
                        Some("no new changes - shown for its open comments".into())
                    }
                    _ if file.state == FileState::UnchangedSinceReview => {
                        Some("no changes since your review".into())
                    }
                    _ if generated => Some("generated file · collapsed by default".into()),
                    _ if file.state == FileState::UncommittedNotCombined => Some(
                        "uncommitted changes left out: a skipped commit touches the same lines"
                            .into(),
                    ),
                    _ if state == ReviewState::ChangedSince => {
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
                let tally = tallies.get(file.path.as_str());
                let uncommitted = shows_uncommitted
                    && file.state != FileState::UncommittedNotCombined
                    && self.uncommitted_paths.contains(&file.path);
                // The newest in-scope commit that touched a deleted path is
                // the one that deleted it: nothing can touch the path after.
                // A deletion carried by the uncommitted layer sits in no
                // commit, so it names none and the tooltip falls back.
                let removed_at = (file.status == FileStatus::Deleted && !uncommitted)
                    .then(|| {
                        self.touched
                            .get(&file.path)?
                            .iter()
                            .find(|id| in_scope_set.contains(id))
                            .map(|id| SharedString::from(id.short().to_string()))
                    })
                    .flatten();
                (
                    SharedString::from(file.path.clone()),
                    FileView {
                        state,
                        status: file.status,
                        note,
                        additions: file.additions,
                        deletions: file.deletions,
                        binary: file.binary,
                        generated,
                        touched,
                        in_scope: in_scope_set.len(),
                        uncommitted,
                        removed_at,
                        comments: tally.map_or(0, |tally| tally.comments),
                        other_revision: tally.map_or(0, |tally| tally.other_revision),
                        changed_code_comments: tally.map_or(0, |tally| tally.changed_since),
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
            resolved_markers,
            hovered_line: self.hovered_line.clone(),
        }
    }

    fn annotations(&self, snapshot: &Snapshot) -> Vec<DiffAnnotation> {
        let mut annotations: Vec<DiffAnnotation> = snapshot
            .threads
            .iter()
            .filter_map(|thread| {
                let placed = self.placed.iter().find(|placed| placed.id == thread.id)?;
                let side = diff_side(placed.side);
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
                            .disabled(self.publishing)
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
                        .when_some(self.pull.as_ref(), |controls, pull| {
                            controls.child(self.render_pull_menu(pull, cx))
                        })
                        .child(
                            ToggleGroup::new("layout")
                                .outline()
                                .segmented()
                                .single()
                                .small()
                                .child(
                                    Toggle::new("split")
                                        .icon(Icon::new(Lucide::Columns2))
                                        .tooltip("Split view")
                                        .checked(split),
                                )
                                .child(
                                    Toggle::new("unified")
                                        .icon(Icon::new(Lucide::Rows2))
                                        .tooltip("Unified view")
                                        .checked(!split),
                                )
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
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_wrap(cx))),
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
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_whitespace(cx))),
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
                            // An icon toggle like wrap and hide-whitespace,
                            // named only in the tooltip. "Outdated comments"
                            // (and a labelled switch) was rejected: it judged
                            // relevance, while the comments are merely pinned
                            // to another revision - the dashed bubble is how
                            // the diff already draws them.
                            Button::new("earlier-comments")
                                .outline()
                                .xsmall()
                                .w(px(28.))
                                .h(px(24.))
                                .icon(Icon::new(Lucide::MessageSquareDashed))
                                .selected(revisions_on)
                                .tooltip("Earlier comments - from other revisions")
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.toggle_other_revisions(cx)),
                                ),
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
        now: i64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = commit.id.clone();
        let hover_id = commit.id.clone();
        let checked = self.picked.contains(&commit.id) || self.scope != ScopeKind::Custom;
        let muted = cx.theme().muted_foreground;
        let accent = comment_accent(cx);
        let mono = cx.theme().mono_font_family.clone();
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
                    .mt(px(3.))
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

    /// The uncommitted layer's picker row, styled like a commit item, so the
    /// working-tree changes can be excluded from a custom scope explicitly.
    fn render_uncommitted_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let checked = self.picked_uncommitted || self.scope != ScopeKind::Custom;
        let muted = cx.theme().muted_foreground;
        let files = self.uncommitted_paths.len();
        h_flex()
            .id("pick-uncommitted")
            .w_full()
            .items_start()
            .gap_2()
            .px_1()
            .py(px(3.))
            .rounded(px(4.))
            .cursor_pointer()
            .when(checked, |row| row.bg(cx.theme().accent))
            .on_click(cx.listener(|this, _: &gpui_kit::ClickEvent, window, cx| {
                window.focus(&this.picker_focus, cx);
                this.toggle_uncommitted(cx);
            }))
            .child(
                Checkbox::new("pick-box-uncommitted")
                    .xsmall()
                    .mt(px(2.))
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
                            .child("Uncommitted changes"),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .text_size(px(11.))
                            .text_color(if checked {
                                cx.theme().blue_light
                            } else {
                                muted
                            })
                            .child(Icon::new(Lucide::Pencil).xsmall())
                            .child(format!(
                                "{files} file{} · working tree",
                                if files == 1 { "" } else { "s" }
                            )),
                    ),
            )
            .into_any_element()
    }

    /// The heading row of one picker group: a check-all box, the title and
    /// the checked count.
    fn render_picker_group(
        &self,
        section: &PickerSection,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let muted = cx.theme().muted_foreground;
        let ids: Vec<Oid> = self.commits[section.range.clone()]
            .iter()
            .map(|commit| commit.id.clone())
            .collect();
        // Outside the custom scope every commit counts as checked.
        let mut checked = if self.scope == ScopeKind::Custom {
            ids.iter().filter(|id| self.picked.contains(id)).count()
        } else {
            ids.len()
        };
        // The uncommitted row counts like a commit in its group.
        let mut len = ids.len();
        if section.with_uncommitted {
            len += 1;
            checked += usize::from(self.picked_uncommitted || self.scope != ScopeKind::Custom);
        }
        let all = checked == len && len > 0;
        let with_uncommitted = section.with_uncommitted;
        let title_ids = ids.clone();
        h_flex()
            .w_full()
            .h(px(24.))
            .gap_2()
            .px_1()
            .when(section.bordered, |row| {
                row.mt(px(2.)).border_t_1().border_color(cx.theme().border)
            })
            .child(
                Checkbox::new(section.title)
                    .xsmall()
                    .checked(all)
                    .on_change(cx.listener(move |this, checked: &bool, _, cx| {
                        if with_uncommitted {
                            // enter_custom first: it would otherwise reset
                            // the layer to checked when leaving another
                            // scope, overriding this very click.
                            this.enter_custom();
                            this.picked_uncommitted = *checked;
                        }
                        this.pick_many(ids.clone(), *checked, cx)
                    })),
            )
            .child(
                // Commit rows toggle on a title click; the heading follows.
                div()
                    .id(SharedString::from(format!("pick-group-{}", section.title)))
                    .flex_1()
                    .font_semibold()
                    .text_color(section.heading_color)
                    .cursor_pointer()
                    .on_click(
                        cx.listener(move |this, _: &gpui_kit::ClickEvent, window, cx| {
                            window.focus(&this.picker_focus, cx);
                            let checked = !all;
                            if with_uncommitted {
                                this.enter_custom();
                                this.picked_uncommitted = checked;
                            }
                            this.pick_many(title_ids.clone(), checked, cx)
                        }),
                    )
                    .child(section.title),
            )
            .child(
                div()
                    .font_normal()
                    .text_color(muted)
                    .child(format!("{checked} of {len}")),
            )
    }

    /// One picker section: the group heading, then its rows, shortened
    /// behind a show-more button until expanded.
    fn render_picker_section(
        &self,
        list: gpui_kit::Div,
        section: PickerSection,
        context: &PickerContext,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let mut list = list;
        if section.range.is_empty() && !section.with_uncommitted {
            return list;
        }
        list = list.child(self.render_picker_group(&section, cx));
        let query = context.query.as_str();
        if section.with_uncommitted && (query.is_empty() || "uncommitted changes".contains(query)) {
            list = list.child(self.render_uncommitted_row(cx));
        }
        let matches = |commit: &Commit| {
            query.is_empty()
                || commit.subject.to_lowercase().contains(query)
                || commit.author.to_lowercase().contains(query)
                || commit.id.short().to_lowercase().starts_with(query)
        };
        let rows: Vec<&Commit> = self.commits[section.range.clone()]
            .iter()
            .filter(|commit| matches(commit))
            .collect();
        let shown = if section.expanded || rows.len() <= 8 {
            rows.len()
        } else {
            6
        };
        for commit in &rows[..shown] {
            let head = self
                .commits
                .first()
                .is_some_and(|first| first.id == commit.id);
            let skipped = context.skipped.contains(&commit.id);
            let comments = context.comment_counts.get(&commit.id).copied().unwrap_or(0);
            list = list.child(self.render_commit_row(
                commit,
                head,
                skipped,
                comments,
                context.now,
                cx,
            ));
        }
        if shown < rows.len() {
            let remaining = rows.len() - shown;
            let toggle = section.title;
            list = list.child(
                Button::new(SharedString::from(format!("more-{}", section.title)))
                    .ghost()
                    .xsmall()
                    .label(format!("Show {remaining} more"))
                    .text_color(cx.theme().muted_foreground)
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
    }

    /// The dropdown with the commit range on its trigger and the grouped,
    /// searchable commit list inside.
    fn render_commit_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
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

        let context = PickerContext {
            query: self.picker_query.read(cx).value().trim().to_lowercase(),
            skipped: skipped_set,
            comment_counts,
            now: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_secs() as i64),
        };

        // The uncommitted layer only earns a row when it has changes to show.
        let uncommitted_entry = self.comparison.uncommitted && !self.uncommitted_paths.is_empty();
        let split_at = self.reviewed_from().unwrap_or(total);
        let mut list = v_flex().w_full().gap_0p5();
        list = self.render_picker_section(
            list,
            PickerSection {
                title: "New since your review",
                range: 0..split_at,
                heading_color: blue_light,
                bordered: false,
                expanded: self.show_all_new,
                with_uncommitted: uncommitted_entry,
            },
            &context,
            cx,
        );
        list = self.render_picker_section(
            list,
            PickerSection {
                title: "Already reviewed",
                range: split_at..total,
                heading_color: muted,
                bordered: split_at > 0,
                expanded: self.show_all_reviewed,
                with_uncommitted: false,
            },
            &context,
            cx,
        );

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
            .child(picker)
    }

    /// The right side of the scope bar: the open-comment count, which is
    /// also the way into the next open comment, and the reviewed-files
    /// progress.
    fn render_review_progress(
        &self,
        bar: gpui_kit::Div,
        reviewed: usize,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let muted = cx.theme().muted_foreground;
        let accent = comment_accent(cx);
        let threads = self
            .reviews
            .as_ref()
            .map(|set| set.threads())
            .unwrap_or_default();
        // "Am I done?" needs the open count in view; every comment counts
        // here, even ones the other-revisions toggle hides.
        let open = threads.iter().filter(|thread| !thread.resolved()).count();
        let any = !threads.is_empty();
        // Open comments no row can show - their file is not in this
        // comparison. Named, so the count never promises more than the
        // window can deliver.
        let unplaced = threads
            .iter()
            .filter(|thread| {
                !thread.resolved()
                    && !self
                        .placed
                        .iter()
                        .any(|placed| placed.id == thread.comment.id)
            })
            .count();
        let bar = match (any, open) {
            (false, _) => bar,
            (true, 0) => bar.child(div().text_color(muted).child("comments handled")),
            // The count is also the way in: clicking walks to the next open
            // comment, clearing whatever hides it (the earlier-comments
            // toggle, or a scope that leaves its file out) - the label
            // alone left no path to them.
            (true, open) => bar
                .child(
                    div()
                        .id("open-comments")
                        .text_color(accent)
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.jump_comment(true, cx);
                        }))
                        .child(format!(
                            "{open} open comment{}",
                            if open == 1 { "" } else { "s" }
                        )),
                )
                .when(unplaced > 0, |bar| {
                    bar.child(
                        div()
                            .text_color(muted)
                            .child(format!("{unplaced} not in this comparison")),
                    )
                }),
        };
        let progress = if self.files.is_empty() {
            0.
        } else {
            reviewed as f32 * 100. / self.files.len() as f32
        };
        bar.child(format!("{reviewed} of {} files reviewed", self.files.len()))
            .child(
                Progress::new("files-reviewed")
                    .value(progress)
                    .xsmall()
                    .w(px(90.)),
            )
    }

    fn render_scope_bar(&self, reviewed: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let explanation = match self.scope {
            ScopeKind::NewForMe => "Each file is compared with the version you last ticked",
            ScopeKind::WholeBranch => "Everything the branch adds",
            ScopeKind::Custom => "Combined diff of the checked commits",
        };
        // The jump history and reload as buttons, so the keyboardless hand
        // has them too; the tooltips teach the keys.
        let (back_key, forward_key, reload_key) = if cx.platform_info().is_mac() {
            ("⌘[", "⌘]", "⌘R")
        } else {
            ("Ctrl+[", "Ctrl+]", "Ctrl+R")
        };
        let nav_button = |id: &'static str, icon: Lucide, tip: String| {
            Button::new(id)
                .ghost()
                .xsmall()
                .h(px(22.))
                .icon(Icon::new(icon))
                .tooltip(tip)
        };
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
                h_flex()
                    .gap_0p5()
                    .child(
                        nav_button("nav-back", Lucide::ArrowLeft, format!("Back ({back_key})"))
                            .disabled(self.back_stack.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| this.jump_back(cx))),
                    )
                    .child(
                        nav_button(
                            "nav-forward",
                            Lucide::ArrowRight,
                            format!("Forward ({forward_key})"),
                        )
                        .disabled(self.forward_stack.is_empty())
                        .on_click(cx.listener(|this, _, _, cx| this.jump_forward(cx))),
                    )
                    .child(
                        nav_button(
                            "nav-reload",
                            Lucide::RefreshCw,
                            format!("Reload ({reload_key})"),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                    ),
            )
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
            .child(self.render_commit_picker(cx))
            .child(div().text_color(muted).child(explanation))
            .child(div().flex_1())
            .map(|bar| self.render_review_progress(bar, reviewed, cx))
    }

    fn render_search_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let extended = self.search_all && self.scope != ScopeKind::WholeBranch;
        let count = if self.search_query.read(cx).value().trim().is_empty() {
            None
        } else if extended {
            Some(format!(
                "{} in view · {} across all commits",
                self.search_hits.len(),
                self.search_all_hits.len()
            ))
        } else {
            Some(match self.search_hits.len() {
                0 => "no matches".to_string(),
                1 => "1 match".to_string(),
                n => format!("{n} matches"),
            })
        };
        h_flex()
            .flex_none()
            .px(px(10.))
            .py(px(3.))
            .gap(px(8.))
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .w(px(280.))
                    .child(Input::new(&self.search_query).small().cleanable(true)),
            )
            .child(
                Button::new("search-previous")
                    .ghost()
                    .xsmall()
                    .w(px(24.))
                    .h(px(24.))
                    .icon(Icon::new(Lucide::ChevronUp))
                    .tooltip("Previous match (shift-enter)")
                    .on_click(cx.listener(|this, _, _, cx| this.search_step(false, cx))),
            )
            .child(
                Button::new("search-next")
                    .ghost()
                    .xsmall()
                    .w(px(24.))
                    .h(px(24.))
                    .icon(Icon::new(Lucide::ChevronDown))
                    .tooltip("Next match (enter)")
                    .on_click(cx.listener(|this, _, _, cx| this.search_step(true, cx))),
            )
            .when_some(count, |bar, count| {
                bar.child(div().text_color(muted).child(count))
            })
            .when(self.scope != ScopeKind::WholeBranch, |bar| {
                bar.child(
                    Checkbox::new("search-all")
                        .xsmall()
                        .label("All commits")
                        .checked(self.search_all)
                        .on_change(cx.listener(|this, checked: &bool, _, cx| {
                            this.search_all = *checked;
                            if this.search_all {
                                this.ensure_whole_patch(cx);
                            }
                            this.run_search(cx);
                        })),
                )
            })
            .child(div().flex_1())
            .child(
                Button::new("search-close")
                    .ghost()
                    .xsmall()
                    .w(px(24.))
                    .h(px(24.))
                    .icon(Icon::new(Lucide::X))
                    .tooltip("Close search (esc)")
                    .on_click(cx.listener(|this, _, window, cx| this.close_search(window, cx))),
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
            .relative()
            .w_full()
            .h(px(22.))
            // Twice the guide-to-text gap of a folder row: the line reads as
            // a margin, not a strikethrough. Root files have no guide and
            // keep the folder alignment.
            .pl(px(if depth == 0 { 4. } else { 7. } + depth as f32 * 10.))
            .pr(px(4.))
            .gap(px(6.))
            .rounded(px(4.))
            .cursor_pointer()
            .children(indent_guides(depth, cx))
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
            // A changed-since triangle sat here and was rejected as noise:
            // the tree stays quiet, the file header's note carries it.
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

    /// Whether any ancestor folder of `path` is shut in the file panel.
    fn folder_shut(&self, path: &str) -> bool {
        path.match_indices('/')
            .any(|(at, _)| self.collapsed_folders.contains(&path[..at]))
    }

    fn render_sidebar(&self, reviewed: usize, cx: &mut Context<Self>) -> impl IntoElement {
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
                            .on_click(cx.listener(|this, _, _, cx| this.set_sidebar(false, cx))),
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
                                        .relative()
                                        .h(px(22.))
                                        .gap(px(4.))
                                        .pl(px(4. + *depth as f32 * 10.))
                                        .text_color(cx.theme().muted_foreground)
                                        .children(indent_guides(*depth, cx))
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
                                            let collapsed = !this.collapsed_folders.remove(&path);
                                            if collapsed {
                                                this.collapsed_folders.insert(path.clone());
                                            }
                                            // The diff follows the panel: a shut
                                            // folder collapses its files in the main
                                            // view too, and reopening frees only the
                                            // files not under another shut folder.
                                            let prefix = format!("{path}/");
                                            let affected: Vec<SharedString> = this
                                                .files
                                                .iter()
                                                .filter(|file| {
                                                    file.path.starts_with(&prefix)
                                                        && (collapsed
                                                            || !this.folder_shut(&file.path))
                                                })
                                                .map(|file| SharedString::from(file.path.clone()))
                                                .collect();
                                            this.diff.update(cx, |state, cx| {
                                                state.set_files_collapsed(affected, collapsed, cx);
                                            });
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
                // A step above the app's 12 px body text: the tree is the
                // main navigation and earns the extra legibility.
                .text_size(px(12.5))
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

        let snapshot = Rc::new(self.snapshot(cx));
        let annotations = self.annotations(&snapshot);
        let owner = cx.entity().downgrade();
        let for_annotations = (snapshot.clone(), owner.clone());
        let for_header = (snapshot.clone(), owner.clone());
        let for_hover = owner.clone();

        let ThemePalette {
            added,
            removed,
            search_line,
            search_match,
            search_match_foreground,
        } = palette(cx);
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
                let side = diff_side(placed.side);
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
        // Search hits fill their whole line: the Kit decorates lines, not
        // character spans, and the last fill supplied wins on overlap.
        let search_spans = self
            .search_open
            .then(|| self.search_hits.iter())
            .into_iter()
            .flatten()
            .map(move |hit| {
                let side = diff_side(hit.side);
                DiffDecoration::new(DiffLineRange::new(
                    SharedString::from(hit.path.clone()),
                    side,
                    hit.line,
                    hit.line,
                ))
                .with_background(search_line)
            });
        // The matched characters themselves get a louder fill over the line
        // wash, so a long hit line still shows where the needle sits.
        let span_highlights: Vec<DiffSpanHighlight> = self
            .search_open
            .then(|| self.search_hits.iter())
            .into_iter()
            .flatten()
            .flat_map(|hit| {
                let side = diff_side(hit.side);
                let position =
                    DiffLinePosition::new(SharedString::from(hit.path.clone()), side, hit.line);
                hit.spans.iter().map(move |span| {
                    DiffSpanHighlight::new(position.clone(), span.clone())
                        .with_background(search_match)
                        .with_foreground(search_match_foreground)
                })
            })
            .collect();
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
            .chain(search_spans)
            .collect();

        let mut diff = Diff::new(&self.diff)
            .size_full()
            .soft_wrap(self.wrap)
            // The decorations below paint richer added and removed fills than
            // the component's own washed-out tint.
            .change_background(false)
            .change_indicator(DiffChangeIndicator::Bars)
            .decorations(decorations)
            .span_highlights(span_highlights)
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
                        tracing::trace!(
                            mouse_down = this.mouse_down,
                            selecting,
                            position = ?position.as_ref().map(|p| (p.path().to_string(), p.line())),
                            "selection hover"
                        );
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
                let removed = view.is_some_and(|view| view.status == FileStatus::Deleted);
                let removed_at = view.and_then(|view| view.removed_at.clone());
                let warning = cx.theme().warning;
                let danger = cx.theme().danger;
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
                        format!(
                            "{} comment{}",
                            view.comments,
                            if view.comments == 1 { "" } else { "s" }
                        )
                    })
                });
                // The breakdown is on-demand detail, not header text; it
                // rides the info tooltip so the header stays one count.
                let comment_detail = view.and_then(|view| {
                    let mut parts = Vec::new();
                    if view.other_revision > 0 {
                        parts.push(format!(
                            "{} comment{} from another revision",
                            view.other_revision,
                            if view.other_revision == 1 { "" } else { "s" }
                        ));
                    }
                    if view.changed_code_comments > 0 {
                        parts.push(format!(
                            "{} comment{} on changed code",
                            view.changed_code_comments,
                            if view.changed_code_comments == 1 {
                                ""
                            } else {
                                "s"
                            }
                        ));
                    }
                    (!parts.is_empty()).then(|| parts.join(" · "))
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
                            // The tree's status letter rides along so a
                            // collapsed removed file reads as removed without
                            // the sidebar open.
                            .when(removed, |row| {
                                // The commit behind the D is hover detail,
                                // not header text; a worktree deletion has
                                // no commit to name.
                                let tip: SharedString = match &removed_at {
                                    Some(rev) => format!("This file got removed at {rev}").into(),
                                    None => "This file got removed".into(),
                                };
                                row.child(
                                    div()
                                        .id(SharedString::from(format!("removed-{path}")))
                                        .flex_none()
                                        .font_family(mono.clone())
                                        .text_size(px(12.))
                                        .font_medium()
                                        .text_color(danger)
                                        .child("D")
                                        .tooltip(move |window, cx| {
                                            Tooltip::new(tip.clone()).build(window, cx)
                                        }),
                                )
                            })
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(mono.clone())
                                    .text_size(px(12.))
                                    .font_medium()
                                    // The name itself carries the removal (a
                                    // trailing label read as noise), and it
                                    // outranks the quiet mute: a ticked
                                    // removal still reads as removed.
                                    .when(quiet && !removed, |name| name.text_color(muted))
                                    .when(removed, |name| name.text_color(danger))
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
                    // The warning colour, plain weight: noticeable without
                    // shouting like an alert.
                    .when(view.is_some_and(|view| view.uncommitted), |row| {
                        row.child(div().flex_none().text_color(warning).child("uncommitted"))
                    })
                    // Quiet metadata: the count messages only matter when the
                    // reviewer asks, so they share one icon's hover.
                    .when_some(
                        {
                            let parts: Vec<_> =
                                [touched, comment_detail].into_iter().flatten().collect();
                            (!parts.is_empty()).then(|| parts.join(" · "))
                        },
                        |row, info| {
                            row.child(
                                Button::new(SharedString::from(format!("info-{path}")))
                                    .ghost()
                                    .xsmall()
                                    .w(px(18.))
                                    .h(px(18.))
                                    .icon(Icon::new(Lucide::Info))
                                    .tooltip(info),
                            )
                        },
                    )
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
            });
        // The marker lane carries the resolved threads' toggles. It is set
        // only when some exist, so a review without verdicts keeps the
        // mockup's tight gutter.
        if !snapshot.resolved_markers.is_empty() {
            let markers: Vec<DiffGutterMarker> = snapshot
                .resolved_markers
                .iter()
                .map(|marker| {
                    DiffGutterMarker::new(
                        ElementId::from(SharedString::from(format!(
                            "resolved-{}-{}-{}",
                            marker.path,
                            marker.line,
                            matches!(marker.side, Side::New),
                        ))),
                        DiffLinePosition::new(
                            SharedString::from(marker.path.clone()),
                            diff_side(marker.side),
                            marker.line,
                        ),
                    )
                    .with_label(resolved_marker_label(marker))
                })
                .collect();
            let for_markers = (snapshot.clone(), owner.clone());
            diff = diff
                .gutter_markers(markers)
                .render_gutter_marker(move |marker, _, cx| {
                    let (snapshot, owner) = &for_markers;
                    render_resolved_marker(marker, snapshot, owner.clone(), cx)
                });
        }
        diff.into_any_element()
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let mono = cx.theme().mono_font_family.clone();
        // Only shortcuts that exist; each new binding earns its hint here.
        let back = if cx.platform_info().is_mac() {
            "⌘[ ⌘]"
        } else {
            "ctrl [ ]"
        };
        let hints: &[(&str, &str)] = &[
            ("] [", "change"),
            ("} {", "file"),
            ("1 2 3", "scope"),
            ("r", "mark reviewed"),
            ("c", "comment"),
            ("n p", "comments"),
            (back, "back, forward"),
            ("/", "search"),
            ("s", "split / unified"),
            ("o", "earlier comments"),
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
                    .on_click(cx.listener(|this, _, _, cx| this.set_sidebar(true, cx))),
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
        if let Some(notice) = &self.notice {
            bar = bar.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(cx.theme().danger)
                    .child(notice.clone()),
            );
        }
        if self.publishing {
            bar = bar.child("Publishing…");
        }
        if let Some((status, failed)) = &self.publish_status {
            bar = bar.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .when(*failed, |status| status.text_color(cx.theme().danger))
                    .child(status.clone()),
            );
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

fn resolved_marker_label(marker: &ResolvedMarker) -> String {
    let what = if marker.ids.len() == 1 {
        "1 resolved comment".to_string()
    } else {
        format!("{} resolved comments", marker.ids.len())
    };
    if marker.folded {
        format!("{what} - click to show")
    } else {
        format!("{what} - click to fold away")
    }
}

/// The quiet mark a resolved thread folds into: a checked comment bubble
/// beside the line number, faint until the pointer is on its line, full
/// strength while its cards are open. A click toggles the cards, for
/// this session only.
fn render_resolved_marker(
    marker: &DiffGutterMarker,
    snapshot: &Snapshot,
    owner: WeakEntity<ReviewApp>,
    cx: &mut App,
) -> AnyElement {
    let Some(fold) = snapshot.resolved_markers.iter().find(|fold| {
        marker.position().path().as_ref() == fold.path
            && marker.position().side() == diff_side(fold.side)
            && marker.position().line() == fold.line
    }) else {
        return div().into_any_element();
    };
    let hovered = snapshot
        .hovered_line
        .as_ref()
        .is_some_and(|line| line == marker.position());
    let ids = fold.ids.clone();
    let unfold = fold.folded;
    let quiet = fold.folded && !hovered;
    Button::new(marker.id().clone())
        .ghost()
        .xsmall()
        .icon(Icon::new(Lucide::MessageSquareCheck))
        .text_color(cx.theme().muted_foreground)
        .tooltip(marker.label().clone())
        .when(quiet, |button| button.opacity(0.35))
        .on_click(move |_, _, cx| {
            let _ = owner.update(cx, |this, cx| this.toggle_folded(&ids, unfold, cx));
        })
        .into_any_element()
}

fn render_thread(thread: &ThreadView, owner: WeakEntity<ReviewApp>, cx: &mut App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let accent = comment_accent(cx);
    let warning = cx.theme().warning;
    let success = cx.theme().success;
    let mono = cx.theme().mono_font_family.clone();
    let id = thread.id.clone();
    let quiet_border = muted.opacity(0.5);
    let submit_hint = if cx.platform_info().is_mac() {
        "⌘↵"
    } else {
        "ctrl ↵"
    };

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
        let kinds = thread.left.clone();
        let (picker_owner, picker_id) = (owner.clone(), id.clone());
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
                // Content, not a child: picking a reaction must also dismiss
                // the popover, and only the content closure can reach it.
                .content(move |_, _, cx| {
                    let popover = cx.entity();
                    let mut picker = v_flex().gap_0p5();
                    for kind in kinds.iter().copied() {
                        let (owner, id, popover) =
                            (picker_owner.clone(), picker_id.clone(), popover.clone());
                        picker = picker.child(
                            Button::new(SharedString::from(format!("react-{id}-{}", kind.label())))
                                .ghost()
                                .xsmall()
                                .h(px(24.))
                                .label(format!("{} {}", kind.emoji(), kind.label()))
                                .on_click(move |_, window, cx| {
                                    let _ = owner
                                        .update(cx, |this, cx| this.toggle_reaction(&id, kind, cx));
                                    popover.update(cx, |state, cx| state.dismiss(window, cx));
                                }),
                        );
                    }
                    picker
                }),
        );
    }
    actions = actions.child(div().flex_1());
    // The explicit way out of a thread: GitHub's "resolve conversation",
    // here simply the Done reaction with a door handle on it.
    if !thread.resolved {
        let (owner, id) = (owner.clone(), id.clone());
        actions = actions.child(
            Button::new(SharedString::from(format!("resolve-{id}")))
                .outline()
                .xsmall()
                .h(px(22.))
                .label("Resolve")
                .tooltip("Reacts Done and closes the thread")
                .on_click(move |_, _, cx| {
                    let _ = owner.update(cx, |this, cx| {
                        this.toggle_reaction(&id, ReactionKind::Done, cx)
                    });
                }),
        );
    }
    // Icon-only like the title bar's toggles, each named by its tooltip:
    // five labels made the row read like a sentence (owner, 2026-11-16).
    // Resolve alone keeps its word - it is the row's one verdict.
    let icon_button = |id: SharedString, icon: Lucide, tip: &'static str| {
        Button::new(id)
            .ghost()
            .xsmall()
            .h(px(22.))
            .icon(Icon::new(icon))
            .text_color(muted)
            .tooltip(tip)
    };
    if let Some((url, stale)) = thread.published.clone() {
        actions = actions.child(
            icon_button(
                SharedString::from(format!("published-{id}")),
                Lucide::Link,
                if stale {
                    "Open on the pull request - edited since, publish again to update it"
                } else {
                    "Open on the pull request"
                },
            )
            .on_click(move |_, _, cx| cx.open_url(&url)),
        );
    }
    // An out-of-scope comment can still be addressed - react, resolve,
    // reply, copy, archive - but not rewritten: its code is not the code
    // on screen, so editing and deleting wait for the revision it was
    // written against (owner, 2026-11-16).
    if thread.mine {
        let (owner, id) = (owner.clone(), id.clone());
        actions = actions.child(
            icon_button(
                SharedString::from(format!("edit-{id}")),
                Lucide::Pencil,
                if thread.other_revision {
                    "Editing needs the comment's own revision in view"
                } else {
                    "Edit the comment"
                },
            )
            .disabled(thread.other_revision)
            .on_click(move |_, window, cx| {
                let _ = owner.update(cx, |this, cx| this.start_edit(&id, window, cx));
            }),
        );
    }
    {
        let (owner, id) = (owner.clone(), id.clone());
        actions = actions.child(
            icon_button(
                SharedString::from(format!("copy-prompt-{id}")),
                Lucide::ClipboardCopy,
                "Copy as a prompt for an agent",
            )
            .on_click(move |_, _, cx| {
                let _ = owner.update(cx, |this, cx| this.copy_comment_as_prompt(&id, cx));
            }),
        );
    }
    if thread.mine {
        let (owner, id) = (owner.clone(), id.clone());
        actions = actions.child(
            icon_button(
                SharedString::from(format!("archive-{id}")),
                Lucide::Archive,
                "Moves the thread into the archive file, out of every view - no restore",
            )
            .on_click(move |_, _, cx| {
                let _ = owner.update(cx, |this, cx| this.archive_comment(&id, cx));
            }),
        );
    }
    if thread.mine {
        let (owner, id) = (owner.clone(), id.clone());
        actions = actions.child(
            icon_button(
                SharedString::from(format!("delete-{id}")),
                Lucide::Trash,
                if thread.other_revision {
                    "Deleting needs the comment's own revision in view"
                } else {
                    "Delete the comment"
                },
            )
            .disabled(thread.other_revision)
            .on_click(move |_, _, cx| {
                let _ = owner.update(cx, |this, cx| this.delete_comment(&id, cx));
            }),
        );
    }

    // Both earlier kinds keep the meta line quiet; the changed-since tag
    // alone carries the warning color, so it reads as the one alert.
    let header_color = if thread.other_revision || thread.changed_since {
        muted
    } else {
        accent
    };
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
    if thread.changed_since {
        header = header.child(
            div()
                .px(px(4.))
                .border_1()
                .border_color(warning.opacity(0.6))
                .rounded(px(3.))
                .text_color(warning)
                .child("code changed"),
        );
    } else if thread.answered && !thread.resolved {
        // An answer without a code change: the comment was a question. The
        // code-changed tag outranks it - the thread shows the reply anyway.
        header = header.child(
            div()
                .px(px(4.))
                .border_1()
                .border_color(success.opacity(0.6))
                .rounded(px(3.))
                .text_color(success)
                .child("answered"),
        );
    }
    if thread.show_author {
        header = header.child(div().font_medium().child(thread.author.clone()));
    }
    header = header.child(div().font_family(mono.clone()).child(thread.commit.clone()));
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
            if thread.changed_since {
                // Dashed like the other earlier kind, in the warning tint:
                // the fix below answers it and awaits the reviewer's verdict.
                card.border_1()
                    .border_dashed()
                    .border_color(warning.opacity(0.55))
                    .bg(warning.opacity(0.05))
            } else if thread.other_revision {
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
        // The jump stepper's landing: the border leaps out so the eye finds
        // the card among its neighbours. Width over padding - the box grows
        // by the border alone, keeping the density rules.
        .when(thread.active, |card| {
            card.border_2().border_color(cx.theme().primary)
        })
        .child(header)
        // The rewritten code the comment was about, quoted from the review
        // file: no diff row shows it any more. The quote follows the diff's
        // wrap toggle; unwrapped it is its own scroll area. The Diff's
        // ScrollableMask owns horizontal wheel input for the whole pane in
        // the capture phase, and it only yields to an occluding hitbox
        // painted in front of it - so the quote paints deferred (layout
        // stays in the card) and occludes. The price is that the wheel
        // does nothing but horizontal scrolling while over the quote.
        .when_some(thread.snippet.clone(), |card, snippet| {
            let quote = v_flex()
                .px(px(6.))
                .py(px(4.))
                .rounded(px(4.))
                .bg(muted.opacity(0.08))
                .font_family(mono.clone())
                .text_size(px(11.))
                .text_color(muted)
                // Unwrapped lines must shrink-wrap their text: stretched to
                // the column width they clip inside themselves and the
                // scroll area never sees an overflow to scroll.
                .when(!thread.wrap, |quote| quote.items_start())
                .children(
                    snippet
                        .split('\n')
                        .map(|line| {
                            div()
                                .when(!thread.wrap, |line| line.whitespace_nowrap())
                                .child(line.to_string())
                        })
                        .collect::<Vec<_>>(),
                );
            card.child(if thread.wrap {
                quote.into_any_element()
            } else {
                // No scroll handler of our own: the occlusion already keeps
                // the pane out of the hit test, and a stop-propagation here
                // can cancel the element's own offset handling.
                deferred(
                    quote
                        .id(SharedString::from(format!("snippet-{}", thread.id)))
                        .overflow_x_scroll()
                        .occlude(),
                )
                .into_any_element()
            })
        })
        // Resolving changes nothing but the reaction: question, answers and
        // the respond box all stay - the thread is the record, and hiding
        // the question under its own answers read as losing it.
        // Markdown, so fenced and backticked code in a comment reads as code.
        .child(TextView::markdown(
            SharedString::from(format!("comment-body-{}", thread.id)),
            thread.body.clone(),
        ))
        .children(thread.replies.iter().enumerate().map(|(index, reply)| {
            // With a session the author line becomes a robot badge: hover
            // names the agent conversation, a click hands the user
            // `claude --resume <id>` to dig into the reasoning.
            let author: AnyElement = match &reply.session {
                Some(session) => {
                    let resume = format!("claude --resume {session}");
                    Button::new(SharedString::from(format!(
                        "reply-who-{}-{index}",
                        thread.id
                    )))
                    .ghost()
                    .xsmall()
                    .h(px(18.))
                    .icon(Icon::new(Lucide::Bot))
                    .label(reply.author.clone())
                    .text_color(muted)
                    .tooltip(SharedString::from(format!(
                        "Claude Code in session {session} - click copies the resume command"
                    )))
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(resume.clone()));
                    })
                    .into_any_element()
                }
                None => div()
                    .text_size(px(11.))
                    .text_color(muted)
                    .font_medium()
                    .child(reply.author.clone())
                    .into_any_element(),
            };
            v_flex()
                .gap_0p5()
                .pl_2()
                .border_l_2()
                .border_color(quiet_border)
                .child(h_flex().child(author))
                .child(TextView::markdown(
                    SharedString::from(format!("reply-{}-{index}", thread.id)),
                    reply.body.clone(),
                ))
        }))
        // A conversation invites the next word: under an open thread with
        // answers the box is already there - a quiet stand-in until a click
        // makes it the real textarea.
        .when(!thread.replies.is_empty(), |card| {
            card.child(match &thread.answer_input {
                Some(input) => v_flex()
                    .gap_0p5()
                    .child(Textarea::new(input))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(muted)
                            .child(format!("{submit_hint} sends · esc closes")),
                    )
                    .into_any_element(),
                None => {
                    let (owner, id) = (owner.clone(), thread.id.clone());
                    h_flex()
                        .id(SharedString::from(format!("answer-open-{}", thread.id)))
                        .h(px(24.))
                        .w_full()
                        .px_2()
                        .items_center()
                        .border_1()
                        .border_color(quiet_border)
                        .rounded(px(4.))
                        .text_size(px(12.))
                        .text_color(muted)
                        .cursor_pointer()
                        .on_click(move |_, window, cx| {
                            let _ = owner.update(cx, |this, cx| this.start_answer(&id, window, cx));
                        })
                        .child("Respond…")
                        .into_any_element()
                }
            })
        })
        .child(
            // A sliver of air so the last answer does not sit on the
            // action buttons.
            div()
                .w_full()
                .when(!thread.replies.is_empty(), |actions| actions.mt(px(2.)))
                .child(actions),
        );

    let wrapped = div().w_full().px_2().py_1().child(card);
    if thread.fading {
        // The fold's last moment: the card fades, then the snapshot stops
        // emitting it and only the line marker remains.
        return wrapped
            .with_animation(
                ElementId::from(SharedString::from(format!("fold-fade-{}", thread.id))),
                Animation::new(FOLD_FADE),
                |card, delta| card.opacity(1. - delta),
            )
            .into_any_element();
    }
    wrapped.into_any_element()
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
        // A click or Escape elsewhere ends the text selection; the
        // remembered lines and the menu payload go with it.
        if (self.text_lines.is_some() || self.pending_comment.is_some())
            && !gpui_kit::base::TextSelection::has_selection(window, cx)
            && !self.mouse_down
        {
            self.text_lines = None;
            self.pending_comment = None;
        }
        // The scope bar and the sidebar both show it; counted once per frame.
        let reviewed = self
            .files
            .iter()
            .filter(|file| self.review_state(&file.path) == ReviewState::Reviewed)
            .count();
        v_flex()
            .id("review")
            .key_context(if self.draft.is_some() || self.answer.is_some() {
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
            .capture_any_mouse_down(cx.listener(|this, event: &gpui_kit::MouseDownEvent, _, _| {
                // Only the primary button drags a selection. A right-click
                // must fall through untouched, or opening the context menu
                // would re-anchor the drag and corrupt the pinned payload.
                if event.button != gpui_kit::MouseButton::Left {
                    return;
                }
                this.mouse_down = true;
                this.text_anchor = this.hovered_line.clone();
                tracing::trace!(
                    anchor = ?this.text_anchor.as_ref().map(|p| (p.path().to_string(), p.line())),
                    "selection anchored"
                );
            }))
            .capture_any_mouse_up(cx.listener(
                |this, event: &gpui_kit::MouseUpEvent, window, cx| {
                    if event.button != gpui_kit::MouseButton::Left {
                        return;
                    }
                    this.mouse_down = false;
                    tracing::trace!(
                        has_selection = gpui_kit::base::TextSelection::has_selection(window, cx),
                        anchor = ?this.text_anchor.as_ref().map(|p| (p.path().to_string(), p.line())),
                        hovered = ?this.hovered_line.as_ref().map(|p| (p.path().to_string(), p.line())),
                        gutter_selection = this.diff.read(cx).selected_lines().is_some(),
                        text = ?gpui_kit::base::TextSelection::selected_text(window, cx)
                            .chars()
                            .take(30)
                            .collect::<String>(),
                        "selection released"
                    );
                    if gpui_kit::base::TextSelection::has_selection(window, cx) {
                        if let (Some(start), Some(end)) =
                            (this.text_anchor.clone(), this.hovered_line.clone())
                        {
                            this.text_lines = Some((start, end));
                        }
                        // Everything the comment needs is captured here: the
                        // right-click that opens the context menu clears the
                        // live selection before its items could read it. Only
                        // a selection with real text counts: a double click
                        // reports a selection too. Line comments start from
                        // the diff's own hover button instead.
                        let payload = this.text_selection_range().and_then(|range| {
                            let text = gpui_kit::base::TextSelection::selected_text(window, cx);
                            let text = text.trim().to_string();
                            (text.chars().count() > 1).then_some((range, Some(text)))
                        });
                        if let Some(payload) = payload {
                            this.pending_comment = Some(payload);
                            tracing::trace!("comment payload captured");
                        }
                    }
                    cx.notify();
                },
            ))
            .on_action(cx.listener(|this, _: &NextChange, _, cx| {
                this.remember_position(cx);
                this.diff.update(cx, |state, cx| state.next_change(cx));
                this.follow_mark(cx);
            }))
            .on_action(cx.listener(|this, _: &NextComment, _, cx| this.jump_comment(true, cx)))
            .on_action(
                cx.listener(|this, _: &PreviousComment, _, cx| this.jump_comment(false, cx)),
            )
            .on_action(cx.listener(|this, _: &OpenSearch, window, cx| {
                this.open_search(window, cx);
            }))
            .on_action(cx.listener(|this, _: &PreviousChange, _, cx| {
                this.remember_position(cx);
                this.diff.update(cx, |state, cx| state.previous_change(cx));
                this.follow_mark(cx);
            }))
            .on_action(cx.listener(|this, _: &NextFile, _, cx| this.move_file(1, cx)))
            .on_action(cx.listener(|this, _: &PreviousFile, _, cx| this.move_file(-1, cx)))
            .on_action(cx.listener(|this, _: &ToggleMode, _, cx| this.toggle_mode(cx)))
            .on_action(cx.listener(|this, _: &ToggleOtherRevisions, _, cx| {
                this.toggle_other_revisions(cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
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
                // Hover is never cleared on leave (leave events lag), so a
                // reload can strand it on a commit the new list no longer
                // has; Space must not toggle a ghost into the picked set.
                if let Some(id) = this.hovered_commit.clone()
                    && this.commits.iter().any(|commit| commit.id == id)
                {
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
                // Only sets, never clears: un-reviewing is deliberate and
                // stays on the checkbox, out of reach of a stray keypress.
                if let Some(path) = this.current_file.clone()
                    && this.review_state(&path) != ReviewState::Reviewed
                {
                    this.set_reviewed(&path, true, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &JumpBack, _, cx| this.jump_back(cx)))
            .on_action(cx.listener(|this, _: &JumpForward, _, cx| this.jump_forward(cx)))
            .on_action(cx.listener(|this, _: &Refresh, _, cx| this.reload(cx)))
            .on_action(|_: &Quit, _, cx| cx.quit())
            // The menu stand-ins (see `commands`) reuse the shortcut
            // handlers' methods.
            .on_action(|_: &MenuAbout, _, _| show_about())
            .on_action(cx.listener(|this, _: &MenuSplitUnified, _, cx| this.toggle_mode(cx)))
            .on_action(cx.listener(|this, _: &MenuWrap, _, cx| this.toggle_wrap(cx)))
            .on_action(
                cx.listener(|this, _: &MenuHideWhitespace, _, cx| this.toggle_whitespace(cx)),
            )
            .on_action(
                cx.listener(|this, _: &MenuCollapseAll, _, cx| this.set_all_collapsed(true, cx)),
            )
            .on_action(
                cx.listener(|this, _: &MenuExpandAll, _, cx| this.set_all_collapsed(false, cx)),
            )
            .on_action(cx.listener(|this, _: &MenuFileList, _, cx| this.toggle_sidebar(cx)))
            .on_action(cx.listener(|this, _: &MenuEarlierComments, _, cx| {
                this.toggle_other_revisions(cx)
            }))
            .on_action(cx.listener(|this, _: &MenuNextChange, _, cx| {
                this.remember_position(cx);
                this.diff.update(cx, |state, cx| state.next_change(cx));
                this.follow_mark(cx);
            }))
            .on_action(cx.listener(|this, _: &MenuPreviousChange, _, cx| {
                this.remember_position(cx);
                this.diff.update(cx, |state, cx| state.previous_change(cx));
                this.follow_mark(cx);
            }))
            .on_action(cx.listener(|this, _: &MenuJumpBack, _, cx| this.jump_back(cx)))
            .on_action(cx.listener(|this, _: &MenuJumpForward, _, cx| this.jump_forward(cx)))
            .on_action(cx.listener(|this, _: &MenuNextFile, _, cx| this.move_file(1, cx)))
            .on_action(cx.listener(|this, _: &MenuPreviousFile, _, cx| this.move_file(-1, cx)))
            .on_action(cx.listener(|this, _: &MenuNextComment, _, cx| this.jump_comment(true, cx)))
            .on_action(
                cx.listener(|this, _: &MenuPreviousComment, _, cx| this.jump_comment(false, cx)),
            )
            .on_action(cx.listener(|this, _: &MenuScopeNewForMe, _, cx| {
                this.set_scope(ScopeKind::NewForMe, cx)
            }))
            .on_action(cx.listener(|this, _: &MenuScopeWholeBranch, _, cx| {
                this.set_scope(ScopeKind::WholeBranch, cx)
            }))
            .on_action(cx.listener(|this, _: &MenuScopeCustom, _, cx| {
                this.set_scope(ScopeKind::Custom, cx)
            }))
            .child(self.render_title(cx))
            .child(self.render_scope_bar(reviewed, cx))
            .when(self.search_open, |root| {
                root.child(self.render_search_bar(cx))
            })
            .child({
                let owner = cx.entity().downgrade();
                let diff_column = v_flex().flex_1().h_full().min_w_0().child(
                    div()
                        .id("diff-pane")
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .child(self.render_diff(cx))
                        // Right-click on a text selection. The payload is
                        // snapshotted when the menu opens, not read on click:
                        // choosing an item left-clicks the overlay, which
                        // clears the live selection before the handler runs.
                        .context_menu(move |menu, _, cx| {
                            let payload = owner
                                .upgrade()
                                .and_then(|app| app.read(cx).pending_comment.clone());
                            let text = payload.as_ref().and_then(|(_, text)| text.clone());
                            // The line under the right-click, for a fresh
                            // line comment with no selection - the way to
                            // start a new thread on a line whose marker
                            // owns the gutter's +.
                            let line = owner
                                .upgrade()
                                .and_then(|app| app.read(cx).hovered_line.clone());
                            let line_owner = owner.clone();
                            let comment_owner = owner.clone();
                            menu.item(
                                PopupMenuItem::new("Copy")
                                    .icon(Icon::new(Lucide::Copy))
                                    .disabled(text.is_none())
                                    .on_click(move |_, _, cx| {
                                        if let Some(text) = text.clone() {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                text,
                                            ));
                                        }
                                    }),
                            )
                            .item(
                                PopupMenuItem::new("Comment")
                                    .icon(Icon::new(Lucide::MessageSquarePlus))
                                    .disabled(payload.is_none())
                                    .on_click(move |_, window, cx| {
                                        let Some((range, selection)) = payload.clone() else {
                                            return;
                                        };
                                        let _ = comment_owner.update(cx, |this, cx| {
                                            this.pending_comment = None;
                                            this.text_lines = None;
                                            this.start_comment(range, selection, window, cx);
                                        });
                                    }),
                            )
                            .item(
                                PopupMenuItem::new("Comment line")
                                    .icon(Icon::new(Lucide::MessageSquare))
                                    .disabled(line.is_none())
                                    .on_click(move |_, window, cx| {
                                        let Some(position) = line.clone() else {
                                            return;
                                        };
                                        let _ = line_owner.update(cx, |this, cx| {
                                            let range = DiffLineRange::new(
                                                position.path().clone(),
                                                position.side(),
                                                position.line(),
                                                position.line(),
                                            );
                                            this.start_comment(range, None, window, cx);
                                        });
                                    }),
                            )
                        }),
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
                                        .child(self.render_sidebar(reviewed, cx)),
                                )
                                .child(resizable_panel().child(diff_column)),
                        )
                    } else {
                        wrap.child(diff_column)
                    }
                })
            })
            .child(self.render_status_bar(cx))
    }
}

/// Runs on a background thread. Everything the window needs for one scope.
fn load(
    backend: &dyn ReviewBackend,
    comparison: &Comparison,
    scope: &Scope,
    extend_picks: bool,
    mine: Option<ReviewFile>,
    ignore_whitespace: bool,
) -> Result<Loaded, String> {
    let text = |error: diffie_core::BackendError| error.to_string();
    let resolved = backend.resolve(comparison).map_err(text)?;
    let commits = backend.commits(&resolved).map_err(text)?;
    // Only now is it known what HEAD gained; the caller could only say
    // whether its picks meant "everything".
    let scope = match scope {
        Scope::Commits { uncommitted, .. } if extend_picks => Scope::Commits {
            picked: commits.iter().map(|commit| commit.id.clone()).collect(),
            uncommitted: *uncommitted,
        },
        other => other.clone(),
    };
    let scope = &scope;
    let picked = match scope {
        Scope::Commits { picked, .. } if extend_picks => Some(picked.clone()),
        _ => None,
    };
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

    // Open comments overrule a reviewed tick in New for me: their files keep
    // their diff so the comments stay visible and judgeable.
    let commented: Vec<String> = reviews
        .threads()
        .iter()
        .filter(|thread| !thread.resolved())
        .map(|thread| thread.comment.path.clone())
        .collect();
    let mut patch = backend
        .patch(
            &resolved,
            scope,
            ReviewInput {
                reviewed: &reviews.mine.reviewed,
                commented: &commented,
            },
            ignore_whitespace,
        )
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
    let uncommitted_paths: HashSet<String> = backend
        .uncommitted_paths(&resolved)
        .map_err(text)?
        .into_iter()
        .collect();
    let in_scope: HashSet<Oid> = match scope {
        Scope::Commits { picked, .. } => picked.iter().cloned().collect(),
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
        uncommitted_paths,
        picked,
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
    let mut regions: HashMap<(Oid, String), Option<Vec<ChangedRegion>>> = HashMap::new();
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
                    Placement::InScope { line } => Some((line, false, false)),
                    Placement::OtherRevision { line } => Some((line, true, false)),
                    // Blame lost the line: the commented code was rewritten.
                    // Follow it through the diff since its commit so the
                    // comment sits beside the fix instead of vanishing. A
                    // commit git no longer has (a rebase dropped it) cannot
                    // be followed; the comment stays unplaced, no guessing.
                    Placement::NotInView => {
                        let key = (comment.commit.clone(), comment.path.clone());
                        let regions = match regions.entry(key) {
                            Entry::Occupied(entry) => entry.into_mut(),
                            Entry::Vacant(entry) => entry.insert(backend.changed_regions(
                                &comment.commit,
                                rev,
                                &comment.path,
                            )?),
                        };
                        regions
                            .as_deref()
                            .map(|regions| (map_line(comment.line, regions).line(), false, true))
                    }
                }
            }
            // No blame for removed lines or for a combined tree: show the
            // comment at its stored line while its commit is in scope. An
            // uncommitted review has no in-scope commits, but its working
            // tree is the revision under review, so its comments stay
            // current instead of dropping out or dimming to other-revision.
            _ => (uncommitted || in_scope.contains(&comment.commit)).then_some((
                comment.line,
                false,
                false,
            )),
        };
        if let Some((line, other_revision, changed_since)) = hit {
            placed.push(Placed {
                id: comment.id.clone(),
                path: comment.path.clone(),
                side: comment.side,
                line,
                lines: comment.lines,
                selection: comment.selection.is_some(),
                other_revision,
                changed_since,
            });
        }
    }
    Ok(placed)
}
