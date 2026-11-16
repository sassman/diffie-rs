//! UI-free core of the diff review app.
//!
//! - [`backend::ReviewBackend`] is the contract the UI programs against.
//! - [`git_cli::GitCli`] is the cheap implementation that shells out to git.
//! - [`review`] holds the review file: comments, reactions, reviewed ticks.
//! - [`forge::Forge`] is the code host a pull request lives on;
//!   [`github::GitHubCli`] implements it through the GitHub CLI.
//!
//! This crate must never depend on GPUI or any UI code.

pub mod backend;
pub mod forge;
pub mod git_cli;
pub mod github;
pub mod model;
pub mod moved;
mod patch;
pub mod pr;
pub mod review;
pub mod search;
pub mod time;

pub use backend::{BackendError, ReviewBackend};
pub use git_cli::GitCli;
pub use model::*;
