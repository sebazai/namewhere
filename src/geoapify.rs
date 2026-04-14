use serde::Deserialize;

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

fn pick_city(r: &GeoResult) -> String {
    r.city
        .clone()
        .or_else(|| r.name.clone())
        .or_else(|| r.county.clone())
        .or_else(|| r.formatted.clone())
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Reverse-geocode with Geoapify; returns `(country, city_or_fallback)`.
pub fn reverse_geocode(lat: f64, lon: f64, api_key: &str) -> Result<(String, String), String> {
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

    let body: GeoapifyResponse = resp.json().map_err(|e| e.to_string())?;
    let first = body
        .results
        .first()
        .ok_or_else(|| "Geoapify returned no results".to_string())?;

    let country = first.country.clone().unwrap_or_default().trim().to_string();
    if country.is_empty() {
        return Err("Geoapify result missing country".to_string());
    }

    let city = pick_city(first);
    if city.is_empty() {
        return Err("Geoapify result missing place name".to_string());
    }

    Ok((country, city))
}
