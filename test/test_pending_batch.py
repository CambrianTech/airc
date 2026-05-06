"""pending.jsonl batch classification tests."""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent


class PendingBatchTests(unittest.TestCase):
    def run_cli(self, snapshot: Path, config: Path, fallback: str = "") -> str:
        result = subprocess.run(
            [
                sys.executable,
                "-m",
                "airc_core.pending_batch",
                "host-broadcast-route",
                "--snapshot",
                str(snapshot),
                "--config",
                str(config),
                "--fallback-gist",
                fallback,
            ],
            cwd=str(REPO_ROOT),
            env={"PYTHONPATH": str(REPO_ROOT / "lib")},
            capture_output=True,
            text=True,
            check=True,
        )
        return result.stdout.strip()

    def test_broadcast_batch_returns_single_route(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot = root / "pending.jsonl"
            config = root / "config.json"
            snapshot.write_text(
                '{"to":"all","channel":"general","msg":"one"}\n'
                '{"channel":"general","msg":"two"}\n',
                encoding="utf-8",
            )
            config.write_text(json.dumps({"channel_gists": {"general": "abc123"}}), encoding="utf-8")

            self.assertEqual(self.run_cli(snapshot, config), "ok\tgeneral\tabc123\t2")

    def test_dm_is_not_batched(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot = root / "pending.jsonl"
            config = root / "config.json"
            snapshot.write_text('{"to":"alice","channel":"general","msg":"secret"}\n', encoding="utf-8")
            config.write_text(json.dumps({"channel_gists": {"general": "abc123"}}), encoding="utf-8")

            self.assertEqual(self.run_cli(snapshot, config), "no\tdm")


if __name__ == "__main__":
    unittest.main()
