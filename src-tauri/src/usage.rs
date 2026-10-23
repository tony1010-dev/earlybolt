//! Subscription usage per service, read with the credentials each official CLI already keeps.
//! Read-only: nothing here sends a prompt or spends quota.

use crate::platform;
use chrono::DateTime;
use serde::Serialize;
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Serialize, Clone)]
pub struct Window {
    /// "5h" | "week" | "month"
    pub kind: String,
    /// Model group when a service meters several separately (Antigravity: "Gemini", "Claude/GPT")
    pub group: Option<String>,
    pub used: f64,
    pub resets_at: Option<i64>,
}

#[derive(Serialize, Clone, Default)]
pub struct Usage {
    pub windows: Vec<Window>,
    /// "stale": the saved sign-in expired, running the CLI renews it / "signed_out" / anything else is an error message
    pub error: Option<String>,
    pub at: i64,
}

fn win(kind: &str, group: Option<&str>, used: f64, resets_at: Option<i64>) -> Window {
    Window { kind: kind.into(), group: group.map(String::from), used: used.clamp(0.0, 100.0), resets_at }
}

fn iso(v: &Value) -> Option<i64> {
    DateTime::parse_from_rfc3339(v.as_str()?).ok().map(|d| d.timestamp())
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn get(url: &str, headers: &[(&str, &str)]) -> Result<Value, String> {
    let mut req = ureq::get(url).timeout(TIMEOUT);
    for (k, v) in headers {
        req = req.set(k, v);
    }
    match req.call() {
        Ok(r) => r.into_json().map_err(|e| e.to_string()),
        Err(ureq::Error::Status(401 | 403, _)) => Err("stale".into()),
        Err(ureq::Error::Status(code, _)) => Err(format!("HTTP {code}")),
        Err(e) => Err(e.to_string()),
    }
}

/// Muse Code has no usage API yet (only a web dashboard), so it is pinged but not shown here
pub fn supported(id: &str) -> bool {
    matches!(id, "claude" | "codex" | "grok" | "agy" | "kimi")
}

pub fn fetch(id: &str, path: &str, cwd: &Path) -> Usage {
    let r = match id {
        "claude" => claude(),
        "codex" => codex(),
        "grok" => grok(),
        "agy" => agy(path, cwd),
        "kimi" => kimi(),
        _ => Err("unsupported".into()),
    };
    let at = chrono::Local::now().timestamp();
    match r {
        Ok(windows) => Usage { windows, error: None, at },
        Err(e) => Usage { windows: vec![], error: Some(e), at },
    }
}

/// Claude Code keeps its sign-in in the macOS Keychain, or in ~/.claude/.credentials.json elsewhere
fn claude_token() -> Option<String> {
    let raw = if cfg!(target_os = "macos") {
        let out = platform::command("/usr/bin/security")
            .args(["find-generic-password", "-s", "Claude Code-credentials", "-w"])
            .output().ok()?;
        String::from_utf8(out.stdout).ok()?
    } else {
        std::fs::read_to_string(platform::home().join(".claude").join(".credentials.json")).ok()?
    };
    let v: Value = serde_json::from_str(raw.trim()).ok()?;
    v["claudeAiOauth"]["accessToken"].as_str().map(String::from)
}

fn claude() -> Result<Vec<Window>, String> {
    let token = claude_token().ok_or("signed_out")?;
    let v = get("https://api.anthropic.com/api/oauth/usage", &[
        ("Authorization", &format!("Bearer {token}")),
        ("anthropic-beta", "oauth-2025-04-20"),
        ("User-Agent", "claude-code/2.1.0"),
    ])?;
    Ok([("five_hour", "5h"), ("seven_day", "week")].iter()
        .filter_map(|(key, kind)| {
            let w = &v[key];
            Some(win(kind, None, w["utilization"].as_f64()?, iso(&w["resets_at"])))
        })
        .collect())
}

fn codex() -> Result<Vec<Window>, String> {
    let home = std::env::var("CODEX_HOME").map(Into::into).unwrap_or_else(|_| platform::home().join(".codex"));
    let auth = read_json(&home.join("auth.json")).ok_or("signed_out")?;
    let token = auth["tokens"]["access_token"].as_str().ok_or("signed_out")?;
    let bearer = format!("Bearer {token}");
    let mut headers = vec![("Authorization", bearer.as_str()), ("User-Agent", "codex-cli"), ("OpenAI-Beta", "codex-1")];
    if let Some(acc) = auth["tokens"]["account_id"].as_str() {
        headers.push(("ChatGPT-Account-Id", acc));
    }
    let v = get("https://chatgpt.com/backend-api/wham/usage", &headers)?;
    Ok(["primary_window", "secondary_window"].iter()
        .filter_map(|key| {
            let w = &v["rate_limit"][key];
            let secs = w["limit_window_seconds"].as_i64().unwrap_or(0);
            let kind = if secs > 0 && secs <= 6 * 3600 { "5h" } else { "week" };
            Some(win(kind, None, w["used_percent"].as_f64()?, w["reset_at"].as_i64()))
        })
        .collect())
}

fn grok() -> Result<Vec<Window>, String> {
    let auth = read_json(&platform::home().join(".grok").join("auth.json")).ok_or("signed_out")?;
    let session = auth.as_object()
        .and_then(|o| o.iter().find(|(k, _)| k.starts_with("https://auth.x.ai")).or_else(|| o.iter().next()))
        .map(|(_, v)| v.clone())
        .ok_or("signed_out")?;
    let token = session["key"].as_str().ok_or("signed_out")?;
    // The CLI renews its short-lived token when it runs (every ping does)
    if iso(&session["expires_at"]).is_some_and(|t| t < chrono::Utc::now().timestamp() + 60) {
        return Err("stale".into());
    }
    let bearer = format!("Bearer {token}");
    let mut headers = vec![("Authorization", bearer.as_str()), ("X-XAI-Token-Auth", "xai-grok-cli"), ("Accept", "application/json")];
    if let Some(uid) = session["user_id"].as_str() {
        headers.push(("x-userid", uid));
    }
    let v = get("https://cli-chat-proxy.grok.com/v1/billing?format=credits", &headers)?;
    let c = if v["config"].is_object() { &v["config"] } else { &v };
    let used = c["creditUsagePercent"].as_f64().ok_or("no usage reported")?;
    let weekly = c["currentPeriod"]["type"].as_str() == Some("USAGE_PERIOD_TYPE_WEEKLY");
    let end = iso(&c["currentPeriod"]["end"]).or_else(|| iso(&c["billingPeriodEnd"]));
    Ok(vec![win(if weekly { "week" } else { "month" }, None, used, end)])
}

/// `agy -p /usage` is a built-in command answered locally (num_turns 0), not a model call
fn agy(path: &str, cwd: &Path) -> Result<Vec<Window>, String> {
    let argv: Vec<String> = ["agy", "-p", "/usage", "--output-format", "json", "--print-timeout", "20s"]
        .iter().map(|s| s.to_string()).collect();
    let (_, out) = platform::run_tool(&argv, path, cwd, Duration::from_secs(40));
    let v = out.lines().map(str::trim).filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v["command"]["data"]["groups"].is_array());
    let Some(v) = v else {
        let low = out.to_lowercase();
        return Err(if low.contains("not logged in") || low.contains("unauthenticated") || low.contains("sign in") {
            "signed_out".into()
        } else {
            "no usage reported".into()
        });
    };
    let mut windows = vec![];
    for g in v["command"]["data"]["groups"].as_array().into_iter().flatten() {
        let name = g["name"].as_str().unwrap_or("");
        let group = if name.starts_with("Gemini") { "Gemini" } else if name.starts_with("Claude") { "Claude/GPT" } else { name };
        for b in g["buckets"].as_array().into_iter().flatten() {
            let Some(left) = b["remaining_fraction"].as_f64() else { continue };
            let kind = if b["window"].as_str() == Some("weekly") { "week" } else { "5h" };
            windows.push(win(kind, Some(group), (1.0 - left) * 100.0, iso(&b["reset_time"])));
        }
    }
    Ok(windows)
}

pub fn kimi_home() -> std::path::PathBuf {
    std::env::var("KIMI_CODE_HOME").ok().filter(|s| !s.trim().is_empty()).map(Into::into)
        .unwrap_or_else(|| platform::home().join(".kimi-code"))
}

fn kimi() -> Result<Vec<Window>, String> {
    let cred = read_json(&kimi_home().join("credentials").join("kimi-code.json")).ok_or("signed_out")?;
    let token = cred["access_token"].as_str().filter(|t| !t.is_empty()).ok_or("signed_out")?;
    // The CLI renews its token when it runs
    if cred["expires_at"].as_i64().is_some_and(|t| t < chrono::Utc::now().timestamp() + 5) {
        return Err("stale".into());
    }
    let v = get("https://api.kimi.com/coding/v1/usages", &[
        ("Authorization", &format!("Bearer {token}")),
        ("Accept", "application/json"),
    ])?;
    let num = |v: &Value| v.as_f64().or_else(|| v.as_str()?.parse().ok());
    // {limit, used | remaining, resetTime}
    let percent = |d: &Value| -> Option<(f64, Option<i64>)> {
        let limit = num(&d["limit"]).filter(|l| *l > 0.0)?;
        let used = num(&d["used"]).or_else(|| Some(limit - num(&d["remaining"])?))?;
        let reset = if d["resetTime"].is_null() { &d["resetAt"] } else { &d["resetTime"] };
        Some((used / limit * 100.0, iso(reset)))
    };
    let mut windows = vec![];
    for l in v["limits"].as_array().into_iter().flatten() {
        let Some((used, reset)) = percent(&l["detail"]) else { continue };
        let n = num(&l["window"]["duration"]).unwrap_or(300.0);
        let unit = l["window"]["timeUnit"].as_str().unwrap_or("").to_uppercase();
        let minutes = if unit.contains("HOUR") { n * 60.0 } else if unit.contains("DAY") { n * 1440.0 } else if unit.contains("SECOND") { n / 60.0 } else { n };
        windows.push(win(if minutes <= 360.0 { "5h" } else { "week" }, None, used, reset));
    }
    // The top-level allowance is the weekly credit pool
    if let Some((used, reset)) = percent(&v["usage"]) {
        windows.push(win("week", None, used, reset));
    }
    if windows.is_empty() {
        return Err("no usage reported".into());
    }
    Ok(windows)
}
