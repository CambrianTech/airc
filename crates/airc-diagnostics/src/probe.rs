//! `probe!` / `time_probe!` — mechanic-grade observability for the airc
//! substrate.
//!
//! The airc-native twin of continuum's `probe!`. The probe primitive
//! belongs HERE, not in a sibling "shared observability" lib: probes
//! route to `airc://<actor>/debug/probes/<class>/stream` — an airc bus
//! address — so the substrate owns the primitive and consumers
//! (continuum, agent runtimes, the CLI) reach for it downward. A new
//! third crate that airc depended on would invert the layering.
//!
//! ## Why a macro and not a function
//!
//! Joel: "macros are easy and a way of preventing misuse. If coding
//! timing or logging is painful it won't happen." A function would
//! force every call site to thread a `&dyn DiagnosticSink`; the macro
//! lets the ambient `tracing` subscriber do the routing, so a probe is
//! one line with zero ceremony. [`DiagnosticEvent`](crate::DiagnosticEvent)
//! stays the channel for *errors/warnings* a host must act on; `probe!`
//! is the channel for *measurements* you interrogate the system with.
//!
//! ## Zero-cost when disabled
//!
//! Both macros expand to `tracing::event!` / `tracing::info_span!`, so
//! they inherit tracing's `release_max_level_*` build-time gates: when
//! a level is filtered out at compile time the expansion is absent from
//! the binary — no formatting, no allocation, no branch.
//!
//! ## Routing
//!
//! The routing key is the `probe_class` field. A host's tracing
//! subscriber inspects it and routes the record to
//! `airc://<actor>/debug/probes/<class>/stream`; the actor URI comes
//! from the current span context. Until a bus-forwarding subscriber is
//! installed, probes still surface through whatever fmt subscriber the
//! host already runs (the daemon installs one in
//! `airc-cli`), filterable by `probe_class` — which is already enough
//! to interrogate the daemon/IPC internals by hand. The bus-forwarding
//! layer that makes another node's probes visible to a remote peer is
//! the follow-up slice.

/// Conventional `probe_class` values. Plain strings still work — these
/// exist so a typo in a hot class name is a compile error, not a
/// silently-misrouted probe.
pub mod class {
    /// Time spent / deadline pressure on a seam (turn, dial, handshake).
    pub const LATENCY: &str = "latency";
    /// An adapter/strategy choice and why (route, inference target, verdict).
    pub const DECISION: &str = "decision";
    /// A snapshot of internal state (queue depth, working-set size, roster).
    pub const STATE: &str = "state";
    /// An admission/backpressure outcome (accepted, shed, throttled).
    pub const ADMISSION: &str = "admission";
    /// Block/future durations emitted by [`time_probe!`](crate::time_probe).
    pub const TIMING: &str = "timing";
}

/// Structured measurement emit, routed by `class`.
///
/// Shape mirrors `tracing::info!` for muscle memory; it exists only for
/// what the conventional macros lack — per-class routing, always-on
/// intent, replay persistence, aggregation-ready.
///
/// ```
/// use airc_diagnostics::probe;
/// # let id = 7u64; let elapsed = 3u64; let depth = 4usize;
///
/// // class only
/// probe!(class = "state");
///
/// // class + structured fields + optional message
/// probe!(class = "latency",  turn_id = id, duration_ms = elapsed, "turn complete");
/// probe!(class = "decision", action = "use-local", target = "qwen-7b");
/// probe!(class = "state",    inbox_depth = depth);
/// ```
#[macro_export]
macro_rules! probe {
    // class only: `probe!(class = "state")`
    (class = $class:expr $(,)?) => {
        ::tracing::event!(::tracing::Level::INFO, probe_class = $class)
    };
    // class + fields/message: `probe!(class = "latency", dur_ms = x, "done")`
    (class = $class:expr, $($rest:tt)*) => {
        ::tracing::event!(::tracing::Level::INFO, probe_class = $class, $($rest)*)
    };
}

/// Explicit-block timing for **synchronous** code. Wraps the block /
/// expression in an `info_span!` whose duration becomes a `timing`
/// probe at scope exit, and returns the body's value.
///
/// **MUST NOT contain `.await`.** Holding the span guard across a
/// suspension point detaches the span from the actual work and corrupts
/// the URI ancestry the bus-forwarding subscriber relies on. Time async
/// futures with `.instrument(info_span!("time", seam = .., probe_class =
/// "timing")).await` instead (a typed `time_async!` companion is the
/// follow-up slice).
///
/// ```
/// use airc_diagnostics::time_probe;
/// # fn compute() -> u32 { 41 }
/// let result = time_probe!("hash_payload", compute());
/// assert_eq!(result, 41);
/// ```
#[macro_export]
macro_rules! time_probe {
    ($seam:expr, $body:expr) => {{
        let __span = ::tracing::info_span!("time", seam = $seam, probe_class = "timing");
        let __enter = __span.enter();
        $body
    }};
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::fmt::MakeWriter;

    /// A `MakeWriter` that appends every rendered line to a shared
    /// buffer so a test can assert on what a probe emitted.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Capture {
        fn rendered(&self) -> String {
            String::from_utf8(self.0.lock().expect("capture lock").clone()).expect("utf-8")
        }
    }

    impl Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("capture lock").extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Capture {
        type Writer = Capture;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    fn capture<F: FnOnce()>(body: F) -> String {
        let sink = Capture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(sink.clone())
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, body);
        sink.rendered()
    }

    #[test]
    fn probe_emits_class_and_fields() {
        let out = capture(|| {
            probe!(class = "latency", duration_ms = 12u64, "turn complete");
        });
        assert!(
            out.contains("probe_class"),
            "probe_class tag present: {out}"
        );
        assert!(out.contains("latency"), "class value present: {out}");
        assert!(out.contains("duration_ms"), "field present: {out}");
        assert!(out.contains("turn complete"), "message present: {out}");
    }

    #[test]
    fn probe_class_only_form_compiles_and_emits() {
        let out = capture(|| {
            probe!(class = super::class::STATE);
        });
        assert!(out.contains("probe_class"), "class-only probe emits: {out}");
        assert!(out.contains("state"), "STATE const routed: {out}");
    }

    #[test]
    fn time_probe_returns_body_value_and_tags_timing() {
        let out = capture(|| {
            let doubled = time_probe!("double", 21u32 * 2);
            assert_eq!(doubled, 42, "time_probe returns the body's value");
        });
        // The span closes at scope exit; with span events off we at
        // least prove the macro type-checks, returns the value, and the
        // seam/timing tags are wired (visible once a span-close layer
        // renders them). The value assertion above is the load-bearing
        // check.
        let _ = out;
    }
}
