<p align="center">
  <img src="assets/logo.svg" width="160" alt="diffie logo: an eagle's eye">
</p>

<h1 align="center">diffie</h1>

<p align="center">
  Review git changes natively, comment on lines, hand the review to your agent.
</p>

<p align="center">
  <a href="https://github.com/sassman/diffie-rs/actions/workflows/build.yml"><img src="https://github.com/sassman/diffie-rs/actions/workflows/build.yml/badge.svg" alt="Build status"></a>
  <a href="https://github.com/sassman/diffie-rs/releases/latest"><img src="https://img.shields.io/github/v/release/sassman/diffie-rs?label=release" alt="Latest release"></a>
  <img src="https://img.shields.io/badge/platforms-macOS%20%7C%20Linux%20%7C%20Windows-informational" alt="Platforms: macOS, Linux, Windows">
  <img src="https://img.shields.io/badge/built%20with-Rust%20%2B%20GPUI%20Kit-orange" alt="Built with Rust and GPUI Kit">
  <img src="https://img.shields.io/badge/status-early%20alpha-yellow" alt="Status: early alpha">
</p>

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
crates/core     diffie-core: repository access, diffs, the review file. No UI.
crates/symbols  diffie-symbols: where a name is defined, from syntax. No UI.
crates/app      diffie: the GPUI Kit window and binary.
docs/         design decisions, handoff, mockup
```

## Install

With Homebrew, on macOS and Linux:

```sh
brew install sassman/tap/diffie
```

Or build it from source with a recent stable Rust; the first build compiles
GPUI and takes several minutes (Linux needs the packages listed below):

```sh
cargo install --locked --git https://github.com/sassman/diffie-rs diffie
```

Prebuilt binaries for macOS, Linux and Windows are attached to every
[release](https://github.com/sassman/diffie-rs/releases).

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

`skills/diffie-review/` lets a coding agent (Claude Code and compatible) run
the review loop with you: it opens the right comparison (or `--pr <number>`
when the branch has a pull request), reads your comments back through
`diffie --prompt` and answers each one through `diffie --reply`.

The skill calls `diffie`, so put the binary on the PATH first, then add the
skill:

```sh
cargo install --path crates/app
npx skills add sassman/diffie-rs
```

Working from a checkout, link it instead so `git pull` updates it:

```sh
ln -s "$PWD/skills/diffie-review" ~/.claude/skills/diffie-review
```

Then ask the agent to "review my changes", and "address the review" once
you are done commenting.
