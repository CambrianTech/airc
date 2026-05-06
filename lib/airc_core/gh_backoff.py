"""Shared GitHub API backoff + request budget state for AIRC transports."""

from __future__ import annotations

import argparse
import contextlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path


DEFAULT_MAX_REQUESTS_PER_MIN = 30
LOCAL_THROTTLE_BACKOFF_SEC = 60


def _uid() -> str:
    return str(os.getuid()) if hasattr(os, "getuid") else os.environ.get("USERNAME", "user")


def backoff_path() -> str:
    return os.path.join(tempfile.gettempdir(), f"airc-gh-backoff-until-{_uid()}")


def audit_path() -> str:
    return os.environ.get(
        "AIRC_GH_AUDIT_LOG",
        os.path.join(tempfile.gettempdir(), f"airc-gh-requests-{_uid()}.jsonl"),
    )


def budget_path() -> str:
    return os.path.join(tempfile.gettempdir(), f"airc-gh-budget-{_uid()}.jsonl")


def lock_path() -> str:
    return os.path.join(tempfile.gettempdir(), f"airc-gh-guard-{_uid()}.lock")


@contextlib.contextmanager
def _guard_lock():
    """Cross-process best-effort lock for budget check+reserve.

    The request budget is per user across all AIRC tabs on a machine.
    Without a lock, ten agents can all read "29/30" concurrently and
    burst through the guard together. Unix uses fcntl; Windows uses
    msvcrt; if neither is available, the unlocked path is still better
    than no guard but tests cover the normal platforms.
    """
    path = lock_path()
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "a+", encoding="utf-8") as f:
        locked = False
        try:
            f.seek(0)
            if os.name == "nt":
                import msvcrt  # type: ignore

                msvcrt.locking(f.fileno(), msvcrt.LK_LOCK, 1)
                locked = True
            else:
                import fcntl  # type: ignore

                fcntl.flock(f.fileno(), fcntl.LOCK_EX)
                locked = True
        except Exception:
            locked = False
        try:
            yield
        finally:
            if locked:
                try:
                    if os.name == "nt":
                        import msvcrt  # type: ignore

                        f.seek(0)
                        msvcrt.locking(f.fileno(), msvcrt.LK_UNLCK, 1)
                    else:
                        import fcntl  # type: ignore

                        fcntl.flock(f.fileno(), fcntl.LOCK_UN)
                except Exception:
                    pass


def backoff_until() -> float:
    try:
        with open(backoff_path(), encoding="utf-8") as f:
            return float(f.read().strip() or "0")
    except (OSError, ValueError):
        return 0.0


def backoff_active() -> bool:
    return time.time() < backoff_until()


def _write_backoff(until: float) -> None:
    if until <= time.time():
        return
    path = backoff_path()
    until = max(until, backoff_until())
    tmp = f"{path}.{os.getpid()}.tmp"
    try:
        with open(tmp, "w", encoding="utf-8") as f:
            f.write(str(int(until)))
        os.replace(tmp, path)
    except OSError:
        try:
            os.unlink(tmp)
        except OSError:
            pass


def record_backoff(output: str) -> None:
    """Record a shared GitHub backoff window from headers/body."""
    body = (output or "").lower()
    if not body:
        return
    now = time.time()
    until = 0.0
    retry = re.search(r"^retry-after:\s*(\d+)\s*$", body, re.MULTILINE)
    if retry:
        until = now + max(1, int(retry.group(1)))
    else:
        remaining = re.search(r"^x-ratelimit-remaining:\s*(\d+)\s*$", body, re.MULTILINE)
        reset = re.search(r"^x-ratelimit-reset:\s*(\d+)\s*$", body, re.MULTILINE)
        if remaining and reset and remaining.group(1) == "0":
            until = float(reset.group(1))
        elif (
            "secondary rate limit" in body
            or "rate limit exceeded" in body
            or "abuse detection" in body
        ):
            until = now + 60.0
    if until <= now:
        return
    _write_backoff(until)


def _guarded_command(args: list[str]) -> bool:
    if not args:
        return False
    if args[0] in {"api", "gist"}:
        return True
    return len(args) >= 2 and args[0] == "auth" and args[1] == "status"


def _command_class(args: list[str]) -> str:
    if not args:
        return "unknown"
    if args[0] == "api":
        for part in args[1:]:
            if part.startswith("-"):
                continue
            return f"api:{part.split('?', 1)[0]}"
        return "api"
    if args[0] == "gist" and len(args) >= 2:
        return f"gist:{args[1]}"
    if args[0] == "auth" and len(args) >= 2:
        return f"auth:{args[1]}"
    return args[0]


def _safe_args(args: list[str]) -> list[str]:
    out: list[str] = []
    redact_next = False
    for arg in args:
        if redact_next:
            out.append("<redacted>")
            redact_next = False
            continue
        if arg in {"--input", "-F", "--field", "-f", "--raw-field"}:
            out.append(arg)
            if arg != "--input":
                redact_next = True
            continue
        if "token" in arg.lower() or "authorization:" in arg.lower():
            out.append("<redacted>")
        else:
            out.append(arg[:180])
    return out


def _append_audit(event: dict) -> None:
    path = Path(audit_path())
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        if path.exists() and path.stat().st_size > int(os.environ.get("AIRC_GH_AUDIT_MAX_BYTES", "262144")):
            rotated = path.with_suffix(path.suffix + ".1")
            try:
                rotated.unlink()
            except OSError:
                pass
            try:
                path.replace(rotated)
            except OSError:
                pass
        with path.open("a", encoding="utf-8") as f:
            f.write(json.dumps(event, sort_keys=True) + "\n")
    except OSError:
        pass


def _recent_request_count(now: float, window_sec: int = 60) -> int:
    path = budget_path()
    cutoff = now - window_sec
    kept: list[float] = []
    try:
        with open(path, encoding="utf-8") as f:
            for line in f:
                try:
                    ts = float(line.strip())
                except ValueError:
                    continue
                if ts >= cutoff:
                    kept.append(ts)
    except OSError:
        pass
    tmp = f"{path}.{os.getpid()}.tmp"
    try:
        with open(tmp, "w", encoding="utf-8") as f:
            for ts in kept:
                f.write(f"{ts:.3f}\n")
        os.replace(tmp, path)
    except OSError:
        try:
            os.unlink(tmp)
        except OSError:
            pass
    return len(kept)


def _record_request(now: float) -> None:
    try:
        with open(budget_path(), "a", encoding="utf-8") as f:
            f.write(f"{now:.3f}\n")
    except OSError:
        pass


def max_requests_per_min() -> int:
    raw = os.environ.get("AIRC_GH_MAX_REQUESTS_PER_MIN", str(DEFAULT_MAX_REQUESTS_PER_MIN))
    try:
        value = int(raw)
    except (TypeError, ValueError):
        return DEFAULT_MAX_REQUESTS_PER_MIN
    return max(1, value)


def guard_decision(args: list[str], now: float | None = None) -> tuple[bool, str]:
    """Return (allowed, reason), recording shared backoff when blocked."""
    if os.environ.get("AIRC_GH_GUARD_DISABLE") == "1" or not _guarded_command(args):
        return True, "unguarded"
    now = time.time() if now is None else now
    until = backoff_until()
    if now < until:
        return False, f"shared backoff active for {int(until - now)}s"
    count = _recent_request_count(now)
    limit = max_requests_per_min()
    if count >= limit:
        _write_backoff(now + LOCAL_THROTTLE_BACKOFF_SEC)
        return False, f"local request budget exceeded ({count}/{limit} in 60s)"
    return True, "allowed"


def _reserve_guarded_request(args: list[str], now: float) -> tuple[bool, str]:
    with _guard_lock():
        allowed, reason = guard_decision(args, now=now)
        if allowed and _guarded_command(args) and reason != "unguarded":
            _record_request(now)
        return allowed, reason


def run_gh(
    gh: str,
    args: list[str],
    *,
    input: str | bytes | None = None,
    capture_output: bool = True,
    text: bool = True,
    timeout: float | None = None,
    **kwargs,
) -> subprocess.CompletedProcess:
    """Run gh under the shared AIRC request governor.

    This preserves subprocess.run's common CompletedProcess contract for
    Python call sites while adding a per-user budget, shared backoff, and
    JSONL audit trail. Shell call sites use the CLI entrypoint below.
    """
    now = time.time()
    if os.environ.get("AIRC_GH_GUARD_DISABLE") == "1" or not _guarded_command(args):
        allowed, reason = True, "unguarded"
    else:
        allowed, reason = _reserve_guarded_request(args, now)
    event = {
        "ts": int(now),
        "pid": os.getpid(),
        "cwd": os.getcwd(),
        "class": _command_class(args),
        "args": _safe_args(args),
        "allowed": allowed,
        "reason": reason,
        "backoff_until": int(backoff_until()),
    }
    if not allowed:
        msg = f"airc gh guard: {reason}; refusing gh {' '.join(_safe_args(args)[:3])}\n"
        event.update({"rc": 75, "outcome": "blocked"})
        _append_audit(event)
        return subprocess.CompletedProcess([gh, *args], 75, "" if text else b"", msg if text else msg.encode())

    try:
        result = subprocess.run(
            [gh, *args],
            input=input,
            capture_output=capture_output,
            text=text,
            timeout=timeout,
            **kwargs,
        )
    except subprocess.TimeoutExpired:
        event.update({"rc": "timeout", "outcome": "timeout"})
        _append_audit(event)
        raise
    except OSError as e:
        event.update({"rc": "oserror", "outcome": str(e)[:200]})
        _append_audit(event)
        raise

    stdout = result.stdout if isinstance(result.stdout, str) else ""
    stderr = result.stderr if isinstance(result.stderr, str) else ""
    combined = stderr + stdout
    if result.returncode != 0:
        record_backoff(combined)
    else:
        headers, _body = split_include_output(stdout)
        record_backoff(headers)
    event.update({
        "rc": result.returncode,
        "outcome": "ok" if result.returncode == 0 else "error",
        "backoff_until": int(backoff_until()),
    })
    _append_audit(event)
    return result


def split_include_output(raw: str) -> tuple[str, str]:
    """Return (headers, body) from `gh api --include` output."""
    text = raw or ""
    normalized = text.replace("\r\n", "\n")
    if normalized.startswith("HTTP/") and "\n\n" in normalized:
        headers, body = normalized.split("\n\n", 1)
        return headers, body
    return "", text


def _cmd_audit(args: argparse.Namespace) -> int:
    if args.reset:
        removed: list[str] = []
        for path in (backoff_path(), budget_path()):
            try:
                os.unlink(path)
                removed.append(path)
            except FileNotFoundError:
                pass
            except OSError as e:
                print(f"Could not remove {path}: {e}", file=sys.stderr)
                return 1
        print("AIRC gh guard reset: cleared shared backoff/budget state.")
        if removed:
            for path in removed:
                print(f"  removed {path}")
        print(f"  audit log retained: {audit_path()}")
        return 0

    if args.clear_audit:
        try:
            os.unlink(audit_path())
            print(f"AIRC gh audit cleared: {audit_path()}")
        except FileNotFoundError:
            print(f"AIRC gh audit already empty: {audit_path()}")
        except OSError as e:
            print(f"Could not clear audit log {audit_path()}: {e}", file=sys.stderr)
            return 1
        return 0

    path = Path(audit_path())
    if not path.exists():
        print(f"No AIRC gh audit log yet: {path}")
        return 0
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError as e:
        print(f"Could not read AIRC gh audit log {path}: {e}", file=sys.stderr)
        return 1

    rows: list[dict] = []
    for line in lines[-max(args.count * 4, args.count):]:
        try:
            event = json.loads(line)
        except (ValueError, TypeError):
            continue
        if isinstance(event, dict):
            rows.append(event)
    rows = rows[-args.count:]

    if args.summary:
        counts: dict[str, int] = {}
        blocked = 0
        for event in rows:
            cls = str(event.get("class") or "unknown")
            counts[cls] = counts.get(cls, 0) + 1
            if not event.get("allowed", True):
                blocked += 1
        print(f"AIRC gh audit: {path}")
        print(f"recent events: {len(rows)}; blocked: {blocked}; shared backoff: {max(0, int(backoff_until() - time.time()))}s")
        for cls, count in sorted(counts.items(), key=lambda item: (-item[1], item[0])):
            print(f"  {count:4d}  {cls}")
        return 0

    print(f"AIRC gh audit: {path}")
    for event in rows:
        ts = event.get("ts", "?")
        cls = event.get("class", "unknown")
        allowed = "ok" if event.get("allowed", True) else "BLOCKED"
        rc = event.get("rc", "")
        reason = event.get("reason", "")
        cwd = event.get("cwd", "")
        print(f"{ts} {allowed:7s} rc={rc!s:>7s} {cls} — {reason} — {cwd}")
    return 0


def _main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="airc_core.gh_backoff")
    sub = parser.add_subparsers(dest="cmd", required=True)
    run = sub.add_parser("run")
    run.add_argument("gh_args", nargs=argparse.REMAINDER)
    audit = sub.add_parser("audit")
    audit.add_argument("--count", type=int, default=50)
    audit.add_argument("--summary", action="store_true")
    audit.add_argument("--reset", action="store_true", help="clear shared backoff/budget state; keep audit log")
    audit.add_argument("--clear-audit", action="store_true", help="delete the local gh audit log")
    args = parser.parse_args(argv)
    if args.cmd == "audit":
        return _cmd_audit(args)
    if args.cmd != "run":
        return 2

    gh_args = list(args.gh_args)
    if gh_args and gh_args[0] == "--":
        gh_args = gh_args[1:]
    gh = shutil.which("gh") or shutil.which("gh.exe")
    if not gh:
        print("airc gh guard: gh CLI not found on PATH", file=sys.stderr)
        return 127

    # Interactive auth flows must keep stdio attached. They are not the
    # spammy background paths, so log lightly and let gh own the terminal.
    if len(gh_args) >= 2 and gh_args[0] == "auth" and gh_args[1] in {"login", "refresh"}:
        _append_audit({
            "ts": int(time.time()),
            "pid": os.getpid(),
            "cwd": os.getcwd(),
            "class": _command_class(gh_args),
            "args": _safe_args(gh_args),
            "allowed": True,
            "reason": "interactive-auth",
        })
        return subprocess.run([gh, *gh_args]).returncode

    result = run_gh(gh, gh_args, capture_output=True, text=True)
    if result.stdout:
        sys.stdout.write(result.stdout)
    if result.stderr:
        sys.stderr.write(result.stderr)
    return int(result.returncode)


if __name__ == "__main__":
    raise SystemExit(_main())
