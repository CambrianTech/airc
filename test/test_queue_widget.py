"""Tests for static AIRC queue widget helpers (airc#570)."""

from __future__ import annotations

import json
import pathlib
import subprocess
import textwrap
import unittest


REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
WIDGET = REPO_ROOT / "widgets" / "queue-board.js"
ROOM_WIDGET = REPO_ROOT / "widgets" / "room-directory.js"


def run_node(source: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["node", "-e", source],
        cwd=str(REPO_ROOT),
        capture_output=True,
        text=True,
        timeout=10,
    )


class QueueWidgetTests(unittest.TestCase):
    def test_parse_group_and_normalize(self) -> None:
        issue = {
            "number": 570,
            "title": "airc-queue: Build widget",
            "html_url": "https://github.com/owner/repo/issues/570",
            "updated_at": "2026-05-13T22:00:00Z",
            "body": textwrap.dedent(
                """
                **airc-queue card**

                ```json
                {
                  "kind": "airc-queue-card-v1",
                  "id": "airc#570",
                  "status": "in-progress",
                  "branch": "feat/widget",
                  "owner": "codex",
                  "next_action": "ship parser"
                }
                ```
                """
            ),
        }
        script = f"""
        const widget = require({json.dumps(str(WIDGET))});
        const item = widget.normalizeIssue({json.dumps(issue)});
        const groups = widget.groupCards([item]);
        console.log(JSON.stringify({{
          id: item.card.id,
          status: item.card.status,
          count: groups["in-progress"].length,
          other: groups.other.length
        }}));
        """
        result = run_node(script)
        self.assertEqual(result.returncode, 0, result.stderr)
        payload = json.loads(result.stdout)
        self.assertEqual(payload["id"], "airc#570")
        self.assertEqual(payload["status"], "in-progress")
        self.assertEqual(payload["count"], 1)
        self.assertEqual(payload["other"], 0)

    def test_non_queue_issue_is_ignored(self) -> None:
        script = f"""
        const widget = require({json.dumps(str(WIDGET))});
        console.log(widget.normalizeIssue({{ body: "plain issue" }}) === null ? "null" : "bad");
        """
        result = run_node(script)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "null")

    def test_room_directory_normalizes_config(self) -> None:
        config = {
            "title": "Project Rooms",
            "rooms": [
                {
                    "name": "#cambriantech",
                    "scope": "CambrianTech",
                    "description": "Coordination",
                    "visibility": "approved",
                    "joinHint": "airc join",
                },
                {"description": "missing name"},
            ],
        }
        script = f"""
        const rooms = require({json.dumps(str(ROOM_WIDGET))});
        const directory = rooms.normalizeDirectory({json.dumps(config)});
        console.log(JSON.stringify({{
          title: directory.title,
          count: directory.rooms.length,
          name: directory.rooms[0].name,
          visibility: directory.rooms[0].visibility
        }}));
        """
        result = run_node(script)
        self.assertEqual(result.returncode, 0, result.stderr)
        payload = json.loads(result.stdout)
        self.assertEqual(payload["title"], "Project Rooms")
        self.assertEqual(payload["count"], 1)
        self.assertEqual(payload["name"], "#cambriantech")
        self.assertEqual(payload["visibility"], "approved")


if __name__ == "__main__":
    unittest.main()
