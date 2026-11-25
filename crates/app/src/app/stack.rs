//! The stack a `--stack` review spans: the title-bar button that stands
//! in the pull-request button's place, the menu that lists the members,
//! and the refresh that keeps their states current.
//!
//! A child of `app` so it can reach `ReviewApp`'s state.

use super::pr_picker::{now_unix, pull_row};
use super::pull::Tone;
use super::*;
use diffie_core::pr::{self, FetchMode, PrSummary, Stack};
use gpui_kit::component::menu::DropdownMenu as _;

/// What the last refresh learned about the members. The resolved
/// membership and the comparison's ends are not in here on purpose: a
/// reload may say where each member stands, never move the pins
/// (owner, 2026-11-25).
pub(super) struct StackStatus {
    /// Each member's row as the forge last answered it, latest first.
    /// Only state, draft and checks are read from it.
    rows: Vec<PrSummary>,
    /// The forge now names other members than the review was resolved
    /// on. Said, never acted on: taking the new shape would change which
    /// commits are under the reviewer's eye and void a Custom selection,
    /// so it needs an explicit reopen.
    moved: bool,
}

/// The rows the menu draws: every member as it was resolved, wearing the
/// state, draft flag and checks of the last refresh. A member the refresh
/// no longer lists keeps the row it was opened with - the pins stay, so
/// the row must too.
fn rows(stack: &Stack, status: Option<&StackStatus>) -> Vec<PrSummary> {
    stack
        .members
        .iter()
        .map(|member| {
            let mut row = member.summary.clone();
            if let Some(fresh) = status
                .and_then(|status| status.rows.iter().find(|fresh| fresh.number == row.number))
            {
                row.state = fresh.state;
                row.draft = fresh.draft;
                row.checks = fresh.checks;
                row.updated = fresh.updated;
            }
            row
        })
        .collect()
}

/// "3 pull requests", for a heading and a tooltip.
fn size_words(size: usize) -> String {
    format!("{size} pull request{}", if size == 1 { "" } else { "s" })
}

impl ReviewApp {
    /// Ask the forge for the stack again - member states, drafts and
    /// checks - off the UI thread. One query, on every reload, and only
    /// what is displayed comes out of it. A stack with no number cannot
    /// be asked again: the read goes through one.
    pub(super) fn refresh_stack_status(&mut self, cx: &mut Context<Self>) {
        let Some(stack) = &self.review.stack else {
            return;
        };
        let Some(number) = stack.number else {
            return;
        };
        let pinned: Vec<u64> = stack
            .members
            .iter()
            .map(|member| member.pull.number)
            .collect();
        let forge = self.forge.clone();
        self.review._stack_status = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    forge
                        .stack_numbered(number)
                        .map_err(|error| error.to_string())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(fresh) => {
                        let members = fresh.map(|stack| stack.members).unwrap_or_default();
                        this.review.stack_status = Some(StackStatus {
                            moved: !members
                                .iter()
                                .map(|member| member.pull.number)
                                .eq(pinned.iter().copied()),
                            rows: members.into_iter().map(|member| member.summary).collect(),
                        });
                        this.review.stack_status_error = None;
                    }
                    // An older answer beats none, as for the pull
                    // request's status.
                    Err(error) => this.review.stack_status_error = Some(error.into()),
                }
                cx.notify();
            });
        });
    }

    /// Opens the whole stack of that number in this window, as `--stack`
    /// would have: resolve, fetch what is missing, then swap the review
    /// over. `adopt` replaces the review wholesale, so the way back from
    /// a member is this re-resolve and nothing lighter. On failure the
    /// current review stays and the status line says why.
    pub(super) fn open_stack(&mut self, number: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.publishing {
            self.notice = Some(format!("publishing; stack {number} can open once it lands").into());
            cx.notify();
            return;
        }
        // The inputs are per-review state too, cleared here because
        // `adopt` runs in an async completion with no window to hand the
        // input its new value.
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
                    pr::open_stack(&*backend, &*forge, number, mode, FetchMode::OnDemand)
                })
                .await;
            let _ = this.update(cx, |this, cx| match result {
                Ok((stack, comparison)) => {
                    // The latest member is the window's pull request, so
                    // the status refresh and publishing keep working.
                    let pull = stack.members.first().map(|latest| Pull {
                        forge: this.forge.clone(),
                        request: latest.pull.clone(),
                    });
                    this.adopt(comparison, pull, Some(stack), cx);
                }
                Err(message) => {
                    this.loading = false;
                    this.notice = Some(message.into());
                    cx.notify();
                }
            });
        });
    }

    /// The stack's menu behind its title-bar button. It reads top down
    /// the way the reviewer moves: what the stack is, what a refresh has
    /// to report, the whole stack again, then the members - latest
    /// first, each row the one the pull-request picker draws.
    pub(super) fn render_stack_menu(
        &self,
        stack: &Stack,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let owner = cx.weak_entity();
        let number = stack.number;
        let size = stack.members.len();
        let base = stack
            .members
            .last()
            .map(|first| first.pull.target_branch.clone())
            .unwrap_or_default();
        let moved = self
            .review
            .stack_status
            .as_ref()
            .is_some_and(|status| status.moved);
        let error = self.review.stack_status_error.clone();
        let rows = rows(stack, self.review.stack_status.as_ref());
        let now = now_unix();
        let forge = self.forge.name();
        let size_words = size_words(size);
        let heading = match number {
            Some(number) => format!("#{number} · {size_words} going into {base}"),
            None => format!("{size_words} going into {base}"),
        };
        let mut tooltip = match number {
            Some(number) => format!("Stack #{number} on {forge} - {size_words} going into {base}"),
            None => format!("Stack on {forge} - {size_words} going into {base}"),
        };
        if moved {
            tooltip.push_str(" - its members have changed since it was opened");
        }
        let changed = Tone::Waiting.color(cx);
        Button::new("stack")
            .outline()
            .xsmall()
            .h(px(24.))
            .icon(Icon::new(Lucide::Layers))
            // The stack's own number and its size; a member's number
            // never appears here, only inside the menu.
            .label(match number {
                Some(number) => format!("#{number} · {size}"),
                None => size.to_string(),
            })
            // One word for a stack whose membership moved on. The review
            // itself stays as it was opened, so the word is all there is
            // to report.
            .when(moved, |button| {
                button.child(
                    div()
                        .flex_none()
                        .text_size(px(10.))
                        .text_color(changed)
                        .child("changed"),
                )
            })
            .loading(self.publishing)
            .tooltip(tooltip)
            .dropdown_menu(move |menu, _, cx| {
                let mut menu = menu.min_w(px(320.)).label(heading.clone());
                if moved {
                    let warning = cx.theme().warning;
                    menu = menu.item(
                        PopupMenuItem::element(move |_, _| {
                            div()
                                .max_w(px(320.))
                                .text_size(px(11.))
                                .text_color(warning)
                                .child("Its members have changed; reopen the stack to review it")
                        })
                        .icon(Icon::new(Lucide::TriangleAlert).text_color(warning))
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
                // Reopening resolves the stack from its number again, so
                // this is both the way back from a member and the one
                // way a changed membership is taken on.
                if let Some(number) = number {
                    let back = owner.clone();
                    menu = menu.separator().item(
                        PopupMenuItem::new("Review the whole stack")
                            .icon(Icon::new(Lucide::Layers))
                            .on_click(move |_: &gpui_kit::ClickEvent, window, cx| {
                                let _ = back.update(cx, |this, cx| {
                                    this.open_stack(number, window, cx);
                                });
                            }),
                    );
                }
                menu = menu.separator().label("Members, latest first");
                for row in rows.iter().cloned() {
                    let member = row.number;
                    let open = owner.clone();
                    menu = menu.item(
                        PopupMenuItem::element(move |_, cx| pull_row(&row, now, cx)).on_click(
                            move |_: &gpui_kit::ClickEvent, window, cx| {
                                let _ = open.update(cx, |this, cx| {
                                    this.open_pull(member, window, cx);
                                });
                            },
                        ),
                    );
                }
                menu
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diffie_core::pr::{CheckState, PrState, PullRequest, StackMember};

    fn member(number: u64, state: PrState) -> StackMember {
        let pull = PullRequest {
            number,
            id: format!("PR_{number}"),
            url: format!("https://github.com/o/r/pull/{number}"),
            state,
            source_branch: format!("feat/{number}"),
            source_commit: "h".repeat(40),
            target_branch: "main".into(),
            target_commit: "b".repeat(40),
            cross_repository: false,
            head_owner: "o".into(),
        };
        let summary = PrSummary {
            number,
            title: format!("Member {number}"),
            author: "ada".into(),
            updated: 0,
            state,
            draft: false,
            checks: None,
            head: pull.source_branch.clone(),
            base: pull.target_branch.clone(),
        };
        StackMember { pull, summary }
    }

    fn stack() -> Stack {
        Stack {
            number: Some(42),
            members: vec![member(2, PrState::Open), member(1, PrState::Open)],
        }
    }

    #[test]
    fn a_refresh_repaints_the_members_it_names() {
        let mut fresh = member(1, PrState::Merged).summary;
        fresh.checks = Some(CheckState::Failing);
        fresh.draft = true;
        let status = StackStatus {
            rows: vec![fresh],
            moved: false,
        };
        let rows = rows(&stack(), Some(&status));
        // The member the refresh did not name keeps what it was opened
        // with; the one it named wears the fresh standing.
        assert_eq!(rows[0].state, PrState::Open);
        assert_eq!(rows[0].checks, None);
        assert_eq!(rows[1].state, PrState::Merged);
        assert_eq!(rows[1].checks, Some(CheckState::Failing));
        assert!(rows[1].draft);
        // Titles are the resolved ones throughout: a refresh paints
        // standing, never membership.
        assert_eq!(rows[0].title, "Member 2");
    }

    #[test]
    fn the_rows_are_the_resolved_members_before_any_refresh() {
        let rows = rows(&stack(), None);
        assert_eq!(
            rows.iter().map(|row| row.number).collect::<Vec<_>>(),
            [2, 1]
        );
    }

    #[test]
    fn one_member_is_still_a_stack() {
        let one = Stack {
            number: Some(42),
            members: vec![member(1, PrState::Open)],
        };
        // Hiding the button would make `--stack` look like it did
        // nothing (owner, 2026-11-25), so a stack of one still has its
        // row and its label.
        assert_eq!(rows(&one, None).len(), 1);
        assert_eq!(size_words(1), "1 pull request");
    }
}
