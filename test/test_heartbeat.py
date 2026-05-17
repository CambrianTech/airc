"""Heartbeat envelope + helper tests (airc#644 PR-1).

Verifies the structural pieces of the peer-heartbeat fix: envelope
shape with kind="heartbeat", AEAD AD binding includes kind so a transit
attacker can't swap kind, the is_heartbeat predicate works, and the
is_process_likely_down helper distinguishes legacy peers (None) from
stale peers (timeout exceeded).

PR-2 will add tests for the cmd_send hook + reminder_timer_loop trigger
when those wires land.
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "lib"))

from airc_core import heartbeat

try:
    from airc_core import crypto, envelope, identity
    _HAS_CRYPTO = True
except ImportError:
    _HAS_CRYPTO = False


class MakeHeartbeatEnvelopeTests(unittest.TestCase):
    """Heartbeat envelope shape — fields the substrate relies on."""

    def test_envelope_has_required_fields(self):
        env = heartbeat.make_heartbeat_envelope(
            from_name="alice",
            channel="general",
            timestamp_iso="2026-05-17T01:00:00Z",
        )
        self.assertEqual(env["from"], "alice")
        self.assertEqual(env["to"], "all")  # heartbeats are room-wide
        self.assertEqual(env["ts"], "2026-05-17T01:00:00Z")
        self.assertEqual(env["channel"], "general")
        self.assertEqual(env["kind"], "heartbeat")
        self.assertEqual(env["msg"], "")  # empty body

    def test_default_timestamp_when_omitted(self):
        env = heartbeat.make_heartbeat_envelope("alice", "general")
        # Should be a non-empty ISO-8601 Z string.
        self.assertTrue(env["ts"].endswith("Z"))
        self.assertEqual(len(env["ts"]), 20)  # YYYY-MM-DDTHH:MM:SSZ


class IsHeartbeatTests(unittest.TestCase):
    """The predicate the monitor formatter uses to filter UI rendering."""

    def test_recognizes_heartbeat_envelope(self):
        env = heartbeat.make_heartbeat_envelope("alice", "general")
        self.assertTrue(heartbeat.is_heartbeat(env))

    def test_does_not_match_chat(self):
        self.assertFalse(heartbeat.is_heartbeat({
            "from": "alice", "to": "all", "channel": "general",
            "msg": "hello", "kind": "chat",
        }))

    def test_does_not_match_legacy_envelope_without_kind(self):
        """Back-compat: pre-#644 envelopes omit the kind field. They
        must not be mistaken for heartbeats — they're chat."""
        self.assertFalse(heartbeat.is_heartbeat({
            "from": "alice", "to": "all", "channel": "general",
            "msg": "hello",
        }))

    def test_does_not_match_system(self):
        self.assertFalse(heartbeat.is_heartbeat({
            "from": "airc", "to": "all", "channel": "general",
            "msg": "alice joined", "kind": "system",
        }))


class HeartbeatAgeTests(unittest.TestCase):
    """The age helper used by cmd_peers to distinguish PROCESS_DOWN
    from heads-down-but-alive."""

    def test_fresh_heartbeat_is_zero_age(self):
        age = heartbeat.heartbeat_age_seconds(last_heartbeat_ts=1000, now_ts=1000)
        self.assertEqual(age, 0)

    def test_old_heartbeat_returns_elapsed(self):
        age = heartbeat.heartbeat_age_seconds(last_heartbeat_ts=1000, now_ts=1180)
        self.assertEqual(age, 180)

    def test_none_returns_none_not_zero(self):
        """Critical: a peer we've never received a heartbeat from must
        return None, NOT 0 or infinity. Downstream code uses None to
        mean 'unknown, don't false-positive'."""
        age = heartbeat.heartbeat_age_seconds(last_heartbeat_ts=None, now_ts=1000)
        self.assertIsNone(age)

    def test_age_clamped_at_zero(self):
        """Clock skew shouldn't produce negative ages."""
        age = heartbeat.heartbeat_age_seconds(last_heartbeat_ts=2000, now_ts=1000)
        self.assertEqual(age, 0)


class IsProcessLikelyDownTests(unittest.TestCase):
    """The process-down predicate. The most important property: None
    (legacy peer) must NOT be treated as down. This is the back-compat
    guarantee for partial rollouts."""

    def test_fresh_heartbeat_is_not_down(self):
        self.assertFalse(heartbeat.is_process_likely_down(last_heartbeat_age_sec=10))

    def test_one_missed_heartbeat_still_not_down(self):
        """One missed heartbeat (61-119s) is within tolerance — could be
        network jitter or a brief GC pause. Not down until 2x cadence."""
        self.assertFalse(heartbeat.is_process_likely_down(last_heartbeat_age_sec=90))

    def test_two_missed_heartbeats_is_down(self):
        """At 121s (>2x cadence=60s) the substrate flags PROCESS_DOWN."""
        self.assertTrue(heartbeat.is_process_likely_down(last_heartbeat_age_sec=121))

    def test_none_is_not_down_not_up(self):
        """Legacy peer: no heartbeats yet. Substrate admits ignorance.
        Returns False (not 'down') so partial rollouts don't false-positive."""
        self.assertFalse(heartbeat.is_process_likely_down(last_heartbeat_age_sec=None))


@unittest.skipUnless(_HAS_CRYPTO, "cryptography unavailable — skip wire-level tests")
class HeartbeatAEADBindingTests(unittest.TestCase):
    """The kind field is bound by AEAD associated data. Tampering with
    kind on the wire invalidates the auth tag → receiver drops.

    Why this matters: without kind in AD, an attacker (or buggy bearer)
    could rewrite a heartbeat envelope's kind to "chat" mid-flight. The
    monitor UI would then surface the heartbeat as a chat message; the
    room would see garbage. AD-binding prevents that silently-succeeding
    bug class.
    """

    def test_kind_default_chat_when_absent(self):
        ad = envelope._ad_fields({"from": "a", "to": "b", "ts": "t", "channel": "c"})
        self.assertEqual(ad["kind"], "chat")

    def test_kind_heartbeat_bound_in_ad(self):
        env = heartbeat.make_heartbeat_envelope("a", "general", timestamp_iso="t")
        ad = envelope._ad_fields(env)
        self.assertEqual(ad["kind"], "heartbeat")

    def test_kind_swap_breaks_ad(self):
        """Concrete tamper-detection test: build a heartbeat, build a
        chat envelope with identical other fields, AD should differ in
        the kind slot — which is what makes the AEAD tag refuse a swap.
        """
        hb_env = heartbeat.make_heartbeat_envelope("a", "general", timestamp_iso="t")
        chat_env = {**hb_env, "kind": "chat"}
        hb_ad = envelope._ad_fields(hb_env)
        chat_ad = envelope._ad_fields(chat_env)
        self.assertNotEqual(hb_ad, chat_ad)
        # Specifically the kind slot:
        self.assertEqual(hb_ad["kind"], "heartbeat")
        self.assertEqual(chat_ad["kind"], "chat")

    def test_wrap_unwrap_roundtrip_preserves_kind(self):
        """End-to-end: wrap a heartbeat with AEAD; unwrap with valid
        keys; kind survives unchanged."""
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            sender_priv, sender_pub = identity.bootstrap(td)
            recv_priv, recv_pub = identity.bootstrap(td + "/r")
            env = heartbeat.make_heartbeat_envelope("alice", "general")
            wrapped = envelope.wrap_envelope(env, sender_priv, recv_pub)
            self.assertEqual(wrapped["kind"], "heartbeat")  # plaintext metadata
            self.assertTrue(envelope.is_encrypted(wrapped))
            unwrapped = envelope.unwrap_envelope(wrapped, recv_priv, sender_pub)
            self.assertIsNotNone(unwrapped, "unwrap should succeed with valid keys")
            self.assertEqual(unwrapped["kind"], "heartbeat")
            self.assertEqual(unwrapped["msg"], "")


if __name__ == "__main__":
    unittest.main()
