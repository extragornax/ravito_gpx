use std::io::BufReader;

use anyhow::{Context, Result, bail};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct RawPoint {
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct KmSample {
    pub km: f64,
    pub lat: f64,
    pub lon: f64,
}

pub fn parse_track(reader: impl std::io::Read) -> Result<Vec<RawPoint>> {
    let g = gpx::read(BufReader::new(reader)).context("failed to parse GPX")?;
    let mut pts = Vec::new();
    for track in g.tracks {
        for seg in track.segments {
            for p in seg.points {
                let pt = p.point();
                pts.push(RawPoint { lat: pt.y(), lon: pt.x() });
            }
        }
    }
    if pts.is_empty() {
        for route in g.routes {
            for p in route.points {
                let pt = p.point();
                pts.push(RawPoint { lat: pt.y(), lon: pt.x() });
            }
        }
    }
    if pts.len() < 2 {
        bail!("GPX has no usable track points");
    }
    Ok(pts)
}

pub fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let r = 6371.0_f64;
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());
    r * c
}

/// Return the cumulative distance (km) of each raw point along the track.
pub fn cumulative_km(raw: &[RawPoint]) -> Vec<f64> {
    let mut cum = Vec::with_capacity(raw.len());
    cum.push(0.0);
    for i in 1..raw.len() {
        let d = haversine_km(raw[i - 1].lat, raw[i - 1].lon, raw[i].lat, raw[i].lon);
        cum.push(cum[i - 1] + d);
    }
    cum
}

pub fn sample_by_km(raw: &[RawPoint], step_km: f64) -> Vec<KmSample> {
    let cum = cumulative_km(raw);
    let total = *cum.last().unwrap_or(&0.0);
    let mut out = Vec::new();
    let mut target = 0.0;
    let mut seg = 1;
    while target <= total + 1e-9 {
        while seg < cum.len() && cum[seg] < target {
            seg += 1;
        }
        let (lat, lon) = if seg >= cum.len() {
            let last = raw.last().unwrap();
            (last.lat, last.lon)
        } else {
            let a = &raw[seg - 1];
            let b = &raw[seg];
            let span = (cum[seg] - cum[seg - 1]).max(1e-9);
            let t = ((target - cum[seg - 1]) / span).clamp(0.0, 1.0);
            (a.lat + (b.lat - a.lat) * t, a.lon + (b.lon - a.lon) * t)
        };
        out.push(KmSample { km: target, lat, lon });
        target += step_km;
    }
    if let Some(last) = out.last()
        && (total - last.km).abs() > 1e-6
    {
        let l = raw.last().unwrap();
        out.push(KmSample { km: total, lat: l.lat, lon: l.lon });
    }
    out
}

/// Closest-point projection of a POI onto the polyline, returning (km, distance_m).
/// km is where on the route the POI sits; distance_m is how far off the route it is.
pub fn project_to_route(raw: &[RawPoint], cum: &[f64], plat: f64, plon: f64) -> (f64, f64) {
    let mut best_km = 0.0;
    let mut best_d = f64::INFINITY;
    for i in 0..raw.len() - 1 {
        let (t, d_km) = point_to_segment_km(plat, plon, &raw[i], &raw[i + 1]);
        if d_km < best_d {
            best_d = d_km;
            let seg_len = cum[i + 1] - cum[i];
            best_km = cum[i] + t * seg_len;
        }
    }
    (best_km, best_d * 1000.0)
}

/// Project (plat, plon) onto the geodesic segment a→b using an equirectangular
/// approximation (accurate enough at the scales we care about — a few km corridor).
/// Returns (t in [0,1], perpendicular distance in km).
fn point_to_segment_km(plat: f64, plon: f64, a: &RawPoint, b: &RawPoint) -> (f64, f64) {
    let mid_lat = ((a.lat + b.lat) / 2.0).to_radians();
    let kx = 111.320 * mid_lat.cos();
    let ky = 110.574;

    let ax = a.lon * kx;
    let ay = a.lat * ky;
    let bx = b.lon * kx;
    let by = b.lat * ky;
    let px = plon * kx;
    let py = plat * ky;

    let dx = bx - ax;
    let dy = by - ay;
    let len2 = dx * dx + dy * dy;
    let t = if len2 < 1e-12 {
        0.0
    } else {
        (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0)
    };
    let cx = ax + t * dx;
    let cy = ay + t * dy;
    let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
    (t, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn haversine_sanity() {
        let d = haversine_km(48.8566, 2.3522, 51.5074, -0.1278);
        assert!(d > 300.0 && d < 400.0);
    }

    #[test]
    fn project_on_route() {
        let raw = vec![
            RawPoint { lat: 48.0, lon: 2.0 },
            RawPoint { lat: 48.0, lon: 2.1 },
        ];
        let cum = cumulative_km(&raw);
        // A point very close to the midpoint.
        let (km, d_m) = project_to_route(&raw, &cum, 48.001, 2.05);
        let total = *cum.last().unwrap();
        assert!(km > total * 0.4 && km < total * 0.6);
        assert!(d_m < 200.0);
    }
}
