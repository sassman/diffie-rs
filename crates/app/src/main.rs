mod app;

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::{AppContext as _, WindowBounds, WindowOptions, component::TitleBar, px, size};
use review_core::{CompareMode, Comparison, GitCli};

const USAGE: &str = "\
Usage: review [--source REV] [--target REV] [--every-difference] [repository]

Reviews the changes in --source (default HEAD) going into --target (default main).
The repository defaults to the current directory. Requires git on PATH.";

fn main() {
    let mut repository = None;
    let mut source = String::from("HEAD");
    let mut target = String::from("main");
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
            "--source" | "--target" => {
                let Some(value) = arguments.next().and_then(|value| value.into_string().ok())
                else {
                    eprintln!("{text} needs a value.\n\n{USAGE}");
                    std::process::exit(2);
                };
                if text == "--source" {
                    source = value;
                } else {
                    target = value;
                }
            }
            _ if !text.starts_with('-') && repository.is_none() => {
                repository = Some(PathBuf::from(argument));
            }
            _ => {
                eprintln!("{USAGE}");
                std::process::exit(2);
            }
        }
    }

    let path = repository.unwrap_or_else(|| PathBuf::from("."));
    let backend = match GitCli::open(&path) {
        Ok(backend) => Arc::new(backend),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    // Name the review after the branch, not after the literal `HEAD`.
    if source == "HEAD"
        && let Ok(Some(branch)) = backend.current_branch()
    {
        source = branch;
    }
    // The project keeps its name in every worktree; the worktree shows
    // beside it in the title bar.
    let (name, worktree) = backend
        .display_names()
        .unwrap_or_else(|_| ("repository".to_string(), None));
    let comparison = Comparison {
        source,
        target,
        mode,
    };

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
