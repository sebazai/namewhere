use clap::{Parser, ValueEnum};
use std::path::PathBuf;

use img_reverse_geolocation::{RenameFlowMode, RunOptions};

/// Rename images/videos using GPS reverse geocoding or manual place names.
#[derive(Parser, Debug)]
#[command(
    name = "img-reverse-geolocation",
    version,
    about = "Rename photos/videos using GPS reverse geocoding or manual place names.",
    after_long_help = "\
Environment variables:\n\
  GEOAPIFY_API_KEY                      Reverse geocoding (required in autonomous mode when any file has GPS)\n\
  IMG_REVERSE_GEO_REFRESH_GEAPIFY_ONLY  Set to 1/true/yes/on to refresh only fully named YYMMDD-*-*-* files with GPS\n\
  IMG_REVERSE_GEO_FLOW                  full | place-date | autonomous (CLI --mode wins)\n\
  IMG_REVERSE_GEO_SESSION               Session year-month for autonomous (e.g. 2026-05) if --session-year-month omitted\n\
  IMG_REVERSE_GEO_FALLBACK_COUNTRY      Default country for files without GPS in autonomous mode\n\
  IMG_REVERSE_GEO_FALLBACK_CITY         Default city for files without GPS in autonomous mode\n\
\n\
Renames are appended to img-reverse-geolocation-renames.csv in the chosen folder.\n\
"
)]
struct Cli {
    /// Folder to process (otherwise pick with the dialog or type a path when the dialog is unavailable)
    #[arg(long, value_name = "DIR")]
    folder: Option<PathBuf>,

    /// full: confirm each Geoapify place and optional descriptions. place-date: YYMMDD-Country-City only; Geoapify without per-file review. autonomous: no further prompts — use with --folder; set session + fallback via flags or env; no-GPS files use fallback place (YYMMDD from EXIF or session YYMM00).
    #[arg(long, value_enum, value_name = "MODE")]
    mode: Option<FlowModeCli>,

    /// Session year-month for autonomous mode (e.g. 2026-5). Embedded capture date still wins when present. Falls back to IMG_REVERSE_GEO_SESSION.
    #[arg(long, value_name = "YYYY-MM")]
    session_year_month: Option<String>,

    /// Fallback country for autonomous mode when a file has no GPS (IMG_REVERSE_GEO_FALLBACK_COUNTRY).
    #[arg(long, value_name = "NAME")]
    fallback_country: Option<String>,

    /// Fallback city for autonomous mode when a file has no GPS (IMG_REVERSE_GEO_FALLBACK_CITY).
    #[arg(long, value_name = "NAME")]
    fallback_city: Option<String>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum FlowModeCli {
    Full,
    #[value(name = "place-date")]
    PlaceDate,
    Autonomous,
}

impl From<FlowModeCli> for RenameFlowMode {
    fn from(value: FlowModeCli) -> Self {
        match value {
            FlowModeCli::Full => RenameFlowMode::Full,
            FlowModeCli::PlaceDate => RenameFlowMode::PlaceDateOnly,
            FlowModeCli::Autonomous => RenameFlowMode::Autonomous,
        }
    }
}

fn main() {
    let cli = Cli::parse();
    let folder_from_cli = cli.folder.is_some();
    if let Err(e) = img_reverse_geolocation::run(RunOptions {
        folder: cli.folder,
        folder_from_cli,
        flow_mode: cli.mode.map(RenameFlowMode::from),
        session_year_month: cli.session_year_month,
        fallback_country: cli.fallback_country,
        fallback_city: cli.fallback_city,
    }) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
