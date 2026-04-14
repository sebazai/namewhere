use std::path::{Path, PathBuf};

/// Makes a single path segment safe for file names; collapses repeated `-`.
pub fn sanitize_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.trim().chars() {
        match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => out.push('-'),
            c if c.is_control() => {}
            ' ' => out.push('-'),
            c => out.push(c),
        }
    }
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out.trim_matches('-').to_string()
}

/// Like [`sanitize_segment`], but runs of whitespace become `_` (not `-`).
fn sanitize_description_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.trim().chars() {
        match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => out.push('-'),
            c if c.is_whitespace() => out.push('_'),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    out.trim_matches(|ch| ch == '-' || ch == '_').to_string()
}

const MAX_BASE_LEN: usize = 180;

fn is_four_digit_yymm(s: &str) -> bool {
    s.len() == 4 && s.chars().all(|c| c.is_ascii_digit())
}

/// Leading four-digit `YYMM` plus at least three more non-empty dash-separated segments.
/// Matches our on-disk shape regardless of which month/year is in the name — used to skip
/// geocoding and renames when the file is already convention-named (even if you picked a
/// different year/month for this run).
pub fn stem_matches_tool_naming_layout(stem: &str) -> bool {
    let parts: Vec<&str> = stem.split('-').filter(|s| !s.is_empty()).collect();
    if parts.len() < 4 {
        return false;
    }
    is_four_digit_yymm(parts[0])
}

/// Builds the filename stem: `YYMM-Country-City` or `YYMM-Country-City-Description`.
/// Description uses underscores for whitespace; country/city still use `-` for spaces.
pub fn build_stem(yymm: &str, country: &str, city: &str, description: Option<&str>) -> String {
    let c = sanitize_segment(country);
    let t = sanitize_segment(city);
    let mut base = format!("{yymm}-{c}-{t}");
    if let Some(d) = description {
        let d = d.trim();
        if !d.is_empty() {
            let sd = sanitize_description_segment(d);
            if !sd.is_empty() {
                base.push('-');
                base.push_str(&sd);
            }
        }
    }
    if base.len() > MAX_BASE_LEN {
        base.truncate(MAX_BASE_LEN);
        base = base
            .trim_end_matches(|ch| ch == '-' || ch == '_')
            .to_string();
    }
    base
}

/// Picks a non-colliding path: `stem.ext`, then `stem-2.ext`, `stem-3.ext`, …
/// `exclude` is the current file path (treated as free so rename-in-place works).
pub fn unique_target_path(dir: &Path, stem: &str, ext: &str, exclude: &Path) -> PathBuf {
    let ext_lower = ext.to_ascii_lowercase();
    let mut n = 1_u32;
    loop {
        let fname = if n == 1 {
            format!("{stem}.{ext_lower}")
        } else {
            format!("{stem}-{n}.{ext_lower}")
        };
        let candidate = dir.join(&fname);
        if candidate == exclude || !candidate.exists() {
            return candidate;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stem_tool_layout_requires_four_digit_yymm_and_three_more() {
        assert!(stem_matches_tool_naming_layout("2504-US-NYC-trip"));
        assert!(stem_matches_tool_naming_layout(
            "2504-United-States-New-York-beach"
        ));
        assert!(stem_matches_tool_naming_layout("2503-US-NYC-trip"));
        assert!(!stem_matches_tool_naming_layout("2504-US-NYC"));
        assert!(!stem_matches_tool_naming_layout("abcd-US-NYC-trip"));
        assert!(!stem_matches_tool_naming_layout("250-US-NYC-trip"));
    }

    #[test]
    fn description_whitespace_becomes_underscore() {
        let s = build_stem("2504", "US", "NYC", Some("my beach trip"));
        assert_eq!(s, "2504-US-NYC-my_beach_trip");
        let s = build_stem("2504", "US", "NYC", Some("a\tb"));
        assert_eq!(s, "2504-US-NYC-a_b");
    }
}
