use serde::Deserialize;
use serde_json::Value;

/// Set to `1`, `true`, `yes`, or `on` to log each reverse-geocode: coordinates, chosen country/city,
/// which Geoapify field supplied the place name, and all candidate fields from the first result.
const ENV_GEOAPIFY_LOG: &str = "IMG_REVERSE_GEO_GEOAPIFY_LOG";
/// Set to `1`, `true`, `yes`, or `on` to print the full Geoapify JSON response (pretty-printed) to stderr.
const ENV_GEOAPIFY_JSON: &str = "IMG_REVERSE_GEO_GEOAPIFY_JSON";

#[derive(Debug, Deserialize)]
struct GeoapifyResponse {
    results: Vec<GeoResult>,
}

#[derive(Debug, Deserialize)]
struct GeoResult {
    country: Option<String>,
    city: Option<String>,
    name: Option<String>,
    county: Option<String>,
    formatted: Option<String>,
}

fn env_truthy(key: &str) -> bool {
    match std::env::var(key) {
        Ok(s) => {
            let t = s.trim().to_ascii_lowercase();
            matches!(t.as_str(), "1" | "true" | "yes" | "on")
        }
        Err(_) => false,
    }
}

fn pick_city(r: &GeoResult) -> (String, &'static str) {
    if let Some(ref c) = r.city {
        let t = c.trim().to_string();
        if !t.is_empty() {
            return (t, "city");
        }
    }
    if let Some(ref n) = r.name {
        let t = n.trim().to_string();
        if !t.is_empty() {
            return (t, "name");
        }
    }
    if let Some(ref c) = r.county {
        let t = c.trim().to_string();
        if !t.is_empty() {
            return (t, "county");
        }
    }
    if let Some(ref f) = r.formatted {
        let t = f.trim().to_string();
        if !t.is_empty() {
            return (t, "formatted");
        }
    }
    (String::new(), "none")
}

/// Reverse-geocode with Geoapify; returns `(country, city_or_fallback)`.
///
/// `file_label` is used only for debug logs (e.g. the file name).
pub fn reverse_geocode(
    lat: f64,
    lon: f64,
    api_key: &str,
    file_label: &str,
) -> Result<(String, String), String> {
    let log_details = env_truthy(ENV_GEOAPIFY_LOG);
    let dump_json = env_truthy(ENV_GEOAPIFY_JSON);

    let url = format!(
        "https://api.geoapify.com/v1/geocode/reverse?lat={lat}&lon={lon}&format=json&apiKey={api_key}"
    );

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;

    let resp = client.get(&url).send().map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        return Err(format!("Geoapify HTTP {}", resp.status()));
    }

    let body = resp.text().map_err(|e| e.to_string())?;

    if dump_json {
        match serde_json::from_str::<Value>(&body) {
            Ok(v) => eprintln!(
                "Geoapify JSON ({file_label}):\n{}",
                serde_json::to_string_pretty(&v).unwrap_or_else(|_| body.clone())
            ),
            Err(_) => eprintln!("Geoapify raw body ({file_label}):\n{body}"),
        }
    }

    let parsed: GeoapifyResponse = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let first = parsed
        .results
        .first()
        .ok_or_else(|| "Geoapify returned no results".to_string())?;

    let country = first.country.clone().unwrap_or_default().trim().to_string();
    if country.is_empty() {
        return Err("Geoapify result missing country".to_string());
    }

    let (city, city_source) = pick_city(first);
    if city.is_empty() {
        return Err("Geoapify result missing place name".to_string());
    }

    if log_details {
        eprintln!(
            "Geoapify ({file_label}): lat={lat} lon={lon} → country={country:?} city={city:?} (from `{city_source}`)"
        );
        eprintln!(
            "Geoapify ({file_label}) first-result fields: city={:?} name={:?} county={:?} formatted={:?}",
            first.city.as_deref(),
            first.name.as_deref(),
            first.county.as_deref(),
            first.formatted.as_deref(),
        );
    }

    Ok((country, city))
}
