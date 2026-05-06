"""Helpers for draining pending.jsonl without shell-side JSON parsing."""

from __future__ import annotations

import argparse
import json
import sys


def _load_config(path: str) -> dict:
    try:
        with open(path, encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return {}


def _read_lines(path: str) -> list[str]:
    try:
        with open(path, encoding="utf-8") as f:
            return [line.rstrip("\n") for line in f if line.strip()]
    except OSError:
        return []


def cmd_host_broadcast_route(args: argparse.Namespace) -> int:
    lines = _read_lines(args.snapshot)
    if not lines:
        print("no\tempty")
        return 0

    channel = ""
    for line in lines:
        try:
            msg = json.loads(line)
        except ValueError:
            print("no\tmalformed")
            return 0
        if msg.get("to", "all") not in ("", "all"):
            print("no\tdm")
            return 0
        line_channel = str(msg.get("channel") or "")
        if not line_channel:
            print("no\tmissing-channel")
            return 0
        if channel and line_channel != channel:
            print("no\tmixed-channel")
            return 0
        channel = line_channel

    config = _load_config(args.config)
    gist = str((config.get("channel_gists") or {}).get(channel) or "")
    if not gist:
        gist = args.fallback_gist or ""
    if not gist:
        print("no\tmissing-gist")
        return 0

    print(f"ok\t{channel}\t{gist}\t{len(lines)}")
    return 0


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="airc_core.pending_batch")
    sub = parser.add_subparsers(dest="cmd", required=True)

    route = sub.add_parser("host-broadcast-route")
    route.add_argument("--snapshot", required=True)
    route.add_argument("--config", required=True)
    route.add_argument("--fallback-gist", default="")
    route.set_defaults(func=cmd_host_broadcast_route)
    return parser


def _main(argv: list[str] | None = None) -> int:
    args = _build_parser().parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(_main())
