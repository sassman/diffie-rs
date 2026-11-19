//! The pull-request picker: "Open a pull request..." in the palette shows
//! the repository's pull requests in a popover styled like the palette
//! itself, and the picked one opens in this window as `--pr` would have.
//!
//! A child of `app` so it can reach `ReviewApp`'s state. The Kit's
//! `Command` draws and filters; this module fetches the lists, shapes the
//! rows and swaps the window's review over.

use std::cell::Cell;

use super::pull::{Tone, state_icon};
use super::*;
use diffie_core::pr::{self, CheckState, FetchMode, PrArgument, PrList, PrState, PrSummary};
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::command::{Command, CommandItem};

/// What the picker shows. Both lists refetch on every open: the point of
/// the picker is the repository's present, not a cache of its past.
#[derive(Default)]
pub(super) struct PrRows {
    open: Rows,
    /// `Idle` until the reviewer asks for merged and closed.
    finished: Rows,
    /// Guards list answers the way `revision` guards loads: an answer for
    /// an earlier opening of the picker must not land in a later one.
    generation: usize,
}

#[derive(Default)]
enum Rows {
    #[default]
    Idle,
    Loading,
    Failed(SharedString),
    Ready(Vec<PrSummary>),
}

/// What a row runs when confirmed. Chosen by identity, never by list
/// position: the list is rebuilt on every frame and may change under the
/// cursor while an answer arrives.
#[derive(Clone, Copy)]
enum PickerRow {
    Pull(u64),
    ShowFinished,
    /// Loading and failure rows: visible, never runnable.
    Inert,
}

/// A row the reviewer can read but not run.
fn inert(label: impl Into<SharedString>) -> (CommandItem, PickerRow) {
    (
        CommandItem::new().label(label).disabled(true),
        PickerRow::Inert,
    )
}

/// The two-row item of one pull request, echoing the commit picker: state
/// and title, then number, author, age and the checks' verdict in the PR
/// widget's tones.
fn pull_item(summary: &PrSummary, now: i64) -> (CommandItem, PickerRow) {
    let row = PickerRow::Pull(summary.number);
    let item = CommandItem::new().label(summary.title.clone()).keywords([
        summary.number.to_string(),
        format!("#{}", summary.number),
        summary.author.clone(),
    ]);
    let summary = summary.clone();
    let item = item.child(move |_, cx| {
        let muted = cx.theme().muted_foreground;
        let state_tone = match (summary.state, summary.draft) {
            (PrState::Open, false) => Tone::Good,
            (PrState::Open, true) => Tone::Quiet,
            (PrState::Merged, _) => Tone::Settled,
            (PrState::Closed, _) => Tone::Bad,
        };
        let mut title = h_flex()
            .gap_2()
            .min_w_0()
            .items_center()
            .child(
                div()
                    .text_color(state_tone.color(cx))
                    .child(Icon::new(state_icon(summary.state)).xsmall()),
            )
            .child(div().truncate().child(summary.title.clone()));
        if summary.draft {
            title = title.child(div().text_size(px(10.)).text_color(muted).child("Draft"));
        }
        let mut meta = h_flex()
            .gap_1()
            .text_size(px(11.))
            .text_color(muted)
            .child(format!("#{}", summary.number))
            .child(Icon::new(Lucide::CircleUserRound).xsmall())
            .child(div().truncate().child(summary.author.clone()))
            .child(format!("· {}", ago(summary.updated, now)));
        if let Some(checks) = summary.checks {
            let (words, tone) = checks_words(checks);
            meta = meta.child(div().text_color(tone.color(cx)).child(format!("· {words}")));
        }
        v_flex().min_w_0().flex_1().child(title).child(meta)
    });
    (item, row)
}

fn checks_words(checks: CheckState) -> (&'static str, Tone) {
    match checks {
        CheckState::Passing => ("checks passing", Tone::Good),
        CheckState::Running => ("checks running", Tone::Waiting),
        CheckState::Failing => ("checks failing", Tone::Bad),
    }
}

/// Open first - the non-drafts before the drafts - then whatever of the
/// finished list the reviewer asked for. Items and row actions come from
/// the one walk, so a confirmed row cannot name another pull request.
fn entries(rows: &PrRows, now: i64) -> (Vec<CommandItem>, Vec<PickerRow>) {
    let mut items = Vec::new();
    let mut actions = Vec::new();
    let mut push = |(item, action): (CommandItem, PickerRow)| {
        items.push(item);
        actions.push(action);
    };
    match &rows.open {
        Rows::Idle | Rows::Loading => push(inert("Loading pull requests…")),
        Rows::Failed(message) => push(inert(message.clone())),
        Rows::Ready(list) if list.is_empty() => push(inert("No open pull requests")),
        Rows::Ready(list) => {
            for summary in list.iter().filter(|summary| !summary.draft) {
                push(pull_item(summary, now));
            }
            for summary in list.iter().filter(|summary| summary.draft) {
                push(pull_item(summary, now));
            }
        }
    }
    match &rows.finished {
        // Merged and closed stay out of the way until asked for, and
        // only once the open list can be read: its loading row would
        // otherwise sit under a lone "show more".
        Rows::Idle => {
            if let Rows::Ready(_) = rows.open {
                push((
                    CommandItem::new()
                        .label("Show merged and closed…")
                        .keywords(["history", "archive", "finished"]),
                    PickerRow::ShowFinished,
                ));
            }
        }
        Rows::Loading => push(inert("Loading merged and closed…")),
        Rows::Failed(message) => push(inert(message.clone())),
        Rows::Ready(list) if list.is_empty() => push(inert("No merged or closed pull requests")),
        Rows::Ready(list) => {
            for summary in list {
                push(pull_item(summary, now));
            }
        }
    }
    (items, actions)
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

impl ReviewApp {
    /// Opens the picker as a dialog styled like the palette and starts the
    /// fresh fetch of the open list.
    pub(super) fn open_pr_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pr_rows.generation += 1;
        self.pr_rows.open = Rows::Loading;
        self.pr_rows.finished = Rows::Idle;
        self.load_pr_list(PrList::Open, cx);
        let state = self.pr_picker.clone();
        // The state outlives the dialog: without this the picker would
        // reopen on the last query and the row last run.
        state.update(cx, |state, cx| {
            state.set_query("", window, cx);
            if state.matched_count() > 0 {
                state.set_selected_index(Some(Default::default()), window, cx);
            }
        });
        let owner = cx.weak_entity();
        let focus_on_mount = Rc::new(Cell::new(true));
        window.open_dialog(cx, move |dialog, _, _| {
            let state = state.clone();
            let owner = owner.clone();
            let focus_on_mount = focus_on_mount.clone();
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
                    let Some(app) = owner.upgrade() else {
                        return content;
                    };
                    let (items, actions) = entries(&app.read(cx).pr_rows, now_unix());
                    let owner = owner.clone();
                    content.child(
                        Command::new(&state)
                            .bordered(false)
                            .rounded(cx.theme().radius_lg)
                            .placeholder("Open a pull request…")
                            .items(items)
                            .on_confirm(move |index, window, cx| {
                                match actions.get(index.row) {
                                    Some(PickerRow::Pull(number)) => {
                                        let number = *number;
                                        window.close_dialog(cx);
                                        let _ = owner.update(cx, |this, cx| {
                                            this.open_pull(number, window, cx);
                                        });
                                    }
                                    // The dialog stays up; the rows grow in
                                    // place under the typed query.
                                    Some(PickerRow::ShowFinished) => {
                                        let _ = owner.update(cx, |this, cx| {
                                            this.pr_rows.finished = Rows::Loading;
                                            this.load_pr_list(PrList::Finished, cx);
                                        });
                                    }
                                    Some(PickerRow::Inert) | None => {}
                                }
                            }),
                    )
                })
        });
    }

    /// Asks the forge for one list in the background. One task slot for
    /// both lists is enough: the finished list can only be requested once
    /// the open list has landed.
    fn load_pr_list(&mut self, which: PrList, cx: &mut Context<Self>) {
        let generation = self.pr_rows.generation;
        let forge = self.forge.clone();
        self._pr_rows = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { forge.pull_requests(which) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.pr_rows.generation != generation {
                    return;
                }
                let rows = match result {
                    Ok(list) => Rows::Ready(list),
                    Err(error) => Rows::Failed(error.to_string().into()),
                };
                match which {
                    PrList::Open => this.pr_rows.open = rows,
                    PrList::Finished => this.pr_rows.finished = rows,
                }
                cx.notify();
            });
        });
    }

    /// Opens the picked pull request in this window, as `--pr` would have:
    /// resolve, fetch what is missing, then swap the review over. On
    /// failure the current review stays and the status line says why.
    pub(super) fn open_pull(&mut self, number: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.publishing {
            // The palette entry is greyed while publishing, but the picker
            // may already be up when a publish starts; the pick must not
            // vanish without a word.
            self.notice = Some(format!("publishing; #{number} can open once it lands").into());
            cx.notify();
            return;
        }
        // The inputs are per-review state too, cleared here because
        // `adopt` runs in an async completion with no window to hand the
        // input its new value. A failed open leaves them cleared: losing
        // a stale filter is no loss.
        self.picker_query
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.search_query
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.loading = true;
        self.notice = None;
        cx.notify();
        let backend = self.backend.clone();
        let forge = self.forge.clone();
        let mode = self.review.comparison.mode;
        self._open_pull = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let argument = PrArgument { number, url: None };
                    pr::open(&*backend, &*forge, &argument, mode, FetchMode::OnDemand)
                })
                .await;
            let _ = this.update(cx, |this, cx| match result {
                Ok((request, comparison)) => {
                    let pull = Pull {
                        forge: this.forge.clone(),
                        request,
                    };
                    this.adopt(comparison, Some(pull), cx);
                }
                Err(message) => {
                    this.loading = false;
                    this.notice = Some(message.into());
                    cx.notify();
                }
            });
        });
    }

    /// Replaces what the window reviews, as if the app had been started on
    /// the new comparison: the one `PerReview` value is swapped for a
    /// fresh one, then `reload` rebuilds the view. The window itself, the
    /// settings, the theme and the palette stay; the picker and search
    /// inputs are cleared by `open_pull`, which has the window this async
    /// completion lacks.
    fn adopt(&mut self, comparison: Comparison, pull: Option<Pull>, cx: &mut Context<Self>) {
        // A pending save still writes the old review file before the
        // review is replaced: directly when no save runs (the task
        // captures file and path at call time), else parked for the save
        // chain's next turn, which would otherwise find the swapped
        // review and write nothing.
        if self.save_pending {
            if self.save_running {
                self.parting_save = self
                    .review
                    .reviews
                    .as_ref()
                    .map(|set| (set.mine.clone(), self.review.review_path.clone()));
            } else {
                self.write_review(cx);
            }
        }
        // The whole per-review value at once: the compiler keeps the swap
        // complete, a field-by-field reset could silently miss one.
        self.review = PerReview::fresh(comparison, pull);
        self.reload(cx);
    }
}
