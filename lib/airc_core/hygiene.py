"""Workspace hygiene policy and cleanup for multi-agent AIRC lanes.

This module is intentionally small and data-oriented so it can move to Rust
without changing the command contract. The policy file is JSON: serde-friendly,
dependency-free, and safe to commit when it contains no secrets.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Iterable


DEFAULT_POLICY_FILE = ".airc-policy.json"


@dataclass(frozen=True)
class HygienePolicy:
    workspace_root: str = "~/.airc-worktrees"
    report_paths: list[str] = field(default_factory=list)
    hooks: list[str] = field(default_factory=list)
    warn_free_gb: float = 50.0
    block_free_gb: float = 15.0
    clean_worktree_rust_targets: bool = True
    clean_worktree_node_modules: bool = True
    clean_main_rust_target: bool = False
    clean_docker_build_cache: bool = False


@dataclass(frozen=True)
class CleanupCandidate:
    kind: str
    path: str
    bytes: int


def _repo_root(start: str | None = None) -> Path:
    cwd = Path(start or os.getcwd()).resolve()
    try:
        out = subprocess.check_output(
            ["git", "rev-parse", "--show-toplevel"],
            cwd=str(cwd),
            stderr=subprocess.DEVNULL,
            text=True,
        ).strip()
        if out:
            return Path(out).resolve()
    except (OSError, subprocess.CalledProcessError):
        pass
    return cwd


def _policy_path(args: argparse.Namespace) -> Path:
    if args.policy:
        return Path(args.policy).expanduser().resolve()
    return _repo_root() / DEFAULT_POLICY_FILE


def load_policy(path: Path) -> HygienePolicy:
    if not path.exists():
        return HygienePolicy()
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        raise SystemExit(f"hygiene: cannot read policy {path}: {exc}") from exc
    allowed = set(HygienePolicy.__dataclass_fields__)
    unknown = sorted(set(data) - allowed)
    if unknown:
        raise SystemExit(
            f"hygiene: unknown policy key(s) in {path}: {', '.join(unknown)}"
        )
    return HygienePolicy(**data)


def write_policy(path: Path, policy: HygienePolicy) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(asdict(policy), indent=2, sort_keys=True) + "\n", encoding="utf-8")


def _path_size(path: Path) -> int:
    if not path.exists():
        return 0
    if path.is_file() or path.is_symlink():
        try:
            return path.lstat().st_size
        except OSError:
            return 0
    total = 0
    for root, dirs, files in os.walk(path, topdown=True):
        # Do not follow symlinked directory trees.
        dirs[:] = [d for d in dirs if not (Path(root) / d).is_symlink()]
        for name in files:
            p = Path(root) / name
            try:
                total += p.lstat().st_size
            except OSError:
                continue
    return total


def _gb(bytes_value: int) -> float:
    return bytes_value / (1024 ** 3)


def _fmt_size(bytes_value: int) -> str:
    value = float(bytes_value)
    for unit in ("B", "KiB", "MiB", "GiB", "TiB"):
        if value < 1024 or unit == "TiB":
            if unit == "B":
                return f"{int(value)} {unit}"
            return f"{value:.1f} {unit}"
        value /= 1024
    return f"{value:.1f} TiB"


def _find_dirs(root: Path, suffix_parts: tuple[str, ...]) -> Iterable[Path]:
    if not root.exists():
        return []
    suffix = os.sep.join(suffix_parts)
    matches: list[Path] = []
    for dirpath, dirnames, _filenames in os.walk(root):
        path = Path(dirpath)
        if str(path).endswith(suffix):
            matches.append(path)
            dirnames[:] = []
            continue
        if path.name in {".git", "dist", ".continuum"}:
            dirnames[:] = [d for d in dirnames if d not in {"target", "node_modules"}]
    return matches


def collect_candidates(policy: HygienePolicy, repo_root: Path) -> list[CleanupCandidate]:
    candidates: list[CleanupCandidate] = []
    workspace_root = Path(policy.workspace_root).expanduser()

    if policy.clean_worktree_rust_targets:
        for path in _find_dirs(workspace_root, ("src", "workers", "target")):
            candidates.append(CleanupCandidate("worktree-rust-target", str(path), _path_size(path)))

    if policy.clean_worktree_node_modules:
        for path in _find_dirs(workspace_root, ("src", "node_modules")):
            candidates.append(CleanupCandidate("worktree-node-modules", str(path), _path_size(path)))

    if policy.clean_main_rust_target:
        path = repo_root / "src" / "workers" / "target"
        if path.exists():
            candidates.append(CleanupCandidate("main-rust-target", str(path), _path_size(path)))

    return sorted(candidates, key=lambda c: c.bytes, reverse=True)


def _free_disk_gb(path: Path) -> float:
    usage = shutil.disk_usage(path if path.exists() else path.parent)
    return _gb(usage.free)


def _memory_available_gb() -> float | None:
    meminfo = Path("/proc/meminfo")
    if meminfo.exists():
        for line in meminfo.read_text(encoding="utf-8", errors="ignore").splitlines():
            if line.startswith("MemAvailable:"):
                parts = line.split()
                if len(parts) >= 2:
                    return int(parts[1]) / (1024 ** 2)
    try:
        out = subprocess.check_output(["vm_stat"], text=True, stderr=subprocess.DEVNULL)
    except (OSError, subprocess.CalledProcessError):
        return None
    page_size = 4096
    free_pages = 0
    for line in out.splitlines():
        if "page size of" in line:
            try:
                page_size = int(line.split("page size of", 1)[1].split("bytes", 1)[0].strip())
            except (IndexError, ValueError):
                page_size = 4096
        if line.startswith(("Pages free:", "Pages inactive:", "Pages speculative:")):
            try:
                free_pages += int(line.split(":", 1)[1].strip().rstrip("."))
            except (IndexError, ValueError):
                continue
    return _gb(free_pages * page_size) if free_pages else None


def resource_snapshot(policy: HygienePolicy) -> dict[str, object]:
    load = None
    try:
        load = os.getloadavg()[0]
    except (AttributeError, OSError):
        pass
    paths = []
    for raw in policy.report_paths:
        path = Path(raw).expanduser()
        if path.exists():
            paths.append({
                "path": str(path),
                "bytes": _path_size(path),
            })
    return {
        "free_disk_gb": _free_disk_gb(Path(policy.workspace_root).expanduser()),
        "cpu_load_1m": load,
        "memory_available_gb": _memory_available_gb(),
        "gpu": "hook-required",
        "paths": paths,
        "hooks_configured": len(policy.hooks),
    }


def _print_report(policy_path: Path, policy: HygienePolicy, candidates: list[CleanupCandidate]) -> None:
    snapshot = resource_snapshot(policy)
    free_gb = float(snapshot["free_disk_gb"])
    print("# airc hygiene report")
    print(f"policy: {policy_path}")
    print(f"workspace_root: {Path(policy.workspace_root).expanduser()}")
    print(f"free_disk: {free_gb:.1f} GiB")
    if snapshot["cpu_load_1m"] is not None:
        print(f"cpu_load_1m: {float(snapshot['cpu_load_1m']):.2f}")
    if snapshot["memory_available_gb"] is not None:
        print(f"memory_available: {float(snapshot['memory_available_gb']):.1f} GiB")
    print(f"gpu: {snapshot['gpu']}")
    print(f"hooks_configured: {snapshot['hooks_configured']}")
    if free_gb < policy.block_free_gb:
        print(f"status: BLOCK ({free_gb:.1f} GiB < {policy.block_free_gb:.1f} GiB)")
    elif free_gb < policy.warn_free_gb:
        print(f"status: WARN ({free_gb:.1f} GiB < {policy.warn_free_gb:.1f} GiB)")
    else:
        print("status: OK")
    print()
    if not candidates:
        print("No safe cleanup candidates found.")
    else:
        total = sum(c.bytes for c in candidates)
        print(f"safe_cleanup_candidates: {len(candidates)} ({_fmt_size(total)})")
        for candidate in candidates[:80]:
            print(f"- {candidate.kind}: {_fmt_size(candidate.bytes)}  {candidate.path}")
        if len(candidates) > 80:
            print(f"... {len(candidates) - 80} more")
    if policy.report_paths:
        print()
        print("reported_paths:")
        for item in snapshot["paths"]:
            print(f"- {_fmt_size(int(item['bytes']))}  {item['path']}")


def cmd_init(args: argparse.Namespace) -> int:
    path = _policy_path(args)
    if path.exists() and not args.force:
        print(f"hygiene: policy already exists: {path}", file=sys.stderr)
        print("use --force to overwrite", file=sys.stderr)
        return 1
    write_policy(path, HygienePolicy())
    print(f"Wrote hygiene policy: {path}")
    return 0


def cmd_report(args: argparse.Namespace) -> int:
    path = _policy_path(args)
    policy = load_policy(path)
    candidates = collect_candidates(policy, _repo_root())
    if args.json:
        snapshot = resource_snapshot(policy)
        print(json.dumps({
            "policy": str(path),
            **snapshot,
            "candidates": [asdict(c) for c in candidates],
        }, indent=2, sort_keys=True))
    else:
        _print_report(path, policy, candidates)
    return 0


def cmd_clean(args: argparse.Namespace) -> int:
    path = _policy_path(args)
    policy = load_policy(path)
    candidates = collect_candidates(policy, _repo_root())
    if not candidates:
        print("No safe cleanup candidates found.")
        return 0
    if not args.yes and not args.dry_run:
        print("hygiene clean: refusing to delete without --yes or --dry-run", file=sys.stderr)
        return 1
    total = sum(c.bytes for c in candidates)
    verb = "would remove" if args.dry_run else "removing"
    print(f"{verb} {len(candidates)} safe cleanup candidate(s), {_fmt_size(total)}")
    for candidate in candidates:
        print(f"- {candidate.kind}: {_fmt_size(candidate.bytes)}  {candidate.path}")
        if not args.dry_run:
            shutil.rmtree(candidate.path, ignore_errors=True)
    if policy.clean_docker_build_cache:
        if args.dry_run:
            print("- docker: would run docker system prune -af")
        else:
            subprocess.run(["docker", "system", "prune", "-af"], check=False)
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="airc_core.hygiene")
    parser.add_argument("--policy", default="", help="policy path (default: repo .airc-policy.json)")
    sub = parser.add_subparsers(dest="cmd", required=True)

    init = sub.add_parser("init", help="write default project hygiene policy")
    init.add_argument("--force", action="store_true")
    init.set_defaults(func=cmd_init)

    report = sub.add_parser("report", help="show cleanup candidates")
    report.add_argument("--json", action="store_true")
    report.set_defaults(func=cmd_report)

    clean = sub.add_parser("clean", help="remove safe rebuildable caches")
    clean.add_argument("--dry-run", action="store_true")
    clean.add_argument("--yes", action="store_true")
    clean.set_defaults(func=cmd_clean)

    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
