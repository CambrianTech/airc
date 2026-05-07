"""Repair local AIRC scope state without network access."""

from __future__ import annotations

import argparse
import json
import os
import re
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path


GIST_RE = re.compile(r"^[0-9a-f]{32}$")
GH_GET_RE = re.compile(r"_gh_api_get\(([0-9a-f]{32})\)")


def _read(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8").strip()
    except OSError:
        return ""


def _write_json(path: Path, data: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f"{path.name}.tmp.{os.getpid()}")
    tmp.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    os.replace(tmp, path)


def _channels_from_bearer_state(home: Path) -> list[str]:
    channels = []
    for path in sorted(home.glob("bearer_state.*.json")):
        channel = path.name.removeprefix("bearer_state.").removesuffix(".json")
        if channel and channel not in channels:
            channels.append(channel)
    return channels


def _gist_from_bearer_log(home: Path, channel: str) -> str:
    path = home / f"bearer_recv.{channel}.log"
    try:
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()[-200:]
    except OSError:
        return ""
    for line in reversed(lines):
        match = GH_GET_RE.search(line)
        if match:
            return match.group(1)
    return ""


def _gone_channel_gist(home: Path, channel: str) -> str:
    gid = _read(home / f"gone_channel_gist.{channel}")
    return gid if GIST_RE.match(gid) else ""


def _name_from_messages(home: Path) -> str:
    try:
        lines = (home / "messages.jsonl").read_text(encoding="utf-8").splitlines()[-500:]
    except OSError:
        return ""
    counts: Counter[str] = Counter()
    for raw in lines:
        try:
            msg = json.loads(raw)
        except json.JSONDecodeError:
            continue
        sender = msg.get("from")
        if isinstance(sender, str) and sender and sender not in {"airc", "unknown"}:
            counts[sender] += 1
    return counts.most_common(1)[0][0] if counts else ""


def _name_from_ssh_comment(home: Path) -> str:
    parts = _read(home / "identity" / "ssh_key.pub").split()
    if len(parts) >= 3 and parts[2].startswith("airc-"):
        return parts[2]
    return ""


def infer_config(home: Path, default_name: str, host: str, existing: dict | None = None) -> dict:
    existing = existing or {}
    room_name = _read(home / "room_name")
    room_gist = _read(home / "room_gist_id")
    host_gist = _read(home / "host_gist_id")

    channels = list(existing.get("subscribed_channels", []) or [])
    parted = set(existing.get("parted_rooms", []) or [])
    for channel in _channels_from_bearer_state(home):
        if channel not in channels and channel not in parted:
            channels.append(channel)
    if room_name and room_name not in channels:
        channels.append(room_name)
    if "cambriantech" in channels:
        channels = ["cambriantech"] + [ch for ch in channels if ch != "cambriantech"]
    elif room_name in channels:
        channels = [room_name] + [ch for ch in channels if ch != room_name]

    channel_gists: dict[str, str] = dict(existing.get("channel_gists", {}) or {})
    if room_name and GIST_RE.match(room_gist) and _gone_channel_gist(home, room_name) != room_gist:
        channel_gists[room_name] = room_gist
    for channel in channels:
        gone_gist = _gone_channel_gist(home, channel)
        if gone_gist:
            if channel_gists.get(channel) == gone_gist:
                channel_gists.pop(channel, None)
            continue
        log_gist = _gist_from_bearer_log(home, channel)
        if GIST_RE.match(log_gist):
            channel_gists[channel] = log_gist
        elif channel not in channel_gists and channel != "general" and GIST_RE.match(host_gist):
            channel_gists[channel] = host_gist

    data = {
        **existing,
        "created": existing.get("created") or datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        "host": existing.get("host") or host,
        "name": existing.get("name") or _name_from_messages(home) or _name_from_ssh_comment(home) or default_name or "airc",
    }
    if channels:
        data["subscribed_channels"] = channels
    if channel_gists:
        data["channel_gists"] = channel_gists
    return data


def cmd_repair_config(args: argparse.Namespace) -> int:
    home = Path(args.home).expanduser()
    config = Path(args.config).expanduser()
    existing: dict = {}
    if config.exists():
        try:
            existing = json.loads(config.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            existing = {}
    if not home.exists():
        return 1
    has_state = any((home / p).exists() for p in ("identity", "messages.jsonl", "room_gist_id", "host_gist_id"))
    has_state = has_state or any(home.glob("bearer_state.*.json"))
    if not has_state:
        return 1
    repaired = infer_config(home, args.default_name, args.host, existing)
    if repaired == existing and config.exists():
        return 0
    _write_json(config, repaired)
    if existing:
        print(f"repaired incomplete config: {config}")
    else:
        print(f"repaired missing config: {config}")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="airc_core.scope_repair")
    sub = parser.add_subparsers(dest="cmd", required=True)
    repair = sub.add_parser("repair-config")
    repair.add_argument("--home", required=True)
    repair.add_argument("--config", required=True)
    repair.add_argument("--default-name", default="")
    repair.add_argument("--host", default="")
    args = parser.parse_args(argv)
    if args.cmd == "repair-config":
        return cmd_repair_config(args)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
