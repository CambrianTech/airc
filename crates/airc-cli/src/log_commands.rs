//! `airc-rs log ...` handlers for legacy shell log paths.

use std::error::Error;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

const LOCK_STALE: Duration = Duration::from_secs(30);
const LOCK_WAIT: Duration = Duration::from_secs(5);
const LOCK_SLEEP: Duration = Duration::from_millis(50);
const TAIL_LIMIT: usize = 5_000;

pub fn run_append(path: &Path) -> Result<(), Box<dyn Error>> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    if input.is_empty() {
        return Ok(());
    }
    append_unique_sig(path, &input)?;
    Ok(())
}

pub fn run_rotate(path: &Path, max_lines: usize, keep_lines: usize) -> Result<(), Box<dyn Error>> {
    rotate_if_needed(path, max_lines, keep_lines)?;
    Ok(())
}

pub fn run_render(since: &str, count: usize, json_output: bool) -> Result<(), Box<dyn Error>> {
    let since_epoch = parse_since(since)?;
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    let events = events_from_lines(input.lines(), since_epoch);
    if json_output {
        println!("{}", render_json(&events, since, count)?);
    } else {
        print!("{}", render_human(&events));
    }
    Ok(())
}

pub struct InboxReadArgs {
    pub home: PathBuf,
    pub cursor_file: PathBuf,
    pub since: String,
    pub count: usize,
    pub peek: bool,
    pub quiet_empty: bool,
    pub exclude_self: bool,
    pub my_name: String,
    pub client_id: String,
}

pub fn run_inbox_reset(home: &Path, cursor_file: &Path) -> Result<(), Box<dyn Error>> {
    let offset = fs::metadata(home.join("messages.jsonl"))
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    write_cursor(cursor_file, offset)?;
    println!("airc inbox cursor reset.");
    Ok(())
}

pub fn run_inbox_read(args: InboxReadArgs) -> Result<(), Box<dyn Error>> {
    if args.count == 0 {
        return Err("inbox --count must be greater than zero".into());
    }
    let log_path = args.home.join("messages.jsonl");
    let (cursor_offset, legacy_since) = read_cursor(&args.cursor_file);
    let since_arg = if !args.since.is_empty() {
        args.since.clone()
    } else if cursor_offset.is_none() {
        legacy_since.unwrap_or_else(|| "5m".to_string())
    } else {
        String::new()
    };
    let since_epoch = if since_arg.is_empty() {
        None
    } else {
        Some(parse_inbox_since(&since_arg)?)
    };
    let size = fs::metadata(&log_path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let start_offset = if since_epoch.is_none() {
        cursor_offset.filter(|offset| *offset <= size).unwrap_or(0)
    } else {
        0
    };

    let mut printed = 0usize;
    let mut last_offset = start_offset;
    if let Ok(mut file) = File::open(&log_path) {
        file.seek(SeekFrom::Start(start_offset))?;
        let mut reader = BufReader::new(file);
        while printed < args.count {
            let mut raw = Vec::new();
            let read = reader.read_until(b'\n', &mut raw)?;
            if read == 0 {
                break;
            }
            let next_offset = reader.stream_position()?;
            let Ok(line) = serde_json::from_slice::<Value>(&raw) else {
                last_offset = next_offset;
                continue;
            };
            if args.exclude_self && inbox_is_self(&line, &args.my_name, &args.client_id) {
                last_offset = next_offset;
                continue;
            }
            if let Some(since_epoch) = since_epoch {
                let Some(ts) = line
                    .get("ts")
                    .and_then(Value::as_str)
                    .and_then(|ts| airc_core::iso_to_epoch(ts).ok())
                else {
                    last_offset = next_offset;
                    continue;
                };
                if ts <= since_epoch {
                    last_offset = next_offset;
                    continue;
                }
            }
            println!("{}", render_inbox_line(&line));
            printed += 1;
            last_offset = next_offset;
        }
    }

    if printed == 0 && !args.quiet_empty {
        println!(
            "No new airc messages since {}",
            if since_arg.is_empty() {
                "last inbox check"
            } else {
                &since_arg
            }
        );
    } else if !args.peek {
        write_cursor(&args.cursor_file, last_offset)?;
    }
    Ok(())
}

pub fn append_unique_sig(path: &Path, line: &str) -> Result<AppendOutcome, Box<dyn Error>> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }

    let framed = if line.ends_with('\n') {
        line.to_string()
    } else {
        format!("{line}\n")
    };
    let sig = line_sig(&framed);
    let lock = LogLock::acquire(path)?;
    let outcome = if sig
        .as_deref()
        .is_some_and(|sig| recent_sigs(path, TAIL_LIMIT).contains(&sig.to_string()))
    {
        AppendOutcome::Skipped
    } else {
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        file.write_all(framed.as_bytes())?;
        AppendOutcome::Appended
    };
    drop(lock);
    Ok(outcome)
}

fn rotate_if_needed(
    path: &Path,
    max_lines: usize,
    keep_lines: usize,
) -> Result<RotateOutcome, Box<dyn Error>> {
    if max_lines <= keep_lines {
        return Err("max-lines must be greater than keep-lines".into());
    }
    if !path.is_file() {
        return Ok(RotateOutcome::Noop);
    }

    let content = fs::read(path)?;
    let lines = split_lines(&content);
    if lines.len() <= max_lines {
        return Ok(RotateOutcome::Noop);
    }

    let tmp_path = temp_path_for(path);
    {
        let mut tmp = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp_path)?;
        for line in lines.iter().skip(lines.len() - keep_lines) {
            tmp.write_all(line)?;
        }
    }

    match fs::rename(&tmp_path, path) {
        Ok(()) => Ok(RotateOutcome::Rotated),
        Err(first_error) => {
            if let Err(remove_error) = fs::remove_file(path) {
                let _ = fs::remove_file(&tmp_path);
                return Err(format!(
                    "failed to replace {}: {first_error}; remove failed: {remove_error}",
                    path.display()
                )
                .into());
            }
            if let Err(rename_error) = fs::rename(&tmp_path, path) {
                let _ = fs::remove_file(&tmp_path);
                return Err(format!(
                    "failed to replace {} after remove: {rename_error}",
                    path.display()
                )
                .into());
            }
            Ok(RotateOutcome::Rotated)
        }
    }
}

fn line_sig(line: &str) -> Option<String> {
    let value: Value = serde_json::from_str(line).ok()?;
    value
        .get("sig")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn recent_sigs(path: &Path, limit: usize) -> Vec<String> {
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut sigs: Vec<String> = content
        .lines()
        .rev()
        .filter_map(line_sig)
        .take(limit)
        .collect();
    sigs.reverse();
    sigs
}

fn split_lines(content: &[u8]) -> Vec<&[u8]> {
    if content.is_empty() {
        return Vec::new();
    }

    let mut start = 0;
    let mut lines = Vec::new();
    for (index, byte) in content.iter().enumerate() {
        if *byte == b'\n' {
            lines.push(&content[start..=index]);
            start = index + 1;
        }
    }
    if start < content.len() {
        lines.push(&content[start..]);
    }
    lines
}

fn temp_path_for(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!(
        ".airc-log-{}-{}.tmp",
        std::process::id(),
        Uuid::new_v4()
    ))
}

fn parse_since(value: &str) -> Result<Option<i64>, Box<dyn Error>> {
    if value.is_empty() {
        return Ok(None);
    }
    if let Some((amount, unit)) = parse_relative_since(value) {
        let now = epoch_now();
        let window = amount
            .checked_mul(unit)
            .ok_or("airc logs --since: relative window is too large")?;
        return Ok(Some(now - window));
    }
    airc_core::iso_to_epoch(value).map(Some).map_err(|_| {
        format!("airc logs --since: cannot parse '{value}' (use ISO timestamp or 60s/5m/1h/2d)")
            .into()
    })
}

fn parse_inbox_since(value: &str) -> Result<i64, Box<dyn Error>> {
    if let Some((amount, unit)) = parse_relative_since(value) {
        let window = amount
            .checked_mul(unit)
            .ok_or("airc inbox --since: relative window is too large")?;
        return Ok(epoch_now() - window);
    }
    airc_core::iso_to_epoch(value)
        .map_err(|_| format!("airc inbox --since: cannot parse '{value}'").into())
}

fn read_cursor(path: &Path) -> (Option<u64>, Option<String>) {
    let Ok(raw) = fs::read_to_string(path) else {
        return (None, None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return (None, None);
    }
    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        return (None, Some(trimmed.to_string()));
    };
    let offset = value.get("offset").and_then(Value::as_u64);
    (offset, None)
}

fn write_cursor(path: &Path, offset: u64) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let tmp_path = path.with_extension(format!(
        "{}tmp.{}",
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| format!("{extension}."))
            .unwrap_or_default(),
        std::process::id()
    ));
    fs::write(&tmp_path, format!("{{\"offset\":{offset}}}\n"))?;
    fs::rename(tmp_path, path)?;
    Ok(())
}

fn inbox_is_self(line: &Value, my_name: &str, client_id: &str) -> bool {
    if !client_id.is_empty()
        && line
            .get("client_id")
            .and_then(Value::as_str)
            .is_some_and(|value| value == client_id)
    {
        return true;
    }
    client_id.is_empty()
        && !my_name.is_empty()
        && line
            .get("from")
            .and_then(Value::as_str)
            .is_some_and(|value| value == my_name)
}

fn render_inbox_line(line: &Value) -> String {
    let msg = line
        .get("msg")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| line.get("msg").map(ToString::to_string).unwrap_or_default());
    format!(
        "[{}] {}: {}",
        string_field(line, "ts", ""),
        string_field(line, "from", "?"),
        msg
    )
}

fn parse_relative_since(value: &str) -> Option<(i64, i64)> {
    let unit = match value.as_bytes().last().copied()? {
        b's' => 1,
        b'm' => 60,
        b'h' => 3_600,
        b'd' => 86_400,
        _ => return None,
    };
    let amount = value[..value.len() - 1].parse::<i64>().ok()?;
    if amount < 0 {
        return None;
    }
    Some((amount, unit))
}

fn events_from_lines<'a>(
    lines: impl IntoIterator<Item = &'a str>,
    since_epoch: Option<i64>,
) -> Vec<LogEvent> {
    lines
        .into_iter()
        .filter_map(|line| event_from_line(line, since_epoch))
        .collect()
}

fn event_from_line(line: &str, since_epoch: Option<i64>) -> Option<LogEvent> {
    let raw: Value = serde_json::from_str(line.trim()).ok()?;
    let Value::Object(raw_obj) = raw else {
        return None;
    };
    let raw = Value::Object(raw_obj);
    let ts = string_field(&raw, "ts", "");
    if let Some(since_epoch) = since_epoch {
        let event_epoch = airc_core::iso_to_epoch(&ts).ok()?;
        if event_epoch <= since_epoch {
            return None;
        }
    }
    let msg = raw
        .get("msg")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| raw.get("msg").map(ToString::to_string).unwrap_or_default());
    Some(LogEvent {
        id: string_field(&raw, "sig", &string_field(&raw, "id", "")),
        ts,
        sender: string_field(&raw, "from", "?"),
        recipient: string_field(&raw, "to", ""),
        channel: string_field(&raw, "channel", ""),
        msg,
        client_id: string_field(&raw, "client_id", &string_field(&raw, "clientId", "")),
        raw,
    })
}

fn render_human(events: &[LogEvent]) -> String {
    events
        .iter()
        .map(|event| format!("[{}] {}: {}\n", event.ts, event.sender, event.msg))
        .collect()
}

fn render_json(events: &[LogEvent], since: &str, count: usize) -> Result<String, Box<dyn Error>> {
    Ok(serde_json::to_string_pretty(&json!({
        "now_utc": format_epoch_utc(epoch_now()),
        "since": since,
        "count": count,
        "events": events,
    }))?)
}

fn string_field(value: &Value, key: &str, default: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_string()
}

fn epoch_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn format_epoch_utc(epoch: i64) -> String {
    let days = epoch.div_euclid(86_400);
    let seconds = epoch.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds / 3_600;
    let minute = (seconds % 3_600) / 60;
    let second = seconds % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era as i32 + era as i32 * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i32::from(month <= 2);
    (year, month as u32, day as u32)
}

#[derive(Debug, Serialize)]
struct LogEvent {
    id: String,
    ts: String,
    sender: String,
    recipient: String,
    channel: String,
    msg: String,
    client_id: String,
    raw: Value,
}

struct LogLock {
    path: PathBuf,
}

impl LogLock {
    fn acquire(path: &Path) -> Result<Self, Box<dyn Error>> {
        let lock_path = path.with_extension(format!(
            "{}lock",
            path.extension()
                .and_then(|extension| extension.to_str())
                .map(|extension| format!("{extension}."))
                .unwrap_or_default()
        ));
        let started = SystemTime::now();

        loop {
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(mut file) => {
                    writeln!(file, "{}", std::process::id())?;
                    return Ok(Self { path: lock_path });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    if is_stale_lock(&lock_path) {
                        let _ = fs::remove_file(&lock_path);
                        continue;
                    }
                    if started.elapsed().unwrap_or_default() >= LOCK_WAIT {
                        return Err(format!(
                            "timed out waiting for log lock: {}",
                            lock_path.display()
                        )
                        .into());
                    }
                    thread::sleep(LOCK_SLEEP);
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
}

impl Drop for LogLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn is_stale_lock(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    modified
        .elapsed()
        .map(|elapsed| elapsed > LOCK_STALE)
        .unwrap_or(false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendOutcome {
    Appended,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RotateOutcome {
    Noop,
    Rotated,
}

#[cfg(test)]
mod tests {
    use super::{
        append_unique_sig, rotate_if_needed, run_inbox_read, run_inbox_reset, AppendOutcome,
        InboxReadArgs, RotateOutcome,
    };

    #[test]
    fn append_skips_duplicate_sig_from_recent_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("messages.jsonl");

        assert_eq!(
            append_unique_sig(&path, r#"{"sig":"a","msg":"one"}"#).unwrap(),
            AppendOutcome::Appended
        );
        assert_eq!(
            append_unique_sig(&path, r#"{"sig":"a","msg":"duplicate"}"#).unwrap(),
            AppendOutcome::Skipped
        );

        let content = std::fs::read_to_string(path).unwrap();
        assert!(content.contains("one"));
        assert!(!content.contains("duplicate"));
    }

    #[test]
    fn append_without_sig_always_appends() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("messages.jsonl");

        append_unique_sig(&path, r#"{"msg":"one"}"#).unwrap();
        append_unique_sig(&path, r#"{"msg":"one"}"#).unwrap();

        assert_eq!(std::fs::read_to_string(path).unwrap().lines().count(), 2);
    }

    #[test]
    fn rotate_keeps_tail_when_over_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("messages.jsonl");
        std::fs::write(&path, "one\ntwo\nthree\nfour\n").unwrap();

        assert_eq!(
            rotate_if_needed(&path, 3, 2).unwrap(),
            RotateOutcome::Rotated
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "three\nfour\n");
    }

    #[test]
    fn rotate_noops_under_limit_or_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("messages.jsonl");

        assert_eq!(rotate_if_needed(&path, 3, 2).unwrap(), RotateOutcome::Noop);
        std::fs::write(&path, "one\ntwo\n").unwrap();
        assert_eq!(rotate_if_needed(&path, 3, 2).unwrap(), RotateOutcome::Noop);
    }

    #[test]
    fn rotate_rejects_non_headroom_thresholds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("messages.jsonl");

        assert!(rotate_if_needed(&path, 2, 2).is_err());
    }

    #[test]
    fn render_human_preserves_legacy_shape() {
        let events = super::events_from_lines(
            [r#"{"ts":"2026-05-16T01:00:00Z","from":"agent","msg":"ready"}"#],
            None,
        );

        assert_eq!(
            super::render_human(&events),
            "[2026-05-16T01:00:00Z] agent: ready\n"
        );
    }

    #[test]
    fn render_json_exposes_stable_event_fields() {
        let events = super::events_from_lines(
            [
                r#"{"sig":"sig-1","ts":"2026-05-16T01:00:00Z","from":"agent","to":"all","channel":"general","msg":"ready","client_id":"client-a"}"#,
            ],
            None,
        );
        let payload: serde_json::Value =
            serde_json::from_str(&super::render_json(&events, "", 20).unwrap()).unwrap();

        assert_eq!(payload["count"], 20);
        assert_eq!(payload["events"][0]["id"], "sig-1");
        assert_eq!(payload["events"][0]["sender"], "agent");
        assert_eq!(payload["events"][0]["recipient"], "all");
        assert_eq!(payload["events"][0]["channel"], "general");
        assert_eq!(payload["events"][0]["client_id"], "client-a");
        assert_eq!(payload["events"][0]["raw"]["msg"], "ready");
    }

    #[test]
    fn since_filters_by_message_timestamp() {
        let since = airc_core::iso_to_epoch("2026-05-16T01:00:00Z").unwrap();
        let events = super::events_from_lines(
            [
                r#"{"ts":"2026-05-16T00:59:59Z","from":"agent","msg":"old"}"#,
                r#"{"ts":"2026-05-16T01:00:01Z","from":"agent","msg":"new"}"#,
            ],
            Some(since),
        );

        assert_eq!(
            events
                .iter()
                .map(|event| event.msg.as_str())
                .collect::<Vec<_>>(),
            ["new"]
        );
    }

    #[test]
    fn relative_since_rejects_negative_windows() {
        assert!(super::parse_since("-5m").is_err());
    }

    #[test]
    fn inbox_read_advances_cursor_and_excludes_self() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let cursor_file = home.join("cursor.json");
        std::fs::write(
            home.join("messages.jsonl"),
            concat!(
                r#"{"ts":"2026-05-16T01:00:00Z","from":"me","client_id":"mine","msg":"self"}"#,
                "\n",
                r#"{"ts":"2026-05-16T01:00:01Z","from":"peer","client_id":"other","msg":"hello"}"#,
                "\n"
            ),
        )
        .unwrap();

        run_inbox_read(InboxReadArgs {
            home: home.to_path_buf(),
            cursor_file: cursor_file.clone(),
            since: "2026-05-16T00:59:59Z".to_string(),
            count: 500,
            peek: false,
            quiet_empty: true,
            exclude_self: true,
            my_name: "me".to_string(),
            client_id: "mine".to_string(),
        })
        .unwrap();

        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                &std::fs::read_to_string(cursor_file).unwrap()
            )
            .unwrap()["offset"]
                .as_u64()
                .unwrap(),
            std::fs::metadata(home.join("messages.jsonl"))
                .unwrap()
                .len()
        );
    }

    #[test]
    fn inbox_reset_moves_cursor_to_log_end() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let cursor_file = home.join("cursor.json");
        std::fs::write(home.join("messages.jsonl"), "one\ntwo\n").unwrap();

        run_inbox_reset(home, &cursor_file).unwrap();

        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                &std::fs::read_to_string(cursor_file).unwrap()
            )
            .unwrap()["offset"]
                .as_u64(),
            Some(8)
        );
    }
}
