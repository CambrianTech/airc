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
