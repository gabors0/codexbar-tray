//! Usage over time: one sample per window per live fetch, kept in `~/.cache/limitray/history.jsonl`
//! long enough to cover a weekly window. The popup draws them as a sparkline per window.

use crate::config::history_path;
use crate::model::{ProviderUsage, Snapshot, Window};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

/// Long enough to cover a weekly window plus a day.
const KEEP_SECS: i64 = 8 * 24 * 3600;
/// Points per window sent to the popup.
const MAX_POINTS: usize = 120;
/// Rewrite the file without expired lines after this many appends.
const COMPACT_EVERY: usize = 5000;

/// `(unix seconds, used percent)` samples per `"<provider>/<window key>"`.
pub type Series = HashMap<String, Vec<(i64, f32)>>;

#[derive(Serialize, Deserialize)]
struct Line {
    t: i64,
    k: String,
    u: f32,
}

pub fn key(provider: &str, w: &Window) -> String {
    format!("{provider}/{}", w.key)
}

#[derive(Default)]
pub struct History {
    series: Series,
    path: Option<PathBuf>,
    appended: usize,
}

impl History {
    pub fn load() -> Self {
        Self::load_from(history_path(), Utc::now().timestamp())
    }

    fn load_from(path: PathBuf, now: i64) -> Self {
        let mut h = History {
            path: Some(path),
            ..Default::default()
        };
        let Ok(text) = std::fs::read_to_string(h.path.as_ref().unwrap()) else {
            return h;
        };
        let mut lines = 0;
        for line in text.lines() {
            lines += 1;
            if let Ok(l) = serde_json::from_str::<Line>(line) {
                h.series.entry(l.k).or_default().push((l.t, l.u));
            }
        }
        for points in h.series.values_mut() {
            points.sort_by_key(|p| p.0);
        }
        h.prune(now);
        if h.len() < lines {
            h.compact();
        }
        h
    }

    fn len(&self) -> usize {
        self.series.values().map(Vec::len).sum()
    }

    fn prune(&mut self, now: i64) {
        for points in self.series.values_mut() {
            points.retain(|p| p.0 >= now - KEEP_SECS);
        }
        self.series.retain(|_, points| !points.is_empty());
    }

    fn compact(&mut self) {
        self.appended = 0;
        let Some(path) = &self.path else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let mut out = Vec::new();
        for (k, points) in &self.series {
            for &(t, u) in points {
                out.extend(serde_json::to_vec(&Line { t, k: k.clone(), u }).unwrap());
                out.push(b'\n');
            }
        }
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, out).is_ok() {
            let _ = std::fs::rename(tmp, path);
        }
    }

    /// Adds the windows of providers that fetched fine (not last-good values kept after an error).
    pub fn record(&mut self, providers: &[ProviderUsage], now: DateTime<Utc>) {
        let t = now.timestamp();
        let mut out = Vec::new();
        for p in providers.iter().filter(|p| p.error.is_none()) {
            for w in &p.windows {
                let k = key(&p.id, w);
                let u = w.used as f32;
                out.extend(serde_json::to_vec(&Line { t, k: k.clone(), u }).unwrap());
                out.push(b'\n');
                self.series.entry(k).or_default().push((t, u));
                self.appended += 1;
            }
        }
        self.prune(t);
        if self.appended >= COMPACT_EVERY {
            self.compact();
        } else if let Some(path) = &self.path {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = f.write_all(&out);
            }
        }
    }

    /// Samples inside each window's current period, thinned to at most `MAX_POINTS` each.
    pub fn for_snapshot(&self, snap: &Snapshot) -> Series {
        let mut out = Series::new();
        for p in &snap.providers {
            for w in &p.windows {
                let k = key(&p.id, w);
                let (Some(points), Some((start, end))) = (self.series.get(&k), bounds(w)) else {
                    continue;
                };
                let inside: Vec<(i64, f32)> =
                    points.iter().copied().filter(|p| p.0 >= start).collect();
                if inside.len() >= 2 {
                    out.insert(k, thin(&inside, start, end));
                }
            }
        }
        out
    }
}

/// Start and end of the window's current period, in unix seconds.
pub fn bounds(w: &Window) -> Option<(i64, i64)> {
    let end = w.resets_at?.timestamp();
    Some((end - w.minutes? as i64 * 60, end))
}

/// The highest sample per time bucket, so short spikes survive thinning.
fn thin(points: &[(i64, f32)], start: i64, end: i64) -> Vec<(i64, f32)> {
    if points.len() <= MAX_POINTS {
        return points.to_vec();
    }
    let width = ((end - start) / MAX_POINTS as i64).max(1);
    let mut out: Vec<(i64, f32)> = Vec::new();
    for &(t, u) in points {
        let bucket = (t - start) / width;
        match out.last_mut() {
            Some(last) if (last.0 - start) / width == bucket => {
                if u >= last.1 {
                    *last = (t, u);
                }
            }
            _ => out.push((t, u)),
        }
    }
    out
}

/// How far back the "recent rate" looks: a fifth of the window, between 30 minutes and a day.
fn lookback(w: &Window) -> i64 {
    (w.minutes.unwrap_or(300) as i64 * 60 / 5).clamp(30 * 60, 24 * 3600)
}

/// When the window hits 100% if usage keeps growing at its recent rate, if that is before the
/// reset. `points` are the window's samples, oldest first.
pub fn projected_full(points: &[(i64, f32)], w: &Window) -> Option<DateTime<Utc>> {
    let &(t1, u1) = points.last()?;
    let span = lookback(w);
    let &(t0, u0) = points.iter().find(|p| p.0 >= t1 - span)?;
    // Too little data, or no real growth: no projection.
    if t1 - t0 < span / 2 || u1 - u0 < 1.0 || u1 >= 100.0 {
        return None;
    }
    let rate = (u1 - u0) as f64 / (t1 - t0) as f64;
    let eta = t1 + ((100.0 - u1 as f64) / rate) as i64;
    let at = DateTime::from_timestamp(eta, 0)?;
    w.resets_at.is_none_or(|r| at < r).then_some(at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn session(used: f64, resets: i64) -> Window {
        Window {
            key: "primary".into(),
            label: "Session".into(),
            minutes: Some(300),
            used,
            resets_at: DateTime::from_timestamp(resets, 0),
            ..Default::default()
        }
    }

    fn provider(id: &str, windows: Vec<Window>, error: Option<&str>) -> ProviderUsage {
        ProviderUsage {
            id: id.into(),
            windows,
            error: error.map(str::to_string),
            ..Default::default()
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("limitray-history-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("history.jsonl")
    }

    #[test]
    fn records_reloads_and_drops_expired_samples() {
        let path = scratch("reload");
        let now = Utc.with_ymd_and_hms(2026, 10, 20, 12, 0, 0).unwrap();
        let mut h = History::load_from(path.clone(), now.timestamp());
        let old = now - chrono::Duration::days(9);
        h.record(&[provider("claude", vec![session(10.0, 0)], None)], old);
        h.record(
            &[
                provider("claude", vec![session(20.0, 0)], None),
                provider("codex", vec![session(50.0, 0)], Some("auth expired")),
            ],
            now,
        );
        let h = History::load_from(path.clone(), now.timestamp());
        assert_eq!(h.series.len(), 1, "failed providers are not recorded");
        assert_eq!(h.series["claude/primary"], vec![(now.timestamp(), 20.0)]);
        // The expired line was compacted out of the file.
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 1);
    }

    #[test]
    fn sends_only_the_current_period() {
        let now = 1_000_000;
        let w = session(30.0, now + 3600);
        let mut h = History::default();
        h.series.insert(
            "claude/primary".into(),
            // The first sample belongs to the previous 5h window.
            vec![(now - 5 * 3600, 90.0), (now - 3600, 10.0), (now, 30.0)],
        );
        let snap = Snapshot {
            providers: vec![provider("claude", vec![w], None)],
            ..Default::default()
        };
        let out = h.for_snapshot(&snap);
        assert_eq!(out["claude/primary"], vec![(now - 3600, 10.0), (now, 30.0)]);
    }

    #[test]
    fn thins_long_series_keeping_peaks() {
        let points: Vec<(i64, f32)> = (0..2016).map(|i| (i * 300, (i % 7) as f32)).collect();
        let out = thin(&points, 0, 2016 * 300);
        assert!(out.len() <= MAX_POINTS + 1);
        assert!(out.iter().all(|p| p.1 == 6.0));
    }

    #[test]
    fn projects_the_limit_from_the_recent_rate() {
        let now = 1_000_000;
        // 10% per 30 minutes, 60% now: 100% in two more hours, before the reset in three.
        let w = session(60.0, now + 3 * 3600);
        let points = [(now - 3600, 40.0), (now - 1800, 50.0), (now, 60.0)];
        let at = projected_full(&points, &w).unwrap();
        assert_eq!(at.timestamp(), now + 2 * 3600);

        // The reset comes first: nothing to warn about.
        let w = session(60.0, now + 3600);
        assert!(projected_full(&points, &w).is_none());

        // Flat usage and too-short histories project nothing.
        let flat = [(now - 3600, 60.0), (now, 60.0)];
        assert!(projected_full(&flat, &session(60.0, now + 3 * 3600)).is_none());
        let short = [(now - 300, 50.0), (now, 60.0)];
        assert!(projected_full(&short, &session(60.0, now + 3 * 3600)).is_none());
    }
}
