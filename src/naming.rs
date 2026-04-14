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

/// First stem segment: legacy `YYMM` (4 digits) or `YYMMDD` (6 digits).
fn is_leading_date_prefix(s: &str) -> bool {
    matches!(s.len(), 4 | 6) && s.chars().all(|c| c.is_ascii_digit())
}

/// Legacy filenames used a 4-digit `YYMM` prefix; expand to `YYMMDD` with day `00` for new names.
pub fn normalize_date_prefix_for_stem(prefix: &str) -> String {
    if prefix.len() == 4 && prefix.chars().all(|c| c.is_ascii_digit()) {
        format!("{prefix}00")
    } else {
        prefix.to_string()
    }
}

fn segment_is_ascii_digits(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

/// How much of our `YYMMDD-Country-City[-Description][-N]` (or legacy `YYMM-…`) stem matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolStemClass {
    /// Date prefix plus country, city, and a real description segment (non-numeric); optional
    /// trailing `-2`, `-3`, … collision suffixes are ignored for this check.
    FullyNamed,
    /// After stripping trailing numeric segments, only `YYMMDD-Country-City` (or legacy
    /// `YYMM-Country-City`) remains — e.g. `260314-Finland-Helsinki-1` or `2603-Finland-Helsinki-2`.
    PlaceOnlyNeedsDescription {
        date_prefix: String,
        country: String,
        city: String,
    },
    /// Does not start with a 4- or 6-digit date prefix in the first segment, or too few segments.
    NotRecognized,
}

/// Strips trailing `-<digits>` pieces (collision counters like `…-2`, or placeholders `…-1`), then
/// classifies the remainder.
pub fn classify_tool_stem(stem: &str) -> ToolStemClass {
    let mut parts: Vec<&str> = stem.split('-').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() || !is_leading_date_prefix(parts[0]) {
        return ToolStemClass::NotRecognized;
    }
    while parts.len() > 1 && segment_is_ascii_digits(parts[parts.len() - 1]) {
        parts.pop();
    }
    match parts.len() {
        n if n >= 4 => ToolStemClass::FullyNamed,
        3 => ToolStemClass::PlaceOnlyNeedsDescription {
            date_prefix: parts[0].to_string(),
            country: parts[1].to_string(),
            city: parts[2].to_string(),
        },
        _ => ToolStemClass::NotRecognized,
    }
}

/// True when the file should skip geocoding and earlier passes entirely (`classify_tool_stem` is
/// [`ToolStemClass::FullyNamed`]).
pub fn stem_matches_tool_naming_layout(stem: &str) -> bool {
    matches!(classify_tool_stem(stem), ToolStemClass::FullyNamed)
}

/// After stripping trailing `-<digits>`, if the stem is exactly `YYMM-Country-City-Description` with
/// a **4-digit** legacy `YYMM` first segment, returns `(yymm, country, city, description)`.
/// Stems with more than four segments (e.g. multi-part place names) return `None`.
pub fn parse_legacy_yymm_four_segment_stem(stem: &str) -> Option<(String, String, String, String)> {
    let mut parts: Vec<&str> = stem.split('-').filter(|s| !s.is_empty()).collect();
    while parts.len() > 1 && segment_is_ascii_digits(parts[parts.len() - 1]) {
        parts.pop();
    }
    if parts.len() != 4 {
        return None;
    }
    let yymm = parts[0];
    if yymm.len() != 4 || !yymm.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((
        yymm.to_string(),
        parts[1].to_string(),
        parts[2].to_string(),
        parts[3].to_string(),
    ))
}

/// Builds the filename stem: `YYMMDD-Country-City` or `…-Description` (legacy `YYMM-…` still works).
/// Description uses underscores for whitespace; country/city still use `-` for spaces.
pub fn build_stem(
    date_prefix: &str,
    country: &str,
    city: &str,
    description: Option<&str>,
) -> String {
    let c = sanitize_segment(country);
    let t = sanitize_segment(city);
    let mut base = format!("{date_prefix}-{c}-{t}");
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
    fn numeric_tail_is_not_fully_named_but_place_only_gets_description_pass() {
        assert!(!stem_matches_tool_naming_layout("2603-Finland-Helsinki-1"));
        assert!(!stem_matches_tool_naming_layout("2603-Finland-Helsinki-2"));
        assert_eq!(
            classify_tool_stem("2603-Finland-Helsinki-1"),
            ToolStemClass::PlaceOnlyNeedsDescription {
                date_prefix: "2603".into(),
                country: "Finland".into(),
                city: "Helsinki".into(),
            }
        );
        assert_eq!(
            classify_tool_stem("260314-Finland-Helsinki-1"),
            ToolStemClass::PlaceOnlyNeedsDescription {
                date_prefix: "260314".into(),
                country: "Finland".into(),
                city: "Helsinki".into(),
            }
        );
        // Description present; trailing `-2` is collision suffix only.
        assert!(stem_matches_tool_naming_layout("2504-US-NYC-beach-2"));
        assert_eq!(
            classify_tool_stem("2504-US-NYC-beach-2"),
            ToolStemClass::FullyNamed
        );
        assert!(stem_matches_tool_naming_layout("250415-US-NYC-beach-2"));
    }

    #[test]
    fn description_whitespace_becomes_underscore() {
        let s = build_stem("250400", "US", "NYC", Some("my beach trip"));
        assert_eq!(s, "250400-US-NYC-my_beach_trip");
        let s = build_stem("250400", "US", "NYC", Some("a\tb"));
        assert_eq!(s, "250400-US-NYC-a_b");
    }

    #[test]
    fn normalize_four_digit_prefix_appends_day_zero() {
        assert_eq!(normalize_date_prefix_for_stem("2603"), "260300");
        assert_eq!(normalize_date_prefix_for_stem("260314"), "260314");
    }

    #[test]
    fn parse_legacy_yymm_four_segment_stem_cases() {
        assert_eq!(
            parse_legacy_yymm_four_segment_stem("2603-Finland-Helsinki-trip"),
            Some((
                "2603".into(),
                "Finland".into(),
                "Helsinki".into(),
                "trip".into(),
            ))
        );
        assert_eq!(
            parse_legacy_yymm_four_segment_stem("2603-Finland-Helsinki-trip-2"),
            Some((
                "2603".into(),
                "Finland".into(),
                "Helsinki".into(),
                "trip".into(),
            ))
        );
        assert_eq!(
            parse_legacy_yymm_four_segment_stem("260314-Finland-Helsinki-trip"),
            None
        );
        assert_eq!(
            parse_legacy_yymm_four_segment_stem("2504-United-States-New-York-beach"),
            None
        );
    }
}
