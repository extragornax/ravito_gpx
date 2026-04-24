//! Minimal OSM `opening_hours` parser.
//!
//! Handles the common cases we actually see on ride routes:
//!   Mo-Fr 07:00-19:00
//!   Mo-Sa 07:00-13:00,15:00-19:30
//!   Mo-Fr 07:00-20:00; Sa 08:00-18:00; Su 08:00-12:00
//!   24/7
//!   closed / off
//!
//! It **does not** handle public-holiday rules (PH), week numbers, month
//! ranges, or sunrise/sunset offsets — those appear on <5% of relevant POIs
//! and we just report `unknown` for them rather than lying.
//!
//! The goal is a cheap answer to "is this bakery going to be open when I
//! roll past at 14:37 on a Tuesday?".

use chrono::{DateTime, Datelike, Timelike, Weekday};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Openness {
    Open,
    Closed,
    Unknown,
}

pub fn status_at<Tz: chrono::TimeZone>(spec: &str, at: &DateTime<Tz>) -> Openness {
    let s = spec.trim();
    if s.is_empty() {
        return Openness::Unknown;
    }
    let lc = s.to_lowercase();
    if lc == "24/7" {
        return Openness::Open;
    }
    if lc == "closed" || lc == "off" {
        return Openness::Closed;
    }

    let wd = at.weekday();
    let minute_of_day = at.hour() as i32 * 60 + at.minute() as i32;

    let mut any_rule_for_day = false;
    for rule in s.split(';') {
        let rule = rule.trim();
        if rule.is_empty() {
            continue;
        }
        let (days_part, time_part) = match split_days_times(rule) {
            Some(pair) => pair,
            None => return Openness::Unknown,
        };
        let Some(days) = parse_days(days_part) else {
            return Openness::Unknown;
        };
        if !days.contains(&wd) {
            continue;
        }
        any_rule_for_day = true;
        for interval in time_part.split(',') {
            let Some((from, to)) = parse_interval(interval.trim()) else {
                return Openness::Unknown;
            };
            if from <= minute_of_day && minute_of_day < to {
                return Openness::Open;
            }
        }
    }
    if any_rule_for_day {
        Openness::Closed
    } else {
        // No rule mentions this weekday at all — conservatively say closed.
        Openness::Closed
    }
}

fn split_days_times(rule: &str) -> Option<(&str, &str)> {
    // "Mo-Fr 07:00-19:00"  or  "07:00-19:00" (implicit every-day).
    let rule = rule.trim();
    if rule.chars().next()?.is_ascii_digit() {
        return Some(("Mo-Su", rule));
    }
    let idx = rule.find(|c: char| c.is_ascii_digit())?;
    let (d, t) = rule.split_at(idx);
    Some((d.trim(), t.trim()))
}

fn parse_days(s: &str) -> Option<Vec<Weekday>> {
    let mut out = Vec::new();
    for piece in s.split(',') {
        let piece = piece.trim();
        if let Some((a, b)) = piece.split_once('-') {
            let a = parse_day(a.trim())?;
            let b = parse_day(b.trim())?;
            let mut d = a;
            loop {
                out.push(d);
                if d == b {
                    break;
                }
                d = d.succ();
            }
        } else {
            out.push(parse_day(piece)?);
        }
    }
    Some(out)
}

fn parse_day(s: &str) -> Option<Weekday> {
    Some(match s {
        "Mo" => Weekday::Mon,
        "Tu" => Weekday::Tue,
        "We" => Weekday::Wed,
        "Th" => Weekday::Thu,
        "Fr" => Weekday::Fri,
        "Sa" => Weekday::Sat,
        "Su" => Weekday::Sun,
        _ => return None,
    })
}

fn parse_interval(s: &str) -> Option<(i32, i32)> {
    let (a, b) = s.split_once('-')?;
    Some((parse_hm(a.trim())?, parse_hm(b.trim())?))
}

fn parse_hm(s: &str) -> Option<i32> {
    let (h, m) = s.split_once(':')?;
    let h: i32 = h.parse().ok()?;
    let m: i32 = m.parse().ok()?;
    if !(0..=48).contains(&h) || !(0..=59).contains(&m) {
        return None;
    }
    Some(h * 60 + m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn t(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    #[test]
    fn open_weekday() {
        // 2026-04-24 is a Friday.
        let o = status_at("Mo-Fr 07:00-19:00", &t(2026, 4, 24, 10, 0));
        assert_eq!(o, Openness::Open);
    }

    #[test]
    fn closed_saturday() {
        // 2026-04-25 is a Saturday.
        let o = status_at("Mo-Fr 07:00-19:00", &t(2026, 4, 25, 10, 0));
        assert_eq!(o, Openness::Closed);
    }

    #[test]
    fn split_intervals() {
        let o = status_at("Mo-Sa 07:00-13:00,15:00-19:30", &t(2026, 4, 24, 14, 0));
        assert_eq!(o, Openness::Closed);
        let o = status_at("Mo-Sa 07:00-13:00,15:00-19:30", &t(2026, 4, 24, 15, 30));
        assert_eq!(o, Openness::Open);
    }

    #[test]
    fn always_open() {
        assert_eq!(status_at("24/7", &t(2026, 4, 24, 3, 0)), Openness::Open);
    }

    #[test]
    fn multi_rule() {
        // Fr 2026-04-24 at 18:00 — Mo-Fr rule should match.
        let spec = "Mo-Fr 07:00-20:00; Sa 08:00-18:00; Su 08:00-12:00";
        assert_eq!(status_at(spec, &t(2026, 4, 24, 18, 0)), Openness::Open);
        // Sa at 19:00 — Sa rule says closes 18:00.
        assert_eq!(status_at(spec, &t(2026, 4, 25, 19, 0)), Openness::Closed);
    }

    #[test]
    fn unknown_on_ph() {
        // We don't handle PH — report unknown instead of guessing.
        assert_eq!(
            status_at("PH off; Mo-Fr 08:00-12:00", &t(2026, 4, 24, 10, 0)),
            Openness::Unknown
        );
    }
}
