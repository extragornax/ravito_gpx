use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::gpx_parse::RawPoint;

/// How long we trust a cached Overpass response before re-fetching.
/// POIs and their opening hours don't change hour-to-hour, so a week is fine
/// and it keeps repeat scrubbing instant even for long rides.
const CACHE_TTL_SECS: i64 = 7 * 24 * 3600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PoiKind {
    Bakery,
    Supermarket,
    Tobacco,
    Fountain,
    Other,
}

impl PoiKind {
    pub fn from_tags(tags: &serde_json::Map<String, serde_json::Value>) -> Self {
        let amenity = tags.get("amenity").and_then(|v| v.as_str()).unwrap_or("");
        let shop = tags.get("shop").and_then(|v| v.as_str()).unwrap_or("");
        if amenity == "drinking_water" || shop == "water" {
            return PoiKind::Fountain;
        }
        if amenity == "bar" || amenity == "cafe" || amenity == "pub" {
            return PoiKind::Tobacco;
        }
        if shop == "convenience" || shop == "supermarket" {
            return PoiKind::Supermarket;
        }
        if shop == "bakery" || shop == "pastry" {
            return PoiKind::Bakery;
        }
        if shop == "tobacco" {
            return PoiKind::Tobacco;
        }
        PoiKind::Other
    }
    pub fn as_str(self) -> &'static str {
        match self {
            PoiKind::Bakery => "bakery",
            PoiKind::Supermarket => "supermarket",
            PoiKind::Tobacco => "bar_tabac",
            PoiKind::Fountain => "fountain",
            PoiKind::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Poi {
    pub osm_id: i64,
    pub kind: PoiKind,
    pub lat: f64,
    pub lon: f64,
    pub name: Option<String>,
    pub opening_hours: Option<String>,
}

pub struct OverpassCache {
    conn: Mutex<Connection>,
    http: reqwest::Client,
    url: String,
}

impl OverpassCache {
    pub fn open(path: &str, url: String) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("open sqlite at {path}"))?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS bbox_cache (
                bbox_key TEXT PRIMARY KEY,
                payload TEXT NOT NULL,
                fetched_at INTEGER NOT NULL
            );
            "#,
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
            http: reqwest::Client::builder()
                .user_agent("ravito_gpx/0.1 (+https://ravito.extragornax.fr)")
                .timeout(std::time::Duration::from_secs(60))
                .build()?,
            url,
        })
    }

    /// Fetch (or load cached) POIs inside the bounding box that contains the
    /// whole route, padded by `corridor_m`. The filtering to the actual route
    /// corridor happens later in the handler.
    pub async fn pois_near_route(&self, raw: &[RawPoint], corridor_m: f64) -> Result<Vec<Poi>> {
        let (min_lat, min_lon, max_lat, max_lon) = bbox(raw);
        // Inflate the bbox by enough degrees to cover the corridor.
        let pad_lat = corridor_m / 111_000.0;
        let pad_lon = corridor_m
            / (111_000.0 * ((min_lat + max_lat) / 2.0).to_radians().cos().max(0.1));
        let bbox_str = format!(
            "{:.4},{:.4},{:.4},{:.4}",
            min_lat - pad_lat,
            min_lon - pad_lon,
            max_lat + pad_lat,
            max_lon + pad_lon
        );
        let key = format!("v1|{}", bbox_str);

        let now = chrono::Utc::now().timestamp();
        if let Some(payload) = self.read_cache(&key, now)? {
            return parse_overpass(&payload);
        }

        let query = format!(
            r#"[out:json][timeout:50];
(
  nwr["shop"~"^(bakery|pastry|supermarket|convenience|tobacco)$"]({bbox});
  nwr["amenity"~"^(bar|cafe|pub|drinking_water)$"]({bbox});
);
out center tags;"#,
            bbox = bbox_str
        );
        let resp = self
            .http
            .post(&self.url)
            .body(query)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        self.write_cache(&key, &resp, now)?;
        parse_overpass(&resp)
    }

    fn read_cache(&self, key: &str, now: i64) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let row: Option<(String, i64)> = conn
            .query_row(
                "SELECT payload, fetched_at FROM bbox_cache WHERE bbox_key=?",
                params![key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(row.and_then(|(p, t)| {
            if now - t < CACHE_TTL_SECS {
                Some(p)
            } else {
                None
            }
        }))
    }

    fn write_cache(&self, key: &str, payload: &str, now: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO bbox_cache (bbox_key, payload, fetched_at)
             VALUES (?, ?, ?)",
            params![key, payload, now],
        )?;
        Ok(())
    }
}

fn bbox(raw: &[RawPoint]) -> (f64, f64, f64, f64) {
    let mut min_lat = f64::INFINITY;
    let mut min_lon = f64::INFINITY;
    let mut max_lat = f64::NEG_INFINITY;
    let mut max_lon = f64::NEG_INFINITY;
    for p in raw {
        if p.lat < min_lat { min_lat = p.lat; }
        if p.lon < min_lon { min_lon = p.lon; }
        if p.lat > max_lat { max_lat = p.lat; }
        if p.lon > max_lon { max_lon = p.lon; }
    }
    (min_lat, min_lon, max_lat, max_lon)
}

fn parse_overpass(body: &str) -> Result<Vec<Poi>> {
    let v: serde_json::Value = serde_json::from_str(body)?;
    let elements = v.get("elements").and_then(|e| e.as_array());
    let Some(elements) = elements else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(elements.len());
    for el in elements {
        let (lat, lon) = match (el.get("lat"), el.get("lon")) {
            (Some(a), Some(b)) => (a.as_f64(), b.as_f64()),
            _ => {
                let c = el.get("center");
                (
                    c.and_then(|c| c.get("lat")).and_then(|v| v.as_f64()),
                    c.and_then(|c| c.get("lon")).and_then(|v| v.as_f64()),
                )
            }
        };
        let (Some(lat), Some(lon)) = (lat, lon) else { continue };
        let osm_id = el.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
        let empty = serde_json::Map::new();
        let tags = el
            .get("tags")
            .and_then(|t| t.as_object())
            .unwrap_or(&empty);
        let kind = PoiKind::from_tags(tags);
        let name = tags.get("name").and_then(|v| v.as_str()).map(String::from);
        let opening_hours = tags
            .get("opening_hours")
            .and_then(|v| v.as_str())
            .map(String::from);
        out.push(Poi {
            osm_id,
            kind,
            lat,
            lon,
            name,
            opening_hours,
        });
    }
    Ok(out)
}
