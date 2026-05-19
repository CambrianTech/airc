use std::env;
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};

use crate::gh_state::{
    append_audit, command_class, cwd, now_seconds, record_backoff, reserve_guarded_request,
    safe_args, split_include_output,
};

const MESSAGES_FILE: &str = "messages.jsonl";
const DEFAULT_GIST_MAX_BYTES: usize = 600_000;
const DEFAULT_GIST_KEEP_LINES: usize = 1000;
const GH_API_TIMEOUT_DETAIL: &str = "gh api command failed";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum SendKind {
    Delivered,
    AuthFailure,
    TransientFailure,
    SecondaryRateLimit,
    Gone,
}

#[derive(Debug, Serialize)]
struct SendOutcome {
    kind: SendKind,
    detail: String,
}

impl SendOutcome {
    fn new(kind: SendKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    fn delivered() -> Self {
        Self::new(SendKind::Delivered, "")
    }
}

pub fn run_send(
    _peer_id: &str,
    _channel: &str,
    _host_target: Option<&str>,
    _identity_key: Option<&str>,
    _remote_home: Option<&str>,
    room_gist_id: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    let payloads = read_payloads(false)?;
    let outcome = send_payloads(room_gist_id, &payloads);
    println!("{}", serde_json::to_string(&outcome)?);
    Ok(())
}

pub fn run_send_batch(
    _peer_id: &str,
    _channel: &str,
    _host_target: Option<&str>,
    _identity_key: Option<&str>,
    _remote_home: Option<&str>,
    room_gist_id: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    let payloads = read_payloads(true)?;
    let outcome = send_payloads(room_gist_id, &payloads);
    println!("{}", serde_json::to_string(&outcome)?);
    Ok(())
}

fn read_payloads(batch: bool) -> Result<Vec<String>, Box<dyn Error>> {
    let mut stdin = String::new();
    io::stdin().read_to_string(&mut stdin)?;
    let payloads = if batch {
        stdin
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(frame_line)
            .collect()
    } else {
        vec![frame_line(&stdin)]
    };
    Ok(payloads)
}

fn send_payloads(room_gist_id: Option<&str>, payloads: &[String]) -> SendOutcome {
    let Some(gist_id) = room_gist_id.filter(|gist| !gist.trim().is_empty()) else {
        return SendOutcome::new(
            SendKind::TransientFailure,
            "resolver error: no registered bearer can serve peer",
        );
    };
    if payloads.is_empty() {
        return SendOutcome::new(SendKind::Delivered, "0 payload(s)");
    }
    if payloads.iter().any(|payload| payload.contains('\0')) {
        return SendOutcome::new(
            SendKind::TransientFailure,
            "payload contains NUL; gh-bearer requires text envelopes",
        );
    }

    let (local_ok, local_detail) = append_local_bus_batch(gist_id, payloads);
    let batch_content = payloads.concat();
    let retries = 8;
    let mut last_detail = String::new();

    for attempt in 0..retries {
        let (gist, get_kind) = gh_api_get_classified(gist_id);
        let Some(gist) = gist else {
            if local_ok
                && matches!(
                    get_kind,
                    SendKind::SecondaryRateLimit
                        | SendKind::TransientFailure
                        | SendKind::AuthFailure
                )
            {
                return SendOutcome::new(
                    SendKind::Delivered,
                    format!(
                        "delivered{} via local bus; gh publish deferred ({})",
                        batch_suffix(payloads),
                        kind_name(get_kind)
                    ),
                );
            }
            if matches!(
                get_kind,
                SendKind::Gone | SendKind::SecondaryRateLimit | SendKind::AuthFailure
            ) {
                return SendOutcome::new(
                    get_kind,
                    format!("GET gists/{gist_id} failed: {}", kind_name(get_kind)),
                );
            }
            return SendOutcome::new(
                SendKind::TransientFailure,
                format!("could not fetch gist {gist_id} (network/5xx/timeout)"),
            );
        };

        let existing = read_messages_content(&gist);
        let new_content = rotate_if_needed(&(rotate_if_needed(&existing) + &batch_content));
        let (ok, detail, patch_kind) = gh_api_patch_classified(gist_id, &new_content);
        if !ok {
            last_detail = detail;
            if patch_kind == PatchKind::Conflict {
                sleep_jittered_backoff(attempt);
                continue;
            }
            let send_kind = patch_kind.into_send_kind();
            if local_ok
                && matches!(
                    send_kind,
                    SendKind::SecondaryRateLimit
                        | SendKind::TransientFailure
                        | SendKind::AuthFailure
                )
            {
                return SendOutcome::new(
                    SendKind::Delivered,
                    format!(
                        "delivered{} via local bus; gh publish deferred ({})",
                        batch_suffix(payloads),
                        kind_name(send_kind)
                    ),
                );
            }
            return SendOutcome::new(send_kind, last_detail);
        }

        let (verify, _) = gh_api_get_classified(gist_id);
        let Some(verify) = verify else {
            return delivered_detail(payloads);
        };
        let content = read_messages_content(&verify);
        if payloads
            .iter()
            .all(|payload| content.contains(payload.trim_end()))
        {
            return delivered_detail(payloads);
        }
        last_detail = if payloads.len() == 1 {
            "verify-after-write: line not in gist post-PATCH".to_string()
        } else {
            "verify-after-write: one or more batch lines missing post-PATCH".to_string()
        };
        sleep_jittered_backoff(attempt);
    }

    if local_ok {
        return SendOutcome::new(
            SendKind::Delivered,
            format!(
                "delivered{} via local bus; gh conflict after {retries} retries; last: {last_detail}",
                batch_suffix(payloads)
            ),
        );
    }
    SendOutcome::new(
        SendKind::TransientFailure,
        format!(
            "{} conflict after {retries} retries; last: {last_detail}; local bus: {local_detail}",
            if payloads.len() == 1 {
                "concurrent-write"
            } else {
                "batch"
            }
        ),
    )
}

fn delivered_detail(payloads: &[String]) -> SendOutcome {
    if payloads.len() == 1 {
        SendOutcome::delivered()
    } else {
        SendOutcome::new(
            SendKind::Delivered,
            format!("{} payload(s)", payloads.len()),
        )
    }
}

fn batch_suffix(payloads: &[String]) -> &'static str {
    if payloads.len() == 1 {
        ""
    } else {
        " batch"
    }
}

fn frame_line(raw: &str) -> String {
    let trimmed = raw.trim_end_matches(['\r', '\n']);
    format!("{trimmed}\n")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PatchKind {
    Delivered,
    Conflict,
    Send(SendKind),
}

impl PatchKind {
    fn into_send_kind(self) -> SendKind {
        match self {
            PatchKind::Delivered => SendKind::Delivered,
            PatchKind::Conflict => SendKind::TransientFailure,
            PatchKind::Send(kind) => kind,
        }
    }
}

fn kind_name(kind: SendKind) -> &'static str {
    match kind {
        SendKind::Delivered => "delivered",
        SendKind::AuthFailure => "auth_failure",
        SendKind::TransientFailure => "transient_failure",
        SendKind::SecondaryRateLimit => "secondary_rate_limit",
        SendKind::Gone => "gone",
    }
}

fn classify_gh_error(combined_output: &str, exit_nonzero: bool) -> SendKind {
    if !exit_nonzero {
        return SendKind::TransientFailure;
    }
    let body = combined_output.to_ascii_lowercase();
    if body.contains("secondary rate limit")
        || body.contains("rate limit exceeded")
        || body.contains("abuse detection")
    {
        return SendKind::SecondaryRateLimit;
    }
    if body.contains("(http 404)")
        || body.contains("404 not found")
        || body.contains("not found (404)")
        || body.contains("gist not found")
    {
        return SendKind::Gone;
    }
    if body.contains("(http 401)")
        || body.contains("(http 403)")
        || body.contains("bad credentials")
        || body.contains("permission")
        || body.contains("401")
    {
        return SendKind::AuthFailure;
    }
    SendKind::TransientFailure
}

fn gh_api_get_classified(gist_id: &str) -> (Option<Value>, SendKind) {
    match run_gh_capture(&["api", "--include", &format!("gists/{gist_id}")], None) {
        Ok(raw) => {
            let (headers, body) = split_include_output(&raw);
            record_backoff(&headers);
            match serde_json::from_str::<Value>(&body) {
                Ok(value) => (Some(value), SendKind::Delivered),
                Err(error) => {
                    eprintln!("[airc:bearer] gh GET gists/{gist_id}: JSON parse failed: {error}");
                    (None, SendKind::TransientFailure)
                }
            }
        }
        Err(detail) => (None, classify_gh_error(&detail, true)),
    }
}

fn gh_api_patch_classified(gist_id: &str, content: &str) -> (bool, String, PatchKind) {
    let input = json!({ "files": { MESSAGES_FILE: { "content": content } } }).to_string();
    match run_gh_capture(
        &[
            "api",
            "--include",
            "--method",
            "PATCH",
            &format!("gists/{gist_id}"),
            "--input",
            "-",
        ],
        Some(input.as_bytes()),
    ) {
        Ok(raw) => {
            let (headers, _) = split_include_output(&raw);
            record_backoff(&headers);
            (true, String::new(), PatchKind::Delivered)
        }
        Err(detail) => {
            let lower = detail.to_ascii_lowercase();
            if detail.contains("409") || lower.contains("cannot be updated") {
                return (false, detail, PatchKind::Conflict);
            }
            let kind = classify_gh_error(&detail, true);
            (false, detail, PatchKind::Send(kind))
        }
    }
}

fn run_gh_capture(args: &[&str], input: Option<&[u8]>) -> Result<String, String> {
    let gh = env::var("AIRC_GH_BIN").unwrap_or_else(|_| "gh".to_string());
    let args_vec = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    let now = now_seconds();
    let (allowed, reason) = reserve_guarded_request(&args_vec, now)
        .map_err(|error| format!("gh governor failed: {error}"))?;
    let mut event = json!({
        "ts": now as i64,
        "pid": std::process::id(),
        "cwd": cwd(),
        "class": command_class(&args_vec),
        "args": safe_args(&args_vec),
        "allowed": allowed,
        "reason": reason,
    });
    if !allowed {
        event["rc"] = json!(75);
        event["outcome"] = json!("blocked");
        append_audit(&event);
        let detail = "secondary rate limit backoff active";
        record_backoff(detail);
        return Err(detail.to_string());
    }

    let mut command = Command::new(&gh);
    command.args(args);
    if input.is_some() {
        command.stdin(Stdio::piped());
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("{GH_API_TIMEOUT_DETAIL}: {error}"))?;
    if let Some(input) = input {
        let Some(mut stdin) = child.stdin.take() else {
            return Err("gh stdin unavailable".to_string());
        };
        stdin
            .write_all(input)
            .map_err(|error| format!("gh stdin write failed: {error}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("{GH_API_TIMEOUT_DETAIL}: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if output.status.success() {
        event["rc"] = json!(0);
        event["outcome"] = json!("ok");
        append_audit(&event);
        Ok(stdout)
    } else {
        let combined = format!("{stderr}{stdout}");
        record_backoff(&combined);
        event["rc"] = json!(output.status.code().unwrap_or(1));
        event["outcome"] = json!("error");
        append_audit(&event);
        Err(combined)
    }
}

fn read_messages_content(gist: &Value) -> String {
    gist.get("files")
        .and_then(Value::as_object)
        .and_then(|files| files.get(MESSAGES_FILE))
        .and_then(|entry| entry.get("content"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn rotate_if_needed(content: &str) -> String {
    let max_bytes = env_usize("AIRC_GIST_MAX_BYTES", DEFAULT_GIST_MAX_BYTES);
    if content.len() <= max_bytes {
        return content.to_string();
    }
    let target_bytes = env_usize("AIRC_GIST_TARGET_BYTES", max_bytes / 2);
    let keep_lines = env_usize("AIRC_GIST_KEEP_LINES", DEFAULT_GIST_KEEP_LINES);
    let mut kept = Vec::new();
    let mut bytes = 0usize;
    for line in content.lines().rev().filter(|line| !line.trim().is_empty()) {
        let line_bytes = line.len() + 1;
        if bytes + line_bytes > target_bytes || kept.len() >= keep_lines {
            break;
        }
        kept.push(line);
        bytes += line_bytes;
    }
    kept.reverse();
    if kept.is_empty() {
        String::new()
    } else {
        format!("{}\n", kept.join("\n"))
    }
}

fn env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn append_local_bus_batch(gist_id: &str, payloads: &[String]) -> (bool, String) {
    let mut ok = true;
    let mut detail = String::new();
    for payload in payloads {
        let (line_ok, line_detail) = append_local_bus(gist_id, payload);
        if !line_ok {
            ok = false;
            detail = line_detail;
        }
    }
    (ok, detail)
}

fn append_local_bus(gist_id: &str, line: &str) -> (bool, String) {
    if truthy(env::var("AIRC_DISABLE_LOCAL_BUS").ok().as_deref()) {
        return (false, "local bus disabled".to_string());
    }
    let path = local_bus_path(gist_id);
    if let Some(parent) = path.parent() {
        if let Err(error) = fs::create_dir_all(parent) {
            return (false, format!("local bus mkdir failed: {error}"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
        }
    }
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut file) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = file.set_permissions(fs::Permissions::from_mode(0o600));
            }
            if let Err(error) = file.write_all(line.as_bytes()) {
                return (false, format!("local bus append failed: {error}"));
            }
            (true, String::new())
        }
        Err(error) => (false, format!("local bus append failed: {error}")),
    }
}

fn local_bus_path(gist_id: &str) -> PathBuf {
    env::var_os("AIRC_LOCAL_BUS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home().join(".airc").join("bus").join("gh"))
        .join(safe_gist_id(gist_id))
        .join(MESSAGES_FILE)
}

fn user_home() -> PathBuf {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn safe_gist_id(gist_id: &str) -> String {
    gist_id
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect()
}

fn truthy(value: Option<&str>) -> bool {
    matches!(
        value.map(|raw| raw.trim().to_ascii_lowercase()),
        Some(raw) if matches!(raw.as_str(), "1" | "true" | "yes" | "on")
    )
}

fn sleep_jittered_backoff(attempt: usize) {
    let base_ms = (100u64.saturating_mul(1u64 << attempt.min(8))).min(30_000);
    let jitter = (now_seconds().to_bits() % base_ms.max(1)).min(base_ms / 2);
    thread::sleep(Duration::from_millis((base_ms / 2) + jitter));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_outcome_serializes_legacy_kind() {
        let outcome = SendOutcome::new(SendKind::SecondaryRateLimit, "slow down");
        let encoded = serde_json::to_string(&outcome).unwrap();
        assert_eq!(
            encoded,
            r#"{"kind":"secondary_rate_limit","detail":"slow down"}"#
        );
    }

    #[test]
    fn classify_distinguishes_gone_auth_rate_and_transient() {
        assert_eq!(
            classify_gh_error("secondary rate limit exceeded (HTTP 403)", true),
            SendKind::SecondaryRateLimit
        );
        assert_eq!(
            classify_gh_error("gist not found (HTTP 404)", true),
            SendKind::Gone
        );
        assert_eq!(
            classify_gh_error("Bad credentials (HTTP 401)", true),
            SendKind::AuthFailure
        );
        assert_eq!(
            classify_gh_error("connection reset by peer", true),
            SendKind::TransientFailure
        );
    }

    #[test]
    fn rotate_keeps_recent_lines_under_target() {
        temp_env::with_vars(
            [
                ("AIRC_GIST_MAX_BYTES", Some("20")),
                ("AIRC_GIST_TARGET_BYTES", Some("12")),
                ("AIRC_GIST_KEEP_LINES", Some("10")),
            ],
            || {
                let rotated = rotate_if_needed("one\ntwo\nthree\nfour\nfive\n");
                assert_eq!(rotated, "four\nfive\n");
            },
        );
    }

    #[test]
    fn frame_line_adds_exactly_one_newline() {
        assert_eq!(frame_line("abc\n\n"), "abc\n");
        assert_eq!(frame_line("abc"), "abc\n");
    }
}
