use std::collections::{HashMap, VecDeque};
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{Datelike, Local, SecondsFormat};
use dialoguer::{theme::ColorfulTheme, Confirm, Input, Password, Select};

use crate::geoapify;
use crate::gps;
use crate::media;
use crate::naming;

fn load_dotenv() {
    let _ = dotenvy::dotenv();
}

/// When set to `1` / `true` / `yes` / `on`, only files that already match fully named
/// `YYMMDD-*-*-*-*` layout (and have GPS) are sent to Geoapify and renamed; the rest of the folder
/// is left untouched.
const ENV_REFRESH_GEAPIFY_ONLY: &str = "IMG_REVERSE_GEO_REFRESH_GEAPIFY_ONLY";

fn env_truthy(key: &str) -> bool {
    match env::var(key) {
        Ok(s) => {
            let t = s.trim().to_ascii_lowercase();
            matches!(t.as_str(), "1" | "true" | "yes" | "on")
        }
        Err(_) => false,
    }
}

const RENAME_LOG_FILENAME: &str = "img-reverse-geolocation-renames.csv";

struct RenameLog {
    path: PathBuf,
    announced: bool,
}

impl RenameLog {
    fn new(folder: &Path) -> Self {
        Self {
            path: folder.join(RENAME_LOG_FILENAME),
            announced: false,
        }
    }

    fn record(&mut self, from: &Path, to: &Path) -> Result<(), String> {
        let ts = Local::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| e.to_string())?;
        writeln!(
            f,
            "{},{},{}",
            csv_escape(&from.to_string_lossy()),
            csv_escape(&to.to_string_lossy()),
            csv_escape(&ts)
        )
        .map_err(|e| e.to_string())?;
        if !self.announced {
            println!("Wrote rename log to {}", self.path.display());
            self.announced = true;
        }
        Ok(())
    }
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        let inner = s.replace('"', "\"\"");
        format!("\"{inner}\"")
    } else {
        s.to_string()
    }
}

fn pick_folder() -> Result<PathBuf, String> {
    // xdg-portal / ashpd talks to zbus, which requires a Tokio 1.x runtime (rfd's sync API uses pollster).
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;

    let picked = runtime.block_on(async { rfd::AsyncFileDialog::new().pick_folder().await });

    if let Some(handle) = picked {
        return Ok(handle.path().to_path_buf());
    }

    let theme = ColorfulTheme::default();
    let raw: String = Input::with_theme(&theme)
        .with_prompt("Folder path (dialog unavailable)")
        .interact_text()
        .map_err(|e| e.to_string())?;

    let path = PathBuf::from(raw.trim());
    if !path.is_dir() {
        return Err(format!("Not a directory: {}", path.display()));
    }
    Ok(path)
}

fn resolve_folder(cli_folder: Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(p) = cli_folder {
        if !p.is_dir() {
            return Err(format!("Not a directory: {}", p.display()));
        }
        let files = media::list_media_files(&p)?;
        if files.is_empty() {
            return Err("No images or videos found in that folder.".to_string());
        }
        return Ok(p);
    }
    loop {
        let folder = pick_folder()?;
        let files = media::list_media_files(&folder)?;
        if files.is_empty() {
            let theme = ColorfulTheme::default();
            let retry = Confirm::with_theme(&theme)
                .with_prompt("No images or videos in that folder. Pick another folder?")
                .default(true)
                .interact()
                .map_err(|e| e.to_string())?;
            if !retry {
                return Err("No images or videos found in that folder.".to_string());
            }
            continue;
        }
        return Ok(folder);
    }
}

/// Parses `YYYY-MM`, `YY-M`, `26/4`, etc. Returns `(yy_two_digits, mm_two_digits)`.
fn parse_combined_year_month(s: &str) -> Result<(String, String), String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("Empty year-month".to_string());
    }
    let parts: Vec<&str> = s
        .split(&['-', '/', '.'][..])
        .filter(|p| !p.trim().is_empty())
        .map(|p| p.trim())
        .collect();
    if parts.len() != 2 {
        return Err("Enter year and month (e.g. 2026-04 or 26/4)".to_string());
    }
    let y: u32 = parts[0].parse().map_err(|_| "Invalid year".to_string())?;
    let m: u32 = parts[1].parse().map_err(|_| "Invalid month".to_string())?;
    if !(1..=12).contains(&m) {
        return Err("Month must be 1-12".to_string());
    }
    let yy = if y < 100 {
        y
    } else if (2000..=2099).contains(&y) {
        y % 100
    } else {
        return Err("Year must be 2000–2099 or a two-digit YY".to_string());
    };
    Ok((format!("{yy:02}"), format!("{m:02}")))
}

fn parse_session_year_month() -> Result<(String, String), String> {
    let now = Local::now();
    let default_line = format!("{}-{:02}", now.year(), now.month());
    let theme = ColorfulTheme::default();
    let raw: String = Input::with_theme(&theme)
        .with_prompt(
            "Session year-month (used as YYMM00 when a file has no embedded capture date; Enter = today)",
        )
        .default(default_line)
        .interact_text()
        .map_err(|e| e.to_string())?;
    let line = raw.trim();
    if line.is_empty() {
        let yy = format!("{:02}", now.year() % 100);
        let mm = format!("{:02}", now.month());
        return Ok((yy, mm));
    }
    parse_combined_year_month(line)
}

/// After year/month: optionally ignore existing tool-style filenames and run GPS + Geoapify for every
/// file with coordinates (no “already named” or stem-place skips).
fn prompt_force_full_rerun() -> Result<bool, String> {
    let theme = ColorfulTheme::default();
    Confirm::with_theme(&theme)
        .with_prompt(
            "Force full rerun? (Ignore existing names — re-geocode all files with GPS; no already-named skips)",
        )
        .default(false)
        .interact()
        .map_err(|e| e.to_string())
}

/// Country or city: first file must be non-empty; later files may press Enter to reuse `last`.
fn prompt_place_line(prompt: &str, last: Option<&str>) -> Result<String, String> {
    let theme = ColorfulTheme::default();
    loop {
        let raw: String = if let Some(d) = last {
            Input::with_theme(&theme)
                .with_prompt(prompt)
                .default(d.to_string())
                .interact_text()
                .map_err(|e| e.to_string())?
        } else {
            Input::with_theme(&theme)
                .with_prompt(prompt)
                .allow_empty(true)
                .interact_text()
                .map_err(|e| e.to_string())?
        };

        let t = raw.trim();
        if !t.is_empty() {
            return Ok(t.to_string());
        }
        if let Some(d) = last {
            return Ok(d.to_string());
        }
        eprintln!("Please enter a non-empty value.");
    }
}

fn prompt_optional_description(default: Option<&str>) -> Result<Option<String>, String> {
    let theme = ColorfulTheme::default();
    let mut input = Input::with_theme(&theme)
        .with_prompt("Description (optional, Enter to skip)")
        .allow_empty(true);
    if let Some(d) = default {
        let t = d.trim();
        if !t.is_empty() {
            input = input.default(t.to_string());
        }
    }
    let description: String = input.interact_text().map_err(|e| e.to_string())?;
    let t = description.trim();
    Ok(if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    })
}

/// Prompt once when env has no key; empty input skips reverse geocoding for this run.
fn prompt_geoapify_api_key() -> Result<Option<String>, String> {
    let theme = ColorfulTheme::default();
    let raw: String = Password::with_theme(&theme)
        .with_prompt("Geoapify API key (Enter to skip GPS reverse geocoding)")
        .allow_empty_password(true)
        .interact()
        .map_err(|e| e.to_string())?;
    let t = raw.trim();
    Ok(if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    })
}

/// Best-effort: close the front Preview document on macOS. Other platforms / default apps are not
/// controllable after `open::that` (no process handle).
fn try_close_preview_best_effort() {
    #[cfg(target_os = "macos")]
    {
        let script = r#"
tell application "Preview"
    if (count of documents) > 0 then
        close front document
    end if
end tell
"#;
        let _ = std::process::Command::new("osascript")
            .arg("-e")
            .arg(script)
            .status();
    }
}

fn extension_as_str(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_string()
}

/// At most `max_calls` recorded instants within `window` (sliding). Records when a call **starts**.
struct SlidingWindowRateLimiter {
    window: Duration,
    max_calls: usize,
    stamps: VecDeque<Instant>,
}

impl SlidingWindowRateLimiter {
    fn new(window: Duration, max_calls: usize) -> Self {
        Self {
            window,
            max_calls,
            stamps: VecDeque::new(),
        }
    }

    fn acquire(&mut self) {
        loop {
            let now = Instant::now();
            while self
                .stamps
                .front()
                .is_some_and(|t| now.duration_since(*t) >= self.window)
            {
                self.stamps.pop_front();
            }
            if self.stamps.len() < self.max_calls {
                self.stamps.push_back(Instant::now());
                return;
            }
            let oldest = *self.stamps.front().expect("len >= max_calls > 0");
            let wait = self.window.saturating_sub(now.duration_since(oldest));
            thread::sleep(wait);
        }
    }
}

struct FileWork {
    current_path: PathBuf,
    place: Option<(String, String)>,
    /// `YYMMDD-*-*-*` (or legacy `YYMM-*-*-*`) on disk; skip API and renames.
    already_named: bool,
    /// User chose to leave this file’s name unchanged (skip rename).
    user_skip_rename: bool,
    /// Manual place + description done in one pass; skip the later description-only pass.
    manual_flow_complete: bool,
    /// `YYMMDD` from EXIF/ffprobe, or session `YYMM` + `00` when unknown.
    date_prefix: String,
    /// Leading date token from the existing filename (legacy 4-digit or 6-digit).
    stem_date_override: Option<String>,
    /// Place is known from stem (`…-place-place` + numeric tail); do not rename in the geocoded-only pass.
    skip_initial_place_rename: bool,
    /// Filename starts with `YYMM`/`YYMMDD` that disagrees with the session month; no embedded date
    /// — skip all renames so the session fallback cannot override the name.
    skip_session_date_mismatch: bool,
    /// Full-rerun mode: `(country, city, optional description)` parsed from the existing stem for prompt defaults.
    stem_placeholders: Option<(String, String, Option<String>)>,
}

/// How many files would call Geoapify if an API key is available (same filters as the geocode pass).
fn geoapify_candidate_count(
    work: &[FileWork],
    force_full_rerun: bool,
    refresh_geocoding_only: bool,
) -> usize {
    let mut n = 0;
    for w in work {
        if !force_full_rerun && w.skip_session_date_mismatch {
            continue;
        }
        if refresh_geocoding_only {
            if !w.already_named {
                continue;
            }
        } else if !force_full_rerun && (w.already_named || w.place.is_some()) {
            continue;
        }
        if gps::coordinates(&w.current_path).is_none() {
            continue;
        }
        n += 1;
    }
    n
}

fn needs_geocode_place_validation(w: &FileWork, refresh_geocoding_only: bool) -> bool {
    if w.skip_session_date_mismatch || w.place.is_none() {
        return false;
    }
    if refresh_geocoding_only {
        w.already_named
    } else {
        !w.already_named && !w.skip_initial_place_rename
    }
}

fn stem_date_for_final_rename(w: &FileWork) -> String {
    if gps::capture_yymmdd(&w.current_path).is_some() {
        w.date_prefix.clone()
    } else {
        w.stem_date_override
            .as_deref()
            .map(naming::normalize_date_prefix_for_stem)
            .unwrap_or_else(|| w.date_prefix.clone())
    }
}

fn finalize_non_refresh_geocoded_file(
    w: &mut FileWork,
    folder: &Path,
    log: &mut RenameLog,
) -> Result<(), String> {
    let desc_opt = prompt_optional_description(
        w.stem_placeholders
            .as_ref()
            .and_then(|(_, _, d)| d.as_deref()),
    )?;
    let stem_date = stem_date_for_final_rename(w);
    let (country, city) = w
        .place
        .as_ref()
        .ok_or_else(|| "internal: missing country/city".to_string())?;
    let stem = naming::build_stem(&stem_date, country, city, desc_opt.as_deref());
    try_rename_with_stem(folder, w, &stem, log)
}

fn try_refresh_rename_one(
    w: &mut FileWork,
    folder: &Path,
    log: &mut RenameLog,
) -> Result<(), String> {
    let name = w
        .current_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let Some(stem_str) = w.current_path.file_stem().and_then(|s| s.to_str()) else {
        return Ok(());
    };
    let Some((file_date, desc_seg)) = naming::parse_fully_named_stem_for_refresh(stem_str) else {
        eprintln!("Refresh: cannot parse stem for {name}; skipping rename.");
        return Ok(());
    };
    let Some((country, city)) = w.place.clone() else {
        return Ok(());
    };
    let stem_date = if gps::capture_yymmdd(&w.current_path).is_some() {
        w.date_prefix.clone()
    } else {
        naming::normalize_date_prefix_for_stem(&file_date)
    };
    let desc_opt = (!desc_seg.is_empty()).then_some(desc_seg.as_str());
    let new_stem = naming::build_stem(&stem_date, &country, &city, desc_opt);
    try_rename_with_stem(folder, w, &new_stem, log)
}

fn refresh_place_review_and_rename(
    work: &mut [FileWork],
    folder: &Path,
    log: &mut RenameLog,
) -> Result<(), String> {
    let theme = ColorfulTheme::default();
    let to_review: Vec<usize> = work
        .iter()
        .enumerate()
        .filter(|(_, w)| needs_geocode_place_validation(w, true))
        .map(|(i, _)| i)
        .collect();

    if to_review.is_empty() {
        return Ok(());
    }

    println!(
        "\n{} file(s) (refresh mode) have Geoapify place names. Confirm or edit each; then rename with updated country/city.",
        to_review.len()
    );
    let do_review = Confirm::with_theme(&theme)
        .with_prompt("Review country/city for those files now?")
        .default(true)
        .interact()
        .map_err(|e| e.to_string())?;

    if !do_review {
        for &idx in &to_review {
            let w = &mut work[idx];
            if w.user_skip_rename {
                continue;
            }
            try_refresh_rename_one(w, folder, log)?;
        }
        return Ok(());
    }

    let total_review = to_review.len();
    let mut bulk_for_same_geoapify: HashMap<(String, String), (String, String)> = HashMap::new();
    let mut auto_yes_same_geo: Option<(String, String)> = None;

    for (i, &idx) in to_review.iter().enumerate() {
        let w = &mut work[idx];
        if w.user_skip_rename {
            continue;
        }
        let name = w
            .current_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();

        if let Some(geo_key) = w.place.clone() {
            if let Some((to_c, to_ci)) = bulk_for_same_geoapify.get(&geo_key) {
                println!("\n--- {name} ---");
                if let Some((lat, lon)) = gps::coordinates(&w.current_path) {
                    println!("GPS: {lat:.6}, {lon:.6}");
                }
                println!(
                    "Geoapify: {} / {}  →  using your saved place: {} / {}",
                    geo_key.0, geo_key.1, to_c, to_ci
                );
                if let Err(e) = open::that(&w.current_path) {
                    eprintln!("Could not open file (continuing): {e}");
                }
                w.place = Some((to_c.clone(), to_ci.clone()));
                try_refresh_rename_one(w, folder, log)?;
                try_close_preview_best_effort();
                continue;
            }
            if auto_yes_same_geo.as_ref() == Some(&geo_key) {
                println!("\n--- {name} ---");
                if let Some((lat, lon)) = gps::coordinates(&w.current_path) {
                    println!("GPS: {lat:.6}, {lon:.6}");
                }
                println!(
                    "Geoapify: {} / {}  →  accepting (yes to all for this Geoapify place)",
                    geo_key.0, geo_key.1
                );
                if let Err(e) = open::that(&w.current_path) {
                    eprintln!("Could not open file (continuing): {e}");
                }
                try_refresh_rename_one(w, folder, log)?;
                try_close_preview_best_effort();
                continue;
            }
        }

        let Some((country, city)) = w.place.clone() else {
            continue;
        };

        println!("\n--- {name} ---");
        if let Some((lat, lon)) = gps::coordinates(&w.current_path) {
            println!("GPS: {lat:.6}, {lon:.6}");
        }
        println!("Geoapify: {country} / {city}");

        if let Err(e) = open::that(&w.current_path) {
            eprintln!("Could not open file (continuing): {e}");
        }

        let has_rest = i + 1 < total_review;
        let sel = if has_rest {
            let items = vec![
                "Yes — this file only".to_string(),
                "No — edit country / city".to_string(),
                format!(
                    "Yes — ALL remaining files with Geoapify \"{} / {}\"",
                    country, city
                ),
                "Skip — leave filename unchanged".to_string(),
            ];
            Select::with_theme(&theme)
                .with_prompt("Use this country and city in the filename? (↑/↓, Enter)")
                .items(&items)
                .default(0)
                .interact()
                .map_err(|e| e.to_string())?
        } else {
            let items = vec![
                "Yes — use Geoapify place".to_string(),
                "No — edit country / city".to_string(),
                "Skip — leave filename unchanged".to_string(),
            ];
            Select::with_theme(&theme)
                .with_prompt("Use this country and city in the filename? (↑/↓, Enter)")
                .items(&items)
                .default(0)
                .interact()
                .map_err(|e| e.to_string())?
        };

        if has_rest && sel == 3 {
            w.user_skip_rename = true;
            try_close_preview_best_effort();
            continue;
        }
        if !has_rest && sel == 2 {
            w.user_skip_rename = true;
            try_close_preview_best_effort();
            continue;
        }

        if sel == 1 {
            let (def_c, def_ci) = match &w.stem_placeholders {
                Some((sc, sci, _)) => (sc.as_str(), sci.as_str()),
                None => (country.as_str(), city.as_str()),
            };
            let c = prompt_place_line("Country", Some(def_c))?;
            let ci = prompt_place_line("City", Some(def_ci))?;
            let from_geo = (country.clone(), city.clone());
            w.place = Some((c.clone(), ci.clone()));
            if has_rest {
                let for_rest = Confirm::with_theme(&theme)
                    .with_prompt(format!(
                        "Use this country & city for all remaining files whose Geoapify place is still \"{} / {}\"?",
                        from_geo.0, from_geo.1
                    ))
                    .default(true)
                    .interact()
                    .map_err(|e| e.to_string())?;
                if for_rest {
                    bulk_for_same_geoapify.insert(from_geo, (c, ci));
                }
            }
            try_refresh_rename_one(w, folder, log)?;
        } else if has_rest && sel == 2 {
            auto_yes_same_geo = Some((country.clone(), city.clone()));
            try_refresh_rename_one(w, folder, log)?;
            try_close_preview_best_effort();
            continue;
        } else {
            try_refresh_rename_one(w, folder, log)?;
        }

        try_close_preview_best_effort();
    }

    Ok(())
}

fn geocoded_interactive_place_desc_rename(
    work: &mut [FileWork],
    folder: &Path,
    log: &mut RenameLog,
) -> Result<(), String> {
    let theme = ColorfulTheme::default();
    let to_review: Vec<usize> = work
        .iter()
        .enumerate()
        .filter(|(_, w)| needs_geocode_place_validation(w, false))
        .map(|(i, _)| i)
        .collect();

    if to_review.is_empty() {
        return Ok(());
    }

    println!(
        "\n{} file(s) have place names from Geoapify (GPS). Confirm or edit each; then optional description; one rename per file.",
        to_review.len()
    );
    let do_review = Confirm::with_theme(&theme)
        .with_prompt("Review country/city for those files now?")
        .default(true)
        .interact()
        .map_err(|e| e.to_string())?;

    if !do_review {
        for &idx in &to_review {
            let w = &mut work[idx];
            if w.user_skip_rename {
                continue;
            }
            finalize_non_refresh_geocoded_file(w, folder, log)?;
        }
        return Ok(());
    }

    let total_review = to_review.len();
    let mut bulk_for_same_geoapify: HashMap<(String, String), (String, String)> = HashMap::new();
    let mut auto_yes_same_geo: Option<(String, String)> = None;

    for (i, &idx) in to_review.iter().enumerate() {
        let w = &mut work[idx];
        if w.user_skip_rename {
            continue;
        }
        let name = w
            .current_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();

        if let Some(geo_key) = w.place.clone() {
            if let Some((to_c, to_ci)) = bulk_for_same_geoapify.get(&geo_key) {
                println!("\n--- {name} ---");
                if let Some((lat, lon)) = gps::coordinates(&w.current_path) {
                    println!("GPS: {lat:.6}, {lon:.6}");
                }
                println!(
                    "Geoapify: {} / {}  →  using your saved place: {} / {}",
                    geo_key.0, geo_key.1, to_c, to_ci
                );
                if let Err(e) = open::that(&w.current_path) {
                    eprintln!("Could not open file (continuing): {e}");
                }
                w.place = Some((to_c.clone(), to_ci.clone()));
                finalize_non_refresh_geocoded_file(w, folder, log)?;
                try_close_preview_best_effort();
                continue;
            }
            if auto_yes_same_geo.as_ref() == Some(&geo_key) {
                println!("\n--- {name} ---");
                if let Some((lat, lon)) = gps::coordinates(&w.current_path) {
                    println!("GPS: {lat:.6}, {lon:.6}");
                }
                println!(
                    "Geoapify: {} / {}  →  accepting (yes to all for this Geoapify place)",
                    geo_key.0, geo_key.1
                );
                if let Err(e) = open::that(&w.current_path) {
                    eprintln!("Could not open file (continuing): {e}");
                }
                finalize_non_refresh_geocoded_file(w, folder, log)?;
                try_close_preview_best_effort();
                continue;
            }
        }

        let Some((country, city)) = w.place.clone() else {
            continue;
        };

        println!("\n--- {name} ---");
        if let Some((lat, lon)) = gps::coordinates(&w.current_path) {
            println!("GPS: {lat:.6}, {lon:.6}");
        }
        println!("Geoapify: {country} / {city}");

        if let Err(e) = open::that(&w.current_path) {
            eprintln!("Could not open file (continuing): {e}");
        }

        let has_rest = i + 1 < total_review;
        let sel = if has_rest {
            let items = vec![
                "Yes — this file only".to_string(),
                "No — edit country / city".to_string(),
                format!(
                    "Yes — ALL remaining files with Geoapify \"{} / {}\"",
                    country, city
                ),
                "Skip — leave filename unchanged".to_string(),
            ];
            Select::with_theme(&theme)
                .with_prompt("Use this country and city in the filename? (↑/↓, Enter)")
                .items(&items)
                .default(0)
                .interact()
                .map_err(|e| e.to_string())?
        } else {
            let items = vec![
                "Yes — use Geoapify place".to_string(),
                "No — edit country / city".to_string(),
                "Skip — leave filename unchanged".to_string(),
            ];
            Select::with_theme(&theme)
                .with_prompt("Use this country and city in the filename? (↑/↓, Enter)")
                .items(&items)
                .default(0)
                .interact()
                .map_err(|e| e.to_string())?
        };

        if has_rest && sel == 3 {
            w.user_skip_rename = true;
            try_close_preview_best_effort();
            continue;
        }
        if !has_rest && sel == 2 {
            w.user_skip_rename = true;
            try_close_preview_best_effort();
            continue;
        }

        if sel == 1 {
            let (def_c, def_ci) = match &w.stem_placeholders {
                Some((sc, sci, _)) => (sc.as_str(), sci.as_str()),
                None => (country.as_str(), city.as_str()),
            };
            let c = prompt_place_line("Country", Some(def_c))?;
            let ci = prompt_place_line("City", Some(def_ci))?;
            let from_geo = (country.clone(), city.clone());
            w.place = Some((c.clone(), ci.clone()));
            if has_rest {
                let for_rest = Confirm::with_theme(&theme)
                    .with_prompt(format!(
                        "Use this country & city for all remaining files whose Geoapify place is still \"{} / {}\"?",
                        from_geo.0, from_geo.1
                    ))
                    .default(true)
                    .interact()
                    .map_err(|e| e.to_string())?;
                if for_rest {
                    bulk_for_same_geoapify.insert(from_geo, (c, ci));
                }
            }
            finalize_non_refresh_geocoded_file(w, folder, log)?;
        } else if has_rest && sel == 2 {
            auto_yes_same_geo = Some((country.clone(), city.clone()));
            finalize_non_refresh_geocoded_file(w, folder, log)?;
        } else {
            finalize_non_refresh_geocoded_file(w, folder, log)?;
        }

        try_close_preview_best_effort();
    }

    Ok(())
}

fn try_rename_with_stem(
    folder: &Path,
    w: &mut FileWork,
    stem: &str,
    log: &mut RenameLog,
) -> Result<(), String> {
    let ext = extension_as_str(&w.current_path);
    if ext.is_empty() {
        return Err(format!(
            "Skipping: file has no extension: {}",
            w.current_path.display()
        ));
    }

    let target = naming::unique_target_path(folder, stem, &ext, &w.current_path);
    if target == w.current_path {
        println!("Already named correctly; skipping rename.");
        return Ok(());
    }

    let from = w.current_path.clone();
    fs::rename(&from, &target).map_err(|e| e.to_string())?;
    if let Err(e) = log.record(&from, &target) {
        eprintln!("Could not append rename log (continuing): {e}");
    }
    println!(
        "Renamed to {}",
        target.file_name().unwrap_or_default().to_string_lossy()
    );
    w.current_path = target;
    Ok(())
}

pub fn run(cli_folder: Option<PathBuf>) -> Result<(), String> {
    load_dotenv();

    let refresh_geocoding_only = env_truthy(ENV_REFRESH_GEAPIFY_ONLY);
    if refresh_geocoding_only {
        println!(
            "Refresh-only mode: {ENV_REFRESH_GEAPIFY_ONLY}=1 — only fully named YYMMDD-*-*-* files with GPS are reverse-geocoded; other files are left as-is."
        );
    }

    let folder = resolve_folder(cli_folder)?;
    let files = media::list_media_files(&folder)?;

    let mut work: Vec<FileWork> = Vec::new();
    for path in files {
        if extension_as_str(&path).is_empty() {
            eprintln!("Skipping: file has no extension: {}", path.display());
            continue;
        }
        work.push(FileWork {
            current_path: path,
            place: None,
            already_named: false,
            user_skip_rename: false,
            manual_flow_complete: false,
            date_prefix: String::new(),
            stem_date_override: None,
            skip_initial_place_rename: false,
            skip_session_date_mismatch: false,
            stem_placeholders: None,
        });
    }

    if work.is_empty() {
        return Err("No processable media files (all missing extensions?).".to_string());
    }

    let force_full_rerun = if refresh_geocoding_only {
        false
    } else {
        prompt_force_full_rerun()?
    };
    if force_full_rerun {
        println!(
            "Full rerun: ignoring filename layout — every file with GPS will be reverse-geocoded; session/YYMM mismatch skips are off."
        );
        for w in &mut work {
            if let Some(stem) = w.current_path.file_stem().and_then(|s| s.to_str()) {
                w.stem_placeholders = naming::parse_stem_placeholders(stem);
            }
        }
    }

    if !force_full_rerun {
        for w in &mut work {
            let Some(stem) = w.current_path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            match naming::classify_tool_stem(stem) {
                naming::ToolStemClass::FullyNamed => {
                    w.already_named = true;
                }
                naming::ToolStemClass::PlaceOnlyNeedsDescription {
                    date_prefix,
                    country,
                    city,
                } => {
                    w.place = Some((country, city));
                    w.stem_date_override = Some(date_prefix);
                    w.skip_initial_place_rename = true;
                }
                naming::ToolStemClass::NotRecognized => {}
            }
        }
    }

    let missing_embedded = work
        .iter()
        .filter(|w| gps::capture_yymmdd(&w.current_path).is_none())
        .count();
    if missing_embedded > 0 {
        println!(
            "{missing_embedded} file(s) have no embedded capture date; session fallback YYMM00 will be used for those."
        );
    }
    let (yy, mm) = parse_session_year_month()?;
    let yymm = format!("{yy}{mm}");
    let fallback_yymmdd = format!("{yymm}00");

    for w in &mut work {
        let exif_or_video_date = gps::capture_yymmdd(&w.current_path);
        w.date_prefix = exif_or_video_date
            .clone()
            .unwrap_or_else(|| fallback_yymmdd.clone());
        if !force_full_rerun && exif_or_video_date.is_none() {
            if let Some(stem) = w.current_path.file_stem().and_then(|s| s.to_str()) {
                if let Some(ref file_yymm) = naming::leading_yymm_from_stem(stem) {
                    if file_yymm != &yymm {
                        w.skip_session_date_mismatch = true;
                    }
                }
            }
        }
    }

    let mut log = RenameLog::new(&folder);

    if !refresh_geocoding_only && !force_full_rerun {
        let mut legacy_yymm_upgraded = 0_u32;
        for w in &mut work {
            if w.skip_session_date_mismatch {
                continue;
            }
            if !w.already_named || !gps::is_probably_image(&w.current_path) {
                continue;
            }
            let Some(stem) = w.current_path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Some((file_yymm, ref country, ref city, ref desc)) =
                naming::parse_legacy_yymm_four_segment_stem(stem)
            else {
                continue;
            };
            let new_prefix =
                gps::capture_yymmdd(&w.current_path).unwrap_or_else(|| format!("{file_yymm}00"));
            let new_stem = naming::build_stem(&new_prefix, country, city, Some(desc.as_str()));
            if new_stem == stem {
                continue;
            }
            match try_rename_with_stem(&folder, w, &new_stem, &mut log) {
                Ok(()) => legacy_yymm_upgraded += 1,
                Err(e) => eprintln!("Legacy YYMM→YYMMDD upgrade: {e}"),
            }
        }
        if legacy_yymm_upgraded > 0 {
            println!(
                "Upgraded {legacy_yymm_upgraded} legacy YYMM- image name(s) to YYMMDD- (EXIF capture date, or YYMM00 from the filename when missing)."
            );
        }

        let mut capture_date_stem_fixed = 0_u32;
        for w in &mut work {
            if w.skip_session_date_mismatch || !w.already_named {
                continue;
            }
            let Some(stem) = w.current_path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Some(capture) = gps::capture_yymmdd(&w.current_path) else {
                continue;
            };
            let Some(new_stem) = naming::stem_with_embedded_capture_date(stem, &capture) else {
                continue;
            };
            match try_rename_with_stem(&folder, w, &new_stem, &mut log) {
                Ok(()) => capture_date_stem_fixed += 1,
                Err(e) => eprintln!("Embedded capture date vs filename prefix: {e}"),
            }
        }
        if capture_date_stem_fixed > 0 {
            println!(
                "Renamed {capture_date_stem_fixed} already-named file(s) so the leading YYMMDD matches embedded capture date."
            );
        }
    }

    if refresh_geocoding_only {
        let eligible = work
            .iter()
            .filter(|w| {
                w.already_named
                    && !w.skip_session_date_mismatch
                    && gps::coordinates(&w.current_path).is_some()
                    && w.current_path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .is_some_and(|st| naming::parse_fully_named_stem_for_refresh(st).is_some())
            })
            .count();
        if eligible == 0 {
            return Err(
                "Refresh-only mode: no fully named YYMMDD-*-*-* files with GPS (or stems could not be parsed for refresh)."
                    .into(),
            );
        }
        println!(
            "Refresh-only: {eligible} fully named file(s) with GPS will be reverse-geocoded; all other files are ignored for this run."
        );
    } else if force_full_rerun {
        println!(
            "{} file(s) in folder; full rerun — all with GPS go to Geoapify; others get prompts (fallback date prefix {fallback_yymmdd} when EXIF/video date is missing).",
            work.len()
        );
    } else {
        let skip_count = work.iter().filter(|w| w.already_named).count();
        let mismatch_skip_count = work.iter().filter(|w| w.skip_session_date_mismatch).count();
        let active_count = work
            .len()
            .saturating_sub(skip_count)
            .saturating_sub(mismatch_skip_count);
        if mismatch_skip_count > 0 {
            println!(
                "{mismatch_skip_count} file(s) skipped: filename starts with YYMM that is not session {yymm} (no embedded capture date); avoiding wrong renames:"
            );
            for w in work.iter().filter(|w| w.skip_session_date_mismatch) {
                let fname = w
                    .current_path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy();
                let head = naming::leading_yymm_from_stem(
                    w.current_path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or(""),
                )
                .unwrap_or_else(|| "?".into());
                println!("  • {fname} (file YYMM {head}, session {yymm})");
            }
        }
        if skip_count > 0 {
            println!(
                "{skip_count} file(s) already look like YYMMDD-*-*-* (or legacy YYMM-*-*-*; not necessarily session {yymm}); skipping API and renames:"
            );
            for w in work.iter().filter(|w| w.already_named) {
                let fname = w
                    .current_path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy();
                let head = w
                    .current_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|st| st.split('-').find(|p| !p.is_empty()))
                    .unwrap_or("?");
                println!("  • {fname} (prefix {head}-…)");
            }
        }

        println!(
            "{} file(s) in folder; {} still go through GPS / geocoding / prompts (fallback date prefix {fallback_yymmdd} when EXIF/video date is missing).",
            work.len(),
            active_count
        );
    }
    let _ = std::io::stdout().flush();

    println!("\nReading GPS and calling Geoapify where needed (videos can be slow)…");
    let _ = std::io::stdout().flush();

    let geoapify_total = geoapify_candidate_count(&work, force_full_rerun, refresh_geocoding_only);
    let mut api_key = env::var("GEOAPIFY_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
        .map(|k| k.trim().to_string());
    if geoapify_total > 0 && api_key.is_none() {
        println!(
            "{geoapify_total} file(s) have GPS and can be reverse-geocoded with a Geoapify API key."
        );
        api_key = prompt_geoapify_api_key()?;
    }
    let mut limiter = SlidingWindowRateLimiter::new(Duration::from_millis(1500), 5);
    let mut geoapify_index = 0usize;

    for w in &mut work {
        if !force_full_rerun && w.skip_session_date_mismatch {
            continue;
        }
        if refresh_geocoding_only {
            if !w.already_named {
                continue;
            }
        } else if !force_full_rerun && (w.already_named || w.place.is_some()) {
            continue;
        }
        let Some((lat, lon)) = gps::coordinates(&w.current_path) else {
            continue;
        };
        let Some(ref key) = api_key else {
            continue;
        };
        limiter.acquire();
        geoapify_index += 1;
        let name = w
            .current_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        println!("Geoapify [{geoapify_index}/{geoapify_total}] {name} — requesting…");
        let _ = std::io::stdout().flush();
        match geoapify::reverse_geocode(lat, lon, key.trim(), &name) {
            Ok(pair) => {
                let (ref country, ref city) = pair;
                println!("  → {country} / {city}");
                w.place = Some(pair);
            }
            Err(e) => eprintln!("Geocoding failed for {name}: {e}"),
        }
    }

    println!("Done GPS / geocode pass.");

    if refresh_geocoding_only {
        refresh_place_review_and_rename(&mut work, &folder, &mut log)?;
        println!("Done refresh-only run.");
        return Ok(());
    }

    geocoded_interactive_place_desc_rename(&mut work, &folder, &mut log)?;

    let manual_total = work
        .iter()
        .filter(|w| !w.already_named && w.place.is_none() && !w.skip_session_date_mismatch)
        .count();
    let mut manual_index = 0_usize;

    if manual_total > 0 {
        println!("\n--- Files without GPS / geocoding: place + optional description ---");
    }
    let mut last_country: Option<String> = None;
    let mut last_city: Option<String> = None;
    for w in &mut work {
        if w.already_named || w.place.is_some() || w.skip_session_date_mismatch {
            continue;
        }
        manual_index += 1;
        let name = w
            .current_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        println!("\n--- Manual [{manual_index}/{manual_total}] {name} ---");

        if let Err(e) = open::that(&w.current_path) {
            eprintln!("Could not open file (continuing): {e}");
        }

        let theme = ColorfulTheme::default();
        let action = Select::with_theme(&theme)
            .with_prompt("This file has no GPS in metadata")
            .items(&[
                "Add country, city & description".to_string(),
                "Skip — leave filename unchanged".to_string(),
            ])
            .default(0)
            .interact()
            .map_err(|e| e.to_string())?;
        if action == 1 {
            w.user_skip_rename = true;
            try_close_preview_best_effort();
            continue;
        }

        let country = prompt_place_line(
            "Country",
            last_country
                .as_deref()
                .or_else(|| w.stem_placeholders.as_ref().map(|s| s.0.as_str())),
        )?;
        let city = prompt_place_line(
            "City",
            last_city
                .as_deref()
                .or_else(|| w.stem_placeholders.as_ref().map(|s| s.1.as_str())),
        )?;
        last_country = Some(country.clone());
        last_city = Some(city.clone());
        w.place = Some((country.clone(), city.clone()));

        let desc_opt = prompt_optional_description(
            w.stem_placeholders
                .as_ref()
                .and_then(|(_, _, d)| d.as_deref()),
        )?;
        let stem = naming::build_stem(&w.date_prefix, &country, &city, desc_opt.as_deref());
        match try_rename_with_stem(&folder, w, &stem, &mut log) {
            Ok(()) => {
                w.manual_flow_complete = true;
                try_close_preview_best_effort();
            }
            Err(e) => eprintln!("{e}"),
        }
    }

    let stem_total = work
        .iter()
        .filter(|w| {
            !w.already_named
                && !w.skip_session_date_mismatch
                && !w.manual_flow_complete
                && !w.user_skip_rename
                && w.skip_initial_place_rename
                && w.place.is_some()
        })
        .count();
    if stem_total > 0 {
        println!(
            "\n--- Stem place + description (country/city from filename; optional description) ---"
        );
        println!(
            "Tip: if the filename used a POI or building number as city, edit before the description."
        );
    }
    let stem_theme = ColorfulTheme::default();
    for w in &mut work {
        if w.already_named
            || w.skip_session_date_mismatch
            || w.manual_flow_complete
            || w.user_skip_rename
            || !w.skip_initial_place_rename
        {
            continue;
        }
        let Some((country, city)) = w.place.clone() else {
            continue;
        };
        let name = w
            .current_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        println!("\n--- {name} ---");

        if let Err(e) = open::that(&w.current_path) {
            eprintln!("Could not open file (continuing): {e}");
        }

        println!("Place from filename: {country} / {city}");
        let keep = Confirm::with_theme(&stem_theme)
            .with_prompt("Keep this country & city in the filename? (No = edit)")
            .default(true)
            .interact()
            .map_err(|e| e.to_string())?;
        if !keep {
            let def_c = w
                .stem_placeholders
                .as_ref()
                .map(|s| s.0.as_str())
                .unwrap_or(country.as_str());
            let def_ci = w
                .stem_placeholders
                .as_ref()
                .map(|s| s.1.as_str())
                .unwrap_or(city.as_str());
            let c = prompt_place_line("Country", Some(def_c))?;
            let ci = prompt_place_line("City", Some(def_ci))?;
            w.place = Some((c, ci));
        }

        let desc_opt = prompt_optional_description(
            w.stem_placeholders
                .as_ref()
                .and_then(|(_, _, d)| d.as_deref()),
        )?;

        let (country, city) = w
            .place
            .as_ref()
            .ok_or_else(|| "internal: missing country/city for stem file".to_string())?;
        let stem_date = stem_date_for_final_rename(w);
        let stem = naming::build_stem(&stem_date, country, city, desc_opt.as_deref());
        match try_rename_with_stem(&folder, w, &stem, &mut log) {
            Ok(()) => try_close_preview_best_effort(),
            Err(e) => eprintln!("{e}"),
        }
    }

    Ok(())
}

#[cfg(test)]
mod parse_year_month_tests {
    use super::parse_combined_year_month;

    #[test]
    fn parses_yyyy_mm() {
        let (yy, mm) = parse_combined_year_month("2026-04").unwrap();
        assert_eq!(yy, "26");
        assert_eq!(mm, "04");
    }

    #[test]
    fn parses_yy_slash_m() {
        let (yy, mm) = parse_combined_year_month("26/4").unwrap();
        assert_eq!(yy, "26");
        assert_eq!(mm, "04");
    }
}
