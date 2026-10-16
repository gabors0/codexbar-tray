use crate::config::Config;
use crate::model::{parse_costs, parse_usage, CostSummary, ProviderUsage};
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(60);
const MAX_OUTPUT: usize = 8 * 1024 * 1024;

/// After Claude OAuth fails (e.g. Anthropic rate-limits the usage endpoint), skip it until this
/// instant and let the CLI's own fallback answer, then try the fast path again.
static CLAUDE_OAUTH_RETRY_AT: Mutex<Option<Instant>> = Mutex::new(None);
const OAUTH_COOLDOWN: Duration = Duration::from_secs(15 * 60);

fn oauth_allowed() -> bool {
    CLAUDE_OAUTH_RETRY_AT
        .lock()
        .unwrap()
        .is_none_or(|t| Instant::now() >= t)
}

fn oauth_failed() {
    *CLAUDE_OAUTH_RETRY_AT.lock().unwrap() = Some(Instant::now() + OAUTH_COOLDOWN);
}

fn run(cfg: &Config, args: &[&str]) -> Result<String, String> {
    let exe = cfg.resolved_cli();
    let mut child = Command::new(&exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("Cannot run {}: {e}", exe.display()))?;
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = (&mut stdout)
            .take(MAX_OUTPUT as u64 + 1)
            .read_to_end(&mut buf);
        buf
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() > TIMEOUT => {
                unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
                let _ = child.wait();
                return Err("CodexBar CLI timed out".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => return Err(e.to_string()),
        }
    }
    let out = reader.join().unwrap_or_default();
    if out.len() > MAX_OUTPUT {
        return Err("CLI output too large".into());
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

fn usage_args<'a>(provider: &'a str, source: Option<&'a str>, status: bool) -> Vec<&'a str> {
    let mut args = vec![
        "usage",
        "--format",
        "json",
        "--json-only",
        "--provider",
        provider,
    ];
    if let Some(s) = source {
        args.extend(["--source", s]);
    }
    if status {
        args.push("--status");
    }
    args
}

pub fn fetch_provider(cfg: &Config, provider: &str) -> Vec<ProviderUsage> {
    let explicit = (cfg.source != "auto").then_some(cfg.source.as_str());
    // The CLI's own "auto" for Claude is web → claude CLI (spawns a PTY, ~9s). The OAuth endpoint
    // answers in well under a second, so try it first and only fall back when it fails.
    if provider == "claude" && explicit.is_none() && oauth_allowed() {
        match run(cfg, &usage_args(provider, Some("oauth"), cfg.show_status))
            .and_then(|o| parse_usage(&o))
        {
            Ok(rows) if rows.iter().all(|r| r.error.is_none()) => return rows,
            _ => oauth_failed(),
        }
    }
    match run(cfg, &usage_args(provider, explicit, cfg.show_status)).and_then(|o| parse_usage(&o)) {
        Ok(rows) => rows,
        Err(error) => vec![ProviderUsage {
            id: provider.to_string(),
            error: Some(error),
            ..Default::default()
        }],
    }
}

/// Re-arm the OAuth fast path (e.g. after the user logs in again).
pub fn reset_fast_path() {
    *CLAUDE_OAUTH_RETRY_AT.lock().unwrap() = None;
}

pub fn fetch_costs(cfg: &Config) -> Result<Vec<CostSummary>, String> {
    let has = |p: &str| cfg.providers.iter().any(|x| x == p);
    let which = match (has("claude"), has("codex")) {
        (true, true) => "both",
        (true, false) => "claude",
        (false, true) => "codex",
        (false, false) => return Ok(Vec::new()),
    };
    run(
        cfg,
        &[
            "cost",
            "--provider",
            which,
            "--format",
            "json",
            "--json-only",
            "--days",
            "30",
        ],
    )
    .and_then(|o| parse_costs(&o))
}
