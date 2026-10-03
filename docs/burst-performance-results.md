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
