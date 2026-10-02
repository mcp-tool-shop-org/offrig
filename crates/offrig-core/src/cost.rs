//! Money and idleness: what the running pod has cost so far, and when an idle pod
//! should be stopped.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::remote::GpuStat;

/// Unix seconds from RunPod's timestamps. The API returns Go's default format
/// (`2026-10-02 15:55:07.106 +0000 UTC`); its schema examples use ISO 8601
/// (`2024-07-12T19:14:40.144Z`). Both are accepted.
pub fn parse_timestamp(s: &str) -> Option<i64> {
    let s = s.trim();
    let num = |a: usize, b: usize| -> Option<i64> { s.get(a..b)?.parse().ok() };
    if !matches!(s.as_bytes().get(10), Some(b' ' | b'T')) {
        return None;
    }
    let (y, m, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hh, mm, ss) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    let rest = &s[19..];
    let rest = rest.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    let offset = if rest.starts_with('Z') || rest.is_empty() {
        0
    } else {
        let o = rest.trim_start();
        let sign = match o.as_bytes().first()? {
            b'+' => 1,
            b'-' => -1,
            _ => return None,
        };
        let digits: String = o[1..]
            .chars()
            .filter(char::is_ascii_digit)
            .take(4)
            .collect();
        if digits.len() != 4 {
            return None;
        }
        let h: i64 = digits[..2].parse().ok()?;
        let mi: i64 = digits[2..].parse().ok()?;
        sign * (h * 3600 + mi * 60)
    };
    Some(days_from_civil(y, m, d) * 86_400 + hh * 3600 + mm * 60 + ss - offset)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `YYYY-MM-DD` (UTC) for unix seconds; the inverse of `days_from_civil`.
pub fn date_utc(unix: i64) -> String {
    let z = unix.div_euclid(86_400) + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Spend since the pod last started, at its hourly rate.
pub fn session_cost(cost_per_hr: f64, started_unix: i64, now_unix: i64) -> f64 {
    let secs = (now_unix - started_unix).max(0);
    cost_per_hr * secs as f64 / 3600.0
}

/// Network volume storage: $0.07/GB/month for the first TB, $0.05 beyond (RunPod docs).
pub fn volume_cost_per_month(size_gb: u32) -> f64 {
    let first = f64::from(size_gb.min(1000));
    let rest = f64::from(size_gb.saturating_sub(1000));
    first * 0.07 + rest * 0.05
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Idle {
    Busy,
    /// Idle for this long, under the limit.
    Idle(Duration),
    /// Idle past the limit: stop the pod.
    Stop,
    /// No GPU readings; never a reason to stop.
    Unknown,
}

/// Stops a pod whose every GPU has stayed under `busy_pct` for `limit`.
pub struct IdleTracker {
    pub busy_pct: u32,
    pub limit: Duration,
    idle_since: Option<Instant>,
}

impl IdleTracker {
    pub fn new(limit: Duration) -> Self {
        Self {
            busy_pct: 5,
            limit,
            idle_since: None,
        }
    }

    pub fn observe(&mut self, stats: &[GpuStat], now: Instant) -> Idle {
        if stats.is_empty() {
            return Idle::Unknown;
        }
        if stats.iter().any(|g| g.util_pct >= self.busy_pct) {
            self.idle_since = None;
            return Idle::Busy;
        }
        let since = *self.idle_since.get_or_insert(now);
        let idle = now.saturating_duration_since(since);
        if idle >= self.limit {
            Idle::Stop
        } else {
            Idle::Idle(idle)
        }
    }

    /// Activity seen elsewhere (e.g. a chat request) also counts as busy.
    pub fn touch(&mut self) {
        self.idle_since = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_runpod_timestamp_formats() {
        assert_eq!(parse_timestamp("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_timestamp("2000-01-01T00:00:00Z"), Some(946_684_800));
        assert_eq!(
            parse_timestamp("2024-07-12T19:14:40.144Z"),
            Some(1_720_811_680)
        );
        assert_eq!(
            parse_timestamp("2026-10-02 15:55:07.106 +0000 UTC"),
            parse_timestamp("2026-10-02T15:55:07Z")
        );
        assert_eq!(
            parse_timestamp("2026-10-02 17:55:07 +0200 CEST"),
            parse_timestamp("2026-10-02T15:55:07Z")
        );
        assert_eq!(parse_timestamp("2024-02-29T12:00:00Z"), Some(1_709_208_000));
    }

    #[test]
    fn date_utc_inverts_parse() {
        assert_eq!(date_utc(0), "1970-01-01");
        assert_eq!(date_utc(946_684_800), "2000-01-01");
        assert_eq!(date_utc(1_709_208_000), "2024-02-29");
        for s in [
            "2026-10-02T00:00:00Z",
            "1999-12-31T23:59:59Z",
            "2100-03-01T12:00:00Z",
        ] {
            let t = parse_timestamp(s).expect("valid");
            assert_eq!(date_utc(t), &s[..10]);
        }
    }

    #[test]
    fn rejects_garbage() {
        for bad in [
            "",
            "yesterday",
            "2026-13-01T00:00:00Z",
            "2026-10-02X15:55:07Z",
            "2026-10-02 15:55:07 +00 UTC",
        ] {
            assert_eq!(parse_timestamp(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn cost_math() {
        assert!((session_cost(2.0, 0, 5400) - 3.0).abs() < 1e-9);
        assert_eq!(session_cost(2.0, 100, 50), 0.0);
        assert!((volume_cost_per_month(400) - 28.0).abs() < 1e-9);
        assert!((volume_cost_per_month(1500) - 95.0).abs() < 1e-9);
    }

    fn gpu(util: u32) -> GpuStat {
        GpuStat {
            index: 0,
            name: "g".into(),
            util_pct: util,
            mem_used_mb: 0,
            mem_total_mb: 1,
        }
    }

    #[test]
    fn idle_tracker_stops_only_after_a_full_quiet_window() {
        let t0 = Instant::now();
        let mut t = IdleTracker::new(Duration::from_secs(600));
        assert_eq!(t.observe(&[gpu(0), gpu(1)], t0), Idle::Idle(Duration::ZERO));
        assert_eq!(
            t.observe(&[gpu(0)], t0 + Duration::from_secs(300)),
            Idle::Idle(Duration::from_secs(300))
        );
        assert_eq!(
            t.observe(&[gpu(0), gpu(40)], t0 + Duration::from_secs(400)),
            Idle::Busy
        );
        assert!(matches!(
            t.observe(&[gpu(0)], t0 + Duration::from_secs(900)),
            Idle::Idle(_)
        ));
        assert_eq!(
            t.observe(&[gpu(0)], t0 + Duration::from_secs(1500)),
            Idle::Stop
        );
        assert_eq!(
            t.observe(&[], t0 + Duration::from_secs(9999)),
            Idle::Unknown
        );
        t.touch();
        assert_eq!(
            t.observe(&[gpu(0)], t0 + Duration::from_secs(1600)),
            Idle::Idle(Duration::ZERO)
        );
    }
}
