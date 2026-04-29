"""Envelope wrap/unwrap tests — Phase E.3 plumbing.

Verifies the encrypt-on-send / decrypt-on-recv path airc uses for
end-to-end encrypted message bodies. Like test_crypto, this skips
cleanly when the cryptography package is missing so a fresh checkout
without the venv set up doesn't fail the full test run.
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "lib"))

try:
    from airc_core import crypto  # noqa: F401
    from airc_core import envelope
    from airc_core import identity
    _HAS_CRYPTO = True
    _SKIP_REASON = ""
except ImportError as e:
    _HAS_CRYPTO = False
    _SKIP_REASON = (
        f"cryptography package not available ({e}). "
        f"Run install.sh to set up the venv."
    )


@unittest.skipUnless(_HAS_CRYPTO, _SKIP_REASON)
class IdentityBootstrapTests(unittest.TestCase):
    """Identity directory bootstrap + idempotency."""

    def test_bootstrap_creates_keypair(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            self.assertFalse(identity.has_x25519_keypair(td))
            priv, pub = identity.bootstrap(td)
            self.assertEqual(len(priv), 32)
            self.assertEqual(len(pub), 32)
            self.assertTrue(identity.has_x25519_keypair(td))

    def test_bootstrap_is_idempotent(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            priv1, pub1 = identity.bootstrap(td)
            priv2, pub2 = identity.bootstrap(td)
            self.assertEqual(priv1, priv2,
                             "second bootstrap must read existing keys, not regenerate")
            self.assertEqual(pub1, pub2)

    def test_load_pub_returns_none_when_missing(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            self.assertIsNone(identity.load_pub(td))

    def test_peer_x25519_pub_round_trip(self):
        import tempfile, os, json as _json
        with tempfile.TemporaryDirectory() as td:
            peers = os.path.join(td, "peers")
            os.makedirs(peers)
            # Seed an existing peer record (ssh-style, no x25519)
            peer_path = os.path.join(peers, "bob.json")
            with open(peer_path, "w") as f:
                _json.dump({"name": "bob", "host": "user@x"}, f)
            # No pubkey yet
            self.assertIsNone(identity.peer_x25519_pub(peers, "bob"))
            # Store one
            _, pub = identity.bootstrap(td)  # use this td's keys for shape only
            self.assertTrue(identity.store_peer_x25519_pub(peers, "bob", pub))
            # Read back
            self.assertEqual(identity.peer_x25519_pub(peers, "bob"), pub)

    def test_store_peer_x25519_rejects_wrong_length(self):
        import tempfile, os, json as _json
        with tempfile.TemporaryDirectory() as td:
            peers = os.path.join(td, "peers")
            os.makedirs(peers)
            with open(os.path.join(peers, "bob.json"), "w") as f:
                _json.dump({"name": "bob"}, f)
            self.assertFalse(identity.store_peer_x25519_pub(peers, "bob", b"\x00" * 16))

    def test_peer_x25519_pub_returns_none_for_missing_peer(self):
        import tempfile, os
        with tempfile.TemporaryDirectory() as td:
            peers = os.path.join(td, "peers")
            os.makedirs(peers)
            self.assertIsNone(identity.peer_x25519_pub(peers, "ghost"))


@unittest.skipUnless(_HAS_CRYPTO, _SKIP_REASON)
class EnvelopeWrapUnwrapTests(unittest.TestCase):
    """The core E2E loop: alice wraps an envelope to bob; bob unwraps.

    Each test exercises a specific failure mode or correctness property.
    """

    def setUp(self):
        priv_a, pub_a = crypto.generate_x25519_keypair()
        priv_b, pub_b = crypto.generate_x25519_keypair()
        self.alice_priv, self.alice_pub = priv_a, pub_a
        self.bob_priv, self.bob_pub = priv_b, pub_b

    def _envelope(self, msg="hello world"):
        return {
            "from": "alice",
            "to": "bob",
            "ts": "2026-04-29T01:23:45Z",
            "channel": "general",
            "msg": msg,
        }

    def test_round_trip_preserves_msg(self):
        env = self._envelope("the actual content")
        wrapped = envelope.wrap_envelope(env, self.alice_priv, self.bob_pub)
        self.assertEqual(wrapped["enc"], "v1")
        self.assertIn("nonce", wrapped)
        self.assertNotEqual(wrapped["msg"], "the actual content",
                            "msg must be ciphertext after wrap")
        unwrapped = envelope.unwrap_envelope(wrapped, self.bob_priv, self.alice_pub)
        self.assertIsNotNone(unwrapped)
        self.assertEqual(unwrapped["msg"], "the actual content")
        self.assertNotIn("enc", unwrapped, "enc field must be stripped on unwrap")
        self.assertNotIn("nonce", unwrapped, "nonce must be stripped on unwrap")

    def test_round_trip_preserves_metadata(self):
        env = self._envelope()
        wrapped = envelope.wrap_envelope(env, self.alice_priv, self.bob_pub)
        # Metadata fields stay plaintext
        self.assertEqual(wrapped["from"], "alice")
        self.assertEqual(wrapped["to"], "bob")
        self.assertEqual(wrapped["ts"], "2026-04-29T01:23:45Z")
        self.assertEqual(wrapped["channel"], "general")

    def test_tampered_metadata_fails_unwrap(self):
        env = self._envelope("private")
        wrapped = envelope.wrap_envelope(env, self.alice_priv, self.bob_pub)
        # Tamper: rewrite "from" to mallory.
        wrapped["from"] = "mallory"
        unwrapped = envelope.unwrap_envelope(wrapped, self.bob_priv, self.alice_pub)
        self.assertIsNone(unwrapped, "AD-bound metadata tamper must invalidate AEAD")

    def test_wrong_recipient_cannot_decrypt(self):
        env = self._envelope("for bob's eyes only")
        wrapped = envelope.wrap_envelope(env, self.alice_priv, self.bob_pub)
        # Carol tries to decrypt with HER private + alice's public. AEAD
        # auth fails because the derived key is different.
        priv_c, _ = crypto.generate_x25519_keypair()
        unwrapped = envelope.unwrap_envelope(wrapped, priv_c, self.alice_pub)
        self.assertIsNone(unwrapped)

    def test_unwrap_returns_none_for_unencrypted(self):
        # Plaintext envelope (no enc field) → unwrap returns None so
        # caller can branch ("wasn't encrypted, pass through plaintext").
        env = self._envelope("plain")
        self.assertIsNone(
            envelope.unwrap_envelope(env, self.bob_priv, self.alice_pub),
            "unwrap of plaintext envelope must return None",
        )

    def test_unwrap_returns_none_for_unknown_version(self):
        env = self._envelope("plain")
        env["enc"] = "v99"
        env["nonce"] = "abc"
        self.assertIsNone(envelope.unwrap_envelope(env, self.bob_priv, self.alice_pub))

    def test_is_encrypted_predicate(self):
        env = self._envelope()
        self.assertFalse(envelope.is_encrypted(env))
        wrapped = envelope.wrap_envelope(env, self.alice_priv, self.bob_pub)
        self.assertTrue(envelope.is_encrypted(wrapped))

    def test_unicode_message(self):
        env = self._envelope("héllo 🌐 wörld — bearer-rewrite")
        wrapped = envelope.wrap_envelope(env, self.alice_priv, self.bob_pub)
        unwrapped = envelope.unwrap_envelope(wrapped, self.bob_priv, self.alice_pub)
        self.assertIsNotNone(unwrapped)
        self.assertEqual(unwrapped["msg"], "héllo 🌐 wörld — bearer-rewrite")

    def test_input_envelope_not_mutated(self):
        env = self._envelope()
        before = dict(env)
        envelope.wrap_envelope(env, self.alice_priv, self.bob_pub)
        self.assertEqual(env, before, "wrap must not mutate caller's dict")


@unittest.skipUnless(_HAS_CRYPTO, _SKIP_REASON)
class EnvelopeCLITests(unittest.TestCase):
    """The bash-callable wrap/unwrap CLI exercises the full pipeline
    cmd_send / monitor_formatter use. Tests run the CLI as a real
    subprocess so the os/argv/stdin contract is exercised end-to-end."""

    def _setup_alice_bob(self):
        import tempfile
        td = tempfile.mkdtemp(prefix="airc-test-envelope-cli-")
        alice_dir = f"{td}/alice/identity"
        bob_dir = f"{td}/bob/identity"
        import os
        os.makedirs(alice_dir)
        os.makedirs(bob_dir)
        priv_a, pub_a = identity.bootstrap(alice_dir)
        priv_b, pub_b = identity.bootstrap(bob_dir)
        return td, alice_dir, bob_dir, pub_a, pub_b

    def _run_cli(self, args, stdin_str):
        import subprocess
        repo_root = REPO_ROOT
        env = {
            "PYTHONPATH": str(repo_root / "lib"),
            "PATH": __import__("os").environ.get("PATH", ""),
        }
        return subprocess.run(
            [sys.executable, "-m", "airc_core.envelope"] + args,
            input=stdin_str.encode("utf-8"),
            capture_output=True,
            timeout=10,
            env=env,
        )

    def test_cli_wrap_unwrap_round_trip(self):
        import json as _json, shutil
        td, alice_dir, bob_dir, pub_a, pub_b = self._setup_alice_bob()
        try:
            envjson = _json.dumps({
                "from": "alice", "to": "bob",
                "ts": "2026-04-29T01:23:45Z", "channel": "general",
                "msg": "hello over the wire",
            })
            wrap = self._run_cli(
                ["wrap", "--recipient-pub", crypto.b64encode(pub_b),
                 "--identity-dir", alice_dir],
                envjson,
            )
            self.assertEqual(wrap.returncode, 0,
                             f"wrap failed: stderr={wrap.stderr.decode()}")
            wrapped = _json.loads(wrap.stdout.decode().strip())
            self.assertEqual(wrapped["enc"], "v1")
            self.assertNotEqual(wrapped["msg"], "hello over the wire")

            unwrap = self._run_cli(
                ["unwrap", "--sender-pub", crypto.b64encode(pub_a),
                 "--identity-dir", bob_dir],
                _json.dumps(wrapped),
            )
            self.assertEqual(unwrap.returncode, 0,
                             f"unwrap failed: stderr={unwrap.stderr.decode()}")
            unwrapped = _json.loads(unwrap.stdout.decode().strip())
            self.assertEqual(unwrapped["msg"], "hello over the wire")
        finally:
            shutil.rmtree(td, ignore_errors=True)

    def test_cli_wrap_passes_through_plaintext_when_no_recipient(self):
        # Empty --recipient-pub = plaintext fallback (peer hasn't paired
        # under E2E yet). CLI must pass through unchanged.
        import json as _json, shutil
        td, alice_dir, _, _, _ = self._setup_alice_bob()
        try:
            envjson = _json.dumps({
                "from": "alice", "to": "bob",
                "ts": "2026-04-29T01:23:45Z", "channel": "general",
                "msg": "fallback plaintext",
            })
            r = self._run_cli(
                ["wrap", "--recipient-pub", "", "--identity-dir", alice_dir],
                envjson,
            )
            self.assertEqual(r.returncode, 0)
            out = _json.loads(r.stdout.decode().strip())
            self.assertEqual(out["msg"], "fallback plaintext",
                             "empty recipient_pub must produce plaintext pass-through")
            self.assertNotIn("enc", out)
        finally:
            shutil.rmtree(td, ignore_errors=True)

    def test_cli_unwrap_passes_through_plaintext(self):
        # Receiving a plaintext envelope (no enc field) → CLI passes
        # through. monitor_formatter relies on this for backward compat.
        import json as _json, shutil
        td, _, bob_dir, pub_a, _ = self._setup_alice_bob()
        try:
            envjson = _json.dumps({
                "from": "alice", "to": "bob", "ts": "x",
                "channel": "general", "msg": "plain inbound",
            })
            r = self._run_cli(
                ["unwrap", "--sender-pub", crypto.b64encode(pub_a),
                 "--identity-dir", bob_dir],
                envjson,
            )
            self.assertEqual(r.returncode, 0)
            out = _json.loads(r.stdout.decode().strip())
            self.assertEqual(out["msg"], "plain inbound")
        finally:
            shutil.rmtree(td, ignore_errors=True)


if __name__ == "__main__":
    unittest.main()
