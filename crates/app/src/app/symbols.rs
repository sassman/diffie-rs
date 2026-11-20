//! Symbol navigation: Cmd/Ctrl-hover lights a resolvable name up as a
//! link, Cmd/Ctrl-click jumps to its definition.
//!
//! A child of `app` so it can reach `ReviewApp`'s state. `diffie-symbols`
//! parses and resolves; this module says which revision each side of the
//! diff reads, and where a jump lands: on the definition's row when the
//! diff carries it, else in a temporary read-only entry holding the item.

use std::cell::Cell;
use std::ops::Range;

use super::*;
use diffie_symbols::{FileSymbols, Origin, RevisionView, SymbolIndex, Target};
use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::component::diff::DiffTextPosition;
use gpui_kit::{Modifiers, MouseButton, Pixels, Point, anchored, point};

/// Leads the Diff path of a definition entry. The real path cannot be the
/// key: the file may already have an entry of its own - a changed file
/// whose patch does not reach the definition, or an orphan snippet. No
/// repository path starts like this, and the extension stays last so the
/// Kit still picks the syntax.
const ENTRY_PREFIX: &str = "\u{2192} ";
const OLD_ENTRY_PREFIX: &str = "\u{2192} before: ";

/// The symbols of the two revisions on screen.
struct Views {
    old: RevisionView,
    new: RevisionView,
}

impl Views {
    fn of(&self, side: DiffSide) -> &RevisionView {
        match side {
            DiffSide::Original => &self.old,
            DiffSide::Modified => &self.new,
        }
    }
}

/// The name under the pointer, known to resolve.
#[derive(Clone, PartialEq)]
pub(super) struct Link {
    pub(super) position: DiffLinePosition,
    pub(super) span: Range<usize>,
    /// The revision the name was read in, which the jump stays in.
    side: DiffSide,
    targets: Vec<Target>,
}

/// A hovered name the repository does not define: there is nowhere to
/// jump, so a card says where it comes from instead.
#[derive(Clone, PartialEq)]
pub(super) struct Hint {
    position: DiffLinePosition,
    span: Range<usize>,
    origin: Origin,
    /// Where the pointer was when it reached the name, in the window.
    at: Point<Pixels>,
}

/// What a hovered name leads to.
enum Named {
    Here(Link),
    Elsewhere {
        position: DiffLinePosition,
        span: Range<usize>,
        origin: Origin,
    },
}

/// A temporary file entry showing definitions from outside the diff.
pub(super) struct DefinitionEntry {
    /// The entry's path in the Diff.
    pub(super) key: SharedString,
    /// The file's path in the repository.
    pub(super) path: String,
    side: DiffSide,
    text: Arc<str>,
    /// First and last line of each item shown, in file order.
    items: Vec<(usize, usize)>,
    /// The file has a diff of its own that does not reach these lines.
    pub(super) in_comparison: bool,
}

impl DefinitionEntry {
    fn key_for(path: &str, side: DiffSide) -> SharedString {
        match side {
            DiffSide::Original => format!("{OLD_ENTRY_PREFIX}{path}"),
            DiffSide::Modified => format!("{ENTRY_PREFIX}{path}"),
        }
        .into()
    }

    pub(super) fn note(&self) -> &'static str {
        match (self.in_comparison, self.side) {
            (true, _) => "outside the changed lines - shown for a definition",
            (false, DiffSide::Modified) => "not in this comparison - shown for a definition",
            (false, DiffSide::Original) => {
                "not in this comparison - shown for a definition, as it was before"
            }
        }
    }

    fn file(&self) -> DiffFile {
        DiffFile::excerpts(self.key.clone(), excerpts(&self.text, &self.items))
    }
}

/// The text of each of `items`' line ranges, with its first line number.
fn excerpts<'t>(text: &'t str, items: &[(usize, usize)]) -> Vec<(usize, &'t str)> {
    let mut starts: Vec<usize> = vec![0];
    starts.extend(text.match_indices('\n').map(|(at, _)| at + 1));
    items
        .iter()
        .filter_map(|&(first, last)| {
            let begin = *starts.get(first.checked_sub(1)?)?;
            let end = starts.get(last).copied().unwrap_or(text.len());
            (begin < end).then(|| (first, &text[begin..end]))
        })
        .collect()
}

/// Adds `item` to `items`, keeping them ordered and merging what touches:
/// a method jumped to after its `impl` block is already on screen.
fn add_item(items: &mut Vec<(usize, usize)>, item: (usize, usize)) {
    items.push(item);
    items.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(items.len());
    for &(first, last) in items.iter() {
        match merged.last_mut() {
            Some(previous) if first <= previous.1 + 1 => previous.1 = previous.1.max(last),
            _ => merged.push((first, last)),
        }
    }
    *items = merged;
}

pub(super) struct Symbols {
    index: Arc<SymbolIndex>,
    /// `None` until the background build lands; hover stays inert till then.
    views: Option<Views>,
    /// A renamed file's path on the old side, by its path in the Diff.
    old_paths: HashMap<SharedString, String>,
    pub(super) link: Option<Link>,
    hint: Option<Hint>,
    /// The source text under the pointer, whatever keys are held.
    pointed: Option<DiffTextPosition>,
    pub(super) entries: Vec<DefinitionEntry>,
    picker: Entity<CommandState>,
    _build: Task<()>,
    _open: Task<()>,
}

impl Symbols {
    pub(super) fn new(window: &mut Window, cx: &mut Context<ReviewApp>) -> Self {
        Self {
            index: Arc::new(SymbolIndex::new()),
            views: None,
            old_paths: HashMap::new(),
            link: None,
            hint: None,
            pointed: None,
            entries: Vec::new(),
            picker: cx.new(|cx| CommandState::new(window, cx)),
            _build: Task::ready(()),
            _open: Task::ready(()),
        }
    }

    /// Forget everything tied to the load on screen; parsed blobs stay.
    pub(super) fn clear(&mut self) {
        self.views = None;
        self.link = None;
        self.hint = None;
        self.pointed = None;
        self.entries.clear();
        self.old_paths.clear();
    }

    pub(super) fn entry(&self, key: &str) -> Option<&DefinitionEntry> {
        self.entries.iter().find(|entry| entry.key.as_ref() == key)
    }
}

impl ReviewApp {
    /// Rebuild both sides' views for the diff just installed. Blobs the
    /// index already holds are not parsed again, so a reload costs the tree
    /// listings and whatever changed.
    pub(super) fn build_symbol_views(&mut self, cx: &mut Context<Self>) {
        // The views describe the revisions of the load they were built
        // for; entries and the link point into those.
        self.symbols.clear();
        let Some(resolved) = self.review.resolved.clone() else {
            return;
        };
        // A combined pick of non-contiguous commits is no tree git can
        // list; the branch tip stands in for it.
        let new_rev = match &self.review.new_side {
            Some(NewSide::Commit(id)) => id.clone(),
            _ => resolved.head.clone(),
        };
        let shows_uncommitted = self.review.comparison.uncommitted
            && (self.review.scope != ScopeKind::Custom || self.review.picked_uncommitted);
        let worktree: Vec<String> = if shows_uncommitted {
            self.review.uncommitted_paths.iter().cloned().collect()
        } else {
            Vec::new()
        };
        let mut shown_old = HashSet::new();
        let mut shown_new = HashSet::new();
        for file in self.diff.read(cx).files() {
            if let Some(path) = file.modified_path() {
                shown_new.insert(path.to_string());
            }
            if let Some(path) = file.original_path() {
                shown_old.insert(path.to_string());
                if path != file.path() {
                    self.symbols
                        .old_paths
                        .insert(file.path().clone(), path.to_string());
                }
            }
        }
        let revision = self.revision;
        let backend = self.backend.clone();
        let index = self.symbols.index.clone();
        self.symbols._build = cx.spawn(async move |this, cx| {
            let built = cx
                .background_spawn(async move {
                    let started = std::time::Instant::now();
                    let old = index.view(&*backend, &resolved.base, &[], &shown_old)?;
                    let new = index.view(&*backend, &new_rev, &worktree, &shown_new)?;
                    tracing::debug!(elapsed = ?started.elapsed(), "symbol views built");
                    Ok::<_, diffie_core::BackendError>(Views { old, new })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                // A newer load is on its way and builds its own.
                if this.revision != revision {
                    return;
                }
                match built {
                    Ok(views) => this.symbols.views = Some(views),
                    Err(error) => {
                        this.notice = Some(format!("Symbol navigation is off: {error}").into())
                    }
                }
                cx.notify();
            });
        });
    }

    /// Which revision and repository path a Diff line reads its symbols
    /// from.
    fn symbol_site(&self, position: &DiffLinePosition) -> (DiffSide, String) {
        if let Some(entry) = self.symbols.entry(position.path()) {
            return (entry.side, entry.path.clone());
        }
        let path = match position.side() {
            DiffSide::Original => self.symbols.old_paths.get(position.path()).cloned(),
            DiffSide::Modified => None,
        };
        (
            position.side(),
            path.unwrap_or_else(|| position.path().to_string()),
        )
    }

    fn name_at(&self, hit: &DiffTextPosition) -> Option<Named> {
        let views = self.symbols.views.as_ref()?;
        let position = hit.position();
        let (side, path) = self.symbol_site(position);
        let view = views.of(side);
        let reference = view.reference_at(&path, position.line(), hit.offset())?;
        // The view is the nearest real revision, not always the exact
        // content on screen: New for me diffs a ticked file against its
        // tick. Trust a reference only where the row spells its name.
        let written = hit.text().get(reference.column.clone())?;
        if written != reference.name && written != "Self" {
            return None;
        }
        let targets = view.resolve(&path, reference);
        if targets.is_empty() {
            return Some(Named::Elsewhere {
                position: position.clone(),
                span: reference.column.clone(),
                origin: view.origin(&path, reference)?,
            });
        }
        Some(Named::Here(Link {
            position: position.clone(),
            span: reference.column.clone(),
            side,
            targets,
        }))
    }

    fn link_at(&self, hit: &DiffTextPosition) -> Option<Link> {
        match self.name_at(hit)? {
            Named::Here(link) => Some(link),
            Named::Elsewhere { .. } => None,
        }
    }

    pub(super) fn symbol_hover(
        &mut self,
        hit: Option<&DiffTextPosition>,
        modifiers: Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.symbols.pointed = hit.cloned();
        let named = hit
            .filter(|_| modifiers.secondary(cx))
            .and_then(|hit| self.name_at(hit));
        let (link, hint) = match named {
            Some(Named::Here(link)) => (Some(link), None),
            Some(Named::Elsewhere {
                position,
                span,
                origin,
            }) => {
                // The card stays where the pointer met the name instead of
                // trailing every move across it.
                let at = self
                    .symbols
                    .hint
                    .as_ref()
                    .filter(|hint| hint.position == position && hint.span == span)
                    .map_or_else(|| window.mouse_position(), |hint| hint.at);
                (
                    None,
                    Some(Hint {
                        position,
                        span,
                        origin,
                        at,
                    }),
                )
            }
            None => (None, None),
        };
        if self.symbols.link != link || self.symbols.hint != hint {
            self.symbols.link = link;
            self.symbols.hint = hint;
            cx.notify();
        }
    }

    /// The full name of the symbol under the pointer, for the context
    /// menu's copy: known by the same rules as the hover. Tied
    /// definitions with different names have no one name to copy.
    pub(super) fn pointed_symbol(&self) -> Option<String> {
        let views = self.symbols.views.as_ref()?;
        match self.name_at(self.symbols.pointed.as_ref()?)? {
            Named::Elsewhere { origin, .. } => Some(origin.path),
            Named::Here(link) => {
                let view = views.of(link.side);
                let mut names: Vec<String> = link
                    .targets
                    .iter()
                    .filter_map(|target| view.qualified_name(target))
                    .collect();
                names.dedup();
                (names.len() == 1).then(|| names.remove(0))
            }
        }
    }

    /// The card naming where the hovered name comes from, at the pointer.
    pub(super) fn render_origin_hint(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let hint = self.symbols.hint.as_ref()?;
        let origin = &hint.origin;
        // Below the pointer, clear of the row being read.
        let at = hint.at + point(px(0.), px(16.));
        let card = v_flex()
            .px(px(8.))
            .py(px(4.))
            .rounded(px(4.))
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .shadow_md()
            .child(
                div()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(px(12.))
                    .child(origin.path.clone()),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "from {} {} - not in this repository",
                        origin.package_word, origin.package
                    )),
            );
        Some(
            deferred(
                anchored()
                    .position(at)
                    .snap_to_window_with_margin(px(8.))
                    .child(card),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// A Cmd/Ctrl-click on source text. True when the name resolved and
    /// the click was spent on it.
    pub(super) fn symbol_click(
        &mut self,
        hit: &DiffTextPosition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(link) = self.link_at(hit) else {
            return false;
        };
        self.symbols.link = None;
        let mut targets = link.targets;
        if targets.len() == 1 {
            self.open_definition(targets.remove(0), link.side, cx);
        } else {
            self.pick_definition(targets, link.side, window, cx);
        }
        cx.notify();
        true
    }

    /// Whether `path` is a Diff entry the comparison did not put there.
    fn synthetic_entry(&self, path: &str) -> bool {
        self.symbols.entry(path).is_some()
            || self
                .review
                .orphan_files
                .iter()
                .any(|file| file.path().as_ref() == path)
    }

    fn open_definition(&mut self, target: Target, side: DiffSide, cx: &mut Context<Self>) {
        let line = target.definition.line;
        let state = self.diff.read(cx);
        let in_diff = state
            .files()
            .iter()
            .filter(|file| !self.synthetic_entry(file.path()))
            .find(|file| {
                let path = match side {
                    DiffSide::Original => file.original_path(),
                    DiffSide::Modified => file.modified_path(),
                };
                path.is_some_and(|path| path.as_ref() == target.path)
            })
            .map(|file| file.path().clone());
        if let Some(path) = &in_diff
            && state.has_line(&DiffLinePosition::new(path.clone(), side, line))
        {
            let core_side = match side {
                DiffSide::Original => Side::Old,
                DiffSide::Modified => Side::New,
            };
            self.show_hit(path, core_side, line, cx);
            self.mark_landing(path.clone(), side, line, cx);
            return;
        }
        let key = DefinitionEntry::key_for(&target.path, side);
        // The file is loaded already: only the item is new.
        if let Some(entry) = self.symbols.entry(&key) {
            let (text, in_comparison) = (entry.text.clone(), entry.in_comparison);
            self.show_definition(target, side, in_comparison, text, None, cx);
            return;
        }
        let Some((blob, worktree)) = self
            .symbols
            .views
            .as_ref()
            .and_then(|views| views.of(side).source(&target.path))
            .map(|(blob, worktree)| (blob.clone(), worktree))
        else {
            return;
        };
        let revision = self.revision;
        let backend = self.backend.clone();
        let path = target.path.clone();
        let in_comparison = in_diff.is_some();
        self.symbols._open = cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_spawn(async move {
                    let bytes = if worktree {
                        backend.worktree_file(&path)?
                    } else {
                        backend.blobs(std::slice::from_ref(&blob))?.remove(&blob)
                    };
                    Ok::<_, diffie_core::BackendError>(bytes.map(|bytes| {
                        let symbols = diffie_symbols::parse(&path, &bytes, true);
                        let text: Arc<str> = String::from_utf8_lossy(&bytes).into();
                        (text, symbols)
                    }))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                // The revision the target was resolved in is gone.
                if this.revision != revision {
                    return;
                }
                match loaded {
                    Ok(Some((text, symbols))) => {
                        this.show_definition(target, side, in_comparison, text, symbols, cx)
                    }
                    Ok(None) => {
                        this.notice = Some(
                            format!("Could not read {} for the definition", target.path).into(),
                        );
                        cx.notify();
                    }
                    Err(error) => {
                        this.notice =
                            Some(format!("Could not open the definition: {error}").into());
                        cx.notify();
                    }
                }
            });
        });
    }

    /// Put `target`'s item into its file's definition entry and land on
    /// it. `symbols` carries the file's references when it was just read,
    /// so names inside the entry can be followed in turn.
    fn show_definition(
        &mut self,
        target: Target,
        side: DiffSide,
        in_comparison: bool,
        text: Arc<str>,
        symbols: Option<FileSymbols>,
        cx: &mut Context<Self>,
    ) {
        let key = DefinitionEntry::key_for(&target.path, side);
        let item = (target.definition.first_line, target.definition.last_line);
        match self
            .symbols
            .entries
            .iter_mut()
            .find(|entry| entry.key == key)
        {
            Some(entry) => add_item(&mut entry.items, item),
            None => self.symbols.entries.push(DefinitionEntry {
                key: key.clone(),
                path: target.path.clone(),
                side,
                text,
                items: vec![item],
                in_comparison,
            }),
        }
        if let (Some(views), Some(symbols)) = (self.symbols.views.as_mut(), symbols) {
            let view = match side {
                DiffSide::Original => &mut views.old,
                DiffSide::Modified => &mut views.new,
            };
            view.set_symbols(&target.path, Arc::new(symbols));
        }
        self.remember_position(cx);
        self.sync_diff_files(cx);
        // An excerpt file holds its lines on the modified side, whichever
        // revision they were read from.
        let line = target.definition.line;
        self.diff.update(cx, |state, cx| {
            state.set_file_collapsed(&key, false, cx);
            state.scroll_to_line(
                DiffLinePosition::new(key.clone(), DiffSide::Modified, line),
                cx,
            );
        });
        self.mark_landing(key.clone(), DiffSide::Modified, line, cx);
        self.review.current_file = Some(key);
        cx.notify();
    }

    /// The landing mark change navigation uses, on the definition's name.
    fn mark_landing(
        &mut self,
        path: SharedString,
        side: DiffSide,
        line: usize,
        cx: &mut Context<Self>,
    ) {
        self.diff.update(cx, |state, cx| {
            state.set_focused_ranges([DiffLineRange::new(path, side, line, line)], cx);
        });
    }

    /// Hand the Diff its file set again: the comparison's files in tree
    /// order with the orphan snippets when their toggle is on, then the
    /// definition entries. The shared sync puts the folds back, which
    /// `set_files` starts over; callers restore or move the viewport
    /// themselves.
    pub(super) fn sync_diff_files(&mut self, cx: &mut Context<Self>) {
        let state = self.diff.read(cx);
        let mut files: Vec<DiffFile> = state
            .files()
            .iter()
            .filter(|file| !self.synthetic_entry(file.path()))
            .cloned()
            .collect();
        if self.show_other_revisions {
            files.extend(self.review.orphan_files.iter().cloned());
        }
        files.sort_by(|a, b| tree_order(a.path().as_ref(), b.path().as_ref()));
        files.extend(self.symbols.entries.iter().map(DefinitionEntry::file));
        self.sync_files_and_folds(files, cx);
    }

    /// The x on a definition entry's header.
    pub(super) fn close_definition(&mut self, key: &str, cx: &mut Context<Self>) {
        let top = self.diff.read(cx).top_line();
        self.symbols
            .entries
            .retain(|entry| entry.key.as_ref() != key);
        if self.review.current_file.as_deref() == Some(key) {
            self.review.current_file = None;
        }
        self.sync_diff_files(cx);
        // Closing an entry below must not lose the reader's place above;
        // a place inside the closed entry has nothing to return to.
        if let Some(top) = top.filter(|top| top.path().as_ref() != key) {
            self.diff
                .update(cx, |state, cx| state.scroll_to_top_line(top, cx));
        }
        cx.notify();
    }

    /// Several definitions rank the same: the reviewer picks. A dialog
    /// over the Kit's command list, like the palette, so arrows, Enter,
    /// Escape and typing to narrow all work.
    fn pick_definition(
        &mut self,
        targets: Vec<Target>,
        side: DiffSide,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.symbols.picker.clone();
        state.update(cx, |state, cx| {
            state.set_query("", window, cx);
            if state.matched_count() > 0 {
                state.set_selected_index(Some(Default::default()), window, cx);
            }
        });
        let owner = cx.weak_entity();
        let focus_on_mount = Rc::new(Cell::new(true));
        let targets = Rc::new(targets);
        window.open_dialog(cx, move |dialog, _, _| {
            let state = state.clone();
            let owner = owner.clone();
            let focus_on_mount = focus_on_mount.clone();
            let targets = targets.clone();
            dialog
                .close_button(false)
                .p_0()
                .content(move |content, window, cx| {
                    if focus_on_mount.replace(false) {
                        let state = state.clone();
                        window.defer(cx, move |window, cx| {
                            state.read(cx).focus_handle(cx).focus(window, cx);
                        });
                    }
                    let muted = cx.theme().muted_foreground;
                    let mono = cx.theme().mono_font_family.clone();
                    let items = targets.iter().map(|target| {
                        let definition = &target.definition;
                        let place: SharedString =
                            format!("{}:{}", target.path, definition.line).into();
                        let kind = definition.kind.label();
                        let signature: SharedString = definition.signature.clone().into();
                        let mono = mono.clone();
                        CommandItem::new()
                            .label(format!("{} {}", definition.name, place))
                            .keywords([
                                kind.to_string(),
                                definition.container.clone().unwrap_or_default(),
                            ])
                            .child(move |_, _| {
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .truncate()
                                            .font_family(mono.clone())
                                            .text_size(px(12.))
                                            .child(signature.clone()),
                                    )
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .text_size(px(11.))
                                            .text_color(muted)
                                            .child(kind)
                                            .child(div().truncate().child(place.clone())),
                                    )
                            })
                    });
                    let owner = owner.clone();
                    let targets = targets.clone();
                    content.child(
                        Command::new(&state)
                            .bordered(false)
                            .rounded(cx.theme().radius_lg)
                            .placeholder("Several definitions match - pick one...")
                            .items(items)
                            .on_confirm(move |index, window, cx| {
                                let Some(target) = targets.get(index.row).cloned() else {
                                    return;
                                };
                                window.close_dialog(cx);
                                let _ = owner.update(cx, |this, cx| {
                                    this.open_definition(target, side, cx);
                                });
                            }),
                    )
                })
        });
    }

    /// A definition entry's row in the file panel, under the tree: it is
    /// in no changeset, so it carries the link icon instead of a status.
    pub(super) fn render_definition_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        // The row list is built a frame before the list paints its rows;
        // an entry closed in between leaves its row empty, not a panic.
        let Some(entry) = self.symbols.entries.get(index) else {
            return div().into_any_element();
        };
        let muted = cx.theme().muted_foreground;
        let name = entry
            .path
            .rsplit_once('/')
            .map_or(entry.path.as_str(), |(_, name)| name)
            .to_string();
        let selected = self.review.current_file.as_ref() == Some(&entry.key);
        let target = entry.key.clone();
        let closing = entry.key.clone();
        let tip: SharedString = format!("{} - {}", entry.path, entry.note()).into();
        h_flex()
            .id(entry.key.clone())
            .w_full()
            .h(px(22.))
            .pl(px(4.))
            .pr(px(4.))
            .gap(px(6.))
            .rounded(px(4.))
            .cursor_pointer()
            .text_color(muted)
            .when(selected, |row| row.bg(cx.theme().accent))
            .hover(|row| row.bg(cx.theme().list_hover))
            .on_click(cx.listener(move |this, _, _, cx| this.reveal_file(target.clone(), cx)))
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .child(Icon::new(Lucide::Link).xsmall().text_color(muted))
            .child(div().flex_1().min_w_0().truncate().child(name))
            .child(
                div()
                    .id(SharedString::from(format!("close-row-{closing}")))
                    .flex_none()
                    // The row's own click would reveal the entry being closed.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.close_definition(&closing, cx)
                    }))
                    .child(Icon::new(Lucide::X).xsmall().text_color(muted)),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "one\ntwo\nthree\nfour\nfive";

    #[test]
    fn excerpts_cut_whole_lines_at_their_numbers() {
        assert_eq!(
            excerpts(TEXT, &[(2, 3), (5, 5)]),
            [(2, "two\nthree\n"), (5, "five")]
        );
        // A range past the end keeps what exists; one wholly outside goes.
        assert_eq!(excerpts(TEXT, &[(4, 9), (7, 8)]), [(4, "four\nfive")]);
        assert!(excerpts("", &[(1, 1)]).is_empty());
    }

    #[test]
    fn items_stay_ordered_and_merge_when_they_touch() {
        let mut items = vec![(10, 20)];
        add_item(&mut items, (40, 45));
        add_item(&mut items, (1, 3));
        assert_eq!(items, [(1, 3), (10, 20), (40, 45)]);
        // Inside, overlapping and directly adjacent all merge.
        add_item(&mut items, (12, 14));
        add_item(&mut items, (18, 25));
        add_item(&mut items, (26, 30));
        assert_eq!(items, [(1, 3), (10, 30), (40, 45)]);
    }

    #[test]
    fn entry_keys_never_collide_with_a_repository_path() {
        let new = DefinitionEntry::key_for("src/app.rs", DiffSide::Modified);
        let old = DefinitionEntry::key_for("src/app.rs", DiffSide::Original);
        assert_ne!(new, old);
        assert!(new.ends_with("src/app.rs") && old.ends_with("src/app.rs"));
        assert_ne!(new.as_ref(), "src/app.rs");
    }
}
