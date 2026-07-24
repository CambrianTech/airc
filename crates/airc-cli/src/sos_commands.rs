//! `airc sos` — the account's out-of-band coordination channel.
//!
//! When the airc wire can't route (blind rooms, stale peers, dead
//! dials) but `gh` still works, agents need a backup channel to
//! coordinate recovery. This formalizes the improvised
//! GitHub-gist-comment thread into a first-class verb.
//!
//! The channel is ONE gist per account, discovered by a stable
//! description marker ([`SOS_MARKER`]) and cached via a local sentinel
//! for cheap re-find. Coordination happens in the gist's COMMENTS:
//! every post is prefixed with this node's readable machine label
//! (`[BIGMAMA] …`) so a human can tell machines apart at a glance, and
//! `watch` self-filters our own label so we never echo our own posts
//! back into our own context (a real context-wasting bug).
//!
//! Reuse notes (one-source-of-truth discipline):
//!   - gh spawn idiom mirrors `channel_gist_commands` / `gh_commands`
//!     (`AIRC_GH_BIN` override, captured stdout/stderr), but SOS fails
//!     LOUD — every failure returns a specific `Err`, never a silent
//!     cache fallthrough, because a broken SOS channel must be visible.
//!   - the gh governor (`gh_state::reserve_guarded_request` +
//!     `record_backoff`) is honored so SOS shares the machine-wide gh
//!     budget with the registry loop rather than racing it.
//!   - the find-or-create-gist-by-marker + local-sentinel shape mirrors
//!     `airc_lib::gh::account_registry` (which coordinates via gist
//!     FILES); SOS coordinates via gist COMMENTS.

use std::error::Error;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::gh_state::{now_seconds, record_backoff, reserve_guarded_request, wait_seconds};

/// Stable gist `description` marker identifying the account's SOS gist.
/// The find path filters the account's gists by exact match on this.
const SOS_MARKER: &str = "airc-account-sos";

/// Filename for the self-describing protocol file seeded into a freshly
/// created SOS gist. Its content ([`SOS_PROTOCOL_DOC`]) documents the
/// channel so a peer who stumbles on the gist understands it.
const SOS_SEED_FILENAME: &str = "airc-account-sos.md";

/// Poll cadence for `watch --follow` (human streaming mode). Deliberately
/// gentle: the whole point of SOS is that it works when the wire is down,
/// so it must not itself hammer `gh` into a rate-limit backoff.
const WATCH_POLL_INTERVAL: Duration = Duration::from_secs(45);

/// Max readable length of a derived machine label. Long enough for a
/// hostname, short enough to keep comment prefixes scannable.
const MAX_LABEL_LEN: usize = 24;

/// Number of trailing comments `status` prints for context.
const STATUS_COMMENT_LIMIT: usize = 10;

/// Self-describing protocol doc seeded into a new SOS gist.
const SOS_PROTOCOL_DOC: &str = "\
# airc SOS channel

This gist is an airc account's **out-of-band coordination channel**.

When the normal airc wire can't route (blind rooms, stale peers, dead
dials) but `gh` still works, agents post to and watch **this gist's
comments** to coordinate recovery.

## Protocol

- Each comment is prefixed with the posting node's machine label, e.g.
  `[BIGMAMA] daemon wedged, restarting`.
- Post:  `airc sos send \"<message>\"`
- Watch: `airc sos watch`            (agent mode: print new peer comments, exit)
- Watch: `airc sos watch --follow`   (human mode: stream continuously)
- Status: `airc sos status`          (gist id + url + recent comments)

`watch` self-filters your own label, so you never see your own posts
echoed back.

Do not delete this gist — it is the account's rendezvous of last resort.
";

// ---------------------------------------------------------------------------
// Public entry points (dispatched from main.rs).
// ---------------------------------------------------------------------------

/// `airc sos send <message>` — post a labelled comment to the account's
/// SOS gist, finding or creating the gist first.
pub async fn run_send(home: &Path, message: &str) -> Result<(), Box<dyn Error>> {
    let message = message.trim();
    if message.is_empty() {
        return Err("airc sos send: message is empty — nothing to post".into());
    }
    let label = node_label(home).await?;
    let gist_id = resolve_sos_gist(home)?;
    let body = format!("[{label}] {message}");
    post_comment(&gist_id, &body)?;
    println!("airc sos: posted to gist {gist_id} as [{label}]");
    Ok(())
}

/// `airc sos watch [--follow]` — surface new PEER comments.
///
/// Agent mode (`follow == false`): one poll, print any new peer
/// comment(s), then exit so an agent harness can re-invoke on its own
/// cadence. Human mode (`follow == true`): stream continuously, never
/// exiting on its own.
pub async fn run_watch(home: &Path, follow: bool) -> Result<(), Box<dyn Error>> {
    let label = node_label(home).await?;
    let gist_id = resolve_sos_gist(home)?;
    let cursor_path = watch_cursor_path(home);

    if !follow {
        let printed = poll_once(&gist_id, &label, &cursor_path)?;
        if printed == 0 {
            eprintln!("airc sos: no new peer messages on gist {gist_id}");
        }
        return Ok(());
    }

    eprintln!(
        "airc sos: watching gist {gist_id} as [{label}] (polling every {}s; Ctrl-C to stop)",
        WATCH_POLL_INTERVAL.as_secs()
    );
    loop {
        // A transient gh hiccup must not kill a long-lived human watch:
        // report it and keep polling. (This is the ONE place SOS tolerates
        // a failure rather than returning Err — a follow loop that dies on
        // the first blip is useless as a recovery channel.)
        if let Err(error) = poll_once(&gist_id, &label, &cursor_path) {
            eprintln!("airc sos: poll failed (will retry): {error}");
        }
        tokio::time::sleep(WATCH_POLL_INTERVAL).await;
    }
}

/// `airc sos status` — print the SOS gist id + html url + the last
/// [`STATUS_COMMENT_LIMIT`] comments so a peer can find and read the
/// channel.
pub async fn run_status(home: &Path) -> Result<(), Box<dyn Error>> {
    let label = node_label(home).await?;
    let gist_id = resolve_sos_gist(home)?;
    let html_url = gist_html_url(&gist_id)?;
    let comments = fetch_comments(&gist_id)?;

    println!("airc sos channel");
    println!("  this node: [{label}]");
    println!("  gist id:   {gist_id}");
    println!("  url:       {html_url}");
    println!("  comments:  {}", comments.len());
    let tail_start = comments.len().saturating_sub(STATUS_COMMENT_LIMIT);
    if comments.is_empty() {
        println!("  (no messages yet)");
    } else {
        println!("  last {}:", comments.len() - tail_start);
        for comment in &comments[tail_start..] {
            println!("    {}", comment.body.replace('\n', " "));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Gist find-or-create + sentinel.
// ---------------------------------------------------------------------------

/// Find (or create) the account's SOS gist and return its id. Consults
/// the local sentinel first for a cheap re-find; on a miss, lists the
/// account's gists filtered by [`SOS_MARKER`]; on a total miss, creates
/// a fresh gist seeded with the protocol doc. The result is written back
/// to the sentinel.
fn resolve_sos_gist(home: &Path) -> Result<String, Box<dyn Error>> {
    if let Some(gist_id) = read_sentinel(home) {
        return Ok(gist_id);
    }
    let gist_id = match find_sos_gist()? {
        Some(existing) => existing,
        None => create_sos_gist()?,
    };
    write_sentinel(home, &gist_id);
    Ok(gist_id)
}

/// List the account's gists and return the id of the first whose
/// description exactly matches [`SOS_MARKER`], if any.
fn find_sos_gist() -> Result<Option<String>, Box<dyn Error>> {
    let jq = format!("[.[] | select(.description == \"{SOS_MARKER}\") | .id] | .[]");
    // `--paginate`: an account with 100+ gists (Joel's has many registry
    // gists) could sink the SOS gist past a single page and spawn a
    // duplicate on miss. Follow Link headers so the find is exhaustive;
    // the jq runs per-page and the matching ids concatenate.
    let stdout = gh_capture(
        &["api", "--paginate", "/gists?per_page=100", "--jq", &jq],
        None,
    )?;
    Ok(stdout
        .lines()
        .map(str::trim)
        .find(|line| valid_gist_id(line))
        .map(ToOwned::to_owned))
}

/// Create a new private SOS gist seeded with the protocol doc, and
/// return its id.
fn create_sos_gist() -> Result<String, Box<dyn Error>> {
    let stdout = gh_capture(
        &[
            "gist",
            "create",
            "--filename",
            SOS_SEED_FILENAME,
            "--desc",
            SOS_MARKER,
            "-",
        ],
        Some(SOS_PROTOCOL_DOC),
    )?;
    parse_created_gist_id(&stdout)
        .ok_or_else(|| format!("airc sos: could not parse gist id from gh output: {stdout}").into())
}

/// html url for the gist, read from the API so we print GitHub's real
/// canonical url rather than a guessed one.
fn gist_html_url(gist_id: &str) -> Result<String, Box<dyn Error>> {
    let stdout = gh_capture(
        &["api", &format!("gists/{gist_id}"), "--jq", ".html_url"],
        None,
    )?;
    let url = stdout.trim();
    if url.is_empty() || url == "null" {
        return Err(format!("airc sos: gist {gist_id} has no html_url (deleted?)").into());
    }
    Ok(url.to_string())
}

// ---------------------------------------------------------------------------
// Comment post / fetch.
// ---------------------------------------------------------------------------

/// One SOS comment: the fields we act on.
struct SosComment {
    id: u64,
    body: String,
}

/// Post a comment body to the gist's comment thread.
fn post_comment(gist_id: &str, body: &str) -> Result<(), Box<dyn Error>> {
    // `gh api gists/<id>/comments -f body=<text>` POSTs {"body": text}.
    let field = format!("body={body}");
    gh_capture(
        &["api", &format!("gists/{gist_id}/comments"), "-f", &field],
        None,
    )?;
    Ok(())
}

/// Fetch the gist's comments in chronological (API) order.
fn fetch_comments(gist_id: &str) -> Result<Vec<SosComment>, Box<dyn Error>> {
    let stdout = gh_capture(
        &["api", &format!("gists/{gist_id}/comments"), "--paginate"],
        None,
    )?;
    parse_comments(&stdout)
}

/// Parse the gh JSON array of gist comments into [`SosComment`]s.
/// Comments missing an integer `id` or string `body` are skipped rather
/// than aborting the whole read.
fn parse_comments(raw: &str) -> Result<Vec<SosComment>, Box<dyn Error>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let value: Value = serde_json::from_str(trimmed)
        .map_err(|error| format!("airc sos: gist comments were not valid JSON: {error}"))?;
    let Some(items) = value.as_array() else {
        return Err("airc sos: expected a JSON array of gist comments".into());
    };
    Ok(items
        .iter()
        .filter_map(|item| {
            let id = item.get("id").and_then(Value::as_u64)?;
            let body = item.get("body").and_then(Value::as_str)?.to_string();
            Some(SosComment { id, body })
        })
        .collect())
}

/// One poll: fetch comments, print any past the cursor that are NOT our
/// own, advance the cursor to the highest id seen. Returns the number of
/// peer comments printed.
fn poll_once(gist_id: &str, my_label: &str, cursor_path: &Path) -> Result<usize, Box<dyn Error>> {
    let comments = fetch_comments(gist_id)?;
    let cursor = read_cursor(cursor_path);
    let mut printed = 0usize;
    let mut high_water = cursor;
    for comment in &comments {
        high_water = high_water.max(comment.id);
        if comment.id <= cursor {
            continue;
        }
        if is_self_comment(&comment.body, my_label) {
            continue;
        }
        println!("{}", comment.body);
        printed += 1;
    }
    if high_water > cursor {
        write_cursor(cursor_path, high_water);
    }
    Ok(printed)
}

// ---------------------------------------------------------------------------
// Machine label + self-filter (pure predicates, unit-tested below).
// ---------------------------------------------------------------------------

/// Derive this node's readable machine label. Prefers a real hostname
/// (`COMPUTERNAME` on Windows, `HOSTNAME` / the `hostname` command on
/// unix); falls back to the platform name + a short peer-id suffix so a
/// hostname-less box still gets a distinguishing, stable label.
async fn node_label(home: &Path) -> Result<String, Box<dyn Error>> {
    if let Some(normalized) = raw_hostname().and_then(|raw| normalize_label(&raw)) {
        return Ok(normalized);
    }
    // Fallback: platform + short peer-id. peer_id is stable across runs,
    // so the fallback label is stable too (self-filter depends on that).
    let identity = airc_identity::LocalIdentity::load_or_generate(home).await?;
    let short_peer: String = identity.peer_id.to_string().chars().take(8).collect();
    let fallback = format!("{}-{short_peer}", std::env::consts::OS);
    normalize_label(&fallback).ok_or_else(|| "airc sos: could not derive a machine label".into())
}

/// Best-effort raw hostname string from the environment / `hostname`
/// command. Returns `None` when nothing usable is found.
fn raw_hostname() -> Option<String> {
    for key in ["COMPUTERNAME", "HOSTNAME"] {
        if let Ok(value) = std::env::var(key) {
            if !value.trim().is_empty() {
                return Some(value);
            }
        }
    }
    let output = Command::new("hostname").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!value.is_empty()).then_some(value)
}

/// Normalize a raw hostname/fallback into a readable label: take the
/// segment before the first `.`, keep ASCII alphanumerics and `-`,
/// uppercase, and truncate to [`MAX_LABEL_LEN`]. Returns `None` when
/// nothing usable survives (so the caller can try the next source).
fn normalize_label(raw: &str) -> Option<String> {
    let head = raw.trim().split('.').next().unwrap_or("").trim();
    let cleaned: String = head
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
        .take(MAX_LABEL_LEN)
        .collect::<String>()
        .to_ascii_uppercase();
    let trimmed = cleaned.trim_matches('-');
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// True when a comment body carries OUR label prefix (`[LABEL] …`), so
/// `watch` can drop our own posts. Labels are already uppercased, so an
/// exact prefix match is correct.
fn is_self_comment(body: &str, my_label: &str) -> bool {
    parse_comment_label(body).is_some_and(|label| label == my_label)
}

/// Extract the `LABEL` from a `[LABEL] …` comment body, if present.
fn parse_comment_label(body: &str) -> Option<&str> {
    let rest = body.trim_start().strip_prefix('[')?;
    let end = rest.find(']')?;
    let label = &rest[..end];
    (!label.is_empty()).then_some(label)
}

// ---------------------------------------------------------------------------
// gh runner (captures output; fails LOUD).
// ---------------------------------------------------------------------------

/// Spawn `gh` with captured stdout/stderr, honoring the shared gh
/// governor. Returns stdout on success; on governor denial or a nonzero
/// exit, returns a specific `Err` (SOS never silently swallows a gh
/// failure — a broken backup channel must be loud).
fn gh_capture(args: &[&str], stdin: Option<&str>) -> Result<String, Box<dyn Error>> {
    let owned: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
    let now = now_seconds();
    let (allowed, reason) = reserve_guarded_request(&owned, now)?;
    if !allowed {
        return Err(format!(
            "airc sos: gh governor blocked this request ({reason}); retry in {}s",
            wait_seconds(now)
        )
        .into());
    }

    let gh = std::env::var("AIRC_GH_BIN").unwrap_or_else(|_| "gh".to_string());
    let mut command = Command::new(&gh);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if stdin.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("airc sos: failed to spawn {gh}: {error}"))?;
    if let (Some(input), Some(mut handle)) = (stdin, child.stdin.take()) {
        handle
            .write_all(input.as_bytes())
            .map_err(|error| format!("airc sos: failed to write gh stdin: {error}"))?;
        // Drop the handle to send EOF before waiting.
        drop(handle);
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("airc sos: failed to wait on gh: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    // Feed GitHub's own rate-limit signal back into the shared governor.
    record_backoff(&format!("{stderr}{stdout}"));
    if !output.status.success() {
        return Err(format!(
            "airc sos: `gh {}` failed: {}",
            owned.join(" "),
            stderr.trim()
        )
        .into());
    }
    Ok(stdout)
}

// ---------------------------------------------------------------------------
// Local persistence (sentinel + watch cursor).
// ---------------------------------------------------------------------------

/// Local sentinel file caching the resolved SOS gist id for cheap re-find.
#[derive(Serialize, Deserialize)]
struct SosSentinel {
    gist_id: String,
}

fn sentinel_path(home: &Path) -> PathBuf {
    home.join("sos-gist.json")
}

fn read_sentinel(home: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(sentinel_path(home)).ok()?;
    let sentinel: SosSentinel = serde_json::from_str(&raw).ok()?;
    valid_gist_id(&sentinel.gist_id).then_some(sentinel.gist_id)
}

fn write_sentinel(home: &Path, gist_id: &str) {
    let path = sentinel_path(home);
    if let Ok(body) = serde_json::to_vec(&SosSentinel {
        gist_id: gist_id.to_string(),
    }) {
        let _ = std::fs::write(path, body);
    }
}

fn watch_cursor_path(home: &Path) -> PathBuf {
    home.join("sos-watch-cursor")
}

fn read_cursor(path: &Path) -> u64 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

fn write_cursor(path: &Path, cursor: u64) {
    let _ = std::fs::write(path, cursor.to_string());
}

// ---------------------------------------------------------------------------
// Small pure helpers.
// ---------------------------------------------------------------------------

/// A gist id is 8–64 lowercase/upper hex characters. Guards sentinel
/// reads and created-id parsing against garbage.
fn valid_gist_id(candidate: &str) -> bool {
    (8..=64).contains(&candidate.len()) && candidate.chars().all(|ch| ch.is_ascii_hexdigit())
}

/// Parse the created gist id from `gh gist create` output — the gist URL
/// is on the last non-empty line; the id is its final path segment.
fn parse_created_gist_id(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .rev()
        .find_map(|line| line.trim().rsplit('/').next())
        .map(str::trim)
        .filter(|candidate| valid_gist_id(candidate))
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_label_uppercases_and_strips_domain() {
        // what this catches: a machine label must be the readable short
        // hostname (domain stripped, uppercased) so a human tells
        // BIGMAMA from a Mac at a glance.
        assert_eq!(normalize_label("BigMama.local").as_deref(), Some("BIGMAMA"));
        assert_eq!(
            normalize_label("  m5-studio ").as_deref(),
            Some("M5-STUDIO")
        );
    }

    #[test]
    fn normalize_label_drops_unsafe_chars_and_empty() {
        // what this catches: labels feed a `[LABEL]` prefix that the
        // self-filter parses by bracket; stray `[`/`]`/spaces would break
        // that parse, and an all-junk hostname must fall through to None
        // so the caller tries the next source.
        assert_eq!(normalize_label("a[b] c/d").as_deref(), Some("ABCD"));
        assert_eq!(normalize_label("...").as_deref(), None);
        assert_eq!(normalize_label("---").as_deref(), None);
    }

    #[test]
    fn normalize_label_truncates_to_max_len() {
        // what this catches: an over-long hostname must be clamped so the
        // comment prefix stays scannable.
        let long = "a".repeat(MAX_LABEL_LEN + 10);
        assert_eq!(normalize_label(&long).map(|l| l.len()), Some(MAX_LABEL_LEN));
    }

    #[test]
    fn parse_comment_label_extracts_bracketed_prefix() {
        // what this catches: the label parse underpins the self-filter;
        // it must read the bracketed prefix and reject unlabelled bodies.
        assert_eq!(parse_comment_label("[M5] hello there"), Some("M5"));
        assert_eq!(
            parse_comment_label("  [BIGMAMA] leading ws"),
            Some("BIGMAMA")
        );
        assert_eq!(parse_comment_label("no prefix here"), None);
        assert_eq!(parse_comment_label("[] empty label"), None);
    }

    #[test]
    fn is_self_comment_matches_only_own_label() {
        // what this catches: the echo bug — watch must drop OUR posts and
        // keep peers'. A prefix that is a different label must NOT be
        // filtered.
        assert!(is_self_comment("[BIGMAMA] restarting daemon", "BIGMAMA"));
        assert!(!is_self_comment("[M5] on it", "BIGMAMA"));
        assert!(!is_self_comment("unlabelled note", "BIGMAMA"));
    }

    #[test]
    fn parse_comments_reads_id_and_body_skips_malformed() {
        // what this catches: comment parsing must extract (id, body) and
        // skip entries missing either, without aborting the whole read.
        let raw = r#"[
            {"id": 1, "body": "[M5] first", "user": {"login": "a"}},
            {"id": 2, "user": {"login": "b"}},
            {"body": "no id"},
            {"id": 3, "body": "[BIGMAMA] third"}
        ]"#;
        let comments = parse_comments(raw).unwrap();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].id, 1);
        assert_eq!(comments[0].body, "[M5] first");
        assert_eq!(comments[1].id, 3);
    }

    #[test]
    fn parse_comments_handles_empty_and_rejects_nonarray() {
        // what this catches: an empty thread is not an error, but a
        // non-array JSON payload is a loud failure (never silently treated
        // as "no messages").
        assert_eq!(parse_comments("").unwrap().len(), 0);
        assert_eq!(parse_comments("[]").unwrap().len(), 0);
        assert!(parse_comments(r#"{"message":"Not Found"}"#).is_err());
    }

    #[test]
    fn parse_created_gist_id_reads_trailing_url_segment() {
        // what this catches: `gh gist create` prints the gist URL; we must
        // recover the hex id from its final path segment and reject
        // non-hex noise.
        assert_eq!(
            parse_created_gist_id("https://gist.github.com/user/0123456789abcdef\n").as_deref(),
            Some("0123456789abcdef")
        );
        assert_eq!(parse_created_gist_id("not a url\n"), None);
    }

    #[test]
    fn valid_gist_id_bounds_and_charset() {
        // what this catches: the sentinel/id guard must accept real gist
        // ids and reject short/non-hex garbage that would poison re-find.
        assert!(valid_gist_id("0123456789abcdef"));
        assert!(!valid_gist_id("short"));
        assert!(!valid_gist_id("zzzzzzzzzzzz"));
    }
}
