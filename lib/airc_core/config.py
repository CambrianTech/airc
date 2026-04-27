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

    return p


def _cli() -> int:
    args = _build_parser().parse_args()
    return args.func(args)


if __name__ == "__main__":
    sys.exit(_cli())
