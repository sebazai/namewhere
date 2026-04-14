use std::collections::{HashMap, VecDeque};
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

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

fn parse_year_month() -> Result<(String, String), String> {
    let theme = ColorfulTheme::default();

    let year_in: String = Input::with_theme(&theme)
        .with_prompt("Year (e.g. 2024 or 24)")
        .interact_text()
        .map_err(|e| e.to_string())?;

    let y: u32 = year_in
        .trim()
        .parse()
        .map_err(|_| "Invalid year".to_string())?;

    let yy = if y < 100 { y } else { y % 100 };
    let yymm_yy = format!("{yy:02}");

    let month_in: String = Input::with_theme(&theme)
        .with_prompt("Month (1-12; used as YYMM00 when file has no EXIF/video date)")
        .interact_text()
        .map_err(|e| e.to_string())?;

    let m: u32 = month_in
        .trim()
        .parse()
        .map_err(|_| "Invalid month".to_string())?;
    if !(1..=12).contains(&m) {
        return Err("Month must be 1-12".to_string());
    }
    let mm = format!("{m:02}");

    Ok((yymm_yy, mm))
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

fn validate_geocoded_places_before_rename(
    work: &mut [FileWork],
    refresh_geocoding_only: bool,
) -> Result<(), String> {
    let theme = ColorfulTheme::default();
    let to_review: Vec<usize> = work
        .iter()
        .enumerate()
        .filter(|(_, w)| needs_geocode_place_validation(w, refresh_geocoding_only))
        .map(|(i, _)| i)
        .collect();

    if to_review.is_empty() {
        return Ok(());
    }

    println!(
        "\n{} file(s) have place names from Geoapify (GPS). You can confirm or edit each before renaming{}.",
        to_review.len(),
        if refresh_geocoding_only {
            " (refresh mode: fully named files)"
        } else {
            ""
        }
    );
    let do_review = Confirm::with_theme(&theme)
        .with_prompt("Review country/city for those files now?")
        .default(true)
        .interact()
        .map_err(|e| e.to_string())?;

    if !do_review {
        return Ok(());
    }

    let total_review = to_review.len();
    // Geoapify (country, city) → user-chosen pair; only applied when a file still has that Geoapify result.
    let mut bulk_for_same_geoapify: HashMap<(String, String), (String, String)> = HashMap::new();
    // After choosing "yes to all" for a Geoapify pair, accept that pair without prompting for matching files.
    let mut auto_yes_same_geo: Option<(String, String)> = None;

    for (i, &idx) in to_review.iter().enumerate() {
        let w = &mut work[idx];
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
            ];
            Select::with_theme(&theme)
                .with_prompt("Use this country and city in the filename? (↑/↓, Enter)")
                .items(&items)
                .default(0)
                .interact()
                .map_err(|e| e.to_string())?
        };

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
        } else if has_rest && sel == 2 {
            auto_yes_same_geo = Some((country.clone(), city.clone()));
        }
        try_close_preview_best_effort();
    }

    Ok(())
}

fn try_rename_with_stem(folder: &Path, w: &mut FileWork, stem: &str) -> Result<(), String> {
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

    fs::rename(&w.current_path, &target).map_err(|e| e.to_string())?;
    println!(
        "Renamed to {}",
        target.file_name().unwrap_or_default().to_string_lossy()
    );
    w.current_path = target;
    Ok(())
}

fn rename_place_only(folder: &Path, w: &mut FileWork) -> Result<(), String> {
    let (country, city) = w
        .place
        .as_ref()
        .ok_or_else(|| "internal: missing country/city".to_string())?;
    let stem = naming::build_stem(&w.date_prefix, country, city, None);
    try_rename_with_stem(folder, w, &stem)
}

pub fn run() -> Result<(), String> {
    load_dotenv();

    let refresh_geocoding_only = env_truthy(ENV_REFRESH_GEAPIFY_ONLY);
    if refresh_geocoding_only {
        println!(
            "Refresh-only mode: {ENV_REFRESH_GEAPIFY_ONLY}=1 — only fully named YYMMDD-*-*-* files with GPS are reverse-geocoded; other files are left as-is."
        );
    }

    let folder = pick_folder()?;
    let files = media::list_media_files(&folder)?;
    if files.is_empty() {
        return Err("No images or videos found in that folder.".to_string());
    }

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

    let (yy, mm) = parse_year_month()?;
    let yymm = format!("{yy}{mm}");
    let fallback_yymmdd = format!("{yymm}00");

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
            match try_rename_with_stem(&folder, w, &new_stem) {
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
            match try_rename_with_stem(&folder, w, &new_stem) {
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

    // Pass 1: reverse geocode (rate-limited) for all files with GPS + API key
    let mut api_key = env::var("GEOAPIFY_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
        .map(|k| k.trim().to_string());
    let mut prompted_for_api_key = false;
    let mut limiter = SlidingWindowRateLimiter::new(Duration::from_millis(1500), 5);

    let geoapify_total = geoapify_candidate_count(&work, force_full_rerun, refresh_geocoding_only);
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
        if api_key.is_none() && !prompted_for_api_key {
            prompted_for_api_key = true;
            api_key = prompt_geoapify_api_key()?;
        }
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

    validate_geocoded_places_before_rename(&mut work, refresh_geocoding_only)?;

    if refresh_geocoding_only {
        println!("\n--- Renaming from refreshed Geoapify country/city ---");
        for w in &mut work {
            if !w.already_named || w.skip_session_date_mismatch {
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
            let Some(stem_str) = w.current_path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Some((file_date, desc_seg)) = naming::parse_fully_named_stem_for_refresh(stem_str)
            else {
                eprintln!("Refresh: cannot parse stem for {name}; skipping rename.");
                continue;
            };
            let stem_date = if gps::capture_yymmdd(&w.current_path).is_some() {
                w.date_prefix.clone()
            } else {
                naming::normalize_date_prefix_for_stem(&file_date)
            };
            let desc_opt = (!desc_seg.is_empty()).then_some(desc_seg.as_str());
            let new_stem = naming::build_stem(&stem_date, &country, &city, desc_opt);
            if let Err(e) = try_rename_with_stem(&folder, w, &new_stem) {
                eprintln!("{e}");
            }
        }
        println!("Done refresh-only run.");
        return Ok(());
    }

    println!("\n--- Renaming geocoded files (YYMMDD-Country-City) ---");
    for w in &mut work {
        if w.already_named || w.skip_session_date_mismatch {
            continue;
        }
        if w.place.is_some() && !w.skip_initial_place_rename {
            if let Err(e) = rename_place_only(&folder, w) {
                eprintln!("{e}");
            }
        }
    }

    println!("\n--- Files without GPS / geocoding: place + optional description ---");
    let mut last_country: Option<String> = None;
    let mut last_city: Option<String> = None;
    for w in &mut work {
        if w.already_named || w.place.is_some() || w.skip_session_date_mismatch {
            continue;
        }
        let name = w
            .current_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        println!("\n--- {name} ---");

        if let Err(e) = open::that(&w.current_path) {
            eprintln!("Could not open file (continuing): {e}");
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
        match try_rename_with_stem(&folder, w, &stem) {
            Ok(()) => {
                w.manual_flow_complete = true;
                try_close_preview_best_effort();
            }
            Err(e) => eprintln!("{e}"),
        }
    }

    println!("\n--- Descriptions (geocoded files only; optional) ---");
    println!(
        "Tip: if geocoding used a POI or bure number (e.g. Bure-30), choose “No” to set a better city or place before the description."
    );
    let desc_theme = ColorfulTheme::default();
    for w in &mut work {
        let name = w
            .current_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        if w.skip_session_date_mismatch {
            println!(
                "\n--- {name} --- (filename YYMM ≠ session {yymm}, no embedded date; skipping)"
            );
            continue;
        }
        if w.already_named {
            println!("\n--- {name} --- (already date-*-*-*; skipping)");
            continue;
        }
        if w.manual_flow_complete {
            println!("\n--- {name} --- (place + description done earlier; skipping)");
            continue;
        }
        println!("\n--- {name} ---");

        if let Err(e) = open::that(&w.current_path) {
            eprintln!("Could not open file (continuing): {e}");
        }

        if let Some((country, city)) = w.place.clone() {
            println!("Geocoded place: {country} / {city}");
            let keep = Confirm::with_theme(&desc_theme)
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
        }

        let desc_opt = prompt_optional_description(
            w.stem_placeholders
                .as_ref()
                .and_then(|(_, _, d)| d.as_deref()),
        )?;

        let (country, city) = w.place.as_ref().expect("place set for geocoded files");
        // Prefer embedded capture date over the stem when both exist (stem can be a stale YYMM/YYMMDD).
        let stem_date = if gps::capture_yymmdd(&w.current_path).is_some() {
            w.date_prefix.clone()
        } else {
            w.stem_date_override
                .as_deref()
                .map(naming::normalize_date_prefix_for_stem)
                .unwrap_or_else(|| w.date_prefix.clone())
        };
        let stem = naming::build_stem(&stem_date, country, city, desc_opt.as_deref());
        match try_rename_with_stem(&folder, w, &stem) {
            Ok(()) => try_close_preview_best_effort(),
            Err(e) => eprintln!("{e}"),
        }
    }

    Ok(())
}
