"""gistparse tests — address-picker helpers used by peer_pick_address.

Covers the address-filtering subcommands that bash callers pipe
host.addresses[] JSON into. The picker chain is what decides whether
the joiner dials localhost / lan / tailscale, so getting it wrong has
real failure modes (loopback dials, destructive self-heal).

Run: cd test && python3 test_gistparse.py
"""

from __future__ import annotations

import io
import json
import sys
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest import mock

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "lib"))

from airc_core import gistparse  # noqa: E402


def _run_pick_addr_excluding(addrs, exclude_scopes):
    """Run cmd_pick_addr_excluding with a fake stdin + capture stdout.
    Returns the printed string with trailing newline stripped."""
    fake_stdin = io.StringIO(json.dumps(addrs))
    fake_args = mock.Mock(exclude_scopes=list(exclude_scopes))
    out = io.StringIO()
    with mock.patch("sys.stdin", fake_stdin), redirect_stdout(out):
        rc = gistparse.cmd_pick_addr_excluding(fake_args)
    assert rc == 0
    return out.getvalue().rstrip("\n")


class PickAddrExcludingTests(unittest.TestCase):
    """Joiner-side reachability filter — must skip scopes the joiner
    can't route to, and return EMPTY when nothing remains so the caller
    falls through to gh-bearer-only routing instead of dialing into
    the void (which triggers destructive self-heal)."""

    LOCALHOST = {"scope": "localhost", "addr": "127.0.0.1", "port": "7547"}
    LAN_42 = {"scope": "lan", "addr": "192.168.1.42",
              "port": "7547", "subnet": "192.168.1.0/24"}
    LAN_99 = {"scope": "lan", "addr": "10.0.0.99",
              "port": "7547", "subnet": "10.0.0.0/24"}
    TAILSCALE = {"scope": "tailscale", "addr": "100.79.156.3", "port": "7547"}

    def test_excludes_localhost_picks_lan(self):
        out = _run_pick_addr_excluding(
            [self.LOCALHOST, self.LAN_42, self.TAILSCALE],
            ["localhost"],
        )
        # Tailscale was first in the list after exclusion; lan was second.
        # First-after-exclusion wins, so this should be lan if lan came first.
        # Order in input: [localhost, lan, tailscale] → after excluding
        # localhost: [lan, tailscale] → first = lan.
        self.assertEqual(out, "192.168.1.42|7547")

    def test_excludes_localhost_and_tailscale_picks_lan(self):
        out = _run_pick_addr_excluding(
            [self.LOCALHOST, self.LAN_42, self.TAILSCALE],
            ["localhost", "tailscale"],
        )
        self.assertEqual(out, "192.168.1.42|7547")

    def test_only_localhost_and_tailscale_returns_empty(self):
        """The motivating case: Mac without Tailscale joins Windows
        host whose addresses[] is [localhost, tailscale]. The Mac
        excludes BOTH (localhost is its own loopback; tailscale is
        unroutable without a tailscale interface). Empty return →
        caller falls through to gh-bearer-only, NOT a doomed TCP
        attempt that would trigger destructive self-heal."""
        out = _run_pick_addr_excluding(
            [self.LOCALHOST, self.TAILSCALE],
            ["localhost", "tailscale"],
        )
        self.assertEqual(out, "")

    def test_empty_input_returns_empty(self):
        out = _run_pick_addr_excluding([], ["localhost"])
        self.assertEqual(out, "")

    def test_first_match_wins_among_remaining(self):
        """Multiple non-excluded entries → first one wins."""
        out = _run_pick_addr_excluding(
            [self.LOCALHOST, self.LAN_42, self.LAN_99],
            ["localhost"],
        )
        self.assertEqual(out, "192.168.1.42|7547")

    def test_skips_entries_missing_addr_or_port(self):
        """Malformed entries (missing addr/port) shouldn't be picked
        even if their scope passes the exclusion check."""
        broken = {"scope": "lan", "addr": "", "port": "7547"}
        out = _run_pick_addr_excluding(
            [broken, self.LAN_42],
            ["localhost"],
        )
        self.assertEqual(out, "192.168.1.42|7547")

    def test_non_dict_entries_skipped(self):
        out = _run_pick_addr_excluding(
            ["not a dict", 42, self.LAN_42],
            ["localhost"],
        )
        self.assertEqual(out, "192.168.1.42|7547")

    def test_malformed_input_returns_empty(self):
        """Non-list stdin → empty (we preserve jq's quiet-on-malformed
        behavior; the bash caller treats empty as 'falls through to
        gh-bearer-only', which is the safe default)."""
        fake_stdin = io.StringIO('{"not":"a list"}')
        fake_args = mock.Mock(exclude_scopes=["localhost"])
        out = io.StringIO()
        with mock.patch("sys.stdin", fake_stdin), redirect_stdout(out):
            rc = gistparse.cmd_pick_addr_excluding(fake_args)
        self.assertEqual(rc, 0)
        self.assertEqual(out.getvalue().rstrip("\n"), "")


class PickAddrNonlocalFirstBackwardCompatTests(unittest.TestCase):
    """pick_addr_nonlocal_first is superseded by pick_addr_excluding
    but kept for backward compat. Verify it still behaves as before."""

    def _run(self, addrs):
        fake_stdin = io.StringIO(json.dumps(addrs))
        out = io.StringIO()
        with mock.patch("sys.stdin", fake_stdin), redirect_stdout(out):
            rc = gistparse.cmd_pick_addr_nonlocal_first(mock.Mock())
        assert rc == 0
        return out.getvalue().rstrip("\n")

    def test_skips_localhost_picks_lan(self):
        out = self._run([
            {"scope": "localhost", "addr": "127.0.0.1", "port": "7547"},
            {"scope": "lan", "addr": "192.168.1.42", "port": "7547"},
        ])
        self.assertEqual(out, "192.168.1.42|7547")

    def test_only_localhost_returns_empty(self):
        out = self._run([
            {"scope": "localhost", "addr": "127.0.0.1", "port": "7547"},
        ])
        self.assertEqual(out, "")


if __name__ == "__main__":
    unittest.main()
