"""airc config.json CRUD.

Migrated from bash get_config_val / get_name (45+ callsites) into the
Python truth-layer (#152 Phase 1).

Pre-migration each callsite was an inline `"$AIRC_PYTHON" -c "import
json; print(json.load(open('$CONFIG')).get('$1','$2'))"` heredoc with
bash-variable substitution INTO the python source. If the bash $1
contained quotes, special chars, or empty, the python source could
break in subtle ways and silently return the default. Continuum-b69f
2026-04-27 traced one symptom (host_target reading empty even when
config.json had it) to this class.

Post-migration: config path comes from `CONFIG` env var, key/default
come from argv. Python source is fixed bytes; bash never touches it.

CLI shape (matches bash callsite expectations):

    CONFIG=/path/to/config.json python -m airc_core.config get <key> [default]
    CONFIG=/path/to/config.json python -m airc_core.config get_name

`get_name` is a special case because the bash one threw on missing key
(used `['name']` not `.get('name', ...)`). The CLI mirrors the
existing contract — prints "unknown" on failure to match the bash
fallback.
"""

from __future__ import annotations

import json
import os
import sys


def get(config_path: str, key: str, default: str = "") -> str:
    """Read a key from config.json. Returns default on any failure.
    Nested objects (dicts/lists) round-trip as JSON-encoded strings so
    callers can re-parse if needed (matches handshake.get_field shape).
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
    """Read 'name' field; returns 'unknown' on failure (matches bash)."""
    return get(config_path, "name", "unknown")


def _cli() -> int:
    cfg = os.environ.get("CONFIG", "")
    if not cfg:
        print("ERROR: CONFIG env var must point at config.json", file=sys.stderr)
        return 2
    if len(sys.argv) < 2:
        return 2
    cmd = sys.argv[1]
    if cmd == "get":
        if len(sys.argv) < 3:
            return 2
        key = sys.argv[2]
        default = sys.argv[3] if len(sys.argv) > 3 else ""
        print(get(cfg, key, default))
        return 0
    if cmd == "get_name":
        print(get_name(cfg))
        return 0
    print(f"unknown subcommand: {cmd}", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(_cli())
