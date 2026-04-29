"""channel_gist tests — convergence on duplicate gists.

Run: cd test && python3 test_channel_gist.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path
from unittest import mock

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "lib"))

from airc_core import channel_gist  # noqa: E402


class FindExistingConvergenceTests(unittest.TestCase):
    """When two+ gists describe the same channel (host-takeover races,
    accidental dup creates), all peers must converge on ONE gist.
    Pre-fix find_existing returned whichever happened to appear first
    in gh's list-response, which is recency-ordered → different peers
    saw different "first" → substrate split silently.

    Convergence rule: oldest-by-created_at wins. Deterministic across
    every peer on the gh account regardless of when they polled."""

    def _gist(self, gid, channel, created_at, desc=None):
        return {
            "id": gid,
            "description": desc or f"airc room: #{channel} (post-3c per-channel gist)",
            "created_at": created_at,
            "files": {
                f"airc-room-{channel}.json": {
                    "content": '{"airc": 1, "kind": "mesh", "channels": ["%s"]}' % channel,
                    "truncated": False,
                },
            },
        }

    def test_returns_oldest_when_two_canonical_dups(self):
        """Two gists describe #general with identical canonical shape;
        find_existing must return the OLDEST regardless of list order."""
        # gh list-response is recency-ordered: NEWER first.
        listing = [
            self._gist("newer-id", "general", "2026-04-29T15:00:00Z"),
            self._gist("older-id", "general", "2026-04-29T07:00:00Z"),
        ]
        with mock.patch.object(channel_gist, "_gh_list_user_gists", return_value=listing):
            chosen = channel_gist.find_existing("general")
        self.assertEqual(chosen, "older-id",
                         "must converge on the OLDEST duplicate, not newest")

    def test_returns_oldest_across_three_dups(self):
        listing = [
            self._gist("middle", "general", "2026-04-29T10:00:00Z"),
            self._gist("newest", "general", "2026-04-29T15:00:00Z"),
            self._gist("oldest", "general", "2026-04-29T05:00:00Z"),
        ]
        with mock.patch.object(channel_gist, "_gh_list_user_gists", return_value=listing):
            chosen = channel_gist.find_existing("general")
        self.assertEqual(chosen, "oldest")

    def test_canonical_wins_over_legacy_even_when_legacy_is_older(self):
        """#290 contract preserved: canonical single-channel gists take
        priority over legacy multi-channel mesh gists. Even if the
        legacy mesh is OLDER, the canonical wins (oldest among
        canonicals). This tiebreak avoids re-introducing the split
        between [#general] and [a, b, c, general] gists."""
        legacy_old = {
            "id": "legacy-old",
            "description": "airc mesh",
            "created_at": "2026-04-29T01:00:00Z",
            "files": {
                "airc-room-mesh.json": {
                    "content": '{"airc": 1, "kind": "mesh", "channels": ["a", "b", "general"]}',
                    "truncated": False,
                },
            },
        }
        canonical_newer = self._gist("canonical-new", "general", "2026-04-29T08:00:00Z")
        with mock.patch.object(channel_gist, "_gh_list_user_gists",
                               return_value=[legacy_old, canonical_newer]):
            chosen = channel_gist.find_existing("general")
        self.assertEqual(chosen, "canonical-new",
                         "canonical (single-channel) priority overrides legacy oldest")

    def test_returns_none_when_no_match(self):
        with mock.patch.object(channel_gist, "_gh_list_user_gists", return_value=[]):
            self.assertIsNone(channel_gist.find_existing("nonexistent"))

    def test_returns_oldest_legacy_when_no_canonical(self):
        """If only legacy mesh gists exist (none canonical), still
        converge on oldest among them."""
        m_old = {
            "id": "mesh-old",
            "description": "airc mesh",
            "created_at": "2026-04-29T05:00:00Z",
            "files": {"airc-room-mesh.json": {
                "content": '{"airc":1,"kind":"mesh","channels":["a","general","c"]}',
                "truncated": False,
            }},
        }
        m_new = {
            "id": "mesh-new",
            "description": "airc mesh",
            "created_at": "2026-04-29T15:00:00Z",
            "files": {"airc-room-mesh.json": {
                "content": '{"airc":1,"kind":"mesh","channels":["general","x"]}',
                "truncated": False,
            }},
        }
        with mock.patch.object(channel_gist, "_gh_list_user_gists",
                               return_value=[m_new, m_old]):
            chosen = channel_gist.find_existing("general")
        self.assertEqual(chosen, "mesh-old")


if __name__ == "__main__":
    unittest.main()
