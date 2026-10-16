use chrono::{DateTime, Local, Utc};

pub fn duration(secs: i64) -> String {
    let m = (secs.max(0) + 59) / 60;
    if m < 60 {
        format!("{m}m")
    } else if m < 1440 {
        format!("{}h {:02}m", m / 60, m % 60)
    } else {
        format!("{}d {}h", m / 1440, (m % 1440) / 60)
    }
}

pub fn resets_in(at: Option<DateTime<Utc>>) -> String {
    match at {
        None => "reset time unknown".into(),
        Some(t) => {
            let secs = (t - Utc::now()).num_seconds();
            if secs <= 0 {
                "reset due".into()
            } else {
                format!("resets in {}", duration(secs))
            }
        }
    }
}

/// "19:20" today, "Thu 10:00" this week, "Oct 23 10:00" otherwise.
pub fn clock(at: DateTime<Utc>) -> String {
    let local = at.with_timezone(&Local);
    let now = Local::now();
    let days = (local.date_naive() - now.date_naive()).num_days();
    if days == 0 {
        local.format("%H:%M").to_string()
    } else if (1..7).contains(&days) {
        local.format("%a %H:%M").to_string()
    } else {
        local.format("%b %-d %H:%M").to_string()
    }
}

pub fn ago(at: Option<DateTime<Utc>>) -> String {
    match at {
        None => "never".into(),
        Some(t) => {
            let s = (Utc::now() - t).num_seconds();
            if s < 45 {
                "just now".into()
            } else if s < 3600 {
                format!("{}m ago", (s + 30) / 60)
            } else if s < 86400 {
                format!("{}h ago", s / 3600)
            } else {
                format!("{}d ago", s / 86400)
            }
        }
    }
}

pub fn money(v: f64) -> String {
    if v >= 1000.0 {
        format!("${:.0}", v)
    } else if v >= 100.0 {
        format!("${:.1}", v)
    } else {
        format!("${:.2}", v)
    }
}

pub fn tokens(v: u64) -> String {
    let f = v as f64;
    if f >= 1e9 {
        format!("{:.2}B", f / 1e9)
    } else if f >= 1e6 {
        format!("{:.1}M", f / 1e6)
    } else if f >= 1e3 {
        format!("{:.1}K", f / 1e3)
    } else {
        v.to_string()
    }
}

/// "claude-opus-5-5" → "Opus 5.5"
pub fn model_name(raw: &str) -> String {
    let s = raw.strip_prefix("claude-").unwrap_or(raw);
    let mut parts = s.split('-');
    let Some(family) = parts.next() else {
        return raw.into();
    };
    let version: Vec<&str> = parts
        .take_while(|p| p.chars().all(|c| c.is_ascii_digit()) && p.len() <= 2)
        .collect();
    let mut name = family
        .chars()
        .next()
        .map(|c| c.to_uppercase().collect::<String>() + &family[1..])
        .unwrap_or_default();
    if !version.is_empty() {
        name.push(' ');
        name.push_str(&version.join("."));
    }
    name
}

#[cfg(test)]
mod tests {
    #[test]
    fn formats() {
        assert_eq!(super::duration(3600 + 12 * 60), "1h 12m");
        assert_eq!(super::duration(5 * 86400 + 3 * 3600), "5d 3h");
        assert_eq!(super::model_name("claude-opus-5-5"), "Opus 5.5");
        assert_eq!(super::model_name("claude-sonnet-4-20250514"), "Sonnet 4");
        assert_eq!(super::tokens(730_035_192), "730.0M");
    }
}
