"""airc messages.jsonl rotation.

Without rotation, the host's log + every joiner's local mirror grows
forever. Joel asked 2026-04-28: "is messages.log ever getting trimmed
down? it could over time consume the editor/reader if it isn't draining
or cycling." Answer was no. This module fixes that.

Strategy: trim in place. When the file exceeds `max_lines`, atomically
replace it with the last `keep_lines` lines. No archive — chat history
is conversational, not a permanent record; `airc logs N` only ever
shows the tail anyway.

Atomicity: write trimmed content to a sibling tempfile, then `os.rename`.
On POSIX (mac/linux/wsl) rename is atomic; the file the SSH-tail is
following is briefly replaced and tail's `-F` flag detects + reopens.
On Windows (Git Bash + msys), `os.rename` over an existing file fails;
fall back to a remove-then-rename that has a tiny race window. Acceptable
trade-off — we lose at most one line if a writer hits the gap.

CLI:
  python -m airc_core.log rotate --path /path/to/messages.jsonl
                                 [--max-lines 5000] [--keep-lines 2500]

Returns 0 on success or no-op (file under threshold). Non-zero on error.
"""

from __future__ import annotations

import argparse
import os
import sys
import tempfile

DEFAULT_MAX_LINES = 5000
DEFAULT_KEEP_LINES = 2500


def rotate_if_needed(path: str, max_lines: int, keep_lines: int) -> str:
    """Trim `path` to last `keep_lines` lines if it exceeds `max_lines`.
    Returns:
      'noop'    — file under threshold or missing
      'rotated' — file rotated successfully
      'error'   — I/O failure (caller decides whether to surface)
    """
    if max_lines <= keep_lines:
        # Caller bug — keep_lines must leave headroom or we'd rotate
        # on every append. Fail loud rather than silently.
        return "error"
    if not os.path.isfile(path):
        return "noop"
    try:
        with open(path, "rb") as f:
            lines = f.readlines()
    except OSError:
        return "error"
    if len(lines) <= max_lines:
        return "noop"

    # Keep the tail. Rebuild the file via an atomic rename.
    tail = lines[-keep_lines:]
    parent = os.path.dirname(path) or "."
    fd, tmp = tempfile.mkstemp(prefix=".airc-log.", dir=parent)
    try:
        with os.fdopen(fd, "wb") as f:
            f.writelines(tail)
        try:
            os.rename(tmp, path)
        except OSError:
            # Windows / cross-volume fallback: remove then rename.
            # Tiny race window — concurrent writers may lose one line.
            try:
                os.remove(path)
                os.rename(tmp, path)
            except OSError:
                # Couldn't replace. Drop the tempfile and report.
                try: os.unlink(tmp)
                except OSError: pass
                return "error"
    except OSError:
        try: os.unlink(tmp)
        except OSError: pass
        return "error"

    return "rotated"


def cmd_rotate(args) -> int:
    result = rotate_if_needed(args.path, args.max_lines, args.keep_lines)
    if result == "error":
        print(f"airc-log-rotate: error rotating {args.path}", file=sys.stderr)
        return 1
    if result == "rotated":
        print(f"airc-log-rotate: trimmed {args.path} to last {args.keep_lines} lines")
    return 0


def _build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="airc_core.log")
    sub = p.add_subparsers(dest="cmd", required=True)

    r = sub.add_parser("rotate")
    r.add_argument("--path", required=True)
    r.add_argument("--max-lines", type=int, default=DEFAULT_MAX_LINES)
    r.add_argument("--keep-lines", type=int, default=DEFAULT_KEEP_LINES)
    r.set_defaults(func=cmd_rotate)

    return p


def _cli() -> int:
    args = _build_parser().parse_args()
    return args.func(args)


if __name__ == "__main__":
    sys.exit(_cli())
