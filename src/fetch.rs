use crate::config::Config;
use crate::model::{parse_costs, parse_usage, CostSummary, ProviderUsage};
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(60);
const MAX_OUTPUT: usize = 8 * 1024 * 1024;
const MAX_STDERR: usize = 64 * 1024;

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
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("Cannot run {}: {e}", exe.display()))?;
    let reader = read_limited(child.stdout.take().unwrap(), MAX_OUTPUT as u64 + 1);
    let err_reader = read_tail(child.stderr.take().unwrap(), MAX_STDERR);
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() > TIMEOUT => {
                unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
                let _ = child.wait();
                return Err("CodexBar CLI timed out".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let out = reader.join().unwrap_or_default();
    if out.len() > MAX_OUTPUT {
        return Err("CLI output too large".into());
    }
    // Provider errors arrive as JSON on stdout; only an empty stdout means the CLI itself failed.
    if out.iter().all(u8::is_ascii_whitespace) {
        let err = err_reader.wait(Duration::from_millis(500));
        return Err(failure_message(status, &err));
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

fn read_limited(pipe: impl Read + Send + 'static, limit: u64) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.take(limit).read_to_end(&mut buf);
        buf
    })
}

/// Drains a pipe in the background (so the child never blocks on it), keeping the last bytes.
struct Tail {
    buf: Arc<Mutex<Vec<u8>>>,
    keep: usize,
    done: Receiver<()>,
}

fn read_tail(mut pipe: impl Read + Send + 'static, keep: usize) -> Tail {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let (tx, done) = channel();
    let shared = buf.clone();
    std::thread::spawn(move || {
        let _tx = tx; // dropped at EOF, which wakes `Tail::wait`
        let mut chunk = [0u8; 8192];
        while let Ok(n @ 1..) = pipe.read(&mut chunk) {
            let mut buf = shared.lock().unwrap();
            buf.extend_from_slice(&chunk[..n]);
            if buf.len() > 2 * keep {
                let excess = buf.len() - keep;
                buf.drain(..excess);
            }
        }
    });
    Tail { buf, keep, done }
}

impl Tail {
    /// What was read by EOF, or by `grace` if something (e.g. a lingering grandchild of the
    /// CLI) still holds the pipe open.
    fn wait(self, grace: Duration) -> Vec<u8> {
        let _ = self.done.recv_timeout(grace);
        let buf = self.buf.lock().unwrap();
        buf[buf.len().saturating_sub(self.keep)..].to_vec()
    }
}

/// The last non-empty stderr line (where CLIs put the actual error), else the exit status.
fn failure_message(status: ExitStatus, stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    match stderr.lines().map(str::trim).rfind(|l| !l.is_empty()) {
        Some(line) => format!(
            "CodexBar CLI: {}",
            line.chars().take(300).collect::<String>()
        ),
        None if status.success() => "CodexBar CLI returned no output".into(),
        None => format!("CodexBar CLI failed ({status})"),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::ExitStatusExt;

    /// Writing a script while another test thread forks can make its exec fail with ETXTBSY,
    /// so tests that spawn a fake CLI take turns.
    static SPAWN: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A stand-in CodexBar CLI: a shell script with the given body.
    fn fake_cli(name: &str, body: &str) -> Config {
        let dir = std::env::temp_dir().join(format!("limitray-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        Config {
            cli: path.display().to_string(),
            ..Config::default()
        }
    }

    #[test]
    fn reports_the_cli_error_line() {
        let _turn = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
        let cfg = fake_cli(
            "fails",
            "echo 'Fetching…' >&2; echo 'Error: Claude credentials expired' >&2; echo >&2; exit 1",
        );
        assert_eq!(
            run(&cfg, &["usage"]),
            Err("CodexBar CLI: Error: Claude credentials expired".into())
        );
    }

    #[test]
    fn reports_the_exit_status_without_stderr() {
        let _turn = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
        let cfg = fake_cli("silent", "exit 3");
        assert_eq!(
            run(&cfg, &["usage"]),
            Err("CodexBar CLI failed (exit status: 3)".into())
        );
    }

    #[test]
    fn stdout_wins_over_stderr_and_exit_code() {
        let _turn = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
        // Provider errors come back as JSON on stdout, possibly with a non-zero exit.
        let cfg = fake_cli("json", "echo 'warning' >&2; echo '[]'; exit 1");
        assert_eq!(run(&cfg, &["usage"]).unwrap().trim(), "[]");
    }

    #[test]
    fn noisy_stderr_does_not_block_the_cli() {
        let _turn = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
        // Far more than a pipe buffer: the child would hang if stderr weren't drained.
        let cfg = fake_cli(
            "noisy",
            "head -c 1000000 /dev/zero | tr '\\0' x >&2; echo >&2; echo 'last words' >&2; exit 1",
        );
        let start = Instant::now();
        assert_eq!(
            run(&cfg, &["usage"]),
            Err("CodexBar CLI: last words".into())
        );
        assert!(start.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn lingering_grandchild_does_not_hang_on_stderr() {
        let _turn = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
        // A helper that outlives the CLI and keeps its stderr open.
        let cfg = fake_cli(
            "lingers",
            "sleep 5 >/dev/null & echo 'Error: helper failed' >&2; exit 1",
        );
        let start = Instant::now();
        assert_eq!(
            run(&cfg, &["usage"]),
            Err("CodexBar CLI: Error: helper failed".into())
        );
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn read_tail_keeps_the_end() {
        let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        let tail = read_tail(std::io::Cursor::new(data.clone()), 1000).wait(Duration::from_secs(5));
        assert_eq!(tail, data[data.len() - 1000..]);
        let short =
            read_tail(std::io::Cursor::new(b"abc".to_vec()), 1000).wait(Duration::from_secs(5));
        assert_eq!(short, b"abc");
    }

    #[test]
    fn failure_message_truncates_long_lines() {
        let long = "e".repeat(1000);
        let msg = failure_message(ExitStatus::from_raw(256), long.as_bytes());
        assert_eq!(msg.chars().count(), "CodexBar CLI: ".len() + 300);
        assert_eq!(
            failure_message(ExitStatus::from_raw(0), b""),
            "CodexBar CLI returned no output"
        );
    }
}
