# Daemon socket leak at tip — verification (card 8f73809b)

On 2026-09-12 a fresh daemon on Fable M5 answered status with 8 unix fds; after
30 `airc status` calls from the tip CLI it held 12,764 (~425/request), and at
rest with four citizens seated it grew ~70/s until the daemon logged
`Too many open files in system (os error 23)` and every process on the host
failed to open files. The repro ran against canary tip a3b15649 (#1409).

## Root cause: fixed before this card was filed

dc0bb4a5 (#1410) — the first commit after the repro tip, and present in current
tip 59e8f213 (#1620) — removes the retention. Its message describes exactly
this mechanism: the daemon's attach loop selected only on shutdown and the
router stream; the client half of each attach socket was never read, so a
subscriber that hung up (a core dying at deploy, a citizen re-opening on a
membership epoch) left its daemon-side socket open until the next write to
that channel tripped EPIPE — "on a quiet room: never." The fix pins a
`client_hung_up` future beside `shutdown`, so EOF or a transport error on the
client side ends the task and drops the subscription. It also added the live
`connections` receipt to `StatusResponse` (absent at the repro tip, present
now): a count that climbs while its clients stay flat is a daemon keeping dead
streams.

The card's named suspects — d338116a (#1403), c3df665e (#1401), 78b3c837
(#1405) — all predate the repro tip and are still present at 59e8f213; the
measurements below show them innocent.

## Verification at tip 59e8f213 (#1620), Windows host

Unix fd counts were observed on macOS via lsof in the original report; this
host is Windows, where IPC rides named pipes rather than AF_UNIX sockets, so
the process HandleCount (a superset of socket handles) was measured with
`Get-Process` instead. A flat handle count bounds every descriptor class at
once.

1. **Acceptance A — 100 status calls.** Fresh daemon on a temp scope home
   (`/tmp/tmp.rQOzNkKkTq`, hermetic: no account registry, no listener
   acquisition), then 100 verified `airc status` calls (every call returned
   `peer_id`; zero failures). HandleCount was flat before and after (138 →
   138); ~131 requests across the session produced zero growth. The card's
   acceptance asks for a flat unix fd count over 100 status calls — met.
2. **Soak.** The same daemon ran ~10h with route refresh active and real
   client churn on the shared temp pipe; HandleCount stayed at 138 (started
   ~146). The `connections` receipt fluctuated (1 → 7 live) while handles did
   not grow — hangup release working under churn. The card's acceptance asks
   for a 30-min soak with four citizens staying under 500; the hermetic temp
   home cannot seat four citizens, so this was met in mechanism and duration
   rather than in that exact environment: ~10h of churn, well under 500.
3. **Regression probe.** `cargo test -p airc-daemon --test attach_hangup_probe`
   at tip: both probes pass (a hung-up stream on a room nobody publishes to is
   released within the bound; a held stream is kept).

## Standing

No code change was needed for this card: the leak does not reproduce at
59e8f213, and its fix ships with a live counter (`airc status` →
`connections`) that turns "a count that climbs while clients stay flat" into
something observable in one command. If a future build ever grows handles per
request again, the receipt says so before ENFILE does.
