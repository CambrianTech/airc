"""Peer heartbeat: periodic liveness signal closing airc#644.

The substrate gap closed by this module: a peer silent for N hours is
indistinguishable on the wire from a peer whose airc process is down.
`airc peers` shows `last seen Nh ago (silent)` for both — but `silent`
conflates "still alive, just not broadcasting" with "process died,
substrate doesn't know."

A heartbeat is a tiny periodic envelope (`kind="heartbeat"`) that every
running airc process emits. Other peers see it; their `airc peers`
tracks `last_heartbeat` SEPARATELY from `last_message`. Two distinct
columns mean the room can tell which kind of silence it is looking at.

## Scope of this module (PR-1 per airc#644)

- Build a heartbeat envelope (kind="heartbeat", empty msg body).
- Emit helper that wraps via the existing `wrap_envelope` path so the
  AEAD associated-data binding (including the new `kind` field) is
  applied identically to chat sends.
- No file I/O, no broadcast wiring — that lives in PR-2 (cmd_send
  hook + reminder_timer_loop trigger).

PR-2 will:
- Add `--heartbeat` flag to cmd_send (same wire path as `--system`).
- Hook into `reminder_timer_loop` in `airc` to emit every
  HEARTBEAT_CADENCE_SEC.
- Update `cmd_peers` to read heartbeats separately + show PROCESS_DOWN
  when last_heartbeat > STALE_HEARTBEAT_SEC.

Why split: the wire-emission path goes through cmd_send.sh (823 lines)
and the multi-bearer broadcast subsystem; that's enough surface to
warrant its own focused PR with its own integration tests.

## What's distinguishable after PR-1 + PR-2 lands

| Symptom on the wire | Explanation |
|---|---|
| last_message: 11h, last_heartbeat: 12s | Peer alive, heads-down working |
| last_message: 11h, last_heartbeat: 11h | Peer's airc process likely down |
| last_message: never, last_heartbeat: 12s | New peer that hasn't spoken yet |
| last_message: 11h, last_heartbeat: never | Peer on pre-#644 airc (no heartbeat support); display as legacy |

The legacy case is the back-compat path: peers running airc without
this module emit no heartbeats; downstream code treats `last_heartbeat
is None` as "unknown" rather than "process down" so we don't
false-positive on rollouts in progress.
"""

from __future__ import annotations

import time
from typing import Optional

# Cadence is configurable but defaults to 60 seconds. Two heartbeats
# missed (>= 2x cadence = 120s) is the substrate's "process likely down"
# signal. This balances responsiveness against network/CPU cost — a
# 30s heartbeat would catch faster but doubles the gossip volume; a
# 5min heartbeat halves volume but makes the room blind to crashes
# for up to 10 min.
HEARTBEAT_CADENCE_SEC = 60
STALE_HEARTBEAT_SEC = HEARTBEAT_CADENCE_SEC * 2

# The kind tag used on the wire. Lower-case-kebab to match airc's
# existing field conventions (channel, from, to).
HEARTBEAT_KIND = "heartbeat"


def make_heartbeat_envelope(
    from_name: str,
    channel: str,
    *,
    timestamp_iso: Optional[str] = None,
) -> dict:
    """Build a heartbeat envelope (unsigned, unencrypted). Caller passes
    through the existing sign + (optionally) wrap_envelope steps.

    The msg body is empty — a heartbeat carries no payload. All the
    information (`from`, `ts`, `kind`) is in metadata.

    `to` is the broadcast token `"all"`. Heartbeats are room-wide, not
    DM-shaped; every peer in the room sees every other peer's heartbeat.

    Args:
        from_name: this peer's airc identity (e.g. "claude-tab-1").
        channel: the room/channel this heartbeat is for. Heartbeats are
            per-channel so `airc peers` filtered by channel works
            correctly.
        timestamp_iso: ISO-8601 timestamp (UTC, no microseconds). If
            None, uses `time.gmtime()` now. Caller-provided is useful
            for deterministic tests.

    Returns:
        A dict matching the envelope shape from `envelope.py`. Body
        is empty; `kind` is "heartbeat"; ready to sign + wrap + send.
    """
    if timestamp_iso is None:
        timestamp_iso = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    return {
        "from": from_name,
        "to": "all",
        "ts": timestamp_iso,
        "channel": channel,
        "kind": HEARTBEAT_KIND,
        "msg": "",
    }


def is_heartbeat(envelope: dict) -> bool:
    """Cheap predicate: should monitor UI filter this envelope out of
    the rendered stream? Heartbeats are protocol traffic — users never
    see them; only `airc peers` consumes them.
    """
    return envelope.get("kind") == HEARTBEAT_KIND


def heartbeat_age_seconds(last_heartbeat_ts: Optional[int], now_ts: Optional[int] = None) -> Optional[int]:
    """How old is the most recent heartbeat from a given peer, in seconds.

    Returns None when we have never received a heartbeat from this peer
    — distinguishes "legacy peer (pre-#644, no heartbeats)" from "peer
    we've never heard from at all." Downstream code (cmd_peers) treats
    None as "unknown" rather than "process down" so a partially-rolled-out
    fleet doesn't false-positive every legacy peer.

    Args:
        last_heartbeat_ts: epoch seconds of the most recent heartbeat,
            or None if no heartbeat has been seen.
        now_ts: epoch seconds for "now"; defaults to `int(time.time())`.
            Caller-provided is useful for deterministic tests.
    """
    if last_heartbeat_ts is None:
        return None
    if now_ts is None:
        now_ts = int(time.time())
    return max(0, now_ts - last_heartbeat_ts)


def is_process_likely_down(last_heartbeat_age_sec: Optional[int]) -> bool:
    """True when last_heartbeat is older than STALE_HEARTBEAT_SEC. False
    when last_heartbeat is fresh OR when last_heartbeat is None (legacy
    peer; we don't know).

    Why not return True for None: a pre-#644 peer emits no heartbeats.
    During the heartbeat-rollout window, every legacy peer would
    spuriously appear "process down" if None defaulted to True. The
    correct behavior is to admit ignorance and display "unknown" until
    the peer's airc updates.
    """
    if last_heartbeat_age_sec is None:
        return False
    return last_heartbeat_age_sec > STALE_HEARTBEAT_SEC
