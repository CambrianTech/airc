"""Codex config installer helpers for airc.

Shell scripts call this module instead of embedding config-editing Python.
It preserves user hooks and only manages the airc-owned hook entry.
"""

from __future__ import annotations

import argparse
import json
import os
import re
from pathlib import Path


AIRC_HOOK_COMMAND = "airc codex-hook user-prompt-submit"
AIRC_HOOK_STATUS = "Checking AIRC inbox"
AIRC_INSTRUCTIONS_START = "# AIRC-CODEX-INSTRUCTIONS-START"
AIRC_INSTRUCTIONS_END = "# AIRC-CODEX-INSTRUCTIONS-END"


def _read_text(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8")
    except OSError:
        return ""


def _write_text(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def _feature_enabled(text: str, key: str) -> bool:
    pattern = re.compile(rf"^[ \t]*{re.escape(key)}[ \t]*=[ \t]*true(?:[ \t]*(?:#.*)?)?$")
    return any(pattern.match(line) for line in text.splitlines())


def _remove_legacy_codex_hooks_feature_text(text: str) -> tuple[str, bool]:
    lines = text.splitlines()
    out: list[str] = []
    changed = False
    idx = 0
    while idx < len(lines):
        line = lines[idx]
        if line.strip() == "# AIRC-CODEX-HOOKS-FEATURE-START":
            block: list[str] = []
            idx += 1
            while idx < len(lines) and lines[idx].strip() != "# AIRC-CODEX-HOOKS-FEATURE-END":
                block.append(lines[idx])
                idx += 1
            if idx < len(lines):
                idx += 1
            if any(_feature_enabled(row, "codex_hooks") for row in block):
                changed = True
                continue
            out.append("# AIRC-CODEX-HOOKS-FEATURE-START")
            out.extend(block)
            out.append("# AIRC-CODEX-HOOKS-FEATURE-END")
            continue
        if line.strip() == "# AIRC-CODEX-HOOKS-FEATURE":
            idx += 1
            if idx < len(lines) and _feature_enabled(lines[idx], "codex_hooks"):
                idx += 1
                changed = True
            else:
                out.append(line)
            continue
        if _feature_enabled(line, "codex_hooks"):
            changed = True
            idx += 1
            continue
        out.append(line)
        idx += 1
    new_text = "\n".join(out)
    if text.endswith("\n") and new_text:
        new_text += "\n"
    return new_text, changed


def _set_hooks_feature(config: Path) -> bool:
    text = _read_text(config)
    text, removed_legacy = _remove_legacy_codex_hooks_feature_text(text)
    if _feature_enabled(text, "hooks"):
        if removed_legacy:
            _write_text(config, text)
        return removed_legacy
    lines = text.splitlines()
    for idx, line in enumerate(lines):
        if line.strip() == "[features]":
            lines.insert(idx + 1, "# AIRC-CODEX-HOOKS-FEATURE")
            lines.insert(idx + 2, "hooks = true")
            _write_text(config, "\n".join(lines) + "\n")
            return True

    block = "\n# AIRC-CODEX-HOOKS-FEATURE-START\n[features]\nhooks = true\n# AIRC-CODEX-HOOKS-FEATURE-END\n"
    suffix = "" if text.endswith("\n") or not text else "\n"
    _write_text(config, text + suffix + block)
    return True


def _remove_hooks_feature(config: Path) -> bool:
    text = _read_text(config)
    if not text:
        return False
    original = text
    lines = text.splitlines()
    out: list[str] = []
    idx = 0
    while idx < len(lines):
        line = lines[idx]
        if line.strip() == "# AIRC-CODEX-HOOKS-FEATURE-START":
            idx += 1
            while idx < len(lines) and lines[idx].strip() != "# AIRC-CODEX-HOOKS-FEATURE-END":
                idx += 1
            idx += 1
            continue
        if line.strip() == "# AIRC-CODEX-HOOKS-FEATURE":
            idx += 1
            if idx < len(lines) and (
                _feature_enabled(lines[idx], "hooks") or _feature_enabled(lines[idx], "codex_hooks")
            ):
                idx += 1
            continue
        out.append(line)
        idx += 1
    new_text = "\n".join(out).rstrip() + ("\n" if out else "")
    if new_text != original:
        _write_text(config, new_text)
        return True
    return False


def _remove_managed_developer_instructions(config: Path) -> bool:
    text = _read_text(config)
    if AIRC_INSTRUCTIONS_START not in text:
        return False
    lines = text.splitlines()
    out: list[str] = []
    idx = 0
    while idx < len(lines):
        line = lines[idx]
        if line.startswith(AIRC_INSTRUCTIONS_START):
            idx += 1
            while idx < len(lines) and not lines[idx].startswith(AIRC_INSTRUCTIONS_END):
                idx += 1
            if idx < len(lines):
                idx += 1
            while idx < len(lines) and lines[idx] == "":
                idx += 1
            continue
        out.append(line)
        idx += 1
    _write_text(config, "\n".join(out).rstrip() + "\n")
    return True


def _load_hooks(path: Path) -> dict:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {"hooks": {}}
    if not isinstance(data, dict):
        return {"hooks": {}}
    hooks = data.get("hooks")
    if not isinstance(hooks, dict):
        data["hooks"] = {}
    return data


def _hook_entry() -> dict:
    return {
        "hooks": [
            {
                "type": "command",
                "command": AIRC_HOOK_COMMAND,
                "timeout": 5,
                "statusMessage": AIRC_HOOK_STATUS,
            }
        ]
    }


def _install_hooks_json(path: Path) -> bool:
    data = _load_hooks(path)
    event = data.setdefault("hooks", {}).setdefault("UserPromptSubmit", [])
    if not isinstance(event, list):
        data["hooks"]["UserPromptSubmit"] = []
        event = data["hooks"]["UserPromptSubmit"]

    for group in event:
        if not isinstance(group, dict):
            continue
        for hook in group.get("hooks", []):
            if isinstance(hook, dict) and hook.get("command") == AIRC_HOOK_COMMAND:
                return False

    event.append(_hook_entry())
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return True


def _uninstall_hooks_json(path: Path) -> bool:
    data = _load_hooks(path)
    hooks = data.get("hooks", {})
    event = hooks.get("UserPromptSubmit")
    if not isinstance(event, list):
        return False
    changed = False
    new_event = []
    for group in event:
        if not isinstance(group, dict):
            new_event.append(group)
            continue
        group_hooks = group.get("hooks")
        if not isinstance(group_hooks, list):
            new_event.append(group)
            continue
        kept = [h for h in group_hooks if not (isinstance(h, dict) and h.get("command") == AIRC_HOOK_COMMAND)]
        if len(kept) != len(group_hooks):
            changed = True
        if kept:
            new_group = dict(group)
            new_group["hooks"] = kept
            new_event.append(new_group)
    if not changed:
        return False
    if new_event:
        hooks["UserPromptSubmit"] = new_event
    else:
        hooks.pop("UserPromptSubmit", None)
    path.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return True


def cmd_install(args: argparse.Namespace) -> int:
    codex_home = Path(args.codex_home).expanduser()
    config = codex_home / "config.toml"
    hooks_json = codex_home / "hooks.json"
    changed_feature = _set_hooks_feature(config)
    changed_hook = _install_hooks_json(hooks_json)
    removed_instructions = _remove_managed_developer_instructions(config)
    if changed_feature:
        print(f"enabled hooks in {config}")
    if changed_hook:
        print(f"installed AIRC UserPromptSubmit hook in {hooks_json}")
    if removed_instructions:
        print(f"removed legacy AIRC Codex polling instructions from {config}")
    return 0


def cmd_uninstall(args: argparse.Namespace) -> int:
    codex_home = Path(args.codex_home).expanduser()
    config = codex_home / "config.toml"
    hooks_json = codex_home / "hooks.json"
    if _remove_hooks_feature(config):
        print(f"removed airc-managed hooks feature from {config}")
    if _uninstall_hooks_json(hooks_json):
        print(f"removed AIRC UserPromptSubmit hook from {hooks_json}")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="airc_core.codex_install")
    parser.add_argument("--codex-home", default=os.path.join(os.path.expanduser("~"), ".codex"))
    sub = parser.add_subparsers(dest="cmd", required=True)
    sub.add_parser("install-hooks")
    sub.add_parser("uninstall-hooks")
    args = parser.parse_args(argv)
    if args.cmd == "install-hooks":
        return cmd_install(args)
    if args.cmd == "uninstall-hooks":
        return cmd_uninstall(args)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
