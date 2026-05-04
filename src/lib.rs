mod flow;
pub mod geoapify;
pub mod gps;
pub mod media;
pub mod naming;

use std::path::PathBuf;

/// How much interactivity and filename detail to use for a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenameFlowMode {
    /// Confirm or edit each Geoapify place; optional description prompts where applicable.
    Full,
    /// `YYMMDD-Country-City` only: no description segment; Geoapify and stem places applied without per-file confirmation.
    PlaceDateOnly,
    /// No prompts after startup configuration: requires `--folder`, session + fallback via CLI/env (or minimal TTY prompts); GPS files need `GEOAPIFY_API_KEY`. Files without GPS rename to `YYMMDD-fallback_country-fallback_city`.
    Autonomous,
}

/// Options for [`run`].
#[derive(Debug, Default)]
pub struct RunOptions {
    /// `None` opens the folder dialog (or terminal path prompt).
    pub folder: Option<PathBuf>,
    /// Set to `true` when `folder` came from `--folder` (required for [`RenameFlowMode::Autonomous`]).
    pub folder_from_cli: bool,
    /// `None` uses `IMG_REVERSE_GEO_FLOW` or an interactive prompt when stdin is a TTY; otherwise defaults to [`RenameFlowMode::Full`]. Autonomous is only selected via CLI or env, not the interactive menu.
    pub flow_mode: Option<RenameFlowMode>,
    /// Session year-month when [`RenameFlowMode::Autonomous`] (e.g. `2026-05`); overrides `IMG_REVERSE_GEO_SESSION`.
    pub session_year_month: Option<String>,
    pub fallback_country: Option<String>,
    pub fallback_city: Option<String>,
}

/// Run the rename tool. Prefer building a [`RunOptions`] value; [`RunOptions::default`] is only useful for tests.
pub fn run(opts: RunOptions) -> Result<(), String> {
    flow::run(opts)
}
