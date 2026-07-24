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
pub async fn run_periodic_refresh<F, Fut>(shutdown: &Notify, mut refresh: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = ()>,
{
    let notified = shutdown.notified();
    tokio::pin!(notified);

    let mut completed: u64 = 0;
    loop {
        tokio::select! {
            biased;
            _ = &mut notified => return,
            () = tokio::time::sleep(delay_before_refresh(completed)) => {}
        }
        tokio::select! {
            biased;
            _ = &mut notified => return,
            () = refresh() => {
                completed = completed.saturating_add(1);
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
        delay_before_refresh, run_periodic_refresh, FIRST_REFRESH_DELAY, REFRESH_INTERVAL,
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

        let task = tokio::spawn({
            let shutdown = shutdown.clone();
            let count = count.clone();
            async move {
                run_periodic_refresh(&shutdown, move || {
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

        let task = tokio::spawn({
            let shutdown = shutdown.clone();
            let in_flight = in_flight.clone();
            let max_in_flight = max_in_flight.clone();
            let completed = completed.clone();
            async move {
                run_periodic_refresh(&shutdown, move || {
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

        let task = tokio::spawn({
            let shutdown = shutdown.clone();
            let count = count.clone();
            async move {
                run_periodic_refresh(&shutdown, move || {
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
}
