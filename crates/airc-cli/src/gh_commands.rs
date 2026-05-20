use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use serde_json::{json, Value};

use crate::gh_state::{
    append_audit, audit_path, backoff_path, backoff_until, budget_path, budget_snapshot,
    command_class, cwd, guarded_command, now_seconds, recent_events, record_backoff,
    reserve_guarded_request, safe_args, split_include_output, wait_seconds,
};

pub fn run_gh(gh_args: Vec<String>) -> Result<(), Box<dyn Error>> {
    let gh_args = strip_separator(gh_args);
    run_gh_with_input(gh_args, None)
}

pub fn run_patch_gist_file(
    gist_id: &str,
    filename: &str,
    content_file: &Path,
) -> Result<(), Box<dyn Error>> {
    if gist_id.trim().is_empty() {
        return Err("gh patch-gist-file: --gist-id is required".into());
    }
    if filename.trim().is_empty() {
        return Err("gh patch-gist-file: --filename is required".into());
    }
    let content = fs::read_to_string(content_file)?;
    let input = patch_gist_file_input(filename, &content);
    run_gh_with_input(
        vec![
            "api".to_string(),
            "--include".to_string(),
            "--method".to_string(),
            "PATCH".to_string(),
            format!("gists/{gist_id}"),
            "--input".to_string(),
            "-".to_string(),
        ],
        Some(input),
    )
}

fn patch_gist_file_input(filename: &str, content: &str) -> String {
    json!({
        "files": {
            filename: {
                "content": content
            }
        }
    })
    .to_string()
}

fn run_gh_with_input(gh_args: Vec<String>, input: Option<String>) -> Result<(), Box<dyn Error>> {
    let gh = env::var("AIRC_GH_BIN").unwrap_or_else(|_| "gh".to_string());

    if gh_args.len() >= 2
        && gh_args[0] == "auth"
        && matches!(gh_args[1].as_str(), "login" | "refresh")
    {
        append_audit(&json!({
            "ts": now_seconds() as i64,
            "pid": std::process::id(),
            "cwd": cwd(),
            "class": command_class(&gh_args),
            "args": safe_args(&gh_args),
            "allowed": true,
            "reason": "interactive-auth",
        }));
        let status = Command::new(&gh).args(&gh_args).status()?;
        return if status.success() {
            Ok(())
        } else {
            Err(format!("gh exited with status {status}").into())
        };
    }

    let now = now_seconds();
    let (allowed, reason) = if env::var("AIRC_GH_GUARD_DISABLE").ok().as_deref() == Some("1")
        || !guarded_command(&gh_args)
    {
        (true, "unguarded".to_string())
    } else {
        reserve_guarded_request(&gh_args, now)?
    };

    let mut event = json!({
        "ts": now as i64,
        "pid": std::process::id(),
        "cwd": cwd(),
        "class": command_class(&gh_args),
        "args": safe_args(&gh_args),
        "allowed": allowed,
        "reason": reason,
        "backoff_until": backoff_until() as i64,
    });

    if !allowed {
        let msg = format!(
            "airc gh guard: {}; refusing gh {}\n",
            reason,
            safe_args(&gh_args)
                .into_iter()
                .take(3)
                .collect::<Vec<_>>()
                .join(" ")
        );
        event["rc"] = json!(75);
        event["outcome"] = json!("blocked");
        append_audit(&event);
        eprint!("{msg}");
        return Err("gh request blocked by governor".into());
    }

    let output = if let Some(input) = input {
        let mut child = Command::new(&gh)
            .args(&gh_args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(input.as_bytes())?;
        }
        child.wait_with_output()?
    } else {
        Command::new(&gh).args(&gh_args).output()?
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    print!("{stdout}");
    eprint!("{stderr}");

    if output.status.success() {
        let (headers, _) = split_include_output(&stdout);
        record_backoff(&headers);
        event["rc"] = json!(0);
        event["outcome"] = json!("ok");
    } else {
        record_backoff(&format!("{stderr}{stdout}"));
        event["rc"] = json!(output.status.code().unwrap_or(1));
        event["outcome"] = json!("error");
        event["backoff_until"] = json!(backoff_until() as i64);
        append_audit(&event);
        return Err(format!("gh exited with status {}", output.status).into());
    }
    event["backoff_until"] = json!(backoff_until() as i64);
    append_audit(&event);
    Ok(())
}

pub fn run_wait_seconds() {
    println!("{}", wait_seconds(now_seconds()));
}

pub fn run_audit(
    count: usize,
    summary: bool,
    reset: bool,
    clear_audit: bool,
) -> Result<(), Box<dyn Error>> {
    if reset {
        let mut removed = Vec::new();
        for path in [backoff_path(), budget_path()] {
            if fs::remove_file(&path).is_ok() {
                removed.push(path);
            }
        }
        println!("AIRC gh guard reset: cleared shared backoff/budget state.");
        for path in removed {
            println!("  removed {}", path.display());
        }
        println!("  audit log retained: {}", audit_path().display());
        return Ok(());
    }
    if clear_audit {
        let path = audit_path();
        match fs::remove_file(&path) {
            Ok(()) => println!("AIRC gh audit cleared: {}", path.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                println!("AIRC gh audit already empty: {}", path.display())
            }
            Err(error) => return Err(error.into()),
        }
        return Ok(());
    }

    let path = audit_path();
    let rows = recent_events(count);
    if !path.exists() {
        println!("No AIRC gh audit log yet: {}", path.display());
        return Ok(());
    }

    if summary {
        let mut counts = BTreeMap::new();
        let mut blocked = 0usize;
        for event in &rows {
            let class = event
                .get("class")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            *counts.entry(class.to_string()).or_insert(0usize) += 1;
            if event.get("allowed").and_then(Value::as_bool) == Some(false) {
                blocked += 1;
            }
        }
        println!("AIRC gh audit: {}", path.display());
        println!(
            "recent events: {}; blocked: {blocked}; shared backoff: {}s",
            rows.len(),
            wait_seconds(now_seconds())
        );
        let mut counts: Vec<_> = counts.into_iter().collect();
        counts.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        for (class, class_count) in counts {
            println!("  {class_count:4}  {class}");
        }
        return Ok(());
    }

    println!("AIRC gh audit: {}", path.display());
    for event in rows {
        let ts = event.get("ts").map_or("?".to_string(), Value::to_string);
        let class = event
            .get("class")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let allowed = if event.get("allowed").and_then(Value::as_bool) == Some(false) {
            "BLOCKED"
        } else {
            "ok"
        };
        let rc = event.get("rc").map_or(String::new(), Value::to_string);
        let reason = event.get("reason").and_then(Value::as_str).unwrap_or("");
        let cwd = event.get("cwd").and_then(Value::as_str).unwrap_or("");
        println!("{ts} {allowed:7} rc={rc:>7} {class} - {reason} - {cwd}");
    }
    Ok(())
}

pub fn run_doctor(count: usize) -> Result<(), Box<dyn Error>> {
    let now = now_seconds();
    let wait = wait_seconds(now);
    let rows = recent_events(count);
    let blocked = rows
        .iter()
        .filter(|event| event.get("allowed").and_then(Value::as_bool) == Some(false))
        .count();
    let (used, limit) = budget_snapshot(now)?;

    println!("  gh governor audit: {}", audit_path().display());
    if wait > 0 {
        println!("  [BLOCKED] gh governor shared backoff active for {wait}s");
    } else if blocked > 0 {
        println!(
            "  [WARN] gh governor blocked {blocked}/{} recent guarded request(s)",
            rows.len()
        );
    } else if used >= limit {
        println!("  [WARN] gh governor local request budget full ({used}/{limit} in 60s)");
    } else {
        println!(
            "  [ok] gh governor: no active backoff; {blocked}/{} blocked; budget {used}/{limit} in 60s",
            rows.len()
        );
    }

    let mut classes = BTreeMap::new();
    for event in rows {
        let class = event
            .get("class")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        *classes.entry(class.to_string()).or_insert(0usize) += 1;
    }
    if classes.is_empty() {
        println!("         no guarded gh requests recorded yet");
    } else {
        println!("         recent gh classes:");
        let mut classes: Vec<_> = classes.into_iter().collect();
        classes.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        for (class, class_count) in classes.into_iter().take(5) {
            println!("           {class_count:4}  {class}");
        }
    }
    if wait > 0 || blocked > 0 || used >= limit {
        Err("gh governor degraded".into())
    } else {
        Ok(())
    }
}

fn strip_separator(mut args: Vec<String>) -> Vec<String> {
    if args.first().map(String::as_str) == Some("--") {
        args.remove(0);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_gist_file_input_targets_exact_filename() {
        let body =
            patch_gist_file_input("airc-room-general.json", "{\"host\":{\"name\":\"beta\"}}\n");
        let parsed: Value = serde_json::from_str(&body).unwrap();

        assert_eq!(
            parsed["files"]["airc-room-general.json"]["content"],
            "{\"host\":{\"name\":\"beta\"}}\n"
        );
        assert!(parsed["files"]["messages.jsonl"].is_null());
    }
}
