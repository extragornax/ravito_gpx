use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::gpx_parse::{KmSample, cumulative_km, parse_track, project_to_route, sample_by_km};
use crate::hours::{Openness, status_at};
use crate::overpass::OverpassCache;

#[derive(Clone)]
pub struct AppState {
    pub cache: Arc<OverpassCache>,
}

pub async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

pub async fn app_css() -> Response {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], APP_CSS).into_response()
}

#[derive(Deserialize)]
pub struct AnalyzeReq {
    pub gpx: String,
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub speed_kmh: Option<f64>,
    /// How far off the route (in metres) a POI is still considered "on the way".
    /// Default 120 m: a reasonable detour for a bakery, avoids cluttering the
    /// list with the 400-m-away shops on parallel streets.
    #[serde(default)]
    pub corridor_m: Option<f64>,
    #[serde(default)]
    pub kinds: Option<Vec<String>>,
}

#[derive(Serialize)]
pub struct PoiOut {
    pub km: f64,
    pub detour_m: f64,
    pub kind: String,
    pub lat: f64,
    pub lon: f64,
    pub name: Option<String>,
    pub opening_hours: Option<String>,
    /// "open", "closed", or "unknown" at the rider's ETA.
    pub status_at_eta: String,
    pub eta_unix: i64,
}

#[derive(Serialize)]
pub struct AnalyzeResp {
    pub total_km: f64,
    pub start_unix: i64,
    pub speed_kmh: f64,
    pub corridor_m: f64,
    pub pois: Vec<PoiOut>,
    pub route: Vec<KmSample>,
}

pub async fn analyze(
    State(state): State<AppState>,
    Json(req): Json<AnalyzeReq>,
) -> Result<Json<AnalyzeResp>, (StatusCode, String)> {
    let raw = parse_track(std::io::Cursor::new(req.gpx.as_bytes()))
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("gpx: {e}")))?;

    let cum = cumulative_km(&raw);
    let total_km = *cum.last().unwrap_or(&0.0);
    let route_samples = sample_by_km(&raw, 1.0);
    let start = match &req.start {
        Some(s) => DateTime::parse_from_rfc3339(s)
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("start: {e}")))?
            .with_timezone(&Utc),
        None => Utc::now(),
    };
    let speed = req.speed_kmh.unwrap_or(22.0).max(5.0);
    let corridor_m = req.corridor_m.unwrap_or(120.0).clamp(20.0, 2_000.0);

    let kinds_filter: Option<std::collections::HashSet<String>> = req
        .kinds
        .as_ref()
        .map(|ks| ks.iter().map(|s| s.to_lowercase()).collect());

    let pois = state
        .cache
        .pois_near_route(&raw, corridor_m)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("overpass: {e}")))?;

    let mut out = Vec::new();
    for p in pois {
        let (km, detour) = project_to_route(&raw, &cum, p.lat, p.lon);
        if detour > corridor_m {
            continue;
        }
        let kind_s = p.kind.as_str().to_string();
        if let Some(f) = &kinds_filter
            && !f.contains(&kind_s)
        {
            continue;
        }
        let eta = start + Duration::seconds(((km / speed) * 3600.0) as i64);
        let status = match &p.opening_hours {
            Some(h) => match status_at(h, &eta) {
                Openness::Open => "open",
                Openness::Closed => "closed",
                Openness::Unknown => "unknown",
            },
            None => "unknown",
        };
        out.push(PoiOut {
            km,
            detour_m: detour,
            kind: kind_s,
            lat: p.lat,
            lon: p.lon,
            name: p.name.clone(),
            opening_hours: p.opening_hours.clone(),
            status_at_eta: status.to_string(),
            eta_unix: eta.timestamp(),
        });
    }
    out.sort_by(|a, b| a.km.partial_cmp(&b.km).unwrap_or(std::cmp::Ordering::Equal));

    Ok(Json(AnalyzeResp {
        total_km,
        start_unix: start.timestamp(),
        speed_kmh: speed,
        corridor_m,
        pois: out,
        route: route_samples,
    }))
}

const INDEX_HTML: &str = include_str!("../static/index.html");
const APP_CSS: &str = include_str!("../static/app.css");
