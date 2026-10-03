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
