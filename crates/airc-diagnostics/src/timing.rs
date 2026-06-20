//! Seam timing accumulator — summed per-seam latency for mechanic-grade
//! localization.
//!
//! Complements [`probe!`](crate::probe) / [`time_probe!`](crate::time_probe):
//! those are the span/event channel (one event per hit, routed via `tracing`);
//! this is the SUMMED channel — aggregate count + total + max per named seam
//! across many iterations, the shape a latency bench reads to find "where did
//! the milliseconds go." Sink-free, subscriber-free, std-only.
//!
//! **Opt-in, near-zero cost when off.** Recording is gated by an atomic flag
//! ([`enable`]/[`disable`], default off). A seam call site on a hot path that
//! isn't being profiled pays one relaxed atomic load and nothing else — no
//! `Instant`, no lock. A bench calls [`enable`] + [`reset`] around its window
//! and reads [`snapshot`].

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Aggregated samples for one seam.
#[derive(Clone, Copy, Default, Debug)]
pub struct SeamStat {
    pub count: u64,
    pub total_ns: u64,
    pub max_ns: u64,
}

impl SeamStat {
    pub fn avg_ns(&self) -> u64 {
        self.total_ns.checked_div(self.count).unwrap_or(0)
    }
    pub fn avg_us(&self) -> u64 {
        self.avg_ns() / 1_000
    }
}

fn registry() -> &'static Mutex<BTreeMap<&'static str, SeamStat>> {
    static REG: OnceLock<Mutex<BTreeMap<&'static str, SeamStat>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Turn recording on (a bench calls this before its measured window).
pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

/// Turn recording off (the production default).
pub fn disable() {
    ENABLED.store(false, Ordering::Relaxed);
}

#[inline]
fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Record one duration sample (nanoseconds) for `seam`. No-op when disabled.
pub fn record(seam: &'static str, nanos: u64) {
    if !is_enabled() {
        return;
    }
    let mut map = registry().lock().unwrap_or_else(|p| p.into_inner());
    let stat = map.entry(seam).or_default();
    stat.count += 1;
    stat.total_ns += nanos;
    if nanos > stat.max_ns {
        stat.max_ns = nanos;
    }
}

/// Time a synchronous closure and record it under `seam`. When disabled, runs
/// the closure directly — not even an `Instant::now()`.
#[inline]
pub fn timed<T>(seam: &'static str, f: impl FnOnce() -> T) -> T {
    if !is_enabled() {
        return f();
    }
    let start = Instant::now();
    let value = f();
    record(seam, start.elapsed().as_nanos() as u64);
    value
}

/// Snapshot every seam (sorted by name).
pub fn snapshot() -> Vec<(&'static str, SeamStat)> {
    registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .map(|(k, v)| (*k, *v))
        .collect()
}

/// Clear all samples (a bench calls this before its measured window).
pub fn reset() {
    registry().lock().unwrap_or_else(|p| p.into_inner()).clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    // what this catches: disabled = no-op (the production default costs nothing);
    // enabled = aggregates count/total/max; reset clears.
    #[test]
    fn records_only_when_enabled_and_aggregates() {
        reset();
        disable();
        record("seam.a", 100);
        assert!(snapshot().is_empty(), "disabled records nothing");

        enable();
        record("seam.a", 100);
        record("seam.a", 300);
        let snap = snapshot();
        let (name, stat) = snap.iter().find(|(n, _)| *n == "seam.a").expect("seam.a");
        assert_eq!(*name, "seam.a");
        assert_eq!(stat.count, 2);
        assert_eq!(stat.avg_ns(), 200);
        assert_eq!(stat.max_ns, 300);

        // timed() runs the closure and records when enabled.
        let out = timed("seam.b", || 7);
        assert_eq!(out, 7);
        assert!(snapshot().iter().any(|(n, _)| *n == "seam.b"));

        reset();
        assert!(snapshot().is_empty(), "reset clears");
        disable();
    }
}
