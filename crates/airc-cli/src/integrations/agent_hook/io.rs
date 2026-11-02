//! Hook stdin and stdout: the session a runtime names, and the payload it reads.

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

use super::HookRuntime;

/// Read a runtime's hook JSON from stdin, with a hard deadline so a
/// hung pipe never deadlocks the process. Returns the session as a runtime
/// client id (`<session_prefix><session_id>`) when the input names one.
///
/// **Why the deadline exists** (airc#1097). On the Windows CI
/// runner, the codex_hook_commands tests hung for 5+ hours because
/// `std::io::stdin().read_to_string` waited forever for an EOF that
/// never arrived — most likely a Windows handle-inheritance leak
/// from `Stdio::piped()` parent → grandchild daemon, where the
/// daemon's inherited duplicate of the pipe-read handle kept the
/// pipe alive after the parent closed its write end. Mac/Linux
/// don't manifest the issue (~1s tests there).
///
/// The robust fix isn't a platform-specific handle-inheritance
/// patch (untestable without a Windows machine, fragile across
/// future cargo / std / Windows changes). It's to **stop relying
/// on stdin-EOF as a liveness signal**: read with a deadline,
/// proceed with empty payload if EOF doesn't arrive in time.
/// Codex sends the JSON in microseconds in the happy path; 5s
/// is far past any legitimate latency.
///
/// On Mac/Linux this changes nothing observable (EOF arrives
/// well within the deadline). On Windows the deadlock becomes a
/// fast-fail-and-proceed — the hook produces no AdditionalContext
/// for that call, which is the same outcome as receiving empty
/// stdin, and the next hook invocation behaves correctly.
///
/// **Intentional trade-offs the deadline introduces** (BIGMAMA
/// review BLOCK#2 + #3 on PR #1197):
///
/// 1. **Slow-but-legitimate producer truncates to empty payload.**
///    The deadline is on EOF, not on byte progress. A producer that
///    streams valid JSON byte-by-byte over >5s gets its input
///    discarded — `read_to_string` never returns, so the partial
///    buffer in the reader thread is dropped and the hook proceeds
///    with empty payload. The current caller (Codex) flushes JSON in
///    microseconds; if a future caller streams slowly, the deadline
///    needs to reset on progress or read to a JSON-object delimiter
///    rather than EOF. Today we accept the truncation because the
///    alternative is "Windows CI hangs for hours."
///
/// 2. **Timeout silently skips JSON validation.** The EOF path
///    rejects non-object input with a hard error
///    (`!value.is_object()` → `Err(...)` → non-zero exit). The
///    timeout path returns `Ok(())` unconditionally — so "malformed
///    input that also arrives slowly" degrades from a hard error to
///    a silent success. The `eprintln!` distinguishes timeout from
///    EOF in logs, but the validation-skip is intentional: a hook
///    that exits non-zero on slow-malformed input would block every
///    Codex prompt-submit on the same Windows hang it was meant to
///    end. Operators see WHY via stderr; the input contract is
///    "validates iff EOF arrives in time."
///
/// Both trade-offs are pinned by
/// `drain_stdin_timeout_proceeds_when_eof_never_arrives` in
/// `tests/codex_hook_commands.rs` — see the regression test for the
/// exact failure shape if a future refactor reverts the deadline.
pub(super) fn drain_stdin(
    runtime: &HookRuntime,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    const STDIN_READ_DEADLINE: Duration = Duration::from_secs(5);

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut raw = String::new();
        let result = std::io::stdin().read_to_string(&mut raw).map(|_| raw);
        let _ = tx.send(result);
    });

    let raw = match rx.recv_timeout(STDIN_READ_DEADLINE) {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(e.into()),
        Err(_timeout) => {
            // Best-effort diagnostic; the orphan reader thread keeps
            // its handle and will exit when the OS reclaims it after
            // process exit.
            eprintln!(
                "airc {}: stdin EOF not received within {STDIN_READ_DEADLINE:?}, \
                 proceeding with empty payload (see airc#1097)",
                runtime.verb
            );
            return Ok(None);
        }
    };

    if raw.trim().is_empty() {
        return Ok(None);
    }
    // PostToolUse may include a large tool response. Validate the JSON while
    // skipping unknown fields instead of building a DOM for the entire payload.
    #[derive(Deserialize)]
    struct HookInput {
        session_id: Option<String>,
    }
    if !raw.trim_start().starts_with('{') {
        return Err(format!("{} hook stdin must be a JSON object", runtime.name).into());
    }
    let input: HookInput = serde_json::from_str(&raw)?;
    Ok(input
        .session_id
        .filter(|session| !session.trim().is_empty())
        .map(|session| format!("{}{session}", runtime.session_prefix)))
}

/// The hook payload both Codex and Claude Code read for UserPromptSubmit and
/// PostToolUse: `{"hookSpecificOutput":{"hookEventName","additionalContext"}}`.
pub(super) fn print_hook_payload(
    context: &str,
    event_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct HookPayload<'a> {
        hook_specific_output: HookSpecificOutput<'a>,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct HookSpecificOutput<'a> {
        hook_event_name: &'a str,
        additional_context: &'a str,
    }

    let payload = HookPayload {
        hook_specific_output: HookSpecificOutput {
            hook_event_name: event_name,
            additional_context: context,
        },
    };
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, &payload)?;
    writeln!(output)?;
    output.flush()?;
    Ok(())
}
