"""airc config.json CRUD.

CLI takes paths as `--config /path/to/config.json` (argparse args), not
env vars. Avoids MSYS path-translation surprises on Git Bash and makes
the module present as a normal Python CLI.
"""

from __future__ import annotations

import argparse
import json
import sys


def get(config_path: str, key: str, default: str = "") -> str:
    """Read a key from config.json. Returns default on any failure.
    Nested objects (dicts/lists) round-trip as JSON-encoded strings so
    callers can re-parse if needed.
    """
    try:
        with open(config_path) as f:
            c = json.load(f)
        v = c.get(key)
        if v is None or v == "":
            return default
        if isinstance(v, (dict, list)):
            return json.dumps(v)
        return str(v)
    except (OSError, ValueError, KeyError):
        return default


def get_name(config_path: str) -> str:
    return get(config_path, "name", "unknown")


def cmd_get(args) -> int:
    print(get(args.config, args.key, args.default))
    return 0


def cmd_get_name(args) -> int:
    print(get_name(args.config))
    return 0


def cmd_set_name(args) -> int:
    """Atomically write the identity name into config.json.

    Replaces the inline-Python heredoc that lived in cmd_rename. With
    multi-scope rename propagation (#179), cmd_rename writes the name
    into the primary scope AND every sidecar scope's config; doing it
    via a single CLI call per scope keeps the write quoting-safe (the
    heredoc inlined `$new_name` into a python string literal which
    would have broken on names containing single quotes — fortunately
    the rename sanitizer only allows [a-z0-9-] today, but the heredoc
    pattern was a sharp edge).
    """
    try:
        c = json.load(open(args.config))
    except (OSError, ValueError) as e:
        print(f"airc-config-set-error: cannot read {args.config}: {e}", file=sys.stderr)
        return 1
    c["name"] = args.name
    try:
        json.dump(c, open(args.config, "w"), indent=2)
        return 0
    except OSError as e:
        print(f"airc-config-set-error: cannot write {args.config}: {e}", file=sys.stderr)
        return 1


def _load(path):
    try: return json.load(open(path))
    except (OSError, ValueError): return {}


def _save(path, c):
    try: json.dump(c, open(path, "w"), indent=2); return 0
    except OSError as e:
        print(f"airc-config-set-error: {e}", file=sys.stderr); return 1


def cmd_set(args) -> int:
    c = _load(args.config); c[args.key] = args.value; return _save(args.config, c)


def cmd_unset_keys(args) -> int:
    c = _load(args.config)
    for k in args.keys: c.pop(k, None)
    return _save(args.config, c)


def cmd_read_parted(args) -> int:
    for r in _load(args.config).get("parted_rooms", []) or []: print(r)
    return 0


def cmd_record_parted(args) -> int:
    c = _load(args.config); p = list(c.get("parted_rooms", []) or [])
    if args.room not in p:
        p.append(args.room); c["parted_rooms"] = p; return _save(args.config, c)
    return 0


def cmd_clear_parted(args) -> int:
    c = _load(args.config); cur = c.get("parted_rooms", []) or []
    new = [r for r in cur if r != args.room]
    if new != cur:
        c["parted_rooms"] = new; return _save(args.config, c)
    return 0


# ── subscribed_channels (Phase 2B) ──────────────────────────────────────
#
# Replaces the per-scope `room_name` file + sidecar scopes. A single
# `airc connect` process now subscribes to N channels in one mesh; the
# config field is the source of truth for "which channels do I display?".
#
# The first element is the DEFAULT channel — the one cmd_send stamps on
# outbound messages when --channel isn't passed. Order matters.
#
# Migration: a one-shot bootstrap reads the legacy `room_name` file (if
# present) and writes it as the single-element subscribed_channels list,
# preserving behavior for users mid-rollover. After that the room_name
# file is no longer authoritative — config wins.

def cmd_read_channels(args) -> int:
    """Print subscribed channels, one per line. Empty output if none."""
    for ch in _load(args.config).get("subscribed_channels", []) or []:
        print(ch)
    return 0


def cmd_default_channel(args) -> int:
    """Print the default (first) subscribed channel. Empty if none."""
    chans = _load(args.config).get("subscribed_channels", []) or []
    if chans:
        print(chans[0])
    return 0


def cmd_subscribe(args) -> int:
    """Add args.channel to subscribed_channels (idempotent).
    --first promotes the channel to subscribed_channels[0] (becomes the
    default for outbound). Without --first, appended at the end.
    """
    c = _load(args.config); cur = list(c.get("subscribed_channels", []) or [])
    new = [ch for ch in cur if ch != args.channel]
    if args.first:
        new = [args.channel] + new
    else:
        new = new + [args.channel]
    if new != cur:
        c["subscribed_channels"] = new
        return _save(args.config, c)
    return 0


def cmd_unsubscribe(args) -> int:
    """Remove args.channel from subscribed_channels."""
    c = _load(args.config); cur = c.get("subscribed_channels", []) or []
    new = [ch for ch in cur if ch != args.channel]
    if new != cur:
        c["subscribed_channels"] = new
        return _save(args.config, c)
    return 0


def cmd_set_host_block(args) -> int:
    """Atomically write the post-handshake host_* fields into config.

    Replaces a fragile env-var-passed python heredoc that bit on MSYS
    Git Bash (continuum-b69f's catch 2026-04-27): MSYS translates env
    var values that look like Unix paths INTO the Windows-binary
    subprocess, so /Users/... silently became C:/Program Files/Git/...
    Argparse `--flags` are per-arg-predictable (callers can `//`-prefix
    individual values or use MSYS2_ARG_CONV_EXCL targeted-ly), and
    the python source is fixed bytes regardless of the values.
    """
    try:
        c = json.load(open(args.config))
    except (OSError, ValueError) as e:
        print(f"airc-config-set-error: cannot read {args.config}: {e}", file=sys.stderr)
        return 1
    c["host_airc_home"] = args.host_airc_home or ""
    c["host_name"] = args.host_name or ""
    try:
        c["host_port"] = int(args.host_port)
    except (TypeError, ValueError):
        c["host_port"] = 7547
    c["host_ssh_pub"] = args.host_ssh_pub or ""
    try:
        c["host_identity"] = json.loads(args.host_identity_json or "{}")
    except ValueError:
        c["host_identity"] = {}
    try:
        json.dump(c, open(args.config, "w"), indent=2)
        return 0
    except OSError as e:
        print(f"airc-config-set-error: cannot write {args.config}: {e}", file=sys.stderr)
        return 1


def _build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="airc_core.config")
    sub = p.add_subparsers(dest="cmd", required=True)

    g = sub.add_parser("get")
    g.add_argument("--config", required=True)
    g.add_argument("key")
    g.add_argument("default", nargs="?", default="")
    g.set_defaults(func=cmd_get)

    n = sub.add_parser("get_name")
    n.add_argument("--config", required=True)
    n.set_defaults(func=cmd_get_name)

    sn = sub.add_parser("set_name")
    sn.add_argument("--config", required=True)
    sn.add_argument("--name", required=True)
    sn.set_defaults(func=cmd_set_name)

    ss = sub.add_parser("set")
    ss.add_argument("--config", required=True)
    ss.add_argument("--key", required=True)
    ss.add_argument("--value", required=True)
    ss.set_defaults(func=cmd_set)

    us = sub.add_parser("unset_keys")
    us.add_argument("--config", required=True)
    us.add_argument("keys", nargs="+")
    us.set_defaults(func=cmd_unset_keys)

    rp = sub.add_parser("read_parted")
    rp.add_argument("--config", required=True)
    rp.set_defaults(func=cmd_read_parted)

    rcp = sub.add_parser("record_parted")
    rcp.add_argument("--config", required=True)
    rcp.add_argument("--room", required=True)
    rcp.set_defaults(func=cmd_record_parted)

    cp = sub.add_parser("clear_parted")
    cp.add_argument("--config", required=True)
    cp.add_argument("--room", required=True)
    cp.set_defaults(func=cmd_clear_parted)

    rc = sub.add_parser("read_channels")
    rc.add_argument("--config", required=True)
    rc.set_defaults(func=cmd_read_channels)

    dc = sub.add_parser("default_channel")
    dc.add_argument("--config", required=True)
    dc.set_defaults(func=cmd_default_channel)

    su = sub.add_parser("subscribe")
    su.add_argument("--config", required=True)
    su.add_argument("--channel", required=True)
    su.add_argument("--first", action="store_true",
                    help="promote channel to subscribed_channels[0] (becomes default)")
    su.set_defaults(func=cmd_subscribe)

    un = sub.add_parser("unsubscribe")
    un.add_argument("--config", required=True)
    un.add_argument("--channel", required=True)
    un.set_defaults(func=cmd_unsubscribe)

    s = sub.add_parser("set_host_block")
    s.add_argument("--config", required=True)
    s.add_argument("--host-airc-home", default="")
    s.add_argument("--host-name", default="")
    s.add_argument("--host-port", default="7547")
    s.add_argument("--host-ssh-pub", default="")
    s.add_argument("--host-identity-json", default="{}")
    s.set_defaults(func=cmd_set_host_block)

    return p


def _cli() -> int:
    args = _build_parser().parse_args()
    return args.func(args)


if __name__ == "__main__":
    sys.exit(_cli())
