"""Codex hook adapter tests."""

from __future__ import annotations

import io
import json
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest.mock import patch

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "lib"))

from airc_core import codex_hook, codex_install  # noqa: E402


class CodexHookTests(unittest.TestCase):
    def _scope(self):
        tmp = tempfile.TemporaryDirectory()
        home = Path(tmp.name)
        return tmp, home, home / "inbox_cursor"

    def _line(self, sender: str, ts: str, msg: str, client_id: str = "") -> str:
        data = {"from": sender, "ts": ts, "msg": msg}
        if client_id:
            data["client_id"] = client_id
        return json.dumps(data) + "\n"

    def test_user_prompt_hook_emits_additional_context_for_unread_peer_messages(self):
        tmp, home, cursor = self._scope()
        with tmp:
            (home / "messages.jsonl").write_text(
                self._line("me", "2099-05-04T20:00:00Z", "self", "self-client")
                + self._line("peer", "2099-05-04T20:00:01Z", "hello", "peer-client"),
                encoding="utf-8",
            )
            out = io.StringIO()
            with patch("sys.stdin", io.StringIO('{"hook_event_name":"UserPromptSubmit"}')):
                with redirect_stdout(out):
                    rc = codex_hook.main(
                        [
                            "user-prompt-submit",
                            "--home",
                            str(home),
                            "--cursor-file",
                            str(cursor),
                            "--my-name",
                            "me",
                            "--client-id",
                            "self-client",
                        ]
                    )
            self.assertEqual(rc, 0)
            payload = json.loads(out.getvalue())
            context = payload["hookSpecificOutput"]["additionalContext"]
            self.assertIn("AIRC: 1 unread", context)
            self.assertIn("peer: hello", context)
            self.assertNotIn("me: self", context)
            self.assertTrue(cursor.exists())

    def test_user_prompt_hook_dedupes_and_limits_digest(self):
        tmp, home, cursor = self._scope()
        with tmp:
            (home / "messages.jsonl").write_text(
                self._line("peer", "2099-05-04T20:00:01Z", "duplicate", "peer-client")
                + self._line("peer", "2099-05-04T20:00:02Z", "duplicate", "peer-client")
                + self._line("peer", "2099-05-04T20:00:03Z", "newest", "peer-client"),
                encoding="utf-8",
            )
            out = io.StringIO()
            with patch("sys.stdin", io.StringIO("{}")):
                with redirect_stdout(out):
                    rc = codex_hook.main(
                        [
                            "user-prompt-submit",
                            "--home",
                            str(home),
                            "--cursor-file",
                            str(cursor),
                            "--client-id",
                            "self-client",
                            "--max-items",
                            "1",
                        ]
                    )
            self.assertEqual(rc, 0)
            context = json.loads(out.getvalue())["hookSpecificOutput"]["additionalContext"]
            self.assertIn("AIRC: 2 unread", context)
            self.assertIn("latest 1 shown", context)
            self.assertIn("newest", context)
            self.assertNotIn("duplicate", context)
            self.assertIn("more: airc inbox --peek --count 50", context)

    def test_user_prompt_hook_is_silent_when_empty(self):
        tmp, home, cursor = self._scope()
        with tmp:
            (home / "messages.jsonl").write_text("", encoding="utf-8")
            out = io.StringIO()
            with patch("sys.stdin", io.StringIO("{}")):
                with redirect_stdout(out):
                    rc = codex_hook.main(
                        [
                            "user-prompt-submit",
                            "--home",
                            str(home),
                            "--cursor-file",
                            str(cursor),
                            "--client-id",
                            "self-client",
                        ]
                    )
            self.assertEqual(rc, 0)
            self.assertEqual(out.getvalue(), "")

    def test_user_prompt_hook_filters_own_rows_when_client_id_missing(self):
        tmp, home, cursor = self._scope()
        with tmp:
            (home / "messages.jsonl").write_text(
                self._line("me", "2099-05-04T20:00:00Z", "own fallback")
                + self._line("peer", "2099-05-04T20:00:01Z", "peer visible", "peer-client"),
                encoding="utf-8",
            )
            out = io.StringIO()
            with patch("sys.stdin", io.StringIO("{}")):
                with redirect_stdout(out):
                    rc = codex_hook.main(
                        [
                            "user-prompt-submit",
                            "--home",
                            str(home),
                            "--cursor-file",
                            str(cursor),
                            "--my-name",
                            "me",
                            "--client-id",
                            "",
                        ]
                    )
            self.assertEqual(rc, 0)
            context = json.loads(out.getvalue())["hookSpecificOutput"]["additionalContext"]
            self.assertIn("peer: peer visible", context)
            self.assertNotIn("me: own fallback", context)

    def test_codex_hook_installer_preserves_existing_hooks(self):
        tmp = tempfile.TemporaryDirectory()
        with tmp:
            codex_home = Path(tmp.name)
            (codex_home / "config.toml").write_text("[features]\nother = true\n", encoding="utf-8")
            (codex_home / "hooks.json").write_text(
                json.dumps(
                    {
                        "hooks": {
                            "UserPromptSubmit": [
                                {
                                    "hooks": [
                                        {
                                            "type": "command",
                                            "command": "echo existing",
                                        }
                                    ]
                                }
                            ]
                        }
                    }
                ),
                encoding="utf-8",
            )
            with redirect_stdout(io.StringIO()):
                codex_install.main(["--codex-home", str(codex_home), "install-hooks"])
            config = (codex_home / "config.toml").read_text(encoding="utf-8")
            hooks = json.loads((codex_home / "hooks.json").read_text(encoding="utf-8"))
            commands = [
                hook["command"]
                for group in hooks["hooks"]["UserPromptSubmit"]
                for hook in group["hooks"]
                if "command" in hook
            ]
            self.assertIn("other = true", config)
            self.assertIn("hooks = true", config)
            self.assertIn("echo existing", commands)
            self.assertIn(codex_install.AIRC_HOOK_COMMAND, commands)

    def test_codex_hook_installer_appends_new_features_table(self):
        tmp = tempfile.TemporaryDirectory()
        with tmp:
            codex_home = Path(tmp.name)
            (codex_home / "config.toml").write_text(
                'default_permissions = "airc"\n[profiles.default]\nmodel = "gpt-5"\n',
                encoding="utf-8",
            )
            with redirect_stdout(io.StringIO()):
                codex_install.main(["--codex-home", str(codex_home), "install-hooks"])
            config = (codex_home / "config.toml").read_text(encoding="utf-8")
            self.assertRegex(config, r'default_permissions = "airc"\n\[profiles\.default\]')
            self.assertRegex(config, r'\[features\]\nhooks = true')

    def test_codex_hook_installer_migrates_deprecated_codex_hooks_feature(self):
        tmp = tempfile.TemporaryDirectory()
        with tmp:
            codex_home = Path(tmp.name)
            (codex_home / "config.toml").write_text(
                "# AIRC-CODEX-HOOKS-FEATURE-START\n"
                "[features]\n"
                "codex_hooks = true\n"
                "# AIRC-CODEX-HOOKS-FEATURE-END\n",
                encoding="utf-8",
            )
            with redirect_stdout(io.StringIO()):
                codex_install.main(["--codex-home", str(codex_home), "install-hooks"])
            config = (codex_home / "config.toml").read_text(encoding="utf-8")
            self.assertIn("hooks = true", config)
            self.assertNotIn("codex_hooks", config)

    def test_codex_hook_installer_removes_legacy_managed_polling_instructions(self):
        tmp = tempfile.TemporaryDirectory()
        with tmp:
            codex_home = Path(tmp.name)
            (codex_home / "config.toml").write_text(
                '# AIRC-CODEX-INSTRUCTIONS-START — managed by install.sh\n'
                'developer_instructions = """run airc codex-poll"""\n'
                '# AIRC-CODEX-INSTRUCTIONS-END\n\n'
                'default_permissions = "airc"\n',
                encoding="utf-8",
            )
            with redirect_stdout(io.StringIO()):
                codex_install.main(["--codex-home", str(codex_home), "install-hooks"])
            config = (codex_home / "config.toml").read_text(encoding="utf-8")
            self.assertNotIn("AIRC-CODEX-INSTRUCTIONS", config)
            self.assertNotIn("developer_instructions", config)
            self.assertIn('default_permissions = "airc"', config)


if __name__ == "__main__":
    unittest.main()
