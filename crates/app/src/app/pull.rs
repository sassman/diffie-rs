//! The pull request a `--pr` review is connected to: where it stands, the
//! title-bar menu, and sending the review to it.
//!
//! A child of `app` so it can reach `ReviewApp`'s state; everything that
//! names a forge or a pull request in the window lives here.

use super::*;
use diffie_core::forge::{Forge, Outcome, Pending, Verdict};
use diffie_core::pr::{CheckState, PrState, PullRequest, PullStatus, ReviewDecision};
use gpui_kit::component::menu::DropdownMenu as _;

/// The pull request the review was opened from (`--pr`), with the forge
/// that hosts it. Publishing and approving exist only with one.
pub struct Pull {
    pub forge: Arc<dyn Forge>,
    pub request: PullRequest,
}

/// "open · approved · checks failing": where the pull request stands, as
/// the menu's first line and the icon's tooltip say it. A part with
/// nothing to report (no review yet, no checks) is left out.
fn status_words(status: &PullStatus) -> String {
    let state = match status.state {
        PrState::Open => "open",
        PrState::Merged => "merged",
        PrState::Closed => "closed",
    };
    let review = status.review.map(|review| match review {
        ReviewDecision::Approved => "approved",
        ReviewDecision::ChangesRequested => "changes requested",
        ReviewDecision::Required => "review required",
    });
    let checks = status.checks.as_ref().map(|checks| match checks.state {
        CheckState::Passing => "checks passing",
        CheckState::Running => "checks running",
        CheckState::Failing => "checks failing",
    });
    [Some(state), review, checks]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
}

/// What a status row says at a glance, before its words are read; the
/// theme gives each tone its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tone {
    /// Nothing stands in the way: open, approved, passing.
    Good,
    /// Under way or waiting: checks running, a review still required.
    Waiting,
    /// Wants the reviewer's eye: failing checks, requested changes, closed.
    Bad,
    /// Over and done: merged.
    Settled,
    /// Nothing to report.
    Quiet,
}

impl Tone {
    fn color(self, cx: &App) -> Hsla {
        match self {
            Tone::Good => cx.theme().success,
            Tone::Waiting => cx.theme().warning,
            Tone::Bad => cx.theme().danger,
            Tone::Settled => cx.theme().info,
            Tone::Quiet => cx.theme().muted_foreground,
        }
    }
}

/// One line of the pull request's standing: an icon in the tone's colour,
/// what the line is about, and its value.
#[derive(Clone)]
struct StatusRow {
    icon: Lucide,
    label: &'static str,
    value: &'static str,
    tone: Tone,
}

fn state_icon(state: PrState) -> Lucide {
    match state {
        PrState::Open => Lucide::GitPullRequest,
        PrState::Merged => Lucide::GitMerge,
        PrState::Closed => Lucide::GitPullRequestClosed,
    }
}

/// The standing as three rows, always the same three so the menu keeps
/// its shape: a part the forge has not answered yet, or has nothing for,
/// says so quietly instead of leaving a gap.
fn status_rows(state: PrState, status: Option<&PullStatus>) -> [StatusRow; 3] {
    let (value, tone) = match state {
        PrState::Open => ("Open", Tone::Good),
        PrState::Merged => ("Merged", Tone::Settled),
        PrState::Closed => ("Closed", Tone::Bad),
    };
    let state_row = StatusRow {
        icon: state_icon(state),
        label: "State",
        value,
        tone,
    };
    let unread = |label| StatusRow {
        icon: Lucide::CircleDashed,
        label,
        value: "…",
        tone: Tone::Quiet,
    };
    let Some(status) = status else {
        return [state_row, unread("Review"), unread("Checks")];
    };
    let (icon, value, tone) = match status.review {
        Some(ReviewDecision::Approved) => (Lucide::CircleCheck, "Approved", Tone::Good),
        Some(ReviewDecision::ChangesRequested) => {
            (Lucide::MessageSquareWarning, "Changes requested", Tone::Bad)
        }
        Some(ReviewDecision::Required) => (Lucide::Eye, "Review required", Tone::Waiting),
        None => (Lucide::CircleDashed, "No review yet", Tone::Quiet),
    };
    let review = StatusRow {
        icon,
        label: "Review",
        value,
        tone,
    };
    let (icon, value, tone) = match status.checks.as_ref().map(|checks| checks.state) {
        Some(CheckState::Passing) => (Lucide::CircleCheck, "Passing", Tone::Good),
        Some(CheckState::Running) => (Lucide::Clock, "Running", Tone::Waiting),
        Some(CheckState::Failing) => (Lucide::CircleX, "Failing", Tone::Bad),
        None => (Lucide::CircleDashed, "No checks", Tone::Quiet),
    };
    let checks = StatusRow {
        icon,
        label: "Checks",
        value,
        tone,
    };
    [state_row, review, checks]
}

/// The one tone the title-bar icon's dot carries: the loudest thing the
/// review decision and the checks have to say. `None` leaves the icon bare
/// - nothing read yet, or nothing to report.
fn trigger_tone(status: Option<&PullStatus>) -> Option<Tone> {
    let [_, review, checks] = status_rows(PrState::Open, Some(status?));
    [Tone::Bad, Tone::Waiting, Tone::Good]
        .into_iter()
        // A required review is the normal state of an open pull request;
        // amber for it would never go away, so only the checks may wait.
        .find(|tone| checks.tone == *tone || (review.tone == *tone && *tone != Tone::Waiting))
}

/// "2 comments, 1 edit": what a publish holds, in the menu's and the
/// status bar's words. `None` when there is nothing.
fn pending_label(pending: Pending) -> Option<String> {
    let part = |count: usize, noun: &str| {
        (count > 0).then(|| format!("{count} {noun}{}", if count == 1 { "" } else { "s" }))
    };
    let parts: Vec<String> = [
        part(pending.comments, "comment"),
        part(pending.edits, "edit"),
        part(pending.reactions, "reaction"),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// The status-bar line after a publish, and whether it reports a failure.
/// A partial publish names what landed before the complaint: those
/// comments are on the pull request now, and silence would hide it.
fn publish_summary(outcome: &Outcome, number: u64) -> (String, bool) {
    // A comment that moved from its line to its file did land, but not
    // where the reviewer put it; the line says so instead of plain success.
    let sent = pending_label(outcome.done()).map(|sent| match outcome.on_file() {
        0 => sent,
        moved => format!("{sent} ({moved} on the file, no line took it)"),
    });
    let verdict = match outcome.verdict {
        Some(Verdict::Approve) => Some("approved"),
        Some(Verdict::RequestChanges) => Some("requested changes on"),
        Some(Verdict::Comment) | None => None,
    };
    let landed = match (verdict, sent) {
        (Some(verdict), Some(sent)) => Some(format!("{verdict} #{number} with {sent}")),
        (Some(verdict), None) => Some(format!("{verdict} #{number}")),
        (None, Some(sent)) => Some(format!("published {sent} to #{number}")),
        (None, None) => None,
    };
    let complaint = outcome.error.as_ref().map(|error| match outcome.refused {
        0 => format!("failed: {error}"),
        1 => format!("1 comment was refused: {error}"),
        refused => format!("{refused} comments were refused: {error}"),
    });
    match (complaint, landed) {
        (Some(complaint), Some(landed)) => (format!("{landed}, but {complaint}"), true),
        (Some(complaint), None) => (format!("nothing reached #{number}, {complaint}"), true),
        (None, Some(landed)) => (landed, false),
        (None, None) => (format!("nothing to publish to #{number}"), false),
    }
}

impl ReviewApp {
    /// Whether the view still shows the pull request as it was opened.
    /// Swapped, the review file belongs to the reverse comparison, and
    /// nothing in it is the pull request's to receive.
    pub(super) fn pull_in_view(&self) -> bool {
        self.pull.as_ref().is_some_and(|pull| {
            self.comparison.source_rev.as_deref() == Some(pull.request.source_commit.as_str())
        })
    }

    /// Ask the forge where the pull request stands - merged or not, the
    /// review decision, the checks - off the UI thread. Runs with every
    /// reload and after every publish; there is no timer behind it.
    pub(super) fn refresh_pull_status(&mut self, cx: &mut Context<Self>) {
        let Some(pull) = &self.pull else {
            return;
        };
        let forge = pull.forge.clone();
        let request = pull.request.clone();
        self._pull_status = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    forge.status(&request).map_err(|error| error.to_string())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(status) => {
                        this.pull_status = Some(status);
                        this.pull_status_error = None;
                    }
                    Err(error) => this.pull_status_error = Some(error.into()),
                }
                cx.notify();
            });
        });
    }

    /// What a publish would send. Nothing while the view is swapped.
    pub(super) fn pull_pending(&self) -> Pending {
        self.reviews
            .as_ref()
            .filter(|_| self.pull_in_view())
            .map(diffie_core::forge::pending)
            .unwrap_or_default()
    }

    /// A merged or closed pull request still takes comments, but no
    /// approval. Until the forge answers, the state read at startup
    /// stands in.
    pub(super) fn pull_state(&self, pull: &Pull) -> PrState {
        self.pull_status
            .as_ref()
            .map_or(pull.request.state, |status| status.state)
    }

    /// Whether an approval can go out, and whether it has comments to
    /// carry: the title-bar menu and the command palette grey the same
    /// items.
    pub(super) fn can_approve(&self, pull: &Pull, with_comments: bool) -> bool {
        !self.publishing
            && self.pull_in_view()
            && self.pull_state(pull) == PrState::Open
            && (!with_comments || pending_label(self.pull_pending()).is_some())
    }

    /// Send the review to the pull request: the unpublished comments (with
    /// edits and reactions) when `with_comments`, under `verdict`. The
    /// forge's ids come back into the reviewer's own file through the one
    /// writer, so a second publish never posts a comment twice.
    pub(super) fn publish(
        &mut self,
        verdict: Verdict,
        with_comments: bool,
        cx: &mut Context<Self>,
    ) {
        if self.publishing || !self.pull_in_view() {
            return;
        }
        let (Some(pull), Some(reviews), Some(resolved)) =
            (&self.pull, &self.reviews, &self.resolved)
        else {
            return;
        };
        let forge = pull.forge.clone();
        let request = pull.request.clone();
        let number = request.number;
        let reviews = reviews.clone();
        let head = resolved.head.clone();
        let backend = self.backend.clone();
        self.publishing = true;
        self.publish_status = None;
        cx.spawn(async move |this, cx| {
            // The review first, and its ids into the file before anything
            // else is sent: edits and reactions go one call at a time, and
            // a quit or a hung call among them must not leave posted
            // comments looking unpublished - the next publish would post
            // them again.
            let (review, plan) = {
                let (forge, request) = (forge.clone(), request.clone());
                cx.background_spawn(async move {
                    // A file gone at head has no blame; its comments go on
                    // the file instead of failing the publish.
                    let plan =
                        diffie_core::forge::plan(&reviews, &head, verdict, with_comments, |path| {
                            backend.blame(&head, path).ok()
                        });
                    let review = diffie_core::forge::publish_review(&*forge, &request, &plan);
                    (review, plan)
                })
                .await
            };
            let landed = review.review_landed();
            if this
                .update(cx, |this, cx| this.record_published(&review, cx))
                .is_err()
            {
                return;
            }
            let mut outcome = review;
            if landed {
                let followups = cx
                    .background_spawn(async move {
                        diffie_core::forge::publish_followups(&*forge, &request, &plan)
                    })
                    .await;
                let _ = this.update(cx, |this, cx| this.record_published(&followups, cx));
                outcome.merge(followups);
            }
            let _ = this.update(cx, |this, cx| {
                this.publishing = false;
                // An approval or a review changes the decision.
                this.refresh_pull_status(cx);
                let (summary, failed) = publish_summary(&outcome, number);
                this.publish_status = Some((summary.into(), failed));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Write what a publish step achieved into the reviewer's own file,
    /// through the one writer.
    fn record_published(&mut self, outcome: &Outcome, cx: &mut Context<Self>) {
        if let Some(set) = self.reviews.as_mut()
            && set.mine.record(outcome)
        {
            self.save_review(cx);
            cx.notify();
        }
    }

    /// The pull request's menu behind its title-bar icon. It reads top
    /// down the way the reviewer decides: where the pull request stands,
    /// the pages to look at, then what to send - comments first, the
    /// approval last and apart, because it is the one verdict.
    pub(super) fn render_pull_menu(&self, pull: &Pull, cx: &mut Context<Self>) -> impl IntoElement {
        let number = pull.request.number;
        let forge = pull.forge.name();
        let url = pull.request.url.clone();
        let owner = cx.weak_entity();
        let in_view = self.pull_in_view();
        let busy = self.publishing;
        let pending = self.pull_pending();
        let status = self.pull_status.clone();
        let state = self.pull_state(pull);
        let open = state == PrState::Open;
        let can_approve_with = self.can_approve(pull, true);
        let can_approve_without = self.can_approve(pull, false);
        let rows = status_rows(state, status.as_ref());
        let error = self.pull_status_error.clone();
        let tooltip = match (&status, &error) {
            (Some(status), _) => {
                format!(
                    "Pull request #{number} on {forge} - {}",
                    status_words(status)
                )
            }
            (None, Some(_)) => format!("Pull request #{number} on {forge} - state unknown"),
            (None, None) => format!("Pull request #{number} on {forge}"),
        };
        let latest_run = status
            .as_ref()
            .and_then(|status| status.checks.as_ref())
            .and_then(|checks| checks.latest_run.clone());
        let heading = format!(
            "#{number} · {} going into {}",
            pull.request.source_branch, pull.request.target_branch
        );
        // Why the sending items are grey, when the label alone cannot say.
        let hint = if !in_view {
            Some("Swap back to the pull request's direction to send")
        } else if !open {
            Some("Only an open pull request takes a verdict")
        } else {
            None
        };
        let dot = trigger_tone(status.as_ref()).map(|tone| tone.color(cx));
        Button::new("pull")
            .outline()
            .xsmall()
            .h(px(24.))
            .icon(Icon::new(state_icon(state)))
            .label(format!("#{number}"))
            // The checks' colour rides on the trigger, so red CI is seen
            // without opening anything. A dot, not a tinted icon: the
            // icon's shape already says open, merged or closed.
            .when_some(dot, |button, color| {
                button.child(div().flex_none().size(px(6.)).rounded_full().bg(color))
            })
            .loading(busy)
            .tooltip(tooltip)
            .dropdown_menu(move |menu, _, cx| {
                let send = |verdict: Verdict, with_comments: bool| {
                    let owner = owner.clone();
                    move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                        let _ = owner.update(cx, |this, cx| {
                            this.publish(verdict, with_comments, cx);
                        });
                    }
                };
                let sent = pending_label(pending);
                let mut menu = menu.min_w(px(280.)).label(heading.clone());
                for row in rows.iter().cloned() {
                    let color = row.tone.color(cx);
                    menu = menu.item(
                        // Disabled: a status line is read, not clicked, so
                        // it takes no hover and no keyboard stop.
                        PopupMenuItem::element(move |_, cx| {
                            h_flex()
                                .w_full()
                                .gap_3()
                                .justify_between()
                                .child(
                                    div()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(row.label),
                                )
                                .child(div().font_medium().text_color(color).child(row.value))
                        })
                        .icon(Icon::new(row.icon).text_color(color))
                        .disabled(true),
                    );
                }
                if let Some(error) = error.clone() {
                    let danger = cx.theme().danger;
                    menu = menu.item(
                        PopupMenuItem::element(move |_, _| {
                            div()
                                .max_w(px(320.))
                                .text_size(px(11.))
                                .text_color(danger)
                                .child(format!("Not refreshed: {error}"))
                        })
                        .icon(Icon::new(Lucide::TriangleAlert).text_color(danger))
                        .disabled(true),
                    );
                }
                menu = menu
                    .separator()
                    .item(
                        PopupMenuItem::link("Open pull request", url.clone())
                            .icon(Icon::new(Lucide::GitPullRequestArrow)),
                    )
                    .item(
                        PopupMenuItem::link(
                            "Open latest CI run",
                            latest_run.clone().unwrap_or_default(),
                        )
                        .icon(Icon::new(Lucide::Workflow))
                        .disabled(latest_run.is_none()),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new(match &sent {
                            Some(sent) => format!("Publish {sent}"),
                            None => "Nothing new to publish".to_string(),
                        })
                        .icon(Icon::new(Lucide::Send))
                        .disabled(busy || sent.is_none())
                        .on_click(send(Verdict::Comment, true)),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new(match &sent {
                            Some(sent) => format!("Approve with {sent}"),
                            None => "Approve with comments".to_string(),
                        })
                        .icon(Icon::new(Lucide::CheckCheck))
                        .disabled(!can_approve_with)
                        .on_click(send(Verdict::Approve, true)),
                    )
                    .item(
                        PopupMenuItem::new("Approve without comments")
                            .icon(Icon::new(Lucide::Check))
                            .disabled(!can_approve_without)
                            .on_click(send(Verdict::Approve, false)),
                    )
                    .item(
                        // Edits and reactions alone do not count: the
                        // request must say what to change, in a comment
                        // this very review publishes. The greyed label
                        // says so, since a menu item has no tooltip.
                        PopupMenuItem::new(match &sent {
                            Some(sent) if pending.comments > 0 => {
                                format!("Request changes with {sent}")
                            }
                            _ => "Request changes - needs a new comment".to_string(),
                        })
                        .icon(Icon::new(Lucide::MessageSquareWarning))
                        .disabled(busy || !open || pending.comments == 0)
                        .on_click(send(Verdict::RequestChanges, true)),
                    );
                match hint {
                    Some(hint) => menu.separator().label(hint),
                    None => menu,
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diffie_core::pr::Checks;

    fn status(
        state: PrState,
        review: Option<ReviewDecision>,
        checks: Option<CheckState>,
    ) -> PullStatus {
        PullStatus {
            state,
            review,
            checks: checks.map(|state| Checks {
                state,
                latest_run: None,
            }),
        }
    }

    #[test]
    fn the_standing_leaves_out_what_has_nothing_to_report() {
        assert_eq!(status_words(&status(PrState::Merged, None, None)), "merged");
        assert_eq!(
            status_words(&status(
                PrState::Open,
                Some(ReviewDecision::Approved),
                Some(CheckState::Failing)
            )),
            "open · approved · checks failing"
        );
        assert_eq!(
            status_words(&status(PrState::Open, None, Some(CheckState::Running))),
            "open · checks running"
        );
    }

    fn tones(rows: &[StatusRow; 3]) -> [(&'static str, Tone); 3] {
        [
            (rows[0].value, rows[0].tone),
            (rows[1].value, rows[1].tone),
            (rows[2].value, rows[2].tone),
        ]
    }

    #[test]
    fn the_rows_keep_their_shape_before_the_forge_answers() {
        assert_eq!(
            tones(&status_rows(PrState::Open, None)),
            [("Open", Tone::Good), ("…", Tone::Quiet), ("…", Tone::Quiet)]
        );
    }

    #[test]
    fn each_row_carries_the_tone_of_what_it_reports() {
        let merged = status(PrState::Merged, Some(ReviewDecision::Approved), None);
        assert_eq!(
            tones(&status_rows(merged.state, Some(&merged))),
            [
                ("Merged", Tone::Settled),
                ("Approved", Tone::Good),
                ("No checks", Tone::Quiet)
            ]
        );
        let red = status(
            PrState::Open,
            Some(ReviewDecision::ChangesRequested),
            Some(CheckState::Failing),
        );
        assert_eq!(
            tones(&status_rows(red.state, Some(&red))),
            [
                ("Open", Tone::Good),
                ("Changes requested", Tone::Bad),
                ("Failing", Tone::Bad)
            ]
        );
    }

    #[test]
    fn the_trigger_dot_says_the_loudest_thing() {
        let tone = |review, checks| trigger_tone(Some(&status(PrState::Open, review, checks)));
        assert_eq!(trigger_tone(None), None);
        assert_eq!(tone(None, None), None);
        assert_eq!(tone(None, Some(CheckState::Passing)), Some(Tone::Good));
        assert_eq!(tone(None, Some(CheckState::Running)), Some(Tone::Waiting));
        assert_eq!(
            tone(Some(ReviewDecision::Approved), Some(CheckState::Failing)),
            Some(Tone::Bad)
        );
        assert_eq!(
            tone(
                Some(ReviewDecision::ChangesRequested),
                Some(CheckState::Passing)
            ),
            Some(Tone::Bad)
        );
        // A review that is merely required never paints the dot amber.
        assert_eq!(tone(Some(ReviewDecision::Required), None), None);
        assert_eq!(
            tone(Some(ReviewDecision::Required), Some(CheckState::Passing)),
            Some(Tone::Good)
        );
    }

    fn pending(comments: usize, edits: usize, reactions: usize) -> Pending {
        Pending {
            comments,
            edits,
            reactions,
        }
    }

    #[test]
    fn names_only_what_there_is_with_plurals() {
        assert_eq!(pending_label(pending(0, 0, 0)), None);
        assert_eq!(pending_label(pending(1, 0, 0)).unwrap(), "1 comment");
        assert_eq!(
            pending_label(pending(2, 1, 3)).unwrap(),
            "2 comments, 1 edit, 3 reactions"
        );
    }

    #[test]
    fn a_bare_approval_and_a_bare_failure_read_plainly() {
        let mut outcome = Outcome::default();
        assert_eq!(
            publish_summary(&outcome, 17),
            ("nothing to publish to #17".to_string(), false)
        );
        outcome.verdict = Some(Verdict::Approve);
        assert_eq!(
            publish_summary(&outcome, 17),
            ("approved #17".to_string(), false)
        );
        outcome.error = Some("locked".into());
        assert_eq!(
            publish_summary(&outcome, 17),
            ("approved #17, but failed: locked".to_string(), true)
        );
        outcome.verdict = Some(Verdict::RequestChanges);
        assert_eq!(
            publish_summary(&outcome, 17),
            (
                "requested changes on #17, but failed: locked".to_string(),
                true
            )
        );
        outcome.verdict = None;
        assert_eq!(
            publish_summary(&outcome, 17),
            ("nothing reached #17, failed: locked".to_string(), true)
        );
        outcome.refused = 2;
        assert_eq!(
            publish_summary(&outcome, 17),
            (
                "nothing reached #17, 2 comments were refused: locked".to_string(),
                true
            )
        );
    }
}
