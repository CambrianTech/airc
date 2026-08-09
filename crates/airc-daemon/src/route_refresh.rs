//! Card 625abe6d slice 2 — daemon-resident periodic route refresh.
//!
//! Slice 1 taught `refresh_route_discovery` to dial every enrolled
//! peer's stored endpoints outbound, but nothing invoked it except an
//! operator running `airc transport health`. That fails the card's
//! design constraints: route health-checks must be CONTINUOUS, and
//! sleep/wake or a daemon restart must re-establish routes with zero
//! operator action. This module is the daemon-side clock for that —
//! it owns *when* a refresh runs; *what* one refresh does is supplied
//! by the daemon host as a closure, because the concrete substrate
//! handle lives in `airc-lib`, which this crate must not depend on
//! (the CLI host wires the two together in `run_daemon`).
//!
//! Failure posture (self-heal doctrine): the loop never exits on a
//! failed refresh — the closure reports failures loudly through the
//! daemon's diagnostic sink and the clock keeps ticking. The only
//! exit is the daemon's own shutdown notifier.

use std::future::Future;
use std::time::Duration;

use tokio::sync::Notify;

/// How long after daemon start the FIRST refresh runs: immediately.
///
/// Card 625abe6d: "sleep/wake + daemon restart re-establish routes
/// with zero operator action." Self-healing join (auto-reconnect
/// after restart): every daemon restart DROPS the LAN transport
/// sessions, and live two-machine evidence showed reconnection never
/// converged unaided — a manual `airc dial` fixed it every time,
/// because peers only redialed on a later tick / next send. The host
/// (`run_daemon`) spawns this loop only after the daemon's identity,
/// trust store, and router are built, so there is nothing left to
/// "settle": dial the stored live peers NOW. The refresh itself
/// still honors the dial-quarantine gates and ghost-peer freshness
/// (fresh-enough endpoints only), so an immediate first tick can
/// never stampede dead endpoints.
pub const FIRST_REFRESH_DELAY: Duration = Duration::ZERO;

/// Steady-state refresh cadence.
///
/// 60s bounds route-outage detection (and stored-endpoint redial) to
/// about a minute — the same liveness granularity as the 60s agent
/// heartbeat cadence. It also leaves wide headroom over the refresh's
/// own worst case: each stored-endpoint dial is bounded at 3s
/// (`PEER_DIAL_TIMEOUT`, slice 1) and dials run sequentially, so even
/// a registry poisoned with a dozen tarpit endpoints (~36s) finishes
/// inside one interval. Overruns still cannot pile up: the interval
/// is measured from refresh COMPLETION to the next start (see
/// [`run_periodic_refresh`]), never start-to-start.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// Minimum spacing between refreshes, enforced on the WAKE path.
///
/// The wake nudge exists so a real reconnect doesn't wait out a full
/// [`REFRESH_INTERVAL`]. But it is fired by the session-drop observer, and
/// a refresh dials every stored peer — so on a node whose registry is
/// mostly dead peers, refresh manufactures the drops that nudge the next
/// refresh, forever. This floor is what makes that cycle terminate: at
/// worst 4 refreshes/minute instead of ~13, and the gh budget the account
/// registry needs is no longer spent by a spin.
///
/// 15s is chosen against the failure it bounds, not tuned: it keeps a
/// genuine reconnect fast (well under the 60s interval it exists to beat)
/// while capping the dial rate low enough that a poisoned registry cannot
/// exhaust a 30-req/60s budget on discovery alone.
pub const MIN_REFRESH_SPACING: Duration = Duration::from_secs(15);

/// Pure scheduling rule: how long to wait before the next refresh,
/// given how many refreshes have already completed.
///
/// Tick 0 (nothing completed yet) waits [`FIRST_REFRESH_DELAY`]
/// (zero — a restarted daemon redials stored live peers immediately
/// instead of waiting out a tick); every later tick waits the steady
/// [`REFRESH_INTERVAL`].
pub fn delay_before_refresh(completed_refreshes: u64) -> Duration {
    if completed_refreshes == 0 {
        FIRST_REFRESH_DELAY
    } else {
        REFRESH_INTERVAL
    }
}

/// Drive `refresh` on the daemon clock until `shutdown` fires.
///
/// Pile-up guard: the refresh future is awaited IN this loop — the
/// next delay only starts after the previous refresh completes, so
/// two refreshes can never run concurrently by construction (a
/// refresh involves up-to-3s-per-endpoint outbound dials; a timer
/// that fired regardless of in-flight work would stack them).
///
/// Shutdown: one `Notified` future is created up front and kept
/// pinned across iterations — the same discipline as `server::run`.
/// The daemon's Stop handler signals with `notify_waiters()`, which
/// wakes only waiters registered at that instant and stores no
/// permit; re-creating `notified()` each turn would leave windows
/// where the signal is lost and the loop never exits.
/// `wake` is the event-driven nudge (#240): the account-registry loop
/// notifies it the instant an import lands fresh beacons / endpoints in the
/// trust store, so a refresh runs IMMEDIATELY instead of waiting out the
/// remaining interval — freshly-advertised endpoints for a disconnected peer
/// are dialed at once, not up to `REFRESH_INTERVAL` later. The nudge only
/// short-circuits the *wait*; the refresh work is unchanged (connected peers
/// skipped, quarantine/ghost gates apply), so a nudge in steady state is a
/// cheap no-op. `Notify` stores one permit, so a nudge landing DURING a
/// refresh is consumed by the next iteration's wait (never lost, coalesced).
pub async fn run_periodic_refresh<F, Fut>(shutdown: &Notify, wake: &Notify, mut refresh: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = ()>,
{
    let notified = shutdown.notified();
    tokio::pin!(notified);

    let mut completed: u64 = 0;
    let mut last_completed: Option<tokio::time::Instant> = None;
    loop {
        // Wait for the interval to elapse OR an external wake nudge, whichever
        // comes first. A fresh `notified()` per iteration so a permit stored
        // while `refresh()` ran fires the next wait immediately.
        {
            let woken = wake.notified();
            tokio::pin!(woken);
            tokio::select! {
                biased;
                _ = &mut notified => return,
                _ = &mut woken => {}
                () = tokio::time::sleep(delay_before_refresh(completed)) => {}
            }
        }
        // Floor the WAKE path. The nudge is edge-triggered on a session
        // drop, and a refresh DIALS every stored peer — so on a node whose
        // registry holds mostly-dead peers, each refresh manufactures the
        // drops that nudge the next one. Measured live on the M5
        // (2026-08-04): 44,704 relay self-elections and 4,951 exhausted-gh
        // -budget errors in a single daemon log, refresh re-entering every
        // ~4.5s against a 60s interval, 52 enrolled peers and zero
        // reachable — the loop starved the account registry that discovery
        // depends on, so it could never climb out.
        //
        // The nudge stays (a real reconnect must not wait out 60s); it just
        // cannot re-enter faster than this floor, which bounds the cycle at
        // 4/min instead of unbounded. The timer path is unaffected — it
        // already waits far longer than the floor.
        if let Some(last) = last_completed {
            let since = last.elapsed();
            if since < MIN_REFRESH_SPACING {
                tokio::select! {
                    biased;
                    _ = &mut notified => return,
                    () = tokio::time::sleep(MIN_REFRESH_SPACING - since) => {}
                }
            }
        }
        tokio::select! {
            biased;
            _ = &mut notified => return,
            () = refresh() => {
                completed = completed.saturating_add(1);
                last_completed = Some(tokio::time::Instant::now());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::sync::Notify;

    use super::{
        delay_before_refresh, run_periodic_refresh, FIRST_REFRESH_DELAY, MIN_REFRESH_SPACING,
        REFRESH_INTERVAL,
    };

    /// Pin the scheduling rule: a restarted daemon dials IMMEDIATELY
    /// (self-healing join: restart drops every transport session, and
    /// waiting out even a short warm-up left two live machines dark
    /// until a manual `airc dial`), then settles into the steady
    /// cadence.
    #[test]
    fn first_refresh_is_immediate_then_steady_interval() {
        assert_eq!(
            delay_before_refresh(0),
            Duration::ZERO,
            "daemon boot must schedule the first stored-peer dial pass \
             immediately, not after a warm-up tick"
        );
        assert_eq!(delay_before_refresh(1), REFRESH_INTERVAL);
        assert_eq!(delay_before_refresh(1_000), REFRESH_INTERVAL);
        assert!(
            FIRST_REFRESH_DELAY < REFRESH_INTERVAL,
            "the first refresh must run sooner than a steady-state tick, \
             or a daemon restart waits out a full interval before \
             re-establishing routes"
        );
    }

    /// The restart-shaped regression (self-healing join, auto-reconnect
    /// after restart): a daemon boot must schedule its first refresh —
    /// the stored-live-peer dial pass — IMMEDIATELY, not after a warm-up
    /// window and not after a steady interval. Mutation check: restoring
    /// any nonzero FIRST_REFRESH_DELAY fails the count==1 assert at t=0+.
    #[tokio::test(start_paused = true)]
    async fn boot_schedules_the_first_refresh_immediately() {
        let shutdown = Arc::new(Notify::new());
        let count = Arc::new(AtomicUsize::new(0));
        let wake = Arc::new(Notify::new());

        let task = tokio::spawn({
            let shutdown = shutdown.clone();
            let wake = wake.clone();
            let count = count.clone();
            async move {
                run_periodic_refresh(&shutdown, &wake, move || {
                    let count = count.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                    }
                })
                .await;
            }
        });

        // With paused time, this sleep advances the clock by only 1ms —
        // the first refresh can only have run if it was scheduled at
        // t=0 (Duration::ZERO), never a warm-up or interval later.
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "daemon boot must run the first stored-peer dial pass immediately"
        );

        // Just before the first steady interval elapses: still one.
        // (-2ms: the probe above already advanced the clock 1ms past
        // the completion instant the next delay is measured from.)
        tokio::time::sleep(REFRESH_INTERVAL - Duration::from_millis(2)).await;
        assert_eq!(count.load(Ordering::SeqCst), 1, "steady cadence respected");

        // Crossing the steady interval: the second refresh.
        tokio::time::sleep(Duration::from_millis(2)).await;
        assert_eq!(
            count.load(Ordering::SeqCst),
            2,
            "second refresh on interval"
        );

        shutdown.notify_waiters();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("loop must exit on shutdown")
            .expect("loop task must not panic");
    }

    /// The pile-up guard: a refresh that takes LONGER than the
    /// interval (worst-case dial walks) must delay the next tick, not
    /// stack a second refresh on top of it.
    #[tokio::test(start_paused = true)]
    async fn refreshes_never_overlap_when_one_outlasts_the_interval() {
        let shutdown = Arc::new(Notify::new());
        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_in_flight = Arc::new(AtomicUsize::new(0));
        let completed = Arc::new(AtomicUsize::new(0));
        let wake = Arc::new(Notify::new());

        let task = tokio::spawn({
            let shutdown = shutdown.clone();
            let wake = wake.clone();
            let in_flight = in_flight.clone();
            let max_in_flight = max_in_flight.clone();
            let completed = completed.clone();
            async move {
                run_periodic_refresh(&shutdown, &wake, move || {
                    let in_flight = in_flight.clone();
                    let max_in_flight = max_in_flight.clone();
                    let completed = completed.clone();
                    async move {
                        let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                        max_in_flight.fetch_max(now, Ordering::SeqCst);
                        // A pathological refresh: 3x the interval.
                        tokio::time::sleep(REFRESH_INTERVAL * 3).await;
                        in_flight.fetch_sub(1, Ordering::SeqCst);
                        completed.fetch_add(1, Ordering::SeqCst);
                    }
                })
                .await;
            }
        });

        // Enough simulated time for several full slow refreshes.
        tokio::time::sleep(REFRESH_INTERVAL * 12).await;

        assert!(
            completed.load(Ordering::SeqCst) >= 2,
            "the loop must keep refreshing despite slow refreshes"
        );
        assert_eq!(
            max_in_flight.load(Ordering::SeqCst),
            1,
            "refreshes must never overlap — the next delay starts only \
             after the previous refresh completes"
        );

        shutdown.notify_waiters();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("loop must exit on shutdown")
            .expect("loop task must not panic");
    }

    /// Shutdown mid-sleep exits the loop without waiting out the
    /// delay — the daemon's Stop must not hang on the refresh clock.
    #[tokio::test(start_paused = true)]
    async fn shutdown_stops_the_loop_mid_sleep() {
        let shutdown = Arc::new(Notify::new());
        let count = Arc::new(AtomicUsize::new(0));
        let wake = Arc::new(Notify::new());

        let task = tokio::spawn({
            let shutdown = shutdown.clone();
            let wake = wake.clone();
            let count = count.clone();
            async move {
                run_periodic_refresh(&shutdown, &wake, move || {
                    let count = count.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                    }
                })
                .await;
            }
        });

        // The first refresh runs immediately at boot; 1s in, the loop
        // is parked on its STEADY-interval sleep with the pinned
        // shutdown waiter registered.
        tokio::time::sleep(Duration::from_secs(1)).await;
        shutdown.notify_waiters();

        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("loop must exit promptly on shutdown")
            .expect("loop task must not panic");
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "exactly the boot-immediate refresh ran before shutdown — the \
             mid-interval sleep was interrupted, not waited out"
        );
    }

    /// #240 event-driven heal: a `wake` nudge (the registry loop after an
    /// import) runs a refresh IMMEDIATELY, mid-interval — freshly-imported
    /// endpoints are dialed at once instead of waiting out the remaining
    /// `REFRESH_INTERVAL`. Mutation check: dropping the `woken` arm from the
    /// wait select fails the "count==2 well before the interval" assert (the
    /// nudge would then be ignored until the timer fired).
    #[tokio::test(start_paused = true)]
    async fn a_wake_nudge_runs_a_refresh_before_the_interval_elapses() {
        let shutdown = Arc::new(Notify::new());
        let wake = Arc::new(Notify::new());
        let count = Arc::new(AtomicUsize::new(0));

        let task = tokio::spawn({
            let shutdown = shutdown.clone();
            let wake = wake.clone();
            let count = count.clone();
            async move {
                run_periodic_refresh(&shutdown, &wake, move || {
                    let count = count.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                    }
                })
                .await;
            }
        });

        // Boot-immediate refresh lands, then the loop parks on its steady
        // interval. Advance to the MIDPOINT of that interval — no timer
        // refresh is due yet.
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert_eq!(count.load(Ordering::SeqCst), 1, "boot refresh only");
        tokio::time::sleep(REFRESH_INTERVAL / 2).await;
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "mid-interval: the timer has not fired a second refresh"
        );

        // Nudge — a fresh import just landed. The refresh must run NOW, not
        // at the far end of the interval.
        wake.notify_one();
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert_eq!(
            count.load(Ordering::SeqCst),
            2,
            "the wake nudge ran a refresh immediately, mid-interval — not \
             deferred to the next timer tick"
        );

        shutdown.notify_waiters();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("loop must exit on shutdown")
            .expect("loop task must not panic");
    }

    /// what this catches: the live ghost measured on the M5, 2026-08-04 —
    /// 44,704 relay self-elections and 4,951 exhausted-gh-budget errors in
    /// one daemon log, refresh re-entering every ~4.5s against a 60s
    /// interval. The wake nudge is fired by the session-drop observer, and
    /// a refresh dials every stored peer, so on a registry full of dead
    /// peers each refresh manufactures the drops that nudge the next one.
    /// A storm of nudges must be FLOORED, or discovery starves the gh
    /// budget it depends on and the node never finds a peer again.
    #[tokio::test(start_paused = true)]
    async fn a_wake_storm_cannot_refresh_faster_than_the_floor() {
        let shutdown = Arc::new(Notify::new());
        let wake = Arc::new(Notify::new());
        let count = Arc::new(AtomicUsize::new(0));

        let loop_shutdown = shutdown.clone();
        let loop_wake = wake.clone();
        let loop_count = count.clone();
        let handle = tokio::spawn(async move {
            run_periodic_refresh(&loop_shutdown, &loop_wake, || {
                let count = loop_count.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                }
            })
            .await;
        });

        // 60 virtual seconds of continuous nudging — the drop-storm shape.
        for _ in 0..600 {
            wake.notify_one();
            tokio::time::advance(Duration::from_millis(100)).await;
        }
        shutdown.notify_waiters();
        let _ = handle.await;

        // Immediate first refresh (FIRST_REFRESH_DELAY is ZERO) plus at most
        // one per MIN_REFRESH_SPACING across the window.
        let refreshes = count.load(Ordering::SeqCst);
        let ceiling = 1 + (60 / MIN_REFRESH_SPACING.as_secs() as usize) + 1;
        assert!(
            refreshes <= ceiling,
            "a wake storm must be floored: {refreshes} refreshes in 60s \
             (ceiling {ceiling}). Unfloored, this re-enters as fast as the \
             dial sweep — the 44,704-self-election ghost."
        );
    }
}
