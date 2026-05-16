"""Machine-readable rendering for AIRC message logs."""

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import asdict, dataclass
from datetime import datetime, timedelta, timezone
from typing import Iterable


@dataclass(frozen=True)
class LogEvent:
    id: str
    ts: str
    sender: str
    recipient: str
    channel: str
    msg: str
    client_id: str
    raw: dict


def parse_since(value: str) -> datetime | None:
    if not value:
        return None
    match = re.fullmatch(r"(\d+)([smhd])", value)
    if match:
        amount = int(match.group(1))
        unit = match.group(2)
        delta = {
            "s": timedelta(seconds=amount),
            "m": timedelta(minutes=amount),
            "h": timedelta(hours=amount),
            "d": timedelta(days=amount),
        }[unit]
        return datetime.now(timezone.utc) - delta
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as exc:
        raise ValueError(
            f"cannot parse '{value}' (use ISO timestamp or 60s/5m/1h/2d)"
        ) from exc
    if parsed.tzinfo is None:
        return parsed.replace(tzinfo=timezone.utc)
    return parsed


def _parse_ts(value: str) -> datetime | None:
    if not value:
        return None
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    if parsed.tzinfo is None:
        return parsed.replace(tzinfo=timezone.utc)
    return parsed


def events_from_lines(lines: Iterable[str], since: datetime | None = None) -> list[LogEvent]:
    events: list[LogEvent] = []
    for line in lines:
        try:
            obj = json.loads(line.strip())
        except Exception:
            continue
        if not isinstance(obj, dict):
            continue
        ts = obj.get("ts", "")
        if not isinstance(ts, str):
            ts = ""
        if since is not None:
            event_dt = _parse_ts(ts)
            if event_dt is None or event_dt <= since:
                continue
        sender = obj.get("from", "?")
        recipient = obj.get("to", "")
        channel = obj.get("channel", "")
        msg = obj.get("msg", "")
        client_id = obj.get("client_id", obj.get("clientId", ""))
        sig = obj.get("sig", obj.get("id", ""))
        events.append(
            LogEvent(
                id=sig if isinstance(sig, str) else "",
                ts=ts,
                sender=sender if isinstance(sender, str) else "?",
                recipient=recipient if isinstance(recipient, str) else "",
                channel=channel if isinstance(channel, str) else "",
                msg=msg if isinstance(msg, str) else str(msg),
                client_id=client_id if isinstance(client_id, str) else "",
                raw=obj,
            )
        )
    return events


def render_human(events: Iterable[LogEvent]) -> str:
    return "".join(f"[{event.ts}] {event.sender}: {event.msg}\n" for event in events)


def render_json(events: list[LogEvent], since_arg: str, count: int) -> str:
    payload = {
        "now_utc": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        "since": since_arg,
        "count": count,
        "events": [asdict(event) for event in events],
    }
    return json.dumps(payload, indent=2) + "\n"


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="airc_core.logs")
    sub = parser.add_subparsers(dest="cmd", required=True)
    render = sub.add_parser("render")
    render.add_argument("--since", default="")
    render.add_argument("--count", type=int, required=True)
    render.add_argument("--json", action="store_true")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = _build_parser().parse_args(argv)
    try:
        since = parse_since(args.since)
    except ValueError as exc:
        print(f"airc logs --since: {exc}", file=sys.stderr)
        return 2
    events = events_from_lines(sys.stdin, since)
    if args.json:
        sys.stdout.write(render_json(events, args.since, args.count))
    else:
        sys.stdout.write(render_human(events))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
