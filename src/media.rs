use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

const MEDIA_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "heic", "tif", "tiff", "mp4", "mov", "m4v", "avi", "mkv",
];

/// Lists media files in `dir` (non-recursive), sorted by file name.
pub fn list_media_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let allowed: HashSet<&str> = MEDIA_EXTENSIONS.iter().copied().collect();
    let mut out = Vec::new();

    for ent in fs::read_dir(dir).map_err(|e| format!("read folder: {e}"))? {
        let ent = ent.map_err(|e| format!("read entry: {e}"))?;
        let path = ent.path();
        if !path.is_file() {
            continue;
        }
        let ext = path
            .extension()
            .and_then(OsStr::to_str)
            .map(|s| s.to_ascii_lowercase());
        let Some(ext) = ext else { continue };
        if allowed.contains(ext.as_str()) {
            out.push(path);
        }
    }

    out.sort_by(|a, b| {
        a.file_name()
            .unwrap_or_default()
            .cmp(b.file_name().unwrap_or_default())
    });
    Ok(out)
}
