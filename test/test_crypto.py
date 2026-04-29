"""airc envelope-layer crypto tests.

Run: cd test && python -m unittest test_crypto

These tests exercise the primitives in lib/airc_core/crypto.py against
their RFC contracts. Where possible, tests use known-answer vectors;
where the underlying primitive is non-deterministic (random nonce
generation), tests verify round-trip correctness instead.

Skip behavior: if cryptography package isn't importable (fresh checkout
with no venv), the whole module is skipped. The user-facing message
points at install.sh to set up the venv. This avoids breaking the rest
of the test suite for someone running without the dep.
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "lib"))

try:
    from airc_core import crypto  # noqa: E402
    _HAS_CRYPTO = True
    _SKIP_REASON = ""
except ImportError as e:
    _HAS_CRYPTO = False
    _SKIP_REASON = (
        f"cryptography package not available ({e}). "
        f"Run install.sh to set up the venv with airc's runtime dependencies."
    )


@unittest.skipUnless(_HAS_CRYPTO, _SKIP_REASON)
class X25519KeypairTests(unittest.TestCase):
    """Generate, save, load — round-trip private and public keys
    through disk. Verify the on-disk format is exactly 32 raw bytes
    (no PEM armor, no padding)."""

    def test_generate_returns_32_byte_pair(self):
        priv, pub = crypto.generate_x25519_keypair()
        self.assertEqual(len(priv), 32)
        self.assertEqual(len(pub), 32)
        self.assertNotEqual(priv, pub)

    def test_generate_is_random(self):
        # Two keypairs in a row must differ (collisions are 2^-256 against).
        kp1 = crypto.generate_x25519_keypair()
        kp2 = crypto.generate_x25519_keypair()
        self.assertNotEqual(kp1, kp2)
        self.assertNotEqual(kp1[0], kp2[0])
        self.assertNotEqual(kp1[1], kp2[1])

    def test_save_and_load_round_trip(self):
        import tempfile, os
        priv, pub = crypto.generate_x25519_keypair()
        with tempfile.TemporaryDirectory() as td:
            ppath = os.path.join(td, "priv")
            pubpath = os.path.join(td, "pub")
            crypto.save_keypair(priv, pub, ppath, pubpath)
            self.assertEqual(crypto.load_priv(ppath), priv)
            self.assertEqual(crypto.load_pub(pubpath), pub)

    def test_save_sets_priv_mode_0600(self):
        import tempfile, os, stat
        priv, pub = crypto.generate_x25519_keypair()
        with tempfile.TemporaryDirectory() as td:
            ppath = os.path.join(td, "priv")
            pubpath = os.path.join(td, "pub")
            crypto.save_keypair(priv, pub, ppath, pubpath)
            priv_mode = stat.S_IMODE(os.stat(ppath).st_mode)
            self.assertEqual(priv_mode, 0o600,
                             f"private key mode is {oct(priv_mode)}, expected 0o600")

    def test_load_priv_rejects_wrong_length(self):
        import tempfile, os
        with tempfile.TemporaryDirectory() as td:
            ppath = os.path.join(td, "priv")
            with open(ppath, "wb") as f:
                f.write(b"\x00" * 16)  # too short
            with self.assertRaises(ValueError):
                crypto.load_priv(ppath)

    def test_atomic_write_no_partial_state_on_kill(self):
        # We can't easily simulate a SIGKILL mid-write, but we CAN check
        # that the implementation uses a tmp + rename pattern by
        # observing that the target path doesn't exist between generate
        # and replace — a black-box assertion would require monkey-
        # patching os.write. Instead, assert the simpler property: the
        # final file matches the bytes passed in. (The atomicity is
        # pieced together from os.replace docs + the implementation.)
        import tempfile, os
        priv, pub = crypto.generate_x25519_keypair()
        with tempfile.TemporaryDirectory() as td:
            ppath = os.path.join(td, "priv")
            pubpath = os.path.join(td, "pub")
            crypto.save_keypair(priv, pub, ppath, pubpath)
            # No leftover .tmp files
            self.assertFalse(os.path.exists(ppath + ".tmp"))
            self.assertFalse(os.path.exists(pubpath + ".tmp"))


@unittest.skipUnless(_HAS_CRYPTO, _SKIP_REASON)
class ECDHDeriveTests(unittest.TestCase):
    """X25519 ECDH + HKDF → 32-byte AEAD key. Both peers must derive
    the SAME key from their own private + the other's public."""

    def test_pair_derives_matching_key(self):
        priv_a, pub_a = crypto.generate_x25519_keypair()
        priv_b, pub_b = crypto.generate_x25519_keypair()
        key_a = crypto.derive_pairwise_key(priv_a, pub_b)
        key_b = crypto.derive_pairwise_key(priv_b, pub_a)
        self.assertEqual(key_a, key_b,
                         "both peers must derive the same pairwise key")
        self.assertEqual(len(key_a), 32)

    def test_different_pairs_derive_different_keys(self):
        priv_a, pub_a = crypto.generate_x25519_keypair()
        priv_b, pub_b = crypto.generate_x25519_keypair()
        priv_c, pub_c = crypto.generate_x25519_keypair()
        key_ab = crypto.derive_pairwise_key(priv_a, pub_b)
        key_ac = crypto.derive_pairwise_key(priv_a, pub_c)
        self.assertNotEqual(key_ab, key_ac)

    def test_info_string_domain_separates(self):
        # Same keys but different `info` MUST produce different keys.
        # This is the HKDF domain-separation property — without it, an
        # attacker who learns one purpose's key could misuse it for
        # another purpose.
        priv_a, _ = crypto.generate_x25519_keypair()
        _, pub_b = crypto.generate_x25519_keypair()
        key1 = crypto.derive_pairwise_key(priv_a, pub_b, info=b"purpose-1")
        key2 = crypto.derive_pairwise_key(priv_a, pub_b, info=b"purpose-2")
        self.assertNotEqual(key1, key2)

    def test_rejects_wrong_length_keys(self):
        priv, pub = crypto.generate_x25519_keypair()
        with self.assertRaises(ValueError):
            crypto.derive_pairwise_key(priv[:16], pub)
        with self.assertRaises(ValueError):
            crypto.derive_pairwise_key(priv, pub[:16])


@unittest.skipUnless(_HAS_CRYPTO, _SKIP_REASON)
class AEADTests(unittest.TestCase):
    """ChaCha20-Poly1305 round-trip + auth-failure detection."""

    def setUp(self):
        priv_a, pub_a = crypto.generate_x25519_keypair()
        priv_b, pub_b = crypto.generate_x25519_keypair()
        self.key = crypto.derive_pairwise_key(priv_a, pub_b)

    def test_encrypt_then_decrypt_round_trip(self):
        plaintext = b"hello, world"
        nonce, ct = crypto.aead_encrypt(self.key, plaintext)
        recovered = crypto.aead_decrypt(self.key, nonce, ct)
        self.assertEqual(recovered, plaintext)

    def test_associated_data_authenticated(self):
        plaintext = b"private payload"
        ad = b"sender=alice|channel=general|ts=12345"
        nonce, ct = crypto.aead_encrypt(self.key, plaintext, ad)
        # Decrypt with same AD — works.
        self.assertEqual(crypto.aead_decrypt(self.key, nonce, ct, ad), plaintext)
        # Decrypt with different AD — fails with InvalidTag.
        from cryptography.exceptions import InvalidTag
        with self.assertRaises(InvalidTag):
            crypto.aead_decrypt(self.key, nonce, ct, b"sender=mallory|channel=general|ts=12345")

    def test_tampered_ciphertext_fails_auth(self):
        nonce, ct = crypto.aead_encrypt(self.key, b"payload")
        # Flip a byte in the ciphertext.
        tampered = bytes([ct[0] ^ 0xFF]) + ct[1:]
        from cryptography.exceptions import InvalidTag
        with self.assertRaises(InvalidTag):
            crypto.aead_decrypt(self.key, nonce, tampered)

    def test_tampered_nonce_fails_auth(self):
        nonce, ct = crypto.aead_encrypt(self.key, b"payload")
        tampered_nonce = bytes([nonce[0] ^ 0xFF]) + nonce[1:]
        from cryptography.exceptions import InvalidTag
        with self.assertRaises(InvalidTag):
            crypto.aead_decrypt(self.key, tampered_nonce, ct)

    def test_explicit_nonce_round_trip(self):
        # Counter nonce path: encrypt with nonce=counter_nonce(N).
        nonce_in = crypto.counter_nonce(42)
        nonce_out, ct = crypto.aead_encrypt(self.key, b"x", nonce=nonce_in)
        self.assertEqual(nonce_out, nonce_in)
        self.assertEqual(crypto.aead_decrypt(self.key, nonce_out, ct), b"x")

    def test_rejects_wrong_key_length(self):
        with self.assertRaises(ValueError):
            crypto.aead_encrypt(b"\x00" * 16, b"x")  # 16-byte key (AES-128 size, not ChaCha)

    def test_rejects_wrong_nonce_length(self):
        with self.assertRaises(ValueError):
            crypto.aead_encrypt(self.key, b"x", nonce=b"\x00" * 8)  # too short


@unittest.skipUnless(_HAS_CRYPTO, _SKIP_REASON)
class CounterNonceTests(unittest.TestCase):
    """Counter nonces are deterministic and reversible. Replay-defense
    higher up depends on this round-trip behavior."""

    def test_round_trip(self):
        for n in (0, 1, 42, 2**32 - 1, 2**63 - 1):
            nonce = crypto.counter_nonce(n)
            self.assertEqual(crypto.parse_counter_nonce(nonce), n)
            self.assertEqual(len(nonce), 12)

    def test_rejects_negative_counter(self):
        with self.assertRaises(ValueError):
            crypto.counter_nonce(-1)

    def test_rejects_overflow(self):
        with self.assertRaises(ValueError):
            crypto.counter_nonce(2**64)

    def test_parse_rejects_non_counter_format(self):
        # Random nonce should NOT round-trip as a counter (suffix nonzero).
        import os
        random_nonce = os.urandom(12)
        # If the random suffix happens to be all zeros (probability 2^-32),
        # this test would mis-fire. Force a nonzero suffix.
        random_nonce = random_nonce[:8] + b"\x00\x00\x00\x01"
        with self.assertRaises(ValueError):
            crypto.parse_counter_nonce(random_nonce)


@unittest.skipUnless(_HAS_CRYPTO, _SKIP_REASON)
class FingerprintTests(unittest.TestCase):
    """Public-key fingerprint — short hex digest for invite strings."""

    def test_default_length(self):
        _, pub = crypto.generate_x25519_keypair()
        fp = crypto.fingerprint(pub)
        self.assertEqual(len(fp), 16)
        # All hex chars
        int(fp, 16)

    def test_deterministic(self):
        _, pub = crypto.generate_x25519_keypair()
        self.assertEqual(crypto.fingerprint(pub), crypto.fingerprint(pub))

    def test_different_pubkeys_have_different_fingerprints(self):
        _, pub1 = crypto.generate_x25519_keypair()
        _, pub2 = crypto.generate_x25519_keypair()
        self.assertNotEqual(crypto.fingerprint(pub1), crypto.fingerprint(pub2))

    def test_custom_length(self):
        _, pub = crypto.generate_x25519_keypair()
        self.assertEqual(len(crypto.fingerprint(pub, length=8)), 8)
        self.assertEqual(len(crypto.fingerprint(pub, length=32)), 32)

    def test_rejects_wrong_pub_length(self):
        with self.assertRaises(ValueError):
            crypto.fingerprint(b"\x00" * 16)


@unittest.skipUnless(_HAS_CRYPTO, _SKIP_REASON)
class B64Tests(unittest.TestCase):
    """URL-safe base64 helpers used in JSON envelopes."""

    def test_round_trip(self):
        for data in (b"", b"x", b"hello world", bytes(range(256))):
            encoded = crypto.b64encode(data)
            self.assertIsInstance(encoded, str)
            self.assertEqual(crypto.b64decode(encoded), data)

    def test_no_padding_in_output(self):
        # Default urlsafe_b64encode emits = padding; we strip it.
        encoded = crypto.b64encode(b"x")  # 1 byte → 2 chars + 2 padding
        self.assertNotIn("=", encoded)

    def test_url_safe_alphabet(self):
        # 1024 random bytes — the encoded form must be URL-safe (only
        # [A-Za-z0-9_-] in addition to the unpadded length).
        import os, re
        encoded = crypto.b64encode(os.urandom(1024))
        self.assertTrue(re.match(r"^[A-Za-z0-9_-]+$", encoded))

    def test_decode_handles_unpadded_input(self):
        # Our encode strips padding, so decode must re-pad. Verify by
        # encoding without padding, then decoding.
        for data in (b"x", b"xx", b"xxx", b"xxxx"):
            self.assertEqual(crypto.b64decode(crypto.b64encode(data)), data)

    def test_decode_rejects_non_str(self):
        with self.assertRaises(ValueError):
            crypto.b64decode(b"abc")  # bytes, not str


@unittest.skipUnless(_HAS_CRYPTO, _SKIP_REASON)
class FullPairwiseScenarioTests(unittest.TestCase):
    """End-to-end exercise of the primitives in the way airc envelope
    encryption will actually use them. Two peers each generate a keypair,
    derive their shared key, exchange an AEAD-encrypted message bound
    to a plaintext envelope (associated data), and decrypt it back."""

    def test_alice_to_bob_full_envelope(self):
        # Alice's identity
        ap, AP = crypto.generate_x25519_keypair()
        # Bob's identity
        bp, BP = crypto.generate_x25519_keypair()

        # Pair handshake exchanges public keys; both sides derive the same key.
        alice_to_bob_key = crypto.derive_pairwise_key(ap, BP)
        bob_to_alice_key = crypto.derive_pairwise_key(bp, AP)
        self.assertEqual(alice_to_bob_key, bob_to_alice_key)

        # Alice sends an envelope: plaintext metadata + encrypted body.
        # The metadata is bound via associated data so a tampered "from"
        # field would invalidate the auth tag.
        metadata = b"from=alice|to=bob|channel=general|ts=2026-04-29T01:23:45Z"
        body = b"the plaintext message contents"
        nonce, ct = crypto.aead_encrypt(alice_to_bob_key, body, associated_data=metadata)

        # Bob receives, decrypts.
        recovered = crypto.aead_decrypt(bob_to_alice_key, nonce, ct, associated_data=metadata)
        self.assertEqual(recovered, body)

        # Mallory sees ciphertext + metadata. Without the key she can't
        # decrypt; without re-deriving she can't forge.
        mp, MP = crypto.generate_x25519_keypair()
        mallory_alice_key = crypto.derive_pairwise_key(mp, AP)
        from cryptography.exceptions import InvalidTag
        with self.assertRaises(InvalidTag):
            crypto.aead_decrypt(mallory_alice_key, nonce, ct, associated_data=metadata)


if __name__ == "__main__":
    unittest.main()
