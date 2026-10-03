# Initial isolated burst measurements

Source: AIRC 9d5557399deb, branch codex/burst-performance.
Command: cargo test -p airc-bus --test burst_performance -- --ignored --nocapture
Profile: Windows debug, default router queue/ring sizes; one tight-loop publisher, no subscribers or network. Synthetic in-memory durable sink delays each batch by 10 ms (Windows timer scheduling may exceed that). These are admission measurements, not delivered/network throughput or SQLite benchmarks.

| Attempts | Accepted | Saturated | Publish p99 | All accepted persisted by | Pinned after drain |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 256 | 256 | 0 | 11.1 us | 93 ms | 0 |
| 2048 | 1029 | 1019 | 3.8 us | 280 ms | 1019 |
| 8192 | 1090 | 7102 | 3.0 us | 316 ms | 7102 |

All accepted events persisted; rejected messages nevertheless remained pinned. Source inserts into ring and fans out before write-behind try_send. Saturation returns an error after those mutations. A preliminary per-row-delay variant reproduced the same retention. A fast return must not be reported as high successful-delivery throughput: most attempts in the largest burst were rejected.

Next: deterministic admission regression using existing GatedSink, independent review of reserve-before-publish semantics, repeated/concurrent bursts, then real SQLite/IPC and loopback delivery measurements. No live daemon, room traffic flood, account database change, installer, or UAC was used.

After the admission fix, the same debug fixture accepted 1029 / rejected 7163
of 8192 attempts, drained accepted writes in 269 ms, and retained ZERO pinned
entries. This is a memory-retention fix, not evidence of improved network
throughput. Publish p99 was 3.8 us; fast rejected calls dominate the large burst.
The deterministic saturation regression failed before the patch (visible cursor
advanced on rejection) and passed afterward, including live-reader exclusion and
successful same-ID retry. The complete airc-bus suite passed locally.

## TLS / SQLite peer-consumer matrix

Windows i7-6800K (12 logical processors), isolated temporary homes, loopback TLS,
standalone library path. Source 9d555 plus the admission fix; no live deployment.
The existing test file now has an opt-in receipt matrix. Each publisher waits for
its preceding call (closed-loop). Times include scenario setup of publisher tasks
and their start barrier. Peer latency runs from call start through subscriber
observation, not wire arrival. This path appends received durable events before
live fan-out, but no independent reopen/crash durability assertion was made.

| Publishers x messages | Received | Publish completion | Last peer observation | Peer p99 | Duplicate observations |
| --- | ---: | ---: | ---: | ---: | ---: |
| 15 x 40 | 600 | 269 ms | 269 ms | 37.10 ms | 0 |
| 64 x 64 | 4096 | 2055 ms | 2058 ms | 63.78 ms | 0 |
| 128 x 64 | 8192 | 3494 ms | 3516 ms | 90.18 ms | 0 |

These are optimized-build results; all expected IDs arrived. Duplicate checking
continues for 250 ms after completion, not indefinitely. Largest observed receipt
rate: 2329.7 messages/sec. One run is not a sustained-load capacity guarantee.

The earlier DEBUG run measured 600 receipts in 5.044 s, 4096 in 35.036 s, and only
7078 of 8192 by its approximately 60.26 s observation deadline (1114 unobserved,
zero duplicate observations). That is a failed deadline, not proven permanent
loss. Latency percentiles for that failed case exclude unobserved events. The
optimized result did not reproduce the severe debug backlog; do not attribute
that debug result to a deployed store or transport defect without profiling.

The revised harness owns publisher tasks with JoinSet, runs the receiver as a
scoped future, bounds setup/subscription to 30 s and the scenario to 90 s, aborts
and drains publishers before surfacing scenario errors, and retains first receipt
timestamps. Independent source reviewer approved those measurement boundaries.

Run: cargo test --release -p airc-lib --test chat_throughput bench_chat_burst_peer_receipts -- --ignored --exact --nocapture --test-threads=1

## Streaming fan-out through daemon IPC

Optimized Windows run on the same i7-6800K, isolated temporary daemon homes.
Each case sends 64 unique frames over real IPC to 1, 8 or 32 readers. The wall
clock ends after all consumer payloads have been decoded and compared, so it
includes validation and task collection, not just transport. Consumers verify
the first 64 frames and their order; they do not watch for trailing duplicates.
The immediate inbox query was empty for these StreamChunk cases. This is not a
long-term or crash/reopen persistence proof.

| Payload bytes | Readers | Publish completion | Validated consumer completion | Aggregate validated MiB/s |
| ---: | ---: | ---: | ---: | ---: |
| 256 | 1 | 38.819 ms | 38.891 ms | 0.40 |
| 256 | 8 | 42.303 ms | 42.480 ms | 2.94 |
| 256 | 32 | 54.269 ms | 54.879 ms | 9.11 |
| 65536 | 1 | 944.751 ms | 952.912 ms | 4.20 |
| 65536 | 8 | 902.162 ms | 930.150 ms | 34.40 |
| 65536 | 32 | 3000.211 ms | 3086.588 ms | 41.47 |

One run, not a sustained capacity guarantee or CPU profile. Every case passed.
The publisher is sequential, and aggregate bytes count a separate delivery to
each reader. These rates are not comparable to durable-chat receipt rates.

Source audit candidates: the server encodes an envelope for each IPC subscriber,
and CBOR uses the existing integer-sequence representation of its opaque bytes.
The compatibility test explicitly demonstrates installed Vec decoders rejecting
CBOR byte strings. No wire representation change is included. Phase profiling
is still needed before attributing the observed wall time to encoding or copies.

The fixture owns its server task before awaiting listener readiness, aborts it
on cancellation, and retains/aborts/awaits the handle if graceful stop exceeds
three seconds. Collectors use JoinSet with cleanup before errors are surfaced.
Independent source review approved these boundaries; the actual startup cleanup
regression and package all-target Clippy also passed.

Run: cargo test --release -p airc-daemon --test owner_core_proof bench_stream_fanout_sizes -- --ignored --exact --nocapture

## Isolated codec phases

Serial optimized in-memory measurement, 512 repetitions of each reused input.
Means include normal allocation/drop costs, not latency percentiles or CPU
profiling. CBOR phases include framing and memory copies. Wire decode clones
Bytes and shares the existing payload allocation. Summing these isolated means
does not reconstruct daemon fan-out time. Actual framed round-trip equality
passed before measurement; no production encoding change is included.

| Payload | Wire bytes | IPC bytes | Wire encode mean | CBOR frame encode mean | CBOR frame decode mean | Wire decode mean |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 256 B | 384 | 744 | 0.738 us | 4.407 us | 30.985 us | 0.109 us |
| 65536 B | 65664 | 131303 | 109.130 us | 664.461 us | 6662.511 us | 0.127 us |

This identifies framed decoding as a candidate for investigation, without
attributing full pipeline latency to that phase. Independent source review
approved measurement framing with the limits above.

Run: cargo test --release -p airc-daemon --test owner_core_proof bench_stream_codec_phases -- --ignored --exact --nocapture

## Canonical event response decoding

The production candidate recognizes only the current writer's exact two-field
CBOR event representation: fixed field order, shortest definite array lengths,
and canonical u8 values. Every other frame goes untouched to the original
Response decoder. The wire format and serialization are unchanged. This avoids
the generic tagged-enum buffer on common live events without changing accepted
noncanonical representations. An earlier high-level visitor prototype was
rejected and removed because normalization changed edge-case acceptance.

Same optimized six-case harness and machine, one run each (not a controlled
sustained-load capacity result):

| Payload | Readers | Baseline validated completion | Candidate validated completion |
| ---: | ---: | ---: | ---: |
| 256 B | 1 | 38.891 ms | 37.985 ms |
| 256 B | 8 | 42.480 ms | 41.287 ms |
| 256 B | 32 | 54.879 ms | 51.487 ms |
| 64 KiB | 1 | 952.912 ms | 519.456 ms |
| 64 KiB | 8 | 930.150 ms | 581.160 ms |
| 64 KiB | 32 | 3086.588 ms | 911.834 ms |

All cases passed first-64 payload/order and immediate empty-inbox checks. The
largest case carries 128 MiB aggregate reader payload and measured 140.38 MiB/s
through validation. No live account daemon or deployed network was changed.
Independent source review approved the raw-frame subset and untouched fallback;
IPC differential mutation/boundary tests, full IPC suite, and workspace Clippy
passed. Existing generic codec phase tests still measure the original decoder.

## SQLite visibility after concurrent IPC publication

An opt-in isolated daemon benchmark opens the existing SQLite file read-only,
without migration or journal changes. Each publisher awaits its preceding RPC.
After all callers finish, it observes the maximum accepted cursor and then polls
for the complete accepted receipt-ID set. A high cursor alone is insufficient:
concurrent publishers can enqueue and commit out of sequence. Unexpected IDs or
duplicates fail immediately; temporarily missing expected IDs may drain within
the scenario deadline. Publication errors fail the measurement.

| Publishers x messages | Accepted | Errors | All publishers finished | Highest accepted cursor observed | Complete SQLite ID set verified | Pinned at publish completion |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 16 x 64 | 1024 | 0 | 196.919 ms | 204.506 ms | 213.610 ms | 1 |
| 64 x 64 | 4096 | 0 | 786.985 ms | 794.076 ms | 830.464 ms | 1 |

One optimized Windows run. Observation begins after publication and includes
poll/query cost; these are upper bounds on visibility, not per-event commit
latencies. Exact IDs were read from SQLite through a separate read-only handle,
not the hot ring. This verifies live-WAL visibility, not power-loss, crash or
reopen durability. Independent review approved bounded task ownership and
complete-set observation; the rerun and package all-target Clippy passed.

Run: cargo test --release -p airc-daemon --test owner_core_proof bench_daemon_sqlite_drain -- --ignored --exact --nocapture

## Recovery after storage backpressure

The existing synthetic delayed-sink test now retries the original rejected IDs
after the first accepted batch drains. The sink delays each batch by 10 ms.
Retries pause 10 ms only on explicit saturation. Both drain phases are bounded;
final rows must contain exactly all original IDs, without duplicate rows or pins.
This tests publish retry behavior, not concurrent idempotent ingestion.

Optimized 8192-attempt case: 1025 initially accepted, 7167 initially rejected;
accepted rows drained in281ms, leaving zero pins. Retrying the rejected IDs
encountered109 additional saturation responses; all8192 IDs were verified with
zero pins after2071ms of retry, drain and final set validation. This is a
synthetic in-memory sink, not SQLite/network throughput. Initial attempts include
tracking-clone overhead; fast rejection dominates their reported attempt rate.
Independent source review approved the retry test and these measurement limits.

## Shared attach-frame encoding (candidate, not deployed)

The daemon reuses immutable CBOR event payloads by the actual Arc allocation,
retaining a Weak identity rather than trusting event IDs. Distinct allocations
with the same event ID remain distinct. OnceCell shares initialization while an
entry is resident; eviction may cause a later encoding of that allocation.

Initial admission limits are 128 entries and 64 MiB of CBOR buffer capacity,
including evicted entries/frames still held by writers. Before encoding, a
32 MiB conservative reservation precedes encoding; the exact event encoder
allocates at most the legal 16 MiB once. The charge then shrinks to actual
retained Vec capacity without copying.
There can be at most two such encoders concurrently. The existing four-byte
prefix stays on the writer stack. Existing Planus FlatBuffer workspace remains
input-dependent and is NOT included in this shared-frame memory bound. Existing
ordinary RPC Vec buffers are also outside it; this is not a total daemon-memory bound.

The canonical IPC codec factors serialization from framed writing. Its event
encoder computes the exact existing CBOR size and validates it before one
allocation. Differential Serde tests guard bytes and full oversize diagnostics.
Length prefix, socket errors and flush behavior remain unchanged. Ordinary RPC
writes retain their original Serde/Vec path and serialization-error precedence.

There is no unchecked fallback allocation. Under pressure, cache references are
evicted and misses asynchronously await admission. Slow socket writers keep their
charges until release, so sufficiently many stalled writers can delay unrelated
rooms. This is an explicit bounded-memory/backpressure tradeoff, not a throughput
or fairness claim. The same pinned daemon-shutdown and client-hangup futures now
interrupt admission and event writes. No lock spans encoding or socket waits.

Focused tests cover shared identity, same-ID distinction, exact legacy bytes,
entry/byte admission publication interleavings, cancellation, retained charges,
serialization failure, and socket failure. Actual stream_attach tests saturate
admission and independently exercise hangup and shutdown using in-memory duplex
streams. Reviewed isolated matrices are documented below; no live account or
installed daemon is changed.

### Superseded generic bounded-writer experiments

Same six-case optimized matrix on the same Windows machine, one candidate run
per implementation, not paired repetitions or sustained-load capacity:

| Payload | Readers | Prior decoder-only completion | Shared-frame candidate completion |
| ---: | ---: | ---: | ---: |
| 256 B | 1 | 37.985 ms | 40.708 ms |
| 256 B | 8 | 41.287 ms | 40.065 ms |
| 256 B | 32 | 51.487 ms | 44.565 ms |
| 64 KiB | 1 | 519.456 ms | 550.153 ms |
| 64 KiB | 8 | 581.160 ms | 566.279 ms |
| 64 KiB | 32 | 911.834 ms | 719.032 ms |

All first-64 payload/order and immediate empty-inbox assertions passed (42630).
The 32-reader large-payload case measured 178.02 MiB/s through validation.
The one-reader large case was slower; this is not a universal speedup claim.
An initial candidate also applied the bounded buffer to ordinary RPCs and had
large-case completions 659.108/620.486/817.881 ms. That broader allocation change
was removed: ordinary RPCs retain Vec while sharing canonical codec policy.
No CPU attribution or independent total-allocation profile has been measured.

Focused native cache/cancellation tests8/8 passed (83750); canonical codec tests
11 passed, one manual benchmark remained ignored (38018). Independent review
approved scoped benchmark safety after fixing a cache-publication/admission
missed-wake race. Final workspace Clippy and formatting checks passed (50153)
after the allocation-path narrowing. Hosted CI and deployed binary/public grid
acceptance remain OPEN.

The existing ignored codec-phase harness adds 16 warmup pairs and 512 timed
pairs, alternating ordinary/bounded order on the same immutable event response.
Byte equality is checked outside timing; allocation/drop are included, socket
framing and scheduling are excluded. Per-call wall-time distributions:

| Buffer / phase | Payload | Mean | p50 | p95 |
| --- | ---: | ---: | ---: | ---: |
| Ordinary, initial paired run | 64 KiB | 590.218 us | 583.800 us | 630.900 us |
| Bounded, initial paired run | 64 KiB | 1417.719 us | 1391.500 us | 1476.600 us |
| Ordinary, final paired run | 256 B | 3.956 us | 3.900 us | 4.100 us |
| Bounded, final paired run | 256 B | 6.774 us | 6.800 us | 6.800 us |
| Ordinary, final paired run | 64 KiB | 534.010 us | 511.300 us | 560.600 us |
| Bounded, final paired run | 64 KiB | 999.723 us | 964.300 us | 1038.600 us |

The superseded bounded writer used Vec length on capacity-fit writes and counts only
discarded oversize bytes separately. That change alone did not materially reduce
cost (bounded mean1427.912us in the intervening run). An explicit inline complete
write implementation avoids the generic partial-write retry adapter; final
paired12315 passed with lower bounded cost, which still exceeds ordinary cost.
All frame limits and late serializer-error behavior remain tested and unchanged.
These measurements do not isolate inlining from retry-adapter removal or establish
whole-pipeline CPU attribution. Final matrix42630 still shows a slower single
reader versus the earlier unpaired decoder-only run, and multi-reader timings
varied between runs. Publication remains held for performance review; there is
no claim of a universal speedup or deployed improvement.

### Paired retained-binary comparison (publication held)

Baseline commit `7321076` and the unchanged candidate above were built serially
in the same release target. Source was preserved with a named stash including
untracked files and a separate seven-file SHA256 backup; exact source hashes and
tracked binary diff were verified after restoration. Restored old file mtimes
caused Cargo to reuse the baseline IPC artifact; a scoped `cargo clean -p airc-ipc
--release` removed 14 files (11.3 MiB), then the candidate rebuilt successfully.
No source timestamp manipulation or live installation was used. Both test
executables were retained independently and hashed before measurement.

The existing matrix uses an in-process library daemon, not an external CLI.
Three serial pairs alternate B/C, C/B, B/C. All 36 scenario assertions passed
(session 43752). Completion includes validation; each sample has 64 events.

| Payload | Readers | Baseline median [range], ms | Candidate median [range], ms | Median change |
| ---: | ---: | ---: | ---: | ---: |
| 256 B | 1 | 38.763 [37.355–40.032] | 39.521 [39.516–40.088] | +1.96% |
| 256 B | 8 | 41.326 [41.107–41.424] | 38.828 [38.555–43.234] | -6.04% |
| 256 B | 32 | 48.594 [48.592–49.506] | 44.545 [44.113–46.506] | -8.33% |
| 64 KiB | 1 | 540.649 [528.703–555.690] | 585.719 [572.987–591.908] | +8.34% |
| 64 KiB | 8 | 565.503 [556.075–567.721] | 551.756 [547.042–555.562] | -2.43% |
| 64 KiB | 32 | 877.735 [874.432–932.199] | 764.297 [744.349–813.880] | -12.92% |

The large single-reader ranges do not overlap: the candidate's cost is real in
these runs, despite fanout improvements. Publication remains held. Three pairs
are not a sustained-load capacity study or CPU attribution. Final formatting,
eight cache tests and full workspace Clippy passed (86034) before source
preservation. Logs, JSON measurements, preserved source and both executable
hashes are retained in `D:/airc-build/doria/shared-frame-paired-20261003`.
### Exact canonical event encoder candidate

The next narrow change adds an IPC-owned encoder for the exact existing Event
shape: the same tagged map prefix, shortest definite-array length and canonical
unsigned u8 sequence. It computes checked complete length, applies the existing
frame limit before allocation, then fills one exact-capacity buffer. It does not
change the wire format, generic RPC serialization, cache admission or cancellation.
The cache retains its conservative reservation; no allocation-bound bypass was
introduced. Serde `Response::event_ref` remains the differential oracle, including
all byte values, array length boundaries, mixed inputs, exact limit and oversize
error text. Thirteen codec tests and eight cache/stream-cancellation tests pass;
full workspace Clippy passes (61753/92426). Independent review approved benchmark
safety, not an unconditional performance or deployment claim.

The existing paired codec harness (38485), with 16 warmup pairs and 512 alternating
pairs, measured ordinary versus exact-event mean/p50/p95 in microseconds:

| Payload | Ordinary | Exact event |
| ---: | ---: | ---: |
| 256 B | 4.035 / 4.000 / 4.100 | 0.972 / 0.900 / 1.000 |
| 64 KiB | 578.887 / 577.800 / 615.900 | 161.857 / 166.800 / 181.500 |

A newly retained candidate executable was then compared against the same retained
baseline in three new pairs, B/C, C/B, B/C. All 36 scenario assertions passed
(53162). No source changed during measurement.

| Payload | Readers | Baseline median [range], ms | Candidate median [range], ms | Median change |
| ---: | ---: | ---: | ---: | ---: |
| 256 B | 1 | 38.072 [37.927–38.213] | 37.582 [37.398–37.757] | -1.29% |
| 256 B | 8 | 40.732 [40.124–43.177] | 40.607 [39.837–40.638] | -0.31% |
| 256 B | 32 | 50.018 [47.728–50.993] | 46.913 [45.220–47.726] | -6.21% |
| 64 KiB | 1 | 534.965 [511.832–612.476] | 500.423 [491.579–512.881] | -6.46% |
| 64 KiB | 8 | 555.271 [552.395–575.992] | 526.961 [505.545–547.661] | -5.10% |
| 64 KiB | 32 | 875.315 [862.397–897.689] | 682.473 [679.507–683.247] | -22.03% |

The earlier consistent single-reader regression is absent in these pairs; its
large-payload ranges now overlap slightly. The 32-reader reduction is larger and
nonoverlapping in these runs. This remains a short local experiment, not a claim
of universal speedup, total CPU savings, deployed improvement or sustained capacity.
Hosted CI, publication review and actual deployed grid acceptance remain open.
Exact-event logs, binary hash and JSON results are retained beside the prior
comparison in `D:/airc-build/doria/shared-frame-paired-20261003`.
The unused generic bounded-writer API and implementation were removed after the
exact-event measurements. Historical results above remain as the decision record;
only the original generic codec and specialized exact-event encoder are shipped.

PR #1521's first hosted Clippy run rejected metadata-lock unwraps and semaphore
expect calls under its separate production-only policy. The local all-targets
command had omitted that stricter existing command. Metadata poison now returns
an explicit `Other` I/O error; closed admission budgets return `BrokenPipe`.
Neither path silently recovers or retries. Focused tests cover poison, release of
an uninserted permit, immediate/waiting closure of both budgets and final permit
accounting. Existing admitted frames can still be served after budget closure;
closure here is an admission failure, not a global shutdown contract.