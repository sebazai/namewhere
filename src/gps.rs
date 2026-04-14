use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::process::Command;

use exif::{Field, In, Reader, Tag, Value};
use regex::Regex;
use serde_json::Value as JsonValue;

fn rational_to_f64(r: exif::Rational) -> f64 {
    r.num as f64 / r.denom as f64
}

fn gps_coord_from_rationals(rats: &[exif::Rational], ref_byte: Option<u8>) -> Option<f64> {
    if rats.len() != 3 {
        return None;
    }
    let d = rational_to_f64(rats[0]);
    let m = rational_to_f64(rats[1]);
    let s = rational_to_f64(rats[2]);
    let mut v = d + m / 60.0 + s / 3600.0;
    let neg = matches!(ref_byte, Some(b'S' | b'W'));
    if neg {
        v = -v;
    }
    Some(v)
}

fn value_first_ascii_byte(v: &Value) -> Option<u8> {
    match v {
        Value::Ascii(parts) => parts.first().and_then(|p| p.first().copied()),
        _ => None,
    }
}

/// Reads GPS from EXIF when `path` looks like a still image we can parse.
fn gps_from_exif(path: &Path) -> Option<(f64, f64)> {
    let file = File::open(path).ok()?;
    let mut buf = BufReader::new(file);
    let exif = Reader::new().read_from_container(&mut buf).ok()?;

    let lat_field = exif.get_field(Tag::GPSLatitude, In::PRIMARY)?;
    let lon_field = exif.get_field(Tag::GPSLongitude, In::PRIMARY)?;
    let lat_ref = exif
        .get_field(Tag::GPSLatitudeRef, In::PRIMARY)
        .and_then(|f| value_first_ascii_byte(&f.value));
    let lon_ref = exif
        .get_field(Tag::GPSLongitudeRef, In::PRIMARY)
        .and_then(|f| value_first_ascii_byte(&f.value));

    let (Value::Rational(lat_rats), Value::Rational(lon_rats)) =
        (&lat_field.value, &lon_field.value)
    else {
        return None;
    };

    let lat = gps_coord_from_rationals(lat_rats, lat_ref)?;
    let lon = gps_coord_from_rationals(lon_rats, lon_ref)?;
    Some((lat, lon))
}

fn exif_ascii_datetime_field(field: &Field) -> Option<&str> {
    match &field.value {
        Value::Ascii(parts) => {
            let p = parts.first()?;
            std::str::from_utf8(p)
                .ok()
                .map(|s| s.trim_end_matches('\0').trim())
        }
        _ => None,
    }
}

/// EXIF date/time: `YYYY:MM:DD HH:MM:SS` or `YYYY:MM:DD`.
fn parse_exif_date_prefix(s: &str) -> Option<(u32, u32, u32)> {
    let date_part = s.split_whitespace().next()?;
    let mut it = date_part.split(':');
    let y: u32 = it.next()?.parse().ok()?;
    let mo: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || !(1900..=2100).contains(&y) {
        return None;
    }
    Some((y, mo, d))
}

fn parse_iso8601_calendar_date(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.trim();
    if s.len() < 10 || s.as_bytes().get(4) != Some(&b'-') {
        return None;
    }
    let y: u32 = s.get(0..4)?.parse().ok()?;
    let mo: u32 = s.get(5..7)?.parse().ok()?;
    let d: u32 = s.get(8..10)?.parse().ok()?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || !(1900..=2100).contains(&y) {
        return None;
    }
    Some((y, mo, d))
}

fn parse_ffprobe_date_tag(raw: &str) -> Option<(u32, u32, u32)> {
    parse_iso8601_calendar_date(raw).or_else(|| parse_exif_date_prefix(raw))
}

fn yymmdd_tuple(y: u32, mo: u32, d: u32) -> String {
    format!("{:02}{:02}{:02}", y % 100, mo, d)
}

fn yymmdd_from_exif(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let mut buf = BufReader::new(file);
    let exif = Reader::new().read_from_container(&mut buf).ok()?;

    for tag in [Tag::DateTimeOriginal, Tag::DateTimeDigitized, Tag::DateTime] {
        if let Some(field) = exif.get_field(tag, In::PRIMARY) {
            if let Some(s) = exif_ascii_datetime_field(field) {
                if let Some((y, mo, d)) = parse_exif_date_prefix(s) {
                    return Some(yymmdd_tuple(y, mo, d));
                }
            }
        }
    }
    None
}

fn yymmdd_from_ffprobe(path: &Path) -> Option<String> {
    let stdout = ffprobe_stdout(path)?;
    let root: JsonValue = serde_json::from_slice(&stdout).ok()?;
    let pairs = collect_ffprobe_tag_pairs(&root);

    const KEYS: &[&str] = &["creation_time", "com.apple.quicktime.creationdate", "date"];

    for key in KEYS {
        if let Some((_, val)) = pairs.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)) {
            if let Some((y, mo, d)) = parse_ffprobe_date_tag(val) {
                return Some(yymmdd_tuple(y, mo, d));
            }
        }
    }

    for (k, v) in &pairs {
        let kl = k.to_ascii_lowercase();
        if kl.contains("creation") && kl.contains("date") {
            if let Some((y, mo, d)) = parse_ffprobe_date_tag(v) {
                return Some(yymmdd_tuple(y, mo, d));
            }
        }
    }

    None
}

/// Capture-time `YYMMDD` from EXIF (images) or ffprobe tags (video). `None` if unknown.
pub fn capture_yymmdd(path: &Path) -> Option<String> {
    if is_probably_image(path) {
        if let Some(s) = yymmdd_from_exif(path) {
            return Some(s);
        }
    }
    if is_probably_video(path) {
        return yymmdd_from_ffprobe(path);
    }
    yymmdd_from_ffprobe(path).or_else(|| yymmdd_from_exif(path))
}

fn parse_lat_lon_tag(raw: &str) -> Option<(f64, f64)> {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"([+-][0-9]+(?:\.[0-9]+)?)([+-][0-9]+(?:\.[0-9]+)?)").expect("regex")
    });
    let caps = re.captures(raw)?;
    let lat: f64 = caps[1].parse().ok()?;
    let lon: f64 = caps[2].parse().ok()?;
    Some((lat, lon))
}

fn ffprobe_stdout(path: &Path) -> Option<Vec<u8>> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(output.stdout)
}

fn collect_ffprobe_tag_pairs(root: &JsonValue) -> Vec<(String, String)> {
    let mut out = Vec::new();

    if let Some(fmt) = root.get("format").and_then(|f| f.as_object()) {
        if let Some(tags) = fmt.get("tags").and_then(|t| t.as_object()) {
            for (k, v) in tags {
                if let Some(s) = v.as_str() {
                    out.push((k.clone(), s.to_string()));
                }
            }
        }
    }

    if let Some(streams) = root.get("streams").and_then(|s| s.as_array()) {
        for stream in streams {
            if let Some(tags) = stream.get("tags").and_then(|t| t.as_object()) {
                for (k, v) in tags {
                    if let Some(s) = v.as_str() {
                        out.push((k.clone(), s.to_string()));
                    }
                }
            }
        }
    }

    out
}

fn gps_from_ffprobe(path: &Path) -> Option<(f64, f64)> {
    let stdout = ffprobe_stdout(path)?;
    let root: JsonValue = serde_json::from_slice(&stdout).ok()?;
    let pairs = collect_ffprobe_tag_pairs(&root);

    let keys_priority = [
        "com.apple.quicktime.location.ISO6709",
        "location",
        "location-eng",
    ];

    for key in keys_priority {
        if let Some((_, val)) = pairs.iter().find(|(k, _)| k == key) {
            if let Some(coords) = parse_lat_lon_tag(val) {
                return Some(coords);
            }
        }
    }

    for (k, v) in &pairs {
        let kl = k.to_ascii_lowercase();
        if kl.contains("location") || kl.contains("iso6709") {
            if let Some(coords) = parse_lat_lon_tag(v) {
                return Some(coords);
            }
        }
    }

    None
}

pub fn is_probably_image(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("jpg" | "jpeg" | "png" | "tif" | "tiff" | "webp" | "heic" | "gif")
    )
}

fn is_probably_video(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("mp4" | "mov" | "m4v" | "avi" | "mkv")
    )
}

/// Best-effort GPS coordinates from embedded metadata (EXIF or video tags).
pub fn coordinates(path: &Path) -> Option<(f64, f64)> {
    if is_probably_image(path) {
        if let Some(c) = gps_from_exif(path) {
            return Some(c);
        }
    }
    if is_probably_video(path) {
        return gps_from_ffprobe(path);
    }
    gps_from_ffprobe(path).or_else(|| gps_from_exif(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parse_exif_date_formats() {
        assert_eq!(
            parse_exif_date_prefix("2024:03:15 14:30:00"),
            Some((2024, 3, 15))
        );
        assert_eq!(parse_exif_date_prefix("2024:03:15"), Some((2024, 3, 15)));
        assert_eq!(
            parse_iso8601_calendar_date("2024-03-15T12:00:00.000Z"),
            Some((2024, 3, 15))
        );
    }

    #[test]
    fn coordinates_reads_gps_from_exif_jpeg_fixture() {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gps_san_francisco.jpg");
        let (lat, lon) = coordinates(&path).expect("fixture JPEG should contain GPS EXIF");
        assert!((lat - 37.7749).abs() < 1e-4);
        assert!((lon - (-122.4194)).abs() < 1e-4);
    }
}
