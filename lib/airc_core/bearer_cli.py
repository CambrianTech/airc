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
import atexit
import json
import os
import signal
import subprocess
import sys
import time
import threading
from dataclasses import asdict
from typing import Optional, Tuple, Union

from .bearer_resolver import resolve


# ── Per-(scope, gist) bearer lock ─────────────────────────────────────
# Without this lock, scope teardown that leaks orphan bearers OR a
# host-gist-rotation respawn that races with the old bearer can end up
# with TWO bearer_cli processes polling the same gist. Both yield each
# new line to their stdout. If their pipes
# converge (the parent monitor's children share a downstream pipe, or
# both feed an unprivileged log), each downstream message arrives twice.
#
# Repro Joel hit 2026-05-04: peer broadcast appeared TWICE in the
# monitor stream with identical body + identical session nonce.
# Diagnosed: 3 live bearer_cli processes for #general across leaked
# test scopes + the live monitor's own respawned child.
#
# Pattern matches cmd_connect's #97 self-heal: pidfile + cmdline-shape
# verification (kill -0 alone is unsafe — OS reuses PIDs after wake).

_LOCK_DISABLED = "disabled"
_LOCK_HELD = "held"
RecvLockResult = Union[Tuple[str, int], str]
_STATE_FILE_LOCK = threading.RLock()


def _pid_alive(pid: int) -> bool:
    """True iff pid is a live process. Same `os.kill(pid, 0)` probe
    cmd_connect / handshake use; no signal sent.

    PermissionError (EPERM) means the PID exists but is owned by another
    user — still counts as alive. ProcessLookupError (ESRCH) means dead.
    Other OSError means we can't tell — be conservative, assume alive.
    """
    if pid <= 0:
        return False
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    except OSError:
        return True


def _bearer_cmdline_matches(pid: int, expected_gist: str) -> Optional[bool]:
    """Return True/False when process shape is known, None when unknown."""
    try:
        out = subprocess.run(
            ["ps", "-p", str(pid), "-o", "command="],
            capture_output=True, text=True, timeout=2,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if out.returncode != 0:
        return None
    cmdline = (out.stdout or "").strip()
    if "bearer_cli" not in cmdline or " recv " not in f" {cmdline} ":
        return False
    if expected_gist:
        return f"--room-gist-id {expected_gist}" in cmdline
    return True


def _is_our_bearer(pid: int, expected_gist: str) -> bool:
    """True iff PID's cmdline matches bearer_cli recv for `expected_gist`.

    Cross-platform via `ps -p <pid> -o command=`. The PID alone is unsafe
    (OS reuses PIDs after sleep/wake — Joel hit this in #97). Verify the
    process is actually a bearer_cli recv AND for our gist; otherwise
    treat the pidfile as stale.
    """
    return _bearer_cmdline_matches(pid, expected_gist) is True


def _read_lock_owner(pidfile: str) -> Tuple[int, str]:
    try:
        with open(pidfile, "r") as f:
            data = f.read().strip()
    except OSError:
        return (0, "")
    parts = data.split("\t", 1)
    try:
        pid = int(parts[0]) if parts and parts[0] else 0
    except ValueError:
        pid = 0
    gist = parts[1] if len(parts) > 1 else ""
    return (pid, gist)


def _recv_lock_pidfile(state_file: str, gist_id: str) -> str:
    """Return the pidfile path for a recv lock.

    Natural key is the wire, not the channel label. Two subscribed
    channels may legitimately point at the same gist during host
    bootstrap, gist rotation, or same-room aliases; only one bearer
    should poll that gist in a scope. The envelope's channel field and
    monitor_formatter handle display/routing downstream.
    """
    if gist_id:
        lock_dir = os.path.dirname(state_file) or "."
        safe = "".join(c if c.isalnum() else "_" for c in gist_id)
        return os.path.join(lock_dir, f"bearer_gist.{safe}.pid")
    if state_file.endswith(".json"):
        return state_file[: -len(".json")] + ".pid"
    return state_file + ".pid"


def _claim_recv_lock(args) -> RecvLockResult:
    """Per-(scope, gist) bearer-recv lock.

    Returns (pidfile_path, my_pid) on successful claim — caller registers
    atexit cleanup. Returns _LOCK_HELD if another live bearer already
    serves this stream — caller should exit 0 cleanly. Returns
    _LOCK_DISABLED when no lock can/should be used, and caller should
    continue without the duplicate-emission guarantee.

    Pidfile path is derived from --state-file's directory and
    --room-gist-id (e.g. bearer_gist.<gist>.pid). Without --state-file,
    no lock is taken (legacy / test invocations that don't use
    state-file stay unaffected). Without --room-gist-id, the legacy
    state-file-derived path is used.

    Stale-pidfile recovery:
      - PID dead → stale; take over.
      - PID alive but cmdline isn't a bearer_cli recv for OUR gist →
        stale; take over. (Covers OS-PID-reuse after sleep/wake AND the
        "old bearer is for a rotated-away gist" case.)
      - PID alive AND cmdline is a bearer_cli recv for our gist →
        another bearer is healthy; exit cleanly.
    """
    state_file = getattr(args, "state_file", None)
    if not state_file:
        return _LOCK_DISABLED

    my_gist = getattr(args, "room_gist_id", None) or ""
    pidfile = _recv_lock_pidfile(state_file, my_gist)
    my_pid = os.getpid()

    lock_payload = f"{my_pid}\t{my_gist}\n".encode("utf-8")
    for _ in range(3):
        try:
            fd = os.open(pidfile, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o644)
            try:
                os.write(fd, lock_payload)
            finally:
                os.close(fd)
            return (pidfile, my_pid)
        except FileExistsError:
            # Format: "<pid>\t<gist_id>". gist_id may be empty for non-gh bearers.
            other_pid, _other_gist = _read_lock_owner(pidfile)
            if other_pid <= 0:
                # A competing process may have won O_EXCL but not yet
                # written the payload. Treat empty/partial owner records as
                # in-progress, not stale, or we defeat the lock.
                time.sleep(0.05)
                continue
            if other_pid != my_pid and _pid_alive(other_pid):
                cmd_match = _bearer_cmdline_matches(other_pid, my_gist)
                if cmd_match is True or (cmd_match is None and _other_gist == my_gist):
                    print(
                        f"bearer_cli recv: another bearer for gist {my_gist or '(none)'} "
                        f"already running (PID {other_pid}); exiting cleanly to avoid "
                        f"duplicate emission",
                        file=sys.stderr, flush=True,
                    )
                    return _LOCK_HELD
                if cmd_match is None:
                    print(
                        f"bearer_cli recv: pidfile {pidfile} is held by live PID {other_pid} "
                        f"but cmdline could not be verified; treating lock as held",
                        file=sys.stderr, flush=True,
                    )
                    return _LOCK_HELD
            # else: stale (dead pid, OS-reused with known non-bearer shape,
            # or old gist with known non-matching shape) → remove then retry
            # O_EXCL claim. If another process wins the race, the next loop
            # sees its pidfile and exits or retries accordingly.
            try:
                os.unlink(pidfile)
            except FileNotFoundError:
                pass
            except OSError as e:
                print(
                    f"bearer_cli recv: could not remove stale pidfile {pidfile}: {e}; "
                    f"proceeding without lock (duplicate emission possible)",
                    file=sys.stderr, flush=True,
                )
                return _LOCK_DISABLED
        except OSError as e:
            # Loud, not silent — we'd rather know we couldn't lock than
            # silently disable the dedup guarantee.
            print(
                f"bearer_cli recv: could not write pidfile {pidfile}: {e}; "
                f"proceeding without lock (duplicate emission possible)",
                file=sys.stderr, flush=True,
            )
            return _LOCK_DISABLED

    print(
        f"bearer_cli recv: pidfile {pidfile} stayed in an in-progress state; "
        f"treating lock as held to avoid duplicate emission",
        file=sys.stderr, flush=True,
    )
    return _LOCK_HELD


def _release_lock(pidfile: str, my_pid: int) -> None:
    """Remove the pidfile only if it still has OUR pid (don't stomp a
    pidfile that some other bearer rewrote after we ran)."""
    owner, _gist = _read_lock_owner(pidfile)
    if owner == my_pid:
        try:
            os.unlink(pidfile)
        except OSError:
            pass


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


def cmd_send_batch(args) -> int:
    peer_meta = {
        "host_target": args.host_target,
        "remote_home": args.remote_home,
        "identity_key": args.identity_key,
        "room_gist_id": getattr(args, "room_gist_id", None),
    }
    peer_meta = {k: v for k, v in peer_meta.items() if v}

    try:
        bearer = resolve(peer_meta)
    except Exception as e:
        print(json.dumps({"kind": "transient_failure", "detail": f"resolver error: {e}"}))
        return 0

    bearer.open(args.peer_id)
    payloads = [
        (line if line.endswith(b"\n") else line + b"\n")
        for line in sys.stdin.buffer.read().splitlines()
        if line.strip()
    ]
    try:
        outcome = bearer.send_many(args.peer_id, args.channel, payloads)
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
    # Per-(scope, channel, gist) bearer lock. Prevents duplicate
    # stream-emission when scope teardown leaks orphan bearers OR a
    # host-gist-rotation respawn races with the old bearer (Joel's
    # 2026-05-04 dup-message diagnosis). Without --state-file, no lock
    # is taken — legacy / test invocations are unaffected.
    lock = _claim_recv_lock(args)
    if lock == _LOCK_HELD:
        # Another live bearer is already serving this stream.
        # Exit cleanly so the parent monitor's watchdog doesn't escalate.
        return 0
    if isinstance(lock, tuple):
        atexit.register(_release_lock, lock[0], lock[1])

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
                if state_file:
                    _touch_state_heartbeat(state_file, _ti.time())
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
                _touch_state_heartbeat(state_file, time.time())
    except KeyboardInterrupt:
        pass
    except Exception as e:
        if state_file:
            _write_state_file(state_file, {
                "kind": getattr(bearer, "KIND", "unknown"),
                "peer_id": args.peer_id,
                "last_recv_ts": None,
                "last_sender": None,
                "events_total": events_total,
                "diag": f"bearer recv failed: {e}",
                "last_error": str(e),
                "last_error_ts": time.time(),
            })
        print(f"bearer recv: stream failed: {e}", file=sys.stderr, flush=True)
        return 3
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
    with _STATE_FILE_LOCK:
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


def _touch_state_heartbeat(path: str, ts: float) -> None:
    """Record bearer-loop heartbeat without pretending a peer message arrived.

    last_recv_ts remains the timestamp of the last real envelope. The
    heartbeat timestamp lets `airc status` distinguish an idle but live
    bearer from a dead channel, which is the signal users and agents need
    when a monitor silently wedges.
    """
    with _STATE_FILE_LOCK:
        try:
            with open(path) as f:
                state = json.load(f)
        except Exception:
            state = {}
        state["last_heartbeat_ts"] = ts
        _write_state_file(path, state)


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

    send_batch = sub.add_parser("send-batch", help="Deliver JSONL payload batch from stdin to peer")
    send_batch.add_argument("peer_id")
    send_batch.add_argument("channel")
    send_batch.add_argument("--host-target", default=None,
                            help="user@host[:port] for SSH bearer")
    send_batch.add_argument("--identity-key", default=None,
                            help="Path to private key file for SSH bearer")
    send_batch.add_argument("--remote-home", default=None,
                            help="Remote AIRC_WRITE_DIR path (e.g. '$HOME/.airc')")
    send_batch.add_argument("--room-gist-id", default=None,
                            help="gh room gist id for GhBearer routing")
    send_batch.set_defaults(func=cmd_send_batch)

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
