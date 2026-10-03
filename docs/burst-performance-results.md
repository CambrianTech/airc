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
