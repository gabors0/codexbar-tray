use chrono::{DateTime, Local, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Pace {
    pub summary: String,
    pub stage: String,
    pub expected_used: Option<f64>,
    pub delta: Option<f64>,
    pub will_last: Option<bool>,
    pub eta_secs: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Window {
    pub key: String,
    pub label: String,
    pub minutes: Option<u32>,
    pub used: f64,
    pub resets_at: Option<DateTime<Utc>>,
    pub pace: Option<Pace>,
    pub scoped: bool,
}

impl Window {
    pub fn is_session(&self) -> bool {
        matches!(self.minutes, Some(m) if (60..=720).contains(&m)) && !self.scoped
    }
    pub fn is_weekly(&self) -> bool {
        self.minutes == Some(10080) && !self.scoped
    }
    pub fn cadence(&self) -> String {
        match self.minutes {
            Some(m) if m >= 1440 => format!("{}d", (m as f64 / 1440.0).round() as u32),
            Some(m) if m >= 60 && m % 60 == 0 => format!("{}h", m / 60),
            Some(m) if m > 0 => format!("{m}m"),
            _ => String::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Status {
    pub description: String,
    pub indicator: String,
    pub url: String,
}

impl Status {
    pub fn is_ok(&self) -> bool {
        self.indicator.is_empty() || self.indicator == "none"
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ExtraUsage {
    pub used: Option<f64>,
    pub limit: Option<f64>,
    pub currency: String,
    pub period: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ProviderUsage {
    pub id: String,
    pub plan: Option<String>,
    pub account: Option<String>,
    pub source: String,
    pub windows: Vec<Window>,
    pub status: Option<Status>,
    pub error: Option<String>,
    pub credits: Option<f64>,
    pub extra: Option<ExtraUsage>,
    pub fetched_at: Option<DateTime<Utc>>,
}

impl ProviderUsage {
    pub fn name(&self) -> String {
        provider_name(&self.id)
    }
    pub fn session(&self) -> Option<&Window> {
        self.windows
            .iter()
            .find(|w| w.is_session())
            .or_else(|| self.windows.iter().find(|w| !w.scoped))
    }
    pub fn weekly(&self) -> Option<&Window> {
        self.windows.iter().find(|w| w.is_weekly())
    }
    /// The window closest to running out, ignoring per-model caps unless they bind harder.
    pub fn tightest(&self) -> Option<&Window> {
        self.windows.iter().max_by(|a, b| a.used.total_cmp(&b.used))
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct DailyCost {
    pub date: NaiveDate,
    pub cost: f64,
    pub tokens: u64,
    pub models: Vec<(String, f64)>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct CostSummary {
    pub provider: String,
    pub today: f64,
    pub today_tokens: u64,
    pub last30: f64,
    pub tokens30: u64,
    pub estimated: bool,
    pub daily: Vec<DailyCost>,
    pub models: Vec<(String, f64)>,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateTarget {
    /// limitray itself.
    App,
    /// The CodexBar CLI that limitray installed.
    Cli,
}

/// `Snapshot::update_status` while a manual update check runs.
pub const UPDATE_CHECKING: &str = "Checking for updates…";

/// A newer release of the app or the CLI, and how installing it is going.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct UpdateOffer {
    pub version: String,
    pub notes_url: String,
    pub installing: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Snapshot {
    pub providers: Vec<ProviderUsage>,
    pub costs: Vec<CostSummary>,
    pub updated_at: Option<DateTime<Utc>>,
    pub busy: bool,
    pub error: Option<String>,
    /// True while showing data restored from the disk cache before the first live fetch.
    pub from_cache: bool,
    /// Transient app-level message, e.g. "Downloading CodexBar CLI…".
    #[serde(default)]
    pub notice: Option<String>,
    #[serde(default)]
    pub update: Option<UpdateOffer>,
    #[serde(default)]
    pub cli_update: Option<UpdateOffer>,
    /// Outcome of the last manual update check, e.g. "Up to date · checked 14:02".
    #[serde(default)]
    pub update_status: Option<String>,
}

impl Snapshot {
    pub fn provider(&self, id: &str) -> Option<&ProviderUsage> {
        self.providers.iter().find(|p| p.id == id)
    }
    pub fn cost(&self, id: &str) -> Option<&CostSummary> {
        self.costs.iter().find(|c| c.provider == id)
    }
    pub fn age_secs(&self) -> Option<i64> {
        self.updated_at.map(|t| (Utc::now() - t).num_seconds())
    }
}

pub fn provider_name(id: &str) -> String {
    match id {
        "claude" => "Claude".into(),
        "codex" => "Codex".into(),
        "copilot" => "GitHub Copilot".into(),
        "gemini" => "Gemini".into(),
        "cursor" => "Cursor".into(),
        "openrouter" => "OpenRouter".into(),
        "zai" => "Z.ai".into(),
        "opencode" => "OpenCode".into(),
        "antigravity" => "Antigravity".into(),
        "kiro" => "Kiro".into(),
        "xai" => "xAI".into(),
        "deepseek" => "DeepSeek".into(),
        "minimax" => "MiniMax".into(),
        _ => {
            let mut c = id.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        }
    }
}

// ---------- parsing the CodexBar CLI JSON ----------

fn num(v: &Value) -> Option<f64> {
    v.as_f64().filter(|n| n.is_finite())
}

fn text(v: &Value) -> Option<String> {
    v.as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(300).collect())
}

fn time(v: &Value) -> Option<DateTime<Utc>> {
    v.as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
}

fn parse_pace(v: &Value) -> Option<Pace> {
    if !v.is_object() {
        return None;
    }
    Some(Pace {
        summary: text(&v["summary"]).unwrap_or_default(),
        stage: text(&v["stage"]).unwrap_or_default(),
        expected_used: num(&v["expectedUsedPercent"]),
        delta: num(&v["deltaPercent"]),
        will_last: v["willLastToReset"].as_bool(),
        eta_secs: num(&v["etaSeconds"]),
    })
}

fn parse_window(
    key: &str,
    label: String,
    w: &Value,
    pace: Option<Pace>,
    scoped: bool,
) -> Option<Window> {
    if w["isSyntheticPlaceholder"].as_bool() == Some(true) {
        return None;
    }
    let used = num(&w["usedPercent"])?.clamp(0.0, 100.0);
    Some(Window {
        key: key.to_string(),
        label,
        minutes: w["windowMinutes"]
            .as_u64()
            .map(|m| m as u32)
            .filter(|m| *m > 0),
        used,
        resets_at: time(&w["resetsAt"]),
        pace,
        scoped,
    })
}

fn default_label(minutes: Option<u64>, index: usize) -> String {
    match minutes {
        Some(m) if (60..=720).contains(&m) => "Session".into(),
        Some(10080) => "Weekly".into(),
        Some(m) if m >= 1440 => format!("{} days", m / 1440),
        Some(m) if m > 0 => format!("{} hours", m / 60),
        _ => ["Session", "Weekly", "Additional"]
            .get(index)
            .unwrap_or(&"Limit")
            .to_string(),
    }
}

pub fn parse_usage(json: &str) -> Result<Vec<ProviderUsage>, String> {
    let decoded: Value =
        serde_json::from_str(json.trim()).map_err(|e| format!("Invalid CLI output: {e}"))?;
    let entries = match decoded {
        Value::Array(a) => a,
        v @ Value::Object(_) => vec![v],
        _ => return Err("Unexpected CLI output".into()),
    };
    let now = Utc::now();
    let mut out = Vec::new();
    for entry in entries {
        let Some(id) = text(&entry["provider"]) else {
            continue;
        };
        let usage = &entry["usage"];
        let identity = &usage["identity"];
        let mut windows = Vec::new();
        for (index, key) in ["primary", "secondary", "tertiary"].iter().enumerate() {
            let w = &usage[*key];
            if !w.is_object() {
                continue;
            }
            let label = text(&entry["rateWindowLabels"][*key])
                .unwrap_or_else(|| default_label(w["windowMinutes"].as_u64(), index));
            if let Some(win) = parse_window(key, label, w, parse_pace(&entry["pace"][*key]), false)
            {
                windows.push(win);
            }
        }
        if let Some(extras) = usage["extraRateWindows"].as_array() {
            for extra in extras.iter().take(8) {
                if extra["usageKnown"].as_bool() == Some(false) {
                    continue;
                }
                let Some(eid) = text(&extra["id"]) else {
                    continue;
                };
                let label = text(&extra["title"]).unwrap_or_else(|| "Additional".into());
                if let Some(win) =
                    parse_window(&format!("extra:{eid}"), label, &extra["window"], None, true)
                {
                    windows.push(win);
                }
            }
        }
        let error = if entry["error"].is_object() {
            Some(text(&entry["error"]["message"]).unwrap_or_else(|| "Usage unavailable".into()))
        } else {
            text(&entry["error"])
        };
        let status = entry["status"].is_object().then(|| Status {
            description: text(&entry["status"]["description"]).unwrap_or_default(),
            indicator: text(&entry["status"]["indicator"]).unwrap_or_default(),
            url: text(&entry["status"]["url"]).unwrap_or_default(),
        });
        let cost = &usage["providerCost"];
        let extra = cost
            .is_object()
            .then(|| ExtraUsage {
                used: num(&cost["used"]),
                limit: num(&cost["limit"]),
                currency: text(&cost["currencyCode"]).unwrap_or_else(|| "USD".into()),
                period: text(&cost["period"]).unwrap_or_default(),
            })
            .filter(|e| e.used.is_some() || e.limit.is_some());
        out.push(ProviderUsage {
            plan: text(&identity["loginMethod"])
                .or_else(|| text(&usage["loginMethod"]))
                .map(clean_plan),
            account: text(&identity["accountEmail"]).or_else(|| text(&usage["accountEmail"])),
            source: text(&entry["source"]).unwrap_or_default(),
            credits: num(&entry["credits"]["remaining"]),
            error: error.or_else(|| {
                windows
                    .is_empty()
                    .then(|| "No quota windows reported".to_string())
            }),
            windows,
            status,
            extra,
            fetched_at: Some(now),
            id,
        });
    }
    if out.is_empty() {
        return Err("CLI returned no providers".into());
    }
    Ok(out)
}

fn clean_plan(plan: String) -> String {
    plan.strip_prefix("Claude ")
        .map(str::to_string)
        .unwrap_or(plan)
}

pub fn parse_costs(json: &str) -> Result<Vec<CostSummary>, String> {
    let decoded: Value =
        serde_json::from_str(json.trim()).map_err(|e| format!("Invalid cost output: {e}"))?;
    let rows = decoded.as_array().ok_or("Unexpected cost output")?;
    let today = Local::now().date_naive();
    let mut out = Vec::new();
    for row in rows {
        let Some(provider) = text(&row["provider"]) else {
            continue;
        };
        let mut daily: Vec<DailyCost> = row["daily"]
            .as_array()
            .map(|days| {
                days.iter()
                    .filter_map(|d| {
                        Some(DailyCost {
                            date: NaiveDate::parse_from_str(d["date"].as_str()?, "%Y-%m-%d")
                                .ok()?,
                            cost: num(&d["totalCost"]).unwrap_or(0.0),
                            tokens: d["totalTokens"].as_u64().unwrap_or(0),
                            models: d["modelBreakdowns"]
                                .as_array()
                                .map(|m| {
                                    m.iter()
                                        .filter_map(|b| {
                                            Some((
                                                text(&b["modelName"])?,
                                                num(&b["cost"]).unwrap_or(0.0),
                                            ))
                                        })
                                        .collect()
                                })
                                .unwrap_or_default(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        daily.sort_by_key(|d| d.date);
        let mut models: Vec<(String, f64)> = Vec::new();
        for day in &daily {
            for (name, cost) in &day.models {
                match models.iter_mut().find(|(n, _)| n == name) {
                    Some(m) => m.1 += cost,
                    None => models.push((name.clone(), *cost)),
                }
            }
        }
        models.sort_by(|a, b| b.1.total_cmp(&a.1));
        let today_row = daily.iter().find(|d| d.date == today);
        out.push(CostSummary {
            today: today_row.map(|d| d.cost).unwrap_or(0.0),
            today_tokens: today_row.map(|d| d.tokens).unwrap_or(0),
            last30: num(&row["last30DaysCostUSD"])
                .unwrap_or_else(|| daily.iter().map(|d| d.cost).sum()),
            tokens30: row["last30DaysTokens"]
                .as_u64()
                .unwrap_or_else(|| daily.iter().map(|d| d.tokens).sum()),
            estimated: row["provenance"].as_str() != Some("actual"),
            error: row["error"]
                .is_object()
                .then(|| "Local cost history unavailable".to_string()),
            daily,
            models,
            provider,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_claude_oauth() {
        let json = r#"[{"rateWindowLabels":{"primary":"Session","secondary":"Weekly"},"usage":{"secondary":{"windowMinutes":10080,"resetsAt":"2026-10-23T08:00:00Z","usedPercent":7},"loginMethod":"Claude Pro","primary":{"windowMinutes":300,"resetsAt":"2026-10-16T17:20:00Z","usedPercent":24},"tertiary":null},"pace":{"primary":{"stage":"farBehind","summary":"64% in reserve","expectedUsedPercent":88,"willLastToReset":true,"deltaPercent":-64}},"source":"oauth","status":{"description":"All Systems Operational","indicator":"none"},"provider":"claude"}]"#;
        let p = &parse_usage(json).unwrap()[0];
        assert_eq!(p.plan.as_deref(), Some("Pro"));
        assert_eq!(p.windows.len(), 2);
        assert_eq!(p.session().unwrap().used, 24.0);
        assert_eq!(p.weekly().unwrap().label, "Weekly");
        assert_eq!(
            p.session().unwrap().pace.as_ref().unwrap().expected_used,
            Some(88.0)
        );
        assert!(p.error.is_none());
        assert!(p.status.as_ref().unwrap().is_ok());
    }

    #[test]
    fn parses_error_entries() {
        let json = r#"[{"error":{"code":1,"message":"Codex auth.json needs refresh.","kind":"provider"},"provider":"codex","source":"auto"}]"#;
        let p = &parse_usage(json).unwrap()[0];
        assert_eq!(p.error.as_deref(), Some("Codex auth.json needs refresh."));
    }
}
