"""Pair-handshake response parsing for airc.

When a joiner connects to a host, the host returns a JSON envelope
with fields the joiner caches in its config (host's name, ssh_pub,
airc_home, reminder interval, identity blob). Pre-migration each
field-extract was an inline `python -c "import json; print(...)"`
heredoc; bash variable substitution into the python source was a
silent-fail vector (continuum-b69f's PR #164/#165 retest 2026-04-27
caught the host_airc_home write-side; this is the read-side).

Post-migration: response JSON comes via stdin, field name + default
via argv. Python source is fixed bytes; bash never touches it.

CLI:

    echo "$response" | python -m airc_core.handshake get_field <name> [default]

Empty stdout on parse failure (matches the bash `|| true` fallback
pattern). Exit always 0 — caller checks the value.
"""

from __future__ import annotations

import json
import sys


def parse_response(response_json: str) -> dict:
    """Parse a handshake-response JSON string. Returns {} on failure."""
    if not response_json:
        return {}
    try:
        obj = json.loads(response_json)
        return obj if isinstance(obj, dict) else {}
    except (ValueError, TypeError):
        return {}


def send(host: str, port: int) -> str:
    """Joiner-side: build payload from env vars, connect to host:port,
    send, read response, return as string. Caller checks for empty
    string on failure.

    Env vars:
        MY_NAME, MY_HOST, MY_SSH_PUB, MY_SIGN_PUB, MY_AIRC_HOME,
        MY_IDENTITY (JSON string of identity dict)

    Pre-migration this was an inline `python -c "..."` heredoc with
    five bash-variable substitutions INTO the python source. Any
    special character in any field (apostrophe in bio, embedded
    newline in ssh_pub) silently broke parsing. Now: env vars + argv.
    """
    import os
    import socket as sock_mod

    payload = json.dumps({
        "name": os.environ.get("MY_NAME", ""),
        "host": os.environ.get("MY_HOST", ""),
        "ssh_pub": os.environ.get("MY_SSH_PUB", ""),
        "sign_pub": os.environ.get("MY_SIGN_PUB", ""),
        "airc_home": os.environ.get("MY_AIRC_HOME", ""),
        "identity": json.loads(os.environ.get("MY_IDENTITY", "{}") or "{}"),
    })

    s = sock_mod.socket(sock_mod.AF_INET, sock_mod.SOCK_STREAM)
    s.settimeout(30)
    s.connect((host, int(port)))
    s.sendall((payload + "\n").encode())
    s.shutdown(sock_mod.SHUT_WR)
    data = b""
    while True:
        chunk = s.recv(4096)
        if not chunk:
            break
        data += chunk
    s.close()
    return data.decode().strip()


def _cli() -> int:
    if len(sys.argv) < 2:
        return 2
    cmd = sys.argv[1]
    if cmd == "get_field":
        if len(sys.argv) < 3:
            return 2
        field = sys.argv[2]
        default = sys.argv[3] if len(sys.argv) > 3 else ""
        try:
            response = sys.stdin.read()
        except Exception:
            print(default)
            return 0
        obj = parse_response(response)
        v = obj.get(field, default)
        # Numbers (e.g. reminder=300) round-trip cleanly through str();
        # nested objects (e.g. identity={}) need json.dumps so callers
        # get a parseable string back rather than Python repr.
        if isinstance(v, (dict, list)):
            print(json.dumps(v))
        else:
            print(v if v != "" else default)
        return 0
    if cmd == "send":
        if len(sys.argv) < 4:
            return 2
        host = sys.argv[2]
        port = sys.argv[3]
        try:
            print(send(host, port))
            return 0
        except Exception as e:
            # Stderr surfaces; bash's `2>&1` capture lets cmd_connect's
            # die() print the actual error per the never-swallow-errors
            # rule.
            print(f"airc-handshake-send-error: {e}", file=sys.stderr)
            return 1
    print(f"unknown subcommand: {cmd}", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(_cli())
