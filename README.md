# diff-review (working name)

A native desktop app for reviewing git changes across several commits or a
whole branch, with comments on lines and selections. Rust and GPUI Kit, for
macOS, Windows and Linux.

This is an early scaffold. Start with:

- `docs/HANDOFF.md`: what exists, what was verified, what is next.
- `docs/design-decisions.md`: how the app is meant to behave.
- `docs/mockup/main-screen.html`: the visual reference (open in a browser).
- `AGENTS.md`: rules for agents working here.

## Layout

```text
crates/core   review-core: repository access, diffs, the review file. No UI.
crates/app    diffie: the GPUI Kit window and binary.
docs/         design decisions, handoff, mockup
```

## Build and run

Requires a recent stable Rust (edition 2024) and `git` on the PATH.

```sh
cargo test -p review-core
cargo run -p diffie -- --target main /path/to/repository
```

The first build compiles GPUI and takes several minutes.

`review` reviews the changes in `--source` (default: the checked-out branch)
going into `--target` (default: `main`).

### Linux packages

Tested on Ubuntu 24.04:

```sh
sudo apt install -y gcc g++ clang libfontconfig-dev libwayland-dev \
  libxkbcommon-x11-dev libx11-xcb-dev libssl-dev libzstd-dev libasound2-dev \
  libvulkan1
```

A Vulkan driver is needed to open the window. Without a GPU, Mesa's software
driver works (`mesa-vulkan-drivers`).

## Where reviews are stored

`<repository>/.git/review/<branch>/<your name>.toml`. Nothing else in the
repository is written. To see a colleague's review, put their file in the same
folder.
