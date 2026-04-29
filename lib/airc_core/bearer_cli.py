"""Bash-callable bearer CLI.

The bridge between airc's bash command files and the Python bearer
abstraction. Two subcommands:

    python -m airc_core.bearer_cli send <peer_id> <channel> \\
        --host-target <ht> --identity-key <k> --remote-home <rh>

    python -m airc_core.bearer_cli recv <peer_id> \\
        --host-target <ht> --identity-key <k> --remote-home <rh> \\
        [--offset-file <path>]

`send` reads payload from stdin (bytes; framing is the caller's concern —
the bearer treats it as opaque). The outcome is printed to stdout as
a single line of JSON:

    {"kind": "delivered", "detail": ""}

`recv` opens bearer.recv_stream() and writes one line per inbound
envelope to stdout — the raw envelope bytes (a JSON object terminated
by newline). Suitable for piping directly into monitor_formatter, which
already consumes JSONL. Exits 0 on EOF / signal / broken pipe; the
bearer handles transport-level reconnects internally. If the caller's
formatter dies (broken pipe), recv exits cleanly so the bash watchdog
can observe the cycle end and decide whether to re-launch.

Bash callers parse `send`'s outcome `kind` and branch. Exit code is
always 0 unless something is structurally wrong (missing required arg,
malformed invocation); send failures are reported via outcome.kind,
not exit status, so the caller can do its own queue/error logic.

Why a CLI rather than direct python imports from bash: bash has no
way to invoke Python class methods without a process boundary anyway,
and the CLI is the natural seam. It also keeps bearer_ssh.py / bearer_gh.py
free of bash-side concerns — they implement the bearer interface and
nothing else.
"""

from __future__ import annotations

import argparse
import json
import os
import signal
import sys
from dataclasses import asdict

from .bearer_resolver import resolve


def cmd_send(args) -> int:
    peer_meta = {
        "host_target": args.host_target,
        "remote_home": args.remote_home,
        "identity_key": args.identity_key,
        "room_gist_id": getattr(args, "room_gist_id", None),
    }
    # Drop None values so can_serve / send check absence cleanly.
    peer_meta = {k: v for k, v in peer_meta.items() if v}

    try:
        bearer = resolve(peer_meta)
    except Exception as e:  # PeerUnreachable + any resolver-side error
        # Surface as a transient failure outcome rather than crashing the
        # caller. The caller's queue+retry path handles it.
        print(json.dumps({"kind": "transient_failure", "detail": f"resolver error: {e}"}))
        return 0

    bearer.open(args.peer_id)
    payload = sys.stdin.buffer.read()
    try:
        outcome = bearer.send(args.peer_id, args.channel, payload)
    finally:
        bearer.close()

    print(json.dumps(asdict(outcome)))
    return 0


def cmd_recv(args) -> int:
    """Stream events from the bearer to stdout as raw envelope bytes.

    One line per event. The line is the unmodified envelope bytes the
    bearer captured from the wire (see ReceivedMessage.payload), which
    is JSONL-shaped — directly pipeable into monitor_formatter.

    Optional --state-file: the bearer-attested liveness surface for
    cross-process consumers (airc status, airc peers). After each event
    we atomically rewrite the file with bearer kind, events_total,
    last_recv_ts, last_sender, and the bearer's diagnostic. Phase 2c
    of the bearer rewrite — replaces the messages.jsonl-mirror-derived
    "last recv" lie that was passing 30+ minute silences as healthy.

    The bearer handles transport-level reconnects (transient SSH drops,
    polling cadence for gh-as-bearer, etc). This loop exits only on:
      - EOF from recv_stream (bearer closed)
      - SIGTERM / SIGINT
      - BrokenPipeError (formatter on the other end of the pipe died);
        the bash monitor's watchdog interprets that and decides whether
        to relaunch us.
    """
    peer_meta = {
        "host_target": args.host_target,
        "remote_home": args.remote_home,
        "identity_key": args.identity_key,
        "offset_file": args.offset_file,
        "room_gist_id": getattr(args, "room_gist_id", None),
    }
    peer_meta = {k: v for k, v in peer_meta.items() if v}

    try:
        bearer = resolve(peer_meta)
    except Exception as e:
        # Same shape as cmd_send: keep stderr loud rather than silent (per
        # CLAUDE.md "never swallow errors") so the bash monitor + a human
        # tailing logs both see why we couldn't open a stream.
        print(f"bearer recv: resolver error: {e}", file=sys.stderr, flush=True)
        return 2

    # Translate SIGTERM into a clean close — pytest / bash kill us this way.
    # Default SIGINT handler already raises KeyboardInterrupt which we catch.
    def _on_term(signum, frame):
        bearer.close()
    try:
        signal.signal(signal.SIGTERM, _on_term)
    except ValueError:
        pass  # not on the main thread (test harness) — best-effort

    bearer.open(args.peer_id)
    out = sys.stdout.buffer
    state_file = args.state_file
    events_total = 0
    if state_file:
        # Initial state on launch — distinguishes "bearer is up but no events
        # yet" from "no bearer at all." Status surfaces this as
        # "awaiting first event" rather than going silent.
        _write_state_file(state_file, {
            "kind": getattr(bearer, "KIND", "unknown"),
            "peer_id": args.peer_id,
            "last_recv_ts": None,
            "last_sender": None,
            "events_total": 0,
            "diag": "bearer open, no events yet",
        })

    # Heartbeat: emit a sentinel JSON line to stdout every N seconds
    # regardless of bearer activity. Two purposes:
    #   1. Lets monitor_formatter's stdin-watchdog distinguish "bearer
    #      idle" (heartbeats arriving) from "bearer stuck" (silence).
    #      Pre-fix the watchdog was disabled for hosts because there
    #      was no signal during idle; with heartbeats, it's safe on.
    #   2. Liveness probe for the bash multi-channel watcher. A
    #      heartbeat in stdout proves the python loop made it past
    #      the GH poll, not just that the process is alive.
    # Joel 2026-04-29: "polling easily shuts down on its own OR never
    # even worked, sending/pinging triggered anything to occur." This
    # was the missing signal.
    import threading as _t
    try:
        _heartbeat_sec = float(os.environ.get("AIRC_BEARER_HEARTBEAT_SEC", "30"))
    except (TypeError, ValueError):
        _heartbeat_sec = 30.0
    _stop_heartbeat = _t.Event()
    _stdout_lock = _t.Lock()

    def _emit_heartbeat():
        import time as _ti
        room = getattr(args, "room_gist_id", "") or ""
        # Tick on a short interval so close() is responsive without
        # missing the cadence target.
        next_tick = _ti.monotonic() + _heartbeat_sec
        while not _stop_heartbeat.is_set():
            now = _ti.monotonic()
            if now >= next_tick:
                line = (json.dumps({
                    "airc_heartbeat": 1,
                    "ts": _ti.time(),
                    "channel": room,
                }) + "\n").encode("utf-8")
                with _stdout_lock:
                    try:
                        out.write(line)
                        out.flush()
                    except (BrokenPipeError, ValueError):
                        # Downstream gone or stdout closed; stop trying.
                        return
                next_tick = now + _heartbeat_sec
            # Wake every 100ms so close() takes effect promptly.
            _stop_heartbeat.wait(0.1)

    _hb_thread = _t.Thread(target=_emit_heartbeat, daemon=True)
    _hb_thread.start()

    try:
        for ev in bearer.recv_stream():
            line = ev.payload
            if not line.endswith(b"\n"):
                line = line + b"\n"
            try:
                with _stdout_lock:
                    out.write(line)
                    out.flush()
            except BrokenPipeError:
                # Downstream formatter exited. Caller's watchdog will
                # observe the broken cycle and reconnect us if needed.
                break
            if state_file:
                events_total += 1
                live = bearer.liveness(args.peer_id)
                _write_state_file(state_file, {
                    "kind": getattr(bearer, "KIND", "unknown"),
                    "peer_id": args.peer_id,
                    "last_recv_ts": live.last_seen_ts,
                    "last_sender": ev.sender_peer_id,
                    "events_total": events_total,
                    "diag": live.bearer_diag,
                })
    except KeyboardInterrupt:
        pass
    finally:
        _stop_heartbeat.set()
        bearer.close()
        # Brief join — daemon=True means we don't hang if the thread
        # is mid-write; the daemon flag handles process exit.
        _hb_thread.join(timeout=0.3)
    return 0


def _write_state_file(path: str, state: dict) -> None:
    """Atomically rewrite the bearer-state file. Best-effort — failures
    are swallowed because state-file IO must never break the recv loop;
    the bash watchdog and the bearer's own liveness signal remain the
    source of truth even if status surfacing goes stale.

    Atomic rewrite via tmp + rename so a reader (airc status) never
    sees a half-written file.
    """
    import os
    import tempfile
    try:
        d = os.path.dirname(path) or "."
        fd, tmp = tempfile.mkstemp(prefix=".bearer_state-", dir=d)
        try:
            with os.fdopen(fd, "w") as f:
                json.dump(state, f)
            os.replace(tmp, path)
        except Exception:
            try:
                os.unlink(tmp)
            except OSError:
                pass
    except OSError:
        pass


def _build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="airc_core.bearer_cli")
    sub = p.add_subparsers(dest="cmd", required=True)

    send = sub.add_parser("send", help="Deliver payload from stdin to peer")
    send.add_argument("peer_id")
    send.add_argument("channel")
    send.add_argument("--host-target", default=None,
                      help="user@host[:port] for SSH bearer")
    send.add_argument("--identity-key", default=None,
                      help="Path to private key file for SSH bearer")
    send.add_argument("--remote-home", default=None,
                      help="Remote AIRC_WRITE_DIR path (e.g. '$HOME/.airc')")
    send.add_argument("--room-gist-id", default=None,
                      help="gh room gist id for GhBearer routing")
    send.set_defaults(func=cmd_send)

    recv = sub.add_parser("recv", help="Stream inbound events as JSONL on stdout")
    recv.add_argument("peer_id")
    recv.add_argument("--host-target", default=None,
                      help="user@host[:port] for SSH bearer")
    recv.add_argument("--identity-key", default=None,
                      help="Path to private key file for SSH bearer")
    recv.add_argument("--remote-home", default=None,
                      help="Remote AIRC_WRITE_DIR path (e.g. '$HOME/.airc')")
    recv.add_argument("--offset-file", default=None,
                      help="Path to monitor_offset file for resume-on-reconnect")
    recv.add_argument("--state-file", default=None,
                      help="Path to bearer_state.json — bearer-attested liveness "
                           "for cross-process consumers (airc status, airc peers)")
    recv.add_argument("--room-gist-id", default=None,
                      help="gh room gist id for GhBearer routing")
    recv.set_defaults(func=cmd_recv)

    return p


def _cli() -> int:
    args = _build_parser().parse_args()
    return args.func(args)


if __name__ == "__main__":
    sys.exit(_cli())
