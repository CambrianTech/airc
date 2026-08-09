//! Daemon self-update — keep the node current without anyone remembering.
//!
//! Falling behind canary silently breaks coordination (we lived it: stale
//! binaries, a lockfile drift hidden behind transitive deps, peers that can't
//! speak the current protocol). Manual `airc update` doesn't scale because *no
//! one remembers*. So automate it.
//!
//! The dangerous part is already solved: `airc update --auto`
//! (`airc-cli::update_commands::run_update_auto`) fetches + ff-pulls the channel,
//! and if HEAD moved, backs up the installed binary to `airc.prev`, rebuilds,
//! **smoke-tests the new binary, and ROLLS BACK on a bad build** — an auto-update
//! can never brick the node. If HEAD is unchanged it's a cheap no-op.
//!
//! This module is just the TRIGGER: on a long interval, when enabled and when
//! the node is idle, spawn a DETACHED `airc update --auto`. Detached so the
//! updater survives this daemon stopping itself mid-update (the updater stops the
//! old daemon, rebuilds, restarts a fresh one).
//!
//! ## Safety gates (why this is OK to default-on)
//! - **Idle-gated.** The caller supplies an `is_idle` predicate; we only fire
//!   when there is no live work to drop. The signal is [`mesh_is_quiet`] — no
//!   healthy peer with frames outstanding — supplied by the daemon from its
//!   delivery-truth stats. It is NOT connection count; see that function's docs
//!   for the day that cost.
//! - **Rollback.** `--auto` smoke-tests + rolls back, so a bad canary can't down
//!   the node.
//! - **No-op when current.** Restarts happen ONLY when canary actually moved, so
//!   steady state is a cheap periodic fetch, not connection churn.
//! - **Opt-out.** `AIRC_NO_AUTOUPDATE=1` disables entirely (CI, pinned deploys;
//!   release-binary installs already no-op `--auto` for lack of a source tree).

use std::time::Duration;

/// Default gap between self-update checks. Long on purpose: a node updates ~once
/// when canary moves, then no-ops. Override with `AIRC_AUTOUPDATE_INTERVAL_SECS`.
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(3600);
/// Floor — never let an override thrash the build toolchain.
pub const MIN_INTERVAL_SECS: u64 = 300;

const DISABLE_ENV: &str = "AIRC_NO_AUTOUPDATE";
const INTERVAL_ENV: &str = "AIRC_AUTOUPDATE_INTERVAL_SECS";

fn env_truthy(key: &str) -> bool {
    matches!(
        std::env::var(key).ok().as_deref(),
        Some("1") | Some("true") | Some("on") | Some("yes")
    )
}

/// Self-update is ON by default — the whole point (no one remembers). Opt OUT
/// with `AIRC_NO_AUTOUPDATE` truthy.
pub fn enabled() -> bool {
    !env_truthy(DISABLE_ENV)
}

/// The check interval, honoring `AIRC_AUTOUPDATE_INTERVAL_SECS` (clamped to a
/// `MIN_INTERVAL_SECS` floor so it can't thrash).
pub fn interval() -> Duration {
    std::env::var(INTERVAL_ENV)
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(|secs| Duration::from_secs(secs.max(MIN_INTERVAL_SECS)))
        .unwrap_or(DEFAULT_INTERVAL)
}

/// Should a tick fire the updater? Enabled AND idle. Kept pure so the policy is
/// testable apart from the I/O.
pub fn should_fire(enabled: bool, is_idle: bool) -> bool {
    enabled && is_idle
}

/// Is the mesh QUIET — i.e. is there no in-flight delivery this node would drop
/// by restarting? True when no healthy peer has frames outstanding.
///
/// ## Why this replaced "zero connected peers" (the bug this function exists for)
///
/// The original idle predicate was `connected_lan_peers == 0`. That is exactly
/// inverted from the intent: a node doing its job — connected to the mesh,
/// talking to its peer — is NEVER idle by that measure, so it could never
/// self-update. Only an already-isolated node updated. Lived on 2026-08-05: two
/// healthy, continuously-connected nodes both sat on a stale build for a full
/// day while a merged transport fix was available, and the human had to run the
/// update by hand on both — the precise outcome task #288 exists to abolish.
/// Connection COUNT is not work; outstanding UNCONFIRMED FRAMES are.
///
/// `attempts_since_ack` is the delivery-truth counter (#280): frames flushed to
/// a peer with no ack back yet. Zero across every peer means nothing is in
/// flight, so a restart drops nothing — regardless of how many peers are
/// connected.
///
/// **A `suspect` peer cannot veto.** A half-open connection's outstanding frames
/// are never going to be acked (that is what `suspect` means — route refresh is
/// about to drop and re-dial it). Counting those as live work would let one
/// wedged pipe block self-update FOREVER: a strictly worse version of the bug
/// being fixed here, and one that would present identically — a node that just
/// never updates.
///
/// Vacuously true when there are no peers at all, which subsumes the old
/// zero-connections case rather than dropping it.
pub fn mesh_is_quiet(peers: impl IntoIterator<Item = (u32, bool)>) -> bool {
    peers
        .into_iter()
        .all(|(attempts_since_ack, suspect)| suspect || attempts_since_ack == 0)
}

/// Spawn a DETACHED `airc update --auto` — its own process group so it survives
/// the daemon stopping itself mid-update. Best-effort: returns the spawn error;
/// it does NOT wait (the updater outlives this process).
pub fn spawn_detached_update() -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("update").arg("--auto");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // New process group: a teardown that signals the daemon's group (the
        // updater's own `stop_daemon`) won't also kill the updater.
        cmd.process_group(0);
    }
    cmd.spawn().map(|_child| ())
}

/// Periodic self-update loop. Reusable + decoupled: the caller supplies the
/// `is_idle` predicate (the mesh-idle signal) and spawns this on the daemon
/// runtime, cancelling via `shutdown`. Returns immediately if disabled — opt-out
/// means zero ticks. The first interval is awaited (never update on boot).
pub async fn run<F>(shutdown: &tokio::sync::Notify, is_idle: F)
where
    F: Fn() -> bool,
{
    if !enabled() {
        return;
    }
    let mut ticker = tokio::time::interval(interval());
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await; // consume the immediate first tick — don't update on boot
    loop {
        tokio::select! {
            _ = shutdown.notified() => break,
            _ = ticker.tick() => {
                if should_fire(enabled(), is_idle()) {
                    if let Err(error) = spawn_detached_update() {
                        // Best-effort: a failed spawn is logged (not silent) and
                        // retried next interval. The daemon has no tracing dep;
                        // stderr is its log sink.
                        eprintln!("airc auto-update: failed to spawn updater: {error}");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // what this catches: the firing decision — only when BOTH enabled and idle.
    // A regression that fired while busy would drop live streams; one that fired
    // while disabled would ignore the opt-out.
    #[test]
    fn fires_only_when_enabled_and_idle() {
        assert!(should_fire(true, true));
        assert!(!should_fire(true, false), "busy node must not auto-restart");
        assert!(!should_fire(false, true), "opt-out must be honored");
        assert!(!should_fire(false, false));
    }

    // what this catches (THE #288/#272 regression): a CONNECTED but quiet node
    // must still be eligible to self-update. The predicate this replaced was
    // `connected_lan_peers == 0`, under which a healthy peered node could never
    // update — two live nodes sat on a stale build for a day on 2026-08-05 and a
    // human had to run the fix by hand on both. Quiet is about outstanding
    // frames, not about having peers.
    #[test]
    fn a_connected_but_quiet_node_is_still_eligible_to_update() {
        // Three peers, all connected, none with unacked frames → quiet.
        assert!(mesh_is_quiet([(0, false), (0, false), (0, false)]));
        // No peers at all → vacuously quiet (subsumes the old zero-conn case).
        assert!(mesh_is_quiet([]));
    }

    // what this catches: the other direction — a node with real in-flight
    // delivery must NOT restart out from under it. One unacked peer vetoes.
    #[test]
    fn outstanding_frames_veto_the_update() {
        assert!(!mesh_is_quiet([(3, false)]));
        assert!(
            !mesh_is_quiet([(0, false), (1, false)]),
            "one busy peer among idle ones still vetoes"
        );
    }

    // what this catches: a wedged half-open pipe must not block self-update
    // FOREVER. A suspect peer's outstanding frames will never be acked (route
    // refresh is about to drop and re-dial it), so counting them as live work
    // would be a strictly worse version of the bug above — a node that simply
    // never updates again, presenting identically.
    #[test]
    fn a_suspect_peer_cannot_veto_forever() {
        assert!(
            mesh_is_quiet([(9999, true)]),
            "suspect peer is not live work"
        );
        assert!(
            mesh_is_quiet([(0, false), (42, true)]),
            "healthy-idle + suspect-stuck is still quiet"
        );
        assert!(
            !mesh_is_quiet([(42, true), (1, false)]),
            "but a HEALTHY peer's outstanding frames still veto"
        );
    }

    // what this catches: the interval floor — an override can't drive the check
    // below MIN_INTERVAL_SECS and thrash the build toolchain.
    #[test]
    fn interval_override_is_floored() {
        // Default when unset.
        std::env::remove_var(INTERVAL_ENV);
        assert_eq!(interval(), DEFAULT_INTERVAL);
        // A too-small override is clamped up to the floor.
        std::env::set_var(INTERVAL_ENV, "5");
        assert_eq!(interval(), Duration::from_secs(MIN_INTERVAL_SECS));
        // A sane override is honored.
        std::env::set_var(INTERVAL_ENV, "7200");
        assert_eq!(interval(), Duration::from_secs(7200));
        std::env::remove_var(INTERVAL_ENV);
    }

    // what this catches: default-ON (no one remembers to enable) + the opt-out.
    #[test]
    fn enabled_by_default_opt_out_honored() {
        std::env::remove_var(DISABLE_ENV);
        assert!(enabled(), "self-update is on by default");
        std::env::set_var(DISABLE_ENV, "1");
        assert!(!enabled(), "AIRC_NO_AUTOUPDATE disables it");
        std::env::remove_var(DISABLE_ENV);
    }
}
