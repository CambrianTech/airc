"""airc inbox cursor tests."""

from __future__ import annotations

import io
import json
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "lib"))

from airc_core import inbox  # noqa: E402


class InboxTests(unittest.TestCase):
    def _scope(self):
        tmp = tempfile.TemporaryDirectory()
        home = Path(tmp.name)
        return tmp, home, home / "cursor.json"

    def _line(self, sender: str, ts: str, msg: str) -> str:
        return json.dumps({"from": sender, "ts": ts, "msg": msg}) + "\n"

    def test_same_second_messages_are_not_dropped(self):
        tmp, home, cursor = self._scope()
        with tmp:
            log = home / "messages.jsonl"
            log.write_text(
                self._line("a", "2026-05-04T20:00:00Z", "one")
                + self._line("b", "2026-05-04T20:00:00Z", "two"),
                encoding="utf-8",
            )
            out = io.StringIO()
            with redirect_stdout(out):
                inbox.main(["read", "--home", str(home), "--cursor-file", str(cursor), "--since", "2026-05-04T19:59:59Z", "--count", "1"])
            self.assertIn("one", out.getvalue())
            out = io.StringIO()
            with redirect_stdout(out):
                inbox.main(["read", "--home", str(home), "--cursor-file", str(cursor), "--count", "1"])
            self.assertIn("two", out.getvalue())

    def test_empty_read_does_not_advance_cursor(self):
        tmp, home, cursor = self._scope()
        with tmp:
            log = home / "messages.jsonl"
            log.write_text("", encoding="utf-8")
            with redirect_stdout(io.StringIO()):
                inbox.main(["read", "--home", str(home), "--cursor-file", str(cursor), "--since", "2026-05-04T19:59:59Z"])
            self.assertFalse(cursor.exists())
            log.write_text(self._line("a", "2026-05-04T20:00:00Z", "late"), encoding="utf-8")
            out = io.StringIO()
            with redirect_stdout(out):
                inbox.main(["read", "--home", str(home), "--cursor-file", str(cursor), "--since", "2026-05-04T19:59:59Z"])
            self.assertIn("late", out.getvalue())

    def test_reset_sets_cursor_to_end(self):
        tmp, home, cursor = self._scope()
        with tmp:
            log = home / "messages.jsonl"
            log.write_text(self._line("a", "2026-05-04T20:00:00Z", "old"), encoding="utf-8")
            with redirect_stdout(io.StringIO()):
                inbox.main(["reset", "--home", str(home), "--cursor-file", str(cursor)])
            log.write_text(
                log.read_text(encoding="utf-8") + self._line("b", "2026-05-04T20:00:01Z", "new"),
                encoding="utf-8",
            )
            out = io.StringIO()
            with redirect_stdout(out):
                inbox.main(["read", "--home", str(home), "--cursor-file", str(cursor), "--count", "10"])
            text = out.getvalue()
            self.assertNotIn("old", text)
            self.assertIn("new", text)

    def test_exclude_self_uses_sender_fallback_only_without_client_id(self):
        tmp, home, cursor = self._scope()
        with tmp:
            log = home / "messages.jsonl"
            log.write_text(
                self._line("me", "2099-05-04T20:00:00Z", "legacy self")
                + self._line("me", "2099-05-04T20:00:01Z", "same-name peer")
                + self._line("peer", "2099-05-04T20:00:02Z", "visible"),
                encoding="utf-8",
            )
            out = io.StringIO()
            with redirect_stdout(out):
                inbox.main(["read", "--home", str(home), "--cursor-file", str(cursor), "--exclude-self", "--my-name", "me"])
            text = out.getvalue()
            self.assertNotIn("legacy self", text)
            self.assertNotIn("same-name peer", text)
            self.assertIn("visible", text)

            cursor.unlink()
            out = io.StringIO()
            with redirect_stdout(out):
                inbox.main([
                    "read",
                    "--home",
                    str(home),
                    "--cursor-file",
                    str(cursor),
                    "--exclude-self",
                    "--my-name",
                    "me",
                    "--client-id",
                    "self-client",
                ])
            text = out.getvalue()
            self.assertIn("legacy self", text)
            self.assertIn("same-name peer", text)
            self.assertIn("visible", text)


if __name__ == "__main__":
    unittest.main()
