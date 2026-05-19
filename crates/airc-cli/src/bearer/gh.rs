use std::env;
use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

use crate::gh_state::{
    append_audit, command_class, cwd, now_seconds, record_backoff, reserve_guarded_request,
    safe_args, split_include_output,
};

use super::outcome::SendKind;

const MESSAGES_FILE: &str = "messages.jsonl";
const GH_API_TIMEOUT_DETAIL: &str = "gh api command failed";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchKind {
    Delivered,
    Conflict,
    Send(SendKind),
}

impl PatchKind {
    pub fn into_send_kind(self) -> SendKind {
        match self {
            PatchKind::Delivered => SendKind::Delivered,
            PatchKind::Conflict => SendKind::TransientFailure,
            PatchKind::Send(kind) => kind,
        }
    }
}

pub fn get_classified(gist_id: &str) -> (Option<Value>, SendKind) {
    match run_capture(&["api", "--include", &format!("gists/{gist_id}")], None) {
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
        Err(detail) => (None, classify_error(&detail, true)),
    }
}

pub fn patch_classified(gist_id: &str, content: &str) -> (bool, String, PatchKind) {
    let input = json!({ "files": { MESSAGES_FILE: { "content": content } } }).to_string();
    match run_capture(
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
            let kind = classify_error(&detail, true);
            (false, detail, PatchKind::Send(kind))
        }
    }
}

pub fn read_messages_content(gist: &Value) -> String {
    gist.get("files")
        .and_then(Value::as_object)
        .and_then(|files| files.get(MESSAGES_FILE))
        .and_then(|entry| entry.get("content"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

pub fn classify_error(combined_output: &str, exit_nonzero: bool) -> SendKind {
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

fn run_capture(args: &[&str], input: Option<&[u8]>) -> Result<String, String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_distinguishes_gone_auth_rate_and_transient() {
        assert_eq!(
            classify_error("secondary rate limit exceeded (HTTP 403)", true),
            SendKind::SecondaryRateLimit
        );
        assert_eq!(
            classify_error("gist not found (HTTP 404)", true),
            SendKind::Gone
        );
        assert_eq!(
            classify_error("Bad credentials (HTTP 401)", true),
            SendKind::AuthFailure
        );
        assert_eq!(
            classify_error("connection reset by peer", true),
            SendKind::TransientFailure
        );
    }
}
