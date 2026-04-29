"""Bash-callable bearer CLI.

The bridge between airc's bash command files and the Python bearer
abstraction. Bash invokes:

    python -m airc_core.bearer_cli send <peer_id> <channel> \\
        --host-target <ht> --identity-key <k> --remote-home <rh>

Payload is read from stdin (bytes; framing is the caller's concern —
the bearer treats it as opaque). The outcome is printed to stdout as
a single line of JSON:

    {"kind": "delivered", "detail": ""}

Bash callers parse `kind` and branch. Exit code is always 0 unless
something is structurally wrong (missing required arg, malformed
invocation); send failures are reported via outcome.kind, not exit
status, so the caller can do its own queue/error logic.

Why a CLI rather than direct python imports from bash: bash has no
way to invoke Python class methods without a process boundary anyway,
and the CLI is the natural seam. It also keeps bearer_ssh.py / bearer_gh.py
free of bash-side concerns — they implement the bearer interface and
nothing else.
"""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import asdict

from .bearer_resolver import resolve


def cmd_send(args) -> int:
    peer_meta = {
        "host_target": args.host_target,
        "remote_home": args.remote_home,
        "identity_key": args.identity_key,
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
    send.set_defaults(func=cmd_send)

    return p


def _cli() -> int:
    args = _build_parser().parse_args()
    return args.func(args)


if __name__ == "__main__":
    sys.exit(_cli())
