//! The command palette: a searchable list of what the reviewer can do
//! right now, behind Cmd/Ctrl+P.
//!
//! A child of `app` so it can reach `ReviewApp`'s state. The Kit's
//! `Command` draws and filters; this module only says which commands
//! exist and runs the chosen one.

use std::cell::Cell;

use super::*;
use diffie_core::forge::Verdict;
use gpui_kit::Action;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::command::{Command, CommandItem};
use gpui_kit::component::kbd::Kbd;

pub(super) use gpui_kit::component::command::CommandState;

/// What the palette can run. Chosen by identity, never by list position:
/// the list is rebuilt on every frame and may change under the cursor.
#[derive(Clone, Copy)]
enum PaletteCommand {
    Reload,
    ToggleMode,
    Back,
    Forward,
    OpenPullRequest,
    Approve { with_comments: bool },
}

struct PaletteEntry {
    command: PaletteCommand,
    label: SharedString,
    /// Search terms beyond the label: what a reviewer might type instead.
    keywords: &'static [&'static str],
    enabled: bool,
    /// The shortcut action doing the same, for the row's key hint only.
    hint: Option<Box<dyn Action>>,
}

impl PaletteEntry {
    /// `diff` is where the shortcuts live: a hint is looked up from there,
    /// since the palette's own focus sits outside every binding's context.
    fn item(&self, diff: &FocusHandle) -> CommandItem {
        let item = CommandItem::new()
            .label(self.label.clone())
            .keywords(self.keywords.iter().copied())
            .disabled(!self.enabled);
        let Some(hint) = self.hint.as_ref().map(|hint| hint.boxed_clone()) else {
            return item;
        };
        // Not `CommandItem::action`: the Kit would dispatch it from the
        // dialog's focus, outside the review view that handles it, and
        // single-key actions are bound to the diff alone.
        let label = self.label.clone();
        let diff = diff.clone();
        item.child(move |window, _| {
            h_flex()
                .flex_1()
                .items_center()
                .justify_between()
                .gap_2()
                .child(label.clone())
                .children(Kbd::binding_for_action_in(hint.as_ref(), &diff, window))
        })
    }
}

impl ReviewApp {
    /// Every command the palette offers in the current state. A command
    /// that exists but cannot run now stays in the list, greyed; the pull
    /// request's only exist in a review opened from one.
    fn palette_entries(&self) -> Vec<PaletteEntry> {
        let mut entries = vec![
            PaletteEntry {
                command: PaletteCommand::Reload,
                label: "Reload".into(),
                keywords: &["refresh", "update"],
                enabled: true,
                hint: Some(Box::new(Refresh)),
            },
            PaletteEntry {
                command: PaletteCommand::ToggleMode,
                label: match self.manual_mode {
                    DiffMode::Split => "Switch to unified view",
                    _ => "Switch to split view",
                }
                .into(),
                keywords: &["split", "unified", "side by side", "layout", "mode"],
                enabled: true,
                hint: Some(Box::new(ToggleMode)),
            },
            // The way back from a jump: to a definition, a comment, a
            // search hit or a file.
            PaletteEntry {
                command: PaletteCommand::Back,
                label: "Go back".into(),
                keywords: &["return", "previous", "jump", "definition", "history"],
                enabled: !self.back_stack.is_empty(),
                hint: Some(Box::new(JumpBack)),
            },
            PaletteEntry {
                command: PaletteCommand::Forward,
                label: "Go forward".into(),
                keywords: &["next", "jump", "definition", "history"],
                enabled: !self.forward_stack.is_empty(),
                hint: Some(Box::new(JumpForward)),
            },
            PaletteEntry {
                command: PaletteCommand::OpenPullRequest,
                label: "Open a pull request…".into(),
                keywords: &["pr", "pull request", "review", "github"],
                // One publish at a time, and a swap under a running
                // publish would blur which review it reported on.
                enabled: !self.publishing,
                hint: None,
            },
        ];
        if let Some(pull) = &self.pull {
            let number = pull.request.number;
            for with_comments in [true, false] {
                entries.push(PaletteEntry {
                    command: PaletteCommand::Approve { with_comments },
                    label: if with_comments {
                        format!("Approve #{number} with comments")
                    } else {
                        format!("Approve #{number} without comments")
                    }
                    .into(),
                    keywords: &["pull request", "pr", "lgtm", "accept"],
                    enabled: self.can_approve(pull, with_comments),
                    hint: None,
                });
            }
        }
        entries
    }

    fn run_palette_command(
        &mut self,
        command: PaletteCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            PaletteCommand::Reload => self.reload(cx),
            PaletteCommand::ToggleMode => self.toggle_mode(cx),
            PaletteCommand::Back => self.jump_back(cx),
            PaletteCommand::Forward => self.jump_forward(cx),
            PaletteCommand::OpenPullRequest => self.open_pr_picker(window, cx),
            PaletteCommand::Approve { with_comments } => {
                self.publish(Verdict::Approve, with_comments, cx)
            }
        }
    }

    /// Opens the palette as a dialog. The Kit's dialog hands focus back
    /// to whatever held it once it closes, so the reviewer returns to the
    /// diff line or the comment box they left.
    pub(super) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.palette.clone();
        // The state outlives the dialog: without this the palette would
        // reopen on the last query and the row last run.
        state.update(cx, |state, cx| {
            state.set_query("", window, cx);
            // Before the first render there is no row to name, and asking
            // for one would pin "no selection"; the Kit picks the first.
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
                    let entries = app.read(cx).palette_entries();
                    let diff = app.read(cx).diff.read(cx).focus_handle(cx);
                    // Rows and commands come from the one list, so the
                    // confirmed row cannot name another command.
                    let commands: Vec<PaletteCommand> =
                        entries.iter().map(|entry| entry.command).collect();
                    let owner = owner.clone();
                    content.child(
                        Command::new(&state)
                            .bordered(false)
                            .rounded(cx.theme().radius_lg)
                            .placeholder("Run a command...")
                            .items(entries.iter().map(|entry| entry.item(&diff)))
                            .on_confirm(move |index, window, cx| {
                                let Some(command) = commands.get(index.row).copied() else {
                                    return;
                                };
                                window.close_dialog(cx);
                                let _ = owner.update(cx, |this, cx| {
                                    this.run_palette_command(command, window, cx);
                                });
                            }),
                    )
                })
        });
    }
}
