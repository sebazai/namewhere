mod flow;
pub mod geoapify;
pub mod gps;
pub mod media;
pub mod naming;

/// `folder`: `None` to pick a folder interactively; `Some(path)` to process that directory.
pub fn run(folder: Option<std::path::PathBuf>) -> Result<(), String> {
    flow::run(folder)
}
