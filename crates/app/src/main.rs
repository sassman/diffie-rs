mod app;

use std::path::PathBuf;
use std::sync::Arc;

use diffie_core::{CompareMode, Comparison, GitCli};
use gpui_kit::{AppContext as _, WindowBounds, WindowOptions, component::TitleBar, px, size};

const USAGE: &str = "\
Usage: diffie [-C <path>] [<revision> [<revision>]] [--every-difference]
       diffie [-C <path>] [--source REV] [--target REV] [--every-difference]

Reads like git diff. With no revision, reviews the uncommitted changes going
into the checked-out branch. With one revision, reviews everything the working
tree adds going into it. With two revisions (or A..B), reviews the second
going into the first. -C runs as if started in <path> (default: the current
directory). Requires git on PATH.";

fn exit_usage(message: &str) -> ! {
    eprintln!("{message}\n\n{USAGE}");
    std::process::exit(2);
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
    let mut source: Option<String> = None;
    let mut target: Option<String> = None;
    let mut revisions: Vec<String> = Vec::new();
    let mut mode = CompareMode::BranchAdds;
    let mut arguments = std::env::args_os().skip(1);
    while let Some(argument) = arguments.next() {
        let text = argument.to_string_lossy().into_owned();
        match text.as_str() {
            "--help" | "-h" => {
                println!("{USAGE}");
                return;
            }
            "--every-difference" => mode = CompareMode::EveryDifference,
            "-C" => {
                let Some(value) = arguments.next() else {
                    exit_usage("-C needs a path.");
                };
                repository = Some(PathBuf::from(value));
            }
            "--source" | "--target" => {
                let Some(value) = arguments.next().and_then(|value| value.into_string().ok())
                else {
                    exit_usage(&format!("{text} needs a value."));
                };
                if text == "--source" {
                    source = Some(value);
                } else {
                    target = Some(value);
                }
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
                // git's range spellings name both ends in one token. The
                // merge-base rule makes two and three dots the same review.
                if let Some((into, from)) = text.split_once("...").or_else(|| text.split_once(".."))
                {
                    let or_head = |rev: &str| {
                        if rev.is_empty() {
                            "HEAD".to_string()
                        } else {
                            rev.to_string()
                        }
                    };
                    revisions.extend([or_head(into), or_head(from)]);
                } else {
                    revisions.push(text);
                }
            }
            _ => exit_usage(&format!("unknown argument `{text}`.")),
        }
    }
    if !revisions.is_empty() && (source.is_some() || target.is_some()) {
        exit_usage("give either revisions or --source/--target, not both.");
    }
    if revisions.len() > 2 {
        exit_usage("at most two revisions.");
    }

    let path = repository.unwrap_or_else(|| PathBuf::from("."));
    let backend = match GitCli::open(&path) {
        Ok(backend) => Arc::new(backend),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    let branch = backend.current_branch().ok().flatten();
    let working_tree = backend.has_working_tree().unwrap_or(false);
    let comparison = match revisions.as_slice() {
        // `diffie A B` reads like `git diff A B`: B's changes going into A.
        [into, from] => Comparison {
            source: from.clone(),
            target: into.clone(),
            mode,
            uncommitted: false,
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
        },
        [into] => exit_usage(&format!(
            "no working tree to diff against `{into}`; give two revisions."
        )),
        [] => {
            // Opened plainly, the review is the uncommitted work going into
            // the checked-out branch. A bare repository or a detached HEAD
            // has no such review and falls back to branch against main.
            let plain = source.is_none() && target.is_none();
            if let Some(into) = branch.clone().filter(|_| plain && working_tree) {
                Comparison {
                    source: "uncommitted".to_string(),
                    target: into,
                    mode,
                    uncommitted: true,
                }
            } else {
                let mut source = source.unwrap_or_else(|| "HEAD".to_string());
                // Name the review after the branch, not after the literal `HEAD`.
                if source == "HEAD"
                    && let Some(branch) = branch
                {
                    source = branch;
                }
                Comparison {
                    source,
                    target: target.unwrap_or_else(|| "main".to_string()),
                    mode,
                    uncommitted: false,
                }
            }
        }
        _ => unreachable!("the length was checked above"),
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
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1440.), px(900.)), cx)),
                window_min_size: Some(size(px(900.), px(560.))),
                app_id: Some("diff-review".into()),
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
            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| {
                    app::ReviewApp::new(
                        backend,
                        name.into(),
                        worktree.map(Into::into),
                        comparison,
                        window,
                        cx,
                    )
                })
            })
            .expect("failed to open the review window");
            cx.activate(true);
        });
}
