use std::collections::VecDeque;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use dialoguer::{theme::ColorfulTheme, Input, Password};

use crate::geoapify;
use crate::gps;
use crate::media;
use crate::naming;

fn load_dotenv() {
    let _ = dotenvy::dotenv();
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
        .with_prompt("Month (1-12)")
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

fn prompt_optional_description() -> Result<Option<String>, String> {
    let theme = ColorfulTheme::default();
    let description: String = Input::with_theme(&theme)
        .with_prompt("Description (optional, Enter to skip)")
        .allow_empty(true)
        .interact_text()
        .map_err(|e| e.to_string())?;
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
    /// `NNNN-*-*-*` naming shape (four-digit YYMM in the file + three segments); skip API and renames.
    already_named: bool,
    /// Manual place + description done in one pass; skip the later description-only pass.
    manual_flow_complete: bool,
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

fn rename_place_only(folder: &Path, yymm: &str, w: &mut FileWork) -> Result<(), String> {
    let (country, city) = w
        .place
        .as_ref()
        .ok_or_else(|| "internal: missing country/city".to_string())?;
    let stem = naming::build_stem(yymm, country, city, None);
    try_rename_with_stem(folder, w, &stem)
}

pub fn run() -> Result<(), String> {
    load_dotenv();

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
        });
    }

    if work.is_empty() {
        return Err("No processable media files (all missing extensions?).".to_string());
    }

    let (yy, mm) = parse_year_month()?;
    let yymm = format!("{yy}{mm}");

    for w in &mut work {
        let Some(stem) = w.current_path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if naming::stem_matches_tool_naming_layout(stem) {
            w.already_named = true;
        }
    }

    let skip_count = work.iter().filter(|w| w.already_named).count();
    let active_count = work.len().saturating_sub(skip_count);
    if skip_count > 0 {
        println!(
            "{skip_count} file(s) already look like YYMM-*-*-* (leading token is any 4 digits, not necessarily {yymm}); skipping API and renames:"
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
        "{} file(s) in folder; {} still go through GPS / geocoding / prompts (session YYMM {yymm}).",
        work.len(),
        active_count
    );
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

    for w in &mut work {
        if w.already_named || w.place.is_some() {
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
        let name = w
            .current_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        match geoapify::reverse_geocode(lat, lon, key.trim()) {
            Ok(pair) => w.place = Some(pair),
            Err(e) => eprintln!("Geocoding failed for {name}: {e}"),
        }
    }

    println!("Done GPS / geocode pass.");

    println!("\n--- Renaming geocoded files (YYMM-Country-City) ---");
    for w in &mut work {
        if w.already_named {
            continue;
        }
        if w.place.is_some() {
            if let Err(e) = rename_place_only(&folder, &yymm, w) {
                eprintln!("{e}");
            }
        }
    }

    println!("\n--- Files without GPS / geocoding: place + optional description ---");
    let mut last_country: Option<String> = None;
    let mut last_city: Option<String> = None;
    for w in &mut work {
        if w.already_named || w.place.is_some() {
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

        let country = prompt_place_line("Country", last_country.as_deref())?;
        let city = prompt_place_line("City", last_city.as_deref())?;
        last_country = Some(country.clone());
        last_city = Some(city.clone());
        w.place = Some((country.clone(), city.clone()));

        let desc_opt = prompt_optional_description()?;
        let stem = naming::build_stem(&yymm, &country, &city, desc_opt.as_deref());
        match try_rename_with_stem(&folder, w, &stem) {
            Ok(()) => {
                w.manual_flow_complete = true;
                try_close_preview_best_effort();
            }
            Err(e) => eprintln!("{e}"),
        }
    }

    println!("\n--- Descriptions (geocoded files only; optional) ---");
    for w in &mut work {
        let name = w
            .current_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        if w.already_named {
            println!("\n--- {name} --- (already YYMM-*-*-*; skipping)");
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

        let desc_opt = prompt_optional_description()?;

        let (country, city) = w.place.as_ref().expect("place set for geocoded files");
        let stem = naming::build_stem(&yymm, country, city, desc_opt.as_deref());
        match try_rename_with_stem(&folder, w, &stem) {
            Ok(()) => try_close_preview_best_effort(),
            Err(e) => eprintln!("{e}"),
        }
    }

    Ok(())
}
