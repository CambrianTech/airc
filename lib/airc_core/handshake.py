"""airc pair-handshake — joiner send + host accept + response field reads.

CLI tools take ARGS, not env vars. Paths come in via --airc-home /
--peers-dir / --identity-dir / --config / --messages so MSYS path-
translation behavior is predictable per-arg (callers can `//`-prefix
or set MSYS2_ARG_CONV_EXCL targeted-ly), and so the modules look
like normal Python CLIs instead of bash-shaped env-var contraptions.

Subcommands:

    python -m airc_core.handshake send <host> <port>
        --my-name X --my-host Y --my-ssh-pub Z --my-sign-pub W
        --my-airc-home /path --my-identity-json '{}'

    python -m airc_core.handshake accept_one
        --host-port 7547 --peers-dir /path --identity-dir /path
        --config /path/config.json --host-name X
        --reminder-interval 300 --airc-home /path --messages /path

    python -m airc_core.handshake get_field <name> [default]
        # reads JSON envelope from stdin, prints field
"""

from __future__ import annotations

import argparse
import json
import sys


# ── parse_response + get_field ──────────────────────────────────────────


def parse_response(response_json: str) -> dict:
    """Parse a handshake-response JSON string. Returns {} on failure."""
    if not response_json:
        return {}
    try:
        obj = json.loads(response_json)
        return obj if isinstance(obj, dict) else {}
    except (ValueError, TypeError):
        return {}


def cmd_get_field(args) -> int:
    try:
        response = sys.stdin.read()
    except Exception:
        print(args.default)
        return 0
    obj = parse_response(response)
    v = obj.get(args.field, args.default)
    if isinstance(v, (dict, list)):
        print(json.dumps(v))
    else:
        print(v if v != "" else args.default)
    return 0


# ── joiner: send ────────────────────────────────────────────────────────


def cmd_send(args) -> int:
    import socket

    payload = json.dumps({
        "name": args.my_name,
        "host": args.my_host,
        "ssh_pub": args.my_ssh_pub,
        "sign_pub": args.my_sign_pub,
        "airc_home": args.my_airc_home,
        "identity": json.loads(args.my_identity_json or "{}"),
    })

    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.settimeout(30)
    try:
        s.connect((args.host, args.port))
        s.sendall((payload + "\n").encode())
        s.shutdown(socket.SHUT_WR)
        data = b""
        while True:
            chunk = s.recv(4096)
            if not chunk:
                break
            data += chunk
        s.close()
        print(data.decode().strip())
        return 0
    except Exception as e:
        print(f"airc-handshake-send-error: {e}", file=sys.stderr)
        return 1


# ── host: accept_one ────────────────────────────────────────────────────


def _start_parent_watch(watch_pid: int):
    """Daemon thread that os._exit()s the moment the watched PID dies (#132).

    The accept_one process is a grandchild of the airc parent bash:
        airc bash → accept-loop subshell → python accept_one
    If the airc parent bash dies (terminal closed, kill, Monitor tool
    teardown), the accept-loop subshell reparents to init but stays
    alive (running its `while kill -0 PARENT` loop until the next
    iteration). During python's in-flight accept() / recv() we'd miss
    that — getppid() points at the accept-loop subshell, which is
    still alive — so any joiner that connects during this window gets
    a real-looking pair handshake against a ghost host (keys land in
    authorized_keys, peer record gets written, no relay behind it).

    Watching the airc bash PID directly (passed in via --watch-pid)
    fixes this. `os.kill(pid, 0)` is the probe: it sends no signal,
    just raises OSError if the PID is gone. Poll once a second; the
    moment the airc bash disappears, os._exit(0) breaks out of any
    blocking syscall and dies cleanly.

    Daemon thread so it doesn't block clean shutdown when the parent
    IS alive and accept_one returns normally.
    """
    import os
    import threading
    import time

    def _watch():
        while True:
            try:
                os.kill(watch_pid, 0)
            except (OSError, ProcessLookupError):
                # airc bash gone — break out of any blocking syscall.
                os._exit(0)
            time.sleep(1)

    t = threading.Thread(target=_watch, daemon=True)
    t.start()


def cmd_accept_one(args) -> int:
    import datetime
    import os
    import socket

    if args.watch_pid:
        _start_parent_watch(args.watch_pid)

    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.bind(("0.0.0.0", args.host_port))
    sock.listen(1)
    sock.settimeout(10)
    while True:
        try:
            conn, _addr = sock.accept()
            break
        except socket.timeout:
            if os.getppid() == 1:
                sock.close()
                return 0

    data = b""
    while True:
        chunk = conn.recv(4096)
        if not chunk:
            break
        data += chunk
        if b"\n" in data:
            break

    joiner = json.loads(data.decode().strip())

    # Authorize joiner's SSH key.
    ssh_dir = os.path.expanduser("~/.ssh")
    os.makedirs(ssh_dir, mode=0o700, exist_ok=True)
    ak = os.path.join(ssh_dir, "authorized_keys")
    ssh_key = joiner.get("ssh_pub", "")
    if ssh_key:
        existing = open(ak).read() if os.path.exists(ak) else ""
        if ssh_key not in existing:
            with open(ak, "a") as f:
                f.write(ssh_key.strip() + "\n")
            os.chmod(ak, 0o600)

    # Save joiner as peer (with stable-host stale cleanup).
    peers_dir = os.path.expanduser(args.peers_dir)
    os.makedirs(peers_dir, exist_ok=True)
    jname = joiner["name"]
    jhost = joiner.get("host", "")
    if jhost and os.path.isdir(peers_dir):
        for entry in os.listdir(peers_dir):
            if not entry.endswith(".json") or entry == jname + ".json":
                continue
            try:
                d = json.load(open(os.path.join(peers_dir, entry)))
            except Exception:
                continue
            if d.get("host") == jhost:
                for ext in (".json", ".pub"):
                    p = os.path.join(peers_dir, entry[:-5] + ext)
                    if os.path.isfile(p):
                        try:
                            os.remove(p)
                        except Exception:
                            pass

    timestamp = datetime.datetime.utcnow().strftime("%Y-%m-%dT%H:%M:%SZ")
    with open(os.path.join(peers_dir, jname + ".json"), "w") as f:
        json.dump({
            "name": jname,
            "host": joiner.get("host", ""),
            "airc_home": joiner.get("airc_home", ""),
            "paired": timestamp,
            "ssh_pub": joiner.get("ssh_pub", ""),
            "identity": joiner.get("identity", {}),
        }, f, indent=2)
    if joiner.get("sign_pub"):
        with open(os.path.join(peers_dir, jname + ".pub"), "w") as f:
            f.write(joiner["sign_pub"])

    # Build response.
    identity_dir = os.path.expanduser(args.identity_dir)
    host_pub = open(os.path.join(identity_dir, "ssh_key.pub")).read().strip()
    host_identity = {}
    try:
        host_config = json.load(open(args.config))
        host_identity = host_config.get("identity", {}) or {}
    except Exception:
        pass
    response = json.dumps({
        "ssh_pub": host_pub,
        "name": args.host_name,
        "reminder": args.reminder_interval,
        "airc_home": args.airc_home,
        "identity": host_identity,
    })
    conn.sendall((response + "\n").encode())
    conn.close()
    sock.close()

    print(f"  Peer joined: {jname}")
    # Surface the join as a system event in messages.jsonl.
    try:
        room_name_path = os.path.join(args.airc_home, "room_name")
        room_name = open(room_name_path).read().strip() if os.path.isfile(room_name_path) else "general"
        event = {
            "ts": timestamp,
            "from": "airc",
            "to": "all",
            "channel": room_name,
            "msg": f"{jname} joined #{room_name}",
        }
        with open(args.messages, "a") as f:
            f.write(json.dumps(event) + "\n")
    except Exception:
        pass
    return 0


# ── CLI entry ───────────────────────────────────────────────────────────


def _build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="airc_core.handshake")
    sub = p.add_subparsers(dest="cmd", required=True)

    # get_field — stdin-driven response field extract
    g = sub.add_parser("get_field")
    g.add_argument("field")
    g.add_argument("default", nargs="?", default="")
    g.set_defaults(func=cmd_get_field)

    # send — joiner-side TCP handshake
    s = sub.add_parser("send")
    s.add_argument("host")
    s.add_argument("port", type=int)
    s.add_argument("--my-name", default="")
    s.add_argument("--my-host", default="")
    s.add_argument("--my-ssh-pub", default="")
    s.add_argument("--my-sign-pub", default="")
    s.add_argument("--my-airc-home", default="")
    s.add_argument("--my-identity-json", default="{}")
    s.set_defaults(func=cmd_send)

    # accept_one — host-side TCP listener (one accept per call)
    a = sub.add_parser("accept_one")
    a.add_argument("--host-port", type=int, default=7547)
    a.add_argument("--peers-dir", required=True)
    a.add_argument("--identity-dir", required=True)
    a.add_argument("--config", required=True)
    a.add_argument("--host-name", required=True)
    a.add_argument("--reminder-interval", type=int, default=300)
    a.add_argument("--airc-home", required=True)
    a.add_argument("--messages", required=True)
    # --watch-pid: airc parent bash PID. The listener spawns a daemon
    # thread that os._exit()s the moment this PID disappears (#132).
    # 0 disables the watch (legacy callers / direct invocations).
    a.add_argument("--watch-pid", type=int, default=0)
    a.set_defaults(func=cmd_accept_one)

    return p


def _cli() -> int:
    args = _build_parser().parse_args()
    return args.func(args)


if __name__ == "__main__":
    sys.exit(_cli())
