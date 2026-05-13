"""Stateful unread polling for non-Monitor runtimes."""

from __future__ import annotations

import argparse
import json
import os
import sys
from datetime import datetime, timedelta, timezone
from typing import Optional


def _parse_since(value: str) -> Optional[datetime]:
    if not value:
        return None
    unit = value[-1:]
    number = value[:-1]
    if unit in "smhd" and number.isdigit():
        n = int(number)
        delta = {
            "s": timedelta(seconds=n),
            "m": timedelta(minutes=n),
            "h": timedelta(hours=n),
            "d": timedelta(days=n),
        }[unit]
        return datetime.now(timezone.utc) - delta
    try:
        dt = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        raise SystemExit(f"airc inbox --since: cannot parse '{value}'")
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    return dt


def _msg_dt(line: dict) -> Optional[datetime]:
    raw = line.get("ts", "")
    if not isinstance(raw, str) or not raw:
        return None
    try:
        dt = datetime.fromisoformat(raw.replace("Z", "+00:00"))
    except ValueError:
        return None
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    return dt


def _format(line: dict) -> str:
    return f"[{line.get('ts', '')}] {line.get('from', '?')}: {line.get('msg', '')}"


def _read_cursor(path: str) -> tuple[Optional[int], str]:
    try:
        with open(path, encoding="utf-8") as f:
            raw = f.read().strip()
    except OSError:
        return (None, "")
    if not raw:
        return (None, "")
    try:
        data = json.loads(raw)
    except json.JSONDecodeError:
        return (None, raw)
    offset = data.get("offset")
    if isinstance(offset, int) and offset >= 0:
        return (offset, "")
    return (None, "")


def _write_cursor(path: str, offset: int) -> None:
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    tmp = f"{path}.tmp.{os.getpid()}"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump({"offset": max(0, offset)}, f, separators=(",", ":"))
        f.write("\n")
    os.replace(tmp, path)


def cmd_reset(args: argparse.Namespace) -> int:
    log_path = os.path.join(args.home, "messages.jsonl")
    try:
        offset = os.path.getsize(log_path)
    except OSError:
        offset = 0
    _write_cursor(args.cursor_file, offset)
    print("airc inbox cursor reset.")
    return 0


def cmd_read(args: argparse.Namespace) -> int:
    log_path = os.path.join(args.home, "messages.jsonl")
    cursor_offset, legacy_since = _read_cursor(args.cursor_file)
    since_arg = args.since or ""
    if not since_arg and cursor_offset is None:
        since_arg = legacy_since or "5m"
    since_dt = _parse_since(since_arg) if since_arg else None

    try:
        size = os.path.getsize(log_path)
    except OSError:
        size = 0
    start_offset = 0
    if since_dt is None and cursor_offset is not None:
        start_offset = cursor_offset if cursor_offset <= size else 0

    printed = 0
    last_offset = start_offset
    try:
        with open(log_path, "rb") as f:
            f.seek(start_offset)
            while printed < args.count:
                raw = f.readline()
                if not raw:
                    break
                next_offset = f.tell()
                try:
                    line = json.loads(raw.decode("utf-8"))
                except Exception:
                    last_offset = next_offset
                    continue
                if args.exclude_self:
                    if args.client_id and line.get("client_id") == args.client_id:
                        last_offset = next_offset
                        continue
                    if not args.client_id and args.my_name and line.get("from") == args.my_name:
                        last_offset = next_offset
                        continue
                if since_dt is not None:
                    dt = _msg_dt(line)
                    if dt is None or dt <= since_dt:
                        last_offset = next_offset
                        continue
                print(_format(line))
                printed += 1
                last_offset = next_offset
    except OSError:
        pass

    if printed == 0 and not args.quiet_empty:
        print(f"No new airc messages since {since_arg or 'last inbox check'}")
    elif not args.peek:
        _write_cursor(args.cursor_file, last_offset)
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="airc_core.inbox")
    sub = parser.add_subparsers(dest="cmd", required=True)
    read = sub.add_parser("read")
    read.add_argument("--home", required=True)
    read.add_argument("--cursor-file", required=True)
    read.add_argument("--since", default="")
    read.add_argument("--count", type=int, default=500)
    read.add_argument("--peek", action="store_true")
    read.add_argument("--quiet-empty", action="store_true")
    read.add_argument("--exclude-self", action="store_true")
    read.add_argument("--my-name", default="")
    read.add_argument("--client-id", default="")
    reset = sub.add_parser("reset")
    reset.add_argument("--home", required=True)
    reset.add_argument("--cursor-file", required=True)
    args = parser.parse_args(argv)
    if args.cmd == "read":
        return cmd_read(args)
    if args.cmd == "reset":
        return cmd_reset(args)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
