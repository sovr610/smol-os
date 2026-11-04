// SPDX-License-Identifier: GPL-3.0-or-later
//! # termos-harness
//!
//! The coding harness (M11c, D-096, D-099, D-100): one window that turns a
//! goal into a plan of typed tasks, routes each task to the backend that
//! suits it (Claude Code or Codex CLI headless under the user's
//! subscriptions, or an API model through TermOS's own agent) and runs
//! them: tasks that change files one after another, read-only ones
//! alongside. Every hand-off is shown and audited.
//!
//! - [`plan`]: what the planner is asked, and reading its answer;
//! - [`route`]: backends, tiers and the order they are tried in;
//! - [`events`]: what headless Claude Code and Codex CLI print;
//! - [`backends`]: running one task on one backend;
//! - [`runner`]: the [`runner::Harness`]: planning, approval and the run;
//! - [`app`]: its window.

pub mod app;
pub mod backends;
pub mod events;
pub mod plan;
pub mod route;
pub mod runner;
