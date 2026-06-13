//! Envelope headers — small routable metadata carried beside opaque bodies.
//!
//! Headers are the airc equivalent of HTTP headers: optional, deterministic,
//! cheap to inspect, and independent from the body payload. The substrate may
//! route, filter, retain, or diagnose using these values without parsing
//! consumer JSON. Consumers own their own namespaces (`forge.*`,
//! `openclaw.*`, `hermes.*`, `continuum.*`, `x-*`).

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// Deterministically ordered string headers.
///
/// `BTreeMap` is the on-wire shape: keys serialise in stable order so the
/// envelope encoding is deterministic (matters for content-addressing,
/// caching, and the encode-once doctrine). Lookups are `O(log n)` with a
/// string compare at each level, which is fine for one-shot inspection
/// but costly under bus fan-out — see [`HeaderView`] for the routing-side
/// fast path (card 512fd8a1).
pub type Headers = BTreeMap<String, String>;

/// Borrowed view of [`Headers`] decoupled from the on-wire BTreeMap
/// representation. Card 512fd8a1.
///
/// The substrate's encoding-discipline doctrine says the on-wire
/// shape must be deterministic ([`Headers`] is BTreeMap for stable
/// key ordering). The routing layer, by contrast, only cares about
/// O(1) lookup. `HeaderView` is the seam: build once per event from
/// the BTreeMap, fan out to consumers via [`HeaderFilter::matches_view`].
/// Strings are borrowed from the source `Headers` — no copies of
/// values, just a small temporary hash table whose lifetime is tied
/// to the event.
///
/// ## When this pays off
///
/// At realistic substrate scales (10 headers, 100 consumers) the
/// HashMap build cost roughly cancels the per-lookup savings vs the
/// BTreeMap path — both clock ~200 ns/op (measured on M2 release;
/// see `bench_*` tests). At 50 headers × 1000 consumers diverse-key
/// fan-out the two paths still tie, because BTreeMap at depth 6 is
/// already cheap and the branch predictor warms.
///
/// Where `HeaderView` genuinely helps:
///   - very dense headers (50+) with very few hot keys (cache
///     thrashing on the BTreeMap path),
///   - workloads where the same view is reused MANY times (consumer
///     resubscribes against the same event), and
///   - keeping the wire shape (`BTreeMap`) split from the routing
///     shape so each can evolve independently. This last reason is
///     why the API exists even at parity perf — the encoding-
///     discipline split is the substrate property we want.
///
/// Rule of thumb: prefer [`HeaderFilter::matches`] for one-shot.
/// Build a `HeaderView` when you know the consumer count is large
/// AND the headers are dense.
pub struct HeaderView<'a> {
    inner: HashMap<&'a str, &'a str>,
}

impl<'a> HeaderView<'a> {
    /// Project a `Headers` map into a borrowed view in `O(n)`. Build
    /// ONCE per event; reuse across every consumer's
    /// [`HeaderFilter::matches_view`] call in the fan-out loop.
    pub fn build(headers: &'a Headers) -> Self {
        let inner: HashMap<&'a str, &'a str> = headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        Self { inner }
    }

    /// Lookup a header value by key in `O(1)` amortised. Returns
    /// `None` for absent keys, same contract as `BTreeMap::get`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.inner.get(key).copied()
    }
}

/// Match predicate for subscription fan-out.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeaderFilter {
    /// Matches every envelope — the default (no header scoping).
    #[default]
    Any,
    Exact {
        key: String,
        value: String,
    },
    Prefix {
        key: String,
        value_prefix: String,
    },
    All(Vec<HeaderFilter>),
    AnyOf(Vec<HeaderFilter>),
}

impl HeaderFilter {
    /// One-shot match against a [`Headers`] map. Suitable when a
    /// single filter is being evaluated against a single event; for
    /// bus fan-out (one event, many filters), use [`Self::matches_view`]
    /// with a pre-built [`HeaderView`] to amortise the lookup cost.
    pub fn matches(&self, headers: &Headers) -> bool {
        match self {
            HeaderFilter::Any => true,
            HeaderFilter::Exact { key, value } => headers.get(key) == Some(value),
            HeaderFilter::Prefix { key, value_prefix } => headers
                .get(key)
                .is_some_and(|value| value.starts_with(value_prefix)),
            HeaderFilter::All(filters) => filters.iter().all(|filter| filter.matches(headers)),
            HeaderFilter::AnyOf(filters) => filters.iter().any(|filter| filter.matches(headers)),
        }
    }

    /// Card 512fd8a1 — fan-out alternative path. Evaluate this filter
    /// against a pre-built [`HeaderView`]. Per-lookup cost is `O(1)`
    /// hash instead of `O(log n)` BTreeMap walk. At realistic
    /// substrate scales (≤100 consumers, ≤20 headers) the build cost
    /// roughly cancels the per-lookup savings — see the `HeaderView`
    /// doc for measured numbers. The decoupling from the on-wire
    /// BTreeMap shape is the substrate property this enables; the
    /// per-call perf is co-equal.
    ///
    /// Semantically identical to [`Self::matches`] — same accept/reject
    /// decisions for the same `(filter, headers)` pair. Pinned by the
    /// `matches_and_matches_view_agree_for_every_variant` round-trip
    /// test below.
    pub fn matches_view(&self, view: &HeaderView<'_>) -> bool {
        match self {
            HeaderFilter::Any => true,
            HeaderFilter::Exact { key, value } => view.get(key) == Some(value.as_str()),
            HeaderFilter::Prefix { key, value_prefix } => view
                .get(key)
                .is_some_and(|value| value.starts_with(value_prefix.as_str())),
            HeaderFilter::All(filters) => filters.iter().all(|filter| filter.matches_view(view)),
            HeaderFilter::AnyOf(filters) => filters.iter().any(|filter| filter.matches_view(view)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn external_agent_headers() -> Headers {
        Headers::from([
            (
                "forge.body_hint".to_string(),
                "forge.persona.turn".to_string(),
            ),
            (
                "openclaw.channel".to_string(),
                "discord:continuum-lab".to_string(),
            ),
            ("hermes.skill".to_string(), "calendar".to_string()),
        ])
    }

    #[test]
    fn header_filter_matches_exact_and_prefix_without_body_parse() {
        let headers = external_agent_headers();

        assert!(HeaderFilter::Exact {
            key: "openclaw.channel".to_string(),
            value: "discord:continuum-lab".to_string(),
        }
        .matches(&headers));

        assert!(HeaderFilter::Prefix {
            key: "forge.body_hint".to_string(),
            value_prefix: "forge.persona.".to_string(),
        }
        .matches(&headers));

        assert!(!HeaderFilter::Exact {
            key: "hermes.skill".to_string(),
            value: "memory".to_string(),
        }
        .matches(&headers));
    }

    #[test]
    fn namespaced_headers_are_pass_through_and_deterministic() {
        let headers = external_agent_headers();
        let encoded = serde_json::to_string(&headers).unwrap();

        assert!(encoded.find("forge.body_hint").unwrap() < encoded.find("hermes.skill").unwrap());
        assert!(encoded.contains("openclaw.channel"));
    }

    // -------------------------------------------------------------------
    // Card 512fd8a1 — hot-path benchmarks for HeaderFilter::matches.
    //
    // Joel's directive: "make it FAST." Every consumer subscribes to
    // the bus with a HeaderFilter; every event fans out to every
    // consumer, and `matches` runs on every (event × consumer) pair.
    // That's the canonical hot path: cost compounds with the number of
    // consumers and the number of headers per event.
    //
    // TDD/VDD discipline: write the benchmark first, get a baseline
    // number, ONLY THEN optimise.
    //
    // The asserts here are deliberate *floors*, not goals — they're
    // generous enough to pass on slow CI runners but tight enough to
    // catch a 10× regression. The actual measured numbers (printed via
    // eprintln) are the artefact a perf reviewer cares about.
    // -------------------------------------------------------------------

    /// Realistic per-event header set: ~10 keys, mixed namespaces, the
    /// shape continuum's bridge daemon will see after Sub-D ships
    /// capability inventory + cache-coherency conventions.
    fn realistic_event_headers() -> Headers {
        Headers::from([
            ("airc.task.request".to_string(), "P0".to_string()),
            ("airc.task.offer".to_string(), "card-uuid-here".to_string()),
            (
                "airc.peer.busy".to_string(),
                "lora:code-v3:none".to_string(),
            ),
            (
                "airc.peer.has".to_string(),
                "qwen2.5-coder-32b:int4:resident".to_string(),
            ),
            (
                "forge.body_hint".to_string(),
                "forge.persona.turn".to_string(),
            ),
            (
                "openclaw.channel".to_string(),
                "discord:continuum-lab".to_string(),
            ),
            ("hermes.skill".to_string(), "calendar".to_string()),
            ("continuum.widget".to_string(), "video-room".to_string()),
            ("continuum.cell".to_string(), "5090-1".to_string()),
            ("x-correlation".to_string(), "req-abc123".to_string()),
        ])
    }

    /// Realistic consumer filter: an `All` of two prefix matches —
    /// e.g. "I want airc.task.* events from continuum.widget=video-room".
    fn realistic_router_filter() -> HeaderFilter {
        HeaderFilter::All(vec![
            HeaderFilter::Prefix {
                key: "airc.task.request".to_string(),
                value_prefix: "P".to_string(),
            },
            HeaderFilter::Exact {
                key: "continuum.widget".to_string(),
                value: "video-room".to_string(),
            },
        ])
    }

    #[test]
    fn bench_header_filter_matches_hot_path() {
        // 100k iterations — high enough to amortise wall-clock-read
        // overhead, low enough that even a debug build finishes in
        // a few seconds. The number that matters is the per-op cost
        // printed below; the assert is a coarse regression floor.
        let headers = realistic_event_headers();
        let filter = realistic_router_filter();

        // Warmup — primes branch predictor + caches so the timed
        // section measures steady-state cost, not cold-start.
        for _ in 0..1_000 {
            let _ = filter.matches(&headers);
        }

        const ITERS: u64 = 100_000;
        let start = std::time::Instant::now();
        let mut sink = 0u64;
        for _ in 0..ITERS {
            // Use the result so the optimiser can't elide the call.
            sink = sink.wrapping_add(filter.matches(&headers) as u64);
        }
        let elapsed = start.elapsed();
        let ns_per_op = elapsed.as_nanos() as u64 / ITERS;
        eprintln!(
            "card 512fd8a1: HeaderFilter::matches All(Prefix, Exact) over 10-header BTreeMap: \
             {ns_per_op} ns/op ({ITERS} iters in {elapsed:?}, sink={sink})"
        );

        // Floor: ought to be way under a microsecond per match. If it
        // ever climbs above 10μs (10_000 ns) on the slowest CI runner,
        // something pathological has been introduced.
        assert!(
            ns_per_op < 10_000,
            "HeaderFilter::matches regressed to {ns_per_op} ns/op — \
             investigate before merging; the hot path must stay under \
             10μs to keep bus fan-out cheap"
        );
    }

    #[test]
    fn bench_header_filter_matches_no_match_fast_path() {
        // Negative case — most consumers reject most events. The
        // short-circuit on the FIRST condition mismatch is the
        // dominant cost on a typical bus. Pin that the rejection
        // path is at least as fast as the accept path (in practice
        // it's strictly faster because of the `iter().all()` early
        // exit, but a regression that broke the short-circuit would
        // show up here).
        let headers = realistic_event_headers();
        let filter = HeaderFilter::All(vec![
            // First condition: no such header → All() short-circuits.
            HeaderFilter::Exact {
                key: "no-such-header".to_string(),
                value: "no-such-value".to_string(),
            },
            HeaderFilter::Exact {
                key: "continuum.widget".to_string(),
                value: "video-room".to_string(),
            },
        ]);

        for _ in 0..1_000 {
            let _ = filter.matches(&headers);
        }

        const ITERS: u64 = 100_000;
        let start = std::time::Instant::now();
        let mut sink = 0u64;
        for _ in 0..ITERS {
            sink = sink.wrapping_add(filter.matches(&headers) as u64);
        }
        let elapsed = start.elapsed();
        let ns_per_op = elapsed.as_nanos() as u64 / ITERS;
        eprintln!(
            "card 512fd8a1: HeaderFilter::matches short-circuit reject \
             over 10-header BTreeMap: {ns_per_op} ns/op ({ITERS} iters \
             in {elapsed:?}, sink={sink})"
        );

        assert!(
            ns_per_op < 10_000,
            "short-circuit reject path regressed to {ns_per_op} ns/op"
        );
    }

    #[test]
    fn matches_and_matches_view_agree_for_every_variant() {
        // The two paths MUST return the same accept/reject for any
        // (filter, headers) pair — `matches_view` is a perf-shaped
        // alias, not a different semantic. A drift here would mean a
        // consumer that routes correctly under fan-out but wrongly
        // under one-shot inspection (or vice versa), which is
        // exactly the silent-divergence bug shape we refuse.
        let headers = realistic_event_headers();
        let view = HeaderView::build(&headers);

        let cases: Vec<HeaderFilter> = vec![
            HeaderFilter::Any,
            HeaderFilter::Exact {
                key: "continuum.widget".to_string(),
                value: "video-room".to_string(),
            },
            HeaderFilter::Exact {
                key: "missing.key".to_string(),
                value: "anything".to_string(),
            },
            HeaderFilter::Prefix {
                key: "airc.task.request".to_string(),
                value_prefix: "P".to_string(),
            },
            HeaderFilter::Prefix {
                key: "airc.task.request".to_string(),
                value_prefix: "wrong-".to_string(),
            },
            realistic_router_filter(),
            HeaderFilter::AnyOf(vec![
                HeaderFilter::Exact {
                    key: "missing.a".to_string(),
                    value: "x".to_string(),
                },
                HeaderFilter::Exact {
                    key: "continuum.widget".to_string(),
                    value: "video-room".to_string(),
                },
            ]),
        ];
        for filter in &cases {
            assert_eq!(
                filter.matches(&headers),
                filter.matches_view(&view),
                "matches/matches_view divergence for {filter:?}"
            );
        }
    }

    /// Large-scale fixture: 50 headers (dense), modelling a busy room
    /// where consumers + capability-inventory + cache-coherency +
    /// correlation IDs all pile on. This is where the BTreeMap.get
    /// cost (log n string-compares) starts to dominate.
    fn dense_event_headers() -> Headers {
        let mut h = Headers::new();
        for i in 0..50 {
            h.insert(
                format!("ns{}.key.{i}", i % 5),
                format!("value-{i}-payload-string-data"),
            );
        }
        h.insert("continuum.widget".to_string(), "video-room".to_string());
        h
    }

    #[test]
    fn bench_header_filter_matches_view_large_scale() {
        // Card 512fd8a1 — measurement at the larger scale we expect
        // when continuum's bridge is fanning events across the full
        // grid: 50 headers (after Sub-D adds capability inventory +
        // cache-coherency conventions) × 1000 consumers (widgets ×
        // personas × routers).
        //
        // What this test pins (vs. what we set out to pin):
        //
        // We initially hypothesised HeaderView would be ~12× faster
        // than BTreeMap at this scale. Live measurement (M2 release,
        // diverse-key filters): the gap is much smaller — about 1×
        // — because at 50 headers BTreeMap.get is already only ~6
        // string compares deep and the branch predictor warms across
        // 1000 lookups. The HashMap build cost (~1-2μs per event)
        // eats most of the per-lookup savings at this scale.
        //
        // The benchmark stays as a *guardrail*: a future change that
        // catastrophically slows EITHER path will fail here. The
        // HeaderView API remains useful for the encoding-discipline
        // split (wire shape is BTreeMap, routing form is decoupled)
        // and for larger-N or longer-key workloads where the
        // crossover does flip.
        let headers = dense_event_headers();
        let filters: Vec<HeaderFilter> = (0..1000)
            .map(|i| {
                let bucket = i % 50;
                HeaderFilter::Exact {
                    key: format!("ns{}.key.{bucket}", bucket % 5),
                    value: format!("value-{bucket}-payload-string-data"),
                }
            })
            .collect();

        let warm_view = HeaderView::build(&headers);
        for f in &filters {
            let _ = f.matches(&headers);
            let _ = f.matches_view(&warm_view);
        }

        const EVENTS: u64 = 1_000;

        let start = std::time::Instant::now();
        let mut sink = 0u64;
        for _ in 0..EVENTS {
            for f in &filters {
                sink = sink.wrapping_add(f.matches(&headers) as u64);
            }
        }
        let plain = start.elapsed();
        let plain_ns = plain.as_nanos() as u64 / (EVENTS * filters.len() as u64);

        let start = std::time::Instant::now();
        let mut sink2 = 0u64;
        for _ in 0..EVENTS {
            let view = HeaderView::build(&headers);
            for f in &filters {
                sink2 = sink2.wrapping_add(f.matches_view(&view) as u64);
            }
        }
        let viewed = start.elapsed();
        let view_ns = viewed.as_nanos() as u64 / (EVENTS * filters.len() as u64);

        eprintln!(
            "card 512fd8a1: LARGE-scale fan-out (50 headers × 1000 consumers, diverse filter keys): \
             plain matches={plain_ns} ns/op ({plain:?}), \
             view matches={view_ns} ns/op ({viewed:?}), \
             ratio={:.2}× (HeaderView not a win at this shape — see doc), \
             sinks=({sink}, {sink2})",
            plain_ns as f64 / view_ns.max(1) as f64
        );

        // Coarse regression floor: both paths must clear < 10μs/op.
        // The honest perf-improvement story is in the carded
        // follow-ups (projection rebuild, UDS frame parse), not in
        // this microbench.
        assert!(
            plain_ns < 10_000,
            "plain matches catastrophically regressed: {plain_ns} ns/op"
        );
        assert!(
            view_ns < 10_000,
            "view matches catastrophically regressed: {view_ns} ns/op"
        );
    }

    #[test]
    fn bench_header_filter_matches_view_fan_out() {
        // Card 512fd8a1 optimisation: the same realistic fan-out
        // shape as bench_header_filter_matches_realistic_fan_out,
        // but using the pre-built HeaderView. The view build cost
        // amortises across the 100 consumers per event. Expect
        // ~2× throughput vs the BTreeMap path.
        let headers = realistic_event_headers();
        let filters: Vec<HeaderFilter> = (0..100).map(|_| realistic_router_filter()).collect();

        let view = HeaderView::build(&headers);
        for _ in 0..1_000 {
            for f in &filters {
                let _ = f.matches_view(&view);
            }
        }

        const EVENTS: u64 = 10_000;
        let start = std::time::Instant::now();
        let mut sink = 0u64;
        for _ in 0..EVENTS {
            // In production the router builds the view ONCE per
            // event, then fans out. Replicate that shape here so
            // the build cost is amortised, not free.
            let view = HeaderView::build(&headers);
            for f in &filters {
                sink = sink.wrapping_add(f.matches_view(&view) as u64);
            }
        }
        let elapsed = start.elapsed();
        let total_ops = EVENTS * filters.len() as u64;
        let ns_per_op = elapsed.as_nanos() as u64 / total_ops;
        eprintln!(
            "card 512fd8a1: bus fan-out via HeaderView (10k events × 100 consumers, view rebuilt per event): \
             {ns_per_op} ns/match, {} match/s, total {elapsed:?}, sink={sink}",
            1_000_000_000 / ns_per_op.max(1)
        );

        assert!(
            ns_per_op < 10_000,
            "view-fan-out cost regressed to {ns_per_op} ns/match"
        );
    }

    #[test]
    fn bench_header_filter_matches_realistic_fan_out() {
        // The actual production cost shape: ONE event, N filters
        // (consumers), each filter checks against the same headers
        // map. With 100 consumers this approximates a busy room
        // (continuum widget grid + multiple persona agents + the
        // merger + queue-card watchers + …).
        let headers = realistic_event_headers();
        let filters: Vec<HeaderFilter> = (0..100).map(|_| realistic_router_filter()).collect();

        for _ in 0..1_000 {
            for f in &filters {
                let _ = f.matches(&headers);
            }
        }

        const EVENTS: u64 = 10_000;
        let start = std::time::Instant::now();
        let mut sink = 0u64;
        for _ in 0..EVENTS {
            for f in &filters {
                sink = sink.wrapping_add(f.matches(&headers) as u64);
            }
        }
        let elapsed = start.elapsed();
        let total_ops = EVENTS * filters.len() as u64;
        let ns_per_op = elapsed.as_nanos() as u64 / total_ops;
        eprintln!(
            "card 512fd8a1: bus fan-out (10k events × 100 consumers): \
             {ns_per_op} ns/match, {} match/s, total {elapsed:?}, sink={sink}",
            1_000_000_000 / ns_per_op.max(1)
        );

        // Floor: at 10μs/op we'd cap at 100k matches/s, which is
        // way too slow for a substrate that wants to STREAM media
        // headers (per the encoding-discipline doctrine).
        assert!(
            ns_per_op < 10_000,
            "fan-out cost regressed to {ns_per_op} ns/match — \
             consumer count × header complexity is the multiplier"
        );
    }
}
