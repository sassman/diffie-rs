mod app;
mod settings;

use std::path::PathBuf;
use std::sync::Arc;

use diffie_core::forge::Forge;
use diffie_core::github::GitHubCli;
use diffie_core::{CompareMode, Comparison, GitCli, ReviewBackend as _};
use gpui_kit::{
    AppContext as _, Bounds, WindowBounds, WindowOptions, component::TitleBar, point, px, size,
};

const USAGE: &str = "\
Usage: diffie [-C <path>] [<revision> [<revision>]] [--every-difference]
       diffie [-C <path>] --pr <pull-request> [--every-difference]
       diffie [-C <path>] --stack <number> [--every-difference]
       diffie [-C <path>] --prompt [<review>] [--all]
       diffie [-C <path>] --reply <id> <answer> [--session <id>]

Reads in the merge direction. With no revision, reviews the uncommitted
changes going into the checked-out branch - and with a clean working
tree, what the branch itself adds going into the default branch (what
the remotes' HEAD names, else main, else master). With one revision, reviews
everything the working tree adds going into it. With two revisions (or A..B),
reviews the first going into the second: `diffie topic main` is topic's
changes going into main. A revision only a remote has is taken from there
when exactly one remote knows it. -C runs as if started in <path> (default:
the current directory). Requires git on PATH.

--pr reviews a GitHub pull request. Give its number, or its URL (which
pins host and repository, GitHub Enterprise included). The GitHub CLI
(gh) describes the PR, and the review pins the PR's exact commits -
never a same-named local branch, whose tip can differ; the title keeps
the branch names. A commit that is not fetched here is refused with the
`git fetch` to run - diffie never fetches. The window then carries a
pull-request menu: open it in the browser, publish the review's comments
to it, approve it. Requires gh on PATH.

--stack reviews a whole stack of pull requests as one comparison: the
latest member's branch going into the branch the first member merges
into. Give the number of any member, or the stack's own. GitHub's native
stacks are the source, so a pull request in none is refused, naming
--pr. The window carries no stack controls; it shows that one
comparison. Requires gh on PATH.

--prompt opens no window: it prints the open review comments the way the
app's \"Copy all as prompt\" button does, and exits. Without a review name
the repository's only review is printed; with several, they are listed and
one must be named. --all includes resolved comments, marked as such.

--reply opens no window either: it threads <answer> under the comment whose
id starts with <id> (--prompt prints the ids), writing the agent's own
review file. Replies are conversation, never verdict: resolving stays a
human reaction in the app. --session records the agent conversation the
answer came from (default: $CLAUDE_SESSION_ID); the app then offers
`claude --resume <id>` behind the answer's robot badge.

--version (-V) prints the version and exits.";

fn exit_usage(message: &str) -> ! {
    eprintln!("{message}\n\n{USAGE}");
    std::process::exit(2);
}

/// The core's resolution rule (`pr::remote_ref`) with the command line's
/// answer to failure: print and exit. `missing` words the no-remote case,
/// because the callers know different things: a PR base exists upstream
/// but is not fetched, while an arbitrary revision may not exist anywhere.
fn remote_ref(backend: &GitCli, branch: &str, missing: impl FnOnce() -> String) -> String {
    diffie_core::pr::remote_ref(backend, branch, missing).unwrap_or_else(|message| {
        eprintln!("{message}");
        std::process::exit(1);
    })
}

/// The branch a bare start compares against when nothing names one: what
/// the remotes' HEAD records as the default branch, else `main`, else
/// `master` - the first that can anchor a comparison here (a revision, or
/// carried by exactly one remote, which the resolve step then takes).
/// `None` keeps the uncommitted default instead of failing a bare start
/// over a branch nobody named.
fn default_target(backend: &GitCli) -> Option<String> {
    let comparable = |branch: &String| {
        backend.revision_exists(branch).unwrap_or(false)
            || backend
                .remotes_with_branch(branch)
                .is_ok_and(|remotes| remotes.len() == 1)
    };
    backend
        .default_branch()
        .ok()
        .flatten()
        .into_iter()
        .chain(["main".to_string(), "master".to_string()])
        .find(comparable)
}

fn main() {
    // RUST_LOG picks the filter (e.g. diffie=trace for the selection flow);
    // without it only warnings reach stderr.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("diffie=warn")),
        )
        .with_writer(std::io::stderr)
        .init();
    let mut repository = None;
    let mut revisions: Vec<String> = Vec::new();
    let mut mode = CompareMode::BranchAdds;
    let mut prompt_mode = false;
    let mut include_resolved = false;
    let mut reply: Option<(String, String)> = None;
    let mut session: Option<String> = None;
    let mut pr: Option<String> = None;
    let mut stack: Option<String> = None;
    let mut arguments = std::env::args_os().skip(1);
    while let Some(argument) = arguments.next() {
        let text = argument.to_string_lossy().into_owned();
        match text.as_str() {
            "--help" | "-h" => {
                println!("{USAGE}");
                return;
            }
            "--version" | "-V" => {
                println!("diffie {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--every-difference" => mode = CompareMode::EveryDifference,
            "--prompt" => prompt_mode = true,
            "--all" => include_resolved = true,
            "--reply" => {
                let (Some(id), Some(body)) = (arguments.next(), arguments.next()) else {
                    exit_usage("--reply needs a comment id and the answer.");
                };
                // Agents paste the id as --prompt prints it, `#` included.
                let id = id.to_string_lossy().trim_start_matches('#').to_string();
                reply = Some((id, body.to_string_lossy().into_owned()));
            }
            "--session" => {
                let Some(value) = arguments.next() else {
                    exit_usage("--session needs the agent conversation's id.");
                };
                session = Some(value.to_string_lossy().into_owned());
            }
            "--pr" => {
                let Some(value) = arguments.next() else {
                    exit_usage("--pr needs a pull-request number or URL.");
                };
                pr = Some(value.to_string_lossy().into_owned());
            }
            "--stack" => {
                let Some(value) = arguments.next() else {
                    exit_usage("--stack needs the number of a pull request or of a stack.");
                };
                stack = Some(value.to_string_lossy().into_owned());
            }
            "-C" => {
                let Some(value) = arguments.next() else {
                    exit_usage("-C needs a path.");
                };
                repository = Some(PathBuf::from(value));
            }
            _ if !text.starts_with('-') => {
                // A bare word is always a revision; only a token spelled like
                // a path that names a real directory is a mistaken project
                // argument. A branch such as `feature/login` passes through
                // because no such directory exists.
                let path_like = text.contains('/') || text.starts_with('.');
                if path_like && std::path::Path::new(&text).is_dir() {
                    exit_usage(&format!(
                        "`{text}` is a directory; pick the project with -C {text}"
                    ));
                }
                // git's range spellings name both ends in one token; both
                // ends keep the typed order, source first. The merge-base
                // rule makes two and three dots the same review.
                if let Some((from, into)) = text.split_once("...").or_else(|| text.split_once(".."))
                {
                    let or_head = |rev: &str| {
                        if rev.is_empty() {
                            "HEAD".to_string()
                        } else {
                            rev.to_string()
                        }
                    };
                    revisions.extend([or_head(from), or_head(into)]);
                } else {
                    revisions.push(text);
                }
            }
            _ => exit_usage(&format!("unknown argument `{text}`.")),
        }
    }
    if revisions.len() > 2 {
        exit_usage("at most two revisions.");
    }
    if prompt_mode && revisions.len() > 1 {
        exit_usage("--prompt takes at most one name: the review to print.");
    }
    if include_resolved && !prompt_mode {
        exit_usage("--all only applies to --prompt.");
    }
    if reply.is_some() && (prompt_mode || !revisions.is_empty()) {
        exit_usage("--reply takes only a comment id and the answer.");
    }
    if session.is_some() && reply.is_none() {
        exit_usage("--session only applies to --reply.");
    }
    if pr.is_some() && !revisions.is_empty() {
        exit_usage("--pr names the branches itself; drop the revisions.");
    }
    if pr.is_some() && prompt_mode {
        exit_usage("--pr opens a review; it does not combine with --prompt.");
    }
    if pr.is_some() && reply.is_some() {
        exit_usage("--pr opens a review; it does not combine with --reply.");
    }
    if stack.is_some() && pr.is_some() {
        exit_usage("--stack reviews the whole stack; it does not combine with --pr.");
    }
    if stack.is_some() && !revisions.is_empty() {
        exit_usage("--stack names the branches itself; drop the revisions.");
    }
    if stack.is_some() && prompt_mode {
        exit_usage("--stack opens a review; it does not combine with --prompt.");
    }
    if stack.is_some() && reply.is_some() {
        exit_usage("--stack opens a review; it does not combine with --reply.");
    }

    let path = repository.unwrap_or_else(|| PathBuf::from("."));
    let backend = match GitCli::open(&path) {
        Ok(backend) => Arc::new(backend),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    if let Some((id, body)) = reply {
        // Headless: the agent answers a comment back. "Claude" rather than
        // `user.name`, which is the human the agent works for - the thread
        // must show who actually wrote the answer.
        const REPLY_AUTHOR: &str = "Claude";
        // The flag wins; the environment covers runners that export it.
        let session = session.or_else(|| std::env::var("CLAUDE_SESSION_ID").ok());
        let result = backend
            .review_dir()
            .map_err(|error| error.to_string())
            .and_then(|dir| {
                diffie_core::review::reply(&dir, &id, &body, REPLY_AUTHOR, session.as_deref())
                    .map_err(|error| error.to_string())
            });
        match result {
            Ok(to) => {
                eprintln!("Replied under {}:{} in {}", to.path, to.line, to.review);
                return;
            }
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
    }
    if prompt_mode {
        // Headless: agents and scripts read the review back without a window.
        let review = revisions.first().map(String::as_str);
        let result = backend
            .review_dir()
            .map_err(|error| error.to_string())
            .and_then(|dir| {
                diffie_core::review::prompt(&dir, review, include_resolved)
                    .map_err(|error| error.to_string())
            });
        match result {
            Ok((picked, text)) => {
                if review.is_none() {
                    eprintln!("Review: {picked}");
                }
                print!("{text}");
                return;
            }
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
    }
    // The code host, for the in-app picker; `--pr` connects the review to
    // it right away. Only ever asked on the reviewer's explicit action.
    let forge: Arc<dyn Forge> = Arc::new(GitHubCli::new(&path));
    // The review's pull request, kept for the window: publishing and
    // approving exist only when the review was opened from one. The
    // resolution itself is shared with the in-app picker (`pr::open`);
    // only the picker may fetch - the command line prints the fetch to
    // run instead.
    let mut connected = None;
    let pr_comparison = pr.map(|value| {
        let argument = match diffie_core::pr::parse(&value) {
            Ok(argument) => argument,
            Err(message) => exit_usage(&message),
        };
        let opened = diffie_core::pr::open(
            &*backend,
            &*forge,
            &argument,
            mode,
            diffie_core::pr::FetchMode::Never,
        );
        let (pull, comparison) = match opened {
            Ok(opened) => opened,
            Err(message) => {
                eprintln!("{message}");
                std::process::exit(1);
            }
        };
        connected = Some(app::Pull {
            forge: forge.clone(),
            request: pull,
        });
        comparison
    });
    // The whole stack as one comparison: the latest member's head going
    // into the first member's base. The latest member keeps the
    // pull-request button, its status and its menu working; nothing here
    // shows the stack itself yet.
    let stack_comparison = stack.map(|value| {
        let argument = match diffie_core::pr::parse(&value) {
            Ok(argument) => argument,
            Err(message) => exit_usage(&message),
        };
        let opened = diffie_core::pr::open_stack(
            &*backend,
            &*forge,
            &argument,
            mode,
            diffie_core::pr::FetchMode::Never,
        );
        let (stack, comparison) = match opened {
            Ok(opened) => opened,
            Err(message) => {
                eprintln!("{message}");
                std::process::exit(1);
            }
        };
        if let Some(latest) = stack.pulls.first() {
            connected = Some(app::Pull {
                forge: forge.clone(),
                request: latest.clone(),
            });
        }
        comparison
    });
    let branch = backend.current_branch().ok().flatten();
    let working_tree = backend.has_working_tree().unwrap_or(false);
    // Both flags pin the comparison, and they are mutually exclusive.
    let pinned = pr_comparison.or(stack_comparison);
    let comparison = pinned.unwrap_or_else(|| match revisions.as_slice() {
        // `diffie A B` and `diffie A..B` read in the merge direction: A's
        // changes going into B. (`git diff` spells it the other way round;
        // that reading kept surprising the owner.)
        [from, into] => Comparison {
            source: from.clone(),
            target: into.clone(),
            mode,
            uncommitted: false,
            ..Default::default()
        },
        // `diffie REV` reads like `git diff REV`: the working tree against
        // it. On the checked-out branch that is just the uncommitted work.
        [into] if working_tree => Comparison {
            source: if branch.as_deref() == Some(into.as_str()) {
                "uncommitted".to_string()
            } else {
                "working tree".to_string()
            },
            target: into.clone(),
            mode,
            uncommitted: true,
            ..Default::default()
        },
        [into] => exit_usage(&format!(
            "no working tree to diff against `{into}`; give two revisions."
        )),
        [] => {
            // Opened plainly, the review is the uncommitted work going into
            // the checked-out branch. A clean working tree has no such
            // review, so the default becomes what the branch itself adds to
            // the default branch (`diffie HEAD..main` spelled out) - but
            // only when that reads as a review: a checked-out branch that is
            // not the default itself, and a default branch to compare
            // against (see `default_target`). A bare repository or a
            // detached HEAD has neither and falls back to branch against
            // the default.
            let branch_into_default = (working_tree
                && !backend.has_uncommitted_changes().unwrap_or(true))
            .then(|| default_target(&backend))
            .flatten()
            .filter(|target| branch.as_deref().is_some_and(|name| name != target));
            if let Some(target) = branch_into_default {
                Comparison {
                    source: branch.clone().unwrap_or_default(),
                    target,
                    mode,
                    uncommitted: false,
                    ..Default::default()
                }
            } else if let Some(into) = branch.clone().filter(|_| working_tree) {
                Comparison {
                    source: "uncommitted".to_string(),
                    target: into,
                    mode,
                    uncommitted: true,
                    ..Default::default()
                }
            } else {
                Comparison {
                    // Name the review after the branch, not the literal HEAD.
                    source: branch.clone().unwrap_or_else(|| "HEAD".to_string()),
                    // `main` when nothing better is known: the resolve step
                    // then words the error.
                    target: default_target(&backend).unwrap_or_else(|| "main".to_string()),
                    mode,
                    uncommitted: false,
                    ..Default::default()
                }
            }
        }
        _ => unreachable!("the length was checked above"),
    });
    // A mistyped revision fails here, not inside an empty window. A branch
    // only the remotes know is accepted when exactly one remote has it, and
    // the review then names that remote's ref; several candidates are a
    // question the user must answer, not a guess.
    let resolve = |revision: &str| -> String {
        let exists = backend.revision_exists(revision).unwrap_or_else(|error| {
            eprintln!("{error}");
            std::process::exit(1);
        });
        if exists {
            return revision.to_string();
        }
        remote_ref(&backend, revision, || {
            format!("`{revision}` is not a revision here, and no remote has it.")
        })
    };
    let comparison = Comparison {
        // The uncommitted pseudo-source names the working tree, not a ref,
        // and a pinned rev (--pr) was already validated against its exact
        // commit, so its display name resolves to nothing on purpose.
        source: if comparison.uncommitted || comparison.source_rev.is_some() {
            comparison.source
        } else {
            resolve(&comparison.source)
        },
        target: if comparison.target_rev.is_some() {
            comparison.target
        } else {
            resolve(&comparison.target)
        },
        ..comparison
    };
    // Reload re-resolves the comparison so HEAD can move and gain commits;
    // the typed target must not move with it. `HEAD~1` names a commit
    // relative to startup, and a branch target would swallow on reload the
    // very commits the reload should add, so the base is pinned to its
    // startup commit while the typed spelling stays the display name.
    let comparison = if comparison.target_rev.is_none() {
        match backend.resolve(&comparison) {
            Ok(resolved) => Comparison {
                target_rev: Some(resolved.target_head.to_string()),
                ..comparison
            },
            // Same stance as a mistyped revision: fail here, not inside an
            // empty window.
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
    } else {
        comparison
    };
    // The project keeps its name in every worktree; the worktree shows
    // beside it in the title bar.
    let (name, worktree) = backend
        .display_names()
        .unwrap_or_else(|_| ("repository".to_string(), None));

    gpui_kit::application()
        // The full Lucide catalogue (about 1 MiB). The default `Assets` embeds only
        // the Kit's own 101 icons; swap/collapse-all are outside that set.
        .with_assets(gpui_kit::assets::AllAssets)
        .run(move |cx| {
            // Inter and JetBrains Mono (both OFL) ship in the binary so the
            // themes can name them on every platform. Fonts must be registered
            // before the first window measures text.
            cx.text_system()
                .add_fonts(vec![
                    include_bytes!("../assets/fonts/Inter-Regular.ttf")
                        .as_slice()
                        .into(),
                    include_bytes!("../assets/fonts/Inter-Medium.ttf")
                        .as_slice()
                        .into(),
                    include_bytes!("../assets/fonts/Inter-SemiBold.ttf")
                        .as_slice()
                        .into(),
                    include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf")
                        .as_slice()
                        .into(),
                    include_bytes!("../assets/fonts/JetBrainsMono-Medium.ttf")
                        .as_slice()
                        .into(),
                    include_bytes!("../assets/fonts/JetBrainsMono-Italic.ttf")
                        .as_slice()
                        .into(),
                ])
                .expect("the embedded fonts must load");
            gpui_kit::init(cx);
            app::init(cx);
            // Only the X11 backend reads `icon` (it becomes _NET_WM_ICON);
            // Wayland takes the icon from the .desktop entry matched by
            // `app_id`. A broken embed must not stop a review, so decode
            // failure just logs.
            let icon = image::load_from_memory(include_bytes!("../../../assets/icon-256.png"))
                .map(|decoded| Arc::new(decoded.into_rgba8()))
                .inspect_err(|error| tracing::warn!("window icon not decodable: {error}"))
                .ok();
            // GPUI never touches the macOS app icon and the dev binary has
            // no bundle to carry an .icns, so the Dock gets the mark at
            // runtime - X11 and Wayland are covered above, which closes the
            // cross-platform story. The 1024 px master because the Dock and
            // Cmd-Tab render large; a failure here costs only the icon.
            #[cfg(target_os = "macos")]
            {
                use objc2::{AnyThread as _, MainThreadMarker};
                use objc2_app_kit::{NSApplication, NSImage};
                use objc2_foundation::NSData;

                let mark = NSImage::initWithData(
                    NSImage::alloc(),
                    &NSData::with_bytes(include_bytes!("../../../assets/icon-1024.png")),
                );
                match (mark, MainThreadMarker::new()) {
                    (Some(mark), Some(mtm)) => unsafe {
                        NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&mark));
                    },
                    (mark, _) => tracing::warn!(
                        decoded = mark.is_some(),
                        "dock icon skipped: image or main thread unavailable"
                    ),
                }
            }
            // The remembered place and size, in global coordinates, so the
            // window also returns to the screen it was left on; first runs
            // center a default. A remembered screen that is gone leaves
            // the window to the OS's own clamping.
            let settings = settings::load();
            let window_bounds = match settings.window {
                Some(window) => WindowBounds::Windowed(Bounds::new(
                    point(px(window.x), px(window.y)),
                    size(px(window.width), px(window.height)),
                )),
                None => WindowBounds::centered(size(px(1440.), px(900.)), cx),
            };
            let options = WindowOptions {
                window_bounds: Some(window_bounds),
                window_min_size: Some(size(px(900.), px(560.))),
                app_id: Some("diff-review".into()),
                icon,
                ..TitleBar::window_options()
            };
            // A single-window tool ends with its window; macOS would
            // otherwise keep the process alive with no way to get a
            // window back.
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            // Like every startup failure: say it and exit, never panic.
            if let Err(error) = gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| {
                    app::ReviewApp::new(
                        backend,
                        forge,
                        app::Subject {
                            repository: name.into(),
                            worktree: worktree.map(Into::into),
                            comparison,
                            pull: connected,
                        },
                        settings,
                        window,
                        cx,
                    )
                })
            }) {
                eprintln!("could not open the review window: {error}");
                std::process::exit(1);
            }
            cx.activate(true);
        });
}
