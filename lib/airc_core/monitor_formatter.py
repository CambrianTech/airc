"""airc monitor formatter.

Reads JSONL message stream from stdin, emits human-readable lines,
handles [rename] markers + ping/pong control traffic + own-send
filtering. Inactivity watchdog forces fmt_exit=2 if the channel
goes silent so the bash retry loop can probe the host.

Migrated from the bash monitor_formatter heredoc (~250 lines of
Python embedded in airc) to a proper Python module (#152 Phase 1).
Same logic, same stdin/stdout contract, but testable + readable in
a real .py file with no `'\\''` shell-escape gymnastics.

CLI:

    python -u -m airc_core.monitor_formatter --peers-dir <path> --my-name <name>
"""

from __future__ import annotations

import json
import os
import re
import signal
import sys

# Inactivity watchdog: if no inbound line arrives in WATCHDOG_SEC,
# exit with a distinct code so the caller's while-loop reconnects.
# Why: the outer SSH tail can hang silently — middleboxes drop idle
# TCP while still ACK'ing SSH ServerAlive keepalives, so SSH does
# not notice the channel is dead, and tail -F never returns EOF. The
# Python read just blocks forever. With an application-level watchdog,
# a truly dead channel forces the formatter out and the reconnect loop
# restarts the ssh. Normal chat traffic keeps resetting the alarm so
# there is no penalty when the channel is healthy.
#
# Joel 2026-04-24: heartbeat is OFF by default (canary 95d9907), so
# every fmt_exit=2 used to look like "host went quiet" and spam restart
# notifications on healthy idle. Fix is in the bash retry loop: it
# probes the host on fmt_exit=2 BEFORE counting/notifying. Probe
# success = healthy idle (silent reset); probe failure = real death
# (notify + count toward escalation).
#
# With the probe, WATCHDOG_SEC is just the polling cadence at which
# we re-check the channel. 150s × ESCALATE_AFTER=2 = 5 minutes total
# dead-host detection per Joel's spec.
WATCHDOG_SEC = 150


def _watchdog_exit(signum=None, frame=None):
    # Diagnostic to stderr only. The bash retry loop owns the
    # user-visible notification — it probes the host on fmt_exit=2
    # to decide whether silence means "healthy idle" (silent reset)
    # or "host actually unreachable" (notify + count). Emitting from
    # python here would notify on every healthy-idle cycle.
    sys.stderr.write(f"[airc:monitor] no inbound in {WATCHDOG_SEC}s — exiting for probe\n")
    sys.stderr.flush()
    os._exit(2)


# Cross-platform watchdog. POSIX (mac/linux/WSL) gets signal.SIGALRM
# which is cheaper (single-thread, kernel-armed). Windows Python has
# no SIGALRM so we fall back to threading.Timer — same exit semantics,
# slight overhead from the timer thread. Either way the fmt_exit=2
# contract is preserved.
try:
    signal.signal(signal.SIGALRM, _watchdog_exit)
    signal.alarm(WATCHDOG_SEC)

    def _arm_watchdog():
        signal.alarm(WATCHDOG_SEC)
except (AttributeError, ValueError):
    import threading

    _wd_timer_holder = [None]

    def _arm_watchdog():
        if _wd_timer_holder[0] is not None:
            _wd_timer_holder[0].cancel()
        t = threading.Timer(WATCHDOG_SEC, _watchdog_exit)
        t.daemon = True
        t.start()
        _wd_timer_holder[0] = t

    _arm_watchdog()


# Marker may carry an optional `host=user@ip` so receivers can find the
# sender via stable host field even when name-keyed lookup would miss
# (chain break from a dropped rename, stale records, etc).
RENAME_RE = re.compile(r"^\[rename\] old=([a-z0-9-]+) new=([a-z0-9-]+)(?:\s+host=(\S+))?")


def _rename_files(peers_dir: str, old: str, new: str) -> bool:
    old_json = os.path.join(peers_dir, f"{old}.json")
    new_json = os.path.join(peers_dir, f"{new}.json")
    if not os.path.isfile(old_json):
        return False
    try:
        os.rename(old_json, new_json)
        d = json.load(open(new_json))
        d["name"] = new
        json.dump(d, open(new_json, "w"), indent=2)
    except Exception:
        pass
    old_pub = os.path.join(peers_dir, f"{old}.pub")
    new_pub = os.path.join(peers_dir, f"{new}.pub")
    if os.path.isfile(old_pub):
        try:
            os.rename(old_pub, new_pub)
        except Exception:
            pass
    return True


def _find_peer_by_host(peers_dir: str, host: str):
    """Return current name of the peer record whose host matches, or None."""
    if not host or not os.path.isdir(peers_dir):
        return None
    for entry in os.listdir(peers_dir):
        if not entry.endswith(".json"):
            continue
        try:
            d = json.load(open(os.path.join(peers_dir, entry)))
        except Exception:
            continue
        if d.get("host") == host:
            return d.get("name") or entry[:-5]
    return None


def _handle_rename(peers_dir: str, msg: str) -> bool:
    m = RENAME_RE.match(msg)
    if not m:
        return False
    old, new, host = m.group(1), m.group(2), m.group(3)
    # Primary path: name-keyed rename.
    if _rename_files(peers_dir, old, new):
        print(f"airc: nick {old} → {new}", flush=True)
        return True
    # Fallback: peer file sits under a different (older) name due to a
    # previous chain break. Resolve via stable host field.
    if host:
        current = _find_peer_by_host(peers_dir, host)
        if current and current != new and _rename_files(peers_dir, current, new):
            print(f"airc: nick (chain-repair) {current} → {new}", flush=True)
            return True
    return False


def run(my_name: str, peers_dir: str) -> int:
    """Stream the formatter loop. Returns process exit code."""
    scope_dir = os.path.dirname(peers_dir)
    config_path = os.path.join(scope_dir, "config.json")
    local_log = os.path.join(scope_dir, "messages.jsonl")
    offset_path = os.path.join(scope_dir, "monitor_offset")

    # Only mirror inbound to the local log when we are a joiner (tailing a
    # REMOTE host over SSH). For a HOST, the local log IS the source the
    # tail reads from — mirroring creates an infinite feedback loop.
    is_joiner = False
    try:
        is_joiner = bool(json.load(open(config_path)).get("host_target", ""))
    except Exception:
        pass

    # Room name for the chat-line prefix. Read once at startup; a rename
    # of the room would require a fresh airc connect to pick up. Default
    # is "general"; legacy single-pair invite scope shows "1:1" as the
    # visual marker.
    room_path = os.path.join(scope_dir, "room_name")
    try:
        room_name = open(room_path).read().strip() or "general"
    except Exception:
        room_name = "1:1"

    def current_name():
        """Read identity name fresh from config.json each time so a rename
        during the session immediately takes effect for own-send filtering.
        Without this the monitor keeps the name it saw at startup and fails
        to filter our own outbound rename markers, which can trigger the
        host-fallback chain-repair against other peers sharing our host."""
        try:
            return json.load(open(config_path)).get("name", "")
        except Exception:
            return ""

    offset_counter = 0
    try:
        with open(offset_path) as f:
            offset_counter = int(f.read().strip() or 0)
    except Exception:
        pass

    for line in sys.stdin:
        # Any inbound line — real message, heartbeat, whatever — means the
        # channel is alive. Reset the watchdog.
        _arm_watchdog()
        line = line.strip()
        if not line:
            continue
        offset_counter += 1
        try:
            with open(offset_path, "w") as f:
                f.write(str(offset_counter))
        except Exception:
            pass
        try:
            m = json.loads(line)
        except Exception:
            continue
        fr = m.get("from", "?")
        to = m.get("to", "")
        msg = m.get("msg", "")
        # Filter own sends early, including our own [rename] markers. Read
        # the name fresh so a mid-session rename takes effect immediately.
        if fr == current_name():
            continue
        # Mirror inbound to the local messages.jsonl ONLY when we are a
        # joiner (tailing the remote host). For a host the local log is
        # already the source of truth; mirroring would create a feedback
        # loop (tail sees line -> we append line -> tail sees it again).
        if is_joiner:
            try:
                with open(local_log, "a") as f:
                    f.write(line + "\n")
            except Exception:
                pass
        if _handle_rename(peers_dir, msg):
            continue
        # Ping/pong monitor-liveness probe. Prefix marker on a normal
        # message so non-implementing clients (older airc, Codex, etc)
        # just see a weird message. Auto-pong here is opportunistic;
        # cmd_ping tails the log for PONG with matching uuid + timeout,
        # which distinguishes wire-dead vs monitor-dead vs peer-no-support.
        ping_match = re.match(r"^\[PING:([a-f0-9-]+)\]", msg or "")
        pong_match = re.match(r"^\[PONG:([a-f0-9-]+)\]", msg or "")
        if ping_match:
            ping_id = ping_match.group(1)
            # Only auto-pong when the ping is addressed to US specifically.
            # Without this check every peer on the mesh auto-replies to
            # every ping they see in the log (monitor tails are shared
            # across the whole host), so a single ping fans out to N
            # PONGs and makes liveness diagnosis meaningless. Broadcast
            # pings (to=all) also skip here — a broadcast ping is a
            # discovery message the operator reads, not a round-trip.
            my_current = current_name()
            if to == my_current:
                # Auto-reply pong via subprocess. Fire-and-forget. Uses
                # airc send so the reply rides the same signed-message
                # path as normal traffic (no protocol divergence).
                import subprocess
                try:
                    subprocess.Popen(
                        ["airc", "send", f"@{fr}", f"[PONG:{ping_id}]"],
                        stdout=subprocess.DEVNULL,
                        stderr=subprocess.DEVNULL,
                    )
                except Exception:
                    pass
            # Suppress from user-visible output (control traffic),
            # regardless of whether we auto-ponged.
            continue
        if pong_match:
            # cmd_ping picks PONG up by tailing messages.jsonl directly.
            # Suppress to keep the chat surface clean.
            continue
        # One-liner per event. Every line starts with `airc:` so the source
        # is unambiguous when other Monitor tasks (continuum, tests, etc.)
        # are also firing notifications.
        #
        # No length cap any more — consumers (Claude Code Monitor, Codex,
        # log tailers, etc.) decide their own display truncation. Truncating
        # in the substrate forced everyone downstream to fall back to
        # `airc logs` to see anything past the cap, which is exactly the
        # polling-vs-substrate anti-pattern Joel called out 2026-04-24.
        # Newlines collapsed to spaces so each emitted event is still a
        # single line, but the full body always reaches the consumer.
        msg_one_line = (msg or "").replace("\n", " ").replace("\r", " ").strip()
        # Phase 2: prefer the envelope's `channel` field over the scope-
        # level `room_name`. The envelope field is per-message, so a
        # single scope can display a multi-channel stream with correct
        # per-line prefixing. Falls back to the scope's `room_name` for
        # pre-Phase-2 messages that don't carry the envelope field.
        line_channel = m.get("channel") or room_name
        try:
            if fr in ("airc", "sys"):
                # System events (joins, parts, drain, auth, watchdog).
                # Example:  airc: [#general] alice joined
                print(f"airc: [#{line_channel}] {msg_one_line}", flush=True)
            elif to and to not in ("all", ""):
                # DM with addressed recipient.
                # Example:  airc: [#general] bigmama → alice: quick question
                print(f"airc: [#{line_channel}] {fr} → {to}: {msg_one_line}", flush=True)
            else:
                # Broadcast.
                # Example:  airc: [#general] bigmama: hello everyone
                print(f"airc: [#{line_channel}] {fr}: {msg_one_line}", flush=True)
        except Exception as e:
            # Belt-and-suspenders — one bad message must never take the
            # whole monitor down. Surface to stderr (which the bash retry
            # loop captures) and keep going.
            try:
                sys.stderr.write(f"[airc:formatter] skipped one line: {e}\n")
                sys.stderr.flush()
            except Exception:
                pass
    return 0


def _cli() -> int:
    import argparse
    p = argparse.ArgumentParser(prog="airc_core.monitor_formatter")
    p.add_argument("--peers-dir", required=True)
    p.add_argument("--my-name", required=True)
    args = p.parse_args()
    return run(args.my_name, args.peers_dir)


if __name__ == "__main__":
    sys.exit(_cli())
