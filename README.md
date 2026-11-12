# diffie 

A native desktop app for reviewing git changes across several commits or a
whole branch, with comments on lines and selections. Rust and GPUI Kit, for
macOS, Windows and Linux.

This is an early alpha. Start with:

- `docs/HANDOFF.md`: what exists, what was verified, what is next.
- `docs/design-decisions.md`: how the app is meant to behave.
- `docs/mockup/main-screen.html`: the visual reference (open in a browser).
- `AGENTS.md`: rules for agents working here.

## Layout

```text
crates/core   diffie-core: repository access, diffs, the review file. No UI.
crates/app    diffie: the GPUI Kit window and binary.
docs/         design decisions, handoff, mockup
```

## Build and run

Requires a recent stable Rust (edition 2024) and `git` on the PATH.

```sh
cargo test -p diffie-core
cargo run -p diffie -- -C /path/to/repository main
```

The first build compiles GPUI and takes several minutes.

`diffie` reads like `git diff`: no revision reviews the uncommitted changes
going into the checked-out branch, one revision reviews everything the
working tree adds going into it, two revisions (or `A..B`) review the second
going into the first. `-C <path>` picks the project (default: the current
directory).

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

## Agent skill

`skills/diffie-review/` teaches a coding agent (Claude Code and compatible)
to open the right comparison for a review request and to read the finished
review back through `diffie --prompt`, which prints the open comments
exactly like the app's "Copy all as prompt" button. Install from this
repository:

```sh
npx skills add sassman/diffie-rs
# or by hand:
cp -r skills/diffie-review ~/.claude/skills/
```
