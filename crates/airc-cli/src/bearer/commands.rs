use std::error::Error;
use std::io::{self, Read};
use std::thread;
use std::time::Duration;

use super::gh::{self, PatchKind};
use super::local_bus;
use super::outcome::{kind_name, SendKind, SendOutcome};
use super::rotate::rotate_if_needed;
use crate::gh_state::now_seconds;

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

    let (local_ok, local_detail) = local_bus::append_batch(gist_id, payloads);
    let batch_content = payloads.concat();
    let retries = 8;
    let mut last_detail = String::new();

    for attempt in 0..retries {
        let (gist, get_kind) = gh::get_classified(gist_id);
        let Some(gist) = gist else {
            return get_failure_outcome(gist_id, payloads, local_ok, get_kind);
        };

        let existing = gh::read_messages_content(&gist);
        let new_content = rotate_if_needed(&(rotate_if_needed(&existing) + &batch_content));
        let (ok, detail, patch_kind) = gh::patch_classified(gist_id, &new_content);
        if !ok {
            last_detail = detail;
            if patch_kind == PatchKind::Conflict {
                sleep_jittered_backoff(attempt);
                continue;
            }
            let send_kind = patch_kind.into_send_kind();
            if let Some(outcome) = local_bus_deferred(payloads, local_ok, send_kind) {
                return outcome;
            }
            return SendOutcome::new(send_kind, last_detail);
        }

        let (verify, _) = gh::get_classified(gist_id);
        let Some(verify) = verify else {
            return delivered_detail(payloads);
        };
        let content = gh::read_messages_content(&verify);
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

    conflict_exhausted_outcome(payloads, local_ok, &local_detail, retries, &last_detail)
}

fn get_failure_outcome(
    gist_id: &str,
    payloads: &[String],
    local_ok: bool,
    get_kind: SendKind,
) -> SendOutcome {
    if let Some(outcome) = local_bus_deferred(payloads, local_ok, get_kind) {
        return outcome;
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
    SendOutcome::new(
        SendKind::TransientFailure,
        format!("could not fetch gist {gist_id} (network/5xx/timeout)"),
    )
}

fn local_bus_deferred(
    payloads: &[String],
    local_ok: bool,
    failure_kind: SendKind,
) -> Option<SendOutcome> {
    if !local_ok
        || !matches!(
            failure_kind,
            SendKind::SecondaryRateLimit | SendKind::TransientFailure | SendKind::AuthFailure
        )
    {
        return None;
    }
    Some(SendOutcome::new(
        SendKind::Delivered,
        format!(
            "delivered{} via local bus; gh publish deferred ({})",
            batch_suffix(payloads),
            kind_name(failure_kind)
        ),
    ))
}

fn conflict_exhausted_outcome(
    payloads: &[String],
    local_ok: bool,
    local_detail: &str,
    retries: usize,
    last_detail: &str,
) -> SendOutcome {
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

fn sleep_jittered_backoff(attempt: usize) {
    let base_ms = (100u64.saturating_mul(1u64 << attempt.min(8))).min(30_000);
    let jitter = (now_seconds().to_bits() % base_ms.max(1)).min(base_ms / 2);
    thread::sleep(Duration::from_millis((base_ms / 2) + jitter));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_line_adds_exactly_one_newline() {
        assert_eq!(frame_line("abc\n\n"), "abc\n");
        assert_eq!(frame_line("abc"), "abc\n");
    }
}
