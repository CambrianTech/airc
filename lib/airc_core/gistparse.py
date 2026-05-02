"""airc gist-envelope + address-array parser. Replaces jq dependency.

Two clusters of jq usage existed (issue #188):

1. **Address filtering** (airc top-level host_address_set): the gist's
   `host.addresses` is a JSON list of `{scope, addr, port, [subnet]}`
   records. We pick the best entry per scope (localhost / lan /
   tailscale) for the joiner's connection-attempt order.

2. **Envelope field extraction** (cmd_connect.sh discovery + parse):
   read the gist content JSON to extract `.airc`, `.kind`, `.invite`,
   `.channels[0]`, `.host.machine_id`, `.host.addresses`, `.last_heartbeat`,
   etc.

Both go through this one Python module via subcommands. Bash callers
pipe the JSON in on stdin and pass a path/selector via flags.

Path syntax (subset of jq):
  .foo            → nested object key
  .foo.bar        → multi-level
  .foo[0]         → first array element
  .foo[0].bar     → mixed
  .[0]            → top-level first element

CLI:
  python -m airc_core.gistparse get [--default ""] <path>          (stdin → value or default)
  python -m airc_core.gistparse get_json <path>                    (stdin → compact JSON)
  python -m airc_core.gistparse pick_addr <scope>                  (stdin = host.addresses list)
  python -m airc_core.gistparse pick_addr_first                    (stdin = host.addresses list, .[0])
  python -m airc_core.gistparse list_lan_entries                   (stdin = host.addresses list)

Each subcommand returns "" + exit 0 on missing field — bash callers
read stdout into a local var. Errors (malformed JSON, etc.) also
exit 0 with empty output unless --strict is passed; jq's behavior was
quiet-and-empty for malformed input and we preserve that.
"""

from __future__ import annotations

import argparse
import json
import re
import sys


_PATH_TOKEN = re.compile(r"""
    \.                       # literal dot
    (?:
        (?P<key>[A-Za-z_][A-Za-z0-9_-]*)
        (?:\[(?P<idx>\d+)\])?
        |
        \[(?P<top_idx>\d+)\]   # leading .[0]
    )
""", re.VERBOSE)


def _navigate(data, path: str):
    """Walk `data` per a dotted/indexed path. Returns None on miss."""
    if not path or path == ".":
        return data
    pos = 0
    while pos < len(path):
        m = _PATH_TOKEN.match(path, pos)
        if not m:
            return None
        if m.group("top_idx") is not None:
            idx = int(m.group("top_idx"))
            if not isinstance(data, list) or idx >= len(data):
                return None
            data = data[idx]
        else:
            key = m.group("key")
            if not isinstance(data, dict):
                return None
            data = data.get(key)
            if m.group("idx") is not None and data is not None:
                idx = int(m.group("idx"))
                if not isinstance(data, list) or idx >= len(data):
                    return None
                data = data[idx]
            if data is None:
                return None
        pos = m.end()
    return data


def _read_stdin_json():
    raw = sys.stdin.read()
    if not raw.strip():
        return None
    try:
        return json.loads(raw)
    except (ValueError, TypeError):
        return None


def _emit(value, default=""):
    """Print value the way jq -r would: string scalars unquoted, dict/list
    as compact JSON, missing → default."""
    if value is None:
        print(default)
    elif isinstance(value, (dict, list)):
        print(json.dumps(value, separators=(",", ":")))
    elif isinstance(value, bool):
        print("true" if value else "false")
    else:
        print(value)


def cmd_get(args) -> int:
    data = _read_stdin_json()
    if data is None:
        print(args.default)
        return 0
    _emit(_navigate(data, args.path), default=args.default)
    return 0


def cmd_get_json(args) -> int:
    """Same as get but always emits compact JSON (or empty string on miss)."""
    data = _read_stdin_json()
    if data is None:
        print("")
        return 0
    v = _navigate(data, args.path)
    if v is None:
        print("")
    else:
        print(json.dumps(v, separators=(",", ":")))
    return 0


def cmd_pick_addr(args) -> int:
    """Stdin is a list of {scope, addr, port, ...}. Print 'addr|port' for
    the FIRST entry whose scope matches args.scope. Empty if no match."""
    data = _read_stdin_json()
    if not isinstance(data, list):
        return 0
    for entry in data:
        if not isinstance(entry, dict):
            continue
        if entry.get("scope") == args.scope:
            addr = entry.get("addr", "")
            port = entry.get("port", "")
            if addr and port != "":
                print(f"{addr}|{port}")
                return 0
    return 0


def cmd_pick_addr_first(args) -> int:
    """Stdin is a list of {scope, addr, port, ...}. Print 'addr|port' for
    the FIRST entry. Empty if list is empty."""
    data = _read_stdin_json()
    if isinstance(data, list) and data and isinstance(data[0], dict):
        addr = data[0].get("addr", "")
        port = data[0].get("port", "")
        if addr and port != "":
            print(f"{addr}|{port}")
    return 0


def cmd_pick_addr_nonlocal_first(args) -> int:
    """Stdin is a list of {scope, addr, port, ...}. Print 'addr|port' for
    the first entry whose scope is NOT 'localhost'. Empty if all entries
    are localhost (or list is empty / malformed).

    Why this exists: peer_pick_address's bash-side fallback was "first
    entry of any kind" — but the gist's host.addresses[] often has
    `localhost` first (127.0.0.1, the host's loopback). For a different
    machine's joiner, picking that means dialing their OWN loopback,
    which never reaches the host. Symptom: Joel's Windows peer subscribed
    to #cambriantech but stuck on a 127.0.0.1 connection because their
    Windows IP didn't match the host's lan/24 subnet check. With this
    helper, the fallback skips localhost entries; if only localhost
    remains, returns empty so the caller falls through to gh-bearer-only
    routing instead of dialing an unreachable address.

    Superseded by `pick_addr_excluding` (#395) for joiner-side
    reachability — kept for backward compat in case external callers
    rely on the name.
    """
    data = _read_stdin_json()
    if not isinstance(data, list):
        return 0
    for entry in data:
        if not isinstance(entry, dict):
            continue
        scope = entry.get("scope", "")
        if scope == "localhost":
            continue
        addr = entry.get("addr", "")
        port = entry.get("port", "")
        if addr and port != "":
            print(f"{addr}|{port}")
            return 0
    return 0


def cmd_pick_addr_excluding(args) -> int:
    """Stdin is a list of {scope, addr, port, ...}. Print 'addr|port' for
    the first entry whose scope is NOT in args.exclude_scopes. Empty if
    every entry is excluded (or list is empty / malformed).

    Why this exists: pick_addr_nonlocal_first hardcoded localhost as the
    only excludable scope, but joiner-side reachability detection needs
    to skip multiple scopes at once. Concrete case: a Mac without
    Tailscale joining a Windows host whose addresses[] is
    [localhost, tailscale]. The Mac can reach NEITHER. With the
    nonlocal_first helper it would pick tailscale (first non-localhost),
    fail to connect (no 100.x route), and trigger destructive self-heal
    — demolishing the room gist that was working fine for everyone
    else. With this helper, the joiner declares its unreachable scopes
    upfront (e.g. `pick_addr_excluding localhost tailscale`), gets
    empty back, and the caller falls through to gh-bearer-only routing.
    """
    excluded = set(args.exclude_scopes)
    data = _read_stdin_json()
    if not isinstance(data, list):
        return 0
    for entry in data:
        if not isinstance(entry, dict):
            continue
        scope = entry.get("scope", "")
        if scope in excluded:
            continue
        addr = entry.get("addr", "")
        port = entry.get("port", "")
        if addr and port != "":
            print(f"{addr}|{port}")
            return 0
    return 0


def cmd_gist_content(args) -> int:
    """Stdin is a gh-api response for a gist (`gh api gists/<id>` or the
    REST equivalent). Extract the first file's `.content`. Replaces:
        gh api gists/$id | jq -r '.files | to_entries[0].value.content // empty'
    """
    data = _read_stdin_json()
    if not isinstance(data, dict):
        return 0
    files = data.get("files")
    if not isinstance(files, dict) or not files:
        return 0
    # Files dict: { "<filename>": {"filename":..., "content":...}, ... }
    first_key = next(iter(files))
    entry = files[first_key]
    if isinstance(entry, dict):
        content = entry.get("content", "")
        if content:
            print(content)
    return 0


def cmd_get_first_of(args) -> int:
    """Try multiple paths in order; print the first non-null value's
    string form. Replaces jq's `.a // .b // empty` fallback chain."""
    data = _read_stdin_json()
    if data is None:
        print(args.default)
        return 0
    for path in args.paths:
        v = _navigate(data, path)
        if v is not None:
            _emit(v, default=args.default)
            return 0
    print(args.default)
    return 0


def cmd_list_lan_entries(args) -> int:
    """Stdin is a list of {scope, addr, port, subnet, ...}. Print each
    LAN entry as compact JSON, one per line. Used by host_address_set
    where we need to iterate every LAN addr (multi-NIC machines)."""
    data = _read_stdin_json()
    if not isinstance(data, list):
        return 0
    for entry in data:
        if isinstance(entry, dict) and entry.get("scope") == "lan":
            print(json.dumps(entry, separators=(",", ":")))
    return 0


def _build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="airc_core.gistparse")
    sub = p.add_subparsers(dest="cmd", required=True)

    g = sub.add_parser("get")
    g.add_argument("path")
    g.add_argument("--default", default="")
    g.set_defaults(func=cmd_get)

    gj = sub.add_parser("get_json")
    gj.add_argument("path")
    gj.set_defaults(func=cmd_get_json)

    pa = sub.add_parser("pick_addr")
    pa.add_argument("scope")
    pa.set_defaults(func=cmd_pick_addr)

    pf = sub.add_parser("pick_addr_first")
    pf.set_defaults(func=cmd_pick_addr_first)

    pnf = sub.add_parser("pick_addr_nonlocal_first")
    pnf.set_defaults(func=cmd_pick_addr_nonlocal_first)

    pe = sub.add_parser("pick_addr_excluding")
    pe.add_argument("exclude_scopes", nargs="+",
                    help="Scope names to skip (e.g. localhost tailscale)")
    pe.set_defaults(func=cmd_pick_addr_excluding)

    ll = sub.add_parser("list_lan_entries")
    ll.set_defaults(func=cmd_list_lan_entries)

    gc = sub.add_parser("gist_content")
    gc.set_defaults(func=cmd_gist_content)

    gfo = sub.add_parser("get_first_of")
    gfo.add_argument("paths", nargs="+")
    gfo.add_argument("--default", default="")
    gfo.set_defaults(func=cmd_get_first_of)

    return p


def _cli() -> int:
    args = _build_parser().parse_args()
    return args.func(args)


if __name__ == "__main__":
    sys.exit(_cli())
