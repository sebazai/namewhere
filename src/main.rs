use clap::Parser;
use std::path::PathBuf;

/// Rename images/videos using GPS reverse geocoding or manual place names.
#[derive(Parser, Debug)]
#[command(
    name = "img-reverse-geolocation",
    version,
    about = "Rename photos/videos using GPS reverse geocoding or manual place names.",
    after_long_help = "\
Environment variables:\n\
  GEOAPIFY_API_KEY                      Reverse geocoding (optional; prompted if missing when needed)\n\
  IMG_REVERSE_GEO_REFRESH_GEAPIFY_ONLY  Set to 1/true/yes/on to refresh only fully named YYMMDD-*-*-* files with GPS\n\
\n\
Renames are appended to img-reverse-geolocation-renames.csv in the chosen folder.\n\
"
)]
struct Cli {
    /// Folder to process (otherwise pick with the dialog or type a path when the dialog is unavailable)
    #[arg(long, value_name = "DIR")]
    folder: Option<PathBuf>,
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = img_reverse_geolocation::run(cli.folder) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
