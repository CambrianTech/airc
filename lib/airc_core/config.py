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
